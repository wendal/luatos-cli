use anyhow::Context;

use crate::{
    event::{self, MessageLevel},
    OutputFormat,
};

/// 检查脚本镜像大小是否超过分区容量，超出则报错并给出详细信息
fn check_script_size(image_len: usize, partition_size: usize) -> anyhow::Result<()> {
    if image_len > partition_size {
        let overflow = image_len - partition_size;
        anyhow::bail!(
            "脚本镜像大小（{} 字节, {:.1} KB）超过分区容量（{} 字节, {:.1} KB），超出 {} 字节（{:.1} KB）。\
             请减少脚本文件数量或大小",
            image_len,
            image_len as f64 / 1024.0,
            partition_size,
            partition_size as f64 / 1024.0,
            overflow,
            overflow as f64 / 1024.0,
        );
    }
    Ok(())
}

pub fn cmd_flash_run(
    soc: &str,
    port: &str,
    baud: Option<u32>,
    script_folders: Option<&[String]>,
    step: u8,
    format: &OutputFormat,
    reset_config: Option<luatos_flash::sf32lb5x::Sf32ResetConfig>,
) -> anyhow::Result<()> {
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let format_clone = *format;

    // Set up Ctrl+C handler
    let cancel_clone = cancel.clone();
    ctrlc::set_handler(move || {
        if let Err(e) = event::emit_message(&format_clone, "flash.run", MessageLevel::Warn, "Cancelling flash...") {
            log::warn!("输出取消事件失败: {e}");
        }
        cancel_clone.store(true, std::sync::atomic::Ordering::Relaxed);
    })?;

    let on_progress = make_progress_callback(format, "flash.run", step);

    // Detect chip type from SOC info.json
    let info = luatos_soc::read_soc_info(soc)?;
    let chip = info.chip.chip_type.as_str();

    match chip {
        "bk72xx" | "air8101" => {
            let folders_refs: Option<Vec<&str>> = script_folders.map(|dirs| dirs.iter().map(|s| s.as_str()).collect());
            let lines = luatos_flash::bk7258::flash_bk7258(soc, folders_refs.as_deref(), port, baud, cancel, on_progress)?;
            match format {
                OutputFormat::Text => {
                    if !lines.is_empty() {
                        println!("\n--- Boot Log ({} lines) ---", lines.len());
                        for line in &lines {
                            println!("{line}");
                        }
                    }
                }
                OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(format, "flash.run", "ok", serde_json::json!({ "boot_log": lines }))?,
            }
        }
        "air6208" | "air101" | "air103" | "air601" => {
            luatos_flash::xt804::flash_xt804(soc, port, on_progress, cancel)?;
            match format {
                OutputFormat::Text => {
                    println!("XT804 flash completed successfully.");
                }
                OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(format, "flash.run", "ok", serde_json::json!({ "chip": chip }))?,
            }
        }
        "air1601" | "ccm4211" => {
            luatos_flash::ccm4211::flash_ccm4211(soc, port, &on_progress, cancel)?;
            match format {
                OutputFormat::Text => {
                    println!("CCM4211 flash completed successfully.");
                }
                OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(format, "flash.run", "ok", serde_json::json!({ "chip": chip }))?,
            }
        }
        "ec7xx" | "air8000" | "air780epm" | "air780ehm" | "air780ehv" | "air780ehg" => {
            // EC718 series: auto-detect boot mode, reboot if needed
            let boot_port = luatos_flash::ec718::auto_enter_boot_mode(Some(port), &on_progress)?;
            luatos_flash::ec718::flash_ec718(soc, &boot_port, &on_progress, cancel)?;
            match format {
                OutputFormat::Text => {
                    println!("EC718 flash completed successfully.");
                }
                OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(format, "flash.run", "ok", serde_json::json!({ "chip": chip }))?,
            }
        }
        "sf32lb58" => {
            let folders_refs: Option<Vec<&str>> = script_folders.map(|dirs| dirs.iter().map(|s| s.as_str()).collect());
            luatos_flash::sf32lb5x::flash_sf32lb5x(soc, port, folders_refs.as_deref(), on_progress, cancel, reset_config.as_ref(), baud)?;
            match format {
                OutputFormat::Text => {
                    println!("SF32LB58 flash completed successfully.");
                }
                OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(format, "flash.run", "ok", serde_json::json!({ "chip": chip }))?,
            }
        }
        _ => {
            anyhow::bail!("Unsupported chip type: {chip}. Supported: bk72xx, air6208, air101, air1601, ec7xx");
        }
    }

    Ok(())
}

