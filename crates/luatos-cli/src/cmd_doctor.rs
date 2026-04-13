// luatos-cli doctor — 环境诊断命令
//
// 一键检查开发环境是否就绪：
//   - 串口驱动与端口检测
//   - 项目配置有效性
//   - SOC 固件文件完整性
//   - 构建工具（内嵌 Lua 编译器）
//   - 固件资源目录
//   - 平台信息

use serde::Serialize;

use crate::{
    event::{self, MessageLevel},
    OutputFormat,
};

#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub name: String,
    pub passed: bool,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub platform: PlatformInfo,
    pub checks: Vec<CheckResult>,
    pub passed: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlatformInfo {
    pub os: String,
    pub arch: String,
    pub cli_version: String,
}

pub fn cmd_doctor(dir: &str, format: &OutputFormat) -> anyhow::Result<()> {
    event::emit_message(format, "doctor", MessageLevel::Info, "🔍 LuatOS 环境诊断中...")?;

    let platform = PlatformInfo {
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        cli_version: env!("CARGO_PKG_VERSION").to_string(),
    };

    let checks = vec![
        check_serial_ports(),
        check_project_config(dir),
        check_soc_file(dir),
        check_lua_compiler(),
        check_resource_dir(dir),
        check_script_dir(dir),
        check_cdn_connectivity(),
    ];

    let passed = checks.iter().filter(|c| c.passed).count();
    let failed = checks.iter().filter(|c| !c.passed).count();

    let report = DoctorReport {
        platform,
        checks: checks.clone(),
        passed,
        failed,
    };

    match format {
        OutputFormat::Text => {
            println!();
            println!("╔══════════════════════════════════════╗");
            println!("║     LuatOS 环境诊断报告              ║");
            println!("╚══════════════════════════════════════╝");
            println!();
            println!("  系统: {} / {}    CLI: v{}", report.platform.os, report.platform.arch, report.platform.cli_version);
            println!();

            for check in &report.checks {
                let icon = if check.passed { "✅" } else { "❌" };
                println!("  {icon} {:<20} {}", check.name, check.detail);
                if let Some(ref suggestion) = check.suggestion {
                    println!("     💡 {suggestion}");
                }
            }

            println!();
            println!("  结果: {passed} 通过, {failed} 未通过{}", if failed == 0 { " — 环境就绪 🎉" } else { " — 请按建议修复" });
            println!();
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            event::emit_result(format, "doctor", "ok", &report)?;
        }
    }

    Ok(())
}

fn check_serial_ports() -> CheckResult {
    let ports = luatos_serial::list_ports();
    if ports.is_empty() {
        CheckResult {
            name: "串口检测".into(),
            passed: false,
            detail: "未发现串口设备".into(),
            suggestion: Some("请检查 USB 线连接，安装 CH343/CP210x 驱动，或查看设备管理器".into()),
        }
    } else {
        let names: Vec<&str> = ports.iter().map(|p| p.port_name.as_str()).collect();
        CheckResult {
            name: "串口检测".into(),
            passed: true,
            detail: format!("发现 {} 个串口: {}", ports.len(), names.join(", ")),
            suggestion: None,
        }
    }
}

fn check_project_config(dir: &str) -> CheckResult {
    let path = std::path::Path::new(dir);
    let config_path = path.join("luatos-project.toml");

    if !config_path.exists() {
        return CheckResult {
            name: "项目配置".into(),
            passed: false,
            detail: format!("未找到 {}", config_path.display()),
            suggestion: Some("运行 `luatos-cli project wizard` 创建项目，或 cd 到项目目录后重试".into()),
        };
    }

    match luatos_project::Project::load(path) {
        Ok(project) => CheckResult {
            name: "项目配置".into(),
            passed: true,
            detail: format!("项目 \"{}\" (芯片: {}, bitw: {})", project.project.name, project.project.chip, project.build.bitw),
            suggestion: None,
        },
        Err(e) => CheckResult {
            name: "项目配置".into(),
            passed: false,
            detail: format!("配置文件解析失败: {e}"),
            suggestion: Some("检查 luatos-project.toml 语法是否正确".into()),
        },
    }
}

