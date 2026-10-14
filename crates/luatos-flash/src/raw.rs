// 任意 Flash 地址读写抽象层（raw flash）。
//
// 把「往任意 flash 地址刷机」的能力从具体芯片协议里抽出来：CLI 只面对
// `RawFlashDevice` 一个 trait，芯片协议的实现细节留在各自模块里。
// 以后接入 XT804 / EC718 / RDA8910 时实现一份 trait 即可，无需再写一遍 CLI。
//
// 设计要点（与 docs/flash-raw-design.md 对应）：
//   1. 底层擦除/写入原语本来就以「地址为参数」，所以 trait 很薄；
//   2. 两颗芯片的写后校验能力不对等，用 `VerifyResult` 显式表达，不假装一致；
//   3. 危险区间保护放在共享驱动 `run()` 里，不分散到各芯片实现中。
//
// 用法：
//   let dev = raw::open(chip, &port, &soc, &cancel, &on_progress)?;
//   let report = raw::run(dev, &req, &on_progress)?;
//   println!("{}", report.describe());

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{bail, ensure, Result};

use crate::{FlashProgress, ProgressCallback};

/// 芯片族标识（决定用哪一套刷机协议）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawChip {
    /// BK7258 / Air8101（原生 UART ISP）
    Bk7258,
    /// CCM4211 / Air1601 / Air1602（ISP + SOC 帧协议）
    Ccm4211,
}

impl RawChip {
    pub fn name(self) -> &'static str {
        match self {
            RawChip::Bk7258 => "bk7258",
            RawChip::Ccm4211 => "ccm4211",
        }
    }

    /// 该协议是否支持写后回读校验。
    pub fn supports_digest_verify(self) -> bool {
        matches!(self, RawChip::Ccm4211)
    }

    /// 协议固定的写入对齐要求（字节）。`0` 表示无需对齐。
    ///
    /// 这是协议层面的常量，不依赖设备实例，因此可以在连接硬件之前预检。
    pub fn write_alignment(self) -> u64 {
        match self {
            // write_sector_4k 固定按 4K 扇区写，地址必须对齐
            RawChip::Bk7258 => BK_SECTOR_SIZE as u64,
            // SET_CODE_DATA_START 携带任意起始地址
            RawChip::Ccm4211 => 0,
        }
    }

    /// 协议固定的擦除粒度（字节）。`None` 表示运行时才确定。
    ///
    /// CCM4211 的 `FLASH_ERASE_BLOCK` 是分区级命令，粒度要问设备
    /// （`GET_DOWNLOAD_INFO` 返回的 block_len），所以这里返回 `None`。
    pub fn erase_granularity(self) -> Option<u64> {
        match self {
            RawChip::Bk7258 => Some(BK_SECTOR_SIZE as u64),
            RawChip::Ccm4211 => None,
        }
    }
}

/// BK7258 的扇区大小，CLI 提示文案与对齐预检共用。
const BK_SECTOR_SIZE: usize = 0x1000;

/// 写后校验结果。
///
/// 刻意区分三种能力，避免把「拿到一个数」误当成「数据写对了」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyResult {
    /// 设备回读该区间并计算摘要，与主机端一致 —— 数据确实写对了。
    DigestMatched { digest: String },
    /// 设备只能给出自己算的 CRC32，主机端无法预测期望值 —— 仅作为记录，不能判定对错。
    DeviceCrcOnly { crc32: u32 },
}

impl VerifyResult {
    /// 文本输出用的一句话结论。
    pub fn summary(&self) -> String {
        match self {
            VerifyResult::DigestMatched { digest } => format!("回读摘要一致: {digest}"),
            VerifyResult::DeviceCrcOnly { crc32 } => {
                format!("设备端 CRC32=0x{crc32:08x}（该协议无法回读数据，此值仅供参考，不构成校验）")
            }
        }
    }

    /// 是否为「真正校验通过」。`DeviceCrcOnly` 不算通过。
    pub fn is_verified(&self) -> bool {
        matches!(self, VerifyResult::DigestMatched { .. })
    }
}

/// 受保护区间：默认禁止写入，需显式 `--force`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtectedRange {
    pub start: u32,
    pub len: u64,
    /// 人类可读名称，用于报错信息。
    pub name: &'static str,
}

impl ProtectedRange {
    pub fn end(&self) -> u64 {
        self.start as u64 + self.len
    }

