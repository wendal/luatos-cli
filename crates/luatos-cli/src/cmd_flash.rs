use crate::OutputFormat;

pub fn cmd_flash_run(
    soc: &str,
    port: &str,
    baud: Option<u32>,
    script_folders: Option<&[String]>,
    format: &OutputFormat,
) -> anyhow::Result<()> {
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Set up Ctrl+C handler
    let cancel_clone = cancel.clone();
    ctrlc::set_handler(move || {
        eprintln!("\nCancelling flash...");
        cancel_clone.store(true, std::sync::atomic::Ordering::Relaxed);
    })?;

    let on_progress = make_progress_callback(format);

    // Detect chip type from SOC info.json
    let info = luatos_soc::read_soc_info(soc)?;
    let chip = info.chip.chip_type.as_str();

    match chip {
        "bk72xx" | "air8101" => {
            let folders_refs: Option<Vec<&str>> =
                script_folders.map(|dirs| dirs.iter().map(|s| s.as_str()).collect());
            let lines = luatos_flash::bk7258::flash_bk7258(
                soc,
                folders_refs.as_deref(),
                port,
                baud,
                cancel,
                on_progress,
            )?;
            match format {
                OutputFormat::Text => {
                    if !lines.is_empty() {
                        println!("\n--- Boot Log ({} lines) ---", lines.len());
                        for line in &lines {
                            println!("{line}");
                        }
                    }
                }
                OutputFormat::Json => {
                    let json = serde_json::json!({
                        "status": "ok",
                        "command": "flash.run",
                        "data": { "boot_log": lines },
                    });
                    println!("{}", serde_json::to_string_pretty(&json)?);
                }
            }
        }
        "air6208" | "air101" | "air103" | "air601" => {
            luatos_flash::xt804::flash_xt804(soc, port, on_progress, cancel)?;
            match format {
                OutputFormat::Text => {
                    println!("XT804 flash completed successfully.");
                }
                OutputFormat::Json => {
                    let json = serde_json::json!({
                        "status": "ok",
                        "command": "flash.run",
                        "data": { "chip": chip },
                    });
                    println!("{}", serde_json::to_string_pretty(&json)?);
                }
            }
        }
        "air1601" | "ccm4211" => {
            luatos_flash::ccm4211::flash_ccm4211(soc, port, &on_progress, cancel)?;
            match format {
                OutputFormat::Text => {
                    println!("CCM4211 flash completed successfully.");
                }
                OutputFormat::Json => {
                    let json = serde_json::json!({
                        "status": "ok",
                        "command": "flash.run",
                        "data": { "chip": chip },
                    });
                    println!("{}", serde_json::to_string_pretty(&json)?);
                }
            }
        }
        "ec7xx" | "air8000" | "air780epm" | "air780ehm" | "air780ehv" | "air780ehg" => {
            // EC718 series: auto-detect boot mode, reboot if needed
            let boot_port = luatos_flash::ec718::auto_enter_boot_mode(
                Some(port),
                &on_progress,
            )?;
            luatos_flash::ec718::flash_ec718(soc, &boot_port, &on_progress, cancel)?;
            match format {
                OutputFormat::Text => {
                    println!("EC718 flash completed successfully.");
                }
                OutputFormat::Json => {
                    let json = serde_json::json!({
                        "status": "ok",
                        "command": "flash.run",
                        "data": { "chip": chip },
                    });
                    println!("{}", serde_json::to_string_pretty(&json)?);
                }
            }
        }
        _ => {
            anyhow::bail!(
                "Unsupported chip type: {chip}. Supported: bk72xx, air6208, air101, air1601, ec7xx"
            );
        }
    }

    Ok(())
}

pub fn make_progress_callback(format: &OutputFormat) -> luatos_flash::ProgressCallback {
    let format_clone = format.clone();
    Box::new(move |p| match format_clone {
        OutputFormat::Text => {
            if p.percent >= 0.0 {
                eprintln!("[{:>6.1}%] {} — {}", p.percent, p.stage, p.message);
            } else {
                eprintln!("          {}", p.message);
            }
        }
        OutputFormat::Json => {
            println!("{}", serde_json::to_string(p).unwrap_or_default());
        }
    })
}

