//! `trun` 子命令（test run）
//!
//! 一站式完成"读 testcase → 合并 ctx.json → 合成 (script.bin 已烧入
//! ctx.json + 可选 soc) → 刷机 → 抓日志 → 关键字校验"，简化
//! luatos-autotest-v2 在开发期的临时合成。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use serde::Serialize;

use luatos_soc::ChipFamily;
use luatos_testcase::{build_ctx, build_script_bin, inject_identifiers, resolve_testcase, scan_testcases, write_ctx_to_temp, ResolvedTestcase};

use crate::cmd_flash;
use crate::cmd_log;
use crate::event::{self, MessageLevel};
use crate::OutputFormat;
use luatos_log::LogEntry;

/// `trun` 子命令
#[derive(Subcommand, Debug)]
pub enum TrunCommands {
    /// 跑一个 testcase（默认行为：合成 → 刷机 → 抓日志 → 分析）
    Run(Box<TrunRunArgs>),
    /// 列出 LuatOS 仓库下的所有 testcase
    List {
        /// LuatOS 仓库根（默认向上找）
        #[arg(long)]
        luatos_root: Option<String>,
        /// 按子目录过滤（如 function_testcase_network）
        #[arg(long)]
        category: Option<String>,
    },
    /// 校验 testcase 目录结构（不刷机）
    Validate {
        /// testcase 路径或名称
        #[arg(value_name = "TESTCASE")]
        testcase: String,
        /// LuatOS 仓库根（默认向上找）
        #[arg(long)]
        luatos_root: Option<String>,
    },
}

/// `testcase run` 的所有参数
#[derive(Args, Debug, Clone)]
pub struct TrunRunArgs {
    /// testcase 路径或名称（名称时递归 `<LuatOS>/testcase/**/<name>/`）
    #[arg(value_name = "TESTCASE")]
    pub testcase: String,

    /// LuatOS 仓库根
    #[arg(long)]
    pub luatos_root: Option<String>,

    /// 源 .soc 文件
    #[arg(long)]
    pub soc: String,

    /// 串口（如 COM6）
    #[arg(long)]
    pub port: String,

    /// 覆盖 log 波特率（默认从 soc.info.json 读）
    #[arg(long)]
    pub baud: Option<u32>,

    /// 公共脚本目录（默认 `<LuatOS>/testcase/common/scripts`）
    #[arg(long)]
    pub common_scripts: Option<String>,

    /// 刷机进度步进（1-50%）
    #[arg(long, default_value = "10", value_parser = clap::value_parser!(u8).range(1..=50))]
    pub progress_step: u8,

    #[command(flatten)]
    pub reset: crate::reset_args::ResetArgs,

    /// 全量刷机：刷入底层固件 + 脚本分区（默认仅刷脚本分区）
    #[arg(long)]
    pub full: bool,

    /// 仅刷机：合成 + 刷机 + 注入 ctx.json 后返回，不抓取日志也不做关键字判定
    #[arg(long)]
    pub flash_only: bool,

    /// 刷机前清除文件系统分区
    #[arg(long)]
    pub clear_fs: bool,

    /// 保留合成的 script.bin / soc 到指定目录
    #[arg(long, value_name = "DIR")]
    pub keep_soc: Option<String>,

    /// PASS 关键字（可重复传入，逗号分隔）
    #[arg(long = "keyword", value_delimiter = ',')]
    pub keywords: Vec<String>,

    /// 命中即 FAIL 的关键字（可重复，逗号分隔）
    #[arg(long = "fail-keyword", value_delimiter = ',')]
    pub fail_keywords: Vec<String>,

    /// 关键字匹配字段 (默认 message, 真实解析结果)
    #[arg(long, value_enum, default_value_t = MatchField::Message)]
    pub match_field: MatchField,

    /// 启用智能诊断（默认开启）
    #[arg(long, default_value_t = true)]
    pub smart: bool,

    /// 抓日志超时秒（覆盖 metas.timeout）
    #[arg(long)]
    pub timeout: Option<u64>,

