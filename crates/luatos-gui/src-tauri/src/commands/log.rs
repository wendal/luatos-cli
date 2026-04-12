//! 日志查看命令 — 文本和二进制日志流

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::state::AppState;

#[derive(Debug, Clone, Serialize)]
pub struct LogLine {
    pub text: String,
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogStatus {
    pub connected: bool,
    pub message: String,
}

/// 尝试获取串口锁，成功返回 Ok，已被占用返回 Err
fn acquire_serial_lock(state: &AppState, port: &str) -> Result<(), String> {
    let mut lock = state.serial_lock.lock().map_err(|_| "串口锁异常".to_string())?;
    if let Some(ref locked_port) = *lock {
        if locked_port == port {
            return Err(format!("串口 {port} 已被占用（可能正在刷机）"));
        }
    }
    *lock = Some(port.to_string());
    Ok(())
}

/// 启动文本日志流
#[tauri::command]
pub fn log_start(app: AppHandle, state: State<'_, AppState>, port: String, baud_rate: u32) -> Result<(), String> {
    // 获取串口互斥锁
    acquire_serial_lock(&state, &port)?;

    // 重置停止标志
    state.log_stop.store(false, Ordering::Relaxed);
    let stop = Arc::clone(&state.log_stop);
    let state_inner = Arc::clone(&state.serial_lock);

    let app2 = app.clone();
    let app3 = app.clone();
    let start_time = Instant::now();
    let port_clone = port.clone();

    // 在独立线程中运行日志流（阻塞调用）
    std::thread::spawn(move || {
        let app_line = app2.clone();
        let result = luatos_serial::stream_log_lines(
            &port,
            baud_rate,
            stop,
            Box::new(move |line| {
                let elapsed = start_time.elapsed().as_millis() as u64;
                let _ = app_line.emit(
                    "log:line",
                    &LogLine {
                        text: line.to_string(),
                        timestamp_ms: elapsed,
                    },
                );
            }),
        );

        // 如果打开串口成功（进入了读取循环），先发 connected
        // stream_log_lines 返回时表示已断开
        let msg = match result {
            Ok(()) => "日志已停止".to_string(),
            Err(e) => format!("日志错误: {e}"),
        };

        // 释放串口锁
        if let Ok(mut lock) = state_inner.lock() {
            if lock.as_deref() == Some(&port) {
                *lock = None;
            }
        }

        let _ = app3.emit("log:status", &LogStatus { connected: false, message: msg });
    });

    // 串口打开成功（线程已启动），发送 connected 状态
    let _ = app.emit(
        "log:status",
        &LogStatus {
            connected: true,
            message: format!("已连接 {port_clone} @ {baud_rate}"),
        },
    );

    Ok(())
}

/// 启动二进制日志流 (SOC / EC718)
#[tauri::command]
pub fn log_start_binary(app: AppHandle, state: State<'_, AppState>, port: String, baud_rate: u32, probe: bool) -> Result<(), String> {
    // 获取串口互斥锁
    acquire_serial_lock(&state, &port)?;

    state.log_stop.store(false, Ordering::Relaxed);
    let stop = Arc::clone(&state.log_stop);
    let state_inner = Arc::clone(&state.serial_lock);

    let app2 = app.clone();
    let app3 = app.clone();
    let start_time = Instant::now();
    let port_clone = port.clone();

    std::thread::spawn(move || {
        // 检测是否为 EC718 模组
        let is_ec718 = luatos_flash::ec718::find_ec718_cmd_port().is_some();
        // EC718 USB CDC 最大支持 921600
        let baud_rate = if is_ec718 && baud_rate == 2_000_000 { 921_600 } else { baud_rate };

        let init_data = if probe { Some(luatos_flash::ec718::build_log_probe()) } else { None };

        let result = if is_ec718 {
            // EC718: 0x7E HDLC 帧
            let decoder = std::sync::Mutex::new(luatos_log::Ec718LogDecoder::new());
            let app_data = app2.clone();
            luatos_serial::stream_binary(
                &port,
                baud_rate,
                stop,
                Box::new(move |data| {
                    if let Ok(mut dec) = decoder.lock() {
                        let entries = dec.feed(data);
                        for entry in &entries {
                            let elapsed = start_time.elapsed().as_millis() as u64;
                            let module = entry.module.as_deref().unwrap_or("-");
                            let text = format!("{}/{} {}", entry.level, module, entry.message);
                            let _ = app_data.emit("log:line", &LogLine { text, timestamp_ms: elapsed });
                        }
                    }
                }),
                init_data.as_deref(),
                true,
            )
        } else {
            // 标准 SOC: 0xA5 帧
            let decoder = std::sync::Mutex::new(luatos_log::SocLogDecoder::new());
            let app_data = app2.clone();
            luatos_serial::stream_binary(
                &port,
                baud_rate,
                stop,
                Box::new(move |data| {
                    if let Ok(mut dec) = decoder.lock() {
                        let entries = dec.feed(data);
                        for entry in &entries {
                            let elapsed = start_time.elapsed().as_millis() as u64;
                            let module = entry.module.as_deref().unwrap_or("-");
                            let text = format!("{}/{} {}", entry.level, module, entry.message);
                            let _ = app_data.emit("log:line", &LogLine { text, timestamp_ms: elapsed });
                        }
                    }
                }),
                init_data.as_deref(),
                false,
            )
        };

        let msg = match result {
            Ok(()) => "日志已停止".to_string(),
            Err(e) => format!("日志错误: {e}"),
        };

        // 释放串口锁
        if let Ok(mut lock) = state_inner.lock() {
            if lock.as_deref() == Some(&port) {
                *lock = None;
            }
        }

        let _ = app3.emit("log:status", &LogStatus { connected: false, message: msg });
    });

    // 线程已启动，发送 connected 状态
    let _ = app.emit(
        "log:status",
        &LogStatus {
            connected: true,
            message: format!("已连接 {port_clone} @ {baud_rate} (二进制模式)"),
        },
    );

    Ok(())
}

/// 停止日志
#[tauri::command]
pub fn log_stop(state: State<'_, AppState>) {
    log::info!("停止日志");
    state.log_stop.store(true, Ordering::Relaxed);
    // 串口锁会在日志线程退出时自动释放
}