    /// 两个闭区间是否相交（用半开区间 [start, end) 判定）。
    pub fn overlaps(&self, start: u64, end: u64) -> bool {
        start < self.end() && end > self.start as u64
    }
}

/// 一次 raw 写请求。
#[derive(Debug, Clone)]
pub struct RawRequest {
    /// 目标 Flash 起始地址。
    pub addr: u32,
    /// 待写入的数据。
    pub data: Vec<u8>,
    /// 写入前先擦除目标范围。
    ///
    /// NOR Flash 不能把 0 写回 1，所以绝大多数情况都必须开。
    pub erase: bool,
    /// 写完做回读校验。
    pub verify: bool,
    /// 写完复位。不复位的话模组会停在 ROM 下载模式（Air8101 表现为黑屏）。
    pub reboot: bool,
    /// 已知 flash 容量（字节）。`0` 表示未知，跳过越界校验。
    pub flash_size: u32,
    /// 需要 `--force` 才能写入的区间。
    pub protected: Vec<ProtectedRange>,
    /// 允许写入受保护区间（可能导致变砖）。
    pub force: bool,
}

impl RawRequest {
    pub fn new(addr: u32, data: Vec<u8>) -> Self {
        Self {
            addr,
            data,
            erase: false,
            verify: false,
            reboot: true,
            flash_size: 0,
            protected: Vec::new(),
            force: false,
        }
    }

    pub fn len(&self) -> u64 {
        self.data.len() as u64
    }

    /// 待写入数据是否为空。
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn end(&self) -> u64 {
        self.addr as u64 + self.len()
    }
}

/// raw 写操作的结果汇总，用于 CLI 的 text / json 输出。
#[derive(Debug, Clone)]
pub struct RawReport {
    pub chip: &'static str,
    pub addr: u32,
    pub len: u64,
    pub erased: bool,
    pub verify: Option<VerifyResult>,
}

impl RawReport {
    pub fn summary(&self) -> String {
        let mut s = format!(
            "{} raw 写入完成: 0x{:08x} .. 0x{:08x} ({} 字节)，已擦除: {}",
            self.chip,
            self.addr,
            self.addr as u64 + self.len,
            self.len,
            if self.erased { "是" } else { "否" }
        );
        if let Some(v) = &self.verify {
            s.push_str(&format!("\n校验: {}", v.summary()));
        }
        s
    }
}

/// 任意地址 Flash 设备。
///
/// 由各芯片协议模块实现；`open()` 按芯片族返回具体实现。
pub trait RawFlashDevice {
    fn chip(&self) -> RawChip;

    /// 协议要求的最小擦除粒度（字节）。调用方据此对齐地址。
    fn erase_granularity(&self) -> u64;

    /// 协议要求的写入对齐（字节）。`0` 表示无需对齐。
    fn write_alignment(&self) -> u64;

    /// 会话持有的进度回调。共享驱动用它发自己的阶段消息。
    fn progress(&self) -> &ProgressCallback;

    /// 写入数据。是否已擦除由调用方通过 `erase` 控制。
    fn write(&mut self, addr: u32, data: &[u8]) -> Result<()>;

    /// 擦除 `[addr, addr+len)`。
    ///
    /// 注意：部分协议（如 CCM4211 的 `FLASH_ERASE_BLOCK`）实际擦除粒度由固件
    /// 决定，可能超出请求范围，调用方应在文档中提示用户。
    fn erase(&mut self, addr: u32, len: u64) -> Result<()>;

    /// 写后校验。返回设备能提供的最强校验结果。
    fn verify(&mut self, addr: u32, data: &[u8]) -> Result<VerifyResult>;

    /// 复位设备并释放串口（对齐 LuaTools `_reboot_and_close`）。
    fn reset(&mut self);
}

/// 按芯片族打开 raw 设备会话（已完成进 bootloader / 加载 ramrun / 去保护）。
///
/// `on_progress` 与 `cancel` 的所有权移交给会话：前者用于发出写入进度，
/// 后者让 Ctrl+C 能在写入长循环里生效。
pub fn open(chip: RawChip, port: &str, soc: &str, cancel: Arc<AtomicBool>, on_progress: ProgressCallback) -> Result<Box<dyn RawFlashDevice>> {
    match chip {
        RawChip::Bk7258 => {
            let info = luatos_soc::read_soc_info(soc)?;
            let br = info.flash_baud_rate();
            crate::bk7258::open_raw(port, br, cancel, on_progress)
        }
        RawChip::Ccm4211 => crate::ccm4211::open_raw(soc, port, cancel, on_progress),
    }
}

