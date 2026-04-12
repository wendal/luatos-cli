//! 设置命令 — 持久化用户偏好

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::AppHandle;
use tauri::Manager;

const SETTINGS_FILE: &str = "settings.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    /// 默认波特率
    #[serde(default = "default_baud_rate")]
    pub default_baud_rate: u32,
    /// 默认 SOC 文件路径
    #[serde(default)]
    pub default_soc_path: Option<String>,
    /// 日志最大行数
    #[serde(default = "default_log_max_lines")]
    pub log_max_lines: u32,
    /// 刷机完成后自动跳转日志
    #[serde(default = "default_true")]
    pub auto_switch_to_log: bool,
    /// 日志保存目录
    #[serde(default)]
    pub log_save_dir: Option<String>,
    /// 最近打开的项目列表
    #[serde(default)]
    pub recent_projects: Vec<RecentProject>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentProject {
    /// 项目目录路径
    pub path: String,
    /// 项目名称
    pub name: String,
    /// 目标芯片
    pub chip: String,
}

fn default_baud_rate() -> u32 {
    115200
}
fn default_log_max_lines() -> u32 {
    5000
}
fn default_true() -> bool {
    true
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            default_baud_rate: 115200,
            default_soc_path: None,
            log_max_lines: 5000,
            auto_switch_to_log: true,
            log_save_dir: None,
            recent_projects: Vec::new(),
        }
    }
}

fn settings_path(app: &AppHandle) -> PathBuf {
    let dir = app.path().app_data_dir().unwrap_or_else(|_| PathBuf::from("."));
    dir.join(SETTINGS_FILE)
}

/// 加载设置
#[tauri::command]
pub fn settings_load(app: AppHandle) -> Result<AppSettings, String> {
    let path = settings_path(&app);
    if !path.exists() {
        return Ok(AppSettings::default());
    }
    let content = std::fs::read_to_string(&path).map_err(|e| format!("读取设置失败: {e}"))?;
    serde_json::from_str(&content).map_err(|e| format!("解析设置失败: {e}"))
}

/// 保存设置
#[tauri::command]
pub fn settings_save(app: AppHandle, settings: AppSettings) -> Result<(), String> {
    let path = settings_path(&app);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建设置目录失败: {e}"))?;
    }
    let content = serde_json::to_string_pretty(&settings).map_err(|e| format!("序列化设置失败: {e}"))?;
    std::fs::write(&path, content).map_err(|e| format!("写入设置失败: {e}"))?;
    Ok(())
}
