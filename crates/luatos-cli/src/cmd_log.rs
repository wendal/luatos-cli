use crate::{
    event::{self, MessageLevel},
    OutputFormat,
};

pub fn cmd_log_view(port: &str, baud: u32, smart: bool, format: &OutputFormat) -> anyhow::Result<()> {
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_clone = stop.clone();
    let _ = ctrlc::set_handler(move || {
        stop_clone.store(true, std::sync::atomic::Ordering::Relaxed);
    });

    event::emit_message(format, "log.view", MessageLevel::Info, format!("Viewing log on {port} @ {baud} bps (Ctrl+C to stop)"))?;
    if smart {
        event::emit_message(format, "log.view", MessageLevel::Info, "🧠 智能分析已启用")?;
    }

    let dispatcher = luatos_log::LogDispatcher::default_parsers();
    let format_clone = *format;
    let analyzer = if smart {
        Some(std::sync::Mutex::new(luatos_log::smart::SmartAnalyzer::new()))
    } else {
        None
    };

    luatos_serial::stream_log_lines(
        port,
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

    // 输出智能分析汇总
    if let Some(analyzer) = if smart { Some(luatos_log::smart::SmartAnalyzer::new()) } else { None } {
        let _ = analyzer; // 已在回调中消费
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

pub fn cmd_log_view_binary(port: &str, baud: u32, probe: bool, save_dir: Option<&str>, smart: bool, format: &OutputFormat) -> anyhow::Result<()> {
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_clone = stop.clone();
    let _ = ctrlc::set_handler(move || {
        stop_clone.store(true, std::sync::atomic::Ordering::Relaxed);
    });

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
