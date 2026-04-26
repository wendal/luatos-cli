// SF32LB58 刷机模块 — 基于 sftool-lib 原生 Rust 实现（Apache-2.0）。
//
// ROM BL 进入方式（手动操作）：
//   1. 短接 MODE 引脚（3-pin 排针）
//   2. 按下 RESET 按键后松开
//   3. 拔掉 MODE 短接帽
//
// ROM BL 进入方式（CH340X 增强 DTR 自动，需改装硬件）：
//   使用 --auto-reset 参数，通过 DTR/RTS 自动控制 BOOT0 和 RESET。
//
// 详细协议说明见 docs/sf32lb58-flash-protocol.md。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use sftool_lib::{
    create_sifli_tool,
    progress::{ProgressEvent, ProgressOperation, ProgressSink, ProgressStatus, ProgressType},
    BeforeOperation, ChipType, SifliToolBase, WriteFlashFile, WriteFlashParams,
};

use crate::{FlashProgress, ProgressCallback};

// ─── 进度适配器 ──────────────────────────────────────────────────────────────

/// 将 sftool-lib 的结构化进度事件适配到 LuatOS 的 FlashProgress 回调。
struct SifliProgressSink {
    callback: Mutex<ProgressCallback>,
    total_bytes: AtomicU64,
    written_bytes: AtomicU64,
    current_addr: Mutex<u32>,
    /// 当前正在写入的分区名称（地址→区域名映射，在写入开始时更新）。
    current_region: Mutex<Option<String>>,
    address_regions: Vec<(u32, String)>,
}

impl SifliProgressSink {
    fn new(callback: ProgressCallback, address_regions: Vec<(u32, String)>) -> Self {
        Self {
            callback: Mutex::new(callback),
            total_bytes: AtomicU64::new(0),
            written_bytes: AtomicU64::new(0),
            current_addr: Mutex::new(0),
            current_region: Mutex::new(None),
            address_regions,
        }
    }

    fn emit(&self, stage: &str, pct: f32, msg: &str) {
        if let Ok(cb) = self.callback.lock() {
            cb(&FlashProgress::info(stage, pct, msg));
        }
    }

    fn emit_done(&self, msg: &str) {
        if let Ok(cb) = self.callback.lock() {
            cb(&FlashProgress::done_ok(msg));
        }
    }
}

