use crate::OutputFormat;

pub fn cmd_log_view(port: &str, baud: u32, format: &OutputFormat) -> anyhow::Result<()> {
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_clone = stop.clone();
    let _ = ctrlc::set_handler(move || {
        stop_clone.store(true, std::sync::atomic::Ordering::Relaxed);
    });

    eprintln!("Viewing log on {port} @ {baud} bps (Ctrl+C to stop)");

    let dispatcher = luatos_log::LogDispatcher::default_parsers();
    let format_clone = format.clone();

    luatos_serial::stream_log_lines(
        port,
        baud,
        stop,
        Box::new(move |line| {
            let entry = dispatcher.parse(line);
            match format_clone {
                OutputFormat::Text => {
                    println!("{}", line);
                }
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string(&entry).unwrap_or_default());
                }
            }
        }),
    )?;

    eprintln!("\nLog viewing stopped.");
    Ok(())
}

pub fn cmd_log_view_binary(
    port: &str,
    baud: u32,
    probe: bool,
    format: &OutputFormat,
) -> anyhow::Result<()> {
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
            eprintln!("Auto-detecting EC718 log port (VID=0x19D1)...");
            match luatos_flash::ec718::find_ec718_log_port() {
                Some(p) => {
                    eprintln!("Found EC718 log port: {p}");
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
            anyhow::bail!(
                "No supported log device found. Try specifying the port manually with --port COMx"
            );
        }
    } else {
        port.to_string()
    };

    // For EC718 USB CDC, 921600 is the supported baud rate.
    // The info.json may specify 2000000 but Windows USB CDC rejects it.
    let baud = if is_ec718 && baud == 2000000 { 921600 } else { baud };

    // Build probe data — same 0xA5 probe works for both chip types
    let init_data = if probe {
        eprintln!("Sending probe to trigger log output ...");
        Some(luatos_flash::ec718::build_log_probe())
    } else {
        None
    };

    eprintln!(
        "Viewing {} binary log on {actual_port} @ {baud} bps (Ctrl+C to stop)",
        if is_ec718 { "EC718" } else { "SOC" }
    );

    let format_clone = format.clone();

    if is_ec718 {
        // EC718: 0x7E HDLC framing, DTR/RTS HIGH
        let decoder = std::sync::Mutex::new(luatos_log::Ec718LogDecoder::new());
        luatos_serial::stream_binary(
            &actual_port,
            baud,
            stop,
            Box::new(move |data| {
                if let Ok(mut dec) = decoder.lock() {
                    let entries = dec.feed(data);
                    for entry in &entries {
                        print_log_entry(entry, &format_clone);
                    }
                }
            }),
            init_data.as_deref(),
            true, // DTR/RTS HIGH for EC718
        )?;
    } else {
        // Standard SOC: 0xA5 framing
        let decoder = std::sync::Mutex::new(luatos_log::SocLogDecoder::new());
        luatos_serial::stream_binary(
            &actual_port,
            baud,
            stop,
            Box::new(move |data| {
                if let Ok(mut dec) = decoder.lock() {
                    let entries = dec.feed(data);
                    for entry in &entries {
                        print_log_entry(entry, &format_clone);
                    }
                }
            }),
            init_data.as_deref(),
            false,
        )?;
    }

    eprintln!("\nLog viewing stopped.");
    Ok(())
}

/// Format and print a single log entry to stdout.
fn print_log_entry(entry: &luatos_log::LogEntry, format: &OutputFormat) {
    match format {
        OutputFormat::Text => {
            let module = entry.module.as_deref().unwrap_or("-");
            let time = entry.device_time.as_deref().unwrap_or("?");
            println!("[{}] {}/{} {}", time, entry.level, module, entry.message);
        }
        OutputFormat::Json => {
            println!("{}", serde_json::to_string(&entry).unwrap_or_default());
        }
    }
}

pub fn cmd_log_record(
    port: &str,
    baud: u32,
    output_dir: &str,
    save_json: bool,
    format: &OutputFormat,
) -> anyhow::Result<()> {
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_clone = stop.clone();
    let _ = ctrlc::set_handler(move || {
        stop_clone.store(true, std::sync::atomic::Ordering::Relaxed);
    });

    let out_path = std::path::Path::new(output_dir);
    std::fs::create_dir_all(out_path)?;

    let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let text_path = out_path.join(format!("log_{timestamp}.txt"));
    let json_path = if save_json {
        Some(out_path.join(format!("log_{timestamp}.jsonl")))
    } else {
        None
    };

    let writer = luatos_log::LogWriter::new(Some(&text_path), json_path.as_deref())?;

    eprintln!(
        "Recording log on {port} @ {baud} bps → {}",
        text_path.display()
    );
    if let Some(ref jp) = json_path {
        eprintln!("  JSON log: {}", jp.display());
    }
    eprintln!("Press Ctrl+C to stop.");

    let dispatcher = luatos_log::LogDispatcher::default_parsers();
    let format_clone = format.clone();

    let writer = std::sync::Mutex::new(writer);
    let line_count = std::sync::atomic::AtomicUsize::new(0);

    luatos_serial::stream_log_lines(
        port,
        baud,
        stop,
        Box::new(move |line| {
            let entry = dispatcher.parse(line);

            match format_clone {
                OutputFormat::Text => {
                    println!("{}", line);
                }
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string(&entry).unwrap_or_default());
                }
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

    eprintln!("\nRecording stopped. Log saved to {}", text_path.display());
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
        OutputFormat::Json => {
            let json = serde_json::json!({
                "status": "ok",
                "command": "log.parse",
                "data": entries,
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
    }
    Ok(())
}
