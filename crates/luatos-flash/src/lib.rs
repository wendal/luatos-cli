// LuatOS flash protocols.
//
// Currently supports:
//   - BK7258 (Air8101): native UART ISP protocol
//   - XT804 (Air6208/Air101): XMODEM-1K flash protocol
//   - CCM4211 (Air1601/Air1602/Air1603/Air1604): ISP + SOC framed download protocol
//   - RDA8910 (Air724UG/UIS8910DM): FDL1/FDL2 staged download protocol
//   - Air6201: external SPI flash UART protocol
//
// `raw` 不实现任何协议，它是「任意 Flash 地址读写」的抽象层，
// 由上面各芯片模块提供具体实现。

pub mod air6201;
pub mod bk7258;
pub mod ccm4211;
pub mod device;
pub mod ec718;
pub mod raw;
pub mod rda8910;
pub mod xt804;

use serde::Serialize;

/// Progress callback data for flash operations.
#[derive(Debug, Clone, Serialize)]
pub struct FlashProgress {
    pub stage: String,
    /// 正在操作的分区/区域名称，如 "ap"、"cp"、"bootloader"、"script"、"fs"、"kv"。
    pub region: Option<String>,
    pub percent: f32,
    pub message: String,
    pub done: bool,
    pub error: bool,
}

impl FlashProgress {
    pub fn info(stage: &str, pct: f32, msg: &str) -> Self {
        Self {
            stage: stage.into(),
            region: None,
            percent: pct,
            message: msg.into(),
            done: false,
            error: false,
        }
    }

    /// 设置正在操作的分区区域名称（构建器方法）。
    pub fn with_region(mut self, region: &str) -> Self {
        self.region = Some(region.into());
        self
    }

    pub fn done_ok(msg: &str) -> Self {
        Self {
            stage: "Done".into(),
            region: None,
            percent: 100.0,
            message: msg.into(),
            done: true,
            error: false,
        }
    }

    pub fn done_err(msg: &str) -> Self {
        Self {
            stage: "Error".into(),
            region: None,
            percent: 0.0,
            message: msg.into(),
            done: true,
            error: true,
        }
    }
}

/// Progress callback type — receives progress updates during flash operations.
pub type ProgressCallback = Box<dyn Fn(&FlashProgress) + Send>;

/// 校验脚本镜像按写入对齐膨胀后是否落在分区容量内。
///
/// `alignment` 是设备实际写入粒度：刷机路径传芯片要求的对齐值；离线构建只检查
/// 镜像本身长度，传 1（不膨胀）。错误信息与判定口径在两条路径间保持一致。
pub fn ensure_image_fits(image_len: usize, capacity: u64, alignment: usize) -> anyhow::Result<()> {
    use anyhow::{ensure, Context};
    ensure!(alignment > 0, "脚本写入对齐不能为 0");
    let write_len = image_len.checked_add(alignment - 1).context("脚本镜像长度溢出")? / alignment * alignment;
    ensure!(
        write_len as u64 <= capacity,
        "脚本镜像大小（{} 字节, {:.1} KB），对齐后写入 {} 字节，超过分区容量（{} 字节, {:.1} KB），超出 {} 字节（{:.1} KB）。请减少脚本文件数量或大小",
        image_len,
        image_len as f64 / 1024.0,
        write_len,
        capacity,
        capacity as f64 / 1024.0,
        write_len as u64 - capacity,
        (write_len as u64 - capacity) as f64 / 1024.0,
    );
    Ok(())
}

/// 在访问设备前校验最终脚本镜像及其对齐后的写入长度。
/// 容量必须由 SOC 明确声明，不能用默认容量推测可写范围。
pub fn validate_script_image(info: &luatos_soc::SocInfo, image_len: usize, alignment: usize) -> anyhow::Result<()> {
    use anyhow::Context;
    let capacity_kb = info
        .rom
        .fs
        .as_ref()
        .and_then(|fs| fs.script.as_ref())
        .and_then(|script| script.size)
        .context("SOC 缺少脚本分区容量 rom.fs.script.size，无法安全下载脚本")?;
    let capacity = capacity_kb.checked_mul(1024).context("脚本分区容量溢出")?;
    ensure_image_fits(image_len, capacity, alignment)
}
