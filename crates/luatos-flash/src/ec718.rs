// EC718 (Air8000 / Air780EPM / Air780EHx) flash protocol.
//
// Multi-stage firmware download for Eigencomm EC718 series chips:
//   1. DLBOOT Sync: ROM bootloader handshake
//   2. AgentBoot: download agent binary to RAM via DLBOOT protocol
//   3. Partition Burn: flash each image via AGBOOT+LPC protocol
//   4. Reset: reboot into flashed firmware
//
// Protocol frame format:
//   DL Command: [cmd:u8][index:u8][order_id:u8=0xCD][norder_id:u8=0x32][len:u32le]
//   DL Response: [cmd:u8][index:u8][order_id:u8][norder_id:u8][state:u8][len:u8]
//   AGBOOT mode appends CRC32 to commands and uses CRC8-Maxim length encoding
//   DLBOOT mode uses self_def_check1 for DOWNLOAD_DATA
//
// Reference: https://github.com/yuzhan-tech/luatos-tools

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serialport::SerialPort;
use sha2::{Digest, Sha256};

use crate::{FlashProgress, ProgressCallback};

// ─── Embedded AgentBoot Binaries ─────────────────────────────────────────────

const AGENTBOOT_EC718M_USB: &[u8] =
    include_bytes!("../../../refs/origin_tools/ec_download/agentboot/ec718m_usb.bin");
const AGENTBOOT_EC718M_UART: &[u8] =
    include_bytes!("../../../refs/origin_tools/ec_download/agentboot/ec718m_uart.bin");

// ─── Protocol Constants ─────────────────────────────────────────────────────

// Handshake sync values
const DLBOOT_HANDSHAKE: u32 = 0x2b02d300;
const AGBOOT_HANDSHAKE: u32 = 0x2b02d3aa;
const LPC_HANDSHAKE: u32 = 0x2b02d3cd;

// Image identifiers
const IMGH_IDENTIFIER: u32 = 0x54494d48;
const AGBT_IDENTIFIER: u32 = 0x4F424D49;
const AIMG_IDENTIFIER: u32 = 0x444B4249;
const CIMG_IDENTIFIER: u32 = 0x43504249;
const FLEX_IDENTIFIER: u32 = 0x464c5849;

// DL command framing
const DL_COMMAND_ID: u8 = 0xcd;
const DL_COMMAND_ID_INV: u8 = 0x32;

// LPC command framing
const LPC_COMMAND_ID: u8 = 0x4c;
const LPC_COMMAND_ID_INV: u8 = 0xb3;

// DL commands
const CMD_GET_VERSION: u8 = 0x20;
const CMD_SEL_IMAGE: u8 = 0x21;
const CMD_VERIFY_IMAGE: u8 = 0x22;
const CMD_DATA_HEAD: u8 = 0x31;
const CMD_DOWNLOAD_DATA: u8 = 0x32;
const CMD_DONE: u8 = 0x3a;

// LPC commands
const LPC_FLASH_ERASE: u8 = 0x10;
const LPC_BURN_ONE: u8 = 0x42;
const LPC_GET_BURN_STATUS: u8 = 0x44;
const LPC_SYS_RST: u8 = 0xaa;

// LPC response magic
const LPC_BURN_STATUS_OK: &[u8] = &[0x00, 0x00, 0x00, 0x00];
const LPC_SYS_RESET_ACK: &[u8] = b"ZzZzZzZz";

// Protocol sizes
const FIXED_PROTOCOL_RSP_LEN: usize = 6;
const MAX_DATA_BLOCK_SIZE: usize = 0x10000; // 64KB

// Storage types
const STYPE_AP_FLASH: u8 = 0x0;
const STYPE_CP_FLASH: u8 = 0x1;
const CP_FLASH_MARKER: u16 = 0xe101;

// Image header size
const IMG_HEAD_SIZE: usize = 272;

// ─── Sync Types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
enum SyncType {
    DlBoot,
    AgBoot,
    Lpc,
}

impl SyncType {
    fn handshake_value(&self) -> u32 {
        match self {
            SyncType::DlBoot => DLBOOT_HANDSHAKE,
            SyncType::AgBoot => AGBOOT_HANDSHAKE,
            SyncType::Lpc => LPC_HANDSHAKE,
        }
    }
}

// ─── Burn Image Types ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
enum BurnImageType {
    Bootloader,
    Ap,
    Cp,
    FlexFile,
    Head,
    AgBoot,
}

impl BurnImageType {
    fn identifier(&self) -> u32 {
        match self {
            BurnImageType::Bootloader | BurnImageType::AgBoot => AGBT_IDENTIFIER,
            BurnImageType::Ap => AIMG_IDENTIFIER,
            BurnImageType::Cp => CIMG_IDENTIFIER,
            BurnImageType::FlexFile => FLEX_IDENTIFIER,
            BurnImageType::Head => IMGH_IDENTIFIER,
        }
    }
}

// ─── Port Type ──────────────────────────────────────────────────────────────

/// Port type for EC718: USB or UART determine agentboot binary and baud rate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Ec718PortType {
    Usb,
    Uart,
}

impl Ec718PortType {
    fn baudrate(&self) -> u32 {
        match self {
            Ec718PortType::Usb => 921600,
            Ec718PortType::Uart => 115200,
        }
    }
}

/// Detect port type from serial port info (USB VID/PID check).
pub fn detect_port_type(port_name: &str) -> Ec718PortType {
    if let Ok(ports) = serialport::available_ports() {
        for p in &ports {
            if p.port_name == port_name {
                if let serialport::SerialPortType::UsbPort(_) = &p.port_type {
                    return Ec718PortType::Usb;
                }
            }
        }
    }
    Ec718PortType::Uart
}

// ─── Checksum Functions ─────────────────────────────────────────────────────

/// CRC8-Maxim (Dallas/Maxim 1-Wire CRC) for length field encoding.
fn crc8_maxim(stream: &[u8]) -> u8 {
    let mut crc: u32 = 0;
    for &c in stream {
        for i in 0..8 {
            let b = (crc & 1) ^ (((c as u32) & (1 << i)) >> i);
            crc = (crc ^ (b * 0x118)) >> 1;
        }
    }
    crc as u8
}

/// Self-defined checksum for DLBOOT DOWNLOAD_DATA commands.
fn self_def_check1(cmd: u8, index: u8, order_id: u8, norder_id: u8, len: u32, data: &[u8]) -> [u8; 4] {
    let mut ck_val: u32 = cmd as u32
        + index as u32
        + order_id as u32
        + norder_id as u32
        + (len & 0xFF)
        + ((len >> 8) & 0xFF)
        + ((len >> 16) & 0xFF)
        + ((len >> 24) & 0xFF);

    for &b in data {
        ck_val = ck_val.wrapping_add(b as u32);
    }
    ck_val.to_le_bytes()
}

// ─── Serial I/O Helpers ─────────────────────────────────────────────────────