pub fn make_progress_callback(format: &OutputFormat, command: impl Into<String>, step: u8) -> luatos_flash::ProgressCallback {
    let format_clone = *format;
    let command = command.into();
    let step = step as f32;
    // 追踪上次输出的 (percent, stage)，用于步进过滤
    let state = std::sync::Mutex::new((f32::NEG_INFINITY, String::new()));
    Box::new(move |p| {
        let should_emit = {
            let mut s = state.lock().unwrap();
            let (last_pct, last_stage) = &mut *s;
            if p.done || p.error {
                // 完成/错误事件始终输出
                true
            } else {
                let stage_changed = p.stage != *last_stage;
                let pct_step_reached = (p.percent - *last_pct).abs() >= step;
                if stage_changed || pct_step_reached {
                    *last_pct = p.percent;
                    *last_stage = p.stage.clone();
                    true
                } else {
                    false
                }
            }
        };
        if should_emit {
            if let Err(e) = event::emit_flash_progress(&format_clone, &command, p) {
                log::warn!("输出进度事件失败: {e}");
            }
        }
    })
}

pub fn cmd_flash_partition(
    op: &str,
    soc: &str,
    port: &str,
    script_folders: Option<&[String]>,
    step: u8,
    format: &OutputFormat,
    reset_config: Option<luatos_flash::sf32lb5x::Sf32ResetConfig>,
    baud: Option<u32>,
) -> anyhow::Result<()> {
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let command = format!("flash.{op}");
    let cancel_command = command.clone();
    let format_clone = *format;

    let cancel_clone = cancel.clone();
    let _ = ctrlc::set_handler(move || {
        if let Err(e) = event::emit_message(&format_clone, &cancel_command, MessageLevel::Warn, "Cancelling...") {
            log::warn!("输出取消事件失败: {e}");
        }
        cancel_clone.store(true, std::sync::atomic::Ordering::Relaxed);
    });

    let on_progress = make_progress_callback(format, command.clone(), step);

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
                luatos_flash::xt804::flash_filesystem(soc, port, &dir_strings, on_progress, cancel)?;
            }
            "clear-kv" => {
                luatos_flash::xt804::clear_kv(soc, port, on_progress, cancel)?;
            }
            _ => unreachable!(),
        },
        "air1601" | "ccm4211" => match op {
            "script" => {
                let folders = script_folders.expect("script folder required");
                let folder_paths: Vec<std::path::PathBuf> = folders.iter().map(std::path::PathBuf::from).collect();
                let path_refs: Vec<&std::path::Path> = folder_paths.iter().map(|p| p.as_path()).collect();
                let script_data = luatos_luadb::build::build_script_image(
                    &path_refs,
                    info.script_use_luac(),
                    info.script_bitw(),
                    info.use_bkcrc(),
                    true, // strip debug info
                )?;
                check_script_size(script_data.len(), info.script_size())?;
                luatos_flash::ccm4211::flash_script_ccm4211(soc, port, &script_data, &on_progress, cancel)?;
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
                let folder_paths: Vec<std::path::PathBuf> = folders.iter().map(std::path::PathBuf::from).collect();
                let path_refs: Vec<&std::path::Path> = folder_paths.iter().map(|p| p.as_path()).collect();
                let script_data = luatos_luadb::build::build_script_image(
                    &path_refs,
                    info.script_use_luac(),
                    info.script_bitw(),
                    info.use_bkcrc(),
                    true, // strip debug info
                )?;
                check_script_size(script_data.len(), info.script_size())?;
                let boot_port = luatos_flash::ec718::auto_enter_boot_mode(Some(port), &on_progress)?;
                luatos_flash::ec718::flash_script_ec718(soc, &boot_port, &script_data, &on_progress, cancel)?;
            }
            _ => {
                anyhow::bail!(
                    "EC718 only supports 'script' partition operation currently. \
                     Use 'flash run' for full firmware flash."
                );
            }
        },
        "sf32lb58" => match op {
            "script" => {
                let folders = script_folders.expect("script folder required");
                let refs: Vec<&str> = folders.iter().map(|s| s.as_str()).collect();
                luatos_flash::sf32lb5x::flash_script_sf32lb5x(soc, port, &refs, on_progress, cancel, reset_config.as_ref(), baud)?;
            }
            "clear-kv" => {
                luatos_flash::sf32lb5x::clear_kv_sf32lb5x(soc, port, on_progress, cancel, reset_config.as_ref(), baud)?;
            }
            "clear-fs" | "flash-fs" => {
                anyhow::bail!("SF32LB58 {op} 暂不支持，请手动使用 ImgDownUart 或其他工具操作分区");
            }
            _ => unreachable!(),
        },
        _ => {
            anyhow::bail!("Unsupported chip type: {chip}");
        }
    }

    match format {
        OutputFormat::Text => {
            println!("Operation '{op}' completed successfully.");
        }
        OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(format, &command, "ok", serde_json::json!({}))?,
    }
    Ok(())
}

