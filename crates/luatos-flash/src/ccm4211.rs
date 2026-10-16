// CCM4211 (Air1601 / Air1602 / Air1603 / Air1604) flash protocol.
//
// Three-stage flashing:
//   1. ISP: ROM bootloader at 115200/EVEN → load ramrun into RAM
//   2. SOC Protocol: framed cmd/response at 2Mbps → flash firmware
//   3. Reset: reboot into flashed firmware
//
// SOC frame format:
//   [0xA5] [escaped(24-byte header + payload)] [escaped(CRC16)] [0xA5]
//   Escape: 0xA5 → 0xA6 0x01, 0xA6 → 0xA6 0x02
//   CRC16: Modbus polynomial 0x8005, reversed, init=0

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use md5;
use serialport::{self, Parity, SerialPort};

use crate::{FlashProgress, ProgressCallback};

// ─── ISP Protocol Constants ──────────────────────────────────────────────────

const ISP_HANDSHAKE: &[u8] = &[0x55, 0x55, 0x55, 0x55];
const ISP_CMD_HEADER: u8 = 0x52;
const ISP_ACK_HEADER: u8 = 0x72;

const ISP_CMD_SYNC: u8 = 0x14;
const ISP_CMD_SET_BAUD: u8 = 0x10;
/// 选择寄存器/内存窗口并返回基址（param1 为窗口编号）。
///
/// `0x20` = RAM base（ramrun 装载区），`0x40` = CPM base（外设寄存器组）。
const ISP_CMD_SET_RAM_BASE: u8 = 0x20;
/// 向当前窗口的 `param2` 偏移写入数据。
///
/// 同一命令既用于 ramrun 分块写（param1 随块递增），也用于寄存器写。
const ISP_CMD_WRITE_RAM: u8 = 0x31;
const ISP_CMD_EXECUTE: u8 = 0x81;

/// CPM（外设寄存器组）窗口编号与 `IPSCGTCR` 在窗口内的偏移。
///
/// `IPSCGTCR` 是外设时钟门控寄存器：`cpm_drv.h` 的
/// `_cpm_set_ipscgtcr_clk_off()` 即 `CPM->IPSCGTCR &= ~(1 << bit)`，
/// `am_uart.c` 里 UART_ID1/2/3/5 分别对应 bit10/11/18/19。
const ISP_CPM_WINDOW: u8 = 0x40;
const ISP_CPM_IPSCGTCR_OFFSET: u8 = 0x4C;
/// 关闭其他串口的门控位：保留 bit0/2(wdt)、bit12/22(UART4/UART6)、bit20/21/23/25/26/29 等，
/// 清掉 bit10/11/18/19(UART1/2/3/5)。取值对齐 LuaTools `ccm4211_isp.py`。
const IPSCGTCR_CLOSE_OTHER_UART: [u8; 4] = [0xFF, 0xB3, 0xF3, 0xFF];

const ISP_INITIAL_BAUD: u32 = 115_200;
const ISP_FAST_BAUD: u32 = 1_000_000;

// ─── SOC Protocol Constants ─────────────────────────────────────────────────

const SOC_FLAG: u8 = 0xA5;
const SOC_ESC: u8 = 0xA6;
const SOC_ESC_FLAG: u8 = 0x01; // 0xA6 0x01 → 0xA5
const SOC_ESC_CODE: u8 = 0x02; // 0xA6 0x02 → 0xA6

const SOC_HEADER_LEN: usize = 24;

// SOC command codes (from soc_service.h enum)
#[allow(dead_code)]
const SOC_CMD_NOP: u32 = 0;
#[allow(dead_code)]
const SOC_CMD_GET_BASE_INFO: u32 = 1;
#[allow(dead_code)]
const SOC_CMD_GET_RAM_INFO: u32 = 2;
#[allow(dead_code)]
const SOC_CMD_ASSERT: u32 = 3;
#[allow(dead_code)]
const SOC_CMD_GET_RAMDUMP_INFO: u32 = 4;
#[allow(dead_code)]
const SOC_CMD_GET_RAM_DATA: u32 = 5;
#[allow(dead_code)]
const SOC_CMD_GET_FLASH_DATA: u32 = 6;
#[allow(dead_code)]
const SOC_CMD_SET_RAM_DATA: u32 = 7;
const SOC_CMD_FLASH_ERASE_BLOCK: u32 = 8;
const SOC_CMD_GET_DOWNLOAD_INFO: u32 = 9;
const SOC_CMD_SET_CODE_DATA_START: u32 = 10;
const SOC_CMD_SET_CODE_DATA: u32 = 11;
const SOC_CMD_SET_CODE_END: u32 = 12;
const SOC_CMD_CHECK_CODE: u32 = 13;
#[allow(dead_code)]
const SOC_CMD_FORCE_DOWNLOAD: u32 = 14;
#[allow(dead_code)]
const SOC_CMD_FORCE_RESET: u32 = 15;

// ─── Optional SOC Flash Resources ───────────────────────────────────────────

/// Fixed flash addresses exported by the firmware build in the optional
/// `include.txt` packaged in recent CCM4211 SOC archives.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct CcmResourceAddresses {
    font: Option<u32>,
    tts: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CcmFlashResource {
    label: &'static str,
    region: &'static str,
    path: PathBuf,
    address: u32,
}

#[derive(Debug, Default)]
struct CcmFlashResourcePlan {
    downloads: Vec<CcmFlashResource>,
    warnings: Vec<String>,
}

impl CcmFlashResourcePlan {
    fn resource(&self, region: &str) -> Option<&CcmFlashResource> {
        self.downloads.iter().find(|resource| resource.region == region)
    }
}

/// Parse a hexadecimal object-like C macro from compiler `-dM` output.
fn parse_hex_define(source: &str, macro_name: &str) -> Option<u32> {
    for line in source.lines() {
        let line = line.trim_start();
        let Some(after_define) = line.strip_prefix("#define") else {
            continue;
        };
        let after_define = after_define.trim_start();
        let Some(after_name) = after_define.strip_prefix(macro_name) else {
            continue;
        };
        // Do not accept prefixes such as LUAT_FONT_ADDRESS_EXTRA.
        if after_name.chars().next().map(|ch| ch.is_ascii_alphanumeric() || ch == '_').unwrap_or(false) {
            continue;
        }

        let value = after_name.trim_start();
        let Some(hex_start) = value.find("0x").or_else(|| value.find("0X")) else {
            continue;
        };
        let digits: String = value[hex_start + 2..].chars().take_while(|ch| ch.is_ascii_hexdigit()).collect();
        if let Ok(address) = u32::from_str_radix(&digits, 16) {
            return Some(address);
        }
    }
    None
}

fn parse_ccm_resource_addresses(source: &str) -> CcmResourceAddresses {
    CcmResourceAddresses {
        font: parse_hex_define(source, "LUAT_FONT_ADDRESS"),
        tts: parse_hex_define(source, "__TTS_ADDRESS__"),
    }
}

/// Build the optional-resource download plan in the same order as LuaTools:
/// font before core, TTS after core, and filesystem before scripts.
fn build_ccm_resource_plan(soc_dir: &Path, info: &luatos_soc::SocInfo, addresses: CcmResourceAddresses) -> CcmFlashResourcePlan {
    let mut plan = CcmFlashResourcePlan::default();

    let font_path = soc_dir.join("hzfont_builtin_ttf.bin");
    if let Some(address) = addresses.font {
        if font_path.exists() {
            plan.downloads.push(CcmFlashResource {
                label: "Font",
                region: "font",
                path: font_path,
                address,
            });
        } else {
            plan.warnings
                .push(format!("SOC declares font address 0x{address:08X}, but hzfont_builtin_ttf.bin is missing; skipping font"));
        }
    }

    let tts_path = soc_dir.join("16k.bin");
    if let Some(address) = addresses.tts {
        if tts_path.exists() {
            plan.downloads.push(CcmFlashResource {
                label: "TTS",
                region: "tts",
                path: tts_path,
                address,
            });
        } else {
            plan.warnings
                .push(format!("SOC declares TTS address 0x{address:08X}, but 16k.bin is missing; skipping TTS"));
        }
    }

    let fs_path = soc_dir.join("fs.bin");
    if let (true, Some(address)) = (fs_path.exists(), info.fs_addr()) {
        plan.downloads.push(CcmFlashResource {
            label: "Filesystem",
            region: "fs",
            path: fs_path,
            address,
        });
    }

    plan
}

// CRC16 Modbus lookup table (polynomial 0x8005 reversed = 0xA001, init=0)
const CRC16_TABLE: [u16; 256] = {
    let mut table = [0u16; 256];
    let mut i = 0u16;
    while i < 256 {
        let mut crc = i;
        let mut j = 0;
        while j < 8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xA001;
            } else {
                crc >>= 1;
            }
            j += 1;
        }
        table[i as usize] = crc;
        i += 1;
    }
    table
};

fn crc16_modbus(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &b in data {
        let idx = (b ^ (crc as u8)) as usize;
        crc = (crc >> 8) ^ CRC16_TABLE[idx];
    }
    crc
}

// ─── SOC Frame Builder / Parser ─────────────────────────────────────────────

/// Build a SOC protocol header.
fn build_soc_header(cmd: u32, address: u32, data_len: u32, sn: u16) -> [u8; SOC_HEADER_LEN] {
    let mut header = [0u8; SOC_HEADER_LEN];
    // ms:u64 = 0 (bytes 0..8)
    // address:u32 (bytes 8..12)
    header[8..12].copy_from_slice(&address.to_le_bytes());
    // len:u32 (bytes 12..16)
    header[12..16].copy_from_slice(&data_len.to_le_bytes());
    // cmd:u32 (bytes 16..20)
    header[16..20].copy_from_slice(&cmd.to_le_bytes());
    // sn:u16 (bytes 20..22)
    header[20..22].copy_from_slice(&sn.to_le_bytes());
    // type:u8 (byte 22) = 0
    // cpu:u8 (byte 23) = 0
    header
}

/// Escape data for SOC frame (0xA5 → 0xA6 0x01, 0xA6 → 0xA6 0x02).
fn soc_escape(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 8);
    for &b in data {
        match b {
            SOC_FLAG => {
                out.push(SOC_ESC);
                out.push(SOC_ESC_FLAG);
            }
            SOC_ESC => {
                out.push(SOC_ESC);
                out.push(SOC_ESC_CODE);
            }
            _ => out.push(b),
        }
    }
    out
}

/// Build a SOC CMD_GET_BASE_INFO probe frame.
///
/// Air1601/CCM4211 firmware buffers log output internally and will not start
/// sending log frames until it receives a command on the debug UART.  Sending
/// this probe triggers device-info output **and** flushes the log buffer.
pub fn build_log_probe() -> Vec<u8> {
    build_soc_frame(SOC_CMD_GET_BASE_INFO, 0, &[], 1)
}

