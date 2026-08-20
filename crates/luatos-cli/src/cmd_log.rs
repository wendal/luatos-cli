use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::{
    event::{self, MessageLevel},
    reset_args::ResetArgs,
    OutputFormat,
};
use anyhow::Context;
use luatos_soc::ChipFamily;

pub fn resolve_log_mode(chip: &str, requested_baud: u32) -> (bool, bool, u32) {
    // 通过芯片族归一化判断（air8000m/air780epv 等新型号自动获得正确行为）
    let family = ChipFamily::from_chip_type(chip);
    let is_ec718 = family.is_ec718();
    let use_binary_log = family.uses_binary_log();
    let baud = if is_ec718 && requested_baud == 2_000_000 { 921_600 } else { requested_baud };
    (use_binary_log, is_ec718, baud)
}

pub fn cmd_log_view(port: &str, baud: u32, smart: bool, reset: &ResetArgs, format: &OutputFormat) -> anyhow::Result<()> {
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_clone = stop.clone();
    let _ = ctrlc::set_handler(move || {
        stop_clone.store(true, std::sync::atomic::Ordering::Relaxed);
    });

    // --port auto：自动探测 RDA8910 运行模式 log 口（按 VID/PID + 接口号，对齐 ec7xx）
    let actual_port = if port == "auto" {
        match luatos_flash::rda8910::find_rda8910_log_port() {
            Some(p) => {
                event::emit_message(format, "log.view", MessageLevel::Info, format!("Auto-detected RDA8910 log port: {p}"))?;
                p
            }
            None => {
                anyhow::bail!(
                    "No RDA8910 log port found. Ensure the module is running (not in boot mode).\n\
                     Try specifying the port manually with --port COMx"
                );
            }
        }
    } else {
        port.to_string()
    };

    // RTS 复位脉冲：先复位模组再开始采集，以捕获开机日志
    reset.execute(&actual_port)?;

    event::emit_message(
        format,
        "log.view",
        MessageLevel::Info,
        format!("Viewing log on {actual_port} @ {baud} bps (Ctrl+C to stop)"),
    )?;
    if smart {
        event::emit_message(format, "log.view", MessageLevel::Info, "🧠 智能分析已启用")?;
    }

    let format_clone = *format;

    if luatos_flash::rda8910::is_running_port_name(&actual_port) {
        // RDA8910：x.6 soc_log 口为二进制 host trace，需 DTR/RTS 拉高才有数据流 + 行提取
        // （对齐 luatools_py3 用 common_log.dll pyAnalyzeHost 解码的行为）
        let rda_decoder = std::sync::Mutex::new(luatos_log::Rda8910LogDecoder::new());
        luatos_serial::stream_binary(
            &actual_port,
            baud,
            stop,
            Box::new(move |data| {
                if let Ok(mut dec) = rda_decoder.lock() {
                    for entry in dec.feed(data) {
                        if let Err(e) = event::emit_log_entry(&format_clone, "log.view", &entry) {
                            log::warn!("输出日志事件失败: {e}");
                        }
                    }
                }
            }),
            None,
            true, // RDA8910 log 口需要 DTR/RTS 拉高
        )?;
    } else {
        let dispatcher = luatos_log::LogDispatcher::default_parsers();
        let analyzer = if smart {
            Some(std::sync::Mutex::new(luatos_log::smart::SmartAnalyzer::new()))
        } else {
            None
        };
        luatos_serial::stream_log_lines(
            &actual_port,
            baud,
            stop,
            Box::new(move |line| {
                let entry = dispatcher.parse(line);
                if let Err(e) = event::emit_log_entry(&format_clone, "log.view", &entry) {
                    log::warn!("输出日志事件失败: {e}");
                }
                if let Some(ref analyzer) = analyzer {
                    if let Ok(mut a) = analyzer.lock() {
                        let diags = a.analyze(&entry);
                        for diag in &diags {
                            match format_clone {
                                OutputFormat::Text => {
                                    eprintln!("\n{}\n", luatos_log::smart::format_diagnostic(diag));
                                }
                                OutputFormat::Json | OutputFormat::Jsonl => {
                                    let _ = event::emit_jsonl_event(
                                        &format_clone,
                                        serde_json::json!({
                                            "type": "diagnostic",
                                            "command": "log.view",
                                            "diagnostic": diag,
                                        }),
                                    );
                                }
                            }
                        }
                    }
                }
            }),
        )?;
    }

    event::emit_message(format, "log.view", MessageLevel::Info, "Log viewing stopped.")?;
    Ok(())
}

