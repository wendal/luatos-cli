//! 向导式项目创建的数据结构与辅助函数。
//!
//! 定义 [`ModelInfo`]、[`VersionEntry`]、[`WizardConfig`]、[`TemplateKind`] 等类型，
//! 以及从 SOC 文件名推断芯片族的辅助函数。
//! 本模块不依赖外部 HTTP/manifest crate，保持纯数据层。

use std::path::PathBuf;

/// 固件版本条目（来自 resource manifest 的 core child）
#[derive(Debug, Clone)]
pub struct VersionEntry {
    /// 版本名称，如 "V2001"
    pub version_name: String,
    /// SOC 文件名，如 "LuatOS-SoC_V2001_Air8101.soc"
    pub filename: String,
    /// 文件大小（字节）
    pub size: u64,
    /// SHA256 哈希（十六进制字符串）
    pub sha256: String,
    /// CDN 相对路径（用于下载）
    pub path: String,
}

/// 单个模组型号的信息（含所有可用固件版本）
#[derive(Debug, Clone)]
pub struct ModelInfo {
    /// 型号名称，如 "Air8101"（来自 manifest 分类名）
    pub name: String,
    /// 芯片族标识，如 "bk72xx"、"ec7xx"
    pub chip: String,
    /// Lua VM 整数位宽（32 或 64）
    pub bitw: u32,
    /// 可用固件版本列表（manifest 顺序，通常最新在前）
    pub versions: Vec<VersionEntry>,
}

/// 向导完成后产生的完整配置，用于驱动脚手架生成
#[derive(Debug, Clone)]
pub struct WizardConfig {
    /// 项目名称
    pub project_name: String,
    /// 项目根目录
    pub project_dir: PathBuf,
    /// 选定的模组型号
    pub model: ModelInfo,
    /// 选定的固件版本（None = 暂不关联固件文件）
    pub selected_version: Option<VersionEntry>,
    /// 是否立即下载固件到 resource/ 目录
    pub download_firmware: bool,
    /// 项目模板类型
    pub template: TemplateKind,
    /// 串口名称（None = 暂不选择）
    pub port: Option<String>,
    /// soc_script 版本（"latest" / "disable" / 具体版本号如 "v2026.04.10.16"）
    pub soc_script: String,
    /// 是否立即下载 soc_script
    pub download_soc_script: bool,
    /// 是否初始化 Git 仓库并生成 .gitignore
    pub git_init: bool,
}

/// 项目模板类型
#[derive(Debug, Clone, PartialEq)]
pub enum TemplateKind {
    /// Hello World：`sys` + 定时器 + `log.info` 输出，标准入门结构
    HelloWorld,
    /// AirUI 界面示例：含按钮/标签/开关，仅适用于有屏幕的型号（bk72xx/air6208/air101）
    Ui,
    /// 空项目：仅创建目录结构，`lua/main.lua` 为空
    Empty,
}

impl TemplateKind {
    /// 返回模板中文显示名称
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::HelloWorld => "Hello World（基础定时器示例）",
            Self::Ui => "AirUI 界面示例（需有屏幕的型号）",
            Self::Empty => "空项目",
        }
    }

    /// 该芯片族是否支持此模板
    pub fn supported_by_chip(&self, chip: &str) -> bool {
        match self {
            Self::Ui => ui_supported(chip),
            _ => true,
        }
    }

    /// 从命令行字符串解析模板类型（用于非交互式参数）
    pub fn from_str_name(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "helloworld" | "hello" | "hello_world" => Some(Self::HelloWorld),
            "ui" | "airui" => Some(Self::Ui),
            "empty" | "blank" => Some(Self::Empty),
            _ => None,
        }
    }

    /// 返回用于命令行参数的标识符
    pub fn id(&self) -> &'static str {
        match self {
            Self::HelloWorld => "helloworld",
            Self::Ui => "ui",
            Self::Empty => "empty",
        }
    }

    /// 所有模板类型列表（用于向导中展示选项）
    pub fn all() -> Vec<Self> {
        vec![Self::HelloWorld, Self::Ui, Self::Empty]
    }
}

/// 判断指定芯片族是否支持 AirUI 屏幕示例
pub fn ui_supported(chip: &str) -> bool {
    matches!(chip, "bk72xx" | "air6208" | "air101")
}

/// 从 SOC 文件名推断芯片族标识。
///
/// 与 [`luatos_project::import`] 中的私有逻辑保持一致，对外公开以便向导和测试使用。
///
/// # 示例
///
/// ```
/// use luatos_project::wizard::chip_from_soc_filename;
/// assert_eq!(chip_from_soc_filename("LuatOS-SoC_V2001_Air8101.soc"), "bk72xx");
/// assert_eq!(chip_from_soc_filename("LuatOS-SoC_V2004_Air780EPM.soc"), "ec7xx");
/// ```
pub fn chip_from_soc_filename(filename: &str) -> &'static str {
    let lower = filename.to_lowercase();
    if lower.contains("air8101") || lower.contains("bk72") || lower.contains("bk7258") {
        "bk72xx"
    } else if lower.contains("air6208") {
        "air6208"
    } else if lower.contains("air101") && !lower.contains("air1016") {
        "air101"
    } else if lower.contains("air8000") {
        "air8000"
    } else if lower.contains("air780") || lower.contains("air600") || lower.contains("ec718") || lower.contains("ec618") || lower.contains("air1601") || lower.contains("air201") {
        "ec7xx"
    } else if lower.contains("esp32") {
        "esp32"
    } else {
        "unknown"
    }
}