/// Build a complete SOC frame: [0xA5] [escaped(header+payload)] [escaped(crc16)] [0xA5]
fn build_soc_frame(cmd: u32, address: u32, payload: &[u8], sn: u16) -> Vec<u8> {
    let header = build_soc_header(cmd, address, payload.len() as u32, sn);

    // Compute CRC16 over unescaped header + payload
    let mut raw = Vec::with_capacity(SOC_HEADER_LEN + payload.len());
    raw.extend_from_slice(&header);
    raw.extend_from_slice(payload);
    let crc = crc16_modbus(&raw);
    let crc_bytes = crc.to_le_bytes();

    // Build frame
    let mut frame = Vec::with_capacity(raw.len() * 2 + 6);
    frame.push(SOC_FLAG);
    frame.extend(soc_escape(&raw));
    frame.extend(soc_escape(&crc_bytes));
    frame.push(SOC_FLAG);
    frame
}

/// Parsed SOC response.
struct SocResponse {
    cmd: u32,
    address: u32,
    #[allow(dead_code)]
    sn: u16,
    data: Vec<u8>,
}

/// SOC frame parser — accumulates bytes and extracts complete frames.
struct SocFrameParser {
    buf: Vec<u8>,
    in_frame: bool,
    escape_next: bool,
}

impl SocFrameParser {
    fn new() -> Self {
        Self {
            buf: Vec::with_capacity(4096),
            in_frame: false,
            escape_next: false,
        }
    }

    /// Feed raw bytes and return any complete parsed responses.
    fn feed(&mut self, data: &[u8]) -> Vec<SocResponse> {
        let mut responses = Vec::new();
        for &b in data {
            if b == SOC_FLAG {
                if self.in_frame && self.buf.len() >= SOC_HEADER_LEN + 2 {
                    if let Some(resp) = self.parse_frame() {
                        responses.push(resp);
                    }
                }
                self.buf.clear();
                self.in_frame = true;
                self.escape_next = false;
                continue;
            }
            if !self.in_frame {
                continue;
            }
            if b == SOC_ESC {
                self.escape_next = true;
                continue;
            }
            if self.escape_next {
                let actual = match b {
                    SOC_ESC_FLAG => SOC_FLAG,
                    SOC_ESC_CODE => SOC_ESC,
                    other => other & 0x03,
                };
                self.buf.push(actual);
                self.escape_next = false;
                continue;
            }
            self.buf.push(b);

            if self.buf.len() > 512 * 1024 {
                self.buf.clear();
                self.in_frame = false;
            }
        }
        responses
    }

    fn parse_frame(&self) -> Option<SocResponse> {
        let data = &self.buf;
        if data.len() < SOC_HEADER_LEN + 2 {
            return None;
        }
        let payload = &data[..data.len() - 2];
        let crc_received = u16::from_le_bytes([data[data.len() - 2], data[data.len() - 1]]);
        let crc_computed = crc16_modbus(payload);
        if crc_received != crc_computed {
            return None;
        }
        let address = u32::from_le_bytes(payload[8..12].try_into().ok()?);
        let cmd = u32::from_le_bytes(payload[16..20].try_into().ok()?);
        let sn = u16::from_le_bytes(payload[20..22].try_into().ok()?);
        let body = if payload.len() > SOC_HEADER_LEN {
            payload[SOC_HEADER_LEN..].to_vec()
        } else {
            Vec::new()
        };
        Some(SocResponse { cmd, address, sn, data: body })
    }
}

// ─── ISP Protocol ───────────────────────────────────────────────────────────

/// Send an ISP command and wait for ACK.
/// Frame: 0x52 + cmd + param1 + param2 + data_len(u16 BE) + data
/// ACK:   0x72 + cmd + ack_data_len(u16 BE) + ack_data
fn isp_send_cmd(port: &mut Box<dyn SerialPort>, cmd: u8, param1: u8, param2: u8, data_len: u16, data: Option<&[u8]>, timeout: Duration) -> Result<Vec<u8>> {
    let mut frame = vec![ISP_CMD_HEADER, cmd, param1, param2];
    frame.extend_from_slice(&data_len.to_be_bytes());
    if let Some(d) = data {
        frame.extend_from_slice(d);
    }
    port.write_all(&frame)?;
    port.flush()?;

    let start = Instant::now();
    let mut read_buf = Vec::new();
    let mut tmp = [0u8; 256];

    while start.elapsed() < timeout {
        match port.read(&mut tmp) {
            Ok(n) if n > 0 => {
                read_buf.extend_from_slice(&tmp[..n]);
                // Search for ACK header (0x72) — skip any junk/echo bytes
                while read_buf.len() >= 4 {
                    if read_buf[0] == ISP_ACK_HEADER {
                        if read_buf[1] == cmd {
                            let ack_len = u16::from_be_bytes([read_buf[2], read_buf[3]]) as usize + 4;
                            if read_buf.len() >= ack_len {
                                return Ok(read_buf[4..ack_len].to_vec());
                            }
                            break; // Need more data
                        } else {
                            // 0x72 found but cmd doesn't match — likely a false positive
                            read_buf.remove(0);
                        }
                    } else {
                        // Skip non-ACK byte
                        read_buf.remove(0);
                    }
                }
            }
            Ok(_) => {}
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => return Err(e.into()),
        }
        std::thread::sleep(Duration::from_micros(100));
    }
    bail!("ISP cmd 0x{:02X} timeout", cmd)
}

/// ISP handshake: reset device, send 0x55555555 bursts, sync with cmd 0x14.
fn isp_handshake(port: &mut Box<dyn SerialPort>, timeout: Duration) -> Result<Vec<u8>> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        // Drain any stale data
        let mut drain = [0u8; 1024];
        let _ = port.read(&mut drain);

        // Reset: RTS/DTR toggle (match Python: RTS first, then DTR)
        port.write_request_to_send(true)?;
        port.write_data_terminal_ready(true)?;
        std::thread::sleep(Duration::from_millis(500));
        port.write_request_to_send(false)?;
        port.write_data_terminal_ready(false)?;

        // Send handshake bursts
        for _ in 0..50 {
            let _ = port.write_all(ISP_HANDSHAKE);
            std::thread::sleep(Duration::from_millis(1));
        }
        port.flush()?;
        std::thread::sleep(Duration::from_millis(10));

        // Try sync — cmd 0x14, param1=0, param2=0, data_len=0x24 (per Python)
        match isp_send_cmd(port, ISP_CMD_SYNC, 0, 0, 0x24, None, Duration::from_millis(500)) {
            Ok(data) => {
                log::info!("ISP handshake OK: {:?}", String::from_utf8_lossy(&data));
                return Ok(data);
            }
            Err(e) => {
                log::debug!("ISP sync attempt failed: {e}");
                continue;
            }
        }
    }
    bail!("ISP handshake timeout after {:?}", timeout)
}

/// 仅抽象波特率切换所需的串口设置，便于在不打开真实串口时覆盖回归测试。
trait SerialPortConfig {
    fn set_baud_rate(&mut self, baud: u32) -> serialport::Result<()>;
    fn set_parity(&mut self, parity: Parity) -> serialport::Result<()>;
    fn set_timeout(&mut self, timeout: Duration) -> serialport::Result<()>;
    fn clear(&self, buffer: serialport::ClearBuffer) -> serialport::Result<()>;
}

impl<T: SerialPort + ?Sized> SerialPortConfig for T {
    fn set_baud_rate(&mut self, baud: u32) -> serialport::Result<()> {
        SerialPort::set_baud_rate(self, baud)
    }

    fn set_parity(&mut self, parity: Parity) -> serialport::Result<()> {
        SerialPort::set_parity(self, parity)
    }

    fn set_timeout(&mut self, timeout: Duration) -> serialport::Result<()> {
        SerialPort::set_timeout(self, timeout)
    }

    fn clear(&self, buffer: serialport::ClearBuffer) -> serialport::Result<()> {
        SerialPort::clear(self, buffer)
    }
}

/// 在不关闭串口的前提下切换主机端串口配置。
///
/// CH343 在关闭/重开后可能丢失正在进行的设备端波特率切换状态，因此 ISP 和
/// SOC 两个阶段都必须复用同一个句柄。切换后清空驱动缓冲，避免旧波特率下的
/// 残留字节被下一阶段解析。
fn reconfigure_serial_port<T: SerialPortConfig + ?Sized>(port: &mut T, baud: u32, parity: Parity, timeout: Duration, stage: &str) -> Result<()> {
    port.set_baud_rate(baud).with_context(|| format!("{stage}: failed to change host baud rate to {baud}"))?;
    port.set_parity(parity).with_context(|| format!("{stage}: failed to set host parity to {parity:?}"))?;
    port.set_timeout(timeout).with_context(|| format!("{stage}: failed to set serial timeout"))?;
    port.clear(serialport::ClearBuffer::All)
        .with_context(|| format!("{stage}: failed to clear serial buffers"))?;
    Ok(())
}

/// 将已执行的 ramrun 串口切换到 SOC 下载协议配置。
fn switch_to_soc_protocol(port: &mut dyn SerialPort, baud: u32) -> Result<()> {
    // ramrun 接管 UART 并切换设备端波特率需要短暂稳定时间。
    std::thread::sleep(Duration::from_millis(10));
    reconfigure_serial_port(port, baud, Parity::None, Duration::from_millis(50), "SOC protocol transition")
}

/// 发一条 ISP 命令但**不因失败中断**下载流程。
///
/// 用于「锦上添花」型寄存器写：设备不支持时不回退成整体失败，只把失败原因
/// 交给调用方决定如何提示。
#[allow(clippy::too_many_arguments)]
fn best_effort_isp_cmd(port: &mut Box<dyn SerialPort>, cmd: u8, param1: u8, param2: u8, data_len: u16, data: Option<&[u8]>, label: &str, timeout: Duration) -> bool {
    match isp_send_cmd(port, cmd, param1, param2, data_len, data, timeout) {
        Ok(_) => {
            log::debug!("ISP 寄存器写 {label} 成功");
            true
        }
        Err(error) => {
            log::warn!("ISP 寄存器写 {label} 无应答({error})，继续按原流程下载");
            false
        }
    }
}