fn check_soc_file(dir: &str) -> CheckResult {
    let path = std::path::Path::new(dir);

    // 优先读取项目配置中的 soc_file
    let soc_path = match luatos_project::Project::load(path) {
        Ok(project) => project.flash.soc_file.map(|f| path.join(&f)),
        Err(_) => None,
    };

    // 如果配置中没有指定，扫描目录下的 .soc 文件
    let soc_path = soc_path.or_else(|| {
        std::fs::read_dir(dir)
            .ok()?
            .filter_map(|e| e.ok())
            .find(|e| e.path().extension().is_some_and(|ext| ext.eq_ignore_ascii_case("soc")))
            .map(|e| e.path())
    });

    match soc_path {
        None => CheckResult {
            name: "SOC 固件".into(),
            passed: false,
            detail: "未找到 .soc 固件文件".into(),
            suggestion: Some("运行 `luatos-cli resource download <模组> <版本>` 下载固件".into()),
        },
        Some(ref p) if !p.exists() => CheckResult {
            name: "SOC 固件".into(),
            passed: false,
            detail: format!("配置的 SOC 文件不存在: {}", p.display()),
            suggestion: Some("检查 flash.soc_file 路径，或重新下载固件".into()),
        },
        Some(ref p) => match luatos_soc::read_soc_info(&p.to_string_lossy()) {
            Ok(info) => {
                let chip = &info.chip.chip_type;
                let version = info.version.map(|v| v.to_string()).unwrap_or_else(|| "?".into());
                CheckResult {
                    name: "SOC 固件".into(),
                    passed: true,
                    detail: format!("{} (芯片: {chip}, 版本: {version})", p.display()),
                    suggestion: None,
                }
            }
            Err(e) => CheckResult {
                name: "SOC 固件".into(),
                passed: false,
                detail: format!("SOC 文件读取失败: {e}"),
                suggestion: Some("文件可能损坏，请重新下载".into()),
            },
        },
    }
}

fn check_lua_compiler() -> CheckResult {
    // 尝试用内嵌 Lua 编译器编译一段简单代码
    let test_source = b"print('hello')";
    match luatos_luadb::build::compile_lua_bytes(test_source, "test", false, 32) {
        Ok(bytecode) if !bytecode.is_empty() => CheckResult {
            name: "Lua 编译器".into(),
            passed: true,
            detail: format!("内嵌 luac 可用 (32-bit, 输出 {} 字节)", bytecode.len()),
            suggestion: None,
        },
        Ok(_) => CheckResult {
            name: "Lua 编译器".into(),
            passed: false,
            detail: "编译器返回空字节码".into(),
            suggestion: Some("请报告此问题到 GitHub Issues".into()),
        },
        Err(e) => CheckResult {
            name: "Lua 编译器".into(),
            passed: false,
            detail: format!("编译测试失败: {e}"),
            suggestion: Some("尝试重新构建: cargo build --release -p luatos-cli".into()),
        },
    }
}

