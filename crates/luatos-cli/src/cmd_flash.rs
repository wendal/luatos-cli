use anyhow::Context;
use luatos_soc::ChipFamily;

use crate::{
    cmd_log,
    event::{self, MessageLevel},
    reset_args::ResetArgs,
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

#[cfg(test)]
fn script_folders_present(folders: Option<&[String]>) -> bool {
    folders.is_some_and(|dirs| !dirs.is_empty())
}

fn build_script_overlay(script_folders: Option<&[String]>, info: &luatos_soc::SocInfo) -> anyhow::Result<Option<Vec<u8>>> {
    let Some(folders) = script_folders.filter(|dirs| !dirs.is_empty()) else {
        return Ok(None);
    };
    Ok(Some(build_script_image_checked(folders, info)?))
}

fn build_script_image_checked(folders: &[String], info: &luatos_soc::SocInfo) -> anyhow::Result<Vec<u8>> {
    let folder_paths: Vec<std::path::PathBuf> = folders.iter().map(std::path::PathBuf::from).collect();
    let path_refs: Vec<&std::path::Path> = folder_paths.iter().map(|p| p.as_path()).collect();
    let script_data = luatos_luadb::build::build_script_image(&path_refs, info.script_use_luac(), info.script_bitw(), info.use_bkcrc(), luatos_luadb::LUAC_DEBUG_ALL)?;
    check_script_size(script_data.len(), info.script_size())?;
    Ok(script_data)
}

fn tail_log_after_flash(soc: &str, port: &str, timeout_secs: u64, format: &OutputFormat) -> anyhow::Result<()> {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use std::time::Duration;

    let info = luatos_soc::read_soc_info(soc)?;
    let chip = info.chip.chip_type.as_str();
    let (use_binary_log, is_ec718, log_baud) = cmd_log::resolve_log_mode(chip, info.log_baud_rate());

    let log_port: String = if is_ec718 {
        match luatos_flash::ec718::wait_for_log_port(15) {
            Some(p) => p,
            None => port.to_string(),
        }
    } else {
        port.to_string()
    };

    event::emit_message(
        format,
        "flash.run",
        MessageLevel::Info,
        format!("刷机完成，继续监听日志 {timeout_secs}s: {log_port} @ {log_baud}"),
    )?;

    let stop = Arc::new(AtomicBool::new(false));
    let stop_timer = stop.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(timeout_secs));
        stop_timer.store(true, Ordering::Relaxed);
    });

    if use_binary_log {
        let decoder = std::sync::Mutex::new(if is_ec718 { None } else { Some(luatos_log::SocLogDecoder::new()) });
        let ec718_decoder = std::sync::Mutex::new(if is_ec718 { Some(luatos_log::Ec718LogDecoder::new()) } else { None });
        let fmt = *format;
        let init_data = Some(luatos_flash::ec718::build_log_probe());
        luatos_serial::stream_binary(
            &log_port,
            log_baud,
            stop,
            Box::new(move |data| {
                if let Ok(mut dec) = decoder.lock() {
                    if let Some(ref mut soc_dec) = *dec {
                        for entry in soc_dec.feed(data) {
                            let _ = event::emit_log_entry(&fmt, "flash.run.tail", &entry);
                        }
                    }
                }
                if let Ok(mut dec) = ec718_decoder.lock() {
                    if let Some(ref mut ec_dec) = *dec {
                        for entry in ec_dec.feed(data) {
                            let _ = event::emit_log_entry(&fmt, "flash.run.tail", &entry);
                        }
                    }
                }
            }),
            init_data.as_deref(),
            is_ec718,
        )?;
    } else {
        let dispatcher = luatos_log::LogDispatcher::default_parsers();
        let fmt = *format;
        luatos_serial::stream_log_lines(
            &log_port,
            log_baud,
            stop,
            Box::new(move |line| {
                let entry = dispatcher.parse(line);
                let _ = event::emit_log_entry(&fmt, "flash.run.tail", &entry);
            }),
        )?;
    }

    Ok(())
}

