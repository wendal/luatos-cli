// BK7258 (Air8101) native serial flasher.
//
// Protocol reference: https://github.com/openshwprojects/BK7231GUIFlashTool
//
// Flash sequence:
//   1. Extract .soc (zip) to temp dir; parse info.json.
//   2. Open COM port at 115200 baud.
//   3. Toggle DTR+RTS to reset device into ROM bootloader.
//   4. LinkCheck handshake until device responds.
//   5. Switch to 2 Mbps (or force_br from info.json).
//   6. Read Flash MID; unprotect (clear BP/CMP bits in SR).
//   7. Erase sectors (4K or 64K blocks), write 4K sectors.
//   8. Optionally flash script partition.
//   9. Close port → device auto-reboots.

use anyhow::{bail, Context, Result};
use luatos_soc::{parse_addr, SocInfo};
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{FlashProgress, ProgressCallback};

const SECTOR_SIZE: usize = 0x1000; // 4 KiB
const SECTORS_PER_BLOCK: usize = 16; // 64 KiB / 4 KiB

// ─── Low-level serial helpers ─────────────────────────────────────────────────

/// Read exactly `buf.len()` bytes within `timeout`.
fn read_exact_timeout(port: &mut dyn serialport::SerialPort, buf: &mut [u8], timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let mut n = 0;
    while n < buf.len() {
        if Instant::now() > deadline {
            bail!("Serial read timeout: got {}/{} bytes", n, buf.len());
        }
        match port.read(&mut buf[n..]) {
            Ok(k) if k > 0 => n += k,
            Ok(_) | Err(_) => std::thread::sleep(Duration::from_millis(1)),
        }
    }
    Ok(())
}

// ─── BK7231 protocol commands ─────────────────────────────────────────────────
//
// Short command (≤255 body bytes):
//   TX: [0x01, 0xE0, 0xFC, LEN, CMD, DATA...]
//   RX: [0x04, 0x0E, LEN, 0x01, 0xE0, 0xFC, CMD, DATA...]
//
// Long command (body > 255 bytes or flash operations):
//   TX: [0x01, 0xE0, 0xFC, 0xFF, 0xF4, LEN_LO, LEN_HI, CMD, DATA...]
//   RX: [0x04, 0x0E, 0xFF, 0x01, 0xE0, 0xFC, 0xF4, LEN_LO, LEN_HI, CMD, ...]

/// Send LinkCheck and verify response. Uses a short 2ms window.
fn link_check_once(port: &mut dyn serialport::SerialPort) -> bool {
    let _ = port.clear(serialport::ClearBuffer::Input);
    let tx = [0x01u8, 0xE0, 0xFC, 0x01, 0x00];
    if port.write_all(&tx).is_err() || port.flush().is_err() {
        return false;
    }
    let mut buf = [0u8; 8];
    let deadline = Instant::now() + Duration::from_millis(2);
    let mut n = 0;
    while n < 8 && Instant::now() < deadline {
        match port.read(&mut buf[n..]) {
            Ok(k) if k > 0 => n += k,
            _ => {}
        }
    }
    n >= 8 && buf == [0x04, 0x0E, 0x05, 0x01, 0xE0, 0xFC, 0x01, 0x00]
}