/// 共享驱动：校验 → 擦除 → 写入 → 校验 → 复位。
///
/// 护栏（对齐检查、危险区间、越界检查）集中在这里，芯片实现只管发协议帧。
pub fn run(mut dev: Box<dyn RawFlashDevice>, req: &RawRequest) -> Result<RawReport> {
    let chip = dev.chip();
    check_request(req)?;
    check_alignment(dev.as_ref(), req)?;

    let erased = if req.erase {
        (dev.progress())(&FlashProgress::info("RawErase", 20.0, &format!("擦除 0x{:08x} .. 0x{:08x}", req.addr, req.end())).with_region("raw"));
        dev.erase(req.addr, req.len())?;
        true
    } else {
        false
    };

    (dev.progress())(&FlashProgress::info("RawWrite", 35.0, &format!("写入 0x{:08x}，{} 字节", req.addr, req.len())).with_region("raw"));
    dev.write(req.addr, &req.data)?;

    let verify = if req.verify {
        (dev.progress())(&FlashProgress::info("RawVerify", 90.0, "回读校验中…").with_region("raw"));
        Some(dev.verify(req.addr, &req.data)?)
    } else {
        None
    };

    if req.reboot {
        (dev.progress())(&FlashProgress::info("RawReset", 97.0, "复位设备…").with_region("raw"));
        dev.reset();
    } else {
        (dev.progress())(&FlashProgress::info("RawReset", 97.0, "已跳过复位，设备将停留在下载模式").with_region("raw"));
    }

    let report = RawReport {
        chip: chip.name(),
        addr: req.addr,
        len: req.len(),
        erased,
        verify,
    };
    (dev.progress())(&FlashProgress::done_ok(&report.summary()));
    Ok(report)
}

/// 写入前的静态校验（不碰硬件）。
pub fn check_request(req: &RawRequest) -> Result<()> {
    ensure!(!req.data.is_empty(), "--file 内容为空，无需刷写");

    if req.flash_size > 0 && req.end() > req.flash_size as u64 {
        bail!(
            "写入范围 0x{:08x}..0x{:08x} 超出 flash 容量 0x{:08x}；如容量判断有误请用 --flash-size 显式指定",
            req.addr,
            req.end(),
            req.flash_size
        );
    }

    if !req.force {
        for p in &req.protected {
            if p.overlaps(req.addr as u64, req.end()) {
                bail!(
                    "写入范围 0x{:08x}..0x{:08x} 与受保护分区 '{}' (0x{:08x}..0x{:08x}) 重叠。\n\
                     如确需写入请加 --force —— 写坏 bootloader/core 会导致模组变砖，且无法用本 CLI 恢复。",
                    req.addr,
                    req.end(),
                    p.name,
                    p.start,
                    p.end()
                );
            }
        }
    }
    Ok(())
}

/// 对齐检查：协议要求写入地址按扇区/块对齐。
///
/// 权威版本：擦除粒度以设备实例为准（CCM4211 需向设备查询 block_len）。
pub fn check_alignment(dev: &dyn RawFlashDevice, req: &RawRequest) -> Result<()> {
    check_write_alignment(dev.chip().name(), req.addr, dev.write_alignment())?;
    if req.erase {
        check_erase_alignment(dev.chip().name(), req.addr, dev.erase_granularity())?;
    }
    Ok(())
}

/// 连接硬件**之前**的对齐预检。
///
/// 写入对齐是协议常量，可以提前拦住，避免用户白等一次串口连接；
/// 擦除粒度只在芯片已知时预检，CCM4211 留到连上设备后再向设备查询。
pub fn precheck_alignment(chip: RawChip, addr: u32, erase: bool) -> Result<()> {
    check_write_alignment(chip.name(), addr, chip.write_alignment())?;
    if erase {
        if let Some(gran) = chip.erase_granularity() {
            check_erase_alignment(chip.name(), addr, gran)?;
        }
    }
    Ok(())
}