/// Collect script files from multiple folders (*.lua, *.luac, *.json, etc.)
/// 自动跳过 .git/.svn/.hg 等版本控制目录。
fn collect_script_files(folders: &[String]) -> anyhow::Result<Vec<String>> {
    const VCS_DIRS: &[&str] = &[".git", ".svn", ".hg"];

    let mut files = Vec::new();
    for folder in folders {
        let dir = std::path::Path::new(folder);
        anyhow::ensure!(dir.exists(), "脚本目录不存在: {}", folder);
        anyhow::ensure!(dir.is_dir(), "指定路径不是目录: {}", folder);
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            // 跳过版本控制目录
            if path.is_dir() {
                if let Some(name) = path.file_name() {
                    let s = name.to_string_lossy();
                    if VCS_DIRS.iter().any(|d| s.eq_ignore_ascii_case(d)) {
                        continue;
                    }
                }
            }
            if path.is_file() {
                files.push(path.to_string_lossy().to_string());
            }
        }
    }
    if files.is_empty() {
        anyhow::bail!("脚本目录中没有找到任何文件: {:?}", folders);
    }
    Ok(files)
}

/// Air6201 外置 SPI Flash 烧录
pub fn cmd_flash_ext_flash(port: &str, baud: u32, partition: &str, file: &str, ext_prog: bool, step: u8, format: &OutputFormat) -> anyhow::Result<()> {
    let data = std::fs::read(file).with_context(|| format!("无法读取文件: {file}"))?;
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let format_clone = *format;

    let cancel_clone = cancel.clone();
    let _ = ctrlc::set_handler(move || {
        if let Err(e) = event::emit_message(&format_clone, "flash.ext-flash", MessageLevel::Warn, "Cancelling...") {
            log::warn!("输出取消事件失败: {e}");
        }
        cancel_clone.store(true, std::sync::atomic::Ordering::Relaxed);
    });

    let on_progress = make_progress_callback(format, "flash.ext-flash", step);
    luatos_flash::air6201::flash_partition(port, baud, partition, &data, ext_prog, &on_progress, cancel)?;

    match format {
        OutputFormat::Text => println!("External flash write completed."),
        OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(format, "flash.ext-flash", "ok", serde_json::json!({ "partition": partition, "size": data.len() }))?,
    }
    Ok(())
}

/// Air6201 外置 SPI Flash 分区擦除
pub fn cmd_flash_ext_erase(port: &str, baud: u32, partition: &str, ext_prog: bool, step: u8, format: &OutputFormat) -> anyhow::Result<()> {
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let format_clone = *format;

    let cancel_clone = cancel.clone();
    let _ = ctrlc::set_handler(move || {
        if let Err(e) = event::emit_message(&format_clone, "flash.ext-erase", MessageLevel::Warn, "Cancelling...") {
            log::warn!("输出取消事件失败: {e}");
        }
        cancel_clone.store(true, std::sync::atomic::Ordering::Relaxed);
    });

    let on_progress = make_progress_callback(format, "flash.ext-erase", step);
    luatos_flash::air6201::erase_ext_partition(port, baud, partition, ext_prog, &on_progress, cancel)?;

    match format {
        OutputFormat::Text => println!("External flash erase completed."),
        OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(format, "flash.ext-erase", "ok", serde_json::json!({ "partition": partition }))?,
    }
    Ok(())
}