// ─── Rolling binary file writer ───────────────────────────────────────────────

// Timestamp injection marker: injected when gap between data chunks exceeds 4 ms.
//
// Format (16 bytes):
//   [0..4]  magic   0xFF 0xFE 0xAB 0xCD
//   [4..12] ms      unix timestamp in ms, little-endian u64
//   [12..16] gap_ms  gap since last data, little-endian u32 (capped at u32::MAX)
const MARKER_MAGIC: &[u8] = &[0xFF, 0xFE, 0xAB, 0xCD];
const MAX_FILE_BYTES: usize = 200 * 1024 * 1024; // 200 MB
const GAP_THRESHOLD_MS: u128 = 4;

struct RollingBinWriter {
    dir: std::path::PathBuf,
    port_safe: String,
    format: OutputFormat,
    command: &'static str,
    writer: std::io::BufWriter<std::fs::File>,
    written: usize,
    current_path: std::path::PathBuf,
    last_recv: std::time::Instant,
}

impl RollingBinWriter {
    fn new(dir: &std::path::Path, port: &str, format: OutputFormat, command: &'static str) -> anyhow::Result<Self> {
        let port_safe = port.replace(|c: char| !c.is_alphanumeric() && c != '-' && c != '_', "_");
        std::fs::create_dir_all(dir)?;
        let (writer, path) = open_new_file(dir, &port_safe)?;
        event::emit_message(&format, command, MessageLevel::Info, format!("AP log recording → {}", path.display()))?;
        Ok(Self {
            dir: dir.to_path_buf(),
            port_safe,
            format,
            command,
            writer,
            written: 0,
            current_path: path,
            last_recv: std::time::Instant::now(),
        })
    }

    fn write_chunk(&mut self, data: &[u8]) -> anyhow::Result<()> {
        use std::io::Write;
        let now = std::time::Instant::now();
        let gap_ms = now.duration_since(self.last_recv).as_millis();
        self.last_recv = now;

        if gap_ms >= GAP_THRESHOLD_MS {
            self.inject_timestamp(gap_ms)?;
        }
        self.writer.write_all(data)?;
        self.written += data.len();

        if self.written >= MAX_FILE_BYTES {
            self.rotate()?;
        }
        Ok(())
    }

    fn inject_timestamp(&mut self, gap_ms: u128) -> anyhow::Result<()> {
        use std::io::Write;
        let ts_ms = chrono::Utc::now().timestamp_millis() as u64;
        let gap_u32 = gap_ms.min(u32::MAX as u128) as u32;
        self.writer.write_all(MARKER_MAGIC)?;
        self.writer.write_all(&ts_ms.to_le_bytes())?;
        self.writer.write_all(&gap_u32.to_le_bytes())?;
        self.written += MARKER_MAGIC.len() + 8 + 4;
        Ok(())
    }

    fn rotate(&mut self) -> anyhow::Result<()> {
        use std::io::Write;
        self.writer.flush()?;
        let (new_writer, new_path) = open_new_file(&self.dir, &self.port_safe)?;
        self.writer = new_writer;
        self.written = 0;
        self.current_path = new_path.clone();
        event::emit_message(&self.format, self.command, MessageLevel::Info, format!("AP log rotated → {}", new_path.display()))?;
        Ok(())
    }

    fn flush(&mut self) {
        use std::io::Write;
        let _ = self.writer.flush();
    }
}

fn open_new_file(dir: &std::path::Path, port_safe: &str) -> anyhow::Result<(std::io::BufWriter<std::fs::File>, std::path::PathBuf)> {
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let filename = format!("ap_{ts}_{port_safe}.bin");
    let path = dir.join(&filename);
    let file = std::fs::File::create(&path).map_err(|e| anyhow::anyhow!("create {}: {e}", path.display()))?;
    Ok((std::io::BufWriter::with_capacity(64 * 1024, file), path))
}