fn check_resource_dir(dir: &str) -> CheckResult {
    let path = std::path::Path::new(dir);

    // 从项目配置获取 resource_dir，默认 "resource/"
    let resource_dir = match luatos_project::Project::load(path) {
        Ok(project) => path.join(&project.build.resource_dir),
        Err(_) => path.join("resource"),
    };

    if !resource_dir.exists() {
        return CheckResult {
            name: "资源目录".into(),
            passed: false,
            detail: format!("{} 不存在", resource_dir.display()),
            suggestion: Some("运行 `luatos-cli resource download <模组> <版本> --output resource/` 下载固件资源".into()),
        };
    }

    // 统计资源目录下文件数
    let file_count = walkdir::WalkDir::new(&resource_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .count();

    if file_count == 0 {
        CheckResult {
            name: "资源目录".into(),
            passed: false,
            detail: format!("{} 为空", resource_dir.display()),
            suggestion: Some("运行 `luatos-cli resource download <模组> <版本>` 下载固件资源".into()),
        }
    } else {
        CheckResult {
            name: "资源目录".into(),
            passed: true,
            detail: format!("{} ({} 个文件)", resource_dir.display(), file_count),
            suggestion: None,
        }
    }
}

fn check_script_dir(dir: &str) -> CheckResult {
    let path = std::path::Path::new(dir);

    let script_dirs = match luatos_project::Project::load(path) {
        Ok(project) => project.build.script_dirs,
        Err(_) => vec!["lua/".to_string()],
    };

    let mut found: Vec<String> = Vec::new();
    let mut lua_count = 0usize;

    for sd in &script_dirs {
        let full = path.join(sd);
        if full.exists() {
            let count = walkdir::WalkDir::new(&full)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_file() && e.path().extension().is_some_and(|ext| ext == "lua" || ext == "luac"))
                .count();
            lua_count += count;
            found.push(format!("{sd} ({count} 个 Lua 文件)"));
        }
    }

    if found.is_empty() {
        CheckResult {
            name: "脚本目录".into(),
            passed: false,
            detail: format!("脚本目录不存在: {}", script_dirs.join(", ")),
            suggestion: Some("创建 lua/ 目录并添加 main.lua 入口文件".into()),
        }
    } else if lua_count == 0 {
        CheckResult {
            name: "脚本目录".into(),
            passed: false,
            detail: "脚本目录存在但无 .lua 文件".into(),
            suggestion: Some("在 lua/ 目录下创建 main.lua 入口文件".into()),
        }
    } else {
        CheckResult {
            name: "脚本目录".into(),
            passed: true,
            detail: found.join("; "),
            suggestion: None,
        }
    }
}

fn check_cdn_connectivity() -> CheckResult {
    let url = luatos_resource::MANIFEST_URLS[0];
    match ureq::get(url).timeout(std::time::Duration::from_secs(5)).call() {
        Ok(resp) if resp.status() == 200 => CheckResult {
            name: "CDN 连通性".into(),
            passed: true,
            detail: "固件 CDN 可达".into(),
            suggestion: None,
        },
        Ok(resp) => CheckResult {
            name: "CDN 连通性".into(),
            passed: false,
            detail: format!("CDN 返回 HTTP {}", resp.status()),
            suggestion: Some("检查网络连接，或配置代理".into()),
        },
        Err(e) => CheckResult {
            name: "CDN 连通性".into(),
            passed: false,
            detail: format!("CDN 连接失败: {e}"),
            suggestion: Some("检查网络连接；离线模式下可忽略此项".into()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_serial_ports_runs() {
        let result = check_serial_ports();
        assert_eq!(result.name, "串口检测");
        // 不要求通过（CI 无串口），但结构正确
    }

    #[test]
    fn check_lua_compiler_passes() {
        let result = check_lua_compiler();
        assert!(result.passed, "内嵌 Lua 编译器应当可用: {}", result.detail);
    }

    #[test]
    fn check_project_config_missing() {
        let result = check_project_config("__nonexistent_dir_12345__");
        assert!(!result.passed);
        assert!(result.suggestion.is_some());
    }

    #[test]
    fn doctor_report_serializes() {
        let report = DoctorReport {
            platform: PlatformInfo {
                os: "windows".into(),
                arch: "x86_64".into(),
                cli_version: "1.6.2".into(),
            },
            checks: vec![CheckResult {
                name: "test".into(),
                passed: true,
                detail: "ok".into(),
                suggestion: None,
            }],
            passed: 1,
            failed: 0,
        };
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"passed\":1"));
    }
}