    /// 关键字命中后立即结束抓取
    #[arg(long, default_value_t = true)]
    pub early_exit: bool,

    /// 额外 ctx.json 字段（与 local_ctx.json 合并）
    #[arg(long, value_name = "FILE")]
    pub ctx: Option<String>,

    /// 完全覆盖 ctx.json（不再合并）
    #[arg(long, value_name = "FILE")]
    pub full_ctx: Option<String>,
}

/// testcase 阶段名（用于 emit_message 的 phase 字段）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Build,
    Ctx,
    Flash,
    LogCapture,
    Finalize,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Build => "build",
            Phase::Ctx => "ctx",
            Phase::Flash => "flash",
            Phase::LogCapture => "log_capture",
            Phase::Finalize => "finalize",
        }
    }
}

/// testcase 最终结果
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Pass,
    Fail,
    /// 保留以便未来扩展（如 ctx.json 回传重新接回后用于"未收到 result"场景）。
    /// 当前 trun 不再起监听器，永远不会构造。
    #[allow(dead_code)]
    Indeterminate,
    #[allow(dead_code)]
    Error,
}

impl Verdict {
    #[allow(dead_code)]
    pub fn as_str(&self) -> &'static str {
        match self {
            Verdict::Pass => "pass",
            Verdict::Fail => "fail",
            Verdict::Indeterminate => "indeterminate",
            Verdict::Error => "error",
        }
    }
    pub fn exit_code(&self) -> i32 {
        match self {
            Verdict::Pass => 0,
            Verdict::Fail => 1,
            Verdict::Indeterminate => 2,
            Verdict::Error => 3,
        }
    }
}

/// 关键字命中结果
#[derive(Debug, Clone, Serialize)]
pub struct KeywordHit {
    pub keyword: String,
    pub found: bool,
}

/// 产物路径汇总
#[derive(Debug, Clone, Serialize, Default)]
pub struct ArtifactSummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script_bin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub combined_soc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_dir: Option<String>,
}

/// 智能诊断条目
#[derive(Debug, Clone, Serialize)]
pub struct SmartDiagnosticEntry {
    pub level: String,
    pub category: String,
    pub message: String,
    pub count: u32,
}

/// testcase 最终输出
#[derive(Debug, Clone, Serialize)]
pub struct TrunOutcome {
    pub verdict: Verdict,
    pub testcase: String,
    pub chip: String,
    pub soc: String,
    pub port: String,
    pub test_id: String,
    pub runner_id: String,
    pub runner_mode: String,
    pub build_path: String, // "flash_script" / "flash_soc"
    pub keywords: Vec<KeywordHit>,
    pub fail_keywords: Vec<KeywordHit>,
    pub matched_fail_keywords: Vec<String>,
    pub fast_failed: bool,
    pub boot_log_count: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub smart_diagnostics: Vec<SmartDiagnosticEntry>,
    pub artifacts: ArtifactSummary,
    pub phase_durations_ms: BTreeMap<String, u64>,
    pub elapsed_ms: u64,
}

/// 入口
pub fn cmd_trun(cmd: &TrunCommands, format: &OutputFormat) -> Result<i32> {
    match cmd {
        TrunCommands::Run(args) => {
            let verdict = cmd_trun_run(args, format)?;
            Ok(verdict.exit_code())
        }
        TrunCommands::List { luatos_root, category } => {
            cmd_trun_list(luatos_root.as_deref(), category.as_deref(), format)?;
            Ok(0)
        }
        TrunCommands::Validate { testcase, luatos_root } => {
            cmd_trun_validate(testcase, luatos_root.as_deref(), format)?;
            Ok(0)
        }
    }
}

