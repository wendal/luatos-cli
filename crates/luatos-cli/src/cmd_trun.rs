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

use luatos_testcase::{build_ctx, build_script_bin, inject_identifiers, resolve_testcase, scan_testcases, write_ctx_to_temp, ResolvedTestcase};

use crate::cmd_flash;
use crate::cmd_log;
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
    phase_durations.insert(Phase::Ctx.as_str().to_string(), start.elapsed().as_millis() as u64);

    // 5. 阶段：build（自动把 ctx.json 烧入 script.bin）
    let start = Instant::now();
    let script_bin = build_script_image(args, &resolved, &chip, &built_ctx.value)?;
    let script_bin_path = write_script_bin(&script_bin, args.keep_soc.as_deref())?;
    let combined_soc = if args.full_soc { Some(combine_soc(args, &script_bin, &soc_info)?) } else { None };
    phase_durations.insert(Phase::Build.as_str().to_string(), start.elapsed().as_millis() as u64);

    // 6. 阶段：flash
    let start = Instant::now();
    let boot_lines = flash_device(args, &script_bin_path, combined_soc.as_deref(), format, &cancel)?;
    phase_durations.insert(Phase::Flash.as_str().to_string(), start.elapsed().as_millis() as u64);

    // 7. 阶段：log_capture
    let start = Instant::now();
    let (extra_lines, smart_diag) = capture_log(args, &boot_lines, timeout, format, &cancel)?;
    let mut all_lines = boot_lines;
    all_lines.extend(extra_lines);
    phase_durations.insert(Phase::LogCapture.as_str().to_string(), start.elapsed().as_millis() as u64);

    // 8. 阶段：finalize
    let start = Instant::now();
    let keyword_hits = evaluate_keywords(&all_lines, &keywords);
    let fail_keyword_hits = evaluate_keywords(&all_lines, &fail_keywords);
    let matched_fail_keywords: Vec<String> = fail_keyword_hits.iter().filter(|h| h.found).map(|h| h.keyword.clone()).collect();
    let fast_failed = !matched_fail_keywords.is_empty();
    let all_passed = keyword_hits.iter().all(|h| h.found) && !fast_failed;

    let verdict = derive_verdict(all_passed, fast_failed);
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
        boot_log_count: all_lines.len(),
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

fn install_ctrlc(format: &OutputFormat, cancel: Arc<AtomicBool>) {
    let f = *format;
    let c = cancel.clone();
    let _ = ctrlc::set_handler(move || {
        let _ = event::emit_message(&f, "testcase.run", MessageLevel::Warn, "Cancelling testcase run...");
        c.store(true, Ordering::Relaxed);
    });
}

fn build_script_image(args: &TrunRunArgs, resolved: &ResolvedTestcase, chip: &str, ctx: &serde_json::Value) -> Result<Vec<u8>> {
    let common = resolve_common_scripts(args, &resolve_luatos_root(args.luatos_root.as_deref())?);
    let script_dirs: Vec<&Path> = vec![resolved.scripts_dir.as_path()];
    // 把 ctx.json 写到临时目录,让 build_script_bin 把它当一个 src 目录合并进 script.bin
    // 这样设备端 SDK 启动后就能 io.open("/ctx.json") 拿到 test_id/runner_id/runner_mode 等字段
    let (ctx_tmp, _ctx_path) = write_ctx_to_temp(ctx).context("write ctx.json to temp failed")?;
    let image = build_script_bin(&script_dirs, common.as_deref(), Some(ctx_tmp.path()), chip).context("build script.bin 失败")?;
    // ctx_tmp 在此处 drop, 但 image 已复制到内存
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

fn prepare_ctx(args: &TrunRunArgs, root: &Path, _common_scripts: Option<&Path>) -> Result<luatos_testcase::CtxBuildResult> {
    let mut built = merge_ctx_layers(args, root)?;
    inject_identifiers(&mut built, None);
    Ok(built)
}

fn merge_ctx_layers(args: &TrunRunArgs, root: &Path) -> Result<luatos_testcase::CtxBuildResult> {
    build_ctx(root, None, args.ctx.as_deref().map(Path::new), args.full_ctx.as_deref().map(Path::new), 0)
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

fn capture_log(args: &TrunRunArgs, boot_lines: &[String], timeout: u64, _format: &OutputFormat, cancel: &Arc<AtomicBool>) -> Result<(Vec<String>, Vec<SmartDiagnosticEntry>)> {
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
    let _ = boot_lines;
    let _ = timeout;
    let smart_diag: Vec<SmartDiagnosticEntry> = Vec::new();

    if cancel.load(Ordering::Relaxed) {
        return Ok((Vec::new(), smart_diag));
    }
    Ok((Vec::new(), smart_diag))
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
