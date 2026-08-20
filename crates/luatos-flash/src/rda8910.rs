// RDA8910 / UIS8910DM (Air724UG) 刷机协议实现。
//
// 分级下载（FDL）模型，三阶段：
//   1. ROM Boot Code → FDL1（RAM 0x8000C0，CRC16 校验）
//   2. FDL1 → FDL2（RAM 0x810000，CheckSum 校验）
//   3. FDL2 → SPI NOR Flash 分区（CheckSum 校验）
//
// 固件来源为 .soc 内的 RDA 私有 PAC 包（magic 0xFFFAFFFA），内含 FDL1/FDL2
// 及各分区镜像。协议细节见 docs/rda-flash-protocol.md。

use anyhow::{bail, Context, Result};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{FlashProgress, ProgressCallback};

// ─── 协议常量 ───────────────────────────────────────────────────────────────

const HDLC_FLAG: u8 = 0x7E;
const HDLC_ESCAPE: u8 = 0x7D;
const HDLC_ESCAPE_MASK: u8 = 0x20;

// 波特率（USB CDC 下为名义值，主机不实际切换）
const INIT_BAUD: u32 = 115_200;
/// FDL CHANGE_BAUD 指令下发的波特率值（经真机验证为 4000000）
const FDL_CHANGE_BAUD: u32 = 4_000_000;

// SPI NOR Flash 基址（FDL2 刷写时物理地址 = 基址 + 分区偏移）
const SPI_FLASH_BASE: u32 = 0x6000_0000;

// FDL 镜像 RAM 地址
const FDL1_RAM_ADDR: u32 = 0x8000C0;
const FDL2_RAM_ADDR: u32 = 0x810000;

// 数据包负载上限（分段下载时每包的数据域长度）
const PDL_CHUNK_LEN: usize = 2048; // PDL MID_DATA，ROM→FDL1
const FDL2_PACKET_LEN: usize = 0x210; // FDL1→FDL2，528B（经真机验证）
const FLASH_PACKET_LEN: usize = 0x3000; // FDL2→Flash，12288B

// 逻辑地址（FDL2 伪分区）
const LOGIC_ERASE_NV: u32 = 0xFE000001;
const LOGIC_PHASECHECK: u32 = 0xFE000002;
const LOGIC_FIXED_NV: u32 = 0xFE000003;
const LOGIC_PREPACK: u32 = 0xFE000004;

// PDL 协议（ROM→FDL1，0xAE 帧，无校验/转义）
const PDL_TAG: u8 = 0xAE;
const PDL_FLOWID: u8 = 0xFF;
const PDL_CMD_CONNECT: u32 = 0;
const PDL_CMD_START_DATA: u32 = 4;
const PDL_CMD_MID_DATA: u32 = 5;
const PDL_CMD_END_DATA: u32 = 6;
const PDL_CMD_EXEC_DATA: u32 = 7;
const PDL_RESP_ACK: u32 = 0;
/// PDL END_DATA 尾随的 4 字节（作用未明，真机按此值通过）
const PDL_END_DATA_TAIL: [u8; 4] = [0x1c, 0x3c, 0x6e, 0x06];

// BSL 指令（FDL 协议，0x7E 帧 + CheckSum）
const BSL_CMD_CONNECT: u16 = 0x00;
const BSL_CMD_START_DATA: u16 = 0x01;
const BSL_CMD_MIDST_DATA: u16 = 0x02;
const BSL_CMD_END_DATA: u16 = 0x03;
const BSL_CMD_EXEC_DATA: u16 = 0x04;
const BSL_CMD_NORMAL_RESET: u16 = 0x05;
const BSL_CMD_CHANGE_BAUD: u16 = 0x09;

// BSL 应答
const BSL_REP_ACK: u16 = 0x80;
const BSL_REP_VER: u16 = 0x81;
const BSL_REP_INVALID_CMD: u16 = 0x82;
const BSL_REP_OPERATION_FAILED: u16 = 0x84;
const BSL_REP_NOT_SUPPORT_BAUDRATE: u16 = 0x85;
const BSL_REP_DOWN_NOT_START: u16 = 0x86;
const BSL_REP_DOWN_DEST_ERROR: u16 = 0x89;
const BSL_REP_DOWN_SIZE_ERROR: u16 = 0x8A;
const BSL_REP_VERIFY_ERROR: u16 = 0x8B;
const BSL_REP_SEC_VERIFY_ERROR: u16 = 0xAA;

// PAC 文件 ID
const PAC_ID_FDL1: &str = "HOST_FDL";
const PAC_ID_FDL2: &str = "FDL2";

// ─── PAC 解析 ────────────────────────────────────────────────────────────────

/// PAC 文件头大小（`struct.pack("48sI512s512s7I200s3I800sI2H")`，小端）。
const PAC_HEADER_SIZE: usize = 2124;
/// PAC 文件条目头大小（`struct.pack("I512s512s512s6I5I996s")`，小端）。
const PAC_FILE_HEADER_SIZE: usize = 2580;
const PAC_MAGIC: u32 = 0xFFFAFFFA;

/// PAC 内单个文件条目。
#[derive(Debug, Clone)]
pub struct PacFileEntry {
    pub id: String,
    pub name: String,
    pub size: u32,
    pub addr: u32,
    pub flag: u32,
    pub omit: u32,
    pub check: u32,
    pub data: Option<Vec<u8>>,
}

/// 解析后的 PAC 文件。
#[derive(Debug)]
pub struct PacFile {
    pub version: String,
    pub prd_name: String,
    pub prd_version: String,
    pub files: Vec<PacFileEntry>,
}

impl PacFile {
    /// 按 szFileID 查找条目。
    pub fn find(&self, id: &str) -> Option<&PacFileEntry> {
        self.files.iter().find(|f| f.id == id)
    }
}

/// 把定长 UTF-16LE 字段解码为 String（遇到首个 NUL 停止）。
fn dec_utf16le(b: &[u8]) -> String {
    let mut units = Vec::with_capacity(b.len() / 2);
    for chunk in b.chunks(2) {
        if chunk.len() < 2 {
            break;
        }
        let u = u16::from_le_bytes([chunk[0], chunk[1]]);
        if u == 0 {
            break;
        }
        units.push(u);
    }
    String::from_utf16_lossy(&units)
}

/// 解析 RDA PAC 私有格式（`pacgen.py` 生成，整数域小端、字符串 UTF-16LE）。
pub fn parse_pac(data: &[u8]) -> Result<PacFile> {
    if data.len() < PAC_HEADER_SIZE {
        bail!("PAC 数据过小: {} bytes", data.len());
    }

    let mut o = 0usize;
    let version = dec_utf16le(&data[o..o + 48]);
    o += 48;
    let pac_size = u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
    o += 4;
    let prd_name = dec_utf16le(&data[o..o + 512]);
    o += 512;
    let prd_version = dec_utf16le(&data[o..o + 512]);
    o += 512;
    let file_count = u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
    o += 4;
    let file_offset = u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
    o += 4;
    o += 5 * 4; // dwMode / dwFlashType / dwNandStrategy / dwIsNvBackup / dwNandPageType
    let _prd_alias = dec_utf16le(&data[o..o + 200]);
    o += 200;
    o += 3 * 4; // dwOmaDmProductFlag / dwIsOmaDm / dwIsPreload
    o += 800; // reserved
    let magic = u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
    o += 4;
    o += 4; // crc1(2) + crc2(2)
    debug_assert_eq!(o, PAC_HEADER_SIZE, "PAC 头解析偏移不一致");

    if magic != PAC_MAGIC {
        bail!("非法 PAC magic: 0x{magic:08X}, 期望 0xFFFAFFFA");
    }
    if data.len() as u32 != pac_size {
        log::warn!("PAC 实际长度 {} 与头声明的 {} 不一致", data.len(), pac_size);
    }

    let mut files = Vec::new();
    let mut cursor = file_offset as usize;
    for _ in 0..file_count {
        if cursor + PAC_FILE_HEADER_SIZE > data.len() {
            bail!("PAC 文件头越界 (cursor=0x{cursor:x})");
        }
        let fh = &data[cursor..cursor + PAC_FILE_HEADER_SIZE];
        // hdr_size(4) + szFileID(512) + szFileName(512) + reserved(512)
        let id = dec_utf16le(&fh[4..516]);
        let name = dec_utf16le(&fh[516..1028]);
        let file_size = u32::from_le_bytes(fh[1540..1544].try_into().unwrap());
        let flag = u32::from_le_bytes(fh[1544..1548].try_into().unwrap());
        let default_check = u32::from_le_bytes(fh[1548..1552].try_into().unwrap());
        let data_offset = u32::from_le_bytes(fh[1552..1556].try_into().unwrap());
        let omit = u32::from_le_bytes(fh[1556..1560].try_into().unwrap());
        let addr = u32::from_le_bytes(fh[1564..1568].try_into().unwrap());
        cursor += PAC_FILE_HEADER_SIZE;

        let entry_data = {
            let start = data_offset as usize;
            let end = start + file_size as usize;
            if end <= data.len() {
                Some(data[start..end].to_vec())
            } else {
                log::warn!("PAC 条目 {} 数据越界 (offset=0x{start:x} size={file_size})", id);
                None
            }
        };

        log::debug!("PAC entry: id={} name={} size={} addr=0x{addr:08X}", id, name, file_size);
        files.push(PacFileEntry {
            id,
            name,
            size: file_size,
            addr,
            flag,
            omit,
            check: default_check,
            data: entry_data,
        });
    }

    Ok(PacFile {
        version,
        prd_name,
        prd_version,
        files,
    })
}