fn cmd_trun_list(luatos_root: Option<&str>, category: Option<&str>, format: &OutputFormat) -> Result<()> {
    let root = resolve_luatos_root(luatos_root)?;
    let mut found = scan_testcases(&root)?;
    if let Some(c) = category {
        found.retain(|t| t.category == c);
    }
    match format {
        OutputFormat::Text => {
            if found.is_empty() {
                println!("未找到任何 testcase（root: {}）", root.display());
            } else {
                println!("找到 {} 个 testcase:", found.len());
                for t in &found {
                    let cat = if t.category.is_empty() { "(top-level)".to_string() } else { t.category.clone() };
                    println!("  {}/{} — {} [{}]", cat, t.name, t.metas.description, t.discovery_source.as_str());
                }
            }
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            let items: Vec<_> = found
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "category": t.category,
                        "name": t.name,
                        "path": t.path.display().to_string(),
                        "description": t.metas.description,
                        "timeout": t.metas.timeout,
                        "priority": t.metas.priority,
                        "source": t.discovery_source.as_str(),
                    })
                })
                .collect();
            event::emit_result(format, "testcase.list", "ok", serde_json::json!({ "items": items, "count": items.len() }))?;
        }
    }
    Ok(())
}

fn cmd_trun_validate(testcase: &str, luatos_root: Option<&str>, format: &OutputFormat) -> Result<()> {
    let root = resolve_luatos_root(luatos_root)?;
    let resolved = resolve_testcase(testcase, &root)?;
    match format {
        OutputFormat::Text => {
            println!("testcase: {}/{}", resolved.category, resolved.name);
            println!("path:     {}", resolved.path.display());
            println!("source:   {}", resolved.discovery_source.as_str());
            println!("description: {}", resolved.metas.description);
            println!(
                "timeout: {}s, priority: {}, action_count: {}",
                resolved.metas.timeout, resolved.metas.priority, resolved.metas.action_count
            );
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            event::emit_result(
                format,
                "testcase.validate",
                "ok",
                serde_json::json!({
                    "category": resolved.category,
                    "name": resolved.name,
                    "path": resolved.path.display().to_string(),
                    "description": resolved.metas.description,
                    "timeout": resolved.metas.timeout,
                    "priority": resolved.metas.priority,
                }),
            )?;
        }
    }
    Ok(())
}

