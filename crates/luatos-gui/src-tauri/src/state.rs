//! 应用全局共享状态

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

/// GUI 应用全局状态，通过 Tauri 的 State 机制在各命令间共享
pub struct AppState {
    /// 刷机取消标志
    pub flash_cancel: Arc<AtomicBool>,
    /// 日志停止标志
    pub log_stop: Arc<AtomicBool>,
    /// 当前占用串口的功能（互斥：日志和刷机不能同时使用同一串口）
    pub serial_lock: Arc<Mutex<Option<String>>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            flash_cancel: Arc::new(AtomicBool::new(false)),
            log_stop: Arc::new(AtomicBool::new(false)),
            serial_lock: Arc::new(Mutex::new(None)),
        }
    }
}