impl ProgressSink for SifliProgressSink {
    fn on_event(&self, event: ProgressEvent) {
        match event {
            ProgressEvent::Start { ctx, .. } => match &ctx.operation {
                ProgressOperation::Connect => {
                    self.emit("connect", 0.0, "正在连接 ROM BL...");
                }
                ProgressOperation::DownloadStub { .. } => {
                    self.emit("stub", 5.0, "正在下载 RAM stub...");
                }
                ProgressOperation::CheckRedownload { address, .. } => {
                    self.emit("check", 8.0, &format!("检查 0x{address:08X}..."));
                }
                ProgressOperation::WriteFlash { address, size } => {
                    let total = if let ProgressType::Bar { total } = &ctx.progress_type { *total } else { *size };
                    self.total_bytes.store(total, Ordering::Relaxed);
                    self.written_bytes.store(0, Ordering::Relaxed);
                    if let Ok(mut a) = self.current_addr.lock() {
                        *a = *address;
                    }
                    // 根据地址查找区域名称
                    let region = self.address_regions.iter().find(|(a, _)| *a == *address).map(|(_, name)| name.clone());
                    if let Ok(mut r) = self.current_region.lock() {
                        *r = region.clone();
                    }
                    let progress = FlashProgress::info("flash", 10.0, &format!("写入 0x{:08X} ({} KB)...", address, size / 1024));
                    let progress = if let Some(r) = region { progress.with_region(&r) } else { progress };
                    if let Ok(cb) = self.callback.lock() {
                        cb(&progress);
                    }
                }
                ProgressOperation::EraseFlash { address, .. } => {
                    self.emit("erase", 2.0, &format!("擦除 0x{address:08X}..."));
                }
                ProgressOperation::Verify { address, .. } => {
                    self.emit("verify", 95.0, &format!("校验 0x{address:08X}..."));
                }
                _ => {}
            },
            ProgressEvent::Advance { delta, .. } => {
                let written = self.written_bytes.fetch_add(delta, Ordering::Relaxed) + delta;
                let total = self.total_bytes.load(Ordering::Relaxed);
                let addr = self.current_addr.lock().map(|a| *a).unwrap_or(0);
                let region = self.current_region.lock().ok().and_then(|r| r.clone());
                if total > 0 {
                    let pct = 10.0 + (written as f32 / total as f32 * 85.0).min(85.0);
                    let progress = FlashProgress::info("flash", pct, &format!("写入 0x{:08X}: {} / {} KB", addr, written / 1024, total / 1024));
                    let progress = if let Some(r) = region { progress.with_region(&r) } else { progress };
                    if let Ok(cb) = self.callback.lock() {
                        cb(&progress);
                    }
                }
            }
            ProgressEvent::Finish { status, .. } => match status {
                ProgressStatus::Success => {
                    let addr = self.current_addr.lock().map(|a| *a).unwrap_or(0);
                    let region = self.current_region.lock().ok().and_then(|r| r.clone());
                    let progress = FlashProgress::info("flash", 99.0, &format!("写入完成 0x{addr:08X}"));
                    let progress = if let Some(r) = region { progress.with_region(&r) } else { progress };
                    if let Ok(cb) = self.callback.lock() {
                        cb(&progress);
                    }
                }
                ProgressStatus::Skipped => {
                    let addr = self.current_addr.lock().map(|a| *a).unwrap_or(0);
                    let region = self.current_region.lock().ok().and_then(|r| r.clone());
                    let progress = FlashProgress::info("flash", 99.0, &format!("跳过 0x{addr:08X}（内容相同）"));
                    let progress = if let Some(r) = region { progress.with_region(&r) } else { progress };
                    if let Ok(cb) = self.callback.lock() {
                        cb(&progress);
                    }
                }
                ProgressStatus::Failed(msg) => {
                    if let Ok(cb) = self.callback.lock() {
                        cb(&FlashProgress::done_err(&msg));
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
}

// ─── CH340X 增强 DTR 自动复位 ────────────────────────────────────────────────

/// SF32LB58 自动复位配置（用于 CH340X 增强 DTR 改装硬件）。
///
/// 允许自定义 DTR/RTS 电平方向和各阶段等待时长，方便调试不同硬件接法。
///
/// 实测有效默认值：dtr_boot=false（DTR LOW → BOOT0 拉高），rts_reset=true（RTS HIGH → CH340X RTS#=LOW → RESET）
#[derive(Debug, Clone)]
pub struct Sf32ResetConfig {
    /// 进入 boot 模式时 DTR 的电平（false=LOW → BOOT0 拉高；true=HIGH → BOOT0 拉低）
    /// 默认 false：实测 CH340X 改装后 DTR 低电平时 BOOT0 有效（具体取决于外部电路接法）
    pub dtr_boot: bool,
    /// 触发复位时 RTS 的电平（true=HIGH → CH340X RTS# 拉低 → RESET 有效）
    /// 默认 true：CH340X RTS# 为倒相输出，软件 HIGH → 引脚 LOW → RESET
    pub rts_reset: bool,
    /// 复位脉冲宽度（毫秒，默认 100ms）
    pub reset_ms: u64,
    /// 进入 boot 后等待 ROM BL 初始化的时长（毫秒，默认 500ms）
    pub boot_wait_ms: u64,
    /// 退出 boot 后等待 MCU 稳定的时长（毫秒，默认 200ms）
    pub exit_wait_ms: u64,
}

impl Default for Sf32ResetConfig {
    fn default() -> Self {
        Self {
            dtr_boot: false, // 实测：DTR LOW 时 BOOT0 有效
            rts_reset: true,
            reset_ms: 100,
            boot_wait_ms: 500,
            exit_wait_ms: 200,
        }
    }
}

/// 通过 CH340X 增强 DTR 模式将 SF32 进入 ROM BL 刷机状态。
///
/// 时序（默认配置）：
///   DTR=LOW（BOOT0↑）→ RTS=HIGH（RTS#↓ = RESET 有效）→ reset_ms
///   → RTS=LOW（RTS#↑ = RESET 释放）→ boot_wait_ms（等待 ROM BL 初始化）
///
/// 函数返回后串口已释放，供 sftool-lib 接管。
pub fn enter_boot_mode_dtr(port_name: &str, cfg: &Sf32ResetConfig) -> Result<()> {
    let mut port = serialport::new(port_name, 115200)
        .timeout(Duration::from_millis(200))
        .open()
        .with_context(|| format!("打开串口 {port_name} 失败（进入 ROM BL 模式）"))?;
    port.write_data_terminal_ready(cfg.dtr_boot).context("设置 DTR 失败")?;
    port.write_request_to_send(cfg.rts_reset).context("设置 RTS 失败")?;
    std::thread::sleep(Duration::from_millis(cfg.reset_ms));
    port.write_request_to_send(!cfg.rts_reset).context("释放 RTS 失败")?;
    std::thread::sleep(Duration::from_millis(cfg.boot_wait_ms));
    // port drop → 串口释放
    Ok(())
}

/// 通过 CH340X 增强 DTR 模式将 SF32 恢复正常运行。
///
/// 失败时静默忽略（刷机已完成，复位失败不影响结果）。
fn exit_boot_mode_dtr(port_name: &str, cfg: &Sf32ResetConfig) {
    if let Ok(mut port) = serialport::new(port_name, 115200).timeout(Duration::from_millis(200)).open() {
        let _ = port.write_data_terminal_ready(!cfg.dtr_boot); // BOOT0 恢复
        let _ = port.write_request_to_send(cfg.rts_reset); // 触发复位
        std::thread::sleep(Duration::from_millis(cfg.reset_ms));
        let _ = port.write_request_to_send(!cfg.rts_reset); // 释放复位
        std::thread::sleep(Duration::from_millis(cfg.exit_wait_ms));
    }
}

/// 打开文件、计算 CRC32，文件指针保持在开头以供 sftool-lib 读取。
fn make_flash_file(path: &Path, address: u32) -> Result<WriteFlashFile> {
    let mut file = File::open(path).with_context(|| format!("打开刷机文件失败: {}", path.display()))?;
    let mut data = Vec::new();
    file.read_to_end(&mut data)?;
    let crc32 = crc32fast::hash(&data);
    file.seek(SeekFrom::Start(0))?;
    Ok(WriteFlashFile { address, file, crc32 })
}

/// 构建 SF32LB58 的 SifliToolBase（NAND 模式，手动进入 ROM BL）。
fn make_sifli_base(port: &str, progress_sink: Arc<dyn ProgressSink>) -> SifliToolBase {
    SifliToolBase::new_with_progress(
        port.to_string(),
        BeforeOperation::NoReset, // 用户已手动进入 ROM BL 模式
        "nand".to_string(),
        1_000_000,
        3,
        false,
        progress_sink,
    )
}

// ─── 公共 API ────────────────────────────────────────────────────────────────

/// SF32LB58 全量刷机：bootloader（NOR）+ ftab（NOR）+ app（NAND）+ script（NAND）。
///
/// 刷机前需进入 ROM BL 模式：
///   - 手动操作：短接 MODE 引脚 + 按 RESET 键（reset_config = None）
///   - 自动操作：传入 `Some(&config)`，通过 CH340X 增强 DTR 自动控制
///
/// `baud` 为 stub 加载后的协商波特率（None 保持默认 1M）。CH342K 支持最高 3M。
pub fn flash_sf32lb5x(
    soc: &str,
    port: &str,
    script_dirs: Option<&[&str]>,
    on_progress: ProgressCallback,
    _cancel: Arc<AtomicBool>,
    reset_config: Option<&Sf32ResetConfig>,
    baud: Option<u32>,
) -> Result<()> {
    // 先读取 SOC 地址信息，用于构建地址→区域名映射
    let soc_info = luatos_soc::read_soc_info(soc).context("读取 SOC 信息失败")?;
    let address_regions: Vec<(u32, String)> = {
        let mut v = Vec::new();
        if let Some(a) = soc_info.bl_addr() {
            v.push((a, "bootloader".to_string()));
        }
        if let Some(a) = soc_info.ftab_addr() {
            v.push((a, "ftab".to_string()));
        }
        if let Some(a) = soc_info.app_addr() {
            v.push((a, "app".to_string()));
        }
        v.push((soc_info.script_addr(), "script".to_string()));
        v
    };

    let sink_impl = Arc::new(SifliProgressSink::new(on_progress, address_regions));
    let sink: Arc<dyn ProgressSink> = sink_impl.clone();

    sink_impl.emit("unpack", 0.0, "解压 SOC 文件...");
    let tempdir = tempfile::tempdir().context("创建临时目录失败")?;
    let soc_dir = tempdir.path();
    let unpacked = luatos_soc::unpack_soc(soc, soc_dir).context("解压 SOC 失败")?;
    let info = &unpacked.info;

    // 获取刷机地址
    let bl_addr = info.bl_addr().context("SOC info.json 缺少 bl_addr（bootloader 地址），请用最新 pack_soc.py 重新打包")?;
    let ftab_addr = info.ftab_addr().context("SOC info.json 缺少 ftab_addr（分区表地址），请用最新 pack_soc.py 重新打包")?;
    let app_addr = info.app_addr().context("SOC info.json 缺少 app_addr")?;
    let script_addr = info.script_addr();

    // 从 rom.files 查找各 bin 文件路径（回退到默认路径）
    let bl_rel = info.extra_file("bootloader").unwrap_or("bootloader/bootloader.bin");
    let ftab_rel = info.extra_file("ftab").unwrap_or("ftab/ftab.bin");
    let bl_path = soc_dir.join(bl_rel);
    let ftab_path = soc_dir.join(ftab_rel);
    let app_path = soc_dir.join(&info.rom.file);

    // 如果提供了脚本目录，重新构建 script.bin（覆盖 SOC 中预置版本）
    let script_path = soc_dir.join("script.bin");
    if let Some(dirs) = script_dirs {
        sink_impl.emit("build", 3.0, "编译 Lua 脚本...");
        let dir_paths: Vec<std::path::PathBuf> = dirs.iter().map(std::path::PathBuf::from).collect();
        let path_refs: Vec<&std::path::Path> = dir_paths.iter().map(|p| p.as_path()).collect();
        let script_data = luatos_luadb::build::build_script_image(&path_refs, info.script_use_luac(), info.script_bitw(), info.use_bkcrc(), info.script_strip_debug())?;
        std::fs::write(&script_path, &script_data).context("写入 script.bin 失败")?;
    }

    // 验证所有刷机文件存在
    for (name, path) in [
        ("bootloader.bin", &bl_path),
        ("ftab.bin", &ftab_path),
        ("main.bin", &app_path),
        ("script.bin", &script_path),
    ] {
        if !path.exists() {
            bail!("SOC 中缺少 {name}: {}", path.display());
        }
    }

    sink_impl.emit("prepare", 5.0, "准备刷机文件...");
    // 写入顺序：NOR（bootloader + ftab）→ NAND（app + script）
    let flash_files = vec![
        make_flash_file(&bl_path, bl_addr)?,
        make_flash_file(&ftab_path, ftab_addr)?,
        make_flash_file(&app_path, app_addr)?,
        make_flash_file(&script_path, script_addr)?,
    ];

    // 自动复位：进入 ROM BL
    if let Some(cfg) = reset_config {
        sink_impl.emit("reset", 7.0, "自动进入 ROM BL 模式（DTR/RTS）...");
        enter_boot_mode_dtr(port, cfg)?;
    }

    let base = make_sifli_base(port, sink);
    let mut tool = create_sifli_tool(ChipType::SF32LB58, base);

    // stub 加载完成后，协商更高波特率以加速传输
    if let Some(b) = baud {
        sink_impl.emit("speed", 9.0, &format!("协商波特率 {b}..."));
        tool.set_speed(b).context("波特率协商失败")?;
    }

    sink_impl.emit("connect", 10.0, "开始写入...");
    let params = WriteFlashParams {
        files: flash_files,
        verify: false,
        no_compress: false,
        erase_all: false,
    };
    tool.write_flash(&params).context("刷机失败")?;
    let _ = tool.soft_reset();
    drop(tool); // 释放串口，供后续复位操作使用

    // 自动复位：恢复正常运行（DTR 拉低 + 再次硬件复位）
    if let Some(cfg) = reset_config {
        std::thread::sleep(Duration::from_millis(300)); // 等待 soft_reset 完成
        exit_boot_mode_dtr(port, cfg);
    }

    sink_impl.emit_done("刷机完成！");
    Ok(())
}

/// SF32LB58 仅刷脚本分区（NAND @ script_addr）。
///
/// 刷机前需进入 ROM BL 模式：
///   - 手动操作：短接 MODE 引脚 + 按 RESET 键（reset_config = None）
///   - 自动操作：传入 `Some(&config)`，通过 CH340X 增强 DTR 自动控制
///
/// `baud` 为 stub 加载后的协商波特率（None 保持默认 1M）。CH342K 支持最高 3M。
pub fn flash_script_sf32lb5x(
    soc: &str,
    port: &str,
    script_dirs: &[&str],
    on_progress: ProgressCallback,
    _cancel: Arc<AtomicBool>,
    reset_config: Option<&Sf32ResetConfig>,
    baud: Option<u32>,
) -> Result<()> {
    let info = luatos_soc::read_soc_info(soc).context("读取 SOC 信息失败")?;
    let script_addr = info.script_addr();

    let address_regions = vec![(script_addr, "script".to_string())];
    let sink_impl = Arc::new(SifliProgressSink::new(on_progress, address_regions));
    let sink: Arc<dyn ProgressSink> = sink_impl.clone();

    sink_impl.emit("build", 0.0, "编译 Lua 脚本...");
    let dir_paths: Vec<std::path::PathBuf> = script_dirs.iter().map(std::path::PathBuf::from).collect();
    let path_refs: Vec<&std::path::Path> = dir_paths.iter().map(|p| p.as_path()).collect();
    let script_data = luatos_luadb::build::build_script_image(&path_refs, info.script_use_luac(), info.script_bitw(), info.use_bkcrc(), info.script_strip_debug())?;

    let tempdir = tempfile::tempdir().context("创建临时目录失败")?;
    let script_path = tempdir.path().join("script.bin");
    std::fs::write(&script_path, &script_data).context("写入 script.bin 失败")?;

    let flash_files = vec![make_flash_file(&script_path, script_addr)?];

    // 自动复位：进入 ROM BL
    if let Some(cfg) = reset_config {
        sink_impl.emit("reset", 12.0, "自动进入 ROM BL 模式（DTR/RTS）...");
        enter_boot_mode_dtr(port, cfg)?;
    }

    let base = make_sifli_base(port, sink);
    let mut tool = create_sifli_tool(ChipType::SF32LB58, base);

    // stub 加载完成后，协商更高波特率以加速传输
    if let Some(b) = baud {
        sink_impl.emit("speed", 16.0, &format!("协商波特率 {b}..."));
        tool.set_speed(b).context("波特率协商失败")?;
    }

    sink_impl.emit("connect", 18.0, "开始写入...");
    let params = WriteFlashParams {
        files: flash_files,
        verify: false,
        no_compress: false,
        erase_all: false,
    };
    tool.write_flash(&params).context("刷脚本失败")?;
    let _ = tool.soft_reset();
    drop(tool); // 释放串口

    // 自动复位：恢复正常运行
    if let Some(cfg) = reset_config {
        std::thread::sleep(Duration::from_millis(300));
        exit_boot_mode_dtr(port, cfg);
    }

    sink_impl.emit_done("脚本刷写完成！");
    Ok(())
}