fn com_write(port: &mut dyn SerialPort, data: &[u8]) -> Result<()> {
    log::debug!("COM WRITE: {} bytes", data.len());
    // On Windows, write all at once; on other platforms chunk to 64 bytes
    if cfg!(target_os = "windows") || data.len() <= 64 {
        port.write_all(data).context("Serial write failed")?;
    } else {
        for chunk in data.chunks(64) {
            port.write_all(chunk).context("Serial write failed")?;
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    Ok(())
}

fn com_read(port: &mut dyn SerialPort, len: usize) -> Result<Option<Vec<u8>>> {
    if len == 0 {
        return Ok(None);
    }
    let mut buf = vec![0u8; len];
    let mut total = 0;
    while total < len {
        match port.read(&mut buf[total..]) {
            Ok(n) => total += n,
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => break,
            Err(e) => return Err(e).context("Serial read failed"),
        }
    }
    if total == 0 {
        log::debug!("COM READ timeout");
        Ok(None)
    } else {
        buf.truncate(total);
        log::debug!("COM READ: {} bytes (requested {})", total, len);
        Ok(Some(buf))
    }
}

// ─── DL Command / Response Structures ───────────────────────────────────────

struct Cmd {
    cmd: u8,
    index: u8,
    order_id: u8,
    norder_id: u8,
    len: u32,
}

impl Cmd {
    fn new(cmd_id: u8) -> Self {
        Cmd {
            cmd: cmd_id,
            index: 0,
            order_id: DL_COMMAND_ID,
            norder_id: DL_COMMAND_ID_INV,
            len: 0,
        }
    }

    fn pack(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(8);
        buf.push(self.cmd);
        buf.push(self.index);
        buf.push(self.order_id);
        buf.push(self.norder_id);
        buf.extend_from_slice(&self.len.to_le_bytes());
        buf
    }
}

struct Rsp {
    #[allow(dead_code)]
    cmd: u8,
    #[allow(dead_code)]
    index: u8,
    #[allow(dead_code)]
    order_id: u8,
    #[allow(dead_code)]
    norder_id: u8,
    state: u8,
    len: u8,
}

impl Rsp {
    fn unpack(data: &[u8]) -> Self {
        Rsp {
            cmd: data[0],
            index: data[1],
            order_id: data[2],
            norder_id: data[3],
            state: data[4],
            len: data[5],
        }
    }
}

struct LpcCmd {
    cmd: u8,
    #[allow(dead_code)]
    index: u8,
    order_id: u8,
    norder_id: u8,
    len: u32,
}

impl LpcCmd {
    fn new(cmd_id: u8) -> Self {
        LpcCmd {
            cmd: cmd_id,
            index: 0,
            order_id: LPC_COMMAND_ID,
            norder_id: LPC_COMMAND_ID_INV,
            len: 0,
        }
    }

    fn pack(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(8);
        buf.push(self.cmd);
        buf.push(self.index);
        buf.push(self.order_id);
        buf.push(self.norder_id);
        buf.extend_from_slice(&self.len.to_le_bytes());
        buf
    }
}

// ─── Image Header ───────────────────────────────────────────────────────────

struct ImgHead {
    data: Vec<u8>,
}

impl ImgHead {
    const CTLINFO_OFFSET: usize = 20;
    const RSVD0_OFFSET: usize = 24;
    const HASHIH_OFFSET: usize = 32;
    const BODY_OFFSET: usize = 64;

    fn new() -> Self {
        let mut data = vec![0u8; IMG_HEAD_SIZE];
        // VersionInfo.vVal = 0x10000001
        data[0..4].copy_from_slice(&0x10000001u32.to_le_bytes());
        // VersionInfo.id = IMGH_IDENTIFIER
        data[4..8].copy_from_slice(&IMGH_IDENTIFIER.to_le_bytes());
        // VersionInfo.dtm = 0x20180507
        data[8..12].copy_from_slice(&0x20180507u32.to_le_bytes());
        // imgnum = 1
        data[16..20].copy_from_slice(&1u32.to_le_bytes());
        // ctlinfo.hashtype = 0xee
        data[Self::CTLINFO_OFFSET] = 0xee;
        // ImgBody.id = AGBT_IDENTIFIER
        data[Self::BODY_OFFSET..Self::BODY_OFFSET + 4]
            .copy_from_slice(&AGBT_IDENTIFIER.to_le_bytes());
        // ImgBody.ldloc = 0x04010000
        data[Self::BODY_OFFSET + 8..Self::BODY_OFFSET + 12]
            .copy_from_slice(&0x04010000u32.to_le_bytes());

        ImgHead { data }
    }

    fn set_body_id(&mut self, id: u32) {
        self.data[Self::BODY_OFFSET..Self::BODY_OFFSET + 4].copy_from_slice(&id.to_le_bytes());
    }

    fn set_burn_addr(&mut self, addr: u32) {
        let off = Self::BODY_OFFSET + 4;
        self.data[off..off + 4].copy_from_slice(&addr.to_le_bytes());
    }

    fn set_img_size(&mut self, size: u32) {
        let off = Self::BODY_OFFSET + 12;
        self.data[off..off + 4].copy_from_slice(&size.to_le_bytes());
    }

    fn set_hashv(&mut self, hash: &[u8; 32]) {
        let off = Self::BODY_OFFSET + 32;
        self.data[off..off + 32].copy_from_slice(hash);
    }

    fn set_baudrate_ctrl(&mut self, baud: u32) {
        let ctrl = if baud != 0 {
            ((baud / 100) + 0x8000) as u16
        } else {
            0
        };
        let off = Self::CTLINFO_OFFSET + 2;
        self.data[off..off + 2].copy_from_slice(&ctrl.to_le_bytes());
    }

    fn set_hashtype(&mut self, hashtype: u8) {
        self.data[Self::CTLINFO_OFFSET] = hashtype;
    }

    fn set_rsvd0(&mut self, val: u32) {
        self.data[Self::RSVD0_OFFSET..Self::RSVD0_OFFSET + 4].copy_from_slice(&val.to_le_bytes());
    }

    fn set_hashih(&mut self, hash: &[u8; 32]) {
        self.data[Self::HASHIH_OFFSET..Self::HASHIH_OFFSET + 32].copy_from_slice(hash);
    }

    fn finalize_hash(&mut self) {
        let hash: [u8; 32] = Sha256::digest(&self.data).into();
        self.set_hashih(&hash);
    }

    fn pack(&self) -> &[u8] {
        &self.data
    }
}

// ─── Binpkg Format ──────────────────────────────────────────────────────────

const PKGMODE_MAGIC: &[u8] = b"pkgmode";
const ENTRY_META_SIZE: usize = 364;

/// A single partition entry from a binpkg file.
#[derive(Debug, Clone)]
pub struct BinpkgEntry {
    pub name: String,
    pub addr: u32,
    pub flash_size: u32,
    pub image_size: u32,
    pub hash: String,
    pub image_type: String,
    pub data: Option<Vec<u8>>,
}

/// Result of parsing a binpkg file.
#[derive(Debug)]
pub struct BinpkgResult {
    pub chip: String,
    pub entries: Vec<BinpkgEntry>,
}

/// Parse a binpkg binary blob, keeping image data for burning.
pub fn parse_binpkg(fdata: &[u8]) -> Result<BinpkgResult> {
    let fsize = fdata.len();
    if fsize < 0x34 {
        bail!("binpkg data too small ({} bytes)", fsize);
    }

    let foffset: usize;
    let chip_name: String;

    // Detect format: pkgmode vs legacy
    if fsize > 0x3F && &fdata[0x38..0x3F] == PKGMODE_MAGIC {
        foffset = 0x1D8;
        let raw = &fdata[0x190..std::cmp::min(0x1A0, fsize)];
        chip_name = raw
            .split(|&b| b == 0)
            .next()
            .map(|s| String::from_utf8_lossy(s).to_string())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "unknown".to_string());
    } else {
        foffset = 0x34;
        chip_name = "unknown".to_string();
    }

    let mut entries = Vec::new();
    let mut cursor = foffset;

    while cursor + ENTRY_META_SIZE <= fsize {
        let meta = &fdata[cursor..cursor + ENTRY_META_SIZE];

        let name_raw = &meta[0..64];
        let name = name_raw
            .split(|&b| b == 0)
            .next()
            .map(|s| String::from_utf8_lossy(s).to_string())
            .unwrap_or_default();

        let addr = u32::from_le_bytes(meta[64..68].try_into().unwrap());
        let flash_size = u32::from_le_bytes(meta[68..72].try_into().unwrap());
        let _offset = u32::from_le_bytes(meta[72..76].try_into().unwrap());
        let img_size = u32::from_le_bytes(meta[76..80].try_into().unwrap());

        let hash_raw = &meta[80..336];
        let hash = hash_raw
            .split(|&b| b == 0)
            .next()
            .map(|s| String::from_utf8_lossy(s).to_string().to_lowercase())
            .unwrap_or_default();

        let img_type_raw = &meta[336..352];
        let image_type = img_type_raw
            .split(|&b| b == 0)
            .next()
            .map(|s| String::from_utf8_lossy(s).to_string())
            .unwrap_or_default();

        cursor += ENTRY_META_SIZE;

        let data = if cursor + (img_size as usize) <= fsize {
            let d = fdata[cursor..cursor + img_size as usize].to_vec();
            cursor += img_size as usize;
            Some(d)
        } else {
            None
        };

        log::debug!(
            "binpkg entry: {} addr=0x{:08X} size={} type={}",
            name,
            addr,
            img_size,
            image_type
        );

        entries.push(BinpkgEntry {
            name,
            addr,
            flash_size,
            image_size: img_size,
            hash,
            image_type,
            data,
        });
    }

    Ok(BinpkgResult { chip: chip_name, entries })
}

// ─── Sync Protocol ──────────────────────────────────────────────────────────

fn burn_sync(port: &mut dyn SerialPort, sync_type: SyncType, counter: u32) -> Result<()> {
    log::debug!("burn_sync {:?} counter={}", sync_type, counter);
    let handshake = sync_type.handshake_value();
    let send_buf = handshake.to_le_bytes();

    for _i in 0..50 {
        for _j in 0..counter {
            com_write(port, &send_buf)?;
            std::thread::sleep(Duration::from_millis(2));
        }

        if let Some(recv_buf) = com_read(port, 4)? {
            log::debug!("sync recv: {:02x?} (expect {:02x?})", recv_buf, send_buf);
            if recv_buf.len() < 4 {
                continue;
            }
            if matches!(sync_type, SyncType::DlBoot) {
                if let Some(extra) = com_read(port, 1)? {
                    log::debug!("sync extra byte: {:02x}", extra[0]);
                    if extra[0] != 0 {
                        continue;
                    }
                }
            }
            if recv_buf == send_buf {
                log::debug!("sync done");
                return Ok(());
            }
        }
    }
    bail!("Sync failed for {:?}", sync_type);
}

// ─── Command Send/Receive ───────────────────────────────────────────────────

/// Send a DL command and receive response.
/// dlboot=true: DLBOOT mode (self_def_check1 for DOWNLOAD_DATA)
/// dlboot=false: AGBOOT mode (CRC32 + CRC8-Maxim length encoding)
fn send_recv_cmd(
    port: &mut dyn SerialPort,
    cmd: &mut Cmd,
    data: &[u8],
    dlboot: bool,
) -> Result<(i32, Option<Vec<u8>>)> {
    let mut tmpdata = cmd.pack();
    tmpdata.extend_from_slice(data);

    if !dlboot {
        // AGBOOT: CRC32 first, then encode length with CRC8
        let ck_val = crc32fast::hash(&tmpdata);
        if cmd.len > 0 {
            let tmp_len = cmd.len & 0x00FFFFFF;
            let len_bytes = tmp_len.to_le_bytes();
            let crc8 = crc8_maxim(&len_bytes[..3]);
            cmd.len = ((crc8 as u32) << 24) | tmp_len;
            tmpdata = cmd.pack();
            tmpdata.extend_from_slice(data);
        }
        tmpdata.extend_from_slice(&ck_val.to_le_bytes());
    } else if cmd.cmd == CMD_DOWNLOAD_DATA {
        // DLBOOT with DOWNLOAD_DATA: self_def_check1
        let ck = self_def_check1(
            cmd.cmd,
            cmd.index,
            cmd.order_id,
            cmd.norder_id,
            cmd.len,
            data,
        );
        tmpdata.extend_from_slice(&ck);
    }

    com_write(port, &tmpdata)?;
    std::thread::sleep(Duration::from_millis(2));

    // Read 6-byte response
    let recv_buf = match com_read(port, FIXED_PROTOCOL_RSP_LEN)? {
        Some(buf) if buf.len() == FIXED_PROTOCOL_RSP_LEN => buf,
        Some(buf) => {
            log::warn!("Response incomplete: {} bytes, expected {}", buf.len(), FIXED_PROTOCOL_RSP_LEN);
            return Ok((-1, None));
        }
        None => {
            log::warn!("Response timeout");
            return Ok((-1, None));
        }
    };

    let rsp = Rsp::unpack(&recv_buf);
    let rsp_data = if rsp.len > 0 {
        com_read(port, rsp.len as usize)?
    } else {
        None
    };

    // In AGBOOT mode, read trailing CRC32
    if !dlboot {
        let _ = com_read(port, 4)?;
    }

    if rsp.state != 0 {
        log::warn!("Response not ACK: state={}", rsp.state);
        return Ok((-2, None));
    }

    Ok((0, rsp_data))
}

/// Send an LPC command and receive response.
fn send_recv_lpc_cmd(
    port: &mut dyn SerialPort,
    cmd: &mut LpcCmd,
    data: &[u8],
) -> Result<(i32, Option<Vec<u8>>)> {
    // CRC32 of original cmd + data
    let mut orig = cmd.pack();
    orig.extend_from_slice(data);
    let ck_val = crc32fast::hash(&orig);

    // Encode length with CRC8-Maxim
    cmd.len = data.len() as u32;
    if cmd.len > 0 {
        let tmp_len = cmd.len & 0x00FFFFFF;
        let len_bytes = tmp_len.to_le_bytes();
        let crc8 = crc8_maxim(&len_bytes[..3]);
        cmd.len = ((crc8 as u32) << 24) | tmp_len;
    }

    let mut tmpdata = cmd.pack();
    tmpdata.extend_from_slice(data);
    tmpdata.extend_from_slice(&ck_val.to_le_bytes());

    com_write(port, &tmpdata)?;

    if let Some(recv_buf) = com_read(port, 6)? {
        if recv_buf.len() < 6 {
            log::warn!("LPC response incomplete: {} bytes", recv_buf.len());
            return Ok((-1, None));
        }
        let rsp = Rsp::unpack(&recv_buf);

        let rsp_data = if rsp.len > 0 {
            com_read(port, rsp.len as usize)?
        } else {
            None
        };

        // Read trailing CRC32
        let _ = com_read(port, 4)?;

        if rsp.state != 0 {
            log::warn!("LPC response not ACK: state={}", rsp.state);
            return Ok((-2, rsp_data));
        }
        return Ok((0, rsp_data));
    }
    Ok((-1, None))
}

// ─── Package Protocol Commands ──────────────────────────────────────────────

fn package_get_version(port: &mut dyn SerialPort, dlboot: bool) -> Result<i32> {
    let mut cmd = Cmd::new(CMD_GET_VERSION);
    let (ok, data) = send_recv_cmd(port, &mut cmd, &[], dlboot)?;
    if ok == 0 {
        if let Some(ref d) = data {
            if d.len() >= 16 {
                let v_val = u32::from_le_bytes(d[0..4].try_into().unwrap());
                let id = u32::from_le_bytes(d[4..8].try_into().unwrap());
                log::info!("version: vVal=0x{:08X} id=0x{:08X}", v_val, id);
                return Ok(0);
            }
        }
    }
    Ok(ok)
}

fn package_sel_image(port: &mut dyn SerialPort, img_type: u32, dlboot: bool) -> Result<i32> {
    let mut cmd = Cmd::new(CMD_SEL_IMAGE);
    let (ok, data) = send_recv_cmd(port, &mut cmd, &[], dlboot)?;
    if ok == 0 {
        if let Some(ref d) = data {
            if d.len() >= 4 {
                let ck_img = u32::from_le_bytes(d[0..4].try_into().unwrap());
                log::debug!("sel_image {:08X} vs {:08X}", img_type, ck_img);
                if img_type == ck_img {
                    return Ok(0);
                }
                log::error!("sel_image type mismatch");
            }
        }
    }
    Ok(-1)
}

fn package_verify_image(port: &mut dyn SerialPort, dlboot: bool) -> Result<i32> {
    let mut cmd = Cmd::new(CMD_VERIFY_IMAGE);
    let (ok, _) = send_recv_cmd(port, &mut cmd, &[], dlboot)?;
    Ok(ok)
}

fn package_base_info(port: &mut dyn SerialPort, img_type: u32, dlboot: bool) -> Result<i32> {
    let ok = package_get_version(port, dlboot)?;
    if ok != 0 {
        return Ok(-1);
    }
    if package_sel_image(port, img_type, dlboot)? != 0 {
        return Ok(-1);
    }
    if package_verify_image(port, dlboot)? != 0 {
        return Ok(-1);
    }
    Ok(0)
}

fn package_data_head(port: &mut dyn SerialPort, remain_size: u32, dlboot: bool) -> Result<(i32, u32)> {
    let mut cmd = Cmd::new(CMD_DATA_HEAD);
    cmd.len = 4;
    let data = remain_size.to_le_bytes();
    let (ok, recv) = send_recv_cmd(port, &mut cmd, &data, dlboot)?;
    if ok == 0 {
        if let Some(ref d) = recv {
            if d.len() >= 4 {
                let tb_size = u32::from_le_bytes(d[0..4].try_into().unwrap());
                return Ok((0, tb_size));
            }
        }
    }
    Ok((ok, 0))
}

fn package_data_single(port: &mut dyn SerialPort, cmd: &mut Cmd, data: &[u8], dlboot: bool) -> Result<i32> {
    cmd.len = data.len() as u32;
    let (ok, _) = send_recv_cmd(port, cmd, data, dlboot)?;
    Ok(ok)
}

fn package_done(port: &mut dyn SerialPort, dlboot: bool) -> Result<i32> {
    let mut cmd = Cmd::new(CMD_DONE);
    let (ok, _) = send_recv_cmd(port, &mut cmd, &[], dlboot)?;
    Ok(ok)
}

/// Transfer data using data_head + data_single loop + done protocol.
fn package_data(port: &mut dyn SerialPort, cmd: &mut Cmd, data: &[u8], dlboot: bool) -> Result<i32> {
    let mut data_offset: usize = 0;
    let mut remain_size = data.len() as u32;
    let mut counter: u8 = 0;
    let mut ret = 0;

    while remain_size > 0 {
        let (ok, tb_size) = package_data_head(port, remain_size, dlboot)?;
        if ok != 0 {
            return Ok(-1);
        }

        cmd.index = counter;
        cmd.len = tb_size;

        if tb_size >= remain_size {
            ret = package_data_single(port, cmd, &data[data_offset..], dlboot)?;
            break;
        }

        let end = data_offset + tb_size as usize;
        ret = package_data_single(port, cmd, &data[data_offset..end], dlboot)?;
        if ret != 0 {
            break;
        }

        counter = counter.wrapping_add(1);
        data_offset += tb_size as usize;
        remain_size -= tb_size;
    }

    if ret == 0 {
        ret = package_done(port, dlboot)?;
    }
    Ok(ret)
}

/// Build and send image header.
fn package_image_head(
    port: &mut dyn SerialPort,
    fdata: &[u8],
    img_type: BurnImageType,
    addr: u32,
    baud: u32,
    dlboot: bool,
    pullup_qspi: u32,
) -> Result<i32> {
    let fhash: [u8; 32] = Sha256::digest(fdata).into();

    let mut img_hd = ImgHead::new();
    img_hd.set_body_id(img_type.identifier());
    img_hd.set_img_size(fdata.len() as u32);
    img_hd.set_burn_addr(addr);
    img_hd.set_hashv(&fhash);
    img_hd.set_baudrate_ctrl(baud);
    img_hd.set_hashtype(0xee);
    img_hd.set_rsvd0(pullup_qspi);
    img_hd.finalize_hash();

    let mut cmd = Cmd::new(CMD_DOWNLOAD_DATA);
    let hd_data = img_hd.pack().to_vec();
    cmd.len = hd_data.len() as u32;

    if package_data(port, &mut cmd, &hd_data, dlboot)? != 0 {
        bail!("image_head failed");
    }
    Ok(0)
}

// ─── LPC Commands ───────────────────────────────────────────────────────────

fn lpc_burn_one(port: &mut dyn SerialPort, img_type: BurnImageType, stor_type: u8) -> Result<i32> {
    let mut cmd = LpcCmd::new(LPC_BURN_ONE);
    let img_id = img_type.identifier();

    let data = if stor_type == STYPE_CP_FLASH {
        cmd.len = 6;
        let mut d = img_id.to_le_bytes().to_vec();
        d.extend_from_slice(&CP_FLASH_MARKER.to_le_bytes());
        d
    } else {
        cmd.len = 4;
        img_id.to_le_bytes().to_vec()
    };

    let (ret, _) = send_recv_lpc_cmd(port, &mut cmd, &data)?;
    Ok(ret)
}

fn lpc_get_burn_status(port: &mut dyn SerialPort) -> Result<i32> {
    let mut cmd = LpcCmd::new(LPC_GET_BURN_STATUS);
    let (ret, data) = send_recv_lpc_cmd(port, &mut cmd, &[])?;
    if ret == 0 {
        if let Some(ref d) = data {
            if d == LPC_BURN_STATUS_OK {
                return Ok(0);
            }
        }
    }
    Ok(-1)
}

#[allow(dead_code)]
fn lpc_flash_erase(port: &mut dyn SerialPort, addr: u32, size: u32) -> Result<i32> {
    let mut cmd = LpcCmd::new(LPC_FLASH_ERASE);
    cmd.len = 8;
    let mut data = size.to_le_bytes().to_vec();
    data.extend_from_slice(&addr.to_le_bytes());
    let (ret, _) = send_recv_lpc_cmd(port, &mut cmd, &data)?;
    Ok(ret)
}

fn lpc_sys_reset(port: &mut dyn SerialPort) -> Result<i32> {
    let mut cmd = LpcCmd::new(LPC_SYS_RST);
    let (ret, data) = send_recv_lpc_cmd(port, &mut cmd, &[])?;
    if ret == 0 {
        if let Some(ref d) = data {
            if d == LPC_SYS_RESET_ACK {
                return Ok(0);
            }
        }
    }
    Ok(-1)
}

// ─── High-Level Burn Operations ─────────────────────────────────────────────

/// Download agent boot binary to device RAM.
fn burn_agboot(port: &mut dyn SerialPort, agent_data: &[u8], baud: u32) -> Result<()> {
    log::info!("Downloading agent boot ({} bytes)", agent_data.len());

    let ret = package_base_info(port, BurnImageType::Head.identifier(), true)?;
    if ret != 0 {
        bail!("Agent boot base_info(HEAD) failed");
    }

    package_image_head(port, agent_data, BurnImageType::AgBoot, 0, baud, true, 1)?;

    burn_sync(port, SyncType::DlBoot, 2)?;

    let ret = package_base_info(port, BurnImageType::Bootloader.identifier(), true)?;
    if ret != 0 {
        bail!("Agent boot base_info(BL) failed");
    }

    let mut cmd = Cmd::new(CMD_DOWNLOAD_DATA);
    cmd.len = agent_data.len() as u32;
    let ret = package_data(port, &mut cmd, agent_data, true)?;
    if ret != 0 {
        bail!("Agent boot data download failed");
    }

    log::info!("Agent boot download complete");
    Ok(())
}

/// Burn a single image partition.
fn burn_img(
    port: &mut dyn SerialPort,
    data: &[u8],
    img_type: BurnImageType,
    stor_type: u8,
    addr: u32,
    tag: &str,
    on_progress: &ProgressCallback,
    base_pct: f32,
    pct_range: f32,
) -> Result<()> {
    log::info!("burn image {} {:?} addr=0x{:08X} size={}", tag, img_type, addr, data.len());

    // 1. LPC sync
    burn_sync(port, SyncType::Lpc, 2)?;

    // 2. LPC burn one
    let ret = lpc_burn_one(port, img_type, stor_type)?;
    if ret != 0 {
        bail!("lpc_burn_one failed for {}", tag);
    }

    // 3. AGBOOT sync x2
    burn_sync(port, SyncType::AgBoot, 2)?;
    burn_sync(port, SyncType::AgBoot, 2)?;

    // 4. Base info
    let ret = package_base_info(port, BurnImageType::Head.identifier(), false)?;
    if ret != 0 {
        bail!("base_info failed for {}", tag);
    }

    // 5. Image header
    package_image_head(port, data, img_type, addr, 0, false, 0)?;

    // 6. Data transfer in 64KB blocks
    let mut remain = data.len();
    let mut data_offset: usize = 0;
    let total = data.len();

    while remain > 0 {
        burn_sync(port, SyncType::AgBoot, 2)?;

        let data_len = if remain > MAX_DATA_BLOCK_SIZE {
            MAX_DATA_BLOCK_SIZE
        } else {
            remain
        };

        let mut cmd = Cmd::new(CMD_DOWNLOAD_DATA);
        cmd.len = data_len as u32;
        let ret = package_data(
            port,
            &mut cmd,
            &data[data_offset..data_offset + data_len],
            false,
        )?;
        if ret != 0 {
            bail!("package_data failed for {}", tag);
        }

        data_offset += data_len;
        remain -= data_len;

        let pct = base_pct + pct_range * (data_offset as f32 / total as f32);
        on_progress(&FlashProgress::info(
            "Flashing",
            pct,
            &format!("{}: {}/{}KB", tag, data_offset / 1024, total / 1024),
        ));
    }

    let ret = lpc_get_burn_status(port)?;
    if ret != 0 {
        bail!("burn verification failed for {}", tag);
    }

    log::info!("{} burn complete", tag);
    Ok(())
}

/// Reset device via LPC command.
fn sys_reset(port: &mut dyn SerialPort) -> Result<()> {
    burn_sync(port, SyncType::Lpc, 2)?;
    let ret = lpc_sys_reset(port)?;
    if ret != 0 {
        log::warn!("sys_reset returned non-zero, device may still reset");
    }
    Ok(())
}

// ─── SOC File Parsing Helpers ───────────────────────────────────────────────

/// Extract binpkg from a 7z SOC archive and parse it.
fn extract_and_parse_soc(soc_path: &str) -> Result<(BinpkgResult, Option<Vec<u8>>, u32)> {
    let tempdir = tempfile::tempdir().context("Create temp dir")?;
    let temppath = tempdir.path();

    extract_soc_7z(soc_path, temppath)?;

    // Read info.json for script_addr
    let info_path = temppath.join("info.json");
    let info: serde_json::Value = serde_json::from_reader(
        std::fs::File::open(&info_path).context("info.json missing")?,
    )
    .context("Parse info.json")?;

    let script_addr = info
        .pointer("/download/script_addr")
        .and_then(|v| v.as_str())
        .and_then(|s| u32::from_str_radix(s, 16).ok())
        .unwrap_or(0);

    let force_br = info
        .pointer("/download/force_br")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);

    // Find and parse binpkg
    let mut binpkg_result: Option<BinpkgResult> = None;
    let mut script_data: Option<Vec<u8>> = None;

    for entry in std::fs::read_dir(temppath)? {
        let entry = entry?;
        let fname = entry.file_name().to_string_lossy().to_string();
        if fname.ends_with(".binpkg") {
            let fdata = std::fs::read(entry.path())?;
            binpkg_result = Some(parse_binpkg(&fdata)?);
        } else if fname.ends_with("script.bin") {
            script_data = Some(std::fs::read(entry.path())?);
        }
    }

    let mut result = binpkg_result.ok_or_else(|| anyhow::anyhow!("No .binpkg in SOC"))?;

    // Add script entry from SOC if present
    if let Some(sdata) = script_data {
        if script_addr > 0 {
            result.entries.push(BinpkgEntry {
                name: "script".to_string(),
                addr: script_addr,
                flash_size: 0,
                image_size: sdata.len() as u32,
                hash: String::new(),
                image_type: "AP".to_string(),
                data: Some(sdata),
            });
        }
    }

    Ok((result, None, force_br))
}