/// Closed-loop flash test: flash firmware → capture boot log → check keywords → PASS/FAIL.
#[allow(clippy::too_many_arguments)]
pub fn cmd_flash_test(
    soc: &str,
    port: &str,
    baud: Option<u32>,
    script_folders: Option<&[String]>,
    timeout_secs: u64,
    keywords: &[String],
    step: u8,
    format: &OutputFormat,
) -> anyhow::Result<()> {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use std::time::{Duration, Instant};

    let cancel = Arc::new(AtomicBool::new(false));
    let format_clone = *format;
    let cancel_clone = cancel.clone();
    let _ = ctrlc::set_handler(move || {
        if let Err(e) = event::emit_message(&format_clone, "flash.test", MessageLevel::Warn, "Cancelling flash test...") {
            log::warn!("输出取消事件失败: {e}");
        }
        cancel_clone.store(true, Ordering::Relaxed);
    });

    let on_progress = make_progress_callback(format, "flash.test", step);

    // Step 1: Flash the firmware
    let info = luatos_soc::read_soc_info(soc)?;
    let chip = info.chip.chip_type.clone();
    let log_br = info.log_baud_rate();
    // For EC718 USB CDC, 2000000 baud is not supported; use 921600
    let log_br = if matches!(chip.as_str(), "ec7xx" | "air8000" | "air780epm" | "air780ehm" | "air780ehv" | "air780ehg") && log_br == 2000000 {
        921600
    } else {
        log_br
    };

    let boot_lines_from_flash: Vec<String> = match chip.as_str() {
        "bk72xx" | "air8101" => {
            let folders_refs: Option<Vec<&str>> = script_folders.map(|dirs| dirs.iter().map(|s| s.as_str()).collect());
            luatos_flash::bk7258::flash_bk7258(soc, folders_refs.as_deref(), port, baud, cancel.clone(), on_progress)?
        }
        "air6208" | "air101" | "air103" | "air601" => {
            let on_progress2 = make_progress_callback(format, "flash.test", step);
            luatos_flash::xt804::flash_xt804(soc, port, on_progress2, cancel.clone())?;
            Vec::new() // XT804 does not return boot lines from flash
        }
        "air1601" | "ccm4211" => {
            let on_progress2 = make_progress_callback(format, "flash.test", step);
            luatos_flash::ccm4211::flash_ccm4211(soc, port, &on_progress2, cancel.clone())?;
            Vec::new()
        }
        "ec7xx" | "air8000" | "air780epm" | "air780ehm" | "air780ehv" | "air780ehg" => {
            let on_progress2 = make_progress_callback(format, "flash.test", step);
            let boot_port = luatos_flash::ec718::auto_enter_boot_mode(Some(port), &on_progress2)?;
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
        event::emit_message(format, "flash.test", MessageLevel::Info, "Waiting for EC718 module to reboot and re-enumerate USB...")?;
        // Wait up to 15s for the log port to appear
        match luatos_flash::ec718::wait_for_log_port(15) {
            Some(p) => {
                event::emit_message(format, "flash.test", MessageLevel::Info, format!("Found EC718 log port: {p}"))?;
                // Give USB a moment to stabilize
                std::thread::sleep(Duration::from_millis(500));
                p
            }
            None => {
                event::emit_message(format, "flash.test", MessageLevel::Warn, format!("EC718 log port not found, trying original port {port}"))?;
                port.to_string()
            }
        }
    } else {
        port.to_string()
    };

    event::emit_message(
        format,
        "flash.test",
        MessageLevel::Info,
        format!("Capturing boot log for {timeout_secs}s on {log_port} @ {log_br}..."),
    )?;

    // Open serial port and capture lines for the timeout period
    let serial = serialport::new(&log_port, log_br).timeout(Duration::from_millis(500)).open();

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
                                let msg = event::format_log_entry(entry);
                                let _ = event::emit_jsonl_event(
                                    format,
                                    serde_json::json!({
                                        "type": "boot_log_line",
                                        "command": "flash.test",
                                        "line": msg,
                                    }),
                                );
                                all_lines.push(msg);
                            }
                            let found_all = keywords.iter().all(|kw| all_lines.iter().any(|line| line.contains(kw.as_str())));
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
                                let msg = event::format_log_entry(entry);
                                let _ = event::emit_jsonl_event(
                                    format,
                                    serde_json::json!({
                                        "type": "boot_log_line",
                                        "command": "flash.test",
                                        "line": msg,
                                    }),
                                );
                                all_lines.push(msg);
                            }
                            let found_all = keywords.iter().all(|kw| all_lines.iter().any(|line| line.contains(kw.as_str())));
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
                                    let _ = event::emit_jsonl_event(
                                        format,
                                        serde_json::json!({
                                            "type": "boot_log_line",
                                            "command": "flash.test",
                                            "line": line,
                                        }),
                                    );
                                    all_lines.push(line);
                                }
                                line_buf.clear();
                            } else {
                                line_buf.push(ch);
                            }
                        }

                        // Early exit if we already found all keywords
                        let found_all = keywords.iter().all(|kw| all_lines.iter().any(|line| line.contains(kw.as_str())));
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
                let line = line_buf.trim_end_matches('\r').to_string();
                let _ = event::emit_jsonl_event(
                    format,
                    serde_json::json!({
                        "type": "boot_log_line",
                        "command": "flash.test",
                        "line": line,
                    }),
                );
                all_lines.push(line);
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
                println!("  [{icon}] Keyword \"{kw}\": {}", if *found { "FOUND" } else { "NOT FOUND" });
            }
            if !all_lines.is_empty() {
                println!("\n--- Boot Log ({} lines) ---", all_lines.len());
                for line in &all_lines {
                    println!("{line}");
                }
            }
        }
        OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(
            format,
            "flash.test",
            if all_passed { "ok" } else { "fail" },
            serde_json::json!({
                "result": result_str,
                "chip": chip,
                "soc": soc,
                "port": port,
                "keywords": keyword_results.iter().map(|(kw, found)| {
                    serde_json::json!({ "keyword": kw, "found": found })
                }).collect::<Vec<_>>(),
                "boot_log": all_lines,
                "log_line_count": all_lines.len(),
            }),
        )?,
    }

    if !all_passed {
        std::process::exit(1);
    }

    Ok(())
}
