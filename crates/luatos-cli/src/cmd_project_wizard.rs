//! 向导式项目创建命令。
//!
//! 提供交互式（dialoguer）和非交互式（全参数）两种模式。
//! 从 CDN 拉取 manifest，引导用户逐步选择型号、版本、模板、COM 口等配置，
//! 最终生成 `luatos-project.toml`、Lua 脚本、README.md，
//! 并可选下载固件/soc_script 及初始化 Git 仓库。

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use luatos_project::wizard::{chip_from_model_name, fallback_models, ModelInfo, TemplateKind, VersionEntry, WizardConfig};
use luatos_resource::{collect_files_for_child, download_files, fetch_manifest_with_cache, find_category, find_child, DownloadEvent, FileEntry, ResourceManifest};

use crate::OutputFormat;

// ─── 向导入口 ─────────────────────────────────────────────────────────────────

/// 非交互式向导所需的参数（全部为可选，缺少时进入交互模式）
pub struct WizardArgs {
    /// 项目名称（None → 交互输入）
    pub project_name: Option<String>,
    /// 项目目录（None → ./<name>）
    pub project_dir: Option<String>,
    /// 模组型号（None → 交互选择）
    pub model: Option<String>,
    /// 固件版本号（None → 交互选择）
    pub firmware_version: Option<String>,
    /// 项目模板（None → 交互选择）
    pub template: Option<String>,
    /// 串口（None → 交互选择）
    pub port: Option<String>,
    /// soc_script 版本（None → 交互选择）
    pub soc_script: Option<String>,
    /// 跳过 git 初始化
    pub no_git: bool,
    /// 跳过固件/soc_script 下载
    pub no_download: bool,
}

/// 向导主入口。根据 `args` 中参数是否完整决定进入交互或非交互模式。
pub fn run_wizard(args: WizardArgs, format: &OutputFormat) -> Result<()> {
    // 拉取 manifest（带缓存）
    let cache_path = manifest_cache_path();
    let manifest = match fetch_manifest_with_cache(&cache_path) {
        Ok(m) => {
            log::debug!("资源清单加载成功，共 {} 个分类", m.resouces.len());
            m
        }
        Err(e) => {
            log::warn!("拉取资源清单失败: {e}，使用预置型号列表（无版本信息）");
            ResourceManifest {
                version: 0,
                mirrors: Vec::new(),
                resouces: Vec::new(),
            }
        }
    };

    // 提取型号列表（manifest 失败则用 fallback）
    let mut models = extract_models(&manifest);
    if models.is_empty() {
        models = fallback_models();
    }

    // 获取 soc_script 版本列表
    let soc_script_versions = get_soc_script_versions(&manifest);

    // 收集串口列表（含端口名 + 设备描述）
    let ports = luatos_serial::list_ports();
    let port_labels: Vec<String> = ports
        .iter()
        .map(|p| {
            if let Some(ref prod) = p.product {
                format!("{} ({})", p.port_name, prod)
            } else {
                p.port_name.clone()
            }
        })
        .collect();

    // 进入交互或非交互模式
    let config = if is_fully_specified(&args, &models) {
        build_config_noninteractive(args, models)?
    } else {
        build_config_interactive(args, models, &ports, &port_labels, soc_script_versions)?
    };

    execute_wizard(config, &manifest, format)
}

// ─── 非交互模式 ───────────────────────────────────────────────────────────────

fn is_fully_specified(args: &WizardArgs, models: &[ModelInfo]) -> bool {
    args.project_name.is_some() && args.template.is_some() && args.model.as_ref().map(|m| models.iter().any(|mi| mi.name.eq_ignore_ascii_case(m))).unwrap_or(false)
}

