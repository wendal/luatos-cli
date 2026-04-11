// CCM4211 (Air1601) flash protocol.
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
const ISP_CMD_SET_RAM_BASE: u8 = 0x20;
const ISP_CMD_WRITE_RAM: u8 = 0x31;
const ISP_CMD_EXECUTE: u8 = 0x81;

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
        Some(SocResponse {
            cmd,
            address,
            sn,
            data: body,
        })
    }
}

// ─── ISP Protocol ───────────────────────────────────────────────────────────

/// Send an ISP command and wait for ACK.
/// Frame: 0x52 + cmd + param1 + param2 + data_len(u16 BE) + data
/// ACK:   0x72 + cmd + ack_data_len(u16 BE) + ack_data
fn isp_send_cmd(
    port: &mut Box<dyn SerialPort>,
    cmd: u8,
    param1: u8,
    param2: u8,
    data_len: u16,
    data: Option<&[u8]>,
    timeout: Duration,
) -> Result<Vec<u8>> {
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
                            let ack_len =
                                u16::from_be_bytes([read_buf[2], read_buf[3]]) as usize + 4;
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
        match isp_send_cmd(
            port,
            ISP_CMD_SYNC,
            0,
            0,
            0x24,
            None,
            Duration::from_millis(500),
        ) {
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

/// Load ramrun binary into device RAM via ISP.
fn isp_load_ramrun(
    port_name: &str,
    ramrun_data: &[u8],
    on_progress: &ProgressCallback,
) -> Result<()> {
    on_progress(&FlashProgress::info(
        "Connect",
        0.0,
        "Opening serial port (ISP)",
    ));

    let mut port: Box<dyn SerialPort> = serialport::new(port_name, ISP_INITIAL_BAUD)
        .parity(Parity::Even)
        .timeout(Duration::from_millis(100))
        .open()
        .with_context(|| format!("Cannot open {port_name}"))?;

    on_progress(&FlashProgress::info(
        "Reset",
        2.0,
        "Handshaking with ISP bootrom",
    ));

    let _sync_data =
        isp_handshake(&mut port, Duration::from_secs(15)).context("ISP handshake failed")?;

    on_progress(&FlashProgress::info("Baud", 5.0, "Switching to 1Mbps"));

    // Switch baud rate to 1Mbps: cmd 0x10, data = [0x00, 0x01, 0x24]
    isp_send_cmd(
        &mut port,
        ISP_CMD_SET_BAUD,
        0,
        0,
        3,
        Some(&[0x00, 0x01, 0x24]),
        Duration::from_millis(50),
    )
    .context("ISP baud rate change failed")?;

    // Reopen at 1Mbps
    drop(port);
    let mut port: Box<dyn SerialPort> = serialport::new(port_name, ISP_FAST_BAUD)
        .parity(Parity::Even)
        .timeout(Duration::from_millis(100))
        .open()
        .with_context(|| format!("Cannot reopen {port_name} at 1Mbps"))?;

    // Verify sync at new baud rate
    isp_send_cmd(
        &mut port,
        ISP_CMD_SYNC,
        0,
        0,
        0x10,
        None,
        Duration::from_millis(50),
    )
    .context("ISP sync at 1Mbps failed")?;

    on_progress(&FlashProgress::info("Baud", 7.0, "ISP baud rate 1Mbps OK"));

    // Set RAM base address: cmd 0x20, param1=0x20, param2=0x00
    isp_send_cmd(
        &mut port,
        ISP_CMD_SET_RAM_BASE,
        0x20,
        0x00,
        4,
        None,
        Duration::from_millis(50),
    )
    .context("ISP set RAM base address failed")?;

    on_progress(&FlashProgress::info(
        "Write",
        8.0,
        "Loading ramrun into RAM",
    ));

    // Write ramrun in 512-byte chunks, address auto-increments by 2
    let mut base_address: u8 = 0x30;
    let total_len = ramrun_data.len();
    let mut done = 0;

    while done < total_len {
        let chunk_len = std::cmp::min(512, total_len - done);
        let chunk = &ramrun_data[done..done + chunk_len];

        isp_send_cmd(
            &mut port,
            ISP_CMD_WRITE_RAM,
            base_address,
            0x00,
            chunk_len as u16,
            Some(chunk),
            Duration::from_millis(500),
        )
        .with_context(|| format!("ISP RAM write failed at offset {done}"))?;

        done += chunk_len;
        base_address = base_address.wrapping_add(2);

        let pct = 8.0 + (done as f32 / total_len as f32) * 7.0; // 8% - 15%
        on_progress(&FlashProgress::info(
            "Write",
            pct,
            &format!("Ramrun {done}/{total_len}"),
        ));
    }

    on_progress(&FlashProgress::info(
        "Write",
        15.0,
        "Ramrun loaded, executing",
    ));

    // Execute ramrun: cmd 0x81
    // Jump address from ramrun binary bytes 4-5
    let go_addr1 = ramrun_data[5];
    let go_addr2 = ramrun_data[4];

    // Execute may not ACK (ramrun starts and takes over UART)
    let _ = isp_send_cmd(
        &mut port,
        ISP_CMD_EXECUTE,
        go_addr1,
        go_addr2,
        0,
        None,
        Duration::from_millis(100),
    );

    drop(port);
    Ok(())
}

// ─── SOC Protocol Operations ────────────────────────────────────────────────

/// Send a SOC command and wait for matching response.
fn soc_send_cmd(
    port: &mut Box<dyn SerialPort>,
    parser: &mut SocFrameParser,
    cmd: u32,
    address: u32,
    payload: &[u8],
    sn: &mut u16,
    timeout: Duration,
) -> Result<SocResponse> {
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
                    log::debug!(
                        "SOC response: cmd={}, addr=0x{:08X}, sn={}, data_len={}",
                        resp.cmd,
                        resp.address,
                        resp.sn,
                        resp.data.len()
                    );
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
    let total_len = data.len();
    let mut done = 0usize;
    let mut cur_addr = address;

    while done < total_len {
        let send_len = std::cmp::min(block_len as usize, total_len - done);
        let chunk = &data[done..done + send_len];

        // CMD 0x0A: set download address + original length
        let orig_len_bytes = (send_len as u32).to_le_bytes();
        soc_send_cmd(
            port,
            parser,
            SOC_CMD_SET_CODE_DATA_START,
            cur_addr,
            &orig_len_bytes,
            sn,
            Duration::from_secs(1),
        )
        .context("SOC set code data start failed")?;

        // CMD 0x0B: send data in sub-chunks (no compression)
        let sect_len = 3 * 1024; // 3KB chunks like Python reference
        let mut s_len = 0;
        while s_len < send_len {
            let ss_len = std::cmp::min(sect_len, send_len - s_len);
            let sub_chunk = &chunk[s_len..s_len + ss_len];

            // Retry up to 10 times (CMD 11 can fail with synchronous I/O)
            let mut last_err = None;
            for retry in 0..10 {
                match soc_send_cmd(
                    port,
                    parser,
                    SOC_CMD_SET_CODE_DATA,
                    0,
                    sub_chunk,
                    sn,
                    Duration::from_millis(500),
                ) {
                    Ok(_) => {
                        last_err = None;
                        break;
                    }
                    Err(e) => {
                        log::debug!(
                            "CMD 11 retry {}/{} at {} offset {}",
                            retry + 1,
                            10,
                            stage_name,
                            done + s_len
                        );
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

        // CMD 0x0C: finalize block (is_lzma = 0, no compression)
        soc_send_cmd(
            port,
            parser,
            SOC_CMD_SET_CODE_END,
            0,
            &[0x00],
            sn,
            Duration::from_secs(3),
        )
        .context("SOC set code end failed")?;

        done += send_len;
        cur_addr += send_len as u32;

        let pct = pct_start + (done as f32 / total_len as f32) * (pct_end - pct_start);
        on_progress(&FlashProgress::info(
            stage_name,
            pct,
            &format!("{done}/{total_len} bytes"),
        ));
    }

    // CMD 0x0D: MD5 verify (10s timeout for large files)
    let total_len_bytes = (data.len() as u32).to_le_bytes();
    let resp = soc_send_cmd(
        port,
        parser,
        SOC_CMD_CHECK_CODE,
        address,
        &total_len_bytes,
        sn,
        Duration::from_secs(10),
    )
    .context("SOC MD5 check failed")?;

    // Verify MD5
    let expected_md5 = md5::compute(data);
    let device_md5_hex = resp
        .data
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
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
/// Downloads bootloader + core + script (if present) via ISP+SOC protocol.
pub fn flash_ccm4211(
    soc_path: &str,
    port_name: &str,
    on_progress: &ProgressCallback,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    // Extract .soc
    on_progress(&FlashProgress::info("Extract", 0.0, "Unpacking .soc file"));
    let tmpdir = tempfile::tempdir().context("Create temp dir")?;
    let unpacked = luatos_soc::unpack_soc(soc_path, tmpdir.path())?;
    let info = &unpacked.info;
    let soc_dir = tmpdir.path();

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

    // Stage 1: ISP — load ramrun
    isp_load_ramrun(port_name, &ramrun_data, on_progress)?;

    if cancel.load(Ordering::Relaxed) {
        bail!("Cancelled");
    }

    // Stage 2: SOC protocol — open at download baud rate
    let dl_baud = info.flash_baud_rate();
    on_progress(&FlashProgress::info(
        "Connect",
        16.0,
        &format!("Connecting SOC protocol at {dl_baud}"),
    ));

    std::thread::sleep(Duration::from_millis(10));
    let mut port: Box<dyn SerialPort> = serialport::new(port_name, dl_baud)
        .parity(Parity::None)
        .timeout(Duration::from_millis(50))
        .open()
        .with_context(|| format!("Cannot open {port_name} at {dl_baud}"))?;

    let mut parser = SocFrameParser::new();
    let mut sn: u16 = 0;

    // Query download block size
    let mut block_len: u32 = 0;
    for _ in 0..3 {
        match soc_send_cmd(
            &mut port,
            &mut parser,
            SOC_CMD_GET_DOWNLOAD_INFO,
            0,
            &[],
            &mut sn,
            Duration::from_millis(100),
        ) {
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
    on_progress(&FlashProgress::info(
        "Connect",
        18.0,
        &format!("Block size: {block_len} bytes"),
    ));

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
        on_progress(&FlashProgress::info(
            "Bootloader",
            20.0,
            "Downloading bootloader",
        ));
        soc_download_file(
            &mut port,
            &mut parser,
            &mut sn,
            bl_addr,
            &bl_data,
            block_len,
            on_progress,
            "Bootloader",
            20.0,
            40.0,
        )?;
        on_progress(&FlashProgress::info("Bootloader", 40.0, "Bootloader OK"));
    }

    if cancel.load(Ordering::Relaxed) {
        bail!("Cancelled");
    }

    // Download core firmware
    if core_path.exists() {
        let core_data = std::fs::read(&core_path)
            .with_context(|| format!("Failed to read {}", info.rom.file))?;
        on_progress(&FlashProgress::info(
            "Core",
            40.0,
            "Downloading core firmware",
        ));
        soc_download_file(
            &mut port,
            &mut parser,
            &mut sn,
            core_addr,
            &core_data,
            block_len,
            on_progress,
            "Core",
            40.0,
            80.0,
        )?;
        on_progress(&FlashProgress::info("Core", 80.0, "Core firmware OK"));
    }

    if cancel.load(Ordering::Relaxed) {
        bail!("Cancelled");
    }

    // Download script (if exists)
    if script_path.exists() {
        let script_data = std::fs::read(&script_path).context("Failed to read script")?;
        on_progress(&FlashProgress::info("Script", 80.0, "Downloading script"));
        soc_download_file(
            &mut port,
            &mut parser,
            &mut sn,
            script_addr,
            &script_data,
            block_len,
            on_progress,
            "Script",
            80.0,
            95.0,
        )?;
        on_progress(&FlashProgress::info("Script", 95.0, "Script OK"));
    }

    // Reset device
    on_progress(&FlashProgress::info("Reset", 98.0, "Resetting device"));
    soc_reset_device(&mut port)?;
    drop(port);

    on_progress(&FlashProgress::done_ok("Flash completed successfully"));
    Ok(())
}

/// Flash only the script partition for CCM4211/Air1601.
pub fn flash_script_ccm4211(
    soc_path: &str,
    port_name: &str,
    script_data: &[u8],
    on_progress: &ProgressCallback,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    on_progress(&FlashProgress::info("Extract", 0.0, "Unpacking .soc file"));
    let tmpdir = tempfile::tempdir().context("Create temp dir")?;
    let unpacked = luatos_soc::unpack_soc(soc_path, tmpdir.path())?;
    let info = &unpacked.info;
    let soc_dir = tmpdir.path();

    let ramrun_path = soc_dir.join("ramrun.bin");
    let ramrun_data = if ramrun_path.exists() {
        std::fs::read(&ramrun_path)?
    } else {
        let soc_parent = std::path::Path::new(soc_path).parent();
        let default_path = soc_parent.map(|p| p.join("ccm4211_ramrun_default.bin"));
        if let Some(ref dp) = default_path {
            if dp.exists() {
                std::fs::read(dp)?
            } else {
                bail!("No ramrun.bin found");
            }
        } else {
            bail!("No ramrun.bin found");
        }
    };

    if cancel.load(Ordering::Relaxed) {
        bail!("Cancelled");
    }

    isp_load_ramrun(port_name, &ramrun_data, on_progress)?;

    if cancel.load(Ordering::Relaxed) {
        bail!("Cancelled");
    }

    let dl_baud = info.flash_baud_rate();
    std::thread::sleep(Duration::from_millis(10));
    let mut port: Box<dyn SerialPort> = serialport::new(port_name, dl_baud)
        .parity(Parity::None)
        .timeout(Duration::from_millis(50))
        .open()?;

    let mut parser = SocFrameParser::new();
    let mut sn: u16 = 0;

    // Get block size
    let mut block_len: u32 = 0;
    for _ in 0..3 {
        if let Ok(resp) = soc_send_cmd(
            &mut port,
            &mut parser,
            SOC_CMD_GET_DOWNLOAD_INFO,
            0,
            &[],
            &mut sn,
            Duration::from_millis(100),
        ) {
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
    on_progress(&FlashProgress::info("Script", 20.0, "Downloading script"));
    soc_download_file(
        &mut port,
        &mut parser,
        &mut sn,
        script_addr,
        script_data,
        block_len,
        on_progress,
        "Script",
        20.0,
        90.0,
    )?;

    on_progress(&FlashProgress::info("Reset", 95.0, "Resetting device"));
    soc_reset_device(&mut port)?;
    drop(port);

    on_progress(&FlashProgress::done_ok("Script flash completed"));
    Ok(())
}

/// Erase a flash partition (FSKV or filesystem).
pub fn erase_partition_ccm4211(
    soc_path: &str,
    port_name: &str,
    partition_addr: u32,
    partition_name: &str,
    on_progress: &ProgressCallback,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    on_progress(&FlashProgress::info("Extract", 0.0, "Unpacking .soc file"));
    let tmpdir = tempfile::tempdir().context("Create temp dir")?;
    let unpacked = luatos_soc::unpack_soc(soc_path, tmpdir.path())?;
    let info = &unpacked.info;
    let soc_dir = tmpdir.path();

    let ramrun_path = soc_dir.join("ramrun.bin");
    let ramrun_data = if ramrun_path.exists() {
        std::fs::read(&ramrun_path)?
    } else {
        let soc_parent = std::path::Path::new(soc_path).parent();
        let default_path = soc_parent.map(|p| p.join("ccm4211_ramrun_default.bin"));
        if let Some(ref dp) = default_path {
            if dp.exists() {
                std::fs::read(dp)?
            } else {
                bail!("No ramrun.bin found");
            }
        } else {
            bail!("No ramrun.bin found");
        }
    };

    if cancel.load(Ordering::Relaxed) {
        bail!("Cancelled");
    }

    isp_load_ramrun(port_name, &ramrun_data, on_progress)?;

    if cancel.load(Ordering::Relaxed) {
        bail!("Cancelled");
    }

    let dl_baud = info.flash_baud_rate();
    std::thread::sleep(Duration::from_millis(10));
    let mut port: Box<dyn SerialPort> = serialport::new(port_name, dl_baud)
        .parity(Parity::None)
        .timeout(Duration::from_millis(50))
        .open()?;

    let mut parser = SocFrameParser::new();
    let mut sn: u16 = 0;

    // Verify ramrun is running
    for _ in 0..3 {
        if soc_send_cmd(
            &mut port,
            &mut parser,
            SOC_CMD_GET_DOWNLOAD_INFO,
            0,
            &[],
            &mut sn,
            Duration::from_millis(100),
        )
        .is_ok()
        {
            break;
        }
    }

    on_progress(&FlashProgress::info(
        "Erase",
        50.0,
        &format!("Erasing {partition_name} at 0x{partition_addr:08X}"),
    ));

    for _ in 0..3 {
        if soc_send_cmd(
            &mut port,
            &mut parser,
            SOC_CMD_FLASH_ERASE_BLOCK,
            partition_addr,
            &[],
            &mut sn,
            Duration::from_secs(10),
        )
        .is_ok()
        {
            on_progress(&FlashProgress::info("Reset", 90.0, "Resetting device"));
            soc_reset_device(&mut port)?;
            drop(port);
            on_progress(&FlashProgress::done_ok(&format!(
                "{partition_name} erased successfully"
            )));
            return Ok(());
        }
    }
    bail!("Failed to erase {partition_name}")
}

/// Clear the filesystem partition for CCM4211/Air1601.
pub fn clear_filesystem(
    soc_path: &str,
    port_name: &str,
    on_progress: &ProgressCallback,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    let info = luatos_soc::read_soc_info(soc_path)?;
    let addr = info
        .fs_addr()
        .context("Air1601 SOC has no fs_addr defined")?;
    erase_partition_ccm4211(soc_path, port_name, addr, "filesystem", on_progress, cancel)
}

/// Clear the FSKV (key-value) partition for CCM4211/Air1601.
pub fn clear_fskv(
    soc_path: &str,
    port_name: &str,
    on_progress: &ProgressCallback,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    let info = luatos_soc::read_soc_info(soc_path)?;
    let addr = info
        .nvm_addr()
        .context("Air1601 SOC has no nvm_addr defined")?;
    erase_partition_ccm4211(soc_path, port_name, addr, "fskv", on_progress, cancel)
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(
            soc_escape(&[0x00, 0xA5, 0xA6, 0xFF]),
            vec![0x00, 0xA6, 0x01, 0xA6, 0x02, 0xFF]
        );
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
            0xA5, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0xC1, 0x96, 0xA5,
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
            0xA5, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x70, 0x14, 0x04,
            0x00, 0x00, 0x00, 0x0A, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x70,
            0x14, 0xEB, 0x12, 0xA5,
        ];
        assert_eq!(
            frame, expected,
            "Frame should match captured CMD 0x0A packet"
        );
    }

    #[test]
    fn test_build_soc_frame_with_escape() {
        // CMD 0x0C (finalize, len=30):
        // A5 00 00 00 00 00 00 00 00 00 00 00 00 01 00 00 00 0C 00 00 00 04 00 00 00 00 A6 02 41 A5
        // Note: 0xA6 appears in CRC → escaped to 0xA6 0x02
        let frame = build_soc_frame(0x0C, 0, &[0x00], 4);
        let expected = vec![
            0xA5, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
            0x00, 0x00, 0x00, 0x0C, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0xA6, 0x02,
            0x41, 0xA5,
        ];
        assert_eq!(
            frame, expected,
            "Frame should match captured CMD 0x0C with escape"
        );
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
}