// ─── 校验算法 ───────────────────────────────────────────────────────────────

/// 计算 FDL 阶段使用的 CheckSum：按 16 位小端字累加 + 折叠 + 取反。
fn fdl_checksum(pkt: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    for chunk in pkt.chunks(2) {
        sum += if chunk.len() == 2 {
            u16::from_le_bytes([chunk[0], chunk[1]]) as u32
        } else {
            chunk[0] as u32
        };
    }
    sum = (sum >> 16) + (sum & 0xffff);
    sum += sum >> 16;
    (!sum) as u16
}

// ─── FDL HDLC 帧 ─────────────────────────────────────────────────────────────

/// 组 FDL 帧（不含尾 0x7E）：type/size 大端 + CheckSum 小端追加 + HDLC 转义。
/// 尾 0x7E 由调用方单独发送（抓包验证 FDL2 写 Flash 的 MIDST 帧需如此）。
fn fdl_frame_no_tail(pkt_type: u16, data: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(4 + data.len() + 2);
    body.extend_from_slice(&pkt_type.to_be_bytes());
    body.extend_from_slice(&(data.len() as u16).to_be_bytes());
    body.extend_from_slice(data);
    let crc = fdl_checksum(&body);
    body.push((crc & 0xff) as u8); // 小端追加
    body.push((crc >> 8) as u8);

    let mut frame = Vec::with_capacity(body.len() + 1);
    frame.push(HDLC_FLAG);
    for b in body {
        if b == HDLC_FLAG || b == HDLC_ESCAPE {
            frame.push(HDLC_ESCAPE);
            frame.push(b ^ HDLC_ESCAPE_MASK);
        } else {
            frame.push(b);
        }
    }
    frame
}

/// 组完整 FDL 帧（含首尾 0x7E）。
fn fdl_frame(pkt_type: u16, data: &[u8]) -> Vec<u8> {
    let mut frame = fdl_frame_no_tail(pkt_type, data);
    frame.push(HDLC_FLAG);
    frame
}

/// 逐字节解包状态机（镜像芯片 `fdlEngineProcess`）。校验通过后返回 (type, data)。
struct HdlcReceiver {
    state: RxState,
    buf: Vec<u8>,
}

#[derive(PartialEq)]
enum RxState {
    Start,
    Run,
    Escape,
    End,
}

impl HdlcReceiver {
    fn new() -> Self {
        Self {
            state: RxState::Start,
            buf: Vec::with_capacity(64),
        }
    }

    /// 喂入一个字节，若收到完整且校验通过的包则返回 (type, data)。
    fn feed(&mut self, b: u8) -> Option<(u16, Vec<u8>)> {
        match self.state {
            RxState::Start => {
                if b == HDLC_FLAG {
                    self.state = RxState::Run;
                    self.buf.clear();
                }
            }
            RxState::End => {
                if b == HDLC_FLAG {
                    let result = self.finish_packet();
                    self.state = RxState::Run;
                    self.buf.clear();
                    return result;
                }
                self.state = RxState::Start;
            }
            RxState::Run => {
                if b == HDLC_FLAG {
                    self.buf.clear();
                } else if b == HDLC_ESCAPE {
                    self.state = RxState::Escape;
                } else {
                    self.buf.push(b);
                    self.after_byte();
                }
            }
            RxState::Escape => {
                self.buf.push(b ^ HDLC_ESCAPE_MASK);
                self.state = RxState::Run;
                self.after_byte();
            }
        }
        None
    }

    /// 当缓冲区达到「4 头 + size + 2 校验」长度时进入 End 等待定界符。
    fn after_byte(&mut self) {
        const HEAD: usize = 4;
        if self.buf.len() >= HEAD + 2 {
            let size = u16::from_be_bytes([self.buf[2], self.buf[3]]) as usize;
            if self.buf.len() == HEAD + size + 2 {
                self.state = RxState::End;
            }
        }
    }

    /// 校验并提取完整包。
    fn finish_packet(&mut self) -> Option<(u16, Vec<u8>)> {
        let len = self.buf.len();
        if len < 6 {
            return None;
        }
        let got = u16::from_le_bytes([self.buf[len - 2], self.buf[len - 1]]);
        if fdl_checksum(&self.buf[..len - 2]) != got {
            log::warn!("RDA 接收 FDL 包校验失败 (len={})", len);
            return None;
        }
        let pkt_type = u16::from_be_bytes([self.buf[0], self.buf[1]]);
        let size = u16::from_be_bytes([self.buf[2], self.buf[3]]) as usize;
        let data = self.buf[4..4 + size].to_vec();
        Some((pkt_type, data))
    }
}

// ─── 串口交互 ───────────────────────────────────────────────────────────────

fn open_serial(port_name: &str, baud: u32) -> Result<Box<dyn serialport::SerialPort>> {
    serialport::new(port_name, baud)
        .timeout(Duration::from_millis(100))
        .open()
        .with_context(|| format!("无法打开串口 {port_name}"))
}

/// 发送 FDL 帧（整帧一次写，含转义）。
fn fdl_send(serial: &mut Box<dyn serialport::SerialPort>, cmd: u16, data: &[u8]) -> Result<()> {
    let frame = fdl_frame(cmd, data);
    serial.write_all(&frame).with_context(|| format!("写串口失败 (type=0x{cmd:02X})"))
}

/// 接收一个完整 FDL 包，直到 deadline。校验失败的重置状态继续等待。
fn fdl_recv(serial: &mut Box<dyn serialport::SerialPort>, deadline: Instant) -> Result<(u16, Vec<u8>)> {
    let mut rx = HdlcReceiver::new();
    let mut buf = [0u8; 1024];
    while Instant::now() < deadline {
        match serial.read(&mut buf) {
            Ok(n) if n > 0 => {
                for &b in &buf[..n] {
                    if let Some(pkt) = rx.feed(b) {
                        return Ok(pkt);
                    }
                }
            }
            Ok(_) => {}
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => return Err(e).context("读串口失败"),
        }
    }
    bail!("等待设备应答超时")
}

/// 把应答码转为人类可读描述（用于诊断）。
fn describe_reply(code: u16) -> &'static str {
    match code {
        BSL_REP_ACK => "ACK 成功",
        BSL_REP_VER => "版本应答",
        BSL_REP_INVALID_CMD => "无效指令",
        BSL_REP_OPERATION_FAILED => "操作失败",
        BSL_REP_NOT_SUPPORT_BAUDRATE => "波特率不支持",
        BSL_REP_DOWN_NOT_START => "未开始下载",
        BSL_REP_DOWN_DEST_ERROR => "目标地址错误",
        BSL_REP_DOWN_SIZE_ERROR => "下载大小超限",
        BSL_REP_VERIFY_ERROR => "数据校验错误",
        BSL_REP_SEC_VERIFY_ERROR => "安全启动校验失败",
        _ => "未知应答",
    }
}