fn check_write_alignment(chip: &str, addr: u32, align: u64) -> Result<()> {
    if align > 1 && !(addr as u64).is_multiple_of(align) {
        bail!(
            "{chip} 写入地址必须按 {align} 字节（0x{align:x}）对齐，当前 0x{addr:08x}。\n\
             非对齐写入会连带覆盖所在扇区内的其它数据。"
        );
    }
    Ok(())
}

fn check_erase_alignment(chip: &str, addr: u32, gran: u64) -> Result<()> {
    if gran > 1 && !(addr as u64).is_multiple_of(gran) {
        bail!("{chip} 擦除地址必须按 {gran} 字节（0x{gran:x}）对齐，当前 0x{addr:08x}");
    }
    Ok(())
}

/// 解析地址参数，支持 `0x` 前缀十六进制与十进制。
pub fn parse_addr(s: &str) -> Result<u32> {
    let t = s.trim();
    let v = if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).map_err(|_| anyhow::anyhow!("地址格式错误: {s}（期望 0x 开头的十六进制）"))?
    } else {
        t.parse::<u64>().map_err(|_| anyhow::anyhow!("地址格式错误: {s}（期望十进制或 0x 开头的十六进制）"))?
    };
    u32::try_from(v).map_err(|_| anyhow::anyhow!("地址超出 32 位 Flash 地址空间: {s}"))
}

/// 解析长度参数（字节）。
pub fn parse_len(s: &str) -> Result<u64> {
    let t = s.trim();
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return u64::from_str_radix(hex, 16).map_err(|_| anyhow::anyhow!("长度格式错误: {s}"));
    }
    t.parse::<u64>().map_err(|_| anyhow::anyhow!("长度格式错误: {s}"))
}