// ─── cmd_log_view_binary ──────────────────────────────────────────────────────

pub fn cmd_log_view_binary(port: &str, baud: u32, probe: bool, save_dir: Option<&str>, smart: bool, reset: &ResetArgs, format: &OutputFormat) -> anyhow::Result<()> {
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_clone = stop.clone();
    let _ = ctrlc::set_handler(move || {
        stop_clone.store(true, std::sync::atomic::Ordering::Relaxed);
    });

    // RTS 复位脉冲：先复位模组再开始采集，以捕获开机日志
    reset.execute(port)?;

    // Detect whether an EC718 module is connected (VID=0x19D1)
    let is_ec718 = luatos_flash::ec718::find_ec718_cmd_port().is_some();

    // Auto-detect log port if "auto" specified
    let actual_port = if port == "auto" {
        if is_ec718 {
            event::emit_message(format, "log.view_binary", MessageLevel::Info, "Auto-detecting EC718 log port (VID=0x19D1)...")?;
            match luatos_flash::ec718::find_ec718_log_port() {
                Some(p) => {
                    event::emit_message(format, "log.view_binary", MessageLevel::Info, format!("Found EC718 log port: {p}"))?;
                    p
                }
                None => {
                    anyhow::bail!(
                        "No EC718 log port found. Ensure the module is running (not in boot mode).\n\
                         Try specifying the port manually with --port COMx"
                    );
                }
            }
        } else {
            anyhow::bail!("No supported log device found. Try specifying the port manually with --port COMx");
        }
    } else {
        port.to_string()
    };

    // For EC718 USB CDC, 921600 is the supported baud rate.
    // The info.json may specify 2000000 but Windows USB CDC rejects it.
    let baud = if is_ec718 && baud == 2000000 { 921600 } else { baud };

    // Build probe data — same 0xA5 probe works for both chip types
    let init_data = if probe {
        event::emit_message(format, "log.view_binary", MessageLevel::Info, "Sending probe to trigger log output ...")?;
        Some(luatos_flash::ec718::build_log_probe())
    } else {
        None
    };

    event::emit_message(
        format,
        "log.view_binary",
        MessageLevel::Info,
        format!(
            "Viewing {} binary log on {actual_port} @ {baud} bps (Ctrl+C to stop)",
            if is_ec718 { "EC718" } else { "SOC" }
        ),
    )?;

    if smart {
        event::emit_message(format, "log.view_binary", MessageLevel::Info, "🧠 智能分析已启用")?;
    }

    // Optional rolling binary recorder
    let bin_writer: Option<std::sync::Arc<std::sync::Mutex<RollingBinWriter>>> = save_dir
        .map(|d| RollingBinWriter::new(std::path::Path::new(d), &actual_port, *format, "log.view_binary").map(|w| std::sync::Arc::new(std::sync::Mutex::new(w))))
        .transpose()?;

    let format_clone = *format;
    let smart_analyzer: Option<std::sync::Arc<std::sync::Mutex<luatos_log::smart::SmartAnalyzer>>> = if smart {
        Some(std::sync::Arc::new(std::sync::Mutex::new(luatos_log::smart::SmartAnalyzer::new())))
    } else {
        None
    };

    if is_ec718 {
        // EC718: 0x7E HDLC framing, DTR/RTS HIGH
        let decoder = std::sync::Mutex::new(luatos_log::Ec718LogDecoder::new());
        let bin_writer_clone = bin_writer.clone();
        let analyzer_clone = smart_analyzer.clone();
        luatos_serial::stream_binary(
            &actual_port,
            baud,
            stop,
            Box::new(move |data| {
                if let Some(ref bw) = bin_writer_clone {
                    if let Ok(mut w) = bw.lock() {
                        let _ = w.write_chunk(data);
                    }
                }
                if let Ok(mut dec) = decoder.lock() {
                    let entries = dec.feed(data);
                    for entry in &entries {
                        if let Err(e) = event::emit_log_entry(&format_clone, "log.view_binary", entry) {
                            log::warn!("输出日志事件失败: {e}");
                        }
                        emit_smart_diagnostics(&analyzer_clone, entry, &format_clone);
                    }
                }
            }),
            init_data.as_deref(),
            true, // DTR/RTS HIGH for EC718
        )?;
    } else {
        // Standard SOC: 0xA5 framing
        let decoder = std::sync::Mutex::new(luatos_log::SocLogDecoder::new());
        let bin_writer_clone = bin_writer.clone();
        let analyzer_clone = smart_analyzer.clone();
        luatos_serial::stream_binary(
            &actual_port,
            baud,
            stop,
            Box::new(move |data| {
                if let Some(ref bw) = bin_writer_clone {
                    if let Ok(mut w) = bw.lock() {
                        let _ = w.write_chunk(data);
                    }
                }
                if let Ok(mut dec) = decoder.lock() {
                    let entries = dec.feed(data);
                    for entry in &entries {
                        if let Err(e) = event::emit_log_entry(&format_clone, "log.view_binary", entry) {
                            log::warn!("输出日志事件失败: {e}");
                        }
                        emit_smart_diagnostics(&analyzer_clone, entry, &format_clone);
                    }
                }
            }),
            init_data.as_deref(),
            false,
        )?;
    }

    // 输出智能分析汇总
    if let Some(ref sa) = smart_analyzer {
        if let Ok(a) = sa.lock() {
            let summary = a.summary();
            if !summary.diagnostics.is_empty() {
                match format {
                    OutputFormat::Text => {
                        eprintln!("\n╔══════════════════════════════════════╗");
                        eprintln!("║     🧠 智能分析汇总                  ║");
                        eprintln!("╚══════════════════════════════════════╝");
                        eprintln!(
                            "  分析 {} 条日志, 检测到 {} 个启动, {} 个错误, {} 个警告",
                            summary.entries_analyzed, summary.boot_count, summary.errors, summary.warnings
                        );
                        for diag in &summary.diagnostics {
                            eprintln!("\n{}", luatos_log::smart::format_diagnostic(diag));
                        }
                        eprintln!();
                    }
                    OutputFormat::Json | OutputFormat::Jsonl => {
                        let _ = event::emit_jsonl_event(
                            format,
                            serde_json::json!({
                                "type": "smart_summary",
                                "command": "log.view_binary",
                                "summary": summary,
                            }),
                        );
                    }
                }
            }
        }
    }

    // Flush any buffered data
    if let Some(bw) = bin_writer {
        if let Ok(mut w) = bw.lock() {
            w.flush();
            event::emit_message(format, "log.view_binary", MessageLevel::Info, format!("Binary log saved to {}", w.current_path.display()))?;
        }
    }

    event::emit_message(format, "log.view_binary", MessageLevel::Info, "Log viewing stopped.")?;
    Ok(())
}