/// 发送 FDL 指令并期待指定应答类型。
fn fdl_cmd_expect(serial: &mut Box<dyn serialport::SerialPort>, cmd: u16, data: &[u8], expect: u16, timeout: Duration) -> Result<Vec<u8>> {
    fdl_send(serial, cmd, data)?;
    let pkt = fdl_recv(serial, Instant::now() + timeout)?;
    if pkt.0 != expect {
        bail!("指令 0x{cmd:02X} 应答异常: type=0x{:02X} ({})，期望 0x{expect:02X}", pkt.0, describe_reply(pkt.0));
    }
    Ok(pkt.1)
}

/// FDL 同步：发送裸 0x7E，收到 BSL_REP_VER（版本串）。失败重试。
fn fdl_sync_version(serial: &mut Box<dyn serialport::SerialPort>) -> Result<Vec<u8>> {
    for attempt in 0..10 {
        serial.write_all(&[HDLC_FLAG]).context("发送 0x7E 同步失败")?;
        match fdl_recv(serial, Instant::now() + Duration::from_millis(500)) {
            Ok((BSL_REP_VER, data)) => {
                log::info!("RDA 版本: {}", String::from_utf8_lossy(&data).trim_matches('\0'));
                return Ok(data);
            }
            Ok((t, _)) => bail!("CHECK_BAUD 收到非 VER 应答: 0x{t:02X}"),
            Err(_) if attempt < 9 => continue,
            Err(e) => return Err(e).context("CHECK_BAUD 同步失败"),
        }
    }
    bail!("CHECK_BAUD 同步失败")
}

/// FDL 分段传输：START_DATA → MIDST_DATA×N → END_DATA，每步期待 ACK。
fn fdl_transfer(serial: &mut Box<dyn serialport::SerialPort>, addr: u32, data: &[u8], chunk_len: usize) -> Result<()> {
    // 数据长度必须为偶数（协议要求），奇数据末尾补 0
    let data = if data.len().is_multiple_of(2) {
        data.to_vec()
    } else {
        let mut v = data.to_vec();
        v.push(0);
        v
    };

    let mut start = Vec::with_capacity(8);
    start.extend_from_slice(&addr.to_be_bytes());
    start.extend_from_slice(&(data.len() as u32).to_be_bytes());
    fdl_cmd_expect(serial, BSL_CMD_START_DATA, &start, BSL_REP_ACK, Duration::from_secs(10))?;

    for chunk in data.chunks(chunk_len) {
        // MIDST：帧(无尾 0x7E) + 单独 0x7E 分开发送（抓包验证 FDL2 写 Flash 需如此）
        let frame = fdl_frame_no_tail(BSL_CMD_MIDST_DATA, chunk);
        serial.write_all(&frame)?;
        serial.write_all(&[HDLC_FLAG])?;
        let pkt = fdl_recv(serial, Instant::now() + Duration::from_secs(30))?;
        if pkt.0 != BSL_REP_ACK {
            bail!("指令 0x{BSL_CMD_MIDST_DATA:02X} 应答异常: type=0x{:02X} ({})", pkt.0, describe_reply(pkt.0));
        }
    }

    fdl_cmd_expect(serial, BSL_CMD_END_DATA, &[], BSL_REP_ACK, Duration::from_secs(60))?;
    Ok(())
}

// ─── PDL 协议（ROM→FDL1，0xAE 帧）────────────────────────────────────────────

/// 写 PDL 帧头（单独一次 USB write——模块按 USB 传输边界解析，帧头与负载必须分开）。
fn pdl_send_header(serial: &mut Box<dyn serialport::SerialPort>, payload_len: u32) -> Result<()> {
    let mut hdr = Vec::with_capacity(8);
    hdr.push(PDL_TAG);
    hdr.extend_from_slice(&payload_len.to_le_bytes());
    hdr.extend_from_slice(&[PDL_FLOWID, 0, 0]);
    serial.write_all(&hdr).context("写 PDL 帧头失败")
}

/// 写 PDL 负载（单独一次 USB write）。
fn pdl_send_payload(serial: &mut Box<dyn serialport::SerialPort>, payload: &[u8]) -> Result<()> {
    serial.write_all(payload).context("写 PDL 负载失败")
}

/// 发送 PDL 帧：帧头 + 负载分开 write。
fn pdl_send(serial: &mut Box<dyn serialport::SerialPort>, payload: &[u8]) -> Result<()> {
    pdl_send_header(serial, payload.len() as u32)?;
    pdl_send_payload(serial, payload)
}

/// 读取 PDL 应答，返回 (flowid, resp)。
fn pdl_recv(serial: &mut Box<dyn serialport::SerialPort>) -> Result<(u8, u32)> {
    let mut buf = [0u8; 16];
    let mut n = 0usize;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let mut byte = [0u8; 1];
        if let Ok(1) = serial.read(&mut byte) {
            if n == 0 && byte[0] != PDL_TAG {
                continue; // 跳过非帧头字节
            }
            buf[n] = byte[0];
            n += 1;
            if n >= 12 {
                let flowid = buf[5];
                let resp = u32::from_le_bytes(buf[8..12].try_into().unwrap());
                return Ok((flowid, resp));
            }
        }
    }
    bail!("等待 PDL 应答超时")
}

/// 发送 PDL 指令并期待 ACK。
fn pdl_cmd_expect(serial: &mut Box<dyn serialport::SerialPort>, cmd: u32, extra: &[u8]) -> Result<()> {
    let mut payload = Vec::with_capacity(4 + extra.len());
    payload.extend_from_slice(&cmd.to_le_bytes());
    payload.extend_from_slice(extra);
    pdl_send(serial, &payload)?;
    let (flowid, resp) = pdl_recv(serial)?;
    if flowid != PDL_FLOWID || resp != PDL_RESP_ACK {
        bail!("PDL 指令 {cmd} 应答异常: flowid=0x{flowid:02X} resp={resp}");
    }
    Ok(())
}

/// PDL 阶段：下载 FDL1 到 RAM（ROM→FDL1）。
fn pdl_download_fdl1(serial: &mut Box<dyn serialport::SerialPort>, fdl1: &[u8]) -> Result<()> {
    // CONNECT
    pdl_cmd_expect(serial, PDL_CMD_CONNECT, &[0u8; 8])?;
    // START_DATA（含 "PDL1" 标记）
    let mut start = Vec::with_capacity(13);
    start.extend_from_slice(&FDL1_RAM_ADDR.to_le_bytes());
    start.extend_from_slice(&(fdl1.len() as u32).to_le_bytes());
    start.extend_from_slice(b"PDL1\0");
    pdl_cmd_expect(serial, PDL_CMD_START_DATA, &start)?;
    // MID_DATA 分块（2048/块；帧头+cmd+数据全发后再 ACK）
    for (idx, chunk) in fdl1.chunks(PDL_CHUNK_LEN).enumerate() {
        let mut mid = Vec::with_capacity(12 + chunk.len());
        mid.extend_from_slice(&PDL_CMD_MID_DATA.to_le_bytes());
        mid.extend_from_slice(&(idx as u32).to_le_bytes());
        mid.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
        mid.extend_from_slice(chunk);
        pdl_send_header(serial, (12 + chunk.len()) as u32)?;
        pdl_send_payload(serial, &mid)?;
        let (flowid, resp) = pdl_recv(serial)?;
        if flowid != PDL_FLOWID || resp != PDL_RESP_ACK {
            bail!("PDL MID_DATA[{idx}] 应答异常: flowid=0x{flowid:02X} resp={resp}");
        }
    }
    // END_DATA
    let mut end = Vec::with_capacity(16);
    end.extend_from_slice(&[0u8; 8]);
    end.extend_from_slice(&PDL_END_DATA_TAIL);
    pdl_cmd_expect(serial, PDL_CMD_END_DATA, &end)?;
    // EXEC_DATA（无 ACK，FDL1 启动）
    let mut exec = Vec::with_capacity(12);
    exec.extend_from_slice(&PDL_CMD_EXEC_DATA.to_le_bytes());
    exec.extend_from_slice(&[0u8; 8]);
    pdl_send(serial, &exec)?;
    Ok(())
}

// ─── 刷机流程 ────────────────────────────────────────────────────────────────

fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        bail!("操作已取消");
    }
    Ok(())
}

