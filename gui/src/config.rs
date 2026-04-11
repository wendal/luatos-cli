use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GuiConfig {
    pub projects: Vec<String>,
    pub current_project: Option<String>,
    pub last_port: Option<String>,
}

impl GuiConfig {
    pub fn load() -> Self {
        let path = Self::config_path();
        if let Ok(data) = std::fs::read_to_string(&path) {
            serde_json::from_str(&data).unwrap_or_default()
        } else {
            Self::default()
        }
    }

    pub fn save(&self) {
        let path = Self::config_path();
        if let Ok(data) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(&path, data);
        }
    }

    fn config_path() -> PathBuf {
        let mut path = std::env::current_exe()
            .unwrap_or_else(|_| PathBuf::from("."))
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        path.push("gui_config.json");
        path
    }
}
