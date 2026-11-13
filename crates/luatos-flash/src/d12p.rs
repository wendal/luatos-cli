//! D12p（D126）AIC USB 升级协议。
//!
//! 设备在升级模式下枚举为厂商自定义 USB Bulk 设备（VID 0x33C3 / PID 0x6677），
//! 传输层是 Bulk-Only Transport：31 字节 CBW + 数据 + 13 字节 CSW；
//! 命令层是 16 字节 "UPGC"/"UPGR" 头。
//!
//! 刷完整镜像分两个阶段：
//! 1. Stage 0（Boot ROM）：把 `image.updater.*` 下载到 RAM 并跳转；
//! 2. 设备重新枚举为 Stage 1（U-Boot）：按 `image.target.*` 写入 SPI NOR。
//!
//! 协议细节与 luban-lite `upgcmd` 保持一致，见
//! `application/baremetal/bootloader/lib/aicupg`。

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use nusb::descriptors::TransferType;
use nusb::transfer::{Buffer, Bulk, In, Out, TransferError};
use nusb::{Device, DeviceInfo, Endpoint, Interface, MaybeFuture};

use crate::{FlashProgress, ProgressCallback};

/// ArtInChip 升级模式 VID。
pub const D12P_VID: u16 = 0x33c3;
/// ArtInChip 升级模式 PID（'fw' = 0x66 0x77）。
pub const D12P_PID: u16 = 0x6677;

/// 单次 USB 传输超时。
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(10);
/// 等待设备重新枚举（Stage 0 → Stage 1）的超时。
const RECONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// Boot ROM 阶段单个数据包上限（与 upgcmd 一致）。
const STAGE0_PACKET_SIZE: usize = 0x800;
/// U-Boot 阶段单个数据包上限。
const STAGE1_PACKET_SIZE: usize = 0x10000;
/// 主机侧读取/发送分块上限。
const MAX_CHUNK: usize = 0x100000;

// ─── 协议常量 ────────────────────────────────────────────────────────────────

const CBW_SIGN: u32 = 0x4342_5355; // "USBC"（LE 字节序）
const CSW_SIGN: u32 = 0x5342_5355; // "USBS"
const CMD_HEADER_MAGIC: u32 = 0x4347_5055; // "UPGC"
const RESP_HEADER_MAGIC: u32 = 0x5247_5055; // "UPGR"
const PROTOCOL_TYPE: u8 = 1;
const PROTOCOL_VERSION: u8 = 1;

const TRANS_LAYER_WRITE: u8 = 0x01;
const TRANS_LAYER_READ: u8 = 0x02;

const CMD_GET_HWINFO: u8 = 0x00;
const CMD_RUN_SHELL_STR: u8 = 0x05;
const CMD_SET_UPG_CFG: u8 = 0x0a;
const CMD_SET_UPG_END: u8 = 0x0b;
const CMD_SET_FWC_META: u8 = 0x10;
const CMD_GET_BLOCK_SIZE: u8 = 0x11;
const CMD_SEND_FWC_DATA: u8 = 0x12;
const CMD_GET_FWC_CRC: u8 = 0x13;
const CMD_GET_FWC_BURN_RESULT: u8 = 0x14;
const CMD_GET_FWC_RUN_RESULT: u8 = 0x15;
const CMD_GET_PARTITION_TABLE: u8 = 0x17;
const CMD_READ_FWC_DATA: u8 = 0x18;

/// 全盘升级。
const UPG_MODE_FULL_DISK: u8 = 0;
/// 分区升级。
const UPG_MODE_PARTITION: u8 = 1;

const IMAGE_HDR_SIZE: usize = 0x800;
const META_SIZE: usize = 0x200;
const META_OFF_OFFSET: usize = 0x88;
const META_OFF_SIZE: usize = 0x8c;
const META_OFF_CRC: usize = 0x90;
const META_OFF_RAM: usize = 0x94;
const META_OFF_ATTR: usize = 0x98;
const META_OFF_PARTITION: usize = 0x48;
const META_OFF_NAME: usize = 0x08;

// ─── 镜像解析 ────────────────────────────────────────────────────────────────

/// AIC 固件镜像（`.img`）中的一个固件组件。
#[derive(Debug, Clone)]
pub struct AicMeta {
    pub name: String,
    pub partition: String,
    pub offset: u32,
    pub size: u32,
    pub crc: u32,
    pub ram: u32,
    pub attr: String,
    /// 原始 512 字节，发送 `SET_FWC_META` 时直接使用。
    raw: [u8; META_SIZE],
}

impl AicMeta {
    fn synthetic(name: &str, partition: &str, size: u32, crc: u32) -> Self {
        let mut raw = [0u8; META_SIZE];
        raw[0..4].copy_from_slice(b"META");
        copy_cstr(&mut raw[META_OFF_NAME..META_OFF_PARTITION], name);
        copy_cstr(&mut raw[META_OFF_PARTITION..META_OFF_OFFSET], partition);
        raw[META_OFF_SIZE..META_OFF_SIZE + 4].copy_from_slice(&size.to_le_bytes());
        raw[META_OFF_CRC..META_OFF_CRC + 4].copy_from_slice(&crc.to_le_bytes());
        raw[META_OFF_RAM..META_OFF_RAM + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let attr = b"mtd;required";
        raw[META_OFF_ATTR..META_OFF_ATTR + attr.len()].copy_from_slice(attr);
        Self {
            name: name.to_string(),
            partition: partition.to_string(),
            offset: 0,
            size,
            crc,
            ram: u32::MAX,
            attr: "mtd;required".to_string(),
            raw,
        }
    }
}

/// D12p 的 MTD 分区。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct D12pPartition {
    pub name: String,
    pub offset: u64,
    pub size: u64,
}