/// 下载 FDL1（PDL 协议）→ FDL2（FDL 协议），完成后 FDL2 已运行待命。
/// 只触碰 RAM、不写 Flash。返回时 FDL2 就绪，可继续刷分区或直接收尾。
fn load_fdls(serial: &mut Box<dyn serialport::SerialPort>, fdl1: &PacFileEntry, fdl2: &PacFileEntry, cancel: &AtomicBool) -> Result<()> {
    let fdl1_data = fdl1.data.as_deref().context("PAC 缺少 HOST_FDL 数据")?;
    let fdl2_data = fdl2.data.as_deref().context("PAC 缺少 FDL2 数据")?;

    // 阶段一：ROM → FDL1（PDL 协议）
    check_cancel(cancel)?;
    pdl_download_fdl1(serial, fdl1_data)?;

    // FDL1 启动后：0x7E 同步 → 版本
    check_cancel(cancel)?;
    fdl_sync_version(serial)?;

    // 阶段二：FDL1 → FDL2（FDL 协议，0x7E + CheckSum + 转义）
    fdl_cmd_expect(serial, BSL_CMD_CONNECT, &[], BSL_REP_ACK, Duration::from_secs(2))?;
    // CHANGE_BAUD（4000000；USB CDC 下为名义值，主机不切换波特率）
    fdl_cmd_expect(serial, BSL_CMD_CHANGE_BAUD, &FDL_CHANGE_BAUD.to_be_bytes(), BSL_REP_ACK, Duration::from_secs(2))?;
    // 二次 CONNECT
    fdl_cmd_expect(serial, BSL_CMD_CONNECT, &[], BSL_REP_ACK, Duration::from_secs(2))?;
    // START_DATA（FDL2 到 RAM 0x810000）
    if fdl2.addr != FDL2_RAM_ADDR {
        log::warn!("PAC 中 FDL2 地址 0x{:08X} 与预期 0x{FDL2_RAM_ADDR:08X} 不一致", fdl2.addr);
    }
    fdl_transfer(serial, FDL2_RAM_ADDR, fdl2_data, FDL2_PACKET_LEN)?;
    // EXEC 启动 FDL2（FDL2 启动即回 ACK）
    fdl_cmd_expect(serial, BSL_CMD_EXEC_DATA, &[], BSL_REP_ACK, Duration::from_secs(5))?;
    Ok(())
}

/// FDL2 就绪后收尾：重启（NORMAL_RESET），设备立即运行新固件并输出日志。
fn fdl_reboot(serial: &mut Box<dyn serialport::SerialPort>) -> Result<()> {
    fdl_cmd_expect(serial, BSL_CMD_NORMAL_RESET, &[], BSL_REP_ACK, Duration::from_secs(2))?;
    Ok(())
}

/// 解析 SOC→PAC，取出 FDL1/FDL2，打开串口并完成两阶段下载（RAM 0x8000C0 / 0x810000）。
/// 只触碰 RAM、不写 Flash。返回已就绪的串口（921600）、PAC 与 info。
fn open_and_load_fdls(
    soc_path: &str,
    port_name: &str,
    on_progress: &ProgressCallback,
    cancel: &AtomicBool,
) -> Result<(Box<dyn serialport::SerialPort>, PacFile, luatos_soc::SocInfo)> {
    on_progress(&FlashProgress::info("Preparing", 0.0, "解析 SOC 固件包与 PAC"));

    let tmp = tempfile::tempdir().context("创建临时目录失败")?;
    let unpacked = luatos_soc::unpack_soc(soc_path, tmp.path())?;
    let info = unpacked.info.clone();
    let pac_data = std::fs::read(&unpacked.rom_path).with_context(|| format!("读取 PAC 失败: {}", unpacked.rom_path.display()))?;
    let pac = parse_pac(&pac_data).context("解析 PAC 失败")?;

    let fdl1 = pac.find(PAC_ID_FDL1).context("PAC 缺少 HOST_FDL")?.clone();
    let fdl2 = pac.find(PAC_ID_FDL2).context("PAC 缺少 FDL2")?.clone();

    on_progress(&FlashProgress::info("Connecting", 5.0, "打开串口 115200"));
    let mut serial = open_serial(port_name, INIT_BAUD)?;
    load_fdls(&mut serial, &fdl1, &fdl2, cancel)?;
    Ok((serial, pac, info))
}

// ─── 下载口自动探测（--port auto）──────────────────────────────────────────────

/// 下载模式 USB VID/PID（SPRD U2S Diag，上电按键进入）。
pub const DOWNLOAD_VID: u16 = 0x0525;
pub const DOWNLOAD_PID: u16 = 0xa4a7;

/// 判断端口类型是否为 RDA8910 下载口（SPRD U2S Diag，VID 0525:PID a4a7）。
fn is_download_port(port_type: &serialport::SerialPortType) -> bool {
    matches!(port_type, serialport::SerialPortType::UsbPort(usb) if usb.vid == DOWNLOAD_VID && usb.pid == DOWNLOAD_PID)
}

/// 查找已处于下载模式的串口（按 USB VID/PID）。
pub fn find_download_port() -> Option<String> {
    let ports = serialport::available_ports().ok()?;
    for port in &ports {
        if is_download_port(&port.port_type) {
            return Some(port.port_name.clone());
        }
    }
    None
}