/// 取消检查的统一入口，芯片实现在长循环里调用。
pub fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        bail!("操作已被用户取消");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(addr: u32, len: usize) -> RawRequest {
        RawRequest::new(addr, vec![0xAB; len])
    }

    #[test]
    fn 正常请求通过校验() {
        let r = req(0x200000, 0x1000);
        check_request(&r).expect("应当通过");
    }

    #[test]
    fn 空数据被拒绝() {
        let r = RawRequest::new(0x200000, Vec::new());
        assert!(check_request(&r).is_err());
    }

    #[test]
    fn 超出容量被拒绝() {
        let mut r = req(0x3F_F000, 0x2000);
        r.flash_size = 0x40_0000;
        let err = check_request(&r).unwrap_err().to_string();
        assert!(err.contains("超出 flash 容量"), "实际错误: {err}");
    }

    #[test]
    fn 受保护区间默认拒绝() {
        let mut r = req(0, 0x1000);
        r.protected.push(ProtectedRange {
            start: 0,
            len: 0x20_0000,
            name: "bootloader+core",
        });
        let err = check_request(&r).unwrap_err().to_string();
        assert!(err.contains("受保护分区"), "实际错误: {err}");
        assert!(err.contains("--force"), "错误信息应提示 --force");
    }

    #[test]
    fn force_可越过受保护区间() {
        let mut r = req(0, 0x1000);
        r.protected.push(ProtectedRange {
            start: 0,
            len: 0x20_0000,
            name: "bootloader+core",
        });
        r.force = true;
        check_request(&r).expect("--force 后应当通过");
    }

    #[test]
    fn 受保护区间边界不误伤() {
        // 写入正好结束在保护区间起点之前 —— 不相交，应放行
        let mut r = req(0x1F_F000, 0x1000);
        r.protected.push(ProtectedRange {
            start: 0x20_0000,
            len: 0x10,
            name: "core",
        });
        check_request(&r).expect("边界不相交应放行");
    }

    #[test]
    fn 保护区间完全包含写入范围也拒绝() {
        let mut r = req(0x20_0000, 0x100);
        r.protected.push(ProtectedRange {
            start: 0,
            len: 0x40_0000,
            name: "low",
        });
        assert!(check_request(&r).is_err());
    }

    #[test]
    fn 区间相交判定() {
        let p = ProtectedRange {
            start: 0x1000,
            len: 0x1000,
            name: "x",
        };
        assert!(p.overlaps(0x0, 0x2000));
        assert!(p.overlaps(0x1800, 0x2000));
        assert!(p.overlaps(0x800, 0x1001));
        assert!(!p.overlaps(0x0, 0x1000));
        assert!(!p.overlaps(0x2000, 0x3000));
    }

    struct FakeDev {
        align: u64,
        on_progress: ProgressCallback,
    }

    impl RawFlashDevice for FakeDev {
        fn chip(&self) -> RawChip {
            RawChip::Bk7258
        }
        fn erase_granularity(&self) -> u64 {
            0x1000
        }
        fn write_alignment(&self) -> u64 {
            self.align
        }
        fn progress(&self) -> &ProgressCallback {
            &self.on_progress
        }
        fn write(&mut self, _addr: u32, _data: &[u8]) -> Result<()> {
            Ok(())
        }
        fn erase(&mut self, _addr: u32, _len: u64) -> Result<()> {
            Ok(())
        }
        fn verify(&mut self, _addr: u32, _data: &[u8]) -> Result<VerifyResult> {
            Ok(VerifyResult::DigestMatched { digest: "d41d8cd9".into() })
        }
        fn reset(&mut self) {}
    }

    fn fake(align: u64) -> FakeDev {
        FakeDev {
            align,
            on_progress: Box::new(|_| {}),
        }
    }

    #[test]
    fn 非对齐地址被拒绝() {
        let dev = fake(0x1000);
        let r = req(0x200001, 0x1000);
        let err = check_alignment(&dev, &r).unwrap_err().to_string();
        assert!(err.contains("对齐"), "实际错误: {err}");
    }

    #[test]
    fn 对齐地址通过() {
        let dev = fake(0x1000);
        let r = req(0x200000, 0x1000);
        check_alignment(&dev, &r).expect("应当通过");
    }

    #[test]
    fn 零对齐视为无要求() {
        let dev = fake(0);
        let r = req(0x200001, 7);
        check_alignment(&dev, &r).expect("align=0 表示无对齐要求");
    }

    #[test]
    fn 擦除地址未对齐被拒绝() {
        let dev = fake(0x1000);
        let mut r = req(0x200800, 0x1000);
        r.erase = true;
        assert!(check_alignment(&dev, &r).is_err());
    }

    #[test]
    fn bk7258_预检拒绝非对齐写入() {
        // 写入对齐是协议常量，应在开串口之前就能拦住
        let err = precheck_alignment(RawChip::Bk7258, 0x200001, false).unwrap_err().to_string();
        assert!(err.contains("对齐"), "实际错误: {err}");
    }

    #[test]
    fn bk7258_预检接受对齐地址() {
        precheck_alignment(RawChip::Bk7258, 0x200000, true).expect("0x200000 应通过");
    }

    #[test]
    fn ccm4211_预检不要求对齐() {
        precheck_alignment(RawChip::Ccm4211, 0x14000123, true).expect("CCM4211 写/擦均无需对齐");
    }

    #[test]
    fn 芯片常量与实现一致() {
        assert_eq!(RawChip::Bk7258.write_alignment(), 0x1000);
        assert_eq!(RawChip::Ccm4211.write_alignment(), 0);
        assert_eq!(RawChip::Bk7258.erase_granularity(), Some(0x1000));
        assert_eq!(RawChip::Ccm4211.erase_granularity(), None, "CCM4211 擦除粒度需向设备查询");
        assert!(RawChip::Ccm4211.supports_digest_verify());
        assert!(!RawChip::Bk7258.supports_digest_verify());
    }

    #[test]
    fn 解析十六进制与十进制地址() {
        assert_eq!(parse_addr("0x200000").unwrap(), 0x200000);
        assert_eq!(parse_addr("0X200000").unwrap(), 0x200000);
        assert_eq!(parse_addr("2097152").unwrap(), 0x200000);
        assert!(parse_addr("0xZZZ").is_err());
        assert!(parse_addr("0x1_0000_0000").is_err(), "应拒绝超出 32 位的地址");
    }

    #[test]
    fn crc_only_不算校验通过() {
        let v = VerifyResult::DeviceCrcOnly { crc32: 0xDEAD_BEEF };
        assert!(!v.is_verified());
        assert!(v.summary().contains("不构成校验"));

        let d = VerifyResult::DigestMatched { digest: "abc".into() };
        assert!(d.is_verified());
    }

    #[test]
    fn 报告摘要包含关键信息() {
        let rep = RawReport {
            chip: "bk7258",
            addr: 0x200000,
            len: 0x1000,
            erased: true,
            verify: None,
        };
        let s = rep.summary();
        assert!(s.contains("0x00200000"));
        assert!(s.contains("4096"));
        assert!(s.contains("已擦除: 是"));
    }
}