/// Helper to extract a 7z SOC file.
fn extract_soc_7z(soc_path: &str, out_dir: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(out_dir)?;
    sevenz_rust2::decompress_file(soc_path, out_dir)
        .with_context(|| format!("7z extraction failed: {soc_path}"))?;
    Ok(())
}

// ─── Map entry to BurnImageType / storage type ──────────────────────────────

fn entry_to_burn_type(entry: &BinpkgEntry) -> Option<(BurnImageType, u8, u32)> {
    let name = entry.name.to_lowercase();
    let itype = entry.image_type.to_uppercase();

    if name.contains("bootloader") || name.contains("bl") {
        Some((BurnImageType::Bootloader, STYPE_AP_FLASH, entry.addr))
    } else if itype == "CP" || name.contains("cp") {
        // EC7xx: CP uses AP_FLASH storage type with adjusted address
        let mut addr = entry.addr;
        if addr >= 0x800000 {
            addr -= 0x800000;
        }
        Some((BurnImageType::Cp, STYPE_AP_FLASH, addr))
    } else if name == "script" {
        Some((BurnImageType::Ap, STYPE_AP_FLASH, entry.addr))
    } else if itype == "AP" || name.contains("ap") || name.contains("system") {
        Some((BurnImageType::Ap, STYPE_AP_FLASH, entry.addr))
    } else if name.contains("flex") || name.contains("rf") {
        let stor = if entry.addr >= 0x800000 { STYPE_AP_FLASH } else { STYPE_CP_FLASH };
        Some((BurnImageType::FlexFile, stor, entry.addr))
    } else {
        Some((BurnImageType::Ap, STYPE_AP_FLASH, entry.addr))
    }
}

