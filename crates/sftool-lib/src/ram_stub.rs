use phf::phf_map;
use rust_embed::Embed;
use std::borrow::Cow;

// ─── 各芯片型号 RAM stub 内嵌结构（按 feature 条件编译，只内嵌启用型号的文件）────

#[cfg(feature = "sf32lb52")]
#[derive(Embed)]
#[folder = "stub/"]
#[include = "ram_patch_52*"]
struct Lb52Stubs;

#[cfg(feature = "sf32lb55")]
#[derive(Embed)]
#[folder = "stub/"]
#[include = "ram_patch_55*"]
#[include = "58X_sig_pub.der"]
struct Lb55Stubs;

#[cfg(feature = "sf32lb56")]
#[derive(Embed)]
#[folder = "stub/"]
#[include = "ram_patch_56*"]
#[include = "factory_cali_56X.bin"]
struct Lb56Stubs;

#[cfg(feature = "sf32lb57")]
#[derive(Embed)]
#[folder = "stub/"]
#[include = "ram_patch_57*"]
struct Lb57Stubs;

#[cfg(feature = "sf32lb58")]
#[derive(Embed)]
#[folder = "stub/"]
#[include = "ram_patch_58*"]
#[include = "factory_cali_58X.bin"]
#[include = "factory_cali.bin"]
#[include = "58X_sig_pub.der"]
struct Lb58Stubs;

pub static CHIP_FILE_NAME: phf::Map<&'static str, &'static str> = phf_map! {
    "sf32lb52_nor" => "ram_patch_52X.bin",
    "sf32lb52_nand" => "ram_patch_52X_NAND.bin",
    "sf32lb52_sd" => "ram_patch_52X_SD.bin",
    "sf32lb55_nor" => "ram_patch_55X.bin",
    "sf32lb55_sd" => "ram_patch_55X_SD.bin",
    "sf32lb56_nor" => "ram_patch_56X.bin",
    "sf32lb56_nand" => "ram_patch_56X_NAND.bin",
    "sf32lb56_sd" => "ram_patch_56X_SD.bin",
    "sf32lb58_nor" => "ram_patch_58x.bin",
    "sf32lb58_nand" => "ram_patch_58X_NAND.bin",
    "sf32lb58_sd" => "ram_patch_SD.bin",
};

// 签名公钥文件常量
pub static SIG_PUB_FILE: &str = "58X_sig_pub.der";

/// Stub 文件数据的包装结构
pub struct StubData {
    pub data: Cow<'static, [u8]>,
}

/// 从各芯片型号的内嵌 stub 中查找文件（按 feature 条件搜索）
pub(crate) fn find_embedded_stub(file_name: &str) -> Option<Cow<'static, [u8]>> {
    #[cfg(feature = "sf32lb52")]
    if let Some(f) = Lb52Stubs::get(file_name) {
        return Some(f.data);
    }
    #[cfg(feature = "sf32lb55")]
    if let Some(f) = Lb55Stubs::get(file_name) {
        return Some(f.data);
    }
    #[cfg(feature = "sf32lb56")]
    if let Some(f) = Lb56Stubs::get(file_name) {
        return Some(f.data);
    }
    #[cfg(feature = "sf32lb57")]
    if let Some(f) = Lb57Stubs::get(file_name) {
        return Some(f.data);
    }
    #[cfg(feature = "sf32lb58")]
    if let Some(f) = Lb58Stubs::get(file_name) {
        return Some(f.data);
    }
    #[allow(unreachable_code)]
    None
}

/// 当 stub 文件未找到时，给出相应 feature 未启用的提示
fn feature_hint(chip_memory_key: &str) -> &'static str {
    if chip_memory_key.starts_with("sf32lb52") {
        " (提示: 编译时未启用 sf32lb52 feature)"
    } else if chip_memory_key.starts_with("sf32lb55") {
        " (提示: 编译时未启用 sf32lb55 feature)"
    } else if chip_memory_key.starts_with("sf32lb56") {
        " (提示: 编译时未启用 sf32lb56 feature)"
    } else if chip_memory_key.starts_with("sf32lb57") {
        " (提示: 编译时未启用 sf32lb57 feature)"
    } else if chip_memory_key.starts_with("sf32lb58") {
        " (提示: 编译时未启用 sf32lb58 feature)"
    } else {
        ""
    }
}

/// 加载 stub 文件，优先使用外部文件，否则使用内嵌文件
///
/// # Arguments
/// * `external_path` - 可选的外部 stub 文件路径
/// * `chip_memory_key` - 芯片和内存类型的键，如 "sf32lb52_nor"
///
/// # Returns
/// * `Ok(StubData)` - 成功加载的 stub 数据
/// * `Err` - 加载失败
pub fn load_stub_file(external_path: Option<&str>, chip_memory_key: &str) -> Result<StubData, std::io::Error> {
    // 如果指定了外部文件路径，优先使用外部文件
    if let Some(path) = external_path {
        tracing::info!("Loading external stub file: {}", path);
        let data = std::fs::read(path).map_err(|e| {
            tracing::error!("Failed to read external stub file '{}': {}", path, e);
            std::io::Error::new(std::io::ErrorKind::NotFound, format!("Failed to read external stub file '{}': {}", path, e))
        })?;
        tracing::debug!("External stub file loaded successfully, size: {} bytes", data.len());
        return Ok(StubData { data: Cow::Owned(data) });
    }

    // 使用内嵌文件
    tracing::debug!("Looking for embedded stub file with key: {}", chip_memory_key);
    let stub_file_name = CHIP_FILE_NAME.get(chip_memory_key).ok_or_else(|| {
        tracing::error!("No stub file found for chip type: {}", chip_memory_key);
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("No stub file found for the given chip and memory type: {}", chip_memory_key),
        )
    })?;

    tracing::debug!("Loading embedded RAM stub file: {}", stub_file_name);
    let data = find_embedded_stub(stub_file_name).ok_or_else(|| {
        let hint = feature_hint(chip_memory_key);
        tracing::error!("Embedded stub file not found: {}{}", stub_file_name, hint);
        std::io::Error::new(std::io::ErrorKind::NotFound, format!("Embedded stub file not found: {}{}", stub_file_name, hint))
    })?;

    tracing::debug!("Embedded stub file loaded successfully, size: {} bytes", data.len());
    Ok(StubData { data })
}