fn build_config_noninteractive(args: WizardArgs, models: Vec<ModelInfo>) -> Result<WizardConfig> {
    let project_name = args.project_name.context("--name 不能为空")?;
    let model_name = args.model.context("--model 不能为空")?;

    let model = models
        .into_iter()
        .find(|m| m.name.eq_ignore_ascii_case(&model_name))
        .with_context(|| format!("未知型号: {model_name}"))?;

    let selected_version = if let Some(ref ver) = args.firmware_version {
        model.versions.iter().find(|v| v.version_name.eq_ignore_ascii_case(ver)).cloned()
    } else {
        model.versions.first().cloned()
    };

    let download_firmware = !args.no_download && selected_version.is_some();

    let template = args.template.as_deref().and_then(TemplateKind::from_str_name).unwrap_or(TemplateKind::HelloWorld);

    let soc_script = args.soc_script.clone().unwrap_or_else(|| "latest".to_string());
    let download_soc_script = !args.no_download && soc_script != "disable";

    let project_dir = resolve_project_dir(args.project_dir.as_deref(), &project_name);

    Ok(WizardConfig {
        project_name,
        project_dir,
        model,
        selected_version,
        download_firmware,
        template,
        port: args.port,
        soc_script,
        download_soc_script,
        git_init: !args.no_git,
    })
}

// ─── 交互模式 ─────────────────────────────────────────────────────────────────

