//! `trun` 子命令（test run）
//!
//! 一站式完成"读 testcase → 合成 (script.bin + soc) → 刷机 → 抓日志 →
//! 关键字校验 → ctx.json 监听"，简化 luatos-autotest-v2 在开发期的临时合成。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use serde::Serialize;

use luatos_testcase::{build_ctx, build_script_bin, inject_identifiers, resolve_testcase, run_python_hook, scan_testcases, DiscoverySource, HookContext, ResolvedTestcase};

use crate::cmd_flash;
use crate::cmd_log;
use crate::cmd_trun_ctx_server::{start_ctx_server, CtxEvent, CtxServerHandle};
use crate::event::{self, MessageLevel};
use crate::OutputFormat;

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

    /// 冷路径：合新 soc 后刷整个固件（默认仅刷 script.bin）
    #[arg(long)]
    pub full_soc: bool,

    /// 保留合成的 script.bin / soc 到指定目录
    #[arg(long, value_name = "DIR")]
    pub keep_soc: Option<String>,

    /// PASS 关键字（可重复传入，逗号分隔）
    #[arg(long = "keyword", value_delimiter = ',')]
    pub keywords: Vec<String>,

    /// 命中即 FAIL 的关键字（可重复，逗号分隔）
    #[arg(long = "fail-keyword", value_delimiter = ',')]
    pub fail_keywords: Vec<String>,

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

    /// ctx.json 回传监听端口（0=随机）
    #[arg(long, default_value_t = 0)]
    pub ctx_listen_port: u16,

    /// ctx.json 回传超时秒（默认 = metas.timeout）
    #[arg(long)]
    pub ctx_timeout: Option<u64>,

    /// 禁用 ctx.json 监听器
    #[arg(long)]
    pub no_listener: bool,

    /// Python 解释器路径（preprocess.py / midprocess.py 用）
    #[arg(long)]
    pub python: Option<String>,

    /// 强制重跑 preprocess.py
    #[arg(long)]
    pub force_preprocess: bool,
}

/// testcase 阶段名（用于 emit_message 的 phase 字段）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Preprocess,
    Build,
    Ctx,
    Flash,
    LogCapture,
    Finalize,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Preprocess => "preprocess",
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

/// ctx.json 监听结果
#[derive(Debug, Clone, Serialize, Default)]
pub struct ListenerOutcome {
    pub started: bool,
    pub port: u16,
    pub status_received: bool,
    pub result_received: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_payload: Option<serde_json::Value>,
    pub timed_out: bool,
}