pub fn cmd_log_record(port: &str, baud: u32, output_dir: &str, save_json: bool, format: &OutputFormat) -> anyhow::Result<()> {
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_clone = stop.clone();
    let _ = ctrlc::set_handler(move || {
        stop_clone.store(true, std::sync::atomic::Ordering::Relaxed);
    });

    let out_path = std::path::Path::new(output_dir);
    std::fs::create_dir_all(out_path)?;

    let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let text_path = out_path.join(format!("log_{timestamp}.txt"));
    let json_path = if save_json { Some(out_path.join(format!("log_{timestamp}.jsonl"))) } else { None };

    let writer = luatos_log::LogWriter::new(Some(&text_path), json_path.as_deref())?;

    event::emit_message(
        format,
        "log.record",
        MessageLevel::Info,
        format!("Recording log on {port} @ {baud} bps → {}", text_path.display()),
    )?;
    if let Some(ref jp) = json_path {
        event::emit_message(format, "log.record", MessageLevel::Info, format!("  JSON log: {}", jp.display()))?;
    }
    event::emit_message(format, "log.record", MessageLevel::Info, "Press Ctrl+C to stop.")?;

    let dispatcher = luatos_log::LogDispatcher::default_parsers();
    let format_clone = *format;

    let writer = std::sync::Mutex::new(writer);
    let line_count = std::sync::atomic::AtomicUsize::new(0);

    luatos_serial::stream_log_lines(
        port,
        baud,
        stop,
        Box::new(move |line| {
            let entry = dispatcher.parse(line);
            if let Err(e) = event::emit_log_entry(&format_clone, "log.record", &entry) {
                log::warn!("输出日志事件失败: {e}");
            }

            if let Ok(mut w) = writer.lock() {
                let _ = w.write(&entry);
                let count = line_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if count.is_multiple_of(50) {
                    let _ = w.flush();
                }
            }
        }),
    )?;

    event::emit_message(format, "log.record", MessageLevel::Info, format!("Recording stopped. Log saved to {}", text_path.display()))?;
    Ok(())
}