fn build_config_interactive(
    prefill: WizardArgs,
    models: Vec<ModelInfo>,
    ports: &[luatos_serial::PortInfo],
    port_labels: &[String],
    soc_script_versions: Vec<String>,
) -> Result<WizardConfig> {
    use dialoguer::{Confirm, FuzzySelect, Input, Select};

    let theme = dialoguer::theme::ColorfulTheme::default();

    println!("\n🚀 LuatOS 项目创建向导\n");

    // ── 步骤 1：项目名称 ──────────────────────────────────
    let project_name: String = if let Some(name) = prefill.project_name {
        println!("📁 项目名称: {name}");
        name
    } else {
        Input::with_theme(&theme).with_prompt("项目名称").with_initial_text("my-luatos-project").interact_text()?
    };

    // ── 步骤 2：项目目录 ──────────────────────────────────
    let default_dir = format!("./{}", project_name);
    let project_dir_str = if let Some(dir) = prefill.project_dir {
        println!("📂 项目目录: {dir}");
        dir
    } else {
        Input::with_theme(&theme).with_prompt("项目目录").with_initial_text(&default_dir).interact_text()?
    };
    let project_dir = PathBuf::from(&project_dir_str);

    // ── 步骤 3：选择模组型号 ──────────────────────────────
    let model = if let Some(ref model_name) = prefill.model {
        models
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case(model_name))
            .cloned()
            .with_context(|| format!("未找到型号: {model_name}"))?
    } else {
        let model_labels: Vec<String> = models.iter().map(|m| format!("{} ({})", m.name, m.chip)).collect();
        let idx = FuzzySelect::with_theme(&theme)
            .with_prompt("选择模组型号（可输入关键字过滤）")
            .items(&model_labels)
            .default(0)
            .interact()?;
        models[idx].clone()
    };

    // ── 步骤 4：选择固件版本 ──────────────────────────────
    let selected_version: Option<VersionEntry> = if model.versions.is_empty() {
        println!("⚠  该型号暂无版本信息（离线模式），跳过固件版本选择");
        None
    } else if let Some(ref ver_name) = prefill.firmware_version {
        model.versions.iter().find(|v| v.version_name.eq_ignore_ascii_case(ver_name)).cloned()
    } else {
        let mut items: Vec<String> = model
            .versions
            .iter()
            .map(|v| format!("{} — {} ({})", v.version_name, v.filename, luatos_resource::format_size(v.size)))
            .collect();
        items.push("暂不选择".to_string());

        let idx = Select::with_theme(&theme).with_prompt("选择固件版本").items(&items).default(0).interact()?;
        if idx < model.versions.len() {
            Some(model.versions[idx].clone())
        } else {
            None
        }
    };

    // ── 步骤 5：是否立即下载固件 ─────────────────────────
    let download_firmware = if prefill.no_download || selected_version.is_none() {
        false
    } else {
        Confirm::with_theme(&theme)
            .with_prompt(format!("立即下载固件 {}？", selected_version.as_ref().unwrap().filename))
            .default(true)
            .interact()?
    };

    // ── 步骤 6：选择项目模板 ──────────────────────────────
    let template = if let Some(ref tmpl_name) = prefill.template {
        TemplateKind::from_str_name(tmpl_name).with_context(|| format!("未知模板: {tmpl_name}"))?
    } else {
        let all_templates = TemplateKind::all();
        let filtered: Vec<&TemplateKind> = all_templates.iter().filter(|t| t.supported_by_chip(&model.chip)).collect();
        let labels: Vec<&str> = filtered.iter().map(|t| t.display_name()).collect();
        let idx = Select::with_theme(&theme).with_prompt("选择项目模板").items(&labels).default(0).interact()?;
        filtered[idx].clone()
    };

    // ── 步骤 7：选择 COM 口 ───────────────────────────────
    let port: Option<String> = if let Some(ref p) = prefill.port {
        if p.eq_ignore_ascii_case("none") {
            None
        } else {
            Some(p.clone())
        }
    } else {
        let mut items = port_labels.to_vec();
        items.push("暂不选择".to_string());
        let idx = Select::with_theme(&theme).with_prompt("选择串口").items(&items).default(items.len() - 1).interact()?;
        if idx < ports.len() {
            Some(ports[idx].port_name.clone())
        } else {
            None
        }
    };

    // ── 步骤 8：soc_script 版本 ──────────────────────────
    let soc_script: String = if let Some(ref ver) = prefill.soc_script {
        ver.clone()
    } else {
        let mut items = vec!["latest（自动使用最新版本）".to_string(), "disable（不使用 soc_script）".to_string()];
        items.extend(soc_script_versions.iter().cloned());
        let idx = Select::with_theme(&theme).with_prompt("soc_script 版本").items(&items).default(0).interact()?;
        match idx {
            0 => "latest".to_string(),
            1 => "disable".to_string(),
            n => soc_script_versions[n - 2].clone(),
        }
    };

    // ── 步骤 9：是否下载 soc_script ──────────────────────
    let download_soc_script = if prefill.no_download || soc_script == "disable" {
        false
    } else {
        Confirm::with_theme(&theme).with_prompt("立即下载 soc_script 扩展库？").default(true).interact()?
    };

    // ── 步骤 10：git init ─────────────────────────────────
    let git_init = if prefill.no_git {
        false
    } else {
        Confirm::with_theme(&theme).with_prompt("初始化 Git 仓库？").default(true).interact()?
    };

    // ── 步骤 11：预览并确认 ───────────────────────────────
    println!("\n─── 配置预览 ──────────────────────────────────────");
    println!("  项目名称:   {project_name}");
    println!("  目录:       {}", project_dir.display());
    println!("  型号:       {} (chip={})", model.name, model.chip);
    if let Some(ref v) = selected_version {
        println!("  固件版本:   {} — {}", v.version_name, v.filename);
    } else {
        println!("  固件版本:   暂不选择");
    }
    println!("  模板:       {}", template.display_name());
    println!("  COM 口:     {}", port.as_deref().unwrap_or("（未选择）"));
    println!("  soc_script: {soc_script}");
    println!("  Git init:   {git_init}");
    println!("───────────────────────────────────────────────────\n");

    if !Confirm::with_theme(&theme).with_prompt("确认创建项目？").default(true).interact()? {
        bail!("已取消");
    }

    Ok(WizardConfig {
        project_name,
        project_dir,
        model,
        selected_version,
        download_firmware,
        template,
        port,
        soc_script,
        download_soc_script,
        git_init,
    })
}

// ─── 向导执行 ─────────────────────────────────────────────────────────────────