pub fn cmd_flash_partition(
    op: &str,
    soc: &str,
    port: &str,
    script_folders: Option<&[String]>,
    format: &OutputFormat,
) -> anyhow::Result<()> {
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    let cancel_clone = cancel.clone();
    let _ = ctrlc::set_handler(move || {
        eprintln!("\nCancelling...");
        cancel_clone.store(true, std::sync::atomic::Ordering::Relaxed);
    });

    let on_progress = make_progress_callback(format);

    // Detect chip type
    let info = luatos_soc::read_soc_info(soc)?;
    let chip = info.chip.chip_type.as_str();

    match chip {
        "bk72xx" | "air8101" => match op {
            "script" => {
                let folders = script_folders.expect("script folder required");
                let refs: Vec<&str> = folders.iter().map(|s| s.as_str()).collect();
                luatos_flash::bk7258::flash_script_only(soc, &refs, port, cancel, on_progress)?;
            }
            "clear-fs" => {
                luatos_flash::bk7258::clear_filesystem(soc, port, cancel, on_progress)?;
            }
            "flash-fs" => {
                let folders = script_folders.expect("script folder required");
                let refs: Vec<&str> = folders.iter().map(|s| s.as_str()).collect();
                luatos_flash::bk7258::flash_filesystem(soc, &refs, port, cancel, on_progress)?;
            }
            "clear-kv" => {
                luatos_flash::bk7258::clear_fskv(soc, port, cancel, on_progress)?;
            }
            _ => unreachable!(),
        },
        "air6208" | "air101" | "air103" | "air601" => match op {
            "script" => {
                let folders = script_folders.expect("script folder required");
                let files = collect_script_files(folders)?;
                luatos_flash::xt804::flash_script_only(soc, port, &files, on_progress, cancel)?;
            }
            "clear-fs" => {
                luatos_flash::xt804::clear_filesystem(soc, port, on_progress, cancel)?;
            }
            "flash-fs" => {
                let folders = script_folders.expect("fs folder required");
                let dir_strings: Vec<String> = folders.to_vec();
                luatos_flash::xt804::flash_filesystem(
                    soc,
                    port,
                    &dir_strings,
                    on_progress,
                    cancel,
                )?;
            }
            "clear-kv" => {
                luatos_flash::xt804::clear_kv(soc, port, on_progress, cancel)?;
            }
            _ => unreachable!(),
        },
        "air1601" | "ccm4211" => match op {
            "script" => {
                let folders = script_folders.expect("script folder required");
                let folder_paths: Vec<std::path::PathBuf> =
                    folders.iter().map(std::path::PathBuf::from).collect();
                let path_refs: Vec<&std::path::Path> =
                    folder_paths.iter().map(|p| p.as_path()).collect();
                let script_data = luatos_luadb::build::build_script_image(
                    &path_refs,
                    info.script_use_luac(),
                    info.script_bitw(),
                    info.use_bkcrc(),
                    true, // strip debug info
                )?;
                luatos_flash::ccm4211::flash_script_ccm4211(
                    soc,
                    port,
                    &script_data,
                    &on_progress,
                    cancel,
                )?;
            }
            "clear-fs" => {
                luatos_flash::ccm4211::clear_filesystem(soc, port, &on_progress, cancel)?;
            }
            "clear-kv" => {
                luatos_flash::ccm4211::clear_fskv(soc, port, &on_progress, cancel)?;
            }
            _ => unreachable!(),
        },
        "ec7xx" | "air8000" | "air780epm" | "air780ehm" | "air780ehv" | "air780ehg" => match op {
            "script" => {
                let folders = script_folders.expect("script folder required");
                let folder_paths: Vec<std::path::PathBuf> =
                    folders.iter().map(std::path::PathBuf::from).collect();
                let path_refs: Vec<&std::path::Path> =
                    folder_paths.iter().map(|p| p.as_path()).collect();
                let script_data = luatos_luadb::build::build_script_image(
                    &path_refs,
                    info.script_use_luac(),
                    info.script_bitw(),
                    info.use_bkcrc(),
                    true, // strip debug info
                )?;
                let boot_port = luatos_flash::ec718::auto_enter_boot_mode(
                    Some(port),
                    &on_progress,
                )?;
                luatos_flash::ec718::flash_script_ec718(
                    soc,
                    &boot_port,
                    &script_data,
                    &on_progress,
                    cancel,
                )?;
            }
            _ => {
                anyhow::bail!(
                    "EC718 only supports 'script' partition operation currently. \
                     Use 'flash run' for full firmware flash."
                );
            }
        },
        _ => {
            anyhow::bail!("Unsupported chip type: {chip}");
        }
    }

    match format {
        OutputFormat::Text => {
            println!("Operation '{op}' completed successfully.");
        }
        OutputFormat::Json => {
            let json = serde_json::json!({
                "status": "ok",
                "command": format!("flash.{op}"),
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
    }
    Ok(())
}

/// Collect script files from multiple folders (*.lua, *.luac, *.json, etc.)
fn collect_script_files(folders: &[String]) -> anyhow::Result<Vec<String>> {
    let mut files = Vec::new();
    for folder in folders {
        for entry in std::fs::read_dir(folder)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                files.push(path.to_string_lossy().to_string());
            }
        }
    }
    if files.is_empty() {
        anyhow::bail!("No script files found in {:?}", folders);
    }
    Ok(files)
}

/// Closed-loop flash test: flash firmware → capture boot log → check keywords → PASS/FAIL.
pub fn cmd_flash_test(
    soc: &str,
    port: &str,
    baud: Option<u32>,
    script_folders: Option<&[String]>,
    timeout_secs: u64,
    keywords: &[String],
    format: &OutputFormat,
) -> anyhow::Result<()> {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use std::time::{Duration, Instant};

    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_clone = cancel.clone();
    let _ = ctrlc::set_handler(move || {
        eprintln!("\nCancelling flash test...");
        cancel_clone.store(true, Ordering::Relaxed);
    });

    let on_progress = make_progress_callback(format);

    // Step 1: Flash the firmware
    let info = luatos_soc::read_soc_info(soc)?;
    let chip = info.chip.chip_type.clone();
    let log_br = info.log_baud_rate();
    // For EC718 USB CDC, 2000000 baud is not supported; use 921600
    let log_br = if matches!(chip.as_str(), "ec7xx" | "air8000" | "air780epm" | "air780ehm" | "air780ehv" | "air780ehg")
        && log_br == 2000000
    {
        921600
    } else {
        log_br
    };

    let boot_lines_from_flash: Vec<String> = match chip.as_str() {
        "bk72xx" | "air8101" => {
            let folders_refs: Option<Vec<&str>> =
                script_folders.map(|dirs| dirs.iter().map(|s| s.as_str()).collect());
            luatos_flash::bk7258::flash_bk7258(
                soc,
                folders_refs.as_deref(),
                port,
                baud,
                cancel.clone(),
                on_progress,
            )?
        }
        "air6208" | "air101" | "air103" | "air601" => {
            let on_progress2 = make_progress_callback(format);
            luatos_flash::xt804::flash_xt804(soc, port, on_progress2, cancel.clone())?;
            Vec::new() // XT804 does not return boot lines from flash
        }
        "air1601" | "ccm4211" => {
            let on_progress2 = make_progress_callback(format);
            luatos_flash::ccm4211::flash_ccm4211(soc, port, &on_progress2, cancel.clone())?;
            Vec::new()
        }
        "ec7xx" | "air8000" | "air780epm" | "air780ehm" | "air780ehv" | "air780ehg" => {
            let on_progress2 = make_progress_callback(format);
            let boot_port = luatos_flash::ec718::auto_enter_boot_mode(
                Some(port),
                &on_progress2,
            )?;
            luatos_flash::ec718::flash_ec718(soc, &boot_port, &on_progress2, cancel.clone())?;
            Vec::new()
        }
        _ => {
            anyhow::bail!("Unsupported chip type for flash test: {chip}");
        }
    };

    if cancel.load(Ordering::Relaxed) {
        anyhow::bail!("Flash test cancelled by user");
    }

    // Step 2: Capture boot log (append to any lines from flash)
    let mut all_lines = boot_lines_from_flash;

    // Determine if this chip uses binary SOC log protocol
    let is_ec718 = matches!(chip.as_str(), "ec7xx" | "air8000" | "air780epm" | "air780ehm" | "air780ehv" | "air780ehg");
    let use_binary_log = matches!(chip.as_str(), "air1601" | "ccm4211") || is_ec718;

    // For EC718: after flash+reset, the boot port disappears and the module
    // re-enumerates as running mode (VID=0x19D1). We need to wait for the
    // new log port to appear and use that instead of the original port.
    let log_port: String = if is_ec718 {
        if format == &OutputFormat::Text {
            eprintln!("Waiting for EC718 module to reboot and re-enumerate USB...");
        }
        // Wait up to 15s for the log port to appear
        match luatos_flash::ec718::wait_for_log_port(15) {
            Some(p) => {
                if format == &OutputFormat::Text {
                    eprintln!("Found EC718 log port: {p}");
                }
                // Give USB a moment to stabilize
                std::thread::sleep(Duration::from_millis(500));
                p
            }
            None => {
                if format == &OutputFormat::Text {
                    eprintln!("EC718 log port not found, trying original port {port}");
                }
                port.to_string()
            }
        }
    } else {
        port.to_string()
    };

    if format == &OutputFormat::Text {
        eprintln!("Capturing boot log for {timeout_secs}s on {log_port} @ {log_br}...");
    }

    // Open serial port and capture lines for the timeout period
    let serial = serialport::new(&log_port, log_br)
        .timeout(Duration::from_millis(500))
        .open();

    if let Ok(mut serial) = serial {
        use std::io::{Read, Write};

        // Send probe to trigger log output on binary-log chips
        if use_binary_log {
            let probe = if is_ec718 {
                luatos_flash::ec718::build_log_probe()
            } else {
                luatos_flash::ccm4211::build_log_probe()
            };
            let _ = serial.write_all(&probe);
            let _ = serial.flush();
        }

        let start = Instant::now();
        let timeout = Duration::from_secs(timeout_secs);
        let mut buf = vec![0u8; 4096];

        if use_binary_log {
            if is_ec718 {
                // EC718: 0x7E framed binary log via Ec718LogDecoder
                let _ = serial.write_data_terminal_ready(true);
                let _ = serial.write_request_to_send(true);
                let mut decoder = luatos_log::Ec718LogDecoder::new();
                while start.elapsed() < timeout && !cancel.load(Ordering::Relaxed) {
                    match serial.read(&mut buf) {
                        Ok(n) if n > 0 => {
                            let entries = decoder.feed(&buf[..n]);
                            for entry in &entries {
                                let module = entry.module.as_deref().unwrap_or("-");
                                let msg = format!(
                                    "[{}] {}/{} {}",
                                    entry.device_time.as_deref().unwrap_or("?"),
                                    entry.level,
                                    module,
                                    entry.message
                                );
                                all_lines.push(msg);
                            }
                            let found_all = keywords
                                .iter()
                                .all(|kw| all_lines.iter().any(|line| line.contains(kw.as_str())));
                            if found_all {
                                break;
                            }
                        }
                        Ok(_) => {}
                        Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                        Err(e) => {
                            log::warn!("Serial read error: {e}");
                            break;
                        }
                    }
                }
            } else {
                // Standard SOC: 0xA5 framed binary log via SocLogDecoder
                let mut decoder = luatos_log::SocLogDecoder::new();
                while start.elapsed() < timeout && !cancel.load(Ordering::Relaxed) {
                    match serial.read(&mut buf) {
                        Ok(n) if n > 0 => {
                            let entries = decoder.feed(&buf[..n]);
                            for entry in &entries {
                                let module = entry.module.as_deref().unwrap_or("-");
                                let msg = format!(
                                    "[{}] {}/{} {}",
                                    entry.device_time.as_deref().unwrap_or("?"),
                                    entry.level,
                                    module,
                                    entry.message
                                );
                                all_lines.push(msg);
                            }
                            let found_all = keywords
                                .iter()
                                .all(|kw| all_lines.iter().any(|line| line.contains(kw.as_str())));
                            if found_all {
                                break;
                            }
                        }
                        Ok(_) => {}
                        Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                        Err(e) => {
                            log::warn!("Serial read error: {e}");
                            break;
                        }
                    }
                }
            }
        } else {
            // Text log: parse as newline-delimited text
            let mut line_buf = String::new();
            while start.elapsed() < timeout && !cancel.load(Ordering::Relaxed) {
                match serial.read(&mut buf) {
                    Ok(n) if n > 0 => {
                        let text = String::from_utf8_lossy(&buf[..n]);
                        for ch in text.chars() {
                            if ch == '\n' {
                                let line = line_buf.trim_end_matches('\r').to_string();
                                if !line.is_empty() {
                                    all_lines.push(line);
                                }
                                line_buf.clear();
                            } else {
                                line_buf.push(ch);
                            }
                        }

                        // Early exit if we already found all keywords
                        let found_all = keywords
                            .iter()
                            .all(|kw| all_lines.iter().any(|line| line.contains(kw.as_str())));
                        if found_all {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                    Err(e) => {
                        log::warn!("Serial read error: {e}");
                        break;
                    }
                }
            }
            // Flush remaining line buffer
            if !line_buf.is_empty() {
                all_lines.push(line_buf.trim_end_matches('\r').to_string());
            }
        }
    } else if all_lines.is_empty() {
        // Could not open serial and no lines from flash
        log::warn!("Could not open serial port for boot log capture");
    }

    // Step 3: Evaluate keywords
    let mut keyword_results: Vec<(String, bool)> = Vec::new();
    for kw in keywords {
        let found = all_lines.iter().any(|line| line.contains(kw.as_str()));
        keyword_results.push((kw.clone(), found));
    }

    let all_passed = keyword_results.iter().all(|(_, found)| *found);
    let result_str = if all_passed { "PASS" } else { "FAIL" };

    // Step 4: Output
    match format {
        OutputFormat::Text => {
            println!("\n===== Flash Test Result: {} =====", result_str);
            println!("  Chip:     {}", chip);
            println!("  SOC:      {}", soc);
            println!("  Port:     {}", port);
            println!("  Log lines: {}", all_lines.len());
            for (kw, found) in &keyword_results {
                let icon = if *found { "✓" } else { "✗" };
                println!(
                    "  [{icon}] Keyword \"{kw}\": {}",
                    if *found { "FOUND" } else { "NOT FOUND" }
                );
            }
            if !all_lines.is_empty() {
                println!("\n--- Boot Log ({} lines) ---", all_lines.len());
                for line in &all_lines {
                    println!("{line}");
                }
            }
        }
        OutputFormat::Json => {
            let json = serde_json::json!({
                "status": if all_passed { "ok" } else { "fail" },
                "command": "flash.test",
                "data": {
                    "result": result_str,
                    "chip": chip,
                    "soc": soc,
                    "port": port,
                    "keywords": keyword_results.iter().map(|(kw, found)| {
                        serde_json::json!({ "keyword": kw, "found": found })
                    }).collect::<Vec<_>>(),
                    "boot_log": all_lines,
                    "log_line_count": all_lines.len(),
                },
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
    }

    if !all_passed {
        std::process::exit(1);
    }

    Ok(())
}