// ─── Public API ─────────────────────────────────────────────────────────────

/// USB Boot mode VID/PID for EC718 series.
pub const BOOT_VID: u16 = 0x17D1;
pub const BOOT_PID: u16 = 0x0001;

/// Log port VID/PID for EC718 series.
pub const LOG_VID: u16 = 0x19D1;
pub const LOG_PID: u16 = 0x0001;

/// Try to reboot module into download mode via command port.
///
/// Protocol (from luatools_py3/soc.py):
///   1. Find command port: VID=0x19D1, PID=0x0001, interface x.2
///   2. Open at 115200 baud
///   3. Send AT+ECRST=delay,799\r\n  (delayed reset)
///   4. Wait 200ms
///   5. Send diag frame: 0x7E 0x00 0x02 0x7E  (enter boot mode)
///   6. Wait 800ms, close port
///   7. Module re-enumerates as VID=0x17D1 (boot mode)
///
/// Returns true if the reboot command was sent successfully.
pub fn try_reboot_to_download(on_progress: &ProgressCallback) -> bool {
    // Find command port (VID=0x19D1, PID=0x0001)
    let cmd_port = find_ec718_cmd_port();
    let cmd_port = match cmd_port {
        Some(p) => p,
        None => {
            log::info!("No EC718 command port found (module may already be in boot mode or disconnected)");
            return false;
        }
    };

    on_progress(&FlashProgress::info(
        "Rebooting",
        0.0,
        &format!("发现运行中的模组 {}, 正在发送重启指令...", cmd_port),
    ));
    log::info!("Found EC718 command port {}, sending reboot-to-download sequence", cmd_port);

    let port = serialport::new(&cmd_port, 115200)
        .timeout(Duration::from_millis(500))
        .open();

    let mut port = match port {
        Ok(p) => p,
        Err(e) => {
            log::warn!("Failed to open command port {}: {}", cmd_port, e);
            return false;
        }
    };

    // Step 1: AT+ECRST=delay,799 — trigger delayed reset
    if let Err(e) = port.write_all(b"AT+ECRST=delay,799\r\n") {
        log::warn!("Failed to send AT+ECRST: {}", e);
        return false;
    }
    let _ = port.flush();
    std::thread::sleep(Duration::from_millis(200));

    // Step 2: DIAG frame — force boot mode entry
    // JTT frame: 0x7E (start) 0x00 (len) 0x02 (cmd: enter boot) 0x7E (end)
    if let Err(e) = port.write_all(b"\x7e\x00\x02\x7e") {
        log::warn!("Failed to send DIAG boot frame: {}", e);
        return false;
    }
    let _ = port.flush();
    std::thread::sleep(Duration::from_millis(800));

    drop(port);
    log::info!("Reboot-to-download commands sent, waiting for USB re-enumeration...");
    true
}