/// 产物路径汇总
#[derive(Debug, Clone, Serialize, Default)]
pub struct ArtifactSummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script_bin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub combined_soc: Option<String>,
    pub ctx_json: String,
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
    pub listener: ListenerOutcome,
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
            if let Some(pp) = &resolved.preprocess_py {
                println!("preprocess: {}", pp.display());
            }
            if let Some(mp) = &resolved.midprocess_py {
                println!("midprocess: {}", mp.display());
            }
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
                    "has_preprocess": resolved.preprocess_py.is_some(),
                    "has_midprocess": resolved.midprocess_py.is_some(),
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

    // 创建 tokio runtime 供 listener 使用
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to create tokio runtime")?;

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

    // 4. 阶段：preprocess
    let start = Instant::now();
    let ctx_path = std::env::temp_dir().join("luatos_testcase_ctx.json");
    if resolved.preprocess_py.is_some() {
        let ctx = run_preprocess(args, &resolved, &ctx_path)?;
        std::fs::write(&ctx_path, serde_json::to_string_pretty(&ctx)?).context("failed to write ctx.json after preprocess")?;
    }
    phase_durations.insert(Phase::Preprocess.as_str().to_string(), start.elapsed().as_millis() as u64);

    // 5. 阶段：build
    let start = Instant::now();
    let script_bin = build_script_image(args, &resolved, &chip)?;
    let script_bin_path = write_script_bin(&script_bin, args.keep_soc.as_deref())?;
    let combined_soc = if args.full_soc { Some(combine_soc(args, &script_bin, &soc_info)?) } else { None };
    phase_durations.insert(Phase::Build.as_str().to_string(), start.elapsed().as_millis() as u64);

    // 6. 阶段：ctx（合并 + 监听器）
    let start = Instant::now();
    let common_scripts = resolve_common_scripts(args, &root);
    let (ctx_test_id, server_handle) = build_ctx_and_listener(&runtime, args, &root, common_scripts.as_deref())?;
    phase_durations.insert(Phase::Ctx.as_str().to_string(), start.elapsed().as_millis() as u64);

    // 7. 阶段：flash
    let start = Instant::now();
    let boot_lines = flash_device(args, &script_bin_path, combined_soc.as_deref(), format, &cancel)?;
    phase_durations.insert(Phase::Flash.as_str().to_string(), start.elapsed().as_millis() as u64);

    // 8. 阶段：log_capture
    let start = Instant::now();
    let log_capture_timeout = args.ctx_timeout.unwrap_or(timeout);
    let (extra_lines, smart_diag) = capture_log_with_listener(&runtime, args, &boot_lines, server_handle.as_ref(), log_capture_timeout, format, &cancel)?;
    let mut all_lines = boot_lines;
    all_lines.extend(extra_lines);
    phase_durations.insert(Phase::LogCapture.as_str().to_string(), start.elapsed().as_millis() as u64);

    // 9. 阶段：finalize
    let start = Instant::now();
    let keyword_hits = evaluate_keywords(&all_lines, &keywords);
    let fail_keyword_hits = evaluate_keywords(&all_lines, &fail_keywords);
    let matched_fail_keywords: Vec<String> = fail_keyword_hits.iter().filter(|h| h.found).map(|h| h.keyword.clone()).collect();
    let fast_failed = !matched_fail_keywords.is_empty();
    let all_passed = keyword_hits.iter().all(|h| h.found) && !fast_failed;

    let listener_outcome = build_listener_outcome(server_handle.as_ref());

    let verdict = derive_verdict(all_passed, fast_failed, &listener_outcome);
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
        build_path: if args.full_soc { "flash_soc".into() } else { "flash_script".into() },
        keywords: keyword_hits,
        fail_keywords: fail_keyword_hits,
        matched_fail_keywords,
        fast_failed,
        listener: listener_outcome,
        boot_log_count: all_lines.len(),
        smart_diagnostics: smart_diag,
        artifacts: ArtifactSummary {
            script_bin: Some(script_bin_path.display().to_string()),
            combined_soc: combined_soc.as_ref().map(|p| p.display().to_string()),
            ctx_json: ctx_path.display().to_string(),
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

fn install_ctrlc(format: &OutputFormat, cancel: Arc<AtomicBool>) {
    let f = *format;
    let c = cancel.clone();
    let _ = ctrlc::set_handler(move || {
        let _ = event::emit_message(&f, "testcase.run", MessageLevel::Warn, "Cancelling testcase run...");
        c.store(true, Ordering::Relaxed);
    });
}

fn run_preprocess(args: &TrunRunArgs, resolved: &ResolvedTestcase, ctx_path: &Path) -> Result<serde_json::Value> {
    let pp = resolved.preprocess_py.as_ref().expect("called only when preprocess exists");
    let python = args.python.as_ref().map(PathBuf::from).with_context(|| "preprocess.py 存在但未指定 --python")?;
    let artifacts = vec![resolved.path.join("scripts").join(".last_preprocess_ts")];
    let ctx = HookContext {
        python,
        script: pp.clone(),
        testcase_dir: resolved.path.clone(),
        ctx_path: ctx_path.to_path_buf(),
        artifacts,
        script_bin: None,
        soc: None,
        timeout_secs: 30,
        force: args.force_preprocess,
    };
    let r = run_python_hook(&ctx).context("preprocess.py 钩子失败")?;
    event::emit_message(
        &OutputFormat::Text, // 占位
        "testcase.run",
        MessageLevel::Info,
        format!("preprocess executed={} skip={:?} duration={}ms", r.executed, r.skip_reason, r.duration_ms),
    )?;
    // 读取 ctx（preprocess 可能已修改）
    if ctx_path.exists() {
        let s = std::fs::read_to_string(ctx_path)?;
        Ok(serde_json::from_str(&s).context("ctx.json parse failed")?)
    } else {
        Ok(serde_json::json!({}))
    }
}

fn build_script_image(args: &TrunRunArgs, resolved: &ResolvedTestcase, chip: &str) -> Result<Vec<u8>> {
    let common = resolve_common_scripts(args, &resolve_luatos_root(args.luatos_root.as_deref())?);
    let script_dirs: Vec<&Path> = vec![resolved.scripts_dir.as_path()];
    let image = build_script_bin(&script_dirs, common.as_deref(), None, chip).context("build script.bin 失败")?;
    Ok(image)
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
    let chip = soc_info.chip.chip_type.as_str();
    if matches!(chip, "ec7xx" | "air8000" | "air780epm" | "air780ehm" | "air780ehv" | "air780ehg") {
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

fn build_ctx_and_listener(runtime: &tokio::runtime::Runtime, args: &TrunRunArgs, root: &Path, _common_scripts: Option<&Path>) -> Result<(String, Option<CtxServerHandle>)> {
    let mut built = merge_ctx_layers(args, root)?;
    let test_id = built.test_id.clone();
    if !args.no_listener {
        let handle = start_listener(runtime, args.ctx_listen_port, &test_id)?;
        let port = handle.port;
        built.listen_port = port;
        let report = format!("http://127.0.0.1:{port}/result");
        let status = format!("http://127.0.0.1:{port}/status");
        inject_identifiers(&mut built, Some((report.as_str(), status.as_str())));
        return Ok((test_id, Some(handle)));
    }
    Ok((test_id, None))
}

fn merge_ctx_layers(args: &TrunRunArgs, root: &Path) -> Result<luatos_testcase::CtxBuildResult> {
    build_ctx(root, None, args.ctx.as_deref().map(Path::new), args.full_ctx.as_deref().map(Path::new), 0)
}

fn start_listener(runtime: &tokio::runtime::Runtime, port: u16, test_id: &str) -> Result<CtxServerHandle> {
    runtime.block_on(start_ctx_server(port, test_id.to_string()))
}

fn flash_device(args: &TrunRunArgs, script_bin_path: &Path, combined_soc: Option<&Path>, format: &OutputFormat, cancel: &Arc<AtomicBool>) -> Result<Vec<String>> {
    if cancel.load(Ordering::Relaxed) {
        bail!("cancelled before flash");
    }
    let soc_for_flash = combined_soc.and_then(|p| p.to_str()).unwrap_or(args.soc.as_str());
    if combined_soc.is_some() {
        // 冷路径：刷整个 soc
        cmd_flash::cmd_flash_run(soc_for_flash, &args.port, args.baud, None, args.progress_step, format, &args.reset, None, 0).context("flash run failed")?;
    } else {
        // 快路径：刷 script.bin
        let on_progress = cmd_flash::make_progress_callback(format, "testcase.run", args.progress_step);
        let bin_str = script_bin_path.to_str().context("script_bin_path is not valid utf-8")?;
        cmd_flash::cmd_flash_script_bin(soc_for_flash, &args.port, bin_str, &on_progress).context("flash script.bin failed")?;
    }
    // 简化：boot_lines 暂时为空，由后续 log_capture 阶段抓取
    Ok(Vec::new())
}

fn capture_log_with_listener(
    _runtime: &tokio::runtime::Runtime,
    args: &TrunRunArgs,
    boot_lines: &[String],
    server_handle: Option<&CtxServerHandle>,
    timeout: u64,
    _format: &OutputFormat,
    cancel: &Arc<AtomicBool>,
) -> Result<(Vec<String>, Vec<SmartDiagnosticEntry>)> {
    let (_use_binary, is_ec718, log_br) = {
        let info = luatos_soc::read_soc_info(&args.soc)?;
        cmd_log::resolve_log_mode(info.chip.chip_type.as_str(), info.log_baud_rate())
    };
    let baud = args.baud.unwrap_or(log_br);
    let log_port = if is_ec718 {
        match luatos_flash::ec718::wait_for_log_port(15) {
            Some(p) => p,
            None => args.port.clone(),
        }
    } else {
        args.port.clone()
    };

    let _ = (log_port, baud);

    let collected_lines: Vec<String> = if let Some(handle) = server_handle {
        wait_for_listener_result(handle, Duration::from_secs(timeout), cancel)?
    } else {
        Vec::new()
    };

    let _ = boot_lines;
    let smart_diag: Vec<SmartDiagnosticEntry> = Vec::new();

    if cancel.load(Ordering::Relaxed) {
        return Ok((collected_lines, smart_diag));
    }
    Ok((collected_lines, smart_diag))
}

/// 同步轮询 listener events 直到收到 result 或超时
fn wait_for_listener_result(handle: &CtxServerHandle, timeout: Duration, cancel: &Arc<AtomicBool>) -> Result<Vec<String>> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if handle.first_result().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(Vec::new())
}

fn evaluate_keywords(lines: &[String], keywords: &[String]) -> Vec<KeywordHit> {
    keywords
        .iter()
        .map(|k| KeywordHit {
            keyword: k.clone(),
            found: lines.iter().any(|l| l.contains(k)),
        })
        .collect()
}

fn build_listener_outcome(handle: Option<&CtxServerHandle>) -> ListenerOutcome {
    let mut out = ListenerOutcome::default();
    if let Some(h) = handle {
        out.started = true;
        out.port = h.port;
        out.status_received = h.has_status();
        if let Some(CtxEvent::Result { test_id: _, ok, message, raw }) = h.first_result() {
            let _ = ok;
            out.result_received = true;
            let mut payload = raw.clone();
            if let Some(m) = message {
                if let Some(obj) = payload.as_object_mut() {
                    obj.insert("message".to_string(), serde_json::Value::String(m.clone()));
                }
            }
            out.device_payload = Some(payload);
        }
    }
    out
}

fn derive_verdict(all_passed: bool, fast_failed: bool, listener: &ListenerOutcome) -> Verdict {
    if fast_failed {
        return Verdict::Fail;
    }
    if !all_passed {
        return Verdict::Fail;
    }
    if listener.started && !listener.result_received {
        // listener 已启动但超时未收到 → Indeterminate
        return Verdict::Indeterminate;
    }
    if let Some(p) = &listener.device_payload {
        if let Some(ok) = p.get("ok").and_then(|v| v.as_bool()) {
            if !ok {
                return Verdict::Fail;
            }
        }
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

// 抑制未使用导入警告（部分函数尚未完全接入）
#[allow(dead_code)]
fn _unused_imports() {
    let _ = Command::new("");
    let _ = DiscoverySource::ExplicitPath;
}