/// testcase run 主流程
pub fn cmd_trun_run(args: &TrunRunArgs, format: &OutputFormat) -> Result<Verdict> {
    let start_total = Instant::now();
    let mut phase_durations: BTreeMap<String, u64> = BTreeMap::new();
    let cancel = Arc::new(AtomicBool::new(false));
    install_ctrlc(format, cancel.clone());

    // 1. 解析 testcase
    let root = resolve_luatos_root(args.luatos_root.as_deref())?;
    let resolved = resolve_testcase(&args.testcase, &root).with_context(|| format!("无法解析 testcase: {}", args.testcase))?;
    event::emit_message(format, "testcase.run", MessageLevel::Info, format!("testcase = {}/{}", resolved.category, resolved.name))?;

    // 2. 读取 soc info
    let soc_info = luatos_soc::read_soc_info(&args.soc).with_context(|| format!("无法读取 soc: {}", args.soc))?;
    let chip = soc_info.chip.chip_type.clone();
    event::emit_message(format, "testcase.run", MessageLevel::Info, format!("chip = {chip}"))?;

    // 3. 合并关键字（metas.keywords + CLI --keyword）
    let mut keywords = resolved.metas.keywords.clone().unwrap_or_default();
    keywords.extend(args.keywords.iter().cloned());
    let mut fail_keywords = resolved.metas.fail_keywords.clone().unwrap_or_default();
    fail_keywords.extend(args.fail_keywords.iter().cloned());

    let timeout = args.timeout.unwrap_or(resolved.metas.timeout);

    // 4. 阶段：ctx（合并 + 注入 test_id，**不**起 HTTP 监听器）
    let start = Instant::now();
    let common_scripts = resolve_common_scripts(args, &root);
    let built_ctx = prepare_ctx(args, &root, common_scripts.as_deref())?;
    let ctx_test_id = built_ctx.test_id.clone();
    // 把 ctx.json 写入临时目录，保持到 flash 阶段结束
    let (ctx_tmp, _ctx_path) = write_ctx_to_temp(&built_ctx.value).context("write ctx.json to temp failed")?;
    phase_durations.insert(Phase::Ctx.as_str().to_string(), start.elapsed().as_millis() as u64);

    // 5. 阶段：build（自动把 ctx.json 烧入 script.bin）
    let start = Instant::now();
    let script_bin = build_script_image_with_ctx(args, &resolved, &chip, &ctx_tmp)?;
    let script_bin_path = write_script_bin(&script_bin, args.keep_soc.as_deref())?;
    // 仅非 bk72xx 在全量模式时才需要合成 combined.soc
    let combined_soc = if args.full && !matches!(soc_info.family(), ChipFamily::Bk72xx) {
        Some(combine_soc(args, &script_bin, &soc_info)?)
    } else {
        None
    };
    phase_durations.insert(Phase::Build.as_str().to_string(), start.elapsed().as_millis() as u64);

    // 6. 阶段：flash
    let start = Instant::now();
    let boot_entries = flash_device(
        args,
        &resolved,
        common_scripts.as_deref(),
        &ctx_tmp,
        &script_bin_path,
        combined_soc.as_deref(),
        format,
        &cancel,
    )?;
    phase_durations.insert(Phase::Flash.as_str().to_string(), start.elapsed().as_millis() as u64);

    // 7. 阶段：log_capture（--flash-only 时跳过）
    let (all_entries, smart_diag) = if args.flash_only {
        event::emit_message(format, "testcase.run", MessageLevel::Info, "flash-only mode: skip log capture and verdict")?;
        (boot_entries, Vec::new())
    } else {
        let start = Instant::now();
        let (extra_entries, smart_diag) = capture_log(args, &boot_entries, timeout, format, &cancel)?;
        let mut all_entries = boot_entries;
        all_entries.extend(extra_entries);
        phase_durations.insert(Phase::LogCapture.as_str().to_string(), start.elapsed().as_millis() as u64);
        (all_entries, smart_diag)
    };

    // 8. 阶段：finalize（--flash-only 时跳过关键字判定）
    let start = Instant::now();
    let (verdict, keyword_hits, fail_keyword_hits, matched_fail_keywords, fast_failed) = if args.flash_only {
        (Verdict::Pass, Vec::new(), Vec::new(), Vec::new(), false)
    } else {
        let keyword_hits: Vec<KeywordHit> = keywords
            .iter()
            .map(|k| KeywordHit {
                keyword: k.clone(),
                found: match_keyword(&all_entries, k, args.match_field),
            })
            .collect();
        let fail_keyword_hits: Vec<KeywordHit> = fail_keywords
            .iter()
            .map(|k| KeywordHit {
                keyword: k.clone(),
                found: match_keyword(&all_entries, k, args.match_field),
            })
            .collect();
        let matched_fail_keywords: Vec<String> = fail_keyword_hits.iter().filter(|h| h.found).map(|h| h.keyword.clone()).collect();
        let fast_failed = !matched_fail_keywords.is_empty();
        let all_passed = keyword_hits.iter().all(|h| h.found) && !fast_failed;
        let verdict = derive_verdict(all_passed, fast_failed);
        (verdict, keyword_hits, fail_keyword_hits, matched_fail_keywords, fast_failed)
    };
    phase_durations.insert(Phase::Finalize.as_str().to_string(), start.elapsed().as_millis() as u64);
    let outcome = TrunOutcome {
        verdict: verdict.clone(),
        testcase: format!("{}/{}", resolved.category, resolved.name),
        chip,
        soc: args.soc.clone(),
        port: args.port.clone(),
        test_id: ctx_test_id,
        runner_id: String::new(),
        runner_mode: "cli-debug".into(),
        build_path: if args.full { "flash_full".into() } else { "flash_script".into() },
        keywords: keyword_hits,
        fail_keywords: fail_keyword_hits,
        matched_fail_keywords,
        fast_failed,
        boot_log_count: all_entries.len(),
        smart_diagnostics: smart_diag,
        artifacts: ArtifactSummary {
            script_bin: Some(script_bin_path.display().to_string()),
            combined_soc: combined_soc.as_ref().map(|p| p.display().to_string()),
            keep_dir: args.keep_soc.clone(),
        },
        phase_durations_ms: phase_durations,
        elapsed_ms: start_total.elapsed().as_millis() as u64,
    };

    emit_outcome(format, &outcome)?;
    Ok(verdict)
}