pub fn cmd_log_parse(path: &str, format: &OutputFormat) -> anyhow::Result<()> {
    let dispatcher = luatos_log::LogDispatcher::default_parsers();
    let entries = luatos_log::parse_log_file(std::path::Path::new(path), &dispatcher)?;

    match format {
        OutputFormat::Text => {
            println!("Parsed {} log entries from {path}:", entries.len());
            for entry in &entries {
                let module = entry.module.as_deref().unwrap_or("-");
                let time = entry.device_time.as_deref().unwrap_or(&entry.timestamp);
                println!("[{}] {}/{} {}", time, entry.level, module, entry.message);
            }
        }
        OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(format, "log.parse", "ok", &entries)?,
    }
    Ok(())
}

/// 在日志回调中发出智能诊断事件
fn emit_smart_diagnostics(analyzer: &Option<std::sync::Arc<std::sync::Mutex<luatos_log::smart::SmartAnalyzer>>>, entry: &luatos_log::LogEntry, format: &OutputFormat) {
    if let Some(ref sa) = analyzer {
        if let Ok(mut a) = sa.lock() {
            let diags = a.analyze(entry);
            for diag in &diags {
                match format {
                    OutputFormat::Text => {
                        eprintln!("\n{}\n", luatos_log::smart::format_diagnostic(diag));
                    }
                    OutputFormat::Json | OutputFormat::Jsonl => {
                        let _ = event::emit_jsonl_event(
                            format,
                            serde_json::json!({
                                "type": "diagnostic",
                                "command": "log.view_binary",
                                "diagnostic": diag,
                            }),
                        );
                    }
                }
            }
        }
    }
}

// ─── capture_log_lines ────────────────────────────────────────────────────────

/// `trun` 抓取日志的产出: 收集到的 LogEntry + 智能诊断条目
#[derive(Debug, Default, Clone)]
#[allow(dead_code)]
pub struct CaptureOutcome {
    /// 抓取到的所有 LogEntry (含解析出的 module/level/message, 用于关键字匹配)
    pub entries: Vec<luatos_log::LogEntry>,
    /// 抓取过程中触发的智能诊断
    pub diagnostics: Vec<luatos_log::smart::Diagnostic>,
}