/// 轮询等待下载口出现（按键进入下载模式后 USB 重新枚举），每 100ms 查一次。
pub fn wait_for_download_port(timeout_secs: u32) -> Option<String> {
    for _ in 0..(timeout_secs * 10) {
        if let Some(port) = find_download_port() {
            return Some(port);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

// ─── 运行模式口探测（软重启进下载）──────────────────────────────────────────────

/// 运行模式 USB VID/PID（UNISOC-8910 复合设备，实测枚举 5 个同身份串口）。
pub const RUNNING_VID: u16 = 0x1782;
pub const RUNNING_PID: u16 = 0x4d11;
/// 运行模式备用 PID（不同批次/固件枚举差异，luatools_py3 口表同列）。
const RUNNING_PID_ALT: u16 = 0x4e00;

/// 判断端口类型是否为 RDA8910 运行模式口（VID 1782，PID 4d11 / 4e00）。
fn is_running_port(port_type: &serialport::SerialPortType) -> bool {
    matches!(
        port_type,
        serialport::SerialPortType::UsbPort(usb) if usb.vid == RUNNING_VID && (usb.pid == RUNNING_PID || usb.pid == RUNNING_PID_ALT)
    )
}

/// 判断某运行口是否为指定 USB 接口号（luatools_py3 用 Windows 位置 "x.N" 匹配，Linux 等价于 bInterfaceNumber）。
fn port_interface_is(port: &serialport::SerialPortInfo, n: u8) -> bool {
    matches!(&port.port_type, serialport::SerialPortType::UsbPort(usb) if usb.interface == Some(n))
}

/// 列出运行模式的所有串口（复合设备可能含 AT/DIAG/Modem 等多个接口）。
pub fn find_running_ports() -> Vec<String> {
    serialport::available_ports()
        .map(|ports| ports.iter().filter(|p| is_running_port(&p.port_type)).map(|p| p.port_name.clone()).collect())
        .unwrap_or_default()
}

/// 按 USB 接口号筛选运行口（host fast 命令口 x.6、DIAG 命令口 x.3 等）。
fn running_ports_by_interface(n: u8) -> Vec<String> {
    serialport::available_ports()
        .map(|ports| {
            ports
                .iter()
                .filter(|p| is_running_port(&p.port_type) && port_interface_is(p, n))
                .map(|p| p.port_name.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// 判断某运行口是否为 RDA8910 的 log 口（按 VID/PID + 接口号，对齐 luatools_py3 `usb_device.py` 端口表）。
///
/// UNISOC-8910 复合设备各接口功能（luatools_py3 用 Windows 位置 "x.N" 匹配，Linux 等价 bInterfaceNumber）：
///   - PID 0x4d11 (8910_soc): soc_log=x.6, cp_log=x.5, user_com=x.2
///   - PID 0x4e00 (8910_air): user_log=x.2, at_log=x.3, ap_log=x.4, cp_log=x.5
///
/// log 口与"host fast 重启进下载"命令口为同一接口（0x4d11→x.6、0x4e00→x.2），
/// 与 `try_reboot_to_download` 的接口选择一致。
fn port_is_log(port: &serialport::SerialPortInfo) -> bool {
    matches!(
        &port.port_type,
        serialport::SerialPortType::UsbPort(usb)
            if usb.vid == RUNNING_VID
                && ((usb.pid == RUNNING_PID && usb.interface == Some(6))
                    || (usb.pid == RUNNING_PID_ALT && usb.interface == Some(2)))
    )
}

/// 判断指定串口名是否为 RDA8910 运行模式口（VID 1782 复合设备）。
/// 用于 `log view` 手动指定口时决定是否走 host trace 二进制解码路径。
pub fn is_running_port_name(port_name: &str) -> bool {
    serialport::available_ports()
        .map(|ports| ports.iter().any(|p| p.port_name == port_name && is_running_port(&p.port_type)))
        .unwrap_or(false)
}

/// 查找 RDA8910 运行模式的 log 口（对齐 ec7xx `find_ec718_log_port` 的 VID/PID + 接口号匹配）。
///
/// 刷机后模组重启会重新枚举为运行口，此时下载口消失，需用本函数重新定位 log 口。
/// 仅做接口号精确匹配：USB 复合设备多个同身份串口只能靠接口号区分，行为探测（发 AT）
/// 可能命中 AT 口而非 log 口，且每次阻塞数百毫秒，不适合放在轮询热路径里。
pub fn find_rda8910_log_port() -> Option<String> {
    let ports = serialport::available_ports().ok()?;
    for port in &ports {
        if port_is_log(port) {
            log::info!("发现 RDA8910 log 口: {}", port.port_name);
            return Some(port.port_name.clone());
        }
    }
    None
}

/// 轮询等待 RDA8910 运行模式 log 口出现（刷机后模组重启重新枚举，每 100ms 查一次）。
///
/// 快速路径为接口号精确匹配；超时后打印运行口接口号便于诊断，并做**一次** AT 行为探测
/// 兜底（与 log 口同设备组，接口号缺失时至少返回一个可用口）。对齐 ec7xx `wait_for_log_port`。
pub fn wait_for_rda8910_log_port(timeout_secs: u32) -> Option<String> {
    for _ in 0..(timeout_secs * 10) {
        if let Some(port) = find_rda8910_log_port() {
            return Some(port);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // 诊断：列出运行口及其接口号，便于排查接口号缺失或布局差异
    if let Ok(ports) = serialport::available_ports() {
        let running: Vec<String> = ports
            .iter()
            .filter(|p| is_running_port(&p.port_type))
            .map(|p| {
                let iface = match &p.port_type {
                    serialport::SerialPortType::UsbPort(usb) => format!("{:?}", usb.interface),
                    _ => "?".into(),
                };
                format!("{}(iface={})", p.port_name, iface)
            })
            .collect();
        if !running.is_empty() {
            log::warn!("RDA8910 运行口未命中 log 口: {}", running.join(", "));
        }
    }
    // 兜底：只做一次，避免在轮询热路径里逐口阻塞数秒
    log::warn!("RDA8910 log 口接口号未命中, 尝试 AT 行为探测");
    find_running_at_port()
}

// ─── 软重启进下载指令 ──────────────────────────────────────────────────────────

/// host fast "重启进下载" 帧：luatools_py3 `pyMakeHostPacket(0xfd, 3, 0,0,0, 0x5244, 0)` 的产物。
/// 字节值取协议文档 rda-flash-protocol.md 硬件验证的通用切换指令 `AD 00 04 FD 03 44 52 E8`：
///   AD(同步) 00 04(长度) FD(host data) 03(cmd) 44 52(param 0x5244 LE="RD") E8(校验)
const HOST_FAST_REBOOT_FRAME: &[u8] = &[0xAD, 0x00, 0x04, 0xFD, 0x03, 0x44, 0x52, 0xE8];

/// DIAG 重启进下载帧（luatools_py3 `aio_send_diag_cmd(5, 0x42)`，HDLC 0x7E 包裹）：
///   `struct.pack("<IHBB", queue=1, len=8, cmd=DIAG_SYSTEM_F=5, sub=DIAG_REBOOT_DOWNLOAD_MS=0x42)`
///   帧 = 0x7E + [queue u32le][len u32le][cmd][sub] + 0x7E
const DIAG_REBOOT_FRAME: &[u8] = &[0x7E, 0x01, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x05, 0x42, 0x7E];

/// 向某串口写入一段数据并关闭，返回是否成功写入。
fn send_bytes(port_name: &str, data: &[u8]) -> bool {
    let mut port = match open_serial(port_name, INIT_BAUD) {
        Ok(p) => p,
        Err(e) => {
            log::warn!("打开串口 {} 失败: {}", port_name, e);
            return false;
        }
    };
    let ok = port.write_all(data).is_ok() && port.flush().is_ok();
    std::thread::sleep(Duration::from_millis(200));
    drop(port);
    ok
}

/// 判断缓冲里是否出现 "OK" 应答（AT 口特征）。
fn at_ok_received(acc: &[u8]) -> bool {
    acc.windows(2).any(|w| w == b"OK")
}

/// 探测某串口是否为 AT 口：发送 `AT\r\n`，在超时内收到含 "OK" 的应答即视为。
///
/// USB CDC 下波特率为名义值，用 115200 即可。
fn port_responds_to_at(port_name: &str, timeout: Duration) -> bool {
    let mut port = match open_serial(port_name, INIT_BAUD) {
        Ok(p) => p,
        Err(_) => return false,
    };
    if port.write_all(b"AT\r\n").is_err() {
        return false;
    }
    let _ = port.flush();
    let mut buf = [0u8; 256];
    let mut acc = Vec::with_capacity(64);
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match port.read(&mut buf) {
            Ok(n) if n > 0 => {
                acc.extend_from_slice(&buf[..n]);
                if at_ok_received(&acc) {
                    return true;
                }
            }
            Ok(_) => {}
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => return false,
        }
    }
    false
}

/// 在运行模式的多个串口中找到 AT 口（同一 VID/PID 无法区分，需行为探测）。
pub fn find_running_at_port() -> Option<String> {
    for port in find_running_ports() {
        log::debug!("探测 {} 是否为 AT 口...", port);
        if port_responds_to_at(&port, Duration::from_millis(800)) {
            return Some(port);
        }
    }
    None
}

/// 向运行中模组发送"重启进下载"指令序列（对齐 luatools_py3 的三级软重启）。
///
/// 优先级：
///   1. host fast 命令口（0x4d11 → 接口 x.6；0x4e00 → 接口 x.2），发 `AD 00 04 FD 03 44 52 E8`
///   2. DIAG 命令口（0x4d11 → 接口 x.3），发 0x7E HDLC 帧
///   3. 接口信息缺失/未命中 → 向全部运行口广播上述帧（宁可多发，不可漏发）
///   4. AT 兜底：行为探测 AT 口，发 `AT*DOWNLOAD=1`
///
/// 返回是否成功发出任一指令（模组随后会复位并重新枚举为下载口 VID 0525:a4a7）。
pub fn try_reboot_to_download(on_progress: &ProgressCallback) -> bool {
    let running = find_running_ports();
    if running.is_empty() {
        log::info!("未发现运行中的 RDA8910 模组 (VID 1782:PID 4d11/4e00)，模组可能已处于下载模式或未连接");
        return false;
    }
    on_progress(&FlashProgress::info(
        "Rebooting",
        2.0,
        &format!("发现运行中的模组 ({} 个串口), 发送重启进下载指令...", running.len()),
    ));

    // 接口号匹配：host fast x.6 / x.2，DIAG x.3
    let mut host_fast_targets = running_ports_by_interface(6);
    host_fast_targets.extend(running_ports_by_interface(2));
    let diag_targets = running_ports_by_interface(3);

    // 接口信息缺失或未命中 → 向全部运行口广播兜底
    let (host_fast_targets, diag_targets) = if host_fast_targets.is_empty() && diag_targets.is_empty() {
        (running.clone(), running.clone())
    } else {
        (host_fast_targets, diag_targets)
    };

    let mut sent = false;
    for p in &host_fast_targets {
        log::info!("{} → host fast 帧", p);
        sent |= send_bytes(p, HOST_FAST_REBOOT_FRAME);
    }
    for p in &diag_targets {
        log::info!("{} → DIAG 0x7E 帧", p);
        sent |= send_bytes(p, DIAG_REBOOT_FRAME);
    }

    // AT 兜底
    if let Some(p) = find_running_at_port() {
        log::info!("{} → AT*DOWNLOAD=1", p);
        sent |= send_bytes(&p, b"AT*DOWNLOAD=1\r\n");
    }
    sent
}

/// 自动检测进入下载模式的口，返回刷机用串口名。
///
/// 流程（运行模式探测与软重启经真机验证：运行口 VID 1782:PID 4d11/4e00）：
///   1. 用户指定口若已是下载口（VID 0525）→ 直接使用
///   2. 全系统按 VID/PID 找下载口 → 使用
///   3. 未找到下载口 → 三级软重启（host fast / DIAG 0x7E / AT*DOWNLOAD=1），等待下载口（30s）
///   4. 仍无 → 提示手动按键进入下载模式，轮询等待（60s）
///   5. 仍无且用户指定了口 → 回退用户口（可能是物理 UART2 通道，供调试）
///
/// 用户口为 "auto" 时视为未指定，不回退。
pub fn auto_enter_boot_mode(user_port: Option<&str>, on_progress: &ProgressCallback) -> Result<String> {
    // Step 1: 用户指定口已是下载口？
    if let Some(port) = user_port.filter(|p| *p != "auto") {
        if let Ok(ports) = serialport::available_ports() {
            if ports.iter().any(|p| p.port_name == port && is_download_port(&p.port_type)) {
                log::info!("用户指定端口 {} 已是下载模式", port);
                return Ok(port.to_string());
            }
        }
    }

    // Step 2: 全系统找下载口
    if let Some(port) = find_download_port() {
        on_progress(&FlashProgress::info("Detecting", 1.0, &format!("检测到下载模式口: {}", port)));
        return Ok(port);
    }

    // Step 3: 运行模式模组 → AT 软件重启进入下载模式
    if try_reboot_to_download(on_progress) {
        on_progress(&FlashProgress::info("Waiting", 3.0, "等待模组重启进入下载模式 (最多30秒)..."));
        if let Some(port) = wait_for_download_port(30) {
            on_progress(&FlashProgress::info("Detecting", 4.0, &format!("模组已进入下载模式: {}", port)));
            std::thread::sleep(Duration::from_millis(500));
            return Ok(port);
        }
        on_progress(&FlashProgress::info("Warning", -1.0, "⚠ 自动重启超时, 请手动按键进入下载模式..."));
    }

    // Step 4: 提示手动按键, 轮询等待
    on_progress(&FlashProgress::info(
        "Warning",
        -1.0,
        "⚠ 未检测到 RDA8910 下载口 (SPRD U2S Diag, VID 0525:PID a4a7). 请:\n\
         1. 按住模组按键 (音量下 / 功能键 3/7)\n\
         2. 重新上电或按 RESET\n\
         3. 松开按键\n\
         等待模组枚举为下载口...",
    ));
    if let Some(port) = wait_for_download_port(60) {
        on_progress(&FlashProgress::info("Detecting", 3.0, &format!("模组已进入下载模式: {}", port)));
        std::thread::sleep(Duration::from_millis(500));
        return Ok(port);
    }

    // Step 5: 回退用户指定口（物理 UART2 通道等）
    if let Some(port) = user_port.filter(|p| *p != "auto") {
        on_progress(&FlashProgress::info("Fallback", 2.0, &format!("使用用户指定端口: {} (可能需手动进入下载模式)", port)));
        return Ok(port.to_string());
    }

    bail!(
        "未检测到 RDA8910 下载口. 请手动操作:\n\
         1. 按住模组按键 (音量下 / 功能键 3/7)\n\
         2. 重新上电或按 RESET\n\
         3. 松开按键\n\
         然后重新运行刷机命令."
    );
}

/// 仅加载 FDL1/FDL2 到 RAM（不写 Flash 分区），验证下载链路后复位恢复。
/// 用于硬件调试：全程只触碰 RAM，不会修改 Flash 中的 modem/factory 等分区。
pub fn load_fdl2_only(soc_path: &str, port_name: &str, on_progress: &ProgressCallback, cancel: Arc<AtomicBool>) -> Result<()> {
    let (mut serial, pac, _info) = open_and_load_fdls(soc_path, port_name, on_progress, &cancel)?;

    on_progress(&FlashProgress::info("Reboot", 99.0, "FDL1/FDL2 加载完成，重启恢复"));
    fdl_reboot(&mut serial)?;
    log::info!("RDA8910 FDL2-only: PAC {} v{}", pac.prd_name, pac.prd_version);
    on_progress(&FlashProgress::done_ok("RDA8910 FDL1/FDL2 加载完成（未写 Flash）"));
    Ok(())
}

/// 把 PAC 的 LUA 条目数据替换为 overlay（`--script` 覆盖），地址保持 PAC 权威值。
/// 返回是否找到 LUA 条目（找不到时全量刷机仍写固件自带脚本）。
fn apply_script_overlay(pac: &mut PacFile, overlay: &[u8]) -> bool {
    match pac.files.iter_mut().find(|f| f.id == "LUA") {
        Some(lua) => {
            lua.data = Some(overlay.to_vec());
            lua.size = overlay.len() as u32;
            true
        }
        None => false,
    }
}

/// 全量刷机：下载 FDL1/FDL2 后按 PAC 顺序刷写分区镜像。
///
/// `script_overlay` 为 `flash run --script` 编译出的 LuaDB 脚本镜像；传 Some 时
/// 覆盖 PAC 内 LUA 条目，使全量刷机写入用户脚本（否则设备跑 .soc 固件自带脚本）。
pub fn flash_rda8910(soc_path: &str, port_name: &str, on_progress: &ProgressCallback, cancel: Arc<AtomicBool>, script_overlay: Option<&[u8]>) -> Result<()> {
    let (mut serial, mut pac, _info) = open_and_load_fdls(soc_path, port_name, on_progress, &cancel)?;

    if let Some(overlay) = script_overlay {
        if apply_script_overlay(&mut pac, overlay) {
            on_progress(&FlashProgress::info("Preparing", 28.0, &format!("应用 --script 覆盖脚本分区 ({}B)", overlay.len())).with_region("script"));
        } else {
            log::warn!("PAC 无 LUA 条目，忽略 --script 覆盖");
        }
    }

    // 阶段三：FDL2 刷写分区
    on_progress(&FlashProgress::info("Connecting", 30.0, "连接 FDL2"));
    fdl_cmd_expect(&mut serial, BSL_CMD_CONNECT, &[], BSL_REP_ACK, Duration::from_secs(2))?;

    let writable: Vec<&PacFileEntry> = pac
        .files
        .iter()
        .filter(|f| matches!(f.id.as_str(), "AP" | "PS" | "LUA") && f.size > 0 && f.data.is_some())
        .collect();
    let total_size: u64 = writable.iter().map(|f| f.size as u64).sum();
    if total_size == 0 {
        bail!("PAC 内没有可刷写的分区镜像");
    }

    let mut written: u64 = 0;
    for entry in writable {
        check_cancel(&cancel)?;
        // 目标地址必须是 Flash 物理地址或已定义的逻辑分区
        let valid_addr = entry.addr >= SPI_FLASH_BASE || matches!(entry.addr, LOGIC_ERASE_NV | LOGIC_PHASECHECK | LOGIC_FIXED_NV | LOGIC_PREPACK);
        if !valid_addr {
            bail!("PAC 条目 {} 目标地址非法: 0x{:08X}", entry.id, entry.addr);
        }
        let base = 30.0 + 68.0 * (written as f32 / total_size as f32);
        on_progress(&FlashProgress::info("Flashing", base, &format!("写入分区 {}", entry.id)).with_region(&entry.id));

        let data = entry.data.as_deref().unwrap();
        fdl_transfer(&mut serial, entry.addr, data, FLASH_PACKET_LEN).with_context(|| format!("刷写分区 {} 失败 (addr=0x{:08X})", entry.id, entry.addr))?;

        written += entry.size as u64;
        let pct = 30.0 + 68.0 * (written as f32 / total_size as f32);
        on_progress(&FlashProgress::info("Flashing", pct, &format!("完成分区 {}", entry.id)).with_region(&entry.id));
    }

    // 收尾：重启
    check_cancel(&cancel)?;
    on_progress(&FlashProgress::info("Reboot", 99.0, "重启设备"));
    fdl_reboot(&mut serial)?;

    on_progress(&FlashProgress::done_ok("RDA8910 刷机完成"));
    log::info!("RDA8910 flash: PAC {} v{}", pac.prd_name, pac.prd_version);
    Ok(())
}

/// 仅刷脚本分区：加载 FDL1/FDL2 后把 LuaDB 脚本写入脚本分区地址。
pub fn flash_script_rda8910(soc_path: &str, port_name: &str, script_data: &[u8], on_progress: &ProgressCallback, cancel: Arc<AtomicBool>) -> Result<()> {
    let (mut serial, pac, info) = open_and_load_fdls(soc_path, port_name, on_progress, &cancel)?;
    // 脚本分区地址优先取 PAC 内 LUA 条目（物理地址，权威值），
    // 仅在 PAC 无 LUA 条目时回退到 info.json 偏移 + SPI_FLASH_BASE。
    // 两者同时存在时做一致性校验：SPI_FLASH_BASE 假设一旦不成立，刷写前即暴露。
    let pac_lua_addr = pac.find("LUA").map(|e| e.addr);
    let info_script_addr = info.script_addr().checked_add(SPI_FLASH_BASE);
    if let (Some(pac_a), Some(info_a)) = (pac_lua_addr, info_script_addr) {
        if pac_a != info_a {
            log::warn!("PAC 脚本地址 0x{pac_a:08X} 与 info.json 推算 0x{info_a:08X} 不一致，以 PAC 为准");
        }
    }
    let script_addr = pac_lua_addr.or(info_script_addr).context("无法确定脚本分区地址")?;

    on_progress(&FlashProgress::info("Connecting", 30.0, "连接 FDL2"));
    fdl_cmd_expect(&mut serial, BSL_CMD_CONNECT, &[], BSL_REP_ACK, Duration::from_secs(2))?;

    on_progress(&FlashProgress::info("Flashing", 50.0, "写入脚本分区").with_region("script"));
    fdl_transfer(&mut serial, script_addr, script_data, FLASH_PACKET_LEN).with_context(|| format!("刷写脚本分区失败 (addr=0x{script_addr:08X})"))?;

    on_progress(&FlashProgress::info("Reboot", 99.0, "重启设备"));
    fdl_reboot(&mut serial)?;

    on_progress(&FlashProgress::done_ok("RDA8910 脚本刷写完成"));
    Ok(())
}

// ─── 测试 ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn build_minimal_pac(files: &[(&str, &str, u32, &[u8])]) -> Vec<u8> {
        // 构造最小合法 PAC：头 + 文件头 + 数据
        let n = files.len();
        let data_offset = PAC_HEADER_SIZE + n * PAC_FILE_HEADER_SIZE;
        let mut data = Vec::new();
        let mut cursor = data_offset;
        let mut bodies = Vec::new();
        for (id, name, addr, content) in files {
            let mut fh = vec![0u8; PAC_FILE_HEADER_SIZE];
            fh[0..4].copy_from_slice(&(PAC_FILE_HEADER_SIZE as u32).to_le_bytes());
            for (i, &b) in id.as_bytes().iter().enumerate() {
                fh[4 + i * 2] = b;
            }
            for (i, &b) in name.as_bytes().iter().enumerate() {
                fh[516 + i * 2] = b;
            }
            let size = content.len() as u32;
            fh[1540..1544].copy_from_slice(&size.to_le_bytes());
            fh[1544..1548].copy_from_slice(&1u32.to_le_bytes()); // flag
            fh[1548..1552].copy_from_slice(&1u32.to_le_bytes()); // defaultCheck
            fh[1552..1556].copy_from_slice(&(cursor as u32).to_le_bytes()); // offset
            fh[1564..1568].copy_from_slice(&addr.to_le_bytes());
            bodies.push((fh, content.to_vec()));
            cursor += content.len();
        }

        let mut header = vec![0u8; PAC_HEADER_SIZE];
        for (i, &b) in b"BP_R1.0.0".iter().enumerate() {
            header[i * 2] = b;
        }
        let pac_size = (data_offset + bodies.iter().map(|(_, c)| c.len()).sum::<usize>()) as u32;
        header[48..52].copy_from_slice(&pac_size.to_le_bytes());
        header[1076..1080].copy_from_slice(&(n as u32).to_le_bytes());
        header[1080..1084].copy_from_slice(&(PAC_HEADER_SIZE as u32).to_le_bytes());
        header[2116..2120].copy_from_slice(&PAC_MAGIC.to_le_bytes());

        data.extend_from_slice(&header);
        for (fh, content) in bodies {
            data.extend_from_slice(&fh);
            data.extend_from_slice(&content);
        }
        data
    }

    #[test]
    fn fdl_checksum_known_frame() {
        // ACK 帧体 [type=0x0080, size=0x0000]：LE 字累加 0x8000，取反 0x7FFF
        assert_eq!(fdl_checksum(&[0x00, 0x80, 0x00, 0x00]), 0x7FFF);
    }

    #[test]
    fn hdlc_escape_and_frame_roundtrip() {
        // 含 0x7E/0x7D 的数据应正确转义并往返
        let data = [0x01, HDLC_FLAG, 0x02, HDLC_ESCAPE, 0x03];
        let frame = fdl_frame(BSL_CMD_MIDST_DATA, &data);
        assert_eq!(frame.first(), Some(&HDLC_FLAG));
        assert_eq!(frame.last(), Some(&HDLC_FLAG));

        let mut rx = HdlcReceiver::new();
        let mut got = None;
        for &b in &frame {
            if let Some(pkt) = rx.feed(b) {
                got = Some(pkt);
            }
        }
        assert_eq!(got, Some((BSL_CMD_MIDST_DATA, data.to_vec())));
    }

    #[test]
    fn fdl_frame_data_roundtrip() {
        // 普通数据帧往返（无转义字节）
        let data = [0x10, 0x20, 0x30];
        let frame = fdl_frame(BSL_CMD_START_DATA, &data);
        let mut rx = HdlcReceiver::new();
        let mut got = None;
        for &b in &frame {
            if let Some(pkt) = rx.feed(b) {
                got = Some(pkt);
            }
        }
        assert_eq!(got, Some((BSL_CMD_START_DATA, data.to_vec())));
    }

    #[test]
    fn parse_pac_minimal_roundtrip() {
        let content = vec![0xAA; 64];
        let data = build_minimal_pac(&[("HOST_FDL", "fdl1.img", 0x8000C0, &content)]);
        let pac = parse_pac(&data).expect("解析失败");
        assert_eq!(pac.version, "BP_R1.0.0");
        assert_eq!(pac.files.len(), 1);
        let f = &pac.files[0];
        assert_eq!(f.id, "HOST_FDL");
        assert_eq!(f.name, "fdl1.img");
        assert_eq!(f.addr, 0x8000C0);
        assert_eq!(f.data.as_deref(), Some(content.as_slice()));
    }

    #[test]
    fn parse_pac_rejects_bad_magic() {
        let mut data = build_minimal_pac(&[]);
        data[2116] ^= 0xFF;
        assert!(parse_pac(&data).is_err());
    }

    #[test]
    fn parse_pac_rejects_too_small() {
        assert!(parse_pac(&[0u8; 16]).is_err());
    }

    #[test]
    fn is_download_port_matches_known_vid_pid() {
        use serialport::{SerialPortType, UsbPortInfo};

        let dl = SerialPortType::UsbPort(UsbPortInfo {
            vid: DOWNLOAD_VID,
            pid: DOWNLOAD_PID,
            serial_number: None,
            manufacturer: None,
            product: None,
            interface: None,
        });
        assert!(is_download_port(&dl), "VID 0525:PID a4a7 (SPRD U2S Diag) 应为下载口");

        let other_pid = SerialPortType::UsbPort(UsbPortInfo {
            vid: DOWNLOAD_VID,
            pid: 0xb001,
            serial_number: None,
            manufacturer: None,
            product: None,
            interface: None,
        });
        assert!(!is_download_port(&other_pid), "PID 不同不应误判");
        assert!(!is_download_port(&SerialPortType::Unknown), "未知端口类型不应误判");
    }

    #[test]
    fn is_running_port_matches_known_vid_pid() {
        use serialport::{SerialPortType, UsbPortInfo};

        let running = SerialPortType::UsbPort(UsbPortInfo {
            vid: RUNNING_VID,
            pid: RUNNING_PID,
            serial_number: None,
            manufacturer: None,
            product: None,
            interface: None,
        });
        assert!(is_running_port(&running), "VID 1782:PID 4d11 (UNISOC-8910) 应为运行口");

        let download = SerialPortType::UsbPort(UsbPortInfo {
            vid: DOWNLOAD_VID,
            pid: DOWNLOAD_PID,
            serial_number: None,
            manufacturer: None,
            product: None,
            interface: None,
        });
        assert!(!is_running_port(&download), "下载口不应误判为运行口");
    }

    #[test]
    fn port_interface_is_matches_interface_number() {
        use serialport::{SerialPortInfo, SerialPortType, UsbPortInfo};

        let make = |iface| SerialPortInfo {
            port_name: "/dev/ttyUSBX".into(),
            port_type: SerialPortType::UsbPort(UsbPortInfo {
                vid: RUNNING_VID,
                pid: RUNNING_PID,
                serial_number: None,
                manufacturer: None,
                product: None,
                interface: iface,
            }),
        };
        assert!(port_interface_is(&make(Some(6)), 6), "接口 6 应命中 x.6");
        assert!(!port_interface_is(&make(Some(3)), 6), "接口 3 不应命中 x.6");
        assert!(!port_interface_is(&make(None), 6), "接口信息缺失不应命中");
    }

    #[test]
    fn port_is_log_matches_luatools_py3_layout() {
        use serialport::{SerialPortInfo, SerialPortType, UsbPortInfo};

        let make = |pid: u16, iface: Option<u8>| SerialPortInfo {
            port_name: "/dev/ttyUSBX".into(),
            port_type: SerialPortType::UsbPort(UsbPortInfo {
                vid: RUNNING_VID,
                pid,
                serial_number: None,
                manufacturer: None,
                product: None,
                interface: iface,
            }),
        };
        // 0x4d11 (8910_soc): soc_log=x.6
        assert!(port_is_log(&make(RUNNING_PID, Some(6))), "0x4d11 接口 6 应为 log 口");
        assert!(!port_is_log(&make(RUNNING_PID, Some(2))), "0x4d11 接口 2 是 user_com, 非 log 口");
        assert!(!port_is_log(&make(RUNNING_PID, Some(3))), "0x4d11 接口 3 是 DIAG, 非 log 口");
        // 0x4e00 (8910_air): user_log=x.2
        assert!(port_is_log(&make(RUNNING_PID_ALT, Some(2))), "0x4e00 接口 2 应为 log 口");
        assert!(!port_is_log(&make(RUNNING_PID_ALT, Some(3))), "0x4e00 接口 3 是 at_log, 非 log 口");
        // 接口信息缺失不应命中
        assert!(!port_is_log(&make(RUNNING_PID, None)), "接口信息缺失不应命中");
    }

    #[test]
    fn reboot_frames_match_documented_bytes() {
        // host fast 帧 = 文档通用切换指令 AD 00 04 FD 03 44 52 E8
        assert_eq!(HOST_FAST_REBOOT_FRAME, &[0xAD, 0x00, 0x04, 0xFD, 0x03, 0x44, 0x52, 0xE8]);
        // DIAG 帧以 0x7E 包裹，含 cmd=5 / sub=0x42
        assert_eq!(DIAG_REBOOT_FRAME.first(), Some(&0x7E));
        assert_eq!(DIAG_REBOOT_FRAME.last(), Some(&0x7E));
        assert!(DIAG_REBOOT_FRAME.windows(2).any(|w| w == [0x05, 0x42]));
    }

    #[test]
    fn at_ok_received_detects_ok_but_not_error() {
        assert!(at_ok_received(b"\r\nOK\r\n"), "AT 应答含 OK 应判定为 AT 口");
        assert!(at_ok_received(b"AT\r\nOK\r\n"));
        assert!(!at_ok_received(b"\r\nERROR\r\n"), "ERROR 不应误判");
        assert!(!at_ok_received(b""), "空应答不应误判");
    }

    #[test]
    fn apply_script_overlay_replaces_lua_entry() {
        let content = vec![0xAA; 64];
        let data = build_minimal_pac(&[("LUA", "script.bin", 0x6023_0000, &content)]);
        let mut pac = parse_pac(&data).expect("解析失败");
        let overlay = vec![0xBB; 128];

        assert!(apply_script_overlay(&mut pac, &overlay), "存在 LUA 条目应成功覆盖");
        let lua = pac.find("LUA").expect("覆盖后 LUA 条目仍存在");
        assert_eq!(lua.data.as_deref(), Some(overlay.as_slice()), "数据应为用户脚本");
        assert_eq!(lua.size as usize, overlay.len(), "size 应随 overlay 更新");
        assert_eq!(lua.addr, 0x6023_0000, "地址保持 PAC 权威值不变");
    }

    #[test]
    fn apply_script_overlay_returns_false_without_lua_entry() {
        let content = vec![0xAA; 64];
        let data = build_minimal_pac(&[("AP", "csdk.img", 0x6001_0000, &content)]);
        let mut pac = parse_pac(&data).expect("解析失败");
        assert!(!apply_script_overlay(&mut pac, b"x"), "无 LUA 条目应返回 false");
    }

    /// 硬件调试：仅加载 FDL1/FDL2 到 RAM（不写 Flash 分区），验证下载链路后复位恢复。
    /// 全程只触碰 RAM，不会修改 Flash 中的 modem/factory 等分区，可安全反复运行。
    /// 运行：LUATOS_SOC=<.soc> LUATOS_PORT=<串口> cargo test -p luatos-flash rda_live_load_fdl2 -- --ignored --nocapture
    #[test]
    #[ignore = "requires Air724UG physically connected; set LUATOS_SOC / LUATOS_PORT"]
    fn rda_live_load_fdl2() {
        let soc = std::env::var("LUATOS_SOC").unwrap_or_else(|_| "LuatOS-SoC_V1009_Air724UG.soc".into());
        let port = std::env::var("LUATOS_PORT").unwrap_or_else(|_| "/dev/ttyUSB0".into());
        println!("\n=== RDA8910 load FDL1/FDL2 (soc={soc}, port={port}) ===");
        let cancel = Arc::new(AtomicBool::new(false));
        let on_progress: ProgressCallback = Box::new(|p| {
            eprintln!("[{:>6.1}%] {} — {}", p.percent, p.stage, p.message);
        });
        load_fdl2_only(&soc, &port, &on_progress, cancel).expect("加载 FDL1/FDL2 失败");
        println!("=== FDL1/FDL2 加载完成，Flash 分区未改动 ===");
    }

    /// 硬件调试：仅刷脚本分区（用 PAC 自带的 LUA 数据，幂等安全，不碰 modem/factory）。
    /// 验证 FDL2 阶段三写入协议（START/MIDST/END 写 Flash）。
    /// 运行：LUATOS_SOC=<.soc> LUATOS_PORT=<串口> cargo test -p luatos-flash rda_live_flash_script -- --ignored --nocapture
    #[test]
    #[ignore = "requires Air724UG physically connected; set LUATOS_SOC / LUATOS_PORT"]
    fn rda_live_flash_script() {
        let soc = std::env::var("LUATOS_SOC").unwrap_or_else(|_| "LuatOS-SoC_V1009_Air724UG.soc".into());
        let port = std::env::var("LUATOS_PORT").unwrap_or_else(|_| "/dev/ttyUSB0".into());
        println!("\n=== RDA8910 script-only flash (soc={soc}, port={port}) ===");

        let tmp = tempfile::tempdir().expect("tempdir");
        let unpacked = luatos_soc::unpack_soc(&soc, tmp.path()).expect("解包 SOC 失败");
        let pac_data = std::fs::read(&unpacked.rom_path).expect("读取 PAC 失败");
        let pac = parse_pac(&pac_data).expect("解析 PAC 失败");
        let lua = pac.find("LUA").expect("PAC 缺少 LUA 条目");
        let script_data = lua.data.as_deref().expect("LUA 数据缺失");
        println!("脚本分区: {}B @ 0x{:08X}", script_data.len(), lua.addr);

        let cancel = Arc::new(AtomicBool::new(false));
        let on_progress: ProgressCallback = Box::new(|p| {
            eprintln!("[{:>6.1}%] {} — {}", p.percent, p.stage, p.message);
        });
        flash_script_rda8910(&soc, &port, script_data, &on_progress, cancel).expect("刷脚本分区失败");
        println!("=== 脚本分区刷写完成 ===");
    }
}