fn execute_wizard(config: WizardConfig, manifest: &ResourceManifest, format: &OutputFormat) -> Result<()> {
    let dir = &config.project_dir;

    // 1. 脚手架：生成目录结构、TOML、模板文件、README
    luatos_project::scaffold_project_full(dir, &config.project_name, &config.model.chip, &config.model.name, &config.template)?;

    // 更新 flash.port（如用户选了串口）
    if config.port.is_some() {
        let mut project = luatos_project::Project::load(dir)?;
        project.flash.port = config.port.clone();
        project.save(dir)?;
    }

    // 2. 下载固件
    if config.download_firmware {
        if let Some(ref ver) = config.selected_version {
            download_firmware_entry(ver, manifest, dir)?;
        }
    }

    // 3. 下载 soc_script
    if config.download_soc_script && config.soc_script != "disable" {
        download_soc_script_entry(&config.soc_script, manifest, dir)?;
    }

    // 4. Git 初始化
    if config.git_init {
        init_git(dir)?;
    }

    // 5. 结果输出
    match format {
        OutputFormat::Text => {
            println!("\n✅ 项目创建成功！");
            println!("  目录:  {}", dir.display());
            println!("  型号:  {} ({})", config.model.name, config.model.chip);
            println!("  模板:  {}", config.template.display_name());
            if config.git_init {
                println!("  Git:   已初始化（含 .gitignore）");
            }
            println!("\n下一步：");
            println!("  cd {}", dir.display());
            println!("  luatos-cli project build");
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            let json = serde_json::json!({
                "status": "ok",
                "command": "project.wizard",
                "data": {
                    "name": config.project_name,
                    "dir": dir,
                    "model": config.model.name,
                    "chip": config.model.chip,
                    "template": config.template.id(),
                    "port": config.port,
                    "soc_script": config.soc_script,
                    "git_init": config.git_init,
                }
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
    }
    Ok(())
}

// ─── 下载辅助 ─────────────────────────────────────────────────────────────────

fn download_firmware_entry(ver: &VersionEntry, manifest: &ResourceManifest, project_dir: &Path) -> Result<()> {
    let output_dir = project_dir.join("resource");
    let file_entry = FileEntry {
        desc: ver.filename.clone(),
        filename: ver.filename.clone(),
        sha256: ver.sha256.clone(),
        size: ver.size,
        path: ver.path.clone(),
    };

    let pb = make_progress_bar(&ver.filename);
    let pb2 = pb.clone();
    let cb: luatos_resource::DownloadCallback = Box::new(move |evt| match evt {
        DownloadEvent::Progress { downloaded, total, .. } => pb2.set_position(*downloaded * 100 / total.max(&1)),
        DownloadEvent::Verified { dest, .. } => pb2.finish_with_message(format!("✓ {dest}")),
        DownloadEvent::FileFailed { filename } => pb2.abandon_with_message(format!("✗ {filename} 失败")),
        _ => {}
    });

    let report = download_files("firmware", &[file_entry], &manifest.mirrors, &output_dir, Some(&cb)).context("固件下载失败")?;
    if report.failed > 0 {
        log::warn!("固件下载失败 {}/{}", report.failed, report.total);
    }
    Ok(())
}

fn download_soc_script_entry(version: &str, manifest: &ResourceManifest, project_dir: &Path) -> Result<()> {
    let output_dir = project_dir.join("resource");

    let Some(public) = find_category(manifest, "public") else {
        log::warn!("manifest 中未找到 public 分类，跳过 soc_script 下载");
        return Ok(());
    };
    let Some(soc_script_child) = find_child(public, "soc_script") else {
        log::warn!("manifest 中未找到 soc_script 子项，跳过下载");
        return Ok(());
    };

    let version_filter = if version == "latest" { None } else { Some(version) };
    let files = collect_files_for_child(soc_script_child, version_filter);

    if files.is_empty() {
        log::warn!("未找到 soc_script 版本 '{version}'，跳过下载");
        return Ok(());
    }

    println!("⬇  下载 soc_script（{} 个文件）…", files.len());
    let report = download_files("soc_script", &files, &manifest.mirrors, &output_dir, None).context("soc_script 下载失败")?;
    if report.failed > 0 {
        log::warn!("soc_script 下载失败 {}/{}", report.failed, report.total);
    } else {
        println!("✓  soc_script 下载完成");
    }
    Ok(())
}

fn make_progress_bar(filename: &str) -> ProgressBar {
    let pb = ProgressBar::new(100);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}% {msg}")
            .unwrap()
            .progress_chars("#>-"),
    );
    pb.set_message(filename.to_string());
    pb
}

// ─── Git 辅助 ─────────────────────────────────────────────────────────────────

fn init_git(dir: &Path) -> Result<()> {
    let gitignore_path = dir.join(".gitignore");
    std::fs::write(&gitignore_path, GITIGNORE_CONTENT).with_context(|| format!("写入 .gitignore 失败: {}", gitignore_path.display()))?;

    match std::process::Command::new("git").arg("init").current_dir(dir).status() {
        Ok(s) if s.success() => log::info!("git init 成功: {}", dir.display()),
        Ok(s) => log::warn!("git init 退出码非零: {s}"),
        Err(e) => log::warn!("git 命令不可用，跳过 git init: {e}"),
    }
    Ok(())
}

const GITIGNORE_CONTENT: &str = r#"# LuatOS 构建产物
build/

# 固件资源（通过 luatos-cli resource download 下载）
resource/

# 项目存档
*.luatos

# 编辑器
.vscode/
.idea/
*.swp
*~
"#;

// ─── 工具函数 ─────────────────────────────────────────────────────────────────

/// 返回 manifest 本地缓存路径：`~/.luatos/manifest_cache.json`
pub fn manifest_cache_path() -> PathBuf {
    home_dir().join(".luatos").join("manifest_cache.json")
}

fn home_dir() -> PathBuf {
    std::env::var("USERPROFILE") // Windows
        .or_else(|_| std::env::var("HOME")) // Unix/macOS
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

fn resolve_project_dir(dir_arg: Option<&str>, project_name: &str) -> PathBuf {
    dir_arg.map(PathBuf::from).unwrap_or_else(|| PathBuf::from(format!("./{project_name}")))
}

/// 从 [`ResourceManifest`] 中提取模组型号列表。
///
/// 遍历所有非 "public" 分类，在其 "core" 子项中寻找 `LuatOS-SoC_*.soc` 文件，
/// 构建 [`ModelInfo`] 列表（按型号名字典序排序）。
pub fn extract_models(manifest: &ResourceManifest) -> Vec<ModelInfo> {
    let mut models = Vec::new();

    for category in &manifest.resouces {
        if category.name.eq_ignore_ascii_case("public") {
            continue;
        }
        let Some(core) = find_child(category, "core") else { continue };

        let versions: Vec<VersionEntry> = core
            .versions
            .iter()
            .flat_map(|ver| {
                ver.file_entries()
                    .into_iter()
                    .filter(|f| {
                        let lower = f.filename.to_lowercase();
                        lower.ends_with(".soc") && lower.contains("luatos-soc_")
                    })
                    .map(|f| VersionEntry {
                        version_name: ver.name.clone(),
                        filename: f.filename.clone(),
                        size: f.size,
                        sha256: f.sha256.clone(),
                        path: f.path.clone(),
                    })
            })
            .collect();

        if versions.is_empty() {
            continue;
        }

        let chip = chip_from_model_name(&category.name).to_string();
        let bitw = luatos_project::default_bitw(&chip);
        models.push(ModelInfo {
            name: category.name.clone(),
            chip,
            bitw,
            versions,
        });
    }

    models.sort_by(|a, b| a.name.cmp(&b.name));
    models
}

/// 从 manifest 获取可用的 soc_script 版本名称列表
pub fn get_soc_script_versions(manifest: &ResourceManifest) -> Vec<String> {
    let Some(public) = find_category(manifest, "public") else { return Vec::new() };
    let Some(soc_script) = find_child(public, "soc_script") else { return Vec::new() };
    soc_script.versions.iter().map(|v| v.name.clone()).collect()
}

// ─── 测试 ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造测试用 manifest（通过 serde JSON 解析，避免操作私有字段）
    fn make_test_manifest() -> ResourceManifest {
        serde_json::from_str(
            r#"{
            "version": 1,
            "mirrors": [],
            "resouces": [
                {
                    "name": "Air8101",
                    "childrens": [{"name": "core", "versions": [
                        {"name": "V2001", "files": [
                            ["底层固件", "LuatOS-SoC_V2001_Air8101.soc", "abc123", 2097152, "/Air8101/V2001/LuatOS-SoC_V2001_Air8101.soc"]
                        ]}
                    ]}]
                },
                {
                    "name": "Air780E",
                    "childrens": [{"name": "core", "versions": [
                        {"name": "V3003", "files": [
                            ["底层固件", "LuatOS-SoC_V3003_Air780E.soc", "def456", 3145728, "/Air780E/V3003/LuatOS-SoC_V3003_Air780E.soc"]
                        ]}
                    ]}]
                },
                {
                    "name": "public",
                    "childrens": [{"name": "soc_script", "versions": [
                        {"name": "v2026.04.10.16", "files": []},
                        {"name": "v2026.03.28.12", "files": []}
                    ]}]
                }
            ]
        }"#,
        )
        .unwrap()
    }

    #[test]
    fn extract_models_skips_public() {
        let manifest = make_test_manifest();
        let models = extract_models(&manifest);
        assert_eq!(models.len(), 2, "应跳过 public 分类");
        assert!(models.iter().all(|m| m.name != "public"), "不应包含 public");
    }

    #[test]
    fn extract_models_correct_chip_and_bitw() {
        let manifest = make_test_manifest();
        let models = extract_models(&manifest);

        let air8101 = models.iter().find(|m| m.name == "Air8101").unwrap();
        assert_eq!(air8101.chip, "bk72xx");
        assert_eq!(air8101.bitw, 32);

        let air780 = models.iter().find(|m| m.name == "Air780E").unwrap();
        assert_eq!(air780.chip, "ec7xx");
        assert_eq!(air780.bitw, 32);
    }

    #[test]
    fn extract_models_versions_populated() {
        let manifest = make_test_manifest();
        let models = extract_models(&manifest);
        let air8101 = models.iter().find(|m| m.name == "Air8101").unwrap();
        assert_eq!(air8101.versions.len(), 1);
        assert_eq!(air8101.versions[0].version_name, "V2001");
        assert_eq!(air8101.versions[0].filename, "LuatOS-SoC_V2001_Air8101.soc");
        assert_eq!(air8101.versions[0].size, 2097152);
    }

    #[test]
    fn extract_models_sorted_by_name() {
        let manifest = make_test_manifest();
        let models = extract_models(&manifest);
        let names: Vec<&str> = models.iter().map(|m| m.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "型号列表应按字典序排序");
    }

    #[test]
    fn get_soc_script_versions_returns_list() {
        let manifest = make_test_manifest();
        let versions = get_soc_script_versions(&manifest);
        assert_eq!(versions, vec!["v2026.04.10.16", "v2026.03.28.12"]);
    }

    #[test]
    fn get_soc_script_versions_empty_without_public() {
        let manifest = ResourceManifest {
            version: 1,
            mirrors: Vec::new(),
            resouces: Vec::new(),
        };
        assert!(get_soc_script_versions(&manifest).is_empty());
    }

    #[test]
    fn manifest_cache_path_correct() {
        let p = manifest_cache_path();
        let s = p.to_string_lossy();
        assert!(s.contains(".luatos"), "应在 .luatos 目录: {s}");
        assert!(s.contains("manifest_cache.json"), "文件名应正确: {s}");
    }

    #[test]
    fn noninteractive_config_defaults() {
        let manifest = make_test_manifest();
        let models = extract_models(&manifest);

        let args = WizardArgs {
            project_name: Some("test-proj".into()),
            project_dir: None,
            model: Some("Air8101".into()),
            firmware_version: None,
            template: Some("helloworld".into()),
            port: None,
            soc_script: None,
            no_git: true,
            no_download: true,
        };

        let config = build_config_noninteractive(args, models).unwrap();
        assert_eq!(config.project_name, "test-proj");
        assert_eq!(config.model.chip, "bk72xx");
        assert_eq!(config.template, TemplateKind::HelloWorld);
        assert_eq!(config.soc_script, "latest");
        assert!(!config.git_init);
        assert!(!config.download_firmware);
        assert!(!config.download_soc_script);
    }
}