/// 解析 bootloader 返回的分区表字符串，例如：
/// `spi0.0:512k(spl),128k(env),1m(scripts),64k(fskv)`。
pub fn parse_mtd_layout(text: &str) -> Result<Vec<D12pPartition>> {
    let table = text.split_once(':').map(|(_, rest)| rest).unwrap_or(text);
    let mut offset = 0u64;
    let mut partitions = Vec::new();
    for entry in table.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (size_text, name) = entry.split_once('(').with_context(|| format!("分区表项格式错误: {entry}"))?;
        let name = name.strip_suffix(')').with_context(|| format!("分区表项缺少右括号: {entry}"))?;
        let size = parse_partition_size(size_text)?;
        partitions.push(D12pPartition {
            name: name.to_string(),
            offset,
            size,
        });
        offset += size;
    }
    anyhow::ensure!(!partitions.is_empty(), "D126 分区表为空");
    Ok(partitions)
}

fn parse_partition_size(text: &str) -> Result<u64> {
    let text = text.trim();
    let (digits, multiplier) = match text.as_bytes().last().copied() {
        Some(b'k' | b'K') => (&text[..text.len() - 1], 1024u64),
        Some(b'm' | b'M') => (&text[..text.len() - 1], 1024u64 * 1024),
        _ => (text, 1u64),
    };
    let value: u64 = digits.parse().with_context(|| format!("分区大小不是数字: {text}"))?;
    Ok(value * multiplier)
}

/// 解析后的 AIC 固件镜像。
#[derive(Debug, Clone)]
pub struct AicImage {
    pub platform: String,
    pub product: String,
    pub version: String,
    pub media_type: String,
    pub metas: Vec<AicMeta>,
    meta_offset: usize,
    data: Vec<u8>,
}

impl AicImage {
    /// 解析 `.img`。文件布局：0x800 包头 + 元数据区 + 文件数据区。
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < IMAGE_HDR_SIZE {
            bail!("D126 镜像长度不足 0x800 字节");
        }
        if &data[0..6] != b"AIC.FW" {
            bail!("不是 AIC 固件镜像（magic 应为 AIC.FW）");
        }

        let meta_offset = read_u32(data, 0x14c) as usize;
        let meta_size = read_u32(data, 0x150) as usize;
        let file_offset = read_u32(data, 0x154) as usize;
        let file_size = read_u32(data, 0x158) as usize;
        if meta_offset < IMAGE_HDR_SIZE || !meta_size.is_multiple_of(META_SIZE) || meta_offset + meta_size > data.len() || file_offset + file_size > data.len() {
            bail!("D126 镜像头偏移/长度非法");
        }

        let mut metas = Vec::with_capacity(meta_size / META_SIZE);
        for start in (meta_offset..meta_offset + meta_size).step_by(META_SIZE) {
            let chunk = &data[start..start + META_SIZE];
            if &chunk[0..4] != b"META" {
                bail!("D126 镜像元数据区缺少 META 标识");
            }
            let mut raw = [0u8; META_SIZE];
            raw.copy_from_slice(chunk);
            metas.push(AicMeta {
                name: read_cstr(&chunk[META_OFF_NAME..META_OFF_PARTITION]),
                partition: read_cstr(&chunk[META_OFF_PARTITION..META_OFF_OFFSET]),
                offset: read_u32(chunk, META_OFF_OFFSET),
                size: read_u32(chunk, META_OFF_SIZE),
                crc: read_u32(chunk, META_OFF_CRC),
                ram: read_u32(chunk, META_OFF_RAM),
                attr: read_cstr(&chunk[META_OFF_ATTR..META_OFF_ATTR + 64]),
                raw,
            });
        }
        if metas.is_empty() {
            bail!("D126 镜像不包含任何固件组件");
        }

        Ok(Self {
            platform: read_cstr(&data[0x08..0x48]),
            product: read_cstr(&data[0x48..0x88]),
            version: read_cstr(&data[0x88..0xc8]),
            media_type: read_cstr(&data[0xc8..0x108]),
            metas,
            meta_offset,
            data: data.to_vec(),
        })
    }

    /// 用新的脚本镜像替换 `image.target.scripts` 组件，并同步更新 META CRC。
    ///
    /// 组件在镜像中的偏移和长度保持不变（D126 固定 1 MiB），因此可以原地写回。
    pub fn replace_script_component(&mut self, overlay: &[u8]) -> Result<()> {
        let index = self
            .metas
            .iter()
            .position(|meta| meta.name == "image.target.scripts")
            .context("D126 镜像缺少 image.target.scripts 组件")?;
        let meta = &self.metas[index];
        if overlay.len() > meta.size as usize {
            bail!("脚本镜像 {} 字节超过组件大小 {} 字节", overlay.len(), meta.size);
        }
        let mut payload = vec![0xffu8; meta.size as usize];
        payload[..overlay.len()].copy_from_slice(overlay);
        let crc = crc32fast::hash(&payload);

        let start = meta.offset as usize;
        let end = start + payload.len();
        if end > self.data.len() {
            bail!("脚本组件超出镜像范围");
        }
        self.data[start..end].copy_from_slice(&payload);

        let crc_offset = self.meta_offset + index * META_SIZE + META_OFF_CRC;
        self.data[crc_offset..crc_offset + 4].copy_from_slice(&crc.to_le_bytes());
        self.metas[index].crc = crc;
        self.metas[index].raw[META_OFF_CRC..META_OFF_CRC + 4].copy_from_slice(&crc.to_le_bytes());
        Ok(())
    }

    /// 返回镜像字节。
    pub fn into_bytes(self) -> Vec<u8> {
        self.data
    }

    /// 组件数据。脚本组件允许用外部镜像覆盖（长度不足时按分区大小补 0xFF）。
    fn payload(&self, meta: &AicMeta, script_overlay: Option<&[u8]>) -> Result<(Vec<u8>, u32)> {
        let is_script = meta.name == "image.target.scripts";
        if is_script {
            if let Some(overlay) = script_overlay {
                if overlay.len() > meta.size as usize {
                    bail!("脚本镜像 {} 字节超过分区大小 {} 字节", overlay.len(), meta.size);
                }
                let mut data = vec![0xffu8; meta.size as usize];
                data[..overlay.len()].copy_from_slice(overlay);
                let crc = crc32fast::hash(&data);
                return Ok((data, crc));
            }
        }

        let start = meta.offset as usize;
        let end = start
            .checked_add(meta.size as usize)
            .filter(|end| *end <= self.data.len())
            .with_context(|| format!("组件 {} 数据超出镜像范围", meta.name))?;
        Ok((self.data[start..end].to_vec(), meta.crc))
    }
}