/// 流式抓取串口日志, 把行收集起来, 命中任一关键字立即停止
///
/// - `soc`: 用于读取 chip / log_baud
/// - `port`: 默认抓取串口; ec718 会先用 `wait_for_log_port` 切到 AP 日志口
/// - `early_exit_keywords`: 命中任意一个就 `stop.store(true)`, 早退
/// - `cancel`: 外部 ctrlc / 上层 cancel 信号
/// - 每行同时通过 `LogDispatcher::default_parsers()` 解析, 走 `event::emit_log_entry` 输出
/// - 每条解析后的 LogEntry 喂给 SmartAnalyzer, 触发的 Diagnostic 一并收集
#[allow(dead_code)]
pub fn capture_log_lines(
    soc: &str,
    port: &str,
    baud_override: Option<u32>,
    timeout_secs: u64,
    early_exit_keywords: &[String],
    format: &OutputFormat,
    cancel: &Arc<AtomicBool>,
) -> anyhow::Result<CaptureOutcome> {
    use std::time::Duration;

    let info = luatos_soc::read_soc_info(soc).with_context(|| format!("无法读取 soc: {soc}"))?;
    let chip = info.chip.chip_type.as_str();
    let (use_binary, is_ec718, log_baud) = resolve_log_mode(chip, info.log_baud_rate());
    let baud = baud_override.unwrap_or(log_baud);

    let log_port: String = if is_ec718 {
        match luatos_flash::ec718::wait_for_log_port(15) {
            Some(p) => p,
            None => port.to_string(),
        }
    } else {
        port.to_string()
    };

    let outcome = Arc::new(Mutex::new(CaptureOutcome::default()));
    let keywords: Vec<String> = early_exit_keywords.to_vec();
    let stop = Arc::new(AtomicBool::new(false));

    // 1. 超时定时器
    let _timer = spawn_cancel_timer(stop.clone(), cancel.clone(), Duration::from_secs(timeout_secs));

    // 2. 启动流
    if use_binary {
        let decoder = Arc::new(Mutex::new(if is_ec718 { None } else { Some(luatos_log::SocLogDecoder::new()) }));
        let ec718_decoder = Arc::new(Mutex::new(if is_ec718 { Some(luatos_log::Ec718LogDecoder::new()) } else { None }));
        let analyzer = Arc::new(Mutex::new(luatos_log::smart::SmartAnalyzer::new()));
        let outcome = outcome.clone();
        let stop_for_cb = stop.clone();
        let fmt = *format;
        let res = luatos_serial::stream_binary(
            &log_port,
            baud,
            stop.clone(),
            Box::new(move |data| {
                if let Ok(mut dec) = decoder.lock() {
                    if let Some(ref mut d) = *dec {
                        for entry in d.feed(data) {
                            push_entry(&outcome, &analyzer, &entry, &stop_for_cb, &keywords, &fmt);
                        }
                    }
                }
                if let Ok(mut dec) = ec718_decoder.lock() {
                    if let Some(ref mut d) = *dec {
                        for entry in d.feed(data) {
                            push_entry(&outcome, &analyzer, &entry, &stop_for_cb, &keywords, &fmt);
                        }
                    }
                }
            }),
            Some(luatos_flash::ec718::build_log_probe()).as_deref(),
            is_ec718,
        );
        if res.is_err() {
            stop.store(true, Ordering::Relaxed);
        }
        res?;
    } else {
        let dispatcher = luatos_log::LogDispatcher::default_parsers();
        let analyzer = Arc::new(Mutex::new(luatos_log::smart::SmartAnalyzer::new()));
        let outcome = outcome.clone();
        let stop_for_cb = stop.clone();
        let fmt = *format;
        let res = luatos_serial::stream_log_lines(
            &log_port,
            baud,
            stop.clone(),
            Box::new(move |line| {
                let entry = dispatcher.parse(line);
                push_entry(&outcome, &analyzer, &entry, &stop_for_cb, &keywords, &fmt);
            }),
        );
        if res.is_err() {
            stop.store(true, Ordering::Relaxed);
        }
        res?;
    }

    let outcome = Arc::try_unwrap(outcome)
        .map_err(|_| anyhow::anyhow!("capture_log_lines: outcome arc still shared"))?
        .into_inner()
        .map_err(|_| anyhow::anyhow!("capture_log_lines: outcome mutex poisoned"))?;
    Ok(outcome)
}

/// 启动一个后台定时线程: `timeout` 到期 / `stop=true` / `cancel=true` 时退出.
///
/// 关键不变量: `cancel` 一旦置位, 必须立刻把 `stop` 也置位, 以便
/// `luatos_serial::stream_*` 的内部 `read()` 能立刻返回, 不再阻塞.
/// (ctrlc 处理只会写 `cancel`, 不会直接写 `stop`, 因此这一步是必需的.)
fn spawn_cancel_timer(stop: Arc<AtomicBool>, cancel: Arc<AtomicBool>, timeout: std::time::Duration) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if cancel.load(Ordering::Relaxed) {
                // cancel 立即传导到 stop, 让 stream_* 的 read() 也能立即返回
                stop.store(true, Ordering::Relaxed);
                return;
            }
            if stop.load(Ordering::Relaxed) {
                return;
            }
            if std::time::Instant::now() >= deadline {
                stop.store(true, Ordering::Relaxed);
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    })
}