/// Find an EC718 port by USB interface number.
///
/// EC718 USB composite device (VID=0x19D1, PID=0x0001) exposes 3 interfaces:
///   interface 2 (x.2) = SOC log + AT command port
///   interface 4 (x.4) = AP log port
///   interface 6 (x.6) = User COM port
///
/// COM port numbers do NOT necessarily correspond to interface order,
/// so we must match by USB interface number, not by sorting COM names.
fn find_ec718_port_by_interface(target_interface: u8) -> Option<String> {
    let ports = serialport::available_ports().ok()?;

    for port in &ports {
        if let serialport::SerialPortType::UsbPort(usb_info) = &port.port_type {
            if usb_info.vid == LOG_VID && usb_info.pid == LOG_PID {
                if usb_info.interface == Some(target_interface) {
                    return Some(port.port_name.clone());
                }
            }
        }
    }

    // Fallback: if interface info unavailable, return any matching port
    for port in &ports {
        if let serialport::SerialPortType::UsbPort(usb_info) = &port.port_type {
            if usb_info.vid == LOG_VID && usb_info.pid == LOG_PID {
                log::warn!(
                    "USB interface number unavailable, falling back to {} (may be wrong port)",
                    port.port_name
                );
                return Some(port.port_name.clone());
            }
        }
    }

    None
}