// ─── USB 设备选择 ────────────────────────────────────────────────────────────

/// `bus:port` 形式的 USB 选择器，例如 `1:2-4-2`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbSelector {
    bus: String,
    port_chain: Vec<u8>,
}

impl UsbSelector {
    /// 解析选择器；空串、`auto` 或 `None` 表示自动选择。
    pub fn parse(value: Option<&str>) -> Result<Option<Self>> {
        let Some(value) = value.map(str::trim).filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("auto")) else {
            return Ok(None);
        };
        let (bus, ports) = value.split_once(':').with_context(|| format!("USB 选择器格式应为 bus:port（如 1:2-4-2），收到 {value}"))?;
        let port_chain = ports
            .split('-')
            .map(|part| part.parse::<u8>().with_context(|| format!("非法 USB 端口号: {part}")))
            .collect::<Result<Vec<_>>>()?;
        if bus.is_empty() || port_chain.is_empty() {
            bail!("USB 选择器格式应为 bus:port（如 1:2-4-2），收到 {value}");
        }
        Ok(Some(Self { bus: bus.to_string(), port_chain }))
    }

    fn matches(&self, info: &DeviceInfo) -> bool {
        info.bus_id() == self.bus && info.port_chain() == self.port_chain.as_slice()
    }
}

fn select_device(selector: Option<&UsbSelector>) -> Result<DeviceInfo> {
    let devices: Vec<DeviceInfo> = nusb::list_devices()
        .wait()
        .context("枚举 USB 设备失败")?
        .filter(|info| info.vendor_id() == D12P_VID && info.product_id() == D12P_PID)
        .collect();

    if devices.is_empty() {
        bail!("未找到 D126 USB 升级设备（33c3:6677）；请让设备进入 USB 升级模式后重试");
    }

    match selector {
        Some(selector) => devices.into_iter().find(|info| selector.matches(info)).with_context(|| {
            format!(
                "未找到指定的 D126 设备 {}:{}",
                selector.bus,
                selector.port_chain.iter().map(u8::to_string).collect::<Vec<_>>().join("-")
            )
        }),
        None if devices.len() == 1 => Ok(devices.into_iter().next().unwrap()),
        None => {
            let list: Vec<String> = devices
                .iter()
                .map(|d| format!("{}:{}", d.bus_id(), d.port_chain().iter().map(u8::to_string).collect::<Vec<_>>().join("-")))
                .collect();
            bail!("检测到多个 D126 USB 升级设备（{}），请用 --port bus:port 指定其中一个", list.join(", "))
        }
    }
}

/// 等待设备重新枚举（Stage 0 → Stage 1 会短暂断开）。
fn wait_for_device(selector: Option<&UsbSelector>, timeout: Duration) -> Result<UsbLink> {
    let deadline = Instant::now() + timeout;
    loop {
        match select_device(selector).and_then(UsbLink::open) {
            Ok(link) => return Ok(link),
            Err(err) if Instant::now() >= deadline => return Err(err),
            Err(_) => {}
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

// ─── 传输层 ──────────────────────────────────────────────────────────────────

/// USB 链路丢失。Stage 切换时设备会主动断开，调用方据此重连。
#[derive(Debug)]
struct LinkLost;

impl fmt::Display for LinkLost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "USB 设备已断开")
    }
}

impl std::error::Error for LinkLost {}

fn is_link_lost(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| cause.downcast_ref::<LinkLost>().is_some())
}

fn transfer_error(err: TransferError) -> anyhow::Error {
    match err {
        TransferError::Disconnected | TransferError::Stall | TransferError::Cancelled | TransferError::Unknown(_) => anyhow::Error::new(LinkLost),
        other => anyhow::anyhow!("USB 传输失败: {other}"),
    }
}

struct UsbLink {
    _device: Device,
    _interface: Interface,
    ep_out: Endpoint<Bulk, Out>,
    ep_in: Endpoint<Bulk, In>,
    seq: u32,
    /// U-Boot 阶段单包上限；Boot ROM 阶段由 ping 调整为 2 KiB。
    packet_size: usize,
}

impl UsbLink {
    fn open(info: DeviceInfo) -> Result<Self> {
        let device = info.open().wait().context("打开 D126 USB 设备失败")?;
        let (interface_number, out_addr, in_addr) = {
            let config = device.active_configuration().context("读取 USB 配置描述符失败")?;
            config
                .interface_alt_settings()
                .find_map(|alt| {
                    let mut out_addr = None;
                    let mut in_addr = None;
                    for ep in alt.endpoints() {
                        if ep.transfer_type() != TransferType::Bulk {
                            continue;
                        }
                        if ep.address() & 0x80 == 0 {
                            out_addr = Some(ep.address());
                        } else {
                            in_addr = Some(ep.address());
                        }
                    }
                    out_addr.zip(in_addr).map(|(o, i)| (alt.interface_number(), o, i))
                })
                .context("D126 设备没有可用的 Bulk IN/OUT 端点")?
        };

        let interface = device.claim_interface(interface_number).wait().context("占用 D126 USB 接口失败（检查 udev 权限）")?;
        let ep_out = interface.endpoint::<Bulk, Out>(out_addr).context("打开 Bulk OUT 端点失败")?;
        let ep_in = interface.endpoint::<Bulk, In>(in_addr).context("打开 Bulk IN 端点失败")?;

        Ok(Self {
            _device: device,
            _interface: interface,
            ep_out,
            ep_in,
            seq: 0,
            packet_size: STAGE1_PACKET_SIZE,
        })
    }

    fn bulk_out(&mut self, data: &[u8]) -> Result<()> {
        let completion = self.ep_out.transfer_blocking(Buffer::from(data), TRANSFER_TIMEOUT);
        completion.status.map_err(transfer_error)?;
        if completion.actual_len != data.len() {
            bail!("USB 发送长度不符: {} != {}", completion.actual_len, data.len());
        }
        Ok(())
    }