// 下面是各阶段辅助函数，串在主流程中

fn resolve_luatos_root(arg: Option<&str>) -> Result<PathBuf> {
    if let Some(s) = arg {
        return Ok(PathBuf::from(s));
    }
    // 默认从 CWD 向上找，命中 `LuatOS/testcase/` 标记
    let mut cur = std::env::current_dir().context("failed to get current dir")?;
    loop {
        if cur.join("testcase").is_dir() {
            return Ok(cur);
        }
        if !cur.pop() {
            bail!("无法自动定位 LuatOS 仓库根（未找到 testcase/ 标记），请传 --luatos-root");
        }
    }
}

fn resolve_common_scripts(args: &TrunRunArgs, root: &Path) -> Option<PathBuf> {
    if let Some(s) = &args.common_scripts {
        return Some(PathBuf::from(s));
    }
    let p = root.join("testcase").join("common").join("scripts");
    if p.is_dir() {
        Some(p)
    } else {
        None
    }
}

/// 注册 ctrl-c handler, 失败时静默忽略。
///
/// 同一进程内第二次注册 ctrlc handler 会得到 `Error::MultipleHandlers`,
/// 在 `trun` 流里 `cmd_flash_run` 也会注册, 这边先到先得。
/// 外部 `cancel` 仍然能被 `cmd_flash_run` 自己的 stop atomic 间接触发
/// (因为两者都响应 ctrl-c); 而 trun 自己的 cancel 在 flash 完成后
/// 不再用, 所以这是安全降级。
fn install_ctrlc(format: &OutputFormat, cancel: Arc<AtomicBool>) {
    let f = *format;
    let c = cancel.clone();
    let _ = ctrlc::set_handler(move || {
        let _ = event::emit_message(&f, "testcase.run", MessageLevel::Warn, "Cancelling testcase run...");
        c.store(true, Ordering::Relaxed);
    });
}

fn build_script_image_with_ctx(args: &TrunRunArgs, resolved: &ResolvedTestcase, chip: &str, ctx_tmp: &tempfile::TempDir) -> Result<Vec<u8>> {
    let common = resolve_common_scripts(args, &resolve_luatos_root(args.luatos_root.as_deref())?);
    let script_dirs: Vec<&Path> = vec![resolved.scripts_dir.as_path()];
    build_script_bin(&script_dirs, common.as_deref(), Some(ctx_tmp.path()), chip).context("build script.bin 失败")
}

fn write_script_bin(image: &[u8], keep_dir: Option<&str>) -> Result<PathBuf> {
    let dir = match keep_dir {
        Some(d) => PathBuf::from(d),
        None => std::env::temp_dir().join("luatos_testcase"),
    };
    std::fs::create_dir_all(&dir).ok();
    let p = dir.join("script.bin");
    std::fs::write(&p, image).with_context(|| format!("failed to write {}", p.display()))?;
    Ok(p)
}

fn combine_soc(args: &TrunRunArgs, script_bin: &[u8], soc_info: &luatos_soc::SocInfo) -> Result<PathBuf> {
    let dir = match &args.keep_soc {
        Some(d) => PathBuf::from(d),
        None => std::env::temp_dir().join("luatos_testcase"),
    };
    std::fs::create_dir_all(&dir).ok();
    let out = dir.join("combined.soc");
    if soc_info.family().is_ec718() {
        let script_addr = soc_info.script_addr();
        luatos_soc::combine_ec7xx_soc(&args.soc, script_bin, script_addr, out.to_str().unwrap()).context("combine_ec7xx_soc failed")?;
    } else {
        // 非 EC7xx：unpack → 替换 script.bin → pack
        let tmp = tempfile::tempdir().context("tempdir failed")?;
        luatos_soc::unpack_soc(&args.soc, tmp.path()).context("unpack_soc failed")?;
        let script_in_unpack = tmp.path().join("script.bin");
        std::fs::write(&script_in_unpack, script_bin).context("write script.bin in unpack dir failed")?;
        luatos_soc::pack_soc(tmp.path(), out.to_str().unwrap()).context("pack_soc failed")?;
    }
    Ok(out)
}