/// Find the EC718 command port (running mode, USB interface 2 = x.2).
///
/// This is the same physical port as the SOC log port. It handles both
/// AT commands (AT+ECRST for reboot) and binary log output (0x7E frames).
pub fn find_ec718_cmd_port() -> Option<String> {
    find_ec718_port_by_interface(2)
}

/// Find the EC718 SOC log port (running mode, USB interface 2 = x.2).
///
/// The SOC binary log (0x7E HDLC frames) is on the same port as the
/// AT command interface (both on USB interface 2).
pub fn find_ec718_log_port() -> Option<String> {
    find_ec718_port_by_interface(2)
}

/// Find a serial port by USB VID/PID.
pub fn find_port_by_vid_pid(vid: u16, pid: u16) -> Option<String> {
    let ports = serialport::available_ports().ok()?;
    for port in ports {
        if let serialport::SerialPortType::UsbPort(usb_info) = &port.port_type {
            if usb_info.vid == vid && usb_info.pid == pid {
                return Some(port.port_name.clone());
            }
        }
    }
    None
}

/// Wait for a USB boot port to appear, polling every 100ms.
pub fn wait_for_boot_port(timeout_secs: u32) -> Option<String> {
    let max_iterations = timeout_secs * 10;
    for _ in 0..max_iterations {
        if let Some(port) = find_port_by_vid_pid(BOOT_VID, BOOT_PID) {
            return Some(port);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

/// Wait for the EC718 log port (running mode) to appear after reboot.
///
/// After flashing, the module resets and re-enumerates USB as VID=0x19D1.
/// The log port is typically the first (lowest COM number) of the enumerated ports.
pub fn wait_for_log_port(timeout_secs: u32) -> Option<String> {
    let max_iterations = timeout_secs * 10;
    for _ in 0..max_iterations {
        if let Some(port) = find_ec718_log_port() {
            return Some(port);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

/// Automatically detect module state and enter boot mode if needed.
///
/// Flow:
///   1. Check if already in boot mode (VID=0x17D1) → use that port
///   2. If running (VID=0x19D1) → send AT+ECRST + DIAG reboot, wait for boot port
///   3. If neither found → prompt user for manual BOOT button intervention
///   4. If user-specified port is given, try it directly as UART fallback
///
/// Returns the boot port name to use for flashing.
pub fn auto_enter_boot_mode(
    user_port: Option<&str>,
    on_progress: &ProgressCallback,
) -> Result<String> {
    // If user specified a port, check if it's already a boot mode port
    if let Some(port) = user_port {
        if let Ok(ports) = serialport::available_ports() {
            for p in &ports {
                if p.port_name == port {
                    if let serialport::SerialPortType::UsbPort(usb) = &p.port_type {
                        if usb.vid == BOOT_VID && usb.pid == BOOT_PID {
                            log::info!("User-specified port {} is already in boot mode", port);
                            return Ok(port.to_string());
                        }
                    }
                }
            }
        }
    }

    // Step 1: Check if already in boot mode
    if let Some(boot_port) = find_port_by_vid_pid(BOOT_VID, BOOT_PID) {
        on_progress(&FlashProgress::info(
            "Detecting",
            1.0,
            &format!("模组已处于下载模式: {}", boot_port),
        ));
        log::info!("Module already in boot mode on {}", boot_port);
        return Ok(boot_port);
    }

    // Step 2: Check if running and try auto-reboot
    if find_ec718_cmd_port().is_some() {
        on_progress(&FlashProgress::info(
            "Detecting",
            1.0,
            "检测到运行中的模组, 正在自动重启进入下载模式...",
        ));

        if try_reboot_to_download(on_progress) {
            // Wait for boot port to appear (up to 30 seconds)
            on_progress(&FlashProgress::info(
                "Waiting",
                2.0,
                "等待模组重启进入下载模式 (最多30秒)...",
            ));

            if let Some(boot_port) = wait_for_boot_port(30) {
                on_progress(&FlashProgress::info(
                    "Detecting",
                    3.0,
                    &format!("模组已进入下载模式: {}", boot_port),
                ));
                log::info!("Module entered boot mode on {}", boot_port);
                // Give the USB device a moment to fully initialize
                std::thread::sleep(Duration::from_millis(500));
                return Ok(boot_port);
            }

            on_progress(&FlashProgress::info(
                "Warning",
                -1.0,
                "⚠ 自动重启超时. 请手动操作:\n\
                 1. 按住BOOT按钮\n\
                 2. 按下RESET或重新上电\n\
                 3. 松开BOOT按钮\n\
                 等待模组进入下载模式...",
            ));
        }
    } else {
        on_progress(&FlashProgress::info(
            "Warning",
            -1.0,
            "⚠ 未检测到EC718模组. 请:\n\
             1. 确认模组已连接并上电\n\
             2. 按住BOOT按钮, 然后按RESET或重新上电, 再松开BOOT\n\
             等待模组进入下载模式...",
        ));
    }

    // Step 3: Wait for manual boot entry (60 seconds)
    if let Some(boot_port) = wait_for_boot_port(60) {
        on_progress(&FlashProgress::info(
            "Detecting",
            3.0,
            &format!("模组已进入下载模式: {}", boot_port),
        ));
        std::thread::sleep(Duration::from_millis(500));
        return Ok(boot_port);
    }

    // If user specified a port, try it as fallback (UART mode)
    if let Some(port) = user_port {
        on_progress(&FlashProgress::info(
            "Fallback",
            2.0,
            &format!("使用用户指定端口: {} (UART模式)", port),
        ));
        return Ok(port.to_string());
    }

    bail!(
        "无法进入下载模式. 请手动操作:\n\
         1. 按住BOOT按钮\n\
         2. 按下RESET或重新上电\n\
         3. 松开BOOT按钮\n\
         然后重新运行刷机命令."
    );
}

/// Build a SOC log probe frame for EC718 (same protocol as CCM4211/Air1601).
///
/// EC718 firmware buffers log output and won't start sending until a command
/// is received on the debug UART. This probe triggers device-info + log flush.
pub fn build_log_probe() -> Vec<u8> {
    // Reuse the CCM4211 SOC probe frame
    crate::ccm4211::build_log_probe()
}

/// Flash EC718 firmware via native Rust protocol.
///
/// Handles full flash: agentboot + all partitions from .soc file.
pub fn flash_ec718(
    soc_path: &str,
    port: &str,
    on_progress: &ProgressCallback,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    on_progress(&FlashProgress::info("Preparing", 0.0, "Parsing SOC file..."));

    // Parse SOC file
    let (binpkg, _script_data, force_br) = extract_and_parse_soc(soc_path)?;

    log::info!(
        "EC718 chip: {}, {} entries, force_br={}",
        binpkg.chip,
        binpkg.entries.len(),
        force_br,
    );

    // Determine port type and agentboot binary
    let port_type = detect_port_type(port);
    let agentboot = match port_type {
        Ec718PortType::Usb => AGENTBOOT_EC718M_USB,
        Ec718PortType::Uart => AGENTBOOT_EC718M_UART,
    };

    on_progress(&FlashProgress::info(
        "Connecting",
        1.0,
        &format!("Opening {} ({:?} mode)...", port, port_type),
    ));

    // Open serial port
    let baudrate = port_type.baudrate();
    let mut serial = serialport::new(port, baudrate)
        .timeout(Duration::from_millis(800))
        .open()
        .with_context(|| format!("Failed to open serial port {}", port))?;

    serial
        .write_data_terminal_ready(true)
        .context("Failed to set DTR")?;

    if cancel.load(Ordering::Relaxed) {
        bail!("Cancelled");
    }

    // Stage 1: DLBOOT sync
    on_progress(&FlashProgress::info(
        "Connecting",
        2.0,
        "Waiting for bootloader (DLBOOT sync)...\n\
         提示: 如果等待超时, 请手动按住BOOT按钮并复位模组进入下载模式",
    ));

    if let Err(e) = burn_sync(serial.as_mut(), SyncType::DlBoot, 2) {
        on_progress(&FlashProgress::info(
            "Connecting",
            -1.0,
            "⚠ 无法连接到bootloader. 请按住BOOT按钮, 然后复位或重新上电模组, 再重试刷机.",
        ));
        return Err(e).context(
            "DLBOOT sync failed. The module may not be in boot mode.\n\
             Please hold the BOOT button, reset/power-cycle the module, then retry.",
        );
    }

    on_progress(&FlashProgress::info("Connecting", 5.0, "Bootloader connected"));

    if cancel.load(Ordering::Relaxed) {
        bail!("Cancelled");
    }

    // Stage 2: Download agentboot
    on_progress(&FlashProgress::info(
        "AgentBoot",
        6.0,
        &format!("Downloading agentboot ({} bytes)...", agentboot.len()),
    ));

    burn_agboot(serial.as_mut(), agentboot, force_br)?;

    on_progress(&FlashProgress::info("AgentBoot", 15.0, "AgentBoot loaded"));

    if cancel.load(Ordering::Relaxed) {
        bail!("Cancelled");
    }

    // Stage 3: Burn each partition
    let entries: Vec<_> = binpkg
        .entries
        .iter()
        .filter(|e| e.data.is_some())
        .collect();
    let num_entries = entries.len();

    for (idx, entry) in entries.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            bail!("Cancelled");
        }

        let (img_type, stor_type, burn_addr) = match entry_to_burn_type(entry) {
            Some(v) => v,
            None => {
                log::warn!("Skipping unknown entry: {}", entry.name);
                continue;
            }
        };

        let data = entry.data.as_ref().unwrap();
        let base_pct = 15.0 + (idx as f32 / num_entries as f32) * 80.0;
        let pct_range = 80.0 / num_entries as f32;

        on_progress(&FlashProgress::info(
            "Flashing",
            base_pct,
            &format!(
                "[{}/{}] {} ({}KB @ 0x{:X})",
                idx + 1,
                num_entries,
                entry.name,
                data.len() / 1024,
                burn_addr,
            ),
        ));

        burn_img(
            serial.as_mut(),
            data,
            img_type,
            stor_type,
            burn_addr,
            &entry.name,
            on_progress,
            base_pct,
            pct_range,
        )?;
    }

    if cancel.load(Ordering::Relaxed) {
        bail!("Cancelled");
    }

    // Stage 4: Reset
    on_progress(&FlashProgress::info("Resetting", 96.0, "Resetting device..."));
    sys_reset(serial.as_mut())?;

    on_progress(&FlashProgress::done_ok("EC718 flash completed successfully"));
    Ok(())
}

/// Flash only the script partition via native protocol.
pub fn flash_script_ec718(
    soc_path: &str,
    port: &str,
    script_data: &[u8],
    on_progress: &ProgressCallback,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    on_progress(&FlashProgress::info("Preparing", 0.0, "Parsing SOC for script addr..."));

    // Parse SOC to get script_addr and force_br
    let (binpkg, _, force_br) = extract_and_parse_soc(soc_path)?;
    let _ = binpkg; // We only need script_addr from info.json

    // Re-read info.json for script_addr
    let info = luatos_soc::read_soc_info(soc_path)?;
    let script_addr = info
        .download
        .script_addr
        .as_deref()
        .and_then(luatos_soc::parse_addr)
        .unwrap_or(0) as u32;

    if script_addr == 0 {
        bail!("No script_addr found in SOC info.json");
    }

    let port_type = detect_port_type(port);
    let agentboot = match port_type {
        Ec718PortType::Usb => AGENTBOOT_EC718M_USB,
        Ec718PortType::Uart => AGENTBOOT_EC718M_UART,
    };

    on_progress(&FlashProgress::info("Connecting", 1.0, &format!("Opening {}...", port)));

    let baudrate = port_type.baudrate();
    let mut serial = serialport::new(port, baudrate)
        .timeout(Duration::from_millis(800))
        .open()
        .with_context(|| format!("Failed to open serial port {}", port))?;

    serial.write_data_terminal_ready(true)?;

    if cancel.load(Ordering::Relaxed) {
        bail!("Cancelled");
    }

    on_progress(&FlashProgress::info(
        "Connecting",
        2.0,
        "Waiting for bootloader...\n\
         提示: 如果等待超时, 请手动按住BOOT按钮并复位模组进入下载模式",
    ));

    if let Err(e) = burn_sync(serial.as_mut(), SyncType::DlBoot, 2) {
        on_progress(&FlashProgress::info(
            "Connecting",
            -1.0,
            "⚠ 无法连接到bootloader. 请按住BOOT按钮, 然后复位或重新上电模组, 再重试.",
        ));
        return Err(e).context("DLBOOT sync failed - module not in boot mode");
    }

    on_progress(&FlashProgress::info("AgentBoot", 10.0, "Downloading agentboot..."));
    burn_agboot(serial.as_mut(), agentboot, force_br)?;

    if cancel.load(Ordering::Relaxed) {
        bail!("Cancelled");
    }

    on_progress(&FlashProgress::info(
        "Flashing",
        25.0,
        &format!("Writing script ({}KB @ 0x{:X})...", script_data.len() / 1024, script_addr),
    ));

    burn_img(
        serial.as_mut(),
        script_data,
        BurnImageType::Ap,
        STYPE_AP_FLASH,
        script_addr,
        "script",
        on_progress,
        25.0,
        65.0,
    )?;

    on_progress(&FlashProgress::info("Resetting", 95.0, "Resetting device..."));
    sys_reset(serial.as_mut())?;

    on_progress(&FlashProgress::done_ok("Script flash completed successfully"));
    Ok(())
}

/// Flash EC718 via FlashToolCLI.exe subprocess (fallback/debug path).
///
/// This uses the vendor tool directly. The exe and config must be in
/// refs/origin_tools/ec_download/.
#[cfg(target_os = "windows")]
pub fn flash_ec718_exe(
    soc_path: &str,
    port: &str,
    on_progress: &ProgressCallback,
) -> Result<()> {
    use std::process::Command;

    on_progress(&FlashProgress::info("Preparing", 0.0, "Extracting SOC file..."));

    let tempdir = tempfile::tempdir().context("Create temp dir")?;
    let temppath = tempdir.path();
    extract_soc_7z(soc_path, temppath)?;

    // Find binpkg file
    let binpkg_path = std::fs::read_dir(temppath)?
        .filter_map(|e| e.ok())
        .find(|e| {
            e.file_name()
                .to_string_lossy()
                .ends_with(".binpkg")
        })
        .map(|e| e.path())
        .ok_or_else(|| anyhow::anyhow!("No .binpkg in SOC"))?;

    // Find FlashToolCLI.exe
    let exe_dir = std::path::Path::new("refs/origin_tools/ec_download");
    let exe_path = exe_dir.join("FlashToolCLI.exe");
    if !exe_path.exists() {
        bail!(
            "FlashToolCLI.exe not found at {}. \
             This fallback requires the vendor tool.",
            exe_path.display()
        );
    }

    // Extract COM port number
    let port_num = port
        .trim_start_matches("COM")
        .trim_start_matches("com");

    on_progress(&FlashProgress::info(
        "Flashing",
        10.0,
        &format!("Running FlashToolCLI.exe on COM{}...", port_num),
    ));

    let output = Command::new(&exe_path)
        .current_dir(exe_dir)
        .arg("-p")
        .arg(port_num)
        .arg("-f")
        .arg(binpkg_path.to_string_lossy().as_ref())
        .output()
        .with_context(|| format!("Failed to run {}", exe_path.display()))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    log::info!("FlashToolCLI stdout:\n{}", stdout);
    if !stderr.is_empty() {
        log::warn!("FlashToolCLI stderr:\n{}", stderr);
    }

    if !output.status.success() {
        on_progress(&FlashProgress::done_err(&format!(
            "FlashToolCLI.exe failed (exit code: {:?})",
            output.status.code()
        )));
        bail!(
            "FlashToolCLI.exe failed with exit code {:?}\nstdout: {}\nstderr: {}",
            output.status.code(),
            stdout,
            stderr
        );
    }

    on_progress(&FlashProgress::done_ok("EC718 flash (exe) completed"));
    Ok(())
}

#[cfg(not(target_os = "windows"))]
pub fn flash_ec718_exe(
    _soc_path: &str,
    _port: &str,
    _on_progress: &ProgressCallback,
) -> Result<()> {
    bail!("FlashToolCLI.exe fallback is only available on Windows");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crc8_maxim() {
        let data = [0x04, 0x00, 0x00];
        let crc = crc8_maxim(&data);
        assert_ne!(crc, 0); // just verify it computes something
    }

    #[test]
    fn test_self_def_check1() {
        let ck = self_def_check1(0x32, 0, 0xcd, 0x32, 100, &[1, 2, 3]);
        assert_eq!(ck.len(), 4);
    }

    #[test]
    fn test_parse_air8000_soc() {
        let soc = r"d:\github\luatos-cli\refs\soc_files\LuatOS-SoC_V2031_Air8000_105.soc";
        if !std::path::Path::new(soc).exists() {
            eprintln!("Skipping: {} not found", soc);
            return;
        }
        let (result, _, force_br) = extract_and_parse_soc(soc).expect("parse SOC");
        println!("Chip: {}", result.chip);
        println!("Force BR: {}", force_br);
        for entry in &result.entries {
            println!(
                "  {} addr=0x{:08X} size={} type={}",
                entry.name, entry.addr, entry.image_size, entry.image_type
            );
        }
        assert!(!result.entries.is_empty());
        assert!(result.chip.contains("EC718"));
    }

    #[test]
    fn test_img_head_build() {
        let data = vec![0xAA; 1024];
        let hash: [u8; 32] = Sha256::digest(&data).into();
        let mut hd = ImgHead::new();
        hd.set_body_id(AIMG_IDENTIFIER);
        hd.set_img_size(data.len() as u32);
        hd.set_burn_addr(0x24000);
        hd.set_hashv(&hash);
        hd.set_baudrate_ctrl(921600);
        hd.set_hashtype(0xee);
        hd.set_rsvd0(1);
        hd.finalize_hash();

        let packed = hd.pack();
        assert_eq!(packed.len(), IMG_HEAD_SIZE);
    }

    #[test]
    #[ignore = "requires Air8000 physically connected in boot mode"]
    fn ec718_live_flash() {
        let soc = r"d:\github\luatos-cli\refs\soc_files\LuatOS-SoC_V2031_Air8000_105.soc";
        let port = "COM3";
        let cancel = Arc::new(AtomicBool::new(false));
        let on_progress: ProgressCallback = Box::new(|p| {
            eprintln!("[{:>6.1}%] {} — {}", p.percent, p.stage, p.message);
        });
        flash_ec718(soc, port, &on_progress, cancel).expect("flash_ec718");
    }
}