/// 注册 Ctrl+C 取消处理。trun 等流程已注册过时静默忽略（返回 false 表示未接管）。
/// 统一取消消息为 "Cancelling..."，避免各刷机入口维护多份重复的 ctrlc 闭包。
fn install_cancel_handler(format: &OutputFormat, command: &str, cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>) {
    let format_clone = *format;
    let command = command.to_string();
    let cancel_clone = cancel.clone();
    let _ = ctrlc::set_handler(move || {
        if let Err(e) = event::emit_message(&format_clone, &command, MessageLevel::Warn, "Cancelling...") {
            log::warn!("输出取消事件失败: {e}");
        }
        cancel_clone.store(true, std::sync::atomic::Ordering::Relaxed);
    });
}

/// cmd_flash_run / cmd_flash_partition 支持刷机的芯片族（作为分发 match 的前置校验）。
/// 每个已知芯片族都必须在此有明确归属，避免新增族时静默落入 Unsupported 分支。
fn family_flash_supported(family: ChipFamily) -> bool {
    matches!(
        family,
        ChipFamily::Bk72xx | ChipFamily::Xt804 | ChipFamily::Ccm4211 | ChipFamily::Ec718 | ChipFamily::Sf32lb58
    )
}

#[allow(clippy::too_many_arguments)]
pub fn cmd_flash_run(
    soc: &str,
    port: &str,
    baud: Option<u32>,
    script_folders: Option<&[String]>,
    step: u8,
    format: &OutputFormat,
    reset: &ResetArgs,
    reset_config: Option<luatos_flash::sf32lb5x::Sf32ResetConfig>,
    tail_log_secs: u64,
) -> anyhow::Result<()> {
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Set up Ctrl+C handler (容错: trun 流程下, trun 已经注册过, 第二次注册返回
    // Error::MultipleHandlers, 这里静默忽略。单独调用 cmd_flash run 时,
    // 这是首次注册, 正常生效。)
    install_cancel_handler(format, "flash.run", &cancel);

    let on_progress = make_progress_callback(format, "flash.run", step);

    // Detect chip type from SOC info.json
    let info = luatos_soc::read_soc_info(soc)?;
    let family = info.family();

    if !family_flash_supported(family) {
        anyhow::bail!(
            "Unsupported chip type: {}. Supported: bk72xx, air6208, air101, air1601, air1602, ec7xx",
            info.chip.chip_type
        );
    }

    match family {
        ChipFamily::Bk72xx => {
            let folders_refs: Option<Vec<&str>> = script_folders.map(|dirs| dirs.iter().map(|s| s.as_str()).collect());
            let lines = luatos_flash::bk7258::flash_bk7258(soc, folders_refs.as_deref(), port, baud, cancel, on_progress, true)?;
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
        ChipFamily::Xt804 => {
            reset.execute(port)?;
            luatos_flash::xt804::flash_xt804(soc, port, on_progress, cancel)?;
            match format {
                OutputFormat::Text => {
                    println!("XT804 flash completed successfully.");
                }
                OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(format, "flash.run", "ok", serde_json::json!({ "chip": family.name() }))?,
            }
        }
        ChipFamily::Ccm4211 => {
            let overlay = build_script_overlay(script_folders, &info)?;
            if overlay.is_some() {
                event::emit_message(format, "flash.run", MessageLevel::Info, "Applying script overlay from --script folders...")?;
            }
            luatos_flash::ccm4211::flash_ccm4211(soc, port, &on_progress, cancel, overlay.as_deref())?;
            match format {
                OutputFormat::Text => {
                    println!("CCM4211 flash completed successfully.");
                }
                OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(format, "flash.run", "ok", serde_json::json!({ "chip": family.name() }))?,
            }
        }
        ChipFamily::Ec718 => {
            // EC718 series: auto-detect boot mode, reboot if needed
            let overlay = build_script_overlay(script_folders, &info)?;
            if overlay.is_some() {
                event::emit_message(format, "flash.run", MessageLevel::Info, "Applying script overlay from --script folders...")?;
            }
            let boot_port = luatos_flash::ec718::auto_enter_boot_mode(Some(port), &on_progress)?;
            luatos_flash::ec718::flash_ec718(soc, &boot_port, &on_progress, cancel, overlay.as_deref())?;
            match format {
                OutputFormat::Text => {
                    println!("EC718 flash completed successfully.");
                }
                OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(format, "flash.run", "ok", serde_json::json!({ "chip": family.name() }))?,
            }
        }
        ChipFamily::Sf32lb58 => {
            let folders_refs: Option<Vec<&str>> = script_folders.map(|dirs| dirs.iter().map(|s| s.as_str()).collect());
            luatos_flash::sf32lb5x::flash_sf32lb5x(soc, port, folders_refs.as_deref(), on_progress, cancel, reset_config.as_ref(), baud)?;
            match format {
                OutputFormat::Text => {
                    println!("SF32LB58 flash completed successfully.");
                }
                OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(format, "flash.run", "ok", serde_json::json!({ "chip": family.name() }))?,
            }
        }
        // family_flash_supported 已在上方校验，其余族不可能到达这里
        ChipFamily::Unknown | ChipFamily::Air6201 => unreachable!("family_flash_supported 已校验芯片族"),
    }

    if tail_log_secs > 0 {
        tail_log_after_flash(soc, port, tail_log_secs, format)?;
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

#[allow(clippy::too_many_arguments)]
pub fn cmd_flash_partition(
    op: &str,
    soc: &str,
    port: &str,
    script_folders: Option<&[String]>,
    step: u8,
    format: &OutputFormat,
    reset: &ResetArgs,
    reset_config: Option<luatos_flash::sf32lb5x::Sf32ResetConfig>,
    baud: Option<u32>,
) -> anyhow::Result<()> {
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let command = format!("flash.{op}");
    install_cancel_handler(format, &command, &cancel);

    let on_progress = make_progress_callback(format, command.clone(), step);

    // Detect chip type
    let info = luatos_soc::read_soc_info(soc)?;
    let family = info.family();

    match family {
        ChipFamily::Bk72xx => match op {
            "script" => {
                let folders = script_folders.expect("script folder required");
                let refs: Vec<&str> = folders.iter().map(|s| s.as_str()).collect();
                luatos_flash::bk7258::flash_script_only(soc, &refs, port, cancel, &on_progress)?;
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
        ChipFamily::Xt804 => {
            reset.execute(port)?;
            match op {
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
            }
        }
        ChipFamily::Ccm4211 => match op {
            "script" => {
                let folders = script_folders.expect("script folder required");
                let script_data = build_script_image_checked(folders, &info)?;
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
        ChipFamily::Ec718 => match op {
            "script" => {
                let folders = script_folders.expect("script folder required");
                let script_data = build_script_image_checked(folders, &info)?;
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
        ChipFamily::Sf32lb58 => match op {
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
        ChipFamily::Unknown | ChipFamily::Air6201 => {
            anyhow::bail!("Unsupported chip type: {}", info.chip.chip_type);
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
    install_cancel_handler(format, "flash.ext-flash", &cancel);

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
    install_cancel_handler(format, "flash.ext-erase", &cancel);

    let on_progress = make_progress_callback(format, "flash.ext-erase", step);
    luatos_flash::air6201::erase_ext_partition(port, baud, partition, ext_prog, &on_progress, cancel)?;

    match format {
        OutputFormat::Text => println!("External flash erase completed."),
        OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(format, "flash.ext-erase", "ok", serde_json::json!({ "partition": partition }))?,
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct KeywordEvaluation {
    results: Vec<(String, bool)>,
    missing_keywords: Vec<String>,
    all_passed: bool,
}

fn evaluate_keyword_results(all_lines: &[String], keywords: &[String]) -> KeywordEvaluation {
    let mut results = Vec::with_capacity(keywords.len());
    let mut missing_keywords = Vec::new();
    for kw in keywords {
        let found = all_lines.iter().any(|line| line.contains(kw.as_str()));
        results.push((kw.clone(), found));
        if !found {
            missing_keywords.push(kw.clone());
        }
    }

    let all_passed = missing_keywords.is_empty();
    KeywordEvaluation {
        results,
        missing_keywords,
        all_passed,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FlashTestOutcome {
    pass: KeywordEvaluation,
    fail_keyword_results: Vec<(String, bool)>,
    matched_fail_keywords: Vec<String>,
    fast_failed: bool,
    all_passed: bool,
}

fn evaluate_keyword_hits(all_lines: &[String], keywords: &[String]) -> Vec<(String, bool)> {
    keywords.iter().map(|kw| (kw.clone(), all_lines.iter().any(|line| line.contains(kw.as_str())))).collect()
}

fn evaluate_flash_test_outcome(all_lines: &[String], pass_keywords: &[String], fail_keywords: &[String]) -> FlashTestOutcome {
    let pass = evaluate_keyword_results(all_lines, pass_keywords);
    let fail_keyword_results = evaluate_keyword_hits(all_lines, fail_keywords);
    let matched_fail_keywords = fail_keyword_results.iter().filter(|(_, found)| *found).map(|(kw, _)| kw.clone()).collect::<Vec<_>>();
    let fast_failed = !matched_fail_keywords.is_empty();
    let all_passed = pass.all_passed && !fast_failed;
    FlashTestOutcome {
        pass,
        fail_keyword_results,
        matched_fail_keywords,
        fast_failed,
        all_passed,
    }
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
    fail_keywords: &[String],
    step: u8,
    format: &OutputFormat,
) -> anyhow::Result<bool> {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use std::time::{Duration, Instant};

    let cancel = Arc::new(AtomicBool::new(false));
    install_cancel_handler(format, "flash.test", &cancel);

    let on_progress = make_progress_callback(format, "flash.test", step);

    // Step 1: Flash the firmware
    let info = luatos_soc::read_soc_info(soc)?;
    let chip = info.chip.chip_type.clone();
    let family = info.family();
    let (_, _, log_br) = cmd_log::resolve_log_mode(chip.as_str(), info.log_baud_rate());

    let boot_lines_from_flash: Vec<String> = match family {
        ChipFamily::Bk72xx => {
            let folders_refs: Option<Vec<&str>> = script_folders.map(|dirs| dirs.iter().map(|s| s.as_str()).collect());
            luatos_flash::bk7258::flash_bk7258(soc, folders_refs.as_deref(), port, baud, cancel.clone(), on_progress, true)?
        }
        ChipFamily::Xt804 => {
            let on_progress2 = make_progress_callback(format, "flash.test", step);
            luatos_flash::xt804::flash_xt804(soc, port, on_progress2, cancel.clone())?;
            Vec::new() // XT804 does not return boot lines from flash
        }
        ChipFamily::Ccm4211 => {
            let on_progress2 = make_progress_callback(format, "flash.test", step);
            let overlay = build_script_overlay(script_folders, &info)?;
            if overlay.is_some() {
                event::emit_message(format, "flash.test", MessageLevel::Info, "Applying script overlay from --script folders...")?;
            }
            luatos_flash::ccm4211::flash_ccm4211(soc, port, &on_progress2, cancel.clone(), overlay.as_deref())?;
            Vec::new()
        }
        ChipFamily::Ec718 => {
            let on_progress2 = make_progress_callback(format, "flash.test", step);
            let overlay = build_script_overlay(script_folders, &info)?;
            if overlay.is_some() {
                event::emit_message(format, "flash.test", MessageLevel::Info, "Applying script overlay from --script folders...")?;
            }
            let boot_port = luatos_flash::ec718::auto_enter_boot_mode(Some(port), &on_progress2)?;
            luatos_flash::ec718::flash_ec718(soc, &boot_port, &on_progress2, cancel.clone(), overlay.as_deref())?;
            Vec::new()
        }
        ChipFamily::Unknown | ChipFamily::Air6201 | ChipFamily::Sf32lb58 => {
            anyhow::bail!("Unsupported chip type for flash test: {}", info.chip.chip_type);
        }
    };

    if cancel.load(Ordering::Relaxed) {
        anyhow::bail!("Flash test cancelled by user");
    }

    // Step 2: Capture boot log (append to any lines from flash)
    let mut all_lines = boot_lines_from_flash;

    // Determine if this chip uses binary SOC log protocol
    let (use_binary_log, is_ec718, _) = cmd_log::resolve_log_mode(chip.as_str(), log_br);

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
        let mut early_outcome: Option<FlashTestOutcome> = None;

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
                            let outcome = evaluate_flash_test_outcome(&all_lines, keywords, fail_keywords);
                            if outcome.fast_failed || outcome.pass.all_passed {
                                early_outcome = Some(outcome);
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
                            let outcome = evaluate_flash_test_outcome(&all_lines, keywords, fail_keywords);
                            if outcome.fast_failed || outcome.pass.all_passed {
                                early_outcome = Some(outcome);
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

                        let outcome = evaluate_flash_test_outcome(&all_lines, keywords, fail_keywords);
                        if outcome.fast_failed || outcome.pass.all_passed {
                            early_outcome = Some(outcome);
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

        if early_outcome.is_some() {
            log::debug!("flash.test exited log capture early due to keyword decision");
        }
    } else if all_lines.is_empty() {
        // Could not open serial and no lines from flash
        log::warn!("Could not open serial port for boot log capture");
    }

    // Step 3: Evaluate keywords
    let outcome = evaluate_flash_test_outcome(&all_lines, keywords, fail_keywords);
    let keyword_results = outcome.pass.results;
    let missing_keywords = outcome.pass.missing_keywords;
    let fail_keyword_results = outcome.fail_keyword_results;
    let matched_fail_keywords = outcome.matched_fail_keywords;
    let fast_failed = outcome.fast_failed;
    let all_passed = outcome.all_passed;
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
            if !fail_keyword_results.is_empty() {
                for (kw, found) in &fail_keyword_results {
                    let icon = if *found { "✗" } else { "✓" };
                    println!("  [{icon}] Fail keyword \"{kw}\": {}", if *found { "HIT" } else { "NOT HIT" });
                }
            }
            if fast_failed {
                println!("  Fast-fail keywords hit: {}", matched_fail_keywords.join(", "));
            }
            if !all_passed {
                println!("  Missing keywords: {}", missing_keywords.join(", "));
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
                "fail_keywords": fail_keyword_results.iter().map(|(kw, found)| {
                    serde_json::json!({ "keyword": kw, "found": found })
                }).collect::<Vec<_>>(),
                "matched_fail_keywords": matched_fail_keywords,
                "fast_failed": fast_failed,
                "missing_keywords": missing_keywords,
                "boot_log": all_lines,
                "log_line_count": all_lines.len(),
            }),
        )?,
    }

    // 退出码由调用方决定：false 时以 FAIL（退出码 1）结束进程
    Ok(all_passed)
}

/// 刷写预编译的 script.bin（跳过 Lua 编译）
/// 适用于用 Luatools 等外部工具编译的脚本镜像
pub fn cmd_flash_script_bin(soc: &str, port: &str, bin_path: &str, on_progress: &luatos_flash::ProgressCallback) -> anyhow::Result<()> {
    use std::sync::{atomic::AtomicBool, Arc};

    let script_data = std::fs::read(bin_path).with_context(|| format!("无法读取脚本镜像: {bin_path}"))?;

    let cancel = Arc::new(AtomicBool::new(false));
    let info = luatos_soc::read_soc_info(soc)?;

    match info.family() {
        ChipFamily::Ec718 => {
            let boot_port = luatos_flash::ec718::auto_enter_boot_mode(Some(port), on_progress)?;
            luatos_flash::ec718::flash_script_ec718(soc, &boot_port, &script_data, on_progress, cancel)?;
        }
        ChipFamily::Ccm4211 => {
            luatos_flash::ccm4211::flash_script_ccm4211(soc, port, &script_data, on_progress, cancel)?;
        }
        _ => {
            anyhow::bail!(
                "--bin script flash not supported for chip '{}'. Use 'flash script' with --script folders instead.",
                info.chip.chip_type
            );
        }
    }

    Ok(())
}

pub fn print_script_result(format: &OutputFormat) -> anyhow::Result<()> {
    match format {
        OutputFormat::Text => {
            println!("Script flash completed successfully.");
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            event::emit_result(format, "flash.script", "ok", serde_json::json!({}))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_folders_present_requires_non_empty_list() {
        let script = vec!["tmp-script".to_string()];
        assert!(script_folders_present(Some(&script)));
        assert!(!script_folders_present(None));
        assert!(!script_folders_present(Some(&[])));
    }

    /// 每个已知芯片族都必须在 family_flash_supported 中有明确归属（真=可刷机，假=明确不支持）。
    #[test]
    fn family_flash_supported_covers_all_known_families() {
        assert!(family_flash_supported(ChipFamily::Bk72xx));
        assert!(family_flash_supported(ChipFamily::Xt804));
        assert!(family_flash_supported(ChipFamily::Ccm4211));
        assert!(family_flash_supported(ChipFamily::Ec718));
        assert!(family_flash_supported(ChipFamily::Sf32lb58));
        assert!(!family_flash_supported(ChipFamily::Air6201));
        assert!(!family_flash_supported(ChipFamily::Unknown));
    }

    /// family_flash_supported 与 cmd_flash_run 的 Unsupported 文案一致（Unknown 族必须被拒绝）。
    #[test]
    fn unknown_family_is_rejected_by_flash_run_support_check() {
        assert!(!family_flash_supported(ChipFamily::Unknown));
    }

    #[test]
    fn evaluate_keyword_results_marks_missing_keyword() {
        let all_lines = vec!["Boot OK".to_string(), "READY token".to_string()];
        let keywords = vec!["READY".to_string(), "LuatOS@".to_string()];

        let evaluated = evaluate_keyword_results(&all_lines, &keywords);
        assert!(!evaluated.all_passed);
        assert_eq!(evaluated.results, vec![("READY".to_string(), true), ("LuatOS@".to_string(), false),]);
        assert_eq!(evaluated.missing_keywords, vec!["LuatOS@".to_string()]);
    }

    #[test]
    fn evaluate_keyword_results_passes_when_all_found() {
        let all_lines = vec!["Boot LuatOS@ READY".to_string()];
        let keywords = vec!["READY".to_string(), "LuatOS@".to_string()];

        let evaluated = evaluate_keyword_results(&all_lines, &keywords);
        assert!(evaluated.all_passed);
        assert_eq!(evaluated.missing_keywords, Vec::<String>::new());
    }

    #[test]
    fn evaluate_flash_test_outcome_fast_fails_when_fail_keyword_matched() {
        let all_lines = vec!["panic: assert failed".to_string(), "LuatOS@ ready".to_string()];
        let pass_keywords = vec!["LuatOS@".to_string()];
        let fail_keywords = vec!["panic".to_string(), "hardfault".to_string()];

        let outcome = evaluate_flash_test_outcome(&all_lines, &pass_keywords, &fail_keywords);
        assert!(!outcome.all_passed);
        assert!(outcome.fast_failed);
        assert_eq!(outcome.matched_fail_keywords, vec!["panic".to_string()]);
    }

    #[test]
    fn evaluate_flash_test_outcome_does_not_fast_fail_when_fail_keywords_unset() {
        let all_lines = vec!["LuatOS@ ready".to_string()];
        let pass_keywords = vec!["LuatOS@".to_string()];
        let fail_keywords = Vec::<String>::new();

        let outcome = evaluate_flash_test_outcome(&all_lines, &pass_keywords, &fail_keywords);
        assert!(outcome.all_passed);
        assert!(!outcome.fast_failed);
        assert_eq!(outcome.matched_fail_keywords, Vec::<String>::new());
    }
}