    fn bulk_in(&mut self, len: usize) -> Result<Vec<u8>> {
        // usbfs 要求 IN 传输长度按端点最大包大小对齐；设备会用短包返回实际长度。
        let packet = self.ep_in.max_packet_size().max(1);
        let request = len.div_ceil(packet) * packet;
        let completion = self.ep_in.transfer_blocking(Buffer::new(request), TRANSFER_TIMEOUT);
        completion.status.map_err(transfer_error)?;
        if completion.actual_len != len {
            bail!("USB 接收长度不符: {} != {len}", completion.actual_len);
        }
        let mut data = completion.buffer.into_vec();
        data.truncate(len);
        Ok(data)
    }

    /// 发送一个 CBW（31 字节）并等待 CSW。
    fn cbw(&mut self, flags: u8, command: u8, data_len: u32) -> Result<u32> {
        self.seq = self.seq.wrapping_add(1);
        let tag = self.seq;
        let mut cbw = [0u8; 31];
        cbw[0..4].copy_from_slice(&CBW_SIGN.to_le_bytes());
        cbw[4..8].copy_from_slice(&tag.to_le_bytes());
        cbw[8..12].copy_from_slice(&data_len.to_le_bytes());
        cbw[12] = flags;
        cbw[14] = 1;
        cbw[15] = command;
        self.bulk_out(&cbw)?;
        Ok(tag)
    }

    fn csw(&mut self, tag: u32) -> Result<()> {
        let csw = self.bulk_in(13)?;
        if read_u32(&csw, 0) != CSW_SIGN {
            bail!("D126 CSW 标识错误");
        }
        if read_u32(&csw, 4) != tag {
            bail!("D126 CSW 序号不匹配");
        }
        if csw[12] != 0 {
            bail!("D126 CSW 状态错误: {}", csw[12]);
        }
        Ok(())
    }

    /// 发送一个完整数据包（CBW + 负载 + CSW）。
    fn send_packet(&mut self, payload: &[u8]) -> Result<()> {
        let tag = self.cbw(0, TRANS_LAYER_WRITE, payload.len() as u32)?;
        if !payload.is_empty() {
            self.bulk_out(payload)?;
        }
        self.csw(tag)
    }

    /// 接收一个数据包（CBW + 负载 + CSW）。
    fn recv_packet(&mut self, len: usize) -> Result<Vec<u8>> {
        let tag = self.cbw(0x80, TRANS_LAYER_READ, len as u32)?;
        let data = self.bulk_in(len)?;
        self.csw(tag)?;
        Ok(data)
    }

    // ─── 命令层 ──────────────────────────────────────────────────────────────

    fn send_cmd_header(&mut self, command: u8, data_len: u32) -> Result<()> {
        let header = cmd_header(command, data_len);
        self.send_packet(&header)
    }

    /// 读取 16 字节响应头，校验状态并返回 `data_length`。
    fn recv_resp_header(&mut self) -> Result<u32> {
        let resp = self.recv_packet(16)?;
        if read_u32(&resp, 0) != RESP_HEADER_MAGIC {
            bail!("D126 响应头标识错误");
        }
        if resp[7] != 0 {
            bail!("D126 命令 0x{:02x} 执行失败（状态 {}）", resp[6], resp[7]);
        }
        Ok(read_u32(&resp, 8))
    }

    fn recv_resp_ok(&mut self) -> Result<()> {
        let len = self.recv_resp_header()?;
        if len != 0 {
            bail!("D126 响应长度异常: {len}");
        }
        Ok(())
    }

    fn recv_resp_u32(&mut self) -> Result<u32> {
        let len = self.recv_resp_header()?;
        if len != 4 {
            bail!("D126 响应长度应为 4，实际 {len}");
        }
        let data = self.recv_packet(4)?;
        Ok(read_u32(&data, 0))
    }

    /// 返回 boot stage：0 = Boot ROM，1 = U-Boot。
    fn get_boot_stage(&mut self) -> Result<u8> {
        self.send_cmd_header(CMD_GET_HWINFO, 0)?;
        let len = self.recv_resp_header()? as usize;
        if len < 44 {
            bail!("D126 HWINFO 长度异常: {len}");
        }
        let data = self.recv_packet(len)?;
        Ok(data[40])
    }

    fn set_upg_cfg(&mut self, mode: u8) -> Result<()> {
        let mut cfg = [0u8; 32];
        cfg[0] = mode;
        self.send_cmd_header(CMD_SET_UPG_CFG, 4 + cfg.len() as u32)?;
        self.send_packet(&(cfg.len() as u32).to_le_bytes())?;
        self.send_packet(&cfg)?;
        self.recv_resp_ok()
    }

    fn set_upg_end(&mut self) -> Result<()> {
        let param = [0u8; 32];
        self.send_cmd_header(CMD_SET_UPG_END, 4 + param.len() as u32)?;
        self.send_packet(&(param.len() as u32).to_le_bytes())?;
        self.send_packet(&param)?;
        self.recv_resp_ok()
    }

    /// 在 U-Boot 控制台执行一条命令。
    fn run_shell(&mut self, command: &str) -> Result<()> {
        let bytes = command.as_bytes();
        if bytes.len() >= 128 {
            bail!("U-Boot 命令过长");
        }
        self.send_cmd_header(CMD_RUN_SHELL_STR, 4 + bytes.len() as u32)?;
        self.send_packet(&(bytes.len() as u32).to_le_bytes())?;
        self.send_packet(bytes)?;
        self.recv_resp_ok()
    }

    fn set_fwc_meta(&mut self, meta: &AicMeta, crc: u32) -> Result<()> {
        let mut raw = meta.raw;
        raw[META_OFF_CRC..META_OFF_CRC + 4].copy_from_slice(&crc.to_le_bytes());
        self.send_cmd_header(CMD_SET_FWC_META, META_SIZE as u32)?;
        self.send_packet(&raw)?;
        self.recv_resp_ok()
    }

    fn get_block_size(&mut self) -> Result<u32> {
        self.send_cmd_header(CMD_GET_BLOCK_SIZE, 0)?;
        self.recv_resp_u32()
    }

    fn send_fwc_data_start(&mut self, size: u32) -> Result<()> {
        self.send_cmd_header(CMD_SEND_FWC_DATA, size)
    }