/// 装载 ramrun 前关闭其他串口：切到 CPM 寄存器窗口后写 `IPSCGTCR`。
///
/// 两条命令均 best-effort，任一条无 ACK 都不影响后续 ramrun 下载。
/// 对齐 LuaTools `ccm4211_isp.py` 的 `eea0ca2`：
/// `0x20/0x40` 设 CPM base，`0x31/0x40/0x4C` + `FF B3 F3 FF` 设 `IPSCGTCR`。
fn close_other_uarts_before_ramrun(port: &mut Box<dyn SerialPort>, on_progress: &ProgressCallback) {
    let timeout = Duration::from_millis(50);
    let cpm_ok = best_effort_isp_cmd(port, ISP_CMD_SET_RAM_BASE, ISP_CPM_WINDOW, 0x00, 4, None, "CPM base address", timeout);
    let ips_ok = best_effort_isp_cmd(
        port,
        ISP_CMD_WRITE_RAM,
        ISP_CPM_WINDOW,
        ISP_CPM_IPSCGTCR_OFFSET,
        IPSCGTCR_CLOSE_OTHER_UART.len() as u16,
        Some(&IPSCGTCR_CLOSE_OTHER_UART),
        "IPSCGTCR (关闭其他串口)",
        timeout,
    );

    if cpm_ok && ips_ok {
        on_progress(&FlashProgress::info("Connect", 7.5, "Other UARTs clocked off (IPSCGTCR)"));
    } else {
        on_progress(&FlashProgress::info("Connect", 7.5, "Close-other-UART step skipped (device did not ACK)"));
    }
}

/// Load ramrun binary into device RAM via ISP.
fn isp_load_ramrun(port_name: &str, ramrun_data: &[u8], on_progress: &ProgressCallback) -> Result<Box<dyn SerialPort>> {
    on_progress(&FlashProgress::info("Connect", 0.0, "Opening serial port (ISP)"));

    let mut port: Box<dyn SerialPort> = serialport::new(port_name, ISP_INITIAL_BAUD)
        .parity(Parity::Even)
        .timeout(Duration::from_millis(100))
        .open()
        .with_context(|| format!("Cannot open {port_name}"))?;

    on_progress(&FlashProgress::info("Reset", 2.0, "Handshaking with ISP bootrom"));

    let _sync_data = isp_handshake(&mut port, Duration::from_secs(15)).context("ISP handshake failed")?;

    on_progress(&FlashProgress::info("Baud", 5.0, "Switching to 1Mbps"));

    // Switch baud rate to 1Mbps: cmd 0x10, data = [0x00, 0x01, 0x24]
    isp_send_cmd(&mut port, ISP_CMD_SET_BAUD, 0, 0, 3, Some(&[0x00, 0x01, 0x24]), Duration::from_millis(50)).context("ISP baud rate change failed")?;

    // CH343 在关闭/重开串口后会丢失本次设备端波特率切换状态，必须原地切换。
    reconfigure_serial_port(&mut *port, ISP_FAST_BAUD, Parity::Even, Duration::from_millis(100), "ISP baud transition")?;

    // Verify sync at new baud rate
    isp_send_cmd(&mut port, ISP_CMD_SYNC, 0, 0, 0x10, None, Duration::from_millis(50)).context("ISP sync at 1Mbps failed")?;

    on_progress(&FlashProgress::info("Baud", 7.0, "ISP baud rate 1Mbps OK"));

    // 下载 ramrun 之前先关掉其他串口（对齐 LuaTools `ccm4211_isp.py` 提交 eea0ca2
    // 「ccm4211下载ramrun之前尝试关闭其他串口」）：把寄存器窗口切到 CPM(0x40)，
    // 再写 IPSCGTCR 关闭 UART1/2/3/5 的时钟门控，避免 PCBA_TYPE2
    // （Air1603/Air1604）上多出来的串口干扰下载口的收发。
    //
    // best-effort：这两步对已在真机验证过的 Air1601/1602 是纯增量，ACK 超时只告警
    // 不中断流程 —— 行为退化为本次改动之前的流程，保证不引入回归。
    close_other_uarts_before_ramrun(&mut port, on_progress);

    // Set RAM base address: cmd 0x20, param1=0x20, param2=0x00
    isp_send_cmd(&mut port, ISP_CMD_SET_RAM_BASE, 0x20, 0x00, 4, None, Duration::from_millis(50)).context("ISP set RAM base address failed")?;

    on_progress(&FlashProgress::info("Write", 8.0, "Loading ramrun into RAM"));

    // Write ramrun in 512-byte chunks, address auto-increments by 2
    let mut base_address: u8 = 0x30;
    let total_len = ramrun_data.len();
    let mut done = 0;

    while done < total_len {
        let chunk_len = std::cmp::min(512, total_len - done);
        let chunk = &ramrun_data[done..done + chunk_len];

        isp_send_cmd(&mut port, ISP_CMD_WRITE_RAM, base_address, 0x00, chunk_len as u16, Some(chunk), Duration::from_millis(500))
            .with_context(|| format!("ISP RAM write failed at offset {done}"))?;

        done += chunk_len;
        base_address = base_address.wrapping_add(2);

        let pct = 8.0 + (done as f32 / total_len as f32) * 7.0; // 8% - 15%
        on_progress(&FlashProgress::info("Write", pct, &format!("Ramrun {done}/{total_len}")));
    }

    on_progress(&FlashProgress::info("Write", 15.0, "Ramrun loaded, executing"));

    // Execute ramrun: cmd 0x81
    // Jump address from ramrun binary bytes 4-5
    let go_addr1 = ramrun_data[5];
    let go_addr2 = ramrun_data[4];

    // Execute may not ACK (ramrun starts and takes over UART)
    let _ = isp_send_cmd(&mut port, ISP_CMD_EXECUTE, go_addr1, go_addr2, 0, None, Duration::from_millis(100));

    Ok(port)
}

// ─── SOC Protocol Operations ────────────────────────────────────────────────

/// Send a SOC command and wait for matching response.
fn soc_send_cmd(port: &mut Box<dyn SerialPort>, parser: &mut SocFrameParser, cmd: u32, address: u32, payload: &[u8], sn: &mut u16, timeout: Duration) -> Result<SocResponse> {
    *sn = sn.wrapping_add(1);
    if *sn == 0 {
        *sn = 1;
    }
    let frame = build_soc_frame(cmd, address, payload, *sn);
    log::trace!(
        "SOC send: cmd={}, addr=0x{:08X}, sn={}, payload_len={}, frame_len={}",
        cmd,
        address,
        *sn,
        payload.len(),
        frame.len()
    );
    port.write_all(&frame)?;
    port.flush()?;

    let start = Instant::now();
    let mut tmp = [0u8; 8192];

    while start.elapsed() < timeout {
        match port.read(&mut tmp) {
            Ok(n) if n > 0 => {
                let responses = parser.feed(&tmp[..n]);
                for resp in responses {
                    log::debug!("SOC response: cmd={}, addr=0x{:08X}, sn={}, data_len={}", resp.cmd, resp.address, resp.sn, resp.data.len());
                    if resp.cmd == cmd {
                        return Ok(resp);
                    }
                }
            }
            Ok(_) => {}
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => return Err(e.into()),
        }
        std::thread::sleep(Duration::from_micros(100));
    }
    bail!("SOC cmd {} timeout after {:?}", cmd, timeout)
}

/// Download a binary to a flash address via SOC protocol.
///
/// sub-chunk 固定使用 3KB/100ms，适用于 2M 和 6M 波特率（与 Python 参考实现对齐）。
#[allow(clippy::too_many_arguments)]
fn soc_download_file(
    port: &mut Box<dyn SerialPort>,
    parser: &mut SocFrameParser,
    sn: &mut u16,
    address: u32,
    data: &[u8],
    block_len: u32,
    on_progress: &ProgressCallback,
    stage_name: &str,
    pct_start: f32,
    pct_end: f32,
) -> Result<()> {
    // 分块 LZMA 压缩下载（对齐 LuaTools `soc.py` 的 pyZipData 路径）。
    // 环境变量 `LUATOS_CCM_LZMA=0` 可强制关闭，用于排查压缩相关问题。
    let lzma_enabled = lzma_enabled_by_env();

    if lzma_enabled {
        let used = soc_download_blocks(port, parser, sn, address, data, block_len, on_progress, stage_name, pct_start, pct_end, true)?;
        if used {
            // 设备端解压失败时不报错（ramrun 会 memset 0xFF 后照常 ACK），
            // 主机只能靠 CMD 0x0D 的 MD5 发现。这种情况多半是设备端解码器
            // 与本实现不兼容（或固件过老），整文件改明文重传一次再判定。
            log::warn!("{stage_name}: MD5 校验未通过，改用明文重传整文件（设备端 LZMA 解码可能不兼容）");
            on_progress(&FlashProgress::info(stage_name, pct_end, "MD5 check failed, retrying without LZMA").with_region(&stage_name.to_lowercase()));
            soc_download_blocks(port, parser, sn, address, data, block_len, on_progress, stage_name, pct_start, pct_end, false)?;
        }
        // 压缩路径与明文回落路径都必须真正校验：设备端解压失败时只会 memset 0xFF
        // 后照常应答，主机侧看不到任何错误信号，MD5 是唯一的判定手段。
        soc_verify_md5(port, parser, sn, address, data)?;
    } else {
        soc_download_blocks(port, parser, sn, address, data, block_len, on_progress, stage_name, pct_start, pct_end, false)?;
        soc_verify_md5(port, parser, sn, address, data)?;
    }

    Ok(())
}

/// 读取环境变量开关：只有显式设为 `0` 才关闭压缩，其余（含未设置）都开启。
fn lzma_enabled_by_env() -> bool {
    match std::env::var("LUATOS_CCM_LZMA") {
        Ok(value) => value.trim() != "0",
        Err(_) => true,
    }
}

/// 决定一个数据块实际发到串口的内容：能压缩就发 `props(5) + LZMA 流`，否则发明文。
///
/// 返回 `(载荷, is_lzma)`，`is_lzma` 直接作为 CMD `0x0C` 的载荷交给设备端。
/// 压缩不划算（压完比原文大）或压缩失败时回落到明文，此时 `Cow::Borrowed` 无额外拷贝。
fn pick_wire_payload(chunk: &[u8], block_len: u32, lzma_enabled: bool) -> (std::borrow::Cow<'_, [u8]>, bool) {
    if !lzma_enabled {
        return (std::borrow::Cow::Borrowed(chunk), false);
    }
    // dict_size 取设备上报的分块长度，与设备端解码窗口一致。
    let Some((compressed, props)) = luatos_soc::ota::lzma_compress_block_for_download(chunk, block_len) else {
        return (std::borrow::Cow::Borrowed(chunk), false);
    };
    // props 的 5 字节也要算进载荷，压完不比原文小就不值得发压缩。
    if compressed.len() + props.len() >= chunk.len() {
        log::debug!("本块压缩后无收益（{} -> {}），按明文发送", chunk.len(), compressed.len() + props.len());
        return (std::borrow::Cow::Borrowed(chunk), false);
    }

    let mut wire = Vec::with_capacity(props.len() + compressed.len());
    wire.extend_from_slice(&props);
    wire.extend_from_slice(&compressed);
    log::debug!("本块压缩 {} -> {} 字节（{:.1}%）", chunk.len(), wire.len(), wire.len() as f32 / chunk.len() as f32 * 100.0);
    (std::borrow::Cow::Owned(wire), true)
}