/// Toggle DTR+RTS to force device into ROM bootloader, then wait for LinkCheck ACK.
///
/// Uses the 5-phase pulse pattern:
///   (DTR=1,RTS=1,50ms) → (0,0,20ms) → (1,0,50ms) → (0,1,50ms) → (0,0) → link checks
///
/// Port read timeout MUST be 1ms — the bootloader window is narrow (~8ms).
fn get_bus(port: &mut dyn serialport::SerialPort) -> Result<bool> {
    let _ = port.set_timeout(Duration::from_millis(1));

    // Initialize to known state
    let _ = port.write_data_terminal_ready(false);
    let _ = port.write_request_to_send(false);
    std::thread::sleep(Duration::from_millis(50));

    for _attempt in 0..30u32 {
        // 5-phase DTR/RTS pulse pattern
        let _ = port.write_data_terminal_ready(true);
        let _ = port.write_request_to_send(true);
        std::thread::sleep(Duration::from_millis(50));

        let _ = port.write_data_terminal_ready(false);
        let _ = port.write_request_to_send(false);
        std::thread::sleep(Duration::from_millis(20));

        let _ = port.write_data_terminal_ready(true);
        let _ = port.write_request_to_send(false);
        std::thread::sleep(Duration::from_millis(50));

        // Phase 4: DTR=0 (release reset), RTS=1 (bootloader mode)
        let _ = port.write_data_terminal_ready(false);
        let _ = port.write_request_to_send(true);
        std::thread::sleep(Duration::from_millis(50));

        let _ = port.write_data_terminal_ready(false);
        let _ = port.write_request_to_send(false);

        // Try LinkCheck rapidly — bootloader window opens within ~8ms
        for _ in 0..200 {
            if link_check_once(port) {
                let _ = port.set_timeout(Duration::from_millis(200));
                return Ok(true);
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    let _ = port.set_timeout(Duration::from_millis(200));
    Ok(false)
}

/// Switch baud rate. Sends command to device, waits, then switches host side.
fn set_baud_rate(port: &mut dyn serialport::SerialPort, baud: u32, delay_ms: u64) -> Result<bool> {
    let [b0, b1, b2, b3] = baud.to_le_bytes();
    let d = delay_ms as u8;
    let tx = [0x01u8, 0xE0, 0xFC, 0x06, 0x0F, b0, b1, b2, b3, d];

    // Drain stale ACKs
    let _ = port.clear(serialport::ClearBuffer::Input);
    std::thread::sleep(Duration::from_millis(50));
    let _ = port.clear(serialport::ClearBuffer::Input);

    port.write_all(&tx)?;
    port.flush()?;
    std::thread::sleep(Duration::from_millis(10));
    std::thread::sleep(Duration::from_millis(delay_ms / 2));
    port.set_baud_rate(baud).context("Failed to change host baud rate")?;

    let mut buf = [0u8; 8];
    if read_exact_timeout(port, &mut buf, Duration::from_millis(600)).is_err() {
        return Ok(false);
    }
    let ok = buf[..2] == [0x04, 0x0E] && buf[3..6] == [0x01, 0xE0, 0xFC] && buf[6] == 0x0F;
    Ok(ok)
}

/// Read flash JEDEC ID (Manufacturer, Memory Type, Capacity).
fn get_flash_mid(port: &mut dyn serialport::SerialPort) -> Result<u32> {
    let tx = [0x01u8, 0xE0, 0xFC, 0xFF, 0xF4, 0x05, 0x00, 0x0E, 0x9F, 0x00, 0x00, 0x00];
    let _ = port.clear(serialport::ClearBuffer::Input);
    port.write_all(&tx)?;
    port.flush()?;
    let mut buf = [0u8; 15];
    read_exact_timeout(port, &mut buf, Duration::from_secs(3))?;
    if buf[0] != 0x04 || buf[1] != 0x0E || buf[2] != 0xFF {
        bail!("GetFlashMID: bad response prefix {:02x?}", &buf[..4]);
    }
    let mid = buf[12] as u32 | ((buf[13] as u32) << 8) | ((buf[14] as u32) << 16);
    Ok(mid)
}

/// Read one flash Status Register byte.
fn read_flash_sr(port: &mut dyn serialport::SerialPort, sr_cmd: u8) -> Result<u8> {
    let tx = [0x01u8, 0xE0, 0xFC, 0xFF, 0xF4, 0x02, 0x00, 0x0C, sr_cmd];
    let _ = port.clear(serialport::ClearBuffer::Input);
    port.write_all(&tx)?;
    port.flush()?;
    let mut buf = [0u8; 13];
    read_exact_timeout(port, &mut buf, Duration::from_secs(2))?;
    if buf[0] != 0x04 || buf[1] != 0x0E || buf[2] != 0xFF {
        bail!("ReadFlashSR 0x{:02x}: bad response prefix", sr_cmd);
    }
    Ok(buf[11])
}

/// Write 1-byte flash Status Register.
fn write_flash_sr_1(port: &mut dyn serialport::SerialPort, wr_cmd: u8, val: u8) -> Result<()> {
    let tx = [0x01u8, 0xE0, 0xFC, 0xFF, 0xF4, 0x03, 0x00, 0x0D, wr_cmd, val];
    let _ = port.clear(serialport::ClearBuffer::Input);
    port.write_all(&tx)?;
    port.flush()?;
    let mut buf = [0u8; 13];
    read_exact_timeout(port, &mut buf, Duration::from_secs(2))?;
    if buf[0] != 0x04 || buf[1] != 0x0E || buf[2] != 0xFF {
        bail!("WriteFlashSR1: bad response prefix");
    }
    Ok(())
}

/// Write 2-byte flash Status Register (SR1+SR2 simultaneously).
fn write_flash_sr_2(port: &mut dyn serialport::SerialPort, wr_cmd: u8, val: u16) -> Result<()> {
    let [v0, v1] = val.to_le_bytes();
    let tx = [0x01u8, 0xE0, 0xFC, 0xFF, 0xF4, 0x04, 0x00, 0x0D, wr_cmd, v0, v1];
    let _ = port.clear(serialport::ClearBuffer::Input);
    port.write_all(&tx)?;
    port.flush()?;
    let mut buf = [0u8; 14];
    read_exact_timeout(port, &mut buf, Duration::from_secs(2))?;
    if buf[0] != 0x04 || buf[1] != 0x0E || buf[2] != 0xFF {
        bail!("WriteFlashSR2: bad response prefix");
    }
    Ok(())
}

/// Erase one sector/block. `sz_cmd`: 0x20 = 4K sector, 0xD8 = 64K block.
fn erase_sector(port: &mut dyn serialport::SerialPort, addr: u32, sz_cmd: u8) -> Result<bool> {
    let [a0, a1, a2, a3] = addr.to_le_bytes();
    let tx = [0x01u8, 0xE0, 0xFC, 0xFF, 0xF4, 0x06, 0x00, 0x0F, sz_cmd, a0, a1, a2, a3];
    let _ = port.clear(serialport::ClearBuffer::Input);
    port.write_all(&tx)?;
    port.flush()?;
    let timeout = if sz_cmd == 0xD8 { Duration::from_secs(15) } else { Duration::from_secs(5) };
    let mut buf = [0u8; 16];
    match read_exact_timeout(port, &mut buf, timeout) {
        Ok(_) => {}
        Err(e) => {
            log::warn!("Erase timeout at 0x{:08x} (sz=0x{:02x}): {}", addr, sz_cmd, e);
            return Ok(false);
        }
    }
    let ok = buf[0] == 0x04 && buf[1] == 0x0E && buf[2] == 0xFF;
    if !ok {
        log::warn!("Erase bad response at 0x{:08x}: {:02x?}", addr, &buf[..5]);
    }
    Ok(ok)
}

/// Write one 4096-byte sector.
fn write_sector_4k(port: &mut dyn serialport::SerialPort, addr: u32, data: &[u8; SECTOR_SIZE]) -> Result<bool> {
    let [a0, a1, a2, a3] = addr.to_le_bytes();
    let [l0, l1] = 4101u16.to_le_bytes();
    let mut tx = Vec::with_capacity(4108);
    tx.extend_from_slice(&[0x01, 0xE0, 0xFC, 0xFF, 0xF4, l0, l1, 0x07, a0, a1, a2, a3]);
    tx.extend_from_slice(data.as_slice());

    let _ = port.clear(serialport::ClearBuffer::Input);
    port.write_all(&tx)?;
    port.flush()?;

    let mut buf = [0u8; 15];
    match read_exact_timeout(port, &mut buf, Duration::from_secs(8)) {
        Ok(_) => {}
        Err(e) => {
            log::warn!("Write timeout at 0x{:08x}: {}", addr, e);
            return Ok(false);
        }
    }
    if buf[0] != 0x04 || buf[1] != 0x0E || buf[2] != 0xFF {
        log::warn!("Write bad response at 0x{:08x}: {:02x?}", addr, &buf[..5]);
        return Ok(false);
    }
    let echo = u32::from_le_bytes([buf[11], buf[12], buf[13], buf[14]]);
    if echo != addr {
        log::warn!("Write addr mismatch: sent 0x{:08x} got echo 0x{:08x}", addr, echo);
        return Ok(false);
    }
    Ok(true)
}

/// CRC check for a flash range [start_addr, end_addr] inclusive.
#[allow(dead_code)]
fn check_crc(port: &mut dyn serialport::SerialPort, start_addr: u32, end_addr: u32) -> Result<u32> {
    let [s0, s1, s2, s3] = start_addr.to_le_bytes();
    let [e0, e1, e2, e3] = end_addr.to_le_bytes();
    let tx = [0x01u8, 0xE0, 0xFC, 0x09, 0x10, s0, s1, s2, s3, e0, e1, e2, e3];
    let _ = port.clear(serialport::ClearBuffer::Input);
    port.write_all(&tx)?;
    port.flush()?;
    let mut buf = [0u8; 11];
    read_exact_timeout(port, &mut buf, Duration::from_secs(10))?;
    if buf[0] != 0x04 || buf[1] != 0x0E || buf[6] != 0x10 {
        bail!("CheckCRC: bad response {:02x?}", &buf);
    }
    Ok(u32::from_le_bytes([buf[7], buf[8], buf[9], buf[10]]))
}

// ─── Flash unprotect ──────────────────────────────────────────────────────────

/// Look up SR parameters for a given Flash MID.
fn flash_sr_params(mid: u32) -> (usize, [u8; 2], u32) {
    const M2: u32 = 0x407C; // 2-byte: clear CMP + BP0-BP4
    const M1: u32 = 0x007C; // 1-byte: clear BP0-BP4 only

    match mid {
        0x144051 | 0x134051 | 0x14405E | 0x13405E | 0x13311C => (1, [0x05, 0xFF], M1),
        0x1464C8 => (1, [0x05, 0xFF], M1),
        0x15701C => (1, [0x05, 0xFF], 0x003C),
        0x1423C2 | 0x1523C2 => (2, [0x05, 0x15], 0x3012),
        0x1560C4 => (2, [0x05, 0x35], M2),
        _ => {
            let cap = (mid >> 16) & 0xFF;
            if cap >= 0x14 {
                (2, [0x05, 0x35], M2)
            } else {
                (1, [0x05, 0x35], M1)
            }
        }
    }
}

/// Clear write-protection bits in the flash Status Register(s).
fn unprotect_flash(port: &mut dyn serialport::SerialPort, mid: u32) -> Result<()> {
    let (sz_sr, rd_cmds, cw_msk) = flash_sr_params(mid);
    let sr1 = read_flash_sr(port, rd_cmds[0])?;
    let sr_val: u16 = if sz_sr >= 2 && rd_cmds[1] != 0xFF {
        let sr2 = read_flash_sr(port, rd_cmds[1]).unwrap_or(0);
        ((sr2 as u16) << 8) | (sr1 as u16)
    } else {
        sr1 as u16
    };
    let new_val = sr_val & !(cw_msk as u16);
    if new_val == sr_val {
        log::info!("Flash already unprotected (SR=0x{:04x})", sr_val);
        return Ok(());
    }
    log::info!("Unprotect: SR 0x{:04x} → 0x{:04x}", sr_val, new_val);
    if sz_sr >= 2 && rd_cmds[1] != 0xFF {
        write_flash_sr_2(port, 0x01, new_val)?;
    } else {
        write_flash_sr_1(port, 0x01, new_val as u8)?;
    }
    std::thread::sleep(Duration::from_millis(20));
    Ok(())
}

// ─── Erase + write primitives ─────────────────────────────────────────────────

/// Erase `num_sectors` × 4K sectors starting at `start_addr`.
/// Uses 64K block erases where possible for speed.
fn erase_range<F>(port: &mut dyn serialport::SerialPort, start_addr: u32, num_sectors: usize, mut progress_cb: F) -> Result<()>
where
    F: FnMut(usize, usize) -> bool,
{
    let mut current = start_addr as usize / SECTOR_SIZE;
    let end = current + num_sectors;
    let mut done = 0usize;

    macro_rules! do_erase_4k {
        () => {{
            let addr = (current * SECTOR_SIZE) as u32;
            let mut ok = false;
            for _ in 0..5 {
                if erase_sector(port, addr, 0x20)? {
                    ok = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            if !ok {
                bail!("4K erase failed at 0x{:08x} after 5 retries", addr);
            }
            current += 1;
            done += 1;
            if !progress_cb(done.min(num_sectors), num_sectors) {
                return Ok(());
            }
        }};
    }

    // 1. Align to 64K block boundary with 4K erases
    while current < end && !current.is_multiple_of(SECTORS_PER_BLOCK) {
        do_erase_4k!();
    }
    // 2. 64K block erases
    while end - current >= SECTORS_PER_BLOCK {
        let addr = (current * SECTOR_SIZE) as u32;
        let mut ok = false;
        for _ in 0..5 {
            if erase_sector(port, addr, 0xD8)? {
                ok = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if !ok {
            bail!("64K erase failed at 0x{:08x} after 5 retries", addr);
        }
        current += SECTORS_PER_BLOCK;
        done += SECTORS_PER_BLOCK;
        if !progress_cb(done.min(num_sectors), num_sectors) {
            return Ok(());
        }
    }
    // 3. Remaining 4K erases
    while current < end {
        do_erase_4k!();
    }
    Ok(())
}

/// Erase then write `data` to flash at `start_addr`.
#[allow(clippy::too_many_arguments)]
fn flash_data(
    port: &mut dyn serialport::SerialPort,
    data: &[u8],
    start_addr: u32,
    pct_start: f32,
    pct_end: f32,
    label: &str,
    cancel: &AtomicBool,
    on_progress: &ProgressCallback,
) -> Result<()> {
    let num_sectors = data.len().div_ceil(SECTOR_SIZE);
    let erase_end = pct_start + (pct_end - pct_start) * 0.4;

    // Erase phase
    erase_range(port, start_addr, num_sectors, |done, total| {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        let pct = pct_start + (erase_end - pct_start) * (done as f32 / total as f32);
        on_progress(&FlashProgress::info("Erasing", pct, &format!("{label} – erasing {done}/{total}")).with_region(label));
        true
    })?;

    if cancel.load(Ordering::Relaxed) {
        bail!("Flash cancelled by user");
    }
    on_progress(&FlashProgress::info("Writing", erase_end, &format!("{label} – erase done, writing…")).with_region(label));

    // Write phase
    for i in 0..num_sectors {
        if cancel.load(Ordering::Relaxed) {
            bail!("Flash cancelled by user");
        }
        let offset = i * SECTOR_SIZE;
        let end_off = (offset + SECTOR_SIZE).min(data.len());
        let addr = start_addr
            .checked_add(offset as u32)
            .ok_or_else(|| anyhow::anyhow!("Flash 地址溢出: start={:#x} offset={:#x}", start_addr, offset))?;

        let mut sector = [0xFFu8; SECTOR_SIZE];
        sector[..end_off - offset].copy_from_slice(&data[offset..end_off]);

        let mut ok = false;
        for attempt in 0..3 {
            match write_sector_4k(port, addr, &sector)? {
                true => {
                    ok = true;
                    break;
                }
                false => {
                    log::warn!("Retry write sector {} attempt {}", i, attempt + 1);
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
        if !ok {
            bail!("Write failed at 0x{:08x} after 3 retries", addr);
        }

        let pct = erase_end + (pct_end - erase_end) * ((i + 1) as f32 / num_sectors as f32);
        on_progress(&FlashProgress::info("Writing", pct, &format!("{label} – writing {}/{num_sectors}", i + 1)).with_region(label));
    }
    Ok(())
}

// ─── Public API ───────────────────────────────────────────────────────────────

/// Boot log capture duration after flashing.
const LOG_CAPTURE_SECS: u64 = 20;

/// Flash via air602_flash.exe subprocess (BK7258 preferred path).
/// Returns captured boot log lines.
fn flash_via_subprocess(exe_path: &Path, rom_path: &Path, port: &str, log_br: u32, cancel: &AtomicBool, on_progress: &ProgressCallback) -> Result<Vec<String>> {
    // Strip "COM" prefix — air602_flash.exe wants a bare number
    let port_num: String = port.chars().filter(|c| c.is_ascii_digit()).collect();
    if port_num.is_empty() {
        bail!("Invalid port name: {port}");
    }

    on_progress(&FlashProgress::info("Flashing", 5.0, &format!("Starting air602_flash.exe on {port} (~37s)…")));

    let mut child = std::process::Command::new(exe_path)
        .args(["download", "-p", &port_num, "-b", "2000000", "-s", "0", "-i"])
        .arg(rom_path)
        .current_dir(exe_path.parent().unwrap_or(Path::new(".")))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("Failed to launch air602_flash.exe")?;

    // Background thread: drain subprocess stdout
    let stdout = child.stdout.take().expect("stdout piped");
    let on_progress_clone: Box<dyn Fn(&FlashProgress) + Send> = {
        // We need to share the callback; use a channel instead
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let progress_thread = std::thread::spawn(move || {
            use std::io::{BufRead, BufReader};
            for line in BufReader::new(stdout).lines().map_while(|l| l.ok()) {
                let t = line.trim().to_string();
                if !t.is_empty() {
                    let _ = tx.send(t);
                }
            }
        });

        // Wait for subprocess to exit
        let _status = child.wait().context("air602_flash.exe wait() failed")?;

        // Drain remaining messages
        let mut pct = 5.0f32;
        while let Ok(t) = rx.try_recv() {
            if t.contains("Gotten Bus") || t.contains("Gotten bus") {
                pct = 15.0;
            } else if t.contains("baudrate") {
                pct = 22.0;
            } else if t.contains("Boot_Reboot") {
                pct = 90.0;
            } else if t.contains("All Finished") {
                pct = 93.0;
            } else if t.contains("Writing") {
                pct = (pct + 0.4).min(88.0);
            }
            on_progress(&FlashProgress::info("Flashing", pct, &format!("[exe] {t}")));
        }
        let _ = progress_thread.join();

        // This is a dummy to satisfy the type; actual progress was already emitted
        Box::new(|_: &FlashProgress| {})
    };
    let _ = on_progress_clone; // silence unused warning

    on_progress(&FlashProgress::info("Booting", 94.0, &format!("Firmware sent! Opening {port} @ {log_br} for boot log…")));

    // Open log port immediately
    let mut log_port = {
        let mut last_err = String::new();
        let mut port_opt = None;
        for _ in 0..10 {
            match serialport::new(port, log_br).timeout(Duration::from_millis(200)).open() {
                Ok(p) => {
                    port_opt = Some(p);
                    break;
                }
                Err(e) => {
                    last_err = e.to_string();
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
        match port_opt {
            Some(p) => p,
            None => {
                on_progress(&FlashProgress::done_err(&format!("Cannot open log port {port}: {last_err}")));
                bail!("Cannot open log port {port}: {last_err}");
            }
        }
    };

    // Capture boot log
    let mut log_bytes: Vec<u8> = Vec::new();
    let mut read_buf = [0u8; 512];
    let deadline = Instant::now() + Duration::from_secs(LOG_CAPTURE_SECS);

    while Instant::now() < deadline {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        match log_port.read(&mut read_buf) {
            Ok(n) if n > 0 => log_bytes.extend_from_slice(&read_buf[..n]),
            _ => {}
        }
    }
    drop(log_port);

    let log_text = String::from_utf8_lossy(&log_bytes);
    let lines: Vec<String> = log_text.lines().map(|l| l.trim_end_matches('\r').to_string()).filter(|l| !l.is_empty()).collect();

    let boot_kw = ["luat:", "ap0:", "ap1:", "LuatOS", "EasyFlash"];
    let passed = boot_kw.iter().any(|kw| log_text.contains(kw));

    if passed {
        on_progress(&FlashProgress::done_ok(&format!(
            "PASS — firmware booted ({} log lines, {} bytes)",
            lines.len(),
            log_bytes.len()
        )));
    } else {
        on_progress(&FlashProgress::done_err(&format!("FAIL — no boot keywords in {} bytes of log", log_bytes.len())));
    }

    Ok(lines)
}

/// Full BK7258 (Air8101) flash routine.
///
/// If `air602_flash.exe` is found in the .soc archive, uses subprocess mode.
/// Otherwise uses native Rust serial protocol.
///
/// Returns captured boot log lines.
pub fn flash_bk7258(
    soc_path: &str,
    script_folders: Option<&[&str]>,
    port: &str,
    baud_rate: Option<u32>,
    cancel: Arc<AtomicBool>,
    on_progress: ProgressCallback,
) -> Result<Vec<String>> {
    cancel.store(false, Ordering::Relaxed);

    on_progress(&FlashProgress::info("Preparing", 1.0, "Extracting .soc…"));

    // 1. Extract .soc
    let tempdir = tempfile::tempdir().context("Failed to create temp dir")?;
    {
        let file = std::fs::File::open(soc_path).with_context(|| format!("Cannot open {soc_path}"))?;
        let mut archive = zip::ZipArchive::new(file)?;
        archive.extract(tempdir.path()).context("Extraction failed")?;
    }

    // 2. Parse info.json
    let info: SocInfo = serde_json::from_reader(std::fs::File::open(tempdir.path().join("info.json")).context("info.json missing")?).context("Parse info.json")?;

    let log_br = info.log_baud_rate();
    let rom_path = tempdir.path().join(&info.rom.file);
    if !rom_path.exists() {
        bail!("ROM file '{}' not found in .soc", info.rom.file);
    }

    // 3. Subprocess path (preferred): use bundled air602_flash.exe
    let exe_path = tempdir.path().join("air602_flash.exe");
    if exe_path.exists() {
        if script_folders.is_some() {
            on_progress(&FlashProgress::info(
                "Preparing",
                2.0,
                "[WARN] Script flashing not supported in subprocess mode; firmware only.",
            ));
        }
        on_progress(&FlashProgress::info("Preparing", 3.0, &format!("Firmware: {} (subprocess mode)", info.rom.file)));
        return flash_via_subprocess(&exe_path, &rom_path, port, log_br, &cancel, &on_progress);
    }

    // 4. Native Rust path (fallback)
    let flash_br = info.flash_baud_rate();
    let flash_br = baud_rate.unwrap_or(flash_br);

    let bl_addr = parse_addr(info.download.bl_addr.as_deref().unwrap_or("0")).unwrap_or(0) as u32;
    let rom_data = std::fs::read(&rom_path).with_context(|| format!("Cannot read {}", info.rom.file))?;

    on_progress(&FlashProgress::info(
        "Preparing",
        2.0,
        &format!("Firmware: {} bytes @ 0x{:x} (native mode)", rom_data.len(), bl_addr),
    ));

    // Optionally build script.bin
    let script_payload: Option<(Vec<u8>, u32)> = if let Some(folders) = script_folders {
        let paths: Vec<&Path> = folders.iter().map(|f| Path::new(*f)).collect();
        let any_valid = paths.iter().any(|p| p.is_dir());
        if any_valid {
            on_progress(&FlashProgress::info("Building", 3.0, "Building script.bin from Lua files…"));
            let valid_paths: Vec<&Path> = paths.into_iter().filter(|p| p.is_dir()).collect();
            match build_script_bin(&valid_paths, &info) {
                Ok(data) => {
                    let sa = parse_addr(info.download.script_addr.as_deref().unwrap_or("0x200000")).unwrap_or(0x200000) as u32;
                    on_progress(&FlashProgress::info("Building", 5.0, &format!("Script: {} bytes @ 0x{:x}", data.len(), sa)));
                    Some((data, sa))
                }
                Err(e) => {
                    on_progress(&FlashProgress::info("Building", 5.0, &format!("[WARN] Script build failed: {e} — firmware only")));
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    if cancel.load(Ordering::Relaxed) {
        bail!("Flash cancelled by user");
    }

    on_progress(&FlashProgress::info("Connecting", 6.0, &format!("Opening {port} @ 115200…")));
    let mut serial = serialport::new(port, 115_200)
        .timeout(Duration::from_millis(200))
        .open()
        .with_context(|| format!("Cannot open serial port {port}"))?;

    on_progress(&FlashProgress::info("Connecting", 8.0, "Resetting device into bootloader…"));
    if !get_bus(&mut *serial)? {
        bail!("Cannot enter bootloader mode on {port}. Check cable and DTR/RTS wiring.");
    }
    on_progress(&FlashProgress::info("Connecting", 10.0, "Bootloader link established!"));

    if flash_br != 115_200 {
        on_progress(&FlashProgress::info("Connecting", 11.0, &format!("Switching to {flash_br} bps…")));
        match set_baud_rate(&mut *serial, flash_br, 200) {
            Ok(true) => {
                on_progress(&FlashProgress::info("Connecting", 12.0, &format!("Baud rate set to {flash_br}")));
            }
            Ok(false) => {
                on_progress(&FlashProgress::info("Connecting", 12.0, "[WARN] Baud rate switch ACK failed — continuing at 115200"));
            }
            Err(e) => {
                on_progress(&FlashProgress::info("Connecting", 12.0, &format!("[WARN] set_baud_rate error: {e}")));
            }
        }
    }

    if cancel.load(Ordering::Relaxed) {
        bail!("Flash cancelled by user");
    }

    let mid = match get_flash_mid(&mut *serial) {
        Ok(m) => {
            on_progress(&FlashProgress::info("Connecting", 13.0, &format!("Flash MID: 0x{m:06x}")));
            m
        }
        Err(e) => {
            on_progress(&FlashProgress::info("Connecting", 13.0, &format!("[WARN] GetFlashMID: {e}")));
            0
        }
    };

    on_progress(&FlashProgress::info("Connecting", 14.0, "Unprotecting flash…"));
    if let Err(e) = unprotect_flash(&mut *serial, mid) {
        on_progress(&FlashProgress::info("Connecting", 14.0, &format!("[WARN] Unprotect: {e}")));
    } else {
        on_progress(&FlashProgress::info("Connecting", 15.0, "Flash unprotected"));
    }

    if cancel.load(Ordering::Relaxed) {
        bail!("Flash cancelled by user");
    }

    let fw_end_pct = if script_payload.is_some() { 80.0f32 } else { 98.0f32 };
    flash_data(&mut *serial, &rom_data, bl_addr, 15.0, fw_end_pct, "Firmware", &cancel, &on_progress)?;

    if cancel.load(Ordering::Relaxed) {
        bail!("Flash cancelled by user");
    }

    if let Some((script_data, script_addr)) = script_payload {
        flash_data(&mut *serial, &script_data, script_addr, 80.0, 98.0, "Script", &cancel, &on_progress)?;
    }

    drop(serial);
    on_progress(&FlashProgress::done_ok("Flash complete! Device is rebooting."));
    Ok(vec![])
}

// ─── Script synthesis ─────────────────────────────────────────────────────────

/// Build a LuaDB script.bin from Lua files in `folder`.
fn build_script_bin(folders: &[&Path], info: &SocInfo) -> Result<Vec<u8>> {
    let use_bkcrc = info.use_bkcrc();
    let use_luac = info.script_use_luac();
    let bitw = info.script_bitw();
    let strip = info.script_strip_debug();

    let mut entries: Vec<luatos_luadb::LuadbEntry> = Vec::new();

    for folder in folders {
        for entry in std::fs::read_dir(folder).with_context(|| format!("Cannot read script folder: {}", folder.display()))? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let data = std::fs::read(&path).with_context(|| format!("Cannot read {}", path.display()))?;

            // Compile .lua files if use_luac is enabled
            let (final_name, final_data) = if use_luac && name.ends_with(".lua") && !name.ends_with(".luac") {
                let chunk_name = format!("@{}", name);
                let bytecode = luatos_luadb::build::compile_lua_bytes(&data, &chunk_name, strip, bitw).with_context(|| format!("Failed to compile {}", path.display()))?;
                let luac_name = format!("{}c", name); // .lua → .luac
                log::info!("compiled {} (bitw={}, strip={})", name, bitw, strip);
                (luac_name, bytecode)
            } else {
                (name, data)
            };

            entries.push(luatos_luadb::LuadbEntry {
                filename: final_name,
                data: final_data,
            });
        }
    }

    if entries.is_empty() {
        bail!("No files found in script folders");
    }

    let mut data = luatos_luadb::pack_luadb(&entries)?;
    if use_bkcrc {
        data = luatos_luadb::add_bk_crc(&data);
    }

    log::info!("script.bin: {} bytes (bkcrc={}, luac={}, bitw={})", data.len(), use_bkcrc, use_luac, bitw);
    Ok(data)
}

// ─── Bootloader connection helper ─────────────────────────────────────────────

/// Connect to BK7258 bootloader and prepare for flash operations.
///
/// Returns an open serial port that has completed handshake, baud switch, and SR unprotect.
fn connect_bootloader(port: &str, flash_br: u32, cancel: &AtomicBool, on_progress: &ProgressCallback) -> Result<Box<dyn serialport::SerialPort>> {
    on_progress(&FlashProgress::info("Connecting", 5.0, &format!("Opening {port} @ 115200…")));
    let mut serial = serialport::new(port, 115_200)
        .timeout(Duration::from_millis(200))
        .open()
        .with_context(|| format!("Cannot open serial port {port}"))?;

    on_progress(&FlashProgress::info("Connecting", 10.0, "Resetting device into bootloader…"));
    if !get_bus(&mut *serial)? {
        bail!("Cannot enter bootloader mode on {port}. Check cable and DTR/RTS wiring.");
    }
    on_progress(&FlashProgress::info("Connecting", 15.0, "Bootloader link established!"));

    if flash_br != 115_200 {
        on_progress(&FlashProgress::info("Connecting", 18.0, &format!("Switching to {flash_br} bps…")));
        match set_baud_rate(&mut *serial, flash_br, 200) {
            Ok(true) => {
                on_progress(&FlashProgress::info("Connecting", 20.0, &format!("Baud rate set to {flash_br}")));
            }
            Ok(false) => {
                on_progress(&FlashProgress::info("Connecting", 20.0, "[WARN] Baud rate switch ACK failed — continuing at 115200"));
            }
            Err(e) => {
                on_progress(&FlashProgress::info("Connecting", 20.0, &format!("[WARN] set_baud_rate error: {e}")));
            }
        }
    }

    if cancel.load(Ordering::Relaxed) {
        bail!("Flash cancelled by user");
    }

    match get_flash_mid(&mut *serial) {
        Ok(m) => {
            on_progress(&FlashProgress::info("Connecting", 22.0, &format!("Flash MID: 0x{m:06x}")));
            unprotect_flash(&mut *serial, m)?;
            on_progress(&FlashProgress::info("Connecting", 25.0, "Flash SR unprotected"));
        }
        Err(e) => {
            on_progress(&FlashProgress::info("Connecting", 22.0, &format!("[WARN] GetFlashMID: {e}")));
        }
    }

    Ok(serial)
}

// ─── Partition-level operations ───────────────────────────────────────────────

/// Flash only the script partition using native ISP protocol.
///
/// This is the most common operation during development:
///   get_bus → set_baud → unprotect → build LuaDB → erase+write at script_addr
pub fn flash_script_only(soc_path: &str, script_folders: &[&str], port: &str, cancel: Arc<AtomicBool>, on_progress: ProgressCallback) -> Result<()> {
    cancel.store(false, Ordering::Relaxed);
    on_progress(&FlashProgress::info("Preparing", 1.0, "Parsing SOC info…"));

    // Parse SOC info
    let file = std::fs::File::open(soc_path).with_context(|| format!("Cannot open {soc_path}"))?;
    let mut archive = zip::ZipArchive::new(file)?;
    let info: SocInfo = {
        let info_file = archive.by_name("info.json").context("info.json missing")?;
        serde_json::from_reader(info_file)?
    };

    let flash_br = info.flash_baud_rate();
    let script_addr = info.script_addr();

    on_progress(&FlashProgress::info("Preparing", 3.0, &format!("Script addr: 0x{script_addr:06x}, baud: {flash_br}")));

    // Build LuaDB script
    on_progress(&FlashProgress::info("Building", 5.0, "Packing script LuaDB…"));
    let script_data = {
        let paths: Vec<&Path> = script_folders.iter().map(|f| Path::new(*f)).collect();
        build_script_bin(&paths, &info)?
    };
    let script_size = info.script_size();
    if script_data.len() > script_size {
        bail!("Script data ({} bytes) exceeds partition size ({} bytes)", script_data.len(), script_size);
    }

    // Connect bootloader
    let mut serial = connect_bootloader(port, flash_br, &cancel, &on_progress)?;

    if cancel.load(Ordering::Relaxed) {
        bail!("Flash cancelled by user");
    }

    // Flash script
    flash_data(&mut *serial, &script_data, script_addr, 30.0, 95.0, "Script", &cancel, &on_progress)?;

    drop(serial);
    on_progress(&FlashProgress::done_ok("Script flash complete! Device is rebooting."));
    Ok(())
}

/// Erase the filesystem partition (fill with 0xFF).
pub fn clear_filesystem(soc_path: &str, port: &str, cancel: Arc<AtomicBool>, on_progress: ProgressCallback) -> Result<()> {
    cancel.store(false, Ordering::Relaxed);
    on_progress(&FlashProgress::info("Preparing", 1.0, "Parsing SOC info…"));

    let file = std::fs::File::open(soc_path).with_context(|| format!("Cannot open {soc_path}"))?;
    let mut archive = zip::ZipArchive::new(file)?;
    let info: SocInfo = {
        let info_file = archive.by_name("info.json").context("info.json missing")?;
        serde_json::from_reader(info_file)?
    };

    let (fs_addr, fs_size) = info.filesystem_partition().context("Filesystem partition not found in info.json")?;
    let flash_br = info.flash_baud_rate();

    on_progress(&FlashProgress::info("Preparing", 3.0, &format!("FS partition: 0x{fs_addr:06x}, {fs_size} bytes")));

    let mut serial = connect_bootloader(port, flash_br, &cancel, &on_progress)?;

    if cancel.load(Ordering::Relaxed) {
        bail!("Flash cancelled by user");
    }

    let num_sectors = fs_size.div_ceil(SECTOR_SIZE);
    on_progress(&FlashProgress::info("Erasing", 30.0, &format!("Erasing {num_sectors} sectors at 0x{fs_addr:06x}…")));

    erase_range(&mut *serial, fs_addr, num_sectors, |done, total| {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        let pct = 30.0 + 65.0 * (done as f32 / total as f32);
        on_progress(&FlashProgress::info("Erasing", pct, &format!("Filesystem erase {done}/{total}")));
        true
    })?;

    drop(serial);
    on_progress(&FlashProgress::done_ok("Filesystem cleared! Device is rebooting."));
    Ok(())
}

/// Build LuaDB from script folders and flash to filesystem partition.
pub fn flash_filesystem(soc_path: &str, script_folders: &[&str], port: &str, cancel: Arc<AtomicBool>, on_progress: ProgressCallback) -> Result<()> {
    cancel.store(false, Ordering::Relaxed);
    on_progress(&FlashProgress::info("Preparing", 1.0, "Parsing SOC info…"));

    let file = std::fs::File::open(soc_path).with_context(|| format!("Cannot open {soc_path}"))?;
    let mut archive = zip::ZipArchive::new(file)?;
    let info: SocInfo = {
        let info_file = archive.by_name("info.json").context("info.json missing")?;
        serde_json::from_reader(info_file)?
    };

    let (fs_addr, fs_size) = info.filesystem_partition().context("Filesystem partition not found in info.json")?;
    let flash_br = info.flash_baud_rate();

    on_progress(&FlashProgress::info("Building", 3.0, "Packing filesystem LuaDB…"));
    let fs_data = {
        let paths: Vec<&Path> = script_folders.iter().map(|f| Path::new(*f)).collect();
        build_script_bin(&paths, &info)?
    };
    if fs_data.len() > fs_size {
        bail!("Filesystem data ({} bytes) exceeds partition ({} bytes)", fs_data.len(), fs_size);
    }

    let mut serial = connect_bootloader(port, flash_br, &cancel, &on_progress)?;

    if cancel.load(Ordering::Relaxed) {
        bail!("Flash cancelled by user");
    }

    flash_data(&mut *serial, &fs_data, fs_addr, 30.0, 95.0, "Filesystem", &cancel, &on_progress)?;

    drop(serial);
    on_progress(&FlashProgress::done_ok("Filesystem flash complete! Device is rebooting."));
    Ok(())
}

/// Erase the FSKV (key-value) partition.
pub fn clear_fskv(soc_path: &str, port: &str, cancel: Arc<AtomicBool>, on_progress: ProgressCallback) -> Result<()> {
    cancel.store(false, Ordering::Relaxed);
    on_progress(&FlashProgress::info("Preparing", 1.0, "Parsing SOC info…"));

    let file = std::fs::File::open(soc_path).with_context(|| format!("Cannot open {soc_path}"))?;
    let mut archive = zip::ZipArchive::new(file)?;
    let info: SocInfo = {
        let info_file = archive.by_name("info.json").context("info.json missing")?;
        serde_json::from_reader(info_file)?
    };

    let (kv_addr, kv_size) = info.kv_partition().context("FSKV partition not found in info.json")?;
    let flash_br = info.flash_baud_rate();

    on_progress(&FlashProgress::info("Preparing", 3.0, &format!("FSKV partition: 0x{kv_addr:06x}, {kv_size} bytes")));

    let mut serial = connect_bootloader(port, flash_br, &cancel, &on_progress)?;

    if cancel.load(Ordering::Relaxed) {
        bail!("Flash cancelled by user");
    }

    let num_sectors = kv_size.div_ceil(SECTOR_SIZE);
    on_progress(&FlashProgress::info("Erasing", 30.0, &format!("Erasing {num_sectors} sectors at 0x{kv_addr:06x}…")));

    erase_range(&mut *serial, kv_addr, num_sectors, |done, total| {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        let pct = 30.0 + 65.0 * (done as f32 / total as f32);
        on_progress(&FlashProgress::info("Erasing", pct, &format!("FSKV erase {done}/{total}")));
        true
    })?;

    drop(serial);
    on_progress(&FlashProgress::done_ok("FSKV cleared! Device is rebooting."));
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flash_sr_params_known_chips() {
        let (sz, cmds, mask) = flash_sr_params(0x144051);
        assert_eq!(sz, 1);
        assert_eq!(cmds, [0x05, 0xFF]);
        assert_eq!(mask, 0x007C);
    }

    #[test]
    fn test_flash_sr_params_heuristic() {
        // Unknown chip with capacity ≥ 0x14 → 2-byte SR
        let (sz, _, mask) = flash_sr_params(0x1500AA);
        assert_eq!(sz, 2);
        assert_eq!(mask, 0x407C);
    }

    /// Non-destructive handshake test.
    /// Run with: cargo test bk_live_handshake -- --ignored --nocapture
    #[test]
    #[ignore = "requires Air8101 physically connected on COM6"]
    fn bk_live_handshake() {
        let port = "COM6";
        println!("\n=== BK7231 live handshake test on {port} ===");

        let mut serial = serialport::new(port, 115_200).timeout(Duration::from_millis(200)).open().expect("open COM6");

        println!("[1] get_bus …");
        let ok = get_bus(&mut *serial).expect("get_bus");
        assert!(ok, "Failed to enter bootloader");
        println!("    LinkCheck ACK ✓");

        println!("[2] set_baud_rate(2_000_000) …");
        let ok = set_baud_rate(&mut *serial, 2_000_000, 200).expect("set_baud_rate");
        assert!(ok, "Baud rate switch failed");
        println!("    Baud rate ✓");

        println!("[3] get_flash_mid …");
        let mid = get_flash_mid(&mut *serial).expect("get_flash_mid");
        println!("    MID = 0x{:06x} ✓", mid);
        assert_ne!(mid, 0);

        println!("[4] unprotect_flash …");
        unprotect_flash(&mut *serial, mid).expect("unprotect_flash");
        println!("    Unprotected ✓");

        println!("\n=== Handshake PASSED ===");
    }
}