    /// 发送一段固件数据，按当前阶段允许的单包大小拆分。
    fn send_fwc_data_update(&mut self, data: &[u8]) -> Result<()> {
        for packet in data.chunks(self.packet_size) {
            self.send_packet(packet)?;
        }
        Ok(())
    }

    fn send_fwc_data_final(&mut self) -> Result<()> {
        self.recv_resp_ok()
    }

    fn get_fwc_crc(&mut self) -> Result<u32> {
        self.send_cmd_header(CMD_GET_FWC_CRC, 0)?;
        self.recv_resp_u32()
    }

    fn get_fwc_burn_result(&mut self) -> Result<u32> {
        self.send_cmd_header(CMD_GET_FWC_BURN_RESULT, 0)?;
        self.recv_resp_u32()
    }

    fn get_fwc_run_result(&mut self) -> Result<u32> {
        self.send_cmd_header(CMD_GET_FWC_RUN_RESULT, 0)?;
        self.recv_resp_u32()
    }

    /// 读取 SPI NOR 的 MTD 分区表字符串。
    fn get_partition_table(&mut self) -> Result<String> {
        let media = storage_media();
        self.send_cmd_header(CMD_GET_PARTITION_TABLE, media.len() as u32)?;
        self.send_packet(&media)?;
        let len = self.recv_resp_header()?;
        if len != 128 {
            bail!("D126 分区表长度异常: {len}");
        }
        let data = self.recv_packet(len as usize)?;
        Ok(read_cstr(&data))
    }
}

// ─── 会话 ────────────────────────────────────────────────────────────────────

struct Session {
    link: UsbLink,
    in_uboot: bool,
    cfg_sent: bool,
}

/// 写组件时的进度上下文。
struct WriteProgress<'a> {
    on_progress: &'a ProgressCallback,
    base: u64,
    total: u64,
}

impl Session {
    fn connect(selector: Option<&UsbSelector>) -> Result<Self> {
        let link = wait_for_device(selector, Duration::from_secs(1))?;
        let mut session = Self {
            link,
            in_uboot: false,
            cfg_sent: false,
        };
        session.ping()?;
        Ok(session)
    }

    fn replace_link(&mut self, link: UsbLink) {
        self.link = link;
        self.in_uboot = false;
        self.cfg_sent = false;
    }

    /// 读取 HWINFO。对齐 upgcmd：首次进入 U-Boot 阶段时设置全盘升级模式；
    /// Boot ROM 阶段把单包大小降到 2 KiB。
    fn ping(&mut self) -> Result<()> {
        let mut last_error = None;
        for attempt in 0..10 {
            match self.link.get_boot_stage() {
                Ok(stage) => {
                    if stage == 1 {
                        if !self.cfg_sent {
                            self.link.set_upg_cfg(UPG_MODE_FULL_DISK)?;
                            self.cfg_sent = true;
                        }
                        self.in_uboot = true;
                        self.link.packet_size = STAGE1_PACKET_SIZE;
                    } else {
                        self.in_uboot = false;
                        self.link.packet_size = STAGE0_PACKET_SIZE;
                    }
                    return Ok(());
                }
                Err(err) if is_link_lost(&err) && attempt < 9 => {
                    last_error = Some(err);
                    std::thread::sleep(Duration::from_millis(500));
                }
                Err(err) => return Err(err),
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("D126 设备无响应")))
    }

    fn write_component(&mut self, meta: &AicMeta, data: &[u8], crc: u32, progress: &WriteProgress, cancel: &AtomicBool) -> Result<()> {
        let on_progress = progress.on_progress;
        let base = progress.base;
        let total = progress.total;
        self.link.set_fwc_meta(meta, crc)?;
        let block_size = self.link.get_block_size()? as usize;
        if block_size == 0 {
            bail!("D126 设备返回的块大小为 0");
        }
        if data.len() != meta.size as usize {
            bail!("组件 {} 数据长度 {} 与元数据 {} 不符", meta.name, data.len(), meta.size);
        }

        let chunk = chunk_size(block_size);
        self.link.send_fwc_data_start(meta.size)?;

        let full_chunks = data.len() / chunk;
        let tail = data.len() % chunk;
        for index in 0..full_chunks {
            check_cancel(cancel)?;
            let start = index * chunk;
            self.link.send_fwc_data_update(&data[start..start + chunk])?;
            report_progress(on_progress, meta, base + (start + chunk) as u64, total);
        }
        if tail > 0 {
            check_cancel(cancel)?;
            let start = full_chunks * chunk;
            let padding = (block_size - tail % block_size) % block_size;
            let mut last = Vec::with_capacity(tail + padding);
            last.extend_from_slice(&data[start..]);
            last.resize(tail + padding, 0);
            self.link.send_fwc_data_update(&last)?;
        }

        self.link.send_fwc_data_final()?;
        let device_crc = self.link.get_fwc_crc()?;
        if device_crc != crc {
            bail!("组件 {} CRC 校验失败：设备 0x{device_crc:08X}，镜像 0x{crc:08X}", meta.name);
        }
        if self.link.get_fwc_burn_result()? != 0 {
            bail!("组件 {} 烧录失败", meta.name);
        }
        if self.link.get_fwc_run_result()? != 0 {
            bail!("组件 {} 运行失败", meta.name);
        }
        report_progress(on_progress, meta, base + meta.size as u64, total);
        Ok(())
    }

    fn finish(mut self) -> Result<()> {
        self.link.set_upg_end()?;
        // 设备不会因为 SET_UPG_END 退出升级模式，需要 U-Boot 执行 reset；
        // reset 会立即断开 USB，响应丢失属于正常现象。
        let _ = self.link.run_shell("reset");
        Ok(())
    }
}

fn chunk_size(block_size: usize) -> usize {
    if block_size > MAX_CHUNK {
        block_size
    } else {
        MAX_CHUNK - (MAX_CHUNK % block_size)
    }
}