fn prepare_ctx(args: &TrunRunArgs, root: &Path, _common_scripts: Option<&Path>) -> Result<luatos_testcase::CtxBuildResult> {
    let mut built = merge_ctx_layers(args, root)?;
    inject_identifiers(&mut built, None);
    Ok(built)
}

fn merge_ctx_layers(args: &TrunRunArgs, root: &Path) -> Result<luatos_testcase::CtxBuildResult> {
    build_ctx(root, None, args.ctx.as_deref().map(Path::new), args.full_ctx.as_deref().map(Path::new), 0)
}

#[allow(clippy::too_many_arguments)]
fn flash_device(
    args: &TrunRunArgs,
    resolved: &ResolvedTestcase,
    common_scripts: Option<&Path>,
    ctx_tmp: &tempfile::TempDir,
    script_bin_path: &Path,
    combined_soc: Option<&Path>,
    format: &OutputFormat,
    cancel: &Arc<AtomicBool>,
) -> Result<Vec<LogEntry>> {
    if cancel.load(Ordering::Relaxed) {
        bail!("cancelled before flash");
    }

    let soc_for_flash = combined_soc.and_then(|p| p.to_str()).unwrap_or(args.soc.as_str());
    let on_progress = cmd_flash::make_progress_callback(format, "testcase.run", args.progress_step);

    // 收集脚本源目录：testcase scripts → common scripts → ctx 临时目录（最后，最高优先级）
    let mut script_folders: Vec<String> = vec![resolved.scripts_dir.display().to_string()];
    if let Some(common) = common_scripts {
        script_folders.push(common.display().to_string());
    }
    script_folders.push(ctx_tmp.path().display().to_string());

    let info = luatos_soc::read_soc_info(soc_for_flash)?;
    let family = info.family();

    match family {
        ChipFamily::Bk72xx => {
            if args.clear_fs {
                let cb = cmd_flash::make_progress_callback(format, "testcase.run", args.progress_step);
                luatos_flash::bk7258::clear_filesystem(soc_for_flash, &args.port, cancel.clone(), cb).context("clear filesystem failed")?;
            }

            if args.full {
                if args.flash_only {
                    // flash_only 模式：只负责合成+刷机，不抓 boot log，避免串口占用。
                    let folders_refs: Option<Vec<&str>> = Some(script_folders.iter().map(|s| s.as_str()).collect());
                    luatos_flash::bk7258::flash_bk7258(soc_for_flash, folders_refs.as_deref(), &args.port, args.baud, cancel.clone(), on_progress, false)
                        .context("flash run failed")?;
                    // air602_flash.exe 刷完 firmware 后会自行重启；随后 native ISP 刷 script 分区
                    // 仅关闭串口并不会让设备从 flash 启动，因此需要显式 RTS 复位确保新 script 跑起来。
                    args.reset.execute(&args.port).context("post-flash reset failed")?;
                } else {
                    // 全量：底层固件 + 脚本分区
                    cmd_flash::cmd_flash_run(
                        soc_for_flash,
                        &args.port,
                        args.baud,
                        Some(&script_folders),
                        args.progress_step,
                        format,
                        &args.reset,
                        None,
                        0,
                    )
                    .context("flash run failed")?;
                }
            } else {
                // 仅刷脚本分区
                cmd_flash::cmd_flash_partition(
                    "script",
                    soc_for_flash,
                    &args.port,
                    Some(&script_folders),
                    args.progress_step,
                    format,
                    &args.reset,
                    None,
                    args.baud,
                )
                .context("flash script partition failed")?;
            }
        }
        _ => {
            if combined_soc.is_some() {
                // 冷路径：刷整个 soc
                cmd_flash::cmd_flash_run(soc_for_flash, &args.port, args.baud, None, args.progress_step, format, &args.reset, None, 0).context("flash run failed")?;
            } else {
                // 快路径：刷 script.bin
                let bin_str = script_bin_path.to_str().context("script_bin_path is not valid utf-8")?;
                cmd_flash::cmd_flash_script_bin(soc_for_flash, &args.port, bin_str, &on_progress).context("flash script.bin failed")?;
            }
        }
    }

    // 简化：boot_entries 暂时为空，由后续 log_capture 阶段抓取
    Ok(Vec::new())
}