/// 从型号名称推断芯片族（逻辑与 [`chip_from_soc_filename`] 相同，
/// 但接受 manifest 中的分类名称如 "Air8101" 而非完整文件名）。
pub fn chip_from_model_name(model: &str) -> &'static str {
    chip_from_soc_filename(model)
}

/// 离线降级时使用的预置常见型号列表。
///
/// 当 manifest 拉取失败且无可用缓存时，向导退回到此列表，
/// 版本列表为空（用户可手动输入 SOC 文件路径）。
pub fn fallback_models() -> Vec<ModelInfo> {
    [
        ("Air8101", "bk72xx"),
        ("Air8000", "air8000"),
        ("Air780EPM", "ec7xx"),
        ("Air780EHM", "ec7xx"),
        ("Air780E", "ec7xx"),
        ("Air1601", "ec7xx"),
        ("Air201", "ec7xx"),
        ("Air6208", "air6208"),
        ("Air101", "air101"),
    ]
    .iter()
    .map(|(name, chip)| ModelInfo {
        name: name.to_string(),
        chip: chip.to_string(),
        bitw: crate::default_bitw(chip),
        versions: Vec::new(),
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chip_detection_soc_filenames() {
        assert_eq!(chip_from_soc_filename("LuatOS-SoC_V2001_Air8101.soc"), "bk72xx");
        assert_eq!(chip_from_soc_filename("LuatOS-SoC_V2004_Air780EPM.soc"), "ec7xx");
        assert_eq!(chip_from_soc_filename("LuatOS-SoC_V1001_Air6208.soc"), "air6208");
        assert_eq!(chip_from_soc_filename("LuatOS-SoC_V2031_Air8000_101.soc"), "air8000");
        assert_eq!(chip_from_soc_filename("LuatOS-SoC_Air101.soc"), "air101");
        assert_eq!(chip_from_soc_filename("LuatOS-SoC_V1001_Air1601.soc"), "ec7xx");
        assert_eq!(chip_from_soc_filename("LuatOS-SoC_V1001_Air201.soc"), "ec7xx");
        assert_eq!(chip_from_soc_filename("LuatOS-SoC_V1001_Air780EHM.soc"), "ec7xx");
        // air1016 不应匹配 air101
        assert_ne!(chip_from_soc_filename("LuatOS-SoC_V1001_Air1016.soc"), "air101");
    }

    #[test]
    fn chip_detection_from_model_name() {
        assert_eq!(chip_from_model_name("Air8101"), "bk72xx");
        assert_eq!(chip_from_model_name("Air780EPM"), "ec7xx");
        assert_eq!(chip_from_model_name("Air8000"), "air8000");
    }

    #[test]
    fn ui_supported_chips() {
        assert!(ui_supported("bk72xx"));
        assert!(ui_supported("air6208"));
        assert!(ui_supported("air101"));
        assert!(!ui_supported("ec7xx"));
        assert!(!ui_supported("air8000"));
        assert!(!ui_supported("unknown"));
    }

    #[test]
    fn template_kind_from_str() {
        assert_eq!(TemplateKind::from_str_name("helloworld"), Some(TemplateKind::HelloWorld));
        assert_eq!(TemplateKind::from_str_name("hello"), Some(TemplateKind::HelloWorld));
        assert_eq!(TemplateKind::from_str_name("ui"), Some(TemplateKind::Ui));
        assert_eq!(TemplateKind::from_str_name("airui"), Some(TemplateKind::Ui));
        assert_eq!(TemplateKind::from_str_name("empty"), Some(TemplateKind::Empty));
        assert_eq!(TemplateKind::from_str_name("blank"), Some(TemplateKind::Empty));
        assert_eq!(TemplateKind::from_str_name("unknown"), None);
    }

    #[test]
    fn template_ui_chip_filter() {
        assert!(TemplateKind::Ui.supported_by_chip("bk72xx"));
        assert!(TemplateKind::Ui.supported_by_chip("air6208"));
        assert!(!TemplateKind::Ui.supported_by_chip("ec7xx"));
        assert!(!TemplateKind::Ui.supported_by_chip("air8000"));
        // 非 ui 模板对所有芯片开放
        assert!(TemplateKind::HelloWorld.supported_by_chip("ec7xx"));
        assert!(TemplateKind::Empty.supported_by_chip("unknown"));
    }

    #[test]
    fn fallback_models_not_empty() {
        let models = fallback_models();
        assert!(!models.is_empty());
        // 验证常见型号在列表中
        assert!(models.iter().any(|m| m.name == "Air8101"));
        assert!(models.iter().any(|m| m.name == "Air780EPM"));
        // 验证 bitw 正确设置
        let air101 = models.iter().find(|m| m.name == "Air101").unwrap();
        assert_eq!(air101.bitw, 64);
        let air8101 = models.iter().find(|m| m.name == "Air8101").unwrap();
        assert_eq!(air8101.bitw, 32);
    }
}