fn report_progress(on_progress: &ProgressCallback, meta: &AicMeta, sent: u64, total: u64) {
    let percent = if total == 0 { 0.0 } else { (sent as f32 * 100.0 / total as f32).min(100.0) };
    let region = if meta.partition.is_empty() { meta.name.as_str() } else { meta.partition.as_str() };
    let message = format!("写入 {}", meta.name);
    on_progress(&FlashProgress::info("D12p", percent, &message).with_region(region));
}

fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        bail!("用户取消");
    }
    Ok(())
}

// ─── 对外入口 ────────────────────────────────────────────────────────────────

/// 刷写完整 AIC 镜像（含 Stage 0 → Stage 1 切换）。
///
/// `script_overlay` 用于替换镜像内的 `image.target.scripts` 组件；长度不得超过
/// 分区大小，不足部分补 0xFF。
pub fn flash_image(data: &[u8], selector: Option<&str>, script_overlay: Option<&[u8]>, on_progress: &ProgressCallback, cancel: Arc<AtomicBool>) -> Result<()> {
    let image = AicImage::parse(data)?;
    let selector = UsbSelector::parse(selector)?;
    let total: u64 = image.metas.iter().map(|meta| meta.size as u64).sum();

    let mut sent: u64 = 0;
    let mut session = Session::connect(selector.as_ref())?;
    let mut index = 0usize;
    let mut attempts = 0u8;

    while index < image.metas.len() {
        check_cancel(&cancel)?;
        let meta = image.metas[index].clone();

        // 每次写组件前刷新阶段信息（与 upgcmd 一致）
        if let Err(err) = session.ping() {
            if !is_link_lost(&err) {
                return Err(err);
            }
            session.replace_link(wait_for_device(selector.as_ref(), RECONNECT_TIMEOUT)?);
            continue;
        }
        let stage0 = !session.in_uboot;

        let (payload, crc) = image.payload(&meta, script_overlay)?;
        let progress = WriteProgress { on_progress, base: sent, total };
        match session.write_component(&meta, &payload, crc, &progress, &cancel) {
            Ok(()) => {
                sent += meta.size as u64;
                index += 1;
                attempts = 0;
            }
            Err(err) if is_link_lost(&err) => {
                // Stage 0 下载完 updater 后设备会主动断开并重新枚举。
                if stage0 && meta.name.starts_with("image.updater") {
                    sent += meta.size as u64;
                    index += 1;
                    attempts = 0;
                } else {
                    attempts += 1;
                    if attempts > 1 {
                        return Err(err).with_context(|| format!("写入 {} 时 USB 连接中断", meta.name));
                    }
                }
                session.replace_link(wait_for_device(selector.as_ref(), RECONNECT_TIMEOUT)?);
            }
            Err(err) => return Err(err),
        }
    }

    session.finish()?;
    on_progress(&FlashProgress::done_ok("D126 烧录完成"));
    Ok(())
}

/// 在 Boot ROM 阶段先下载镜像内的 updater，把设备带到 U-Boot 升级阶段。
fn bootstrap_to_uboot(session: &mut Session, image: &AicImage, selector: Option<&UsbSelector>, on_progress: &ProgressCallback, cancel: &AtomicBool) -> Result<()> {
    let total: u64 = image.metas.iter().take_while(|meta| is_bootstrap_component(meta)).map(|meta| meta.size as u64).sum();
    let mut sent = 0u64;

    for meta in image.metas.iter().take_while(|meta| is_bootstrap_component(meta)) {
        check_cancel(cancel)?;
        if let Err(err) = session.ping() {
            if !is_link_lost(&err) {
                return Err(err);
            }
            session.replace_link(wait_for_device(selector, RECONNECT_TIMEOUT)?);
            session.ping()?;
        }
        let stage0 = !session.in_uboot;
        let (payload, crc) = image.payload(meta, None)?;
        let progress = WriteProgress {
            on_progress,
            base: sent,
            total: total.max(1),
        };
        match session.write_component(meta, &payload, crc, &progress, cancel) {
            Ok(()) => sent += meta.size as u64,
            Err(err) if is_link_lost(&err) => {
                if !(stage0 && meta.name.starts_with("image.updater")) {
                    return Err(err).with_context(|| format!("下载 {} 时设备断开", meta.name));
                }
                sent += meta.size as u64;
                session.replace_link(wait_for_device(selector, RECONNECT_TIMEOUT)?);
            }
            Err(err) => return Err(err),
        }
    }

    session.ping()?;
    anyhow::ensure!(session.in_uboot, "D126 未能进入 U-Boot 升级阶段");
    Ok(())
}

fn is_bootstrap_component(meta: &AicMeta) -> bool {
    meta.name.starts_with("image.updater") || meta.name == "image.info"
}

/// 写任意命名分区（U-Boot 阶段分区升级）。
///
/// 设备仍处于 Boot ROM 阶段时，用 `bootstrap_image`（`.soc` 内的完整 AIC 镜像）
/// 先下载 updater 进入 U-Boot，再写分区。`partition` 必须出现在 bootloader 的
/// MTD 分区表里，例如 `scripts` / `data` / `fskv`。
pub fn flash_partition_data(
    partition: &str,
    data: &[u8],
    bootstrap_image: Option<&[u8]>,
    selector: Option<&str>,
    on_progress: &ProgressCallback,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    if partition.is_empty() {
        bail!("分区名不能为空");
    }
    if data.is_empty() {
        bail!("分区数据为空");
    }
    let selector = UsbSelector::parse(selector)?;
    let mut session = Session::connect(selector.as_ref())?;
    if !session.in_uboot {
        let Some(image) = bootstrap_image else {
            bail!("D126 当前处于 Boot ROM 阶段，无法单独写分区；请先执行一次 flash run");
        };
        let image = AicImage::parse(image)?;
        bootstrap_to_uboot(&mut session, &image, selector.as_ref(), on_progress, &cancel)?;
    }

    // 切换为分区升级模式
    session.link.set_upg_cfg(UPG_MODE_PARTITION)?;

    let name = format!("image.target.{partition}");
    let meta = AicMeta::synthetic(&name, partition, data.len() as u32, crc32fast::hash(data));
    let progress = WriteProgress {
        on_progress,
        base: 0,
        total: data.len() as u64,
    };
    check_cancel(&cancel)?;
    session.write_component(&meta, data, meta.crc, &progress, &cancel)?;
    session.finish()?;
    let message = format!("D126 {partition} 分区烧录完成");
    on_progress(&FlashProgress::done_ok(&message));
    Ok(())
}

