//! SOC 文件信息命令

use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct SocInfoResult {
    pub chip_type: String,
    pub version: String,
    pub files: Vec<String>,
}

/// 获取 SOC 文件详情
#[tauri::command]
pub fn soc_info(soc_path: String) -> Result<SocInfoResult, String> {
    let info = luatos_soc::read_soc_info(&soc_path).map_err(|e| format!("打开 SOC 文件失败: {e}"))?;
    let files = luatos_soc::list_soc_files(&soc_path).unwrap_or_default();

    Ok(SocInfoResult {
        chip_type: info.chip.chip_type.clone(),
        version: info.rom.version_bsp.clone().unwrap_or_default(),
        files,
    })
}