fn capture_log(
    args: &TrunRunArgs,
    _boot_entries: &[LogEntry],
    timeout_secs: u64,
    format: &OutputFormat,
    cancel: &Arc<AtomicBool>,
) -> Result<(Vec<LogEntry>, Vec<SmartDiagnosticEntry>)> {
    let early_kw: Vec<String> = if args.early_exit { args.keywords.clone() } else { Vec::new() };

    let outcome = cmd_log::capture_log_lines(&args.soc, &args.port, args.baud, timeout_secs, &early_kw, format, cancel).context("capture_log_lines failed")?;

    let smart_entries: Vec<SmartDiagnosticEntry> = outcome
        .diagnostics
        .into_iter()
        .map(|d| SmartDiagnosticEntry {
            level: format!("{:?}", d.severity),
            category: d.rule,
            message: d.suggestion,
            count: 1,
        })
        .collect();

    Ok((outcome.entries, smart_entries))
}

/// 关键字匹配字段模式
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum MatchField {
    /// 仅 message 字段 (LuatosParser 剥掉帧头后的内容, 默认)
    #[default]
    Message,
    /// 仅 raw 字段 (含 SOH+len+type 帧头, 旧行为, 降级路径)
    Raw,
    /// message / module / level 任一字段包含 keyword 即命中
    /// 注意: 任一单字符关键字 ("I"/"W"/"E"/"D"/"T"/"?"/"-") 都会匹配 level 字段
    /// (例如 `--match-field any --keyword "I"` 会命中每条 Info 日志, 通常不是想要的)
    Any,
    /// message / module / level 全部字段都包含 keyword 才命中
    /// 注意: 任一单字符关键字 ("I"/"W"/"E"/"D"/"T"/"?"/"-") 都会匹配 level 字段
    /// (例如 `--match-field all --keyword "I"` 会命中每条 Info 日志, 通常不是想要的)
    All,
}

/// 在 LogEntry 列表上按 field 模式匹配单个 keyword
pub fn match_keyword(entries: &[LogEntry], keyword: &str, field: MatchField) -> bool {
    entries.iter().any(|e| match field {
        MatchField::Raw => e.raw.contains(keyword),
        MatchField::Message => e.message.contains(keyword),
        MatchField::Any => e.message.contains(keyword) || e.module.as_deref().unwrap_or("").contains(keyword) || e.level.as_str().contains(keyword),
        MatchField::All => e.message.contains(keyword) && e.module.as_deref().unwrap_or("").contains(keyword) && e.level.as_str().contains(keyword),
    })
}

fn derive_verdict(all_passed: bool, fast_failed: bool) -> Verdict {
    if fast_failed {
        return Verdict::Fail;
    }
    if !all_passed {
        return Verdict::Fail;
    }
    Verdict::Pass
}