/// 读回一个命名分区（U-Boot 阶段 `READ_FWC_DATA`）。
///
/// D12p 的 AIC 协议没有任意地址读，只能按 MTD 分区读；设备在 Boot ROM
/// 阶段时先用 `bootstrap_image` 引导到 U-Boot。
pub fn dump_partition(partition: &str, bootstrap_image: Option<&[u8]>, selector: Option<&str>, on_progress: &ProgressCallback, cancel: Arc<AtomicBool>) -> Result<Vec<u8>> {
    let selector = UsbSelector::parse(selector)?;
    let mut session = Session::connect(selector.as_ref())?;
    if !session.in_uboot {
        let Some(image) = bootstrap_image else {
            bail!("D126 当前处于 Boot ROM 阶段，无法读取分区；请先执行一次 flash run");
        };
        let image = AicImage::parse(image)?;
        bootstrap_to_uboot(&mut session, &image, selector.as_ref(), on_progress, &cancel)?;
    }

    let table = session.link.get_partition_table()?;
    let partitions = parse_mtd_layout(&table)?;
    let part = partitions
        .iter()
        .find(|part| part.name == partition)
        .with_context(|| {
            let names: Vec<&str> = partitions.iter().map(|part| part.name.as_str()).collect();
            format!("D126 分区表中没有 {partition}；可用分区: {}", names.join(", "))
        })?
        .clone();

    let size = usize::try_from(part.size).context("分区过大")?;
    let message = format!("读取 {} ({} 字节)", part.name, part.size);
    on_progress(&FlashProgress::info("D12p", 0.0, &message).with_region(&part.name));

    let meta = media_partition(&part.name, part.offset, part.size);
    session.link.send_cmd_header(CMD_READ_FWC_DATA, meta.len() as u32)?;
    session.link.send_packet(&meta)?;
    let resp_len = session.link.recv_resp_header()?;
    if resp_len as u64 != part.size {
        bail!("D126 返回的分区长度 {resp_len} 与分区表 {} 不符", part.size);
    }

    let mut data = Vec::with_capacity(size);
    while data.len() < size {
        check_cancel(&cancel)?;
        let chunk = (size - data.len()).min(session.link.packet_size);
        let buf = session.link.recv_packet(chunk)?;
        data.extend_from_slice(&buf);
        let percent = data.len() as f32 * 100.0 / size as f32;
        let message = format!("读取 {}", part.name);
        on_progress(&FlashProgress::info("D12p", percent, &message).with_region(&part.name));
    }

    session.finish()?;
    Ok(data)
}

// ─── 工具函数 ────────────────────────────────────────────────────────────────

/// `struct storage_media`：media_type[64] + media_dev_id(u32)，`__packed`。
fn storage_media() -> [u8; 68] {
    let mut media = [0u8; 68];
    media[..7].copy_from_slice(b"spi-nor");
    media
}

/// 设备端 `struct media_partition`（32 位 ARM，未 packed）：
/// media[68] + pad[4] + name[144] + index(4) + pad[4] + start(8) + size(8)
/// + next(4) + pad[4] + attr[64]，共 312 字节。
fn media_partition(name: &str, start: u64, size: u64) -> [u8; 312] {
    let mut partition = [0u8; 312];
    partition[..7].copy_from_slice(b"spi-nor");
    copy_cstr(&mut partition[72..216], name);
    partition[224..232].copy_from_slice(&start.to_le_bytes());
    partition[232..240].copy_from_slice(&size.to_le_bytes());
    copy_cstr(&mut partition[248..312], "mtd");
    partition
}

fn cmd_header(command: u8, data_length: u32) -> [u8; 16] {
    let mut header = [0u8; 16];
    header[0..4].copy_from_slice(&CMD_HEADER_MAGIC.to_le_bytes());
    header[4] = PROTOCOL_TYPE;
    header[5] = PROTOCOL_VERSION;
    header[6] = command;
    header[7] = 0;
    header[8..12].copy_from_slice(&data_length.to_le_bytes());
    let sum = CMD_HEADER_MAGIC
        .wrapping_add(((PROTOCOL_VERSION as u32) << 8) | PROTOCOL_TYPE as u32 | ((command as u32) << 16))
        .wrapping_add(data_length);
    header[12..16].copy_from_slice(&sum.to_le_bytes());
    header
}

fn read_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([data[offset], data[offset + 1], data[offset + 2], data[offset + 3]])
}

fn read_cstr(data: &[u8]) -> String {
    let end = data.iter().position(|byte| *byte == 0).unwrap_or(data.len());
    String::from_utf8_lossy(&data[..end]).trim().to_string()
}

