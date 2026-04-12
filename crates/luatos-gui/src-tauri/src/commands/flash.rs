//! 刷机命令 — 支持 BK7258/XT804/CCM4211/EC718 全系列芯片

use std::sync::atomic::Ordering;
use std::sync::Arc;

use tauri::{AppHandle, Emitter, State};

use crate::state::AppState;
use luatos_flash::FlashProgress;

/// 全量刷机
#[tauri::command]
pub async fn flash_run(
    app: AppHandle,
    state: State<'_, AppState>,
    soc_path: String,
    port: String,
    baud_rate: Option<u32>,
    script_folders: Option<Vec<String>>,
) -> Result<(), String> {
    // 重置取消标志
    state.flash_cancel.store(false, Ordering::Relaxed);
    let cancel = Arc::clone(&state.flash_cancel);

    let app2 = app.clone();
    tokio::task::spawn_blocking(move || {
        let on_progress: luatos_flash::ProgressCallback = {
            let app = app2.clone();
            Box::new(move |p: &FlashProgress| {
                let _ = app.emit("flash:progress", p);
            })
        };

        // 检测芯片类型
        let info = luatos_soc::read_soc_info(&soc_path).map_err(|e| format!("读取 SOC 信息失败: {e}"))?;
        let chip = info.chip.chip_type.as_str();

        match chip {
            "bk72xx" | "air8101" => {
                let folders_refs: Option<Vec<&str>> = script_folders.as_ref().map(|dirs| dirs.iter().map(|s| s.as_str()).collect());
                luatos_flash::bk7258::flash_bk7258(&soc_path, folders_refs.as_deref(), &port, baud_rate, cancel, on_progress).map_err(|e| format!("BK7258 刷机失败: {e}"))?;
            }
            "air6208" | "air101" | "air103" | "air601" => {
                luatos_flash::xt804::flash_xt804(&soc_path, &port, on_progress, cancel).map_err(|e| format!("XT804 刷机失败: {e}"))?;
            }
            "air1601" | "ccm4211" => {
                luatos_flash::ccm4211::flash_ccm4211(&soc_path, &port, &on_progress, cancel).map_err(|e| format!("CCM4211 刷机失败: {e}"))?;
            }
            "ec7xx" | "air8000" | "air780epm" | "air780ehm" | "air780ehv" | "air780ehg" => {
                let boot_port = luatos_flash::ec718::auto_enter_boot_mode(Some(&port), &on_progress).map_err(|e| format!("EC718 进入下载模式失败: {e}"))?;
                luatos_flash::ec718::flash_ec718(&soc_path, &boot_port, &on_progress, cancel).map_err(|e| format!("EC718 刷机失败: {e}"))?;
            }
            _ => {
                return Err(format!("不支持的芯片类型: {chip}"));
            }
        }

        // 发送完成事件
        let _ = app2.emit("flash:progress", &FlashProgress::done_ok("刷机完成"));
        Ok(())
    })
    .await
    .map_err(|e| format!("刷机任务异常: {e}"))?
}

/// 取消刷机
#[tauri::command]
pub fn flash_cancel(state: State<'_, AppState>) {
    log::info!("取消刷机");
    state.flash_cancel.store(true, Ordering::Relaxed);
}