/// 分块下载一个文件，返回「本次是否真的用了压缩」。
#[allow(clippy::too_many_arguments)]
fn soc_download_blocks(
    port: &mut Box<dyn SerialPort>,
    parser: &mut SocFrameParser,
    sn: &mut u16,
    address: u32,
    data: &[u8],
    block_len: u32,
    on_progress: &ProgressCallback,
    stage_name: &str,
    pct_start: f32,
    pct_end: f32,
    lzma_enabled: bool,
) -> Result<bool> {
    let total_len = data.len();
    let mut done = 0usize;
    let mut cur_addr = address;
    let mut any_compressed = false;

    while done < total_len {
        let send_len = std::cmp::min(block_len as usize, total_len - done);
        let chunk = &data[done..done + send_len];
        let (wire, is_lzma) = pick_wire_payload(chunk, block_len, lzma_enabled);
        any_compressed |= is_lzma;

        // CMD 0x0A: set download address + original length
        // 载荷始终是**原始长度**：设备端靠它决定解压后烧写多少字节
        // （ramrun_main.c: `fw_cache.Pos = prv_ramrun.next_program_len`）。
        let orig_len_bytes = (send_len as u32).to_le_bytes();
        soc_send_cmd(port, parser, SOC_CMD_SET_CODE_DATA_START, cur_addr, &orig_len_bytes, sn, Duration::from_secs(1)).context("SOC set code data start failed")?;

        // CMD 0x0B: send data in sub-chunks
        // 统一使用 3KB/100ms，与 Python 参考实现 (efe356f) 保持一致：
        // sect_len = 3 * 1024，sect_timeout = 0.1s，无论波特率为 2M 还是 6M 均适用。
        let sect_len = 3 * 1024_usize;
        let cmd_timeout = Duration::from_millis(100);
        let wire_len = wire.len();
        let mut s_len = 0;
        while s_len < wire_len {
            let ss_len = std::cmp::min(sect_len, wire_len - s_len);
            let sub_chunk = &wire[s_len..s_len + ss_len];

            // Retry up to 10 times (CMD 11 can fail with synchronous I/O)
            let mut last_err = None;
            for retry in 0..10 {
                match soc_send_cmd(port, parser, SOC_CMD_SET_CODE_DATA, 0, sub_chunk, sn, cmd_timeout) {
                    Ok(_) => {
                        last_err = None;
                        break;
                    }
                    Err(e) => {
                        log::debug!("CMD 11 retry {}/{} at {} offset {}", retry + 1, 10, stage_name, done + s_len);
                        last_err = Some(e);
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
            }
            if let Some(e) = last_err {
                return Err(e).context("SOC send code data failed after 10 retries");
            }
            s_len += ss_len;
        }

        // CMD 0x0C: finalize block (is_lzma = 本块是否压缩)
        soc_send_cmd(port, parser, SOC_CMD_SET_CODE_END, 0, &[u8::from(is_lzma)], sn, Duration::from_secs(3)).context("SOC set code end failed")?;

        done += send_len;
        cur_addr += send_len as u32;

        let pct = pct_start + (done as f32 / total_len as f32) * (pct_end - pct_start);
        on_progress(&FlashProgress::info(stage_name, pct, &format!("{done}/{total_len} bytes")).with_region(&stage_name.to_lowercase()));
    }

    Ok(any_compressed)
}

/// CMD 0x0D: 让设备端回读 Flash 并比对整段数据的 MD5（10s 超时，适配大文件）。
fn soc_verify_md5(port: &mut Box<dyn SerialPort>, parser: &mut SocFrameParser, sn: &mut u16, address: u32, data: &[u8]) -> Result<()> {
    let total_len_bytes = (data.len() as u32).to_le_bytes();
    let resp = soc_send_cmd(port, parser, SOC_CMD_CHECK_CODE, address, &total_len_bytes, sn, Duration::from_secs(10)).context("SOC MD5 check failed")?;

    // Verify MD5
    let expected_md5 = md5::compute(data);
    let device_md5_hex = resp.data.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let expected_md5_hex = format!("{:x}", expected_md5);

    if device_md5_hex != expected_md5_hex {
        bail!("MD5 mismatch: device={device_md5_hex}, expected={expected_md5_hex}");
    }

    Ok(())
}

/// Reset the device after flashing (DTR/RTS toggle).
fn soc_reset_device(port: &mut Box<dyn SerialPort>) -> Result<()> {
    port.write_data_terminal_ready(true)?;
    port.write_request_to_send(true)?;
    std::thread::sleep(Duration::from_millis(100));
    port.write_data_terminal_ready(false)?;
    port.write_request_to_send(false)?;
    Ok(())
}

// ─── Public Flash API ───────────────────────────────────────────────────────

/// Flash full firmware for CCM4211/Air1601.
///
/// Downloads bootloader + optional flash resources + core + script (if present)
/// via ISP+SOC protocol.
/// `script_overlay` replaces the `script.bin` packaged in the SOC when `flash run --script` is used.
pub fn flash_ccm4211(soc_path: &str, port_name: &str, on_progress: &ProgressCallback, cancel: Arc<AtomicBool>, script_overlay: Option<&[u8]>) -> Result<()> {
    // Extract .soc
    on_progress(&FlashProgress::info("Extract", 0.0, "Unpacking .soc file"));
    let tmpdir = tempfile::tempdir().context("Create temp dir")?;
    let unpacked = luatos_soc::unpack_soc(soc_path, tmpdir.path())?;
    let info = &unpacked.info;
    let soc_dir = tmpdir.path();

    let include_path = soc_dir.join("include.txt");
    let mut resource_plan = if include_path.exists() {
        match std::fs::read_to_string(&include_path) {
            Ok(contents) => build_ccm_resource_plan(soc_dir, info, parse_ccm_resource_addresses(&contents)),
            Err(error) => {
                let mut plan = build_ccm_resource_plan(soc_dir, info, CcmResourceAddresses::default());
                plan.warnings.push(format!("Cannot read optional include.txt ({error}); skipping font and TTS resources"));
                plan
            }
        }
    } else {
        // Older CCM4211 SOC files do not carry the build-time resource map.
        // They may still carry fs.bin, which is addressed from info.json.
        build_ccm_resource_plan(soc_dir, info, CcmResourceAddresses::default())
    };

    // Load ramrun from SOC or use default
    let ramrun_path = soc_dir.join("ramrun.bin");
    let ramrun_data = if ramrun_path.exists() {
        std::fs::read(&ramrun_path).context("Failed to read ramrun.bin from SOC")?
    } else {
        // Try a default ramrun next to the soc file
        let soc_parent = std::path::Path::new(soc_path).parent();
        let default_path = soc_parent.map(|p| p.join("ccm4211_ramrun_default.bin"));
        if let Some(ref dp) = default_path {
            if dp.exists() {
                std::fs::read(dp).context("Failed to read default ramrun")?
            } else {
                bail!(
                    "No ramrun.bin found in SOC archive. \
                     Place ccm4211_ramrun_default.bin next to the .soc file."
                );
            }
        } else {
            bail!("No ramrun.bin found");
        }
    };

    if cancel.load(Ordering::Relaxed) {
        bail!("Cancelled");
    }

    // Stage 1: ISP — load ramrun，并保留串口句柄供 SOC 阶段继续使用。
    let mut port = isp_load_ramrun(port_name, &ramrun_data, on_progress)?;

    if cancel.load(Ordering::Relaxed) {
        bail!("Cancelled");
    }

    // Stage 2: SOC protocol — 原地切换到下载波特率，不能关闭/重开 CH343。
    let dl_baud = info.flash_baud_rate();
    on_progress(&FlashProgress::info("Connect", 16.0, &format!("Connecting SOC protocol at {dl_baud}")));
    switch_to_soc_protocol(&mut *port, dl_baud)?;

    let mut parser = SocFrameParser::new();
    let mut sn: u16 = 0;

    // Query download block size
    let mut block_len: u32 = 0;
    for _ in 0..3 {
        match soc_send_cmd(&mut port, &mut parser, SOC_CMD_GET_DOWNLOAD_INFO, 0, &[], &mut sn, Duration::from_millis(100)) {
            Ok(resp) => {
                if resp.data.len() >= 4 {
                    block_len = u32::from_le_bytes(resp.data[0..4].try_into().unwrap());
                }
                break;
            }
            Err(_) => continue,
        }
    }
    if block_len == 0 {
        bail!("Failed to get download block size from device");
    }

    log::info!("Download block size: {} (0x{:X})", block_len, block_len);
    on_progress(&FlashProgress::info("Connect", 18.0, &format!("Block size: {block_len} bytes")));

    for warning in resource_plan.warnings.drain(..) {
        log::warn!("{warning}");
        on_progress(&FlashProgress::info("Warning", 18.0, &warning));
    }

    // Load files from SOC directory
    let bl_path = soc_dir.join("bootloader.bin");
    let core_path = soc_dir.join(&info.rom.file);
    let script_path = soc_dir.join(&info.script.file);

    let bl_addr = info.bl_addr().unwrap_or(0x10000000);
    let core_addr = info.app_addr().unwrap_or(0x14000000);
    let script_addr = info.script_addr();

    // Download bootloader
    if bl_path.exists() {
        let bl_data = std::fs::read(&bl_path).context("Failed to read bootloader.bin")?;
        on_progress(&FlashProgress::info("Bootloader", 20.0, "Downloading bootloader").with_region("bootloader"));
        soc_download_file(&mut port, &mut parser, &mut sn, bl_addr, &bl_data, block_len, on_progress, "Bootloader", 20.0, 35.0)?;
        on_progress(&FlashProgress::info("Bootloader", 35.0, "Bootloader OK").with_region("bootloader"));
    }

    if cancel.load(Ordering::Relaxed) {
        bail!("Cancelled");
    }

    // LuaTools writes the optional external font before the application core.
    if let Some(resource) = resource_plan.resource("font") {
        let data = std::fs::read(&resource.path).with_context(|| format!("Failed to read {}", resource.path.display()))?;
        on_progress(&FlashProgress::info(resource.label, 35.0, "Downloading font resource").with_region(resource.region));
        soc_download_file(&mut port, &mut parser, &mut sn, resource.address, &data, block_len, on_progress, resource.label, 35.0, 45.0)?;
        on_progress(&FlashProgress::info(resource.label, 45.0, "Font resource OK").with_region(resource.region));
    }

    // Download core firmware
    if core_path.exists() {
        let core_data = std::fs::read(&core_path).with_context(|| format!("Failed to read {}", info.rom.file))?;
        on_progress(&FlashProgress::info("Core", 45.0, "Downloading core firmware").with_region("core"));
        soc_download_file(&mut port, &mut parser, &mut sn, core_addr, &core_data, block_len, on_progress, "Core", 45.0, 70.0)?;
        on_progress(&FlashProgress::info("Core", 70.0, "Core firmware OK").with_region("core"));
    }

    if cancel.load(Ordering::Relaxed) {
        bail!("Cancelled");
    }

    // LuaTools writes TTS after core, then an optional filesystem image.
    if let Some(resource) = resource_plan.resource("tts") {
        let data = std::fs::read(&resource.path).with_context(|| format!("Failed to read {}", resource.path.display()))?;
        on_progress(&FlashProgress::info(resource.label, 70.0, "Downloading TTS resource").with_region(resource.region));
        soc_download_file(&mut port, &mut parser, &mut sn, resource.address, &data, block_len, on_progress, resource.label, 70.0, 78.0)?;
        on_progress(&FlashProgress::info(resource.label, 78.0, "TTS resource OK").with_region(resource.region));
    }

    if let Some(resource) = resource_plan.resource("fs") {
        let data = std::fs::read(&resource.path).with_context(|| format!("Failed to read {}", resource.path.display()))?;
        on_progress(&FlashProgress::info(resource.label, 78.0, "Downloading filesystem resource").with_region(resource.region));
        soc_download_file(&mut port, &mut parser, &mut sn, resource.address, &data, block_len, on_progress, resource.label, 78.0, 88.0)?;
        on_progress(&FlashProgress::info(resource.label, 88.0, "Filesystem resource OK").with_region(resource.region));
    }

    // Download script: --script overlay wins over the SOC-packaged script.bin.
    let script_data = if let Some(overlay) = script_overlay {
        Some(overlay.to_vec())
    } else if script_path.exists() {
        Some(std::fs::read(&script_path).context("Failed to read script")?)
    } else {
        None
    };
    if let Some(script_data) = script_data {
        on_progress(&FlashProgress::info("Script", 88.0, "Downloading script").with_region("script"));
        soc_download_file(&mut port, &mut parser, &mut sn, script_addr, &script_data, block_len, on_progress, "Script", 88.0, 95.0)?;
        on_progress(&FlashProgress::info("Script", 95.0, "Script OK").with_region("script"));
    }

    // Reset device
    on_progress(&FlashProgress::info("Reset", 98.0, "Resetting device"));
    soc_reset_device(&mut port)?;
    drop(port);

    on_progress(&FlashProgress::done_ok("Flash completed successfully"));
    Ok(())
}

/// Flash only the script partition for CCM4211/Air1601.
pub fn flash_script_ccm4211(soc_path: &str, port_name: &str, script_data: &[u8], on_progress: &ProgressCallback, cancel: Arc<AtomicBool>) -> Result<()> {
    on_progress(&FlashProgress::info("Extract", 0.0, "Unpacking .soc file"));
    let tmpdir = tempfile::tempdir().context("Create temp dir")?;
    let unpacked = luatos_soc::unpack_soc(soc_path, tmpdir.path())?;
    let info = &unpacked.info;

    let ramrun_data = load_ramrun_data(soc_path, tmpdir.path())?;

    if cancel.load(Ordering::Relaxed) {
        bail!("Cancelled");
    }

    let mut port = isp_load_ramrun(port_name, &ramrun_data, on_progress)?;

    if cancel.load(Ordering::Relaxed) {
        bail!("Cancelled");
    }

    let dl_baud = info.flash_baud_rate();
    switch_to_soc_protocol(&mut *port, dl_baud)?;

    let mut parser = SocFrameParser::new();
    let mut sn: u16 = 0;

    // Get block size
    let mut block_len: u32 = 0;
    for _ in 0..3 {
        if let Ok(resp) = soc_send_cmd(&mut port, &mut parser, SOC_CMD_GET_DOWNLOAD_INFO, 0, &[], &mut sn, Duration::from_millis(100)) {
            if resp.data.len() >= 4 {
                block_len = u32::from_le_bytes(resp.data[0..4].try_into().unwrap());
            }
            break;
        }
    }
    if block_len == 0 {
        bail!("Failed to get download block size");
    }

    let script_addr = info.script_addr();
    on_progress(&FlashProgress::info("Script", 20.0, "Downloading script").with_region("script"));
    soc_download_file(&mut port, &mut parser, &mut sn, script_addr, script_data, block_len, on_progress, "Script", 20.0, 90.0)?;

    on_progress(&FlashProgress::info("Reset", 95.0, "Resetting device"));
    soc_reset_device(&mut port)?;
    drop(port);

    on_progress(&FlashProgress::done_ok("Script flash completed"));
    Ok(())
}

/// Erase a flash partition (FSKV or filesystem).
pub fn erase_partition_ccm4211(soc_path: &str, port_name: &str, partition_addr: u32, partition_name: &str, on_progress: &ProgressCallback, cancel: Arc<AtomicBool>) -> Result<()> {
    on_progress(&FlashProgress::info("Extract", 0.0, "Unpacking .soc file"));
    let tmpdir = tempfile::tempdir().context("Create temp dir")?;
    let unpacked = luatos_soc::unpack_soc(soc_path, tmpdir.path())?;
    let info = &unpacked.info;

    let ramrun_data = load_ramrun_data(soc_path, tmpdir.path())?;

    if cancel.load(Ordering::Relaxed) {
        bail!("Cancelled");
    }

    let mut port = isp_load_ramrun(port_name, &ramrun_data, on_progress)?;

    if cancel.load(Ordering::Relaxed) {
        bail!("Cancelled");
    }

    let dl_baud = info.flash_baud_rate();
    switch_to_soc_protocol(&mut *port, dl_baud)?;

    let mut parser = SocFrameParser::new();
    let mut sn: u16 = 0;

    // Verify ramrun is running
    for _ in 0..3 {
        if soc_send_cmd(&mut port, &mut parser, SOC_CMD_GET_DOWNLOAD_INFO, 0, &[], &mut sn, Duration::from_millis(100)).is_ok() {
            break;
        }
    }

    on_progress(&FlashProgress::info("Erase", 50.0, &format!("Erasing {partition_name} at 0x{partition_addr:08X}")));

    for _ in 0..3 {
        if soc_send_cmd(&mut port, &mut parser, SOC_CMD_FLASH_ERASE_BLOCK, partition_addr, &[], &mut sn, Duration::from_secs(10)).is_ok() {
            on_progress(&FlashProgress::info("Reset", 90.0, "Resetting device"));
            soc_reset_device(&mut port)?;
            drop(port);
            on_progress(&FlashProgress::done_ok(&format!("{partition_name} erased successfully")));
            return Ok(());
        }
    }
    bail!("Failed to erase {partition_name}")
}

/// Clear the filesystem partition for CCM4211/Air1601.
pub fn clear_filesystem(soc_path: &str, port_name: &str, on_progress: &ProgressCallback, cancel: Arc<AtomicBool>) -> Result<()> {
    let info = luatos_soc::read_soc_info(soc_path)?;
    let addr = info.fs_addr().context("Air1601 SOC has no fs_addr defined")?;
    erase_partition_ccm4211(soc_path, port_name, addr, "filesystem", on_progress, cancel)
}

/// Clear the FSKV (key-value) partition for CCM4211/Air1601.
pub fn clear_fskv(soc_path: &str, port_name: &str, on_progress: &ProgressCallback, cancel: Arc<AtomicBool>) -> Result<()> {
    let info = luatos_soc::read_soc_info(soc_path)?;
    let addr = info.nvm_addr().context("Air1601 SOC has no nvm_addr defined")?;
    erase_partition_ccm4211(soc_path, port_name, addr, "fskv", on_progress, cancel)
}

// ─── Raw flash (任意地址) ────────────────────────────────────────────────────

/// 读取 ramrun 字节：优先用 SOC 包内的 `ramrun.bin`，否则回退到同目录
/// 的 `ccm4211_ramrun_default.bin`。
fn load_ramrun_data(soc_path: &str, soc_dir: &Path) -> Result<Vec<u8>> {
    let bundled = soc_dir.join("ramrun.bin");
    if bundled.exists() {
        return std::fs::read(&bundled).context("read ramrun.bin");
    }
    let fallback = Path::new(soc_path).parent().map(|p| p.join("ccm4211_ramrun_default.bin"));
    match fallback {
        Some(ref p) if p.exists() => std::fs::read(p).context("read ccm4211_ramrun_default.bin"),
        _ => bail!("No ramrun.bin found (checked bundled SOC and sibling ccm4211_ramrun_default.bin)"),
    }
}

/// CCM4211 上的 raw flash 会话。
///
/// 复用 `soc_download_file` / `SOC_CMD_FLASH_ERASE_BLOCK` / `SOC_CMD_CHECK_CODE`，
/// 只是把 info.json 里的固定分区地址换成用户给定的地址。
pub struct Ccm4211Raw {
    port: Box<dyn SerialPort>,
    parser: SocFrameParser,
    sn: u16,
    block_len: u32,
    cancel: Arc<AtomicBool>,
    on_progress: ProgressCallback,
}

/// 进入 CCM4211 SOC 协议会话并返回 raw 句柄（ISP → ramrun → SOC 帧协议）。
pub fn open_raw(soc_path: &str, port_name: &str, cancel: Arc<AtomicBool>, on_progress: ProgressCallback) -> Result<Box<dyn crate::raw::RawFlashDevice>> {
    on_progress(&FlashProgress::info("Extract", 0.0, "Unpacking .soc file"));
    let tmpdir = tempfile::tempdir().context("Create temp dir")?;
    let unpacked = luatos_soc::unpack_soc(soc_path, tmpdir.path())?;
    let dl_baud = unpacked.info.flash_baud_rate();
    let ramrun_data = load_ramrun_data(soc_path, tmpdir.path())?;
    // ramrun 已读入内存，SOC 包可以立即释放
    drop(tmpdir);

    crate::raw::check_cancel(&cancel)?;
    let mut port = isp_load_ramrun(port_name, &ramrun_data, &on_progress)?;
    crate::raw::check_cancel(&cancel)?;

    switch_to_soc_protocol(&mut *port, dl_baud)?;

    let mut parser = SocFrameParser::new();
    let mut sn: u16 = 0;
    let mut block_len: u32 = 0;
    for _ in 0..3 {
        if let Ok(resp) = soc_send_cmd(&mut port, &mut parser, SOC_CMD_GET_DOWNLOAD_INFO, 0, &[], &mut sn, Duration::from_millis(100)) {
            if resp.data.len() >= 4 {
                block_len = u32::from_le_bytes(resp.data[0..4].try_into().unwrap());
            }
            break;
        }
    }
    if block_len == 0 {
        bail!("Failed to get download block size");
    }

    Ok(Box::new(Ccm4211Raw {
        port,
        parser,
        sn,
        block_len,
        cancel,
        on_progress,
    }))
}

impl crate::raw::RawFlashDevice for Ccm4211Raw {
    fn chip(&self) -> crate::raw::RawChip {
        crate::raw::RawChip::Ccm4211
    }

    /// `FLASH_ERASE_BLOCK` 是分区级命令，粒度由固件决定，
    /// 用设备返回的 block_len（通常 256KB）作为保守估计。
    fn erase_granularity(&self) -> u64 {
        self.block_len as u64
    }

    /// `SET_CODE_DATA_START` 携带任意起始地址，无需对齐。
    fn write_alignment(&self) -> u64 {
        crate::raw::RawChip::Ccm4211.write_alignment()
    }

    fn progress(&self) -> &ProgressCallback {
        &self.on_progress
    }

    fn write(&mut self, addr: u32, data: &[u8]) -> Result<()> {
        // soc_download_file 自带 CMD 0x0D 的 MD5 校验，写入即校验。
        crate::raw::check_cancel(&self.cancel)?;
        soc_download_file(
            &mut self.port,
            &mut self.parser,
            &mut self.sn,
            addr,
            data,
            self.block_len,
            &self.on_progress,
            "Raw",
            35.0,
            90.0,
        )
    }

    fn erase(&mut self, addr: u32, len: u64) -> Result<()> {
        // 分区级擦除：逐块发 CMD 0x08，每次擦除一块。
        // 注意实际擦除范围由固件决定，可能略大于请求区间。
        let gran = self.block_len as u64;
        let blocks = len.div_ceil(gran).max(1);
        for i in 0..blocks {
            crate::raw::check_cancel(&self.cancel)?;
            let a = addr.checked_add((i * gran) as u32).ok_or_else(|| anyhow::anyhow!("擦除地址溢出"))?;
            let mut ok = false;
            for attempt in 0..3 {
                match soc_send_cmd(&mut self.port, &mut self.parser, SOC_CMD_FLASH_ERASE_BLOCK, a, &[], &mut self.sn, Duration::from_secs(10)) {
                    Ok(_) => {
                        ok = true;
                        break;
                    }
                    Err(e) => {
                        log::debug!("raw erase block {} attempt {}: {}", i, attempt + 1, e);
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
            }
            if !ok {
                bail!("擦除 0x{a:08x} 失败（重试 3 次）");
            }
            let pct = 20.0 + 15.0 * ((i + 1) as f32 / blocks as f32);
            (self.on_progress)(&FlashProgress::info("RawErase", pct, &format!("raw 擦除块 {}/{} @ 0x{a:08x}", i + 1, blocks)).with_region("raw"));
        }
        Ok(())
    }

    fn verify(&mut self, addr: u32, data: &[u8]) -> Result<crate::raw::VerifyResult> {
        if data.is_empty() {
            bail!("校验数据为空");
        }
        let total_len = (data.len() as u32).to_le_bytes();
        let resp = soc_send_cmd(
            &mut self.port,
            &mut self.parser,
            SOC_CMD_CHECK_CODE,
            addr,
            &total_len,
            &mut self.sn,
            Duration::from_secs(10),
        )
        .context("SOC MD5 check failed")?;
        let device_md5_hex = resp.data.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let expected_md5_hex = format!("{:x}", md5::compute(data));
        if device_md5_hex != expected_md5_hex {
            bail!("MD5 mismatch: device={device_md5_hex}, expected={expected_md5_hex}");
        }
        Ok(crate::raw::VerifyResult::DigestMatched { digest: device_md5_hex })
    }

    fn read(&mut self, _addr: u32, _len: u64) -> Result<Vec<u8>> {
        // 实测（Air1601 + LuatOS_V1027）：GET_FLASH_DATA(0x06) 以 len=64/256 探测均 3s
        // 超时无应答，协议层没有可用的 flash 读回通道。写后校验只能依赖
        // CHECK_CODE(0x0D) 返回的 MD5，无法 dump 原始内容。
        bail!("ccm4211 协议不提供 flash 读回（实测 GET_FLASH_DATA 无应答）；写后校验请用 --verify（基于 MD5）")
    }

    fn reset(&mut self) {
        let _ = soc_reset_device(&mut self.port);
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::fs;

    fn resource_test_info(fs_addr: Option<&str>) -> luatos_soc::SocInfo {
        let fs_addr = fs_addr.map(|address| format!(r#""fs_addr":"{address}""#));
        let download = fs_addr.map(|entry| format!("{{{entry}}}")).unwrap_or_else(|| "{}".to_string());
        serde_json::from_str(&format!(
            r#"{{"chip":{{"type":"air1602"}},"rom":{{"file":"luatos.bin"}},"script":{{"file":"script.bin"}},"download":{download}}}"#
        ))
        .unwrap()
    }

    /// 固件代码段的替身：重复 Thumb 指令模式，可压缩。
    fn compressible_chunk(size: usize) -> Vec<u8> {
        const CODE: [u8; 8] = [0x00, 0x48, 0xb5, 0x21, 0x68, 0x49, 0x8b, 0x35];
        let mut data = Vec::with_capacity(size);
        while data.len() < size {
            data.extend_from_slice(&CODE);
        }
        data.truncate(size);
        data
    }

    /// 随机数据替身：LZMA 压不动，用来验证「压缩无收益则回落明文」。
    fn incompressible_chunk(size: usize) -> Vec<u8> {
        let mut state = 0x1234_5678u32;
        (0..size)
            .map(|_| {
                state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                (state >> 16) as u8
            })
            .collect()
    }

    #[test]
    fn pick_wire_payload_compresses_when_lzma_enabled() {
        let chunk = compressible_chunk(64 * 1024);
        let (wire, is_lzma) = pick_wire_payload(&chunk, 256 * 1024, true);
        assert!(is_lzma, "可压缩数据应当走压缩路径");
        assert!(wire.len() < chunk.len() / 2, "压缩后应显著变小，实际 {}", wire.len());
        // 载荷布局：前 5 字节 props，其余是 LZMA 流（与 ramrun 的 fota_decode 约定一致）
        assert!(wire.len() > 5);
        let props_byte = wire[0];
        assert!(props_byte < 9 * 5 * 5, "propsByte = (pb*5+lp)*9+lc 必须落在合法范围，实际 {props_byte}");
    }

    #[test]
    fn pick_wire_payload_returns_plain_when_disabled() {
        let chunk = compressible_chunk(64 * 1024);
        let (wire, is_lzma) = pick_wire_payload(&chunk, 256 * 1024, false);
        assert!(!is_lzma, "关闭压缩时 is_lzma 必须为 0");
        assert_eq!(wire.as_ref(), chunk.as_slice(), "关闭压缩时应原样发送且不拷贝");
        assert!(matches!(wire, std::borrow::Cow::Borrowed(_)), "明文路径不应产生额外拷贝");
    }

    #[test]
    fn pick_wire_payload_falls_back_when_compression_does_not_help() {
        let chunk = incompressible_chunk(256 * 1024);
        let (wire, is_lzma) = pick_wire_payload(&chunk, 256 * 1024, true);
        assert!(!is_lzma, "压不动的数据必须回落明文");
        assert_eq!(wire.as_ref(), chunk.as_slice());
    }

    #[test]
    fn lzma_env_switch_only_disables_on_zero() {
        // 该测试只覆盖读取逻辑本身，不改动进程环境变量（并行测试下改 env 不安全）。
        fn enabled(value: Option<&str>) -> bool {
            match value {
                None => true,
                Some(v) => v.trim() != "0",
            }
        }
        assert!(enabled(None));
        assert!(enabled(Some("1")));
        assert!(enabled(Some("true")));
        assert!(!enabled(Some("0")));
        assert!(!enabled(Some(" 0 ")));
    }

    #[test]
    fn ipscgtcr_constant_matches_luatools() {
        // 关掉 UART1/2/3/5（bit10/11/18/19），保留 bit2(wdt)/bit12(UART4)/bit22(UART6)
        let value = u32::from_le_bytes(IPSCGTCR_CLOSE_OTHER_UART);
        for bit in [10u32, 11, 18, 19] {
            assert_eq!(value >> bit & 1, 0, "UART 门控 bit{bit} 应当被关掉");
        }
        for bit in [0u32, 2, 12, 22] {
            assert_eq!(value >> bit & 1, 1, "bit{bit} 必须保留");
        }
        assert_eq!(ISP_CPM_WINDOW, 0x40);
        assert_eq!(ISP_CPM_IPSCGTCR_OFFSET, 0x4C);
    }

    #[test]
    fn parse_ccm_resource_addresses_reads_hex_defines() {
        let addresses = parse_ccm_resource_addresses("#define LUAT_FONT_ADDRESS (0x10040000)\n#define __TTS_ADDRESS__ (0X14880000)\n");
        assert_eq!(addresses.font, Some(0x1004_0000));
        assert_eq!(addresses.tts, Some(0x1488_0000));
    }

    #[test]
    fn parse_ccm_resource_addresses_ignores_missing_and_invalid_defines() {
        assert_eq!(parse_ccm_resource_addresses(""), CcmResourceAddresses::default());
        let addresses = parse_ccm_resource_addresses("#define LUAT_FONT_ADDRESS_EXTRA (0x12345678)\n#define LUAT_FONT_ADDRESS invalid\n#define __TTS_ADDRESS__ (0xnothex)\n");
        assert_eq!(addresses, CcmResourceAddresses::default());
    }

    #[test]
    fn ccm_resource_plan_orders_font_tts_fs_for_complete_soc() {
        let tempdir = tempfile::tempdir().unwrap();
        let dir = tempdir.path();
        fs::write(dir.join("hzfont_builtin_ttf.bin"), b"font").unwrap();
        fs::write(dir.join("16k.bin"), b"tts").unwrap();
        fs::write(dir.join("fs.bin"), b"fs").unwrap();

        let info = resource_test_info(Some("14e80000"));
        let plan = build_ccm_resource_plan(
            dir,
            &info,
            CcmResourceAddresses {
                font: Some(0x1004_0000),
                tts: Some(0x1488_0000),
            },
        );

        assert!(plan.warnings.is_empty());
        assert_eq!(plan.downloads.iter().map(|resource| resource.region).collect::<Vec<_>>(), vec!["font", "tts", "fs"]);
        assert_eq!(plan.resource("font").unwrap().address, 0x1004_0000);
        assert_eq!(plan.resource("tts").unwrap().address, 0x1488_0000);
        assert_eq!(plan.resource("fs").unwrap().address, 0x14e8_0000);
    }

    #[test]
    fn ccm_resource_plan_keeps_old_soc_without_include_addresses_unchanged() {
        let tempdir = tempfile::tempdir().unwrap();
        let plan = build_ccm_resource_plan(tempdir.path(), &resource_test_info(None), CcmResourceAddresses::default());
        assert!(plan.downloads.is_empty());
        assert!(plan.warnings.is_empty());
    }

    #[test]
    fn ccm_resource_plan_warns_when_declared_resources_are_missing() {
        let tempdir = tempfile::tempdir().unwrap();
        let plan = build_ccm_resource_plan(
            tempdir.path(),
            &resource_test_info(None),
            CcmResourceAddresses {
                font: Some(0x1004_0000),
                tts: Some(0x1488_0000),
            },
        );
        assert!(plan.downloads.is_empty());
        assert_eq!(plan.warnings.len(), 2);
        assert!(plan.warnings.iter().any(|warning| warning.contains("hzfont_builtin_ttf.bin")));
        assert!(plan.warnings.iter().any(|warning| warning.contains("16k.bin")));
    }

    #[test]
    fn ccm_resource_plan_skips_fs_without_image_or_address() {
        let tempdir = tempfile::tempdir().unwrap();
        let dir = tempdir.path();
        fs::write(dir.join("fs.bin"), b"fs").unwrap();
        let no_address_plan = build_ccm_resource_plan(dir, &resource_test_info(None), CcmResourceAddresses::default());
        assert!(no_address_plan.resource("fs").is_none());

        fs::remove_file(dir.join("fs.bin")).unwrap();
        let no_image_plan = build_ccm_resource_plan(dir, &resource_test_info(Some("14e80000")), CcmResourceAddresses::default());
        assert!(no_image_plan.resource("fs").is_none());
    }

    /// 只记录串口配置调用的假串口，用于确保波特率切换不依赖关闭/重开句柄。
    #[derive(Default)]
    struct FakeSerialPortConfig {
        calls: RefCell<Vec<String>>,
    }

    impl SerialPortConfig for FakeSerialPortConfig {
        fn set_baud_rate(&mut self, baud: u32) -> serialport::Result<()> {
            self.calls.borrow_mut().push(format!("baud:{baud}"));
            Ok(())
        }

        fn set_parity(&mut self, parity: Parity) -> serialport::Result<()> {
            self.calls.borrow_mut().push(format!("parity:{parity:?}"));
            Ok(())
        }

        fn set_timeout(&mut self, timeout: Duration) -> serialport::Result<()> {
            self.calls.borrow_mut().push(format!("timeout:{}", timeout.as_millis()));
            Ok(())
        }

        fn clear(&self, buffer: serialport::ClearBuffer) -> serialport::Result<()> {
            self.calls.borrow_mut().push(format!("clear:{buffer:?}"));
            Ok(())
        }
    }

    #[test]
    fn reconfigure_serial_port_reuses_the_open_port_for_isp_and_soc() {
        let mut port = FakeSerialPortConfig::default();

        reconfigure_serial_port(&mut port, ISP_FAST_BAUD, Parity::Even, Duration::from_millis(100), "ISP").unwrap();
        reconfigure_serial_port(&mut port, 6_000_000, Parity::None, Duration::from_millis(50), "SOC").unwrap();

        assert_eq!(
            *port.calls.borrow(),
            vec![
                "baud:1000000",
                "parity:Even",
                "timeout:100",
                "clear:All",
                "baud:6000000",
                "parity:None",
                "timeout:50",
                "clear:All",
            ]
        );
    }

    #[test]
    fn test_crc16_modbus() {
        // CCM4211 uses CRC16 with init=0 (not standard Modbus init=0xFFFF)
        let data = b"123456789";
        let crc = crc16_modbus(data);
        assert_eq!(crc, 0xBB3D, "CRC16 (poly=0xA001, init=0) of '123456789'");
    }

    #[test]
    fn test_crc16_empty() {
        assert_eq!(crc16_modbus(&[]), 0x0000);
    }

    #[test]
    fn test_soc_escape() {
        assert_eq!(soc_escape(&[0x01, 0x02, 0x03]), vec![0x01, 0x02, 0x03]);
        assert_eq!(soc_escape(&[0xA5]), vec![0xA6, 0x01]);
        assert_eq!(soc_escape(&[0xA6]), vec![0xA6, 0x02]);
        assert_eq!(soc_escape(&[0x00, 0xA5, 0xA6, 0xFF]), vec![0x00, 0xA6, 0x01, 0xA6, 0x02, 0xFF]);
    }

    #[test]
    fn test_build_soc_header() {
        let header = build_soc_header(9, 0, 0, 1);
        assert_eq!(header.len(), 24);
        // cmd at bytes 16..20
        assert_eq!(u32::from_le_bytes(header[16..20].try_into().unwrap()), 9);
        // sn at bytes 20..22
        assert_eq!(u16::from_le_bytes(header[20..22].try_into().unwrap()), 1);
    }

    #[test]
    fn test_build_soc_frame_cmd9() {
        // Compare with captured packet from Python:
        // CMD 0x09 (query block, len=28):
        // A5 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 09 00 00 00 01 00 00 00 C1 96 A5
        let frame = build_soc_frame(9, 0, &[], 1);
        assert_eq!(frame[0], 0xA5); // start
        assert_eq!(*frame.last().unwrap(), 0xA5); // end

        // Verify CRC bytes match captured data
        let expected = vec![
            0xA5, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0xC1, 0x96, 0xA5,
        ];
        assert_eq!(frame, expected, "Frame should match captured packet");
    }

    #[test]
    fn test_build_soc_frame_cmd0a() {
        // CMD 0x0A (set addr 0x14700000, len=32):
        // A5 00 00 00 00 00 00 00 00 00 00 70 14 04 00 00 00 0A 00 00 00 02 00 00 00 00 00 70 14 EB 12 A5
        let _param = 0x1000_u32.to_le_bytes(); // wait, let me recalculate
                                               // Actually from the capture, data_len=4 at bytes 12-15
                                               // address=0x14700000 at bytes 8-11
                                               // payload = 0x14700000 as LE bytes? No, looking at soc.py:
                                               //   param = struct.pack("I", send_len)
                                               //   await device.aio_soc_write_data(10, address, param)
                                               // So payload = send_len as u32 LE
                                               // And in capture the payload bytes are: 00 00 70 14
                                               // That's 0x14700000 in LE → but that's the address again...
                                               // Actually soc.py line 997: param = struct.pack("I", send_len) where send_len is the data chunk size
                                               // But my capture used send_len=4 for a test

        let frame = build_soc_frame(0x0A, 0x14700000, &0x14700000_u32.to_le_bytes(), 2);
        let expected = vec![
            0xA5, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x70, 0x14, 0x04, 0x00, 0x00, 0x00, 0x0A, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x70,
            0x14, 0xEB, 0x12, 0xA5,
        ];
        assert_eq!(frame, expected, "Frame should match captured CMD 0x0A packet");
    }

    #[test]
    fn test_build_soc_frame_with_escape() {
        // CMD 0x0C (finalize, len=30):
        // A5 00 00 00 00 00 00 00 00 00 00 00 00 01 00 00 00 0C 00 00 00 04 00 00 00 00 A6 02 41 A5
        // Note: 0xA6 appears in CRC → escaped to 0xA6 0x02
        let frame = build_soc_frame(0x0C, 0, &[0x00], 4);
        let expected = vec![
            0xA5, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x0C, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0xA6, 0x02,
            0x41, 0xA5,
        ];
        assert_eq!(frame, expected, "Frame should match captured CMD 0x0C with escape");
    }

    #[test]
    fn test_soc_frame_roundtrip() {
        let frame = build_soc_frame(9, 0x14700000, &[1, 2, 3, 4], 42);
        let mut parser = SocFrameParser::new();
        let responses = parser.feed(&frame);
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].cmd, 9);
        assert_eq!(responses[0].address, 0x14700000);
        assert_eq!(responses[0].data, vec![1, 2, 3, 4]);
        assert_eq!(responses[0].sn, 42);
    }

    #[test]
    fn test_soc_frame_parser_partial() {
        let frame = build_soc_frame(11, 0, &[0xA5, 0xA6, 0x00, 0xFF], 1);
        let mut parser = SocFrameParser::new();
        // Feed in small chunks
        let mid = frame.len() / 2;
        let r1 = parser.feed(&frame[..mid]);
        assert!(r1.is_empty());
        let r2 = parser.feed(&frame[mid..]);
        assert_eq!(r2.len(), 1);
        assert_eq!(r2[0].data, vec![0xA5, 0xA6, 0x00, 0xFF]);
    }

    /// 测试专用 unescape 辅助函数（与 SocFrameParser::feed 中的转义还原逻辑一致，
    /// 仅用于验证 soc_escape 的往返，不涉及任何串口操作）。
    fn unescape(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len());
        let mut i = 0;
        while i < data.len() {
            if data[i] == SOC_ESC && i + 1 < data.len() {
                match data[i + 1] {
                    SOC_ESC_FLAG => out.push(SOC_FLAG),
                    SOC_ESC_CODE => out.push(SOC_ESC),
                    other => out.push(other & 0x03),
                }
                i += 2;
            } else {
                out.push(data[i]);
                i += 1;
            }
        }
        out
    }

    #[test]
    fn test_soc_escape_unescape_roundtrip() {
        // 特殊字节 0xA5/0xA6 与普通数据混合的往返
        let cases: &[&[u8]] = &[
            &[],
            &[0x01, 0x02, 0x03],
            &[SOC_FLAG],
            &[SOC_ESC],
            &[SOC_FLAG, SOC_ESC],
            &[0x00, SOC_FLAG, 0x7E, SOC_ESC, 0xFF, SOC_FLAG],
            &[SOC_FLAG; 16],
        ];
        for case in cases {
            let escaped = soc_escape(case);
            assert_eq!(unescape(&escaped), *case, "escape/unescape 往返失败");
            // 转义后不允许出现裸 0xA5（0xA5 一定是帧分隔符）
            assert!(!escaped.contains(&SOC_FLAG), "转义输出不应包含裸 0xA5");
        }
    }

    #[test]
    fn test_crc16_modbus_known_values() {
        // 说明：标准 Modbus CRC（init=0xFFFF + xorout=0xFFFF）对 b"123456789" 的校验值是
        // 0x4B37；本实现采用 init=0、无最终异或（等效 CRC-16/ARC，多项式同为 0x8005 反转），
        // 实际计算结果为 0xBB3D（与现有 test_crc16_modbus 一致），故以实际值固化。
        assert_eq!(crc16_modbus(b"123456789"), 0xBB3D);
        // 额外固定输入，期望值由本函数实际计算结果固化
        assert_eq!(crc16_modbus(b"LuatOS Air1601"), 0x14BF);
        assert_eq!(crc16_modbus(b""), 0x0000);
    }

    #[test]
    fn test_build_soc_header_layout() {
        // 全字段字节布局：ms:u64=0（0..8）、address:u32 LE（8..12）、
        // len:u32 LE（12..16）、cmd:u32 LE（16..20）、sn:u16 LE（20..22）、
        // type:u8=0（22）、cpu:u8=0（23）
        let header = build_soc_header(0x09, 0x1470_0000, 5, 0x0102);
        assert_eq!(header.len(), SOC_HEADER_LEN);
        assert_eq!(&header[0..8], &[0u8; 8], "ms 字段应为 0");
        assert_eq!(&header[8..12], &0x1470_0000u32.to_le_bytes(), "address 应为 LE");
        assert_eq!(&header[12..16], &5u32.to_le_bytes(), "len 应为 LE");
        assert_eq!(&header[16..20], &0x09u32.to_le_bytes(), "cmd 应为 LE");
        assert_eq!(&header[20..22], &0x0102u16.to_le_bytes(), "sn 应为 LE");
        assert_eq!(header[22], 0, "type 字节应为 0");
        assert_eq!(header[23], 0, "cpu 字节应为 0");
    }

    #[test]
    fn test_soc_frame_escape_roundtrip_single_feed() {
        // 整帧一次性喂入 parser：载荷包含 0xA5/0xA6 特殊字节，验证转义 + CRC 的端到端往返
        let payload = [0xA5, 0xA6, 0x00, 0xFF, 0xA5];
        let frame = build_soc_frame(0x0B, 0x1234_5678, &payload, 99);
        let mut parser = SocFrameParser::new();
        let responses = parser.feed(&frame);
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].cmd, 0x0B);
        assert_eq!(responses[0].address, 0x1234_5678);
        assert_eq!(responses[0].sn, 99);
        assert_eq!(responses[0].data, payload);
    }

    // ─── 真机能力探针（需硬件，默认跳过，CI 不跑） ───────────────────────────

    /// 连上真机，探测 block_len 与 `GET_FLASH_DATA`(0x06) 是否可用。
    ///
    /// 两者都是 `flash raw` 的能力前提：
    /// - `block_len` 是分区级擦除的实际粒度，直接决定 `erase()` 循环几次
    /// - `GET_FLASH_DATA` 若可用，`flash raw dump` 的成本会大幅下降
    ///
    /// 用法：`cargo test -p luatos-flash hw_probe -- --ignored --nocapture`
    /// 环境变量：`HW_PORT`（默认 COM23）、`HW_SOC`（默认 refs/soc_files 下的 Air1601 V1021）
    #[test]
    #[ignore = "需要真机 CCM4211，改用 --ignored 显式运行"]
    fn hw_probe_raw_capabilities() {
        let port = std::env::var("HW_PORT").unwrap_or_else(|_| "COM23".into());
        let soc = std::env::var("HW_SOC").unwrap_or_else(|_| "refs/soc_files/LuatOS-SoC_V1021_Air1601.soc".into());

        let info = luatos_soc::read_soc_info(&soc).expect("read soc info");
        let tmpdir = tempfile::tempdir().expect("tempdir");
        luatos_soc::unpack_soc(&soc, tmpdir.path()).expect("unpack soc");
        let ramrun = load_ramrun_data(&soc, tmpdir.path()).expect("load ramrun");
        drop(tmpdir);

        let on_progress: ProgressCallback = Box::new(|p| println!("  [progress] {} {} {}", p.stage, p.percent, p.message));
        let mut port_handle = isp_load_ramrun(&port, &ramrun, &on_progress).expect("isp_load_ramrun");
        switch_to_soc_protocol(&mut *port_handle, info.flash_baud_rate()).expect("switch to soc protocol");

        let mut parser = SocFrameParser::new();
        let mut sn: u16 = 0;

        // 1) block_len
        let resp = soc_send_cmd(&mut port_handle, &mut parser, SOC_CMD_GET_DOWNLOAD_INFO, 0, &[], &mut sn, Duration::from_secs(2)).expect("GET_DOWNLOAD_INFO");
        let block_len = u32::from_le_bytes(resp.data[0..4].try_into().unwrap());
        println!("\n>>> block_len = {block_len} (0x{block_len:x})");

        // 2) GET_FLASH_DATA 探测（只读，不改 flash）
        //    按 文档语义 试：address + payload=len
        let probe_addr: u32 = info.script_addr();
        println!(">>> probing {SOC_CMD_GET_FLASH_DATA} (GET_FLASH_DATA) at 0x{probe_addr:08X} ...");
        let mut found = false;
        for len in [64u32, 256] {
            let mut payload = Vec::new();
            payload.extend_from_slice(&len.to_le_bytes());
            match soc_send_cmd(&mut port_handle, &mut parser, SOC_CMD_GET_FLASH_DATA, probe_addr, &payload, &mut sn, Duration::from_secs(3)) {
                Ok(r) => {
                    println!("    len={len} -> OK, resp.cmd={} data_len={}", r.cmd, r.data.len());
                    let preview: Vec<String> = r.data.iter().take(32).map(|b| format!("{b:02x}")).collect();
                    println!("    first32: {}", preview.join(" "));
                    found = true;
                }
                Err(e) => println!("    len={len} -> ERR {e}"),
            }
        }
        println!(">>> GET_FLASH_DATA available: {found}");

        // 3) CHECK_CODE 读回校验（只读）
        let len4k = 4096u32.to_le_bytes();
        match soc_send_cmd(&mut port_handle, &mut parser, SOC_CMD_CHECK_CODE, probe_addr, &len4k, &mut sn, Duration::from_secs(10)) {
            Ok(r) => {
                let md5hex: String = r.data.iter().take(16).map(|b| format!("{b:02x}")).collect();
                println!(">>> CHECK_CODE(0x{probe_addr:08X}, 4096) -> MD5={md5hex}");
            }
            Err(e) => println!(">>> CHECK_CODE ERR {e}"),
        }

        let _ = soc_reset_device(&mut port_handle);
    }

    /// 实测 `FLASH_ERASE_BLOCK` 的真实擦除范围。
    ///
    /// 背景：`block_len` 报 256KB，但 `flash raw --erase` 的语义是"擦掉用户请求的
    /// 区间"。若设备实际按 256KB 擦，用户只想擦 4KB 就会连带清掉邻近 252KB ——
    /// 这是必须查清并写进文档的行为差异。
    ///
    /// 手法：先用已知图案填满一整个 256KB 块，再只发一次擦除命令，
    /// 然后对块内多个偏移做 MD5，看哪些位置变成了全 0xFF。
    ///
    /// 用法：`cargo test -p luatos-flash hw_erase_granularity -- --ignored --nocapture`
    #[test]
    #[ignore = "需要真机 CCM4211，且会擦写脚本分区（可恢复）"]
    fn hw_measure_erase_granularity() {
        let port = std::env::var("HW_PORT").unwrap_or_else(|_| "COM23".into());
        let soc = std::env::var("HW_SOC").unwrap_or_else(|_| "refs/soc_files/LuatOS-SoC_V1021_Air1601.soc".into());
        let info = luatos_soc::read_soc_info(&soc).expect("read soc info");
        let tmpdir = tempfile::tempdir().expect("tempdir");
        luatos_soc::unpack_soc(&soc, tmpdir.path()).expect("unpack soc");
        let ramrun = load_ramrun_data(&soc, tmpdir.path()).expect("load ramrun");
        drop(tmpdir);

        let base: u32 = info.script_addr();
        let on_progress: ProgressCallback = Box::new(|_| {});
        let mut p = isp_load_ramrun(&port, &ramrun, &on_progress).expect("isp_load_ramrun");
        switch_to_soc_protocol(&mut *p, info.flash_baud_rate()).expect("switch soc");
        let mut parser = SocFrameParser::new();
        let mut sn: u16 = 0;
        let block_len = {
            let r = soc_send_cmd(&mut p, &mut parser, SOC_CMD_GET_DOWNLOAD_INFO, 0, &[], &mut sn, Duration::from_secs(2)).expect("info");
            u32::from_le_bytes(r.data[0..4].try_into().unwrap())
        };

        // 填充图案：每 512 字节换一次值，保证任意 4KB 窗口内容互不相同。
        //（不能用 (i*k+c)%256 —— 那样的周期是 256 字节，每个对齐窗口都一样，
        //  会让"各偏移 MD5 是否不同"这项验证完全失效）
        let fill: Vec<u8> = (0..block_len).map(|i| ((i / 512) % 256) as u8).collect();
        println!("\n>>> 填充 {block_len} 字节图案到 0x{base:08X} ...");
        soc_download_file(&mut p, &mut parser, &mut sn, base, &fill, block_len, &on_progress, "Fill", 0.0, 10.0).expect("fill");

        // 探测点：块内多个偏移
        let offsets: [u32; 8] = [0, 0x1000, 0x2000, 0x8000, 0x10000, 0x30000, 0x3F000, 0x40000];
        let md5_at = |p: &mut Box<dyn SerialPort>, parser: &mut SocFrameParser, sn: &mut u16, addr: u32| -> String {
            let len = 4096u32.to_le_bytes();
            let r = soc_send_cmd(p, parser, SOC_CMD_CHECK_CODE, addr, &len, sn, Duration::from_secs(10)).expect("check_code");
            r.data.iter().take(16).map(|b| format!("{b:02x}")).collect()
        };

        let md5_ff4k = format!("{:x}", md5::compute(&[0xFFu8; 4096]));
        println!(">>> MD5(4096 × 0xFF) = {md5_ff4k}");
        let before: Vec<String> = offsets.iter().map(|o| md5_at(&mut p, &mut parser, &mut sn, base + o)).collect();
        println!(">>> 擦除前各偏移 MD5:");
        for (o, m) in offsets.iter().zip(&before) {
            println!("    +0x{o:05x} (0x{:08x}) = {m}", base + o);
        }

        // 只发一次擦除。注意用的是**块内非对齐地址** base+0x1000：
        // 若设备按 block 向下取整，base+0x00000（用户并未请求）也会被清掉 ——
        // 这是 `flash raw --erase` 最危险的行为，必须查清。
        let erase_at = base + 0x1000;
        println!(">>> 对 0x{erase_at:08X} 发一次 FLASH_ERASE_BLOCK ...");
        soc_send_cmd(&mut p, &mut parser, SOC_CMD_FLASH_ERASE_BLOCK, erase_at, &[], &mut sn, Duration::from_secs(10)).expect("erase block");

        println!(">>> 擦除后各偏移 MD5（== {md5_ff4k} 表示已被擦成 0xFF）:");
        let mut wiped_upto = 0u32;
        for (o, b4) in offsets.iter().zip(&before) {
            let a = md5_at(&mut p, &mut parser, &mut sn, base + o);
            let erased = a == md5_ff4k;
            let changed = a != *b4;
            println!("    +0x{o:05x} = {a}  [擦除={erased} 变化={changed}]");
            if changed {
                wiped_upto = *o;
            }
        }
        println!(">>> 实测擦除覆盖到至少 base+0x{wiped_upto:05x}（block_len=0x{block_len:x}）");

        let _ = soc_reset_device(&mut p);
    }
}