fn copy_cstr(dest: &mut [u8], value: &str) {
    let bytes = value.as_bytes();
    let len = bytes.len().min(dest.len().saturating_sub(1));
    dest[..len].copy_from_slice(&bytes[..len]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_image() -> Vec<u8> {
        let mut image = vec![0u8; IMAGE_HDR_SIZE + META_SIZE * 2 + 0x100];
        image[0..6].copy_from_slice(b"AIC.FW");
        copy_cstr(&mut image[0x08..0x48], "d12p");
        copy_cstr(&mut image[0x48..0x88], "d126-test");
        copy_cstr(&mut image[0x88..0xc8], "1.0.0");
        copy_cstr(&mut image[0xc8..0x108], "spi-nor");
        image[0x14c..0x150].copy_from_slice(&(IMAGE_HDR_SIZE as u32).to_le_bytes());
        image[0x150..0x154].copy_from_slice(&((META_SIZE * 2) as u32).to_le_bytes());
        image[0x154..0x158].copy_from_slice(&((IMAGE_HDR_SIZE + META_SIZE * 2) as u32).to_le_bytes());
        image[0x158..0x15c].copy_from_slice(&0x100u32.to_le_bytes());

        for (index, name) in ["image.updater.spl", "image.target.os"].iter().enumerate() {
            let meta = &mut image[IMAGE_HDR_SIZE + index * META_SIZE..IMAGE_HDR_SIZE + (index + 1) * META_SIZE];
            meta[0..4].copy_from_slice(b"META");
            copy_cstr(&mut meta[META_OFF_NAME..META_OFF_PARTITION], name);
            copy_cstr(&mut meta[META_OFF_PARTITION..META_OFF_OFFSET], "os");
            meta[META_OFF_OFFSET..META_OFF_OFFSET + 4].copy_from_slice(&((IMAGE_HDR_SIZE + META_SIZE * 2) as u32).to_le_bytes());
            meta[META_OFF_SIZE..META_OFF_SIZE + 4].copy_from_slice(&0x100u32.to_le_bytes());
            meta[META_OFF_CRC..META_OFF_CRC + 4].copy_from_slice(&0x1234u32.to_le_bytes());
        }
        image
    }

    #[test]
    fn parse_aic_image_reads_components() {
        let image = AicImage::parse(&build_image()).expect("parse");
        assert_eq!(image.platform, "d12p");
        assert_eq!(image.product, "d126-test");
        assert_eq!(image.media_type, "spi-nor");
        assert_eq!(image.metas.len(), 2);
        assert_eq!(image.metas[0].name, "image.updater.spl");
        assert_eq!(image.metas[1].partition, "os");
        assert_eq!(image.metas[1].size, 0x100);
    }

    #[test]
    fn replace_script_component_updates_payload_and_meta_crc() {
        let mut raw = build_image();
        let meta_start = IMAGE_HDR_SIZE + META_SIZE;
        raw[meta_start + META_OFF_NAME..meta_start + META_OFF_PARTITION].fill(0);
        copy_cstr(&mut raw[meta_start + META_OFF_NAME..meta_start + META_OFF_PARTITION], "image.target.scripts");
        copy_cstr(&mut raw[meta_start + META_OFF_PARTITION..meta_start + META_OFF_OFFSET], "scripts");

        let mut image = AicImage::parse(&raw).unwrap();
        let overlay = b"new-scripts";
        image.replace_script_component(overlay).unwrap();
        let out = image.into_bytes();

        let data_start = IMAGE_HDR_SIZE + META_SIZE * 2;
        assert_eq!(&out[data_start..data_start + overlay.len()], overlay);
        assert!(out[data_start + overlay.len()..data_start + 0x100].iter().all(|byte| *byte == 0xFF));

        let mut payload = vec![0xFFu8; 0x100];
        payload[..overlay.len()].copy_from_slice(overlay);
        let expected_crc = crc32fast::hash(&payload);
        assert_eq!(read_u32(&out, meta_start + META_OFF_CRC), expected_crc);
    }

    #[test]
    fn parse_rejects_short_or_wrong_magic() {
        assert!(AicImage::parse(&[0u8; 16]).is_err());
        let mut image = build_image();
        image[0] = b'X';
        assert!(AicImage::parse(&image).is_err());
    }

    #[test]
    fn command_header_checksum_matches_firmware() {
        let header = cmd_header(CMD_GET_BLOCK_SIZE, 0);
        assert_eq!(&header[0..4], b"UPGC");
        assert_eq!(header[4], 1);
        assert_eq!(header[5], 1);
        assert_eq!(header[6], CMD_GET_BLOCK_SIZE);
        let sum = read_u32(&header, 12);
        assert_eq!(sum, CMD_HEADER_MAGIC + 0x0011_0101);
    }

    #[test]
    fn chunk_size_is_block_aligned() {
        assert_eq!(chunk_size(2048), MAX_CHUNK);
        assert_eq!(chunk_size(4096), MAX_CHUNK);
        assert_eq!(chunk_size(MAX_CHUNK + 1), MAX_CHUNK + 1);
    }

    #[test]
    fn selector_parses_bus_and_port_chain() {
        let selector = UsbSelector::parse(Some("1:2-4-2")).unwrap().unwrap();
        assert_eq!(selector.bus, "1");
        assert_eq!(selector.port_chain, vec![2, 4, 2]);
        assert!(UsbSelector::parse(Some("auto")).unwrap().is_none());
        assert!(UsbSelector::parse(Some("bad")).is_err());
    }

    #[test]
    fn parse_mtd_layout_computes_offsets() {
        let parts = parse_mtd_layout("spi0.0:512k(spl),128k(env),1m(scripts),64k(fskv)").unwrap();
        assert_eq!(parts.len(), 4);
        assert_eq!(
            parts[0],
            D12pPartition {
                name: "spl".into(),
                offset: 0,
                size: 512 * 1024
            }
        );
        assert_eq!(parts[2].name, "scripts");
        assert_eq!(parts[2].offset, 512 * 1024 + 128 * 1024);
        assert_eq!(parts[2].size, 1024 * 1024);
        assert_eq!(parts[3].size, 64 * 1024);
    }

    #[test]
    fn media_partition_matches_device_layout() {
        let partition = media_partition("scripts", 0xD0_0000, 0x10_0000);
        assert_eq!(&partition[..7], b"spi-nor");
        assert_eq!(read_cstr(&partition[72..216]), "scripts");
        assert_eq!(u64::from_le_bytes(partition[224..232].try_into().unwrap()), 0xD0_0000);
        assert_eq!(u64::from_le_bytes(partition[232..240].try_into().unwrap()), 0x10_0000);
        assert_eq!(read_cstr(&partition[248..312]), "mtd");
    }

    #[test]
    fn synthetic_meta_has_expected_layout() {
        let meta = AicMeta::synthetic("image.target.scripts", "scripts", 1024, 0xabcd);
        assert_eq!(&meta.raw[0..4], b"META");
        assert_eq!(read_u32(&meta.raw, META_OFF_SIZE), 1024);
        assert_eq!(read_u32(&meta.raw, META_OFF_CRC), 0xabcd);
        assert_eq!(read_u32(&meta.raw, META_OFF_RAM), u32::MAX);
        assert_eq!(read_cstr(&meta.raw[META_OFF_ATTR..META_OFF_ATTR + 64]), "mtd;required");
    }
}