fn emit_outcome(format: &OutputFormat, outcome: &TrunOutcome) -> Result<()> {
    let status = match outcome.verdict {
        Verdict::Pass => "ok",
        Verdict::Fail => "fail",
        Verdict::Indeterminate => "indeterminate",
        Verdict::Error => "error",
    };
    event::emit_result(format, "testcase.run", status, serde_json::to_value(outcome)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use luatos_log::{LogEntry, LogLevel};

    fn entry(msg: &str, module: Option<&str>, level: LogLevel) -> LogEntry {
        let level_str = level.as_str();
        LogEntry {
            timestamp: "2026-06-27T22:00:00.000Z".into(),
            device_time: None,
            level: level.clone(),
            module: module.map(String::from),
            message: msg.into(),
            raw: format!("`R\x00\x00{}{}{}", level_str, module.unwrap_or(""), msg),
        }
    }

    #[test]
    fn match_keyword_message_only() {
        let entries = vec![entry("hello world", Some("user.main"), LogLevel::Info)];
        assert!(match_keyword(&entries, "hello", MatchField::Message));
        assert!(!match_keyword(&entries, "user.main", MatchField::Message), "module 不应被 message 模式命中");
        assert!(!match_keyword(&entries, "I", MatchField::Message), "level 不应被 message 模式命中");
    }

    #[test]
    fn match_keyword_raw_only() {
        let entries = vec![entry("hello", Some("user.main"), LogLevel::Info)];
        // raw 包含帧头, 也包含 module 名称
        assert!(match_keyword(&entries, "user.main", MatchField::Raw));
        assert!(match_keyword(&entries, "hello", MatchField::Raw));
    }

    #[test]
    fn match_keyword_any_field() {
        let entries = vec![entry("hello", Some("user.testrunner"), LogLevel::Info)];
        // message 命中
        assert!(match_keyword(&entries, "hello", MatchField::Any));
        // module 命中
        assert!(match_keyword(&entries, "testrunner", MatchField::Any));
        // level 命中 (单字符 "I")
        assert!(match_keyword(&entries, "I", MatchField::Any));
        // 不存在的关键字
        assert!(!match_keyword(&entries, "nonexistent", MatchField::Any));
    }

    #[test]
    fn match_keyword_all_fields() {
        // 构造一个 module="user.main" level=Info message="hello" 的 entry
        let entries = vec![entry("hello", Some("user.main"), LogLevel::Info)];
        // 三个字段都包含 "main": message? no. level? no. module? yes.
        assert!(!match_keyword(&entries, "main", MatchField::All), "仅 module 命中不应触发 All");
        // 三个字段都包含 "user.main" 是不可能的 (只有 module 有 user.main)
        assert!(!match_keyword(&entries, "user.main", MatchField::All), "All 必须三字段都包含, 仅 module 不够");
        // 构造一个三字段都包含同一字符串的: 用 "I" 不行 (message 不含 I 除非巧合)
        // 改用 entry("I-info", Some("user.I"), LogLevel::Info) 三个都含 "I"
        let triple = vec![entry("I-info", Some("user.I"), LogLevel::Info)];
        assert!(match_keyword(&triple, "I", MatchField::All), "三字段都含 I, All 应命中");
    }

    #[test]
    fn install_ctrlc_is_idempotent() {
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;
        let cancel = Arc::new(AtomicBool::new(false));
        // 第一次: 模拟 trun 的 install_ctrlc
        install_ctrlc(&crate::OutputFormat::Text, cancel.clone());
        // 第二次: 模拟 flash 的 ctrlc::set_handler(...)?  (不带 ?)
        let res = std::panic::catch_unwind(|| {
            let c = cancel.clone();
            let _ = ctrlc::set_handler(move || {
                c.store(true, std::sync::atomic::Ordering::Relaxed);
            });
        });
        assert!(res.is_ok(), "second set_handler must not panic");
    }

    #[test]
    fn capture_log_invalid_soc_returns_err() {
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;
        let args = TrunRunArgs {
            testcase: "x".into(),
            luatos_root: None,
            soc: "D:/nonexistent/fake.soc".into(),
            port: "COM999".into(),
            baud: None,
            common_scripts: None,
            progress_step: 10,
            reset: crate::reset_args::ResetArgs::default(),
            full: false,
            flash_only: false,
            clear_fs: false,
            keep_soc: None,
            keywords: vec![],
            fail_keywords: vec![],
            match_field: MatchField::Message,
            smart: true,
            timeout: Some(1),
            early_exit: true,
            ctx: None,
            full_ctx: None,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let res = capture_log(&args, &[] as &[LogEntry], 1, &crate::OutputFormat::Text, &cancel);
        assert!(res.is_err(), "fake soc should return Err, got {res:?}");
    }
}