/// 内部辅助: 把单条 LogEntry 推入 outcome, 同步 emit + 关键字检测 + 智能诊断
#[allow(dead_code)]
fn push_entry(
    outcome: &Arc<Mutex<CaptureOutcome>>,
    analyzer: &Arc<Mutex<luatos_log::smart::SmartAnalyzer>>,
    entry: &luatos_log::LogEntry,
    stop: &Arc<AtomicBool>,
    keywords: &[String],
    fmt: &OutputFormat,
) {
    // 关键字早退检测 (在持锁外做, 避免和 capture 线程互相等待)
    if !keywords.is_empty() && keywords.iter().any(|k| entry.raw.contains(k)) {
        stop.store(true, Ordering::Relaxed);
    }
    // 智能诊断
    let mut diags: Vec<luatos_log::smart::Diagnostic> = Vec::new();
    if let Ok(mut a) = analyzer.lock() {
        diags = a.analyze(entry);
    }
    // 收集 outcome
    if let Ok(mut out) = outcome.lock() {
        out.entries.push(entry.clone());
        out.diagnostics.extend(diags);
    }
    if let Err(e) = event::emit_log_entry(fmt, "trun.capture", entry) {
        log::warn!("emit_log_entry failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn capture_log_lines_invalid_port_returns_err() {
        // 故意给一个不存在的串口, 应该 anyhow! 失败而不是 panic
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let res = super::capture_log_lines("D:/nonexistent/fake.soc", "COM999", None, 1, &[], &super::OutputFormat::Text, &stop);
        assert!(res.is_err(), "fake port should return Err, got {res:?}");
    }

    #[test]
    fn resolve_log_mode_ec718_forces_921600() {
        let (binary, ec718, baud) = super::resolve_log_mode("ec7xx", 2_000_000);
        assert!(binary);
        assert!(ec718);
        assert_eq!(baud, 921_600);
    }

    #[test]
    fn resolve_log_mode_air1601_binary_keeps_baud() {
        let (binary, ec718, baud) = super::resolve_log_mode("air1601", 2_000_000);
        assert!(binary);
        assert!(!ec718);
        assert_eq!(baud, 2_000_000);
    }

    #[test]
    fn resolve_log_mode_bk72xx_text() {
        let (binary, ec718, baud) = super::resolve_log_mode("bk72xx", 921_600);
        assert!(!binary);
        assert!(!ec718);
        assert_eq!(baud, 921_600);
    }

    /// spawn_cancel_timer 必须把 cancel 立即传导到 stop,
    /// 否则 ctrlc 之后 stream_* 的 read() 会阻塞, 抓日志不能立即停止.
    #[test]
    fn cancel_timer_propagates_to_stop() {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let handle = super::spawn_cancel_timer(stop.clone(), cancel.clone(), std::time::Duration::from_secs(60));
        // 给定时线程一点时间进入循环
        std::thread::sleep(std::time::Duration::from_millis(100));
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        // 定时线程最多 100ms 轮询一次, 给 300ms 足够它把 stop 置位
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(stop.load(std::sync::atomic::Ordering::Relaxed), "cancel must propagate to stop within 300ms");
        let _ = handle.join();
    }

    /// stop 单独置位时, 定时线程也应该立即退出 (不等待 cancel).
    #[test]
    fn stop_alone_terminates_timer() {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let handle = super::spawn_cancel_timer(stop.clone(), cancel.clone(), std::time::Duration::from_secs(60));
        std::thread::sleep(std::time::Duration::from_millis(100));
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        std::thread::sleep(std::time::Duration::from_millis(300));
        let _ = handle.join(); // 不应 panic / hang
    }

    #[test]
    fn capture_outcome_uses_entries_field() {
        // 编译期断言: CaptureOutcome 必须有 entries: Vec<LogEntry> 字段
        // 这个测试如果编译过, 就证明字段名正确
        use luatos_log::LogEntry;
        let _entries: Vec<LogEntry> = crate::cmd_log::CaptureOutcome::default().entries;
    }
}
