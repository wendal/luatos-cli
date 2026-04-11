// XT804 (Air6208 / Air101) serial flasher.
//
// Protocol reference: wm_tool.c from luatos-soc-air101
//
// Flash sequence:
//   1. Extract .soc (7z) to temp dir; parse info.json.
//   2. Open COM port at 115200 baud.
//   3. Toggle DTR/RTS to reset device into bootloader.
//   4. Send ESC (0x1B) repeatedly until 3 consecutive 'C' or 'P' replies.
//   5. Switch to target baud rate (default 2 Mbps).
//   6. Send erase command, wait for 3 consecutive 'C'/'P'.
//   7. Transfer firmware image via XMODEM-1K (STX, 1024-byte blocks, CRC16-CCITT).
//   8. Send reset command to reboot device.
//
// Image format:
//   - 256-byte header: magic 0xA0FFFF9F @ offset 0, CRC32s, run address, etc.
//   - Body: raw firmware binary

use anyhow::{bail, Context, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{FlashProgress, ProgressCallback};

// ─── Constants ─────────────────────────────────────────────────────────────────

#[allow(dead_code)]
const XMODEM_SOH: u8 = 0x01;
const XMODEM_STX: u8 = 0x02;
const XMODEM_EOT: u8 = 0x04;
const XMODEM_ACK: u8 = 0x06;
const XMODEM_NAK: u8 = 0x15;
const XMODEM_CAN: u8 = 0x18;

const XMODEM_DATA_SIZE_1K: usize = 1024;
const XMODEM_CRC_SIZE: usize = 2;

const IMAGE_HEADER_SIZE: usize = 256;
const IMAGE_MAGIC: u32 = 0xA0FFFF9F;

// XT804 predefined command sequences (from wm_tool.c)

/// Set baud to 115200
const CMD_BAUD_115200: [u8; 13] = [
    0x21, 0x0a, 0x00, 0x97, 0x4b, 0x31, 0x00, 0x00, 0x00, 0x00, 0xc2, 0x01, 0x00,
];

/// Set baud to 460800
const CMD_BAUD_460800: [u8; 13] = [
    0x21, 0x0a, 0x00, 0x07, 0x00, 0x31, 0x00, 0x00, 0x00, 0x00, 0x08, 0x07, 0x00,
];

/// Set baud to 921600
const CMD_BAUD_921600: [u8; 13] = [
    0x21, 0x0a, 0x00, 0x5d, 0x63, 0x31, 0x00, 0x00, 0x00, 0x00, 0x10, 0x0e, 0x00,
];

/// Set baud to 1000000
const CMD_BAUD_1000000: [u8; 13] = [
    0x21, 0x0a, 0x00, 0x7e, 0x1a, 0x31, 0x00, 0x00, 0x00, 0x40, 0x42, 0x0f, 0x00,
];

/// Set baud to 2000000
const CMD_BAUD_2000000: [u8; 13] = [
    0x21, 0x0a, 0x00, 0xef, 0x2a, 0x31, 0x00, 0x00, 0x00, 0x80, 0x84, 0x1e, 0x00,
];

/// Erase command
const CMD_ERASE: [u8; 13] = [
    0x21, 0x0a, 0x00, 0xc3, 0x35, 0x32, 0x00, 0x00, 0x00, 0x02, 0x00, 0xfe, 0x01,
];

/// Read MAC address
const CMD_MAC_READ: [u8; 9] = [
    0x21, 0x06, 0x00, 0xea, 0x2d, 0x38, 0x00, 0x00, 0x00,
];

/// Reset device
const CMD_RESET: [u8; 9] = [
    0x21, 0x06, 0x00, 0xc7, 0x7c, 0x3f, 0x00, 0x00, 0x00,
];

// ─── CRC16-CCITT ──────────────────────────────────────────────────────────────

/// CRC16-CCITT for XMODEM (polynomial 0x1021, init 0).
fn crc16_ccitt(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

// ─── Serial helpers ───────────────────────────────────────────────────────────

/// Read bytes from serial port with timeout.
fn serial_read(port: &mut dyn serialport::SerialPort, buf: &mut [u8], timeout: Duration) -> usize {
    let deadline = Instant::now() + timeout;
    let mut n = 0;
    while n < buf.len() && Instant::now() < deadline {
        match port.read(&mut buf[n..]) {
            Ok(k) if k > 0 => n += k,
            _ => std::thread::sleep(Duration::from_millis(1)),
        }
    }
    n
}

/// Drain any pending data from the serial receive buffer.
fn serial_drain(port: &mut dyn serialport::SerialPort) {
    let mut junk = [0u8; 512];
    let _ = port.set_timeout(Duration::from_millis(5));
    loop {
        match port.read(&mut junk) {
            Ok(k) if k > 0 => continue,
            _ => break,
        }
    }
}

// ─── DTR/RTS reset sequence ──────────────────────────────────────────────────

/// Reset the device into bootloader mode using DTR/RTS signals.
///
/// Sequence from wm_tool.c (RTS reset mode):
///   DTR=0, RTS=1 (50ms) → DTR=1, RTS=0 (50ms) → DTR=0
fn reset_to_bootloader(port: &mut dyn serialport::SerialPort) -> Result<()> {
    // Phase 1: DTR=0 (assert reset), RTS=1 (assert boot mode)
    port.write_data_terminal_ready(false)?;
    port.write_request_to_send(true)?;
    std::thread::sleep(Duration::from_millis(50));

    // Phase 2: DTR=1 (release reset), RTS=0 (release boot)
    port.write_data_terminal_ready(true)?;
    port.write_request_to_send(false)?;
    std::thread::sleep(Duration::from_millis(50));

    // Phase 3: DTR=0 (final state)
    port.write_data_terminal_ready(false)?;
    std::thread::sleep(Duration::from_millis(50));

    Ok(())
}

// ─── ESC burst helper ────────────────────────────────────────────────────────

/// Send ESC (0x1B) bytes at 10ms intervals for `duration_ms` milliseconds.
fn send_esc_burst(port: &mut dyn serialport::SerialPort, duration_ms: u32) {
    let count = duration_ms / 10;
    for _ in 0..count {
        let _ = port.write_all(&[0x1B]);
        let _ = port.flush();
        std::thread::sleep(Duration::from_millis(10));
    }
}

// ─── Sync phase ──────────────────────────────────────────────────────────────

/// Sync with the bootloader by sending ESC (0x1B) and waiting for 'C' or 'P' responses.
/// Need 3 consecutive valid responses to confirm synchronization.
///
/// Protocol from wm_tool.c:
///   1. Send initial 50 ESC bytes (500ms burst)
///   2. Loop: read response; on 'C'/'P' increment counter; on other reset counter
///   3. On read timeout, send 3 ESC bytes (30ms burst)
///   4. Toggle RTS every 2 seconds as recovery
///   5. Success when counter reaches 3
///   6. Timeout after 60 seconds
fn sync_bootloader(
    port: &mut dyn serialport::SerialPort,
    cancel: &AtomicBool,
) -> Result<()> {
    let _ = port.set_timeout(Duration::from_millis(10));

    // Initial ESC burst: 50 bytes over 500ms
    send_esc_burst(port, 500);

    let start = Instant::now();
    let timeout = Duration::from_secs(60);
    let mut consecutive = 0u32;
    let mut last_toggle = Instant::now();
    let mut rts_state = false;

    while start.elapsed() < timeout {
        if cancel.load(Ordering::Relaxed) {
            bail!("Sync cancelled");
        }

        // Try to read a response byte
        let mut buf = [0u8; 1];
        match port.read(&mut buf) {
            Ok(1) => {
                if buf[0] == b'C' || buf[0] == b'P' {
                    consecutive += 1;
                    if consecutive >= 3 {
                        return Ok(());
                    }
                } else {
                    consecutive = 0;
                }
            }
            _ => {
                // No data — send retry burst (3 ESC bytes over 30ms)
                send_esc_burst(port, 30);
            }
        }

        // Toggle RTS every 2 seconds as recovery mechanism
        if last_toggle.elapsed() >= Duration::from_secs(2) {
            rts_state = !rts_state;
            let _ = port.write_request_to_send(rts_state);
            last_toggle = Instant::now();
        }
    }

    bail!("Sync timeout: no bootloader response after 60s");
}

// ─── Baud rate switch ────────────────────────────────────────────────────────

/// Get the baud rate command for the target baud rate.
fn get_baud_cmd(baud: u32) -> &'static [u8; 13] {
    match baud {
        460800 => &CMD_BAUD_460800,
        921600 => &CMD_BAUD_921600,
        1000000 => &CMD_BAUD_1000000,
        2000000 => &CMD_BAUD_2000000,
        _ => &CMD_BAUD_115200,
    }
}

/// Switch the baud rate. Sends command to device, waits, then changes host side.
fn set_baud_rate(port: &mut dyn serialport::SerialPort, baud: u32) -> Result<()> {
    if baud == 115200 {
        return Ok(()); // Already at default
    }

    let cmd = get_baud_cmd(baud);
    port.write_all(cmd)?;
    port.flush()?;

    // Wait for device to switch
    std::thread::sleep(Duration::from_secs(1));

    // Switch host side
    port.set_baud_rate(baud)
        .context("Failed to change host baud rate")?;

    Ok(())
}

// ─── Erase ───────────────────────────────────────────────────────────────────

/// Send erase command and wait for 3 consecutive 'C'/'P' responses.
fn erase_flash(
    port: &mut dyn serialport::SerialPort,
    cancel: &AtomicBool,
) -> Result<()> {
    port.write_all(&CMD_ERASE)?;
    port.flush()?;

    // Erase can take a long time — up to 60 seconds
    let start = Instant::now();
    let timeout = Duration::from_secs(60);
    let mut consecutive = 0u32;

    let _ = port.set_timeout(Duration::from_millis(100));

    while start.elapsed() < timeout {
        if cancel.load(Ordering::Relaxed) {
            bail!("Erase cancelled");
        }

        let mut buf = [0u8; 1];
        match port.read(&mut buf) {
            Ok(1) if buf[0] == b'C' || buf[0] == b'P' => {
                consecutive += 1;
                if consecutive >= 3 {
                    return Ok(());
                }
            }
            Ok(1) => {
                consecutive = 0;
            }
            _ => {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    bail!("Erase timeout: no completion signal after {timeout:?}");
}

// ─── XMODEM-1K transfer ──────────────────────────────────────────────────────

/// Transfer firmware image using XMODEM-1K protocol.
///
/// Frame format: [STX, blk#, 255-blk#, 1024_data, CRC16_HI, CRC16_LO]
fn xmodem_transfer(
    port: &mut dyn serialport::SerialPort,
    data: &[u8],
    cancel: &AtomicBool,
    on_progress: &ProgressCallback,
) -> Result<()> {
    let total_blocks = (data.len() + XMODEM_DATA_SIZE_1K - 1) / XMODEM_DATA_SIZE_1K;
    let mut block_num: u8 = 1; // XMODEM block numbers start at 1

    let _ = port.set_timeout(Duration::from_millis(100));

    for (i, chunk) in data.chunks(XMODEM_DATA_SIZE_1K).enumerate() {
        if cancel.load(Ordering::Relaxed) {
            // Send CAN to abort
            let _ = port.write_all(&[XMODEM_CAN]);
            bail!("Transfer cancelled");
        }

        // Pad to 1024 bytes if needed
        let mut block = [0u8; XMODEM_DATA_SIZE_1K];
        block[..chunk.len()].copy_from_slice(chunk);

        // Build frame: STX + block_num + ~block_num + data + CRC16
        let crc = crc16_ccitt(&block);
        let mut frame = Vec::with_capacity(3 + XMODEM_DATA_SIZE_1K + XMODEM_CRC_SIZE);
        frame.push(XMODEM_STX);
        frame.push(block_num);
        frame.push(!block_num);
        frame.extend_from_slice(&block);
        frame.push((crc >> 8) as u8); // CRC high byte
        frame.push((crc & 0xFF) as u8); // CRC low byte

        // Retry loop
        let mut sent = false;
        for retry in 0..100 {
            port.write_all(&frame)?;
            port.flush()?;

            // Wait for ACK/NAK
            let mut resp = [0u8; 1];
            let n = serial_read(port, &mut resp, Duration::from_secs(5));
            if n == 1 {
                match resp[0] {
                    XMODEM_ACK => {
                        sent = true;
                        break;
                    }
                    XMODEM_NAK => {
                        // Retry
                        if retry >= 99 {
                            bail!("XMODEM block {block_num}: max retries exceeded");
                        }
                        continue;
                    }
                    XMODEM_CAN => {
                        bail!("XMODEM: receiver cancelled transfer");
                    }
                    _ => {
                        // Unexpected byte, retry
                        continue;
                    }
                }
            } else {
                // Timeout, retry
                continue;
            }
        }

        if !sent {
            bail!("XMODEM block {block_num}: failed after retries");
        }

        block_num = block_num.wrapping_add(1);

        // Report progress
        let pct = ((i + 1) as f32 / total_blocks as f32) * 100.0;
        on_progress(&FlashProgress::info(
            "Write",
            pct,
            &format!("Block {}/{total_blocks}", i + 1),
        ));
    }

    // Send EOT to end transfer
    for _ in 0..10 {
        port.write_all(&[XMODEM_EOT])?;
        port.flush()?;

        let mut resp = [0u8; 1];
        let n = serial_read(port, &mut resp, Duration::from_secs(2));
        if n == 1 && resp[0] == XMODEM_ACK {
            return Ok(());
        }
    }

    bail!("XMODEM EOT: no ACK from receiver");
}

// ─── Image verification ──────────────────────────────────────────────────────

/// Verify the XT804 image header.
/// Header is 256 bytes, magic at offset 0 = 0xA0FFFF9F.
fn verify_image(data: &[u8]) -> Result<()> {
    if data.len() < IMAGE_HEADER_SIZE {
        bail!("Image too small: {} bytes (need >= {IMAGE_HEADER_SIZE})", data.len());
    }

    let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    if magic != IMAGE_MAGIC {
        bail!("Invalid image magic: 0x{magic:08X} (expected 0x{IMAGE_MAGIC:08X})");
    }

    Ok(())
}

// ─── MAC address ─────────────────────────────────────────────────────────────

/// Read MAC address from device. Returns "AA:BB:CC:DD:EE:FF" format.
pub fn read_mac(port: &mut dyn serialport::SerialPort) -> Result<String> {
    port.write_all(&CMD_MAC_READ)?;
    port.flush()?;

    let mut buf = [0u8; 64];
    let n = serial_read(port, &mut buf, Duration::from_secs(3));
    let response = String::from_utf8_lossy(&buf[..n]);

    // Look for "MAC:AABBCCDDEEFF\n"
    if let Some(mac_start) = response.find("MAC:") {
        let mac_hex = &response[mac_start + 4..];
        let mac_hex = mac_hex.trim();
        if mac_hex.len() >= 12 {
            let formatted = format!(
                "{}:{}:{}:{}:{}:{}",
                &mac_hex[0..2],
                &mac_hex[2..4],
                &mac_hex[4..6],
                &mac_hex[6..8],
                &mac_hex[8..10],
                &mac_hex[10..12],
            );
            return Ok(formatted);
        }
    }

    bail!("Failed to read MAC address");
}

// ─── Reset ───────────────────────────────────────────────────────────────────

/// Send reset command to reboot the device.
fn reset_device(port: &mut dyn serialport::SerialPort) -> Result<()> {
    port.write_all(&CMD_RESET)?;
    port.flush()?;
    std::thread::sleep(Duration::from_millis(500));
    Ok(())
}

// ─── Public API ──────────────────────────────────────────────────────────────

/// Open port, enter bootloader, and prepare for flash operations.
/// Returns the serial port handle at the target baud rate.
fn connect_bootloader(
    port: &str,
    target_baud: u32,
    cancel: &Arc<AtomicBool>,
    on_progress: &ProgressCallback,
) -> Result<Box<dyn serialport::SerialPort>> {
    on_progress(&FlashProgress::info("Connect", 0.0, &format!("Opening {port} at 115200")));

    let mut serial = serialport::new(port, 115200)
        .timeout(Duration::from_millis(100))
        .open()
        .with_context(|| format!("Cannot open serial port: {port}"))?;

    // Allow serial port to stabilize (per wm_tool.c)
    std::thread::sleep(Duration::from_millis(500));

    // Reset into bootloader
    on_progress(&FlashProgress::info("Reset", 5.0, "Resetting device into bootloader"));
    reset_to_bootloader(serial.as_mut())?;

    // Sync
    on_progress(&FlashProgress::info("Sync", 10.0, "Waiting for bootloader..."));
    sync_bootloader(serial.as_mut(), cancel)?;
    on_progress(&FlashProgress::info("Sync", 15.0, "Bootloader detected"));

    // Switch baud rate
    if target_baud != 115200 {
        on_progress(&FlashProgress::info(
            "Baud",
            20.0,
            &format!("Switching to {target_baud} bps"),
        ));
        set_baud_rate(serial.as_mut(), target_baud)?;

        // Re-sync at new baud rate
        serial_drain(serial.as_mut());
        sync_bootloader(serial.as_mut(), cancel)?;
        on_progress(&FlashProgress::info("Baud", 25.0, "Baud rate switched"));
    }

    Ok(serial)
}

/// Flash a .soc firmware to an XT804 device (Air6208/Air101).
///
/// This is the main entry point — handles everything from .soc extraction
/// through verification.
pub fn flash_xt804(
    soc_path: &str,
    port: &str,
    on_progress: ProgressCallback,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    // Extract .soc
    on_progress(&FlashProgress::info("Extract", 0.0, "Unpacking .soc file"));
    let tmpdir = tempfile::tempdir().context("Create temp dir")?;
    let unpacked = luatos_soc::unpack_soc(soc_path, tmpdir.path())?;
    let info = &unpacked.info;

    let flash_br = info.flash_baud_rate();
    on_progress(&FlashProgress::info(
        "Extract",
        5.0,
        &format!("Chip: {}, ROM: {}", info.chip.chip_type, info.rom.file),
    ));

    // Read firmware image
    let image_data = std::fs::read(&unpacked.rom_path)
        .with_context(|| format!("Cannot read ROM: {:?}", unpacked.rom_path))?;

    // Verify image header
    verify_image(&image_data)?;

    on_progress(&FlashProgress::info(
        "Extract",
        8.0,
        &format!("Image size: {} bytes", image_data.len()),
    ));

    // Connect to bootloader
    let mut serial = connect_bootloader(port, flash_br, &cancel, &on_progress)?;

    // Erase
    on_progress(&FlashProgress::info("Erase", 30.0, "Erasing flash..."));
    erase_flash(serial.as_mut(), &cancel)?;
    on_progress(&FlashProgress::info("Erase", 40.0, "Flash erased"));

    // Transfer via XMODEM
    on_progress(&FlashProgress::info("Write", 40.0, "Starting XMODEM transfer..."));
    xmodem_transfer(serial.as_mut(), &image_data, &cancel, &on_progress)?;
    on_progress(&FlashProgress::info("Write", 95.0, "Transfer complete"));

    // Reset device
    on_progress(&FlashProgress::info("Reset", 98.0, "Resetting device..."));
    reset_device(serial.as_mut())?;

    on_progress(&FlashProgress::done_ok("Flash completed successfully"));
    Ok(())
}

/// Flash a .soc firmware using the bundled air101_flash.exe subprocess.
///
/// Falls back to this when the SOC file includes the flasher executable.
pub fn flash_via_subprocess(
    soc_path: &str,
    port: &str,
    on_progress: ProgressCallback,
) -> Result<()> {
    on_progress(&FlashProgress::info("Extract", 0.0, "Unpacking .soc file"));
    let tmpdir = tempfile::tempdir().context("Create temp dir")?;
    let unpacked = luatos_soc::unpack_soc(soc_path, tmpdir.path())?;

    let flash_exe = unpacked
        .flash_exe
        .as_ref()
        .context("No flash executable found in .soc")?;

    on_progress(&FlashProgress::info(
        "Flash",
        10.0,
        &format!("Using: {:?}", flash_exe.file_name().unwrap_or_default()),
    ));

    // Build command: air101_flash.exe -ds <baud> -p <COM> -rs at -eo all -dl <fls_file>
    let baud = unpacked.info.flash_baud_rate().to_string();
    let output = std::process::Command::new(flash_exe)
        .arg("-ds")
        .arg(&baud)
        .arg("-p")
        .arg(port)
        .arg("-rs")
        .arg("at")
        .arg("-eo")
        .arg("all")
        .arg("-dl")
        .arg(unpacked.rom_path.to_string_lossy().as_ref())
        .current_dir(tmpdir.path())
        .output()
        .context("Failed to run flash tool")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if output.status.success() {
        on_progress(&FlashProgress::done_ok(&format!("Subprocess flash OK\n{stdout}")));
        Ok(())
    } else {
        bail!("Flash tool failed:\nstdout: {stdout}\nstderr: {stderr}");
    }
}

/// Flash only the script partition of an XT804 device.
///
/// This writes a pre-built script.img (LuaDB format) to the script address.
pub fn flash_script_only(
    soc_path: &str,
    port: &str,
    script_files: &[String],
    on_progress: ProgressCallback,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    // Parse SOC for addresses
    let info = luatos_soc::read_soc_info(soc_path)?;
    let _script_addr = info.script_addr();
    let flash_br = info.flash_baud_rate();

    on_progress(&FlashProgress::info(
        "Build",
        0.0,
        &format!("Building LuaDB for {} files", script_files.len()),
    ));

    // Build LuaDB entries from file paths
    let mut entries = Vec::new();
    for path_str in script_files {
        let path = std::path::Path::new(path_str);
        let filename = path
            .file_name()
            .context("Invalid file path")?
            .to_string_lossy()
            .to_string();
        let data = std::fs::read(path)
            .with_context(|| format!("Cannot read script file: {path_str}"))?;
        entries.push(luatos_luadb::LuadbEntry { filename, data });
    }
    let final_data = luatos_luadb::pack_luadb(&entries);

    on_progress(&FlashProgress::info(
        "Build",
        10.0,
        &format!("Script image: {} bytes", final_data.len()),
    ));

    // Connect to bootloader
    let mut serial = connect_bootloader(port, flash_br, &cancel, &on_progress)?;

    // For script-only flash, we need to erase first then transfer
    on_progress(&FlashProgress::info("Erase", 30.0, "Erasing script area..."));
    erase_flash(serial.as_mut(), &cancel)?;

    // Transfer
    on_progress(&FlashProgress::info("Write", 40.0, "Writing script..."));
    xmodem_transfer(serial.as_mut(), &final_data, &cancel, &on_progress)?;

    // Reset
    reset_device(serial.as_mut())?;
    on_progress(&FlashProgress::done_ok("Script flash completed"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crc16_ccitt() {
        // CRC16-CCITT (init=0, poly=0x1021): "123456789" → 0x31C3
        let data = b"123456789";
        let crc = crc16_ccitt(data);
        assert_eq!(crc, 0x31C3);
    }

    #[test]
    fn test_crc16_ccitt_empty() {
        assert_eq!(crc16_ccitt(&[]), 0x0000);
    }

    #[test]
    fn test_verify_image_valid() {
        let mut data = vec![0u8; 512];
        // Write magic at offset 0
        data[0..4].copy_from_slice(&IMAGE_MAGIC.to_le_bytes());
        assert!(verify_image(&data).is_ok());
    }

    #[test]
    fn test_verify_image_bad_magic() {
        let data = vec![0u8; 512];
        assert!(verify_image(&data).is_err());
    }

    #[test]
    fn test_verify_image_too_small() {
        let data = vec![0u8; 100];
        assert!(verify_image(&data).is_err());
    }

    #[test]
    fn test_baud_cmd_lookup() {
        assert_eq!(get_baud_cmd(2000000), &CMD_BAUD_2000000);
        assert_eq!(get_baud_cmd(921600), &CMD_BAUD_921600);
        assert_eq!(get_baud_cmd(115200), &CMD_BAUD_115200);
        assert_eq!(get_baud_cmd(9600), &CMD_BAUD_115200); // Unknown falls back
    }

    #[test]
    fn test_xmodem_frame_structure() {
        // Verify frame format: STX + blk# + ~blk# + 1024 data + CRC16 HI + CRC16 LO
        let data = [0x55u8; 1024];
        let crc = crc16_ccitt(&data);
        let block_num: u8 = 1;

        let mut frame = Vec::new();
        frame.push(XMODEM_STX);
        frame.push(block_num);
        frame.push(!block_num);
        frame.extend_from_slice(&data);
        frame.push((crc >> 8) as u8);
        frame.push((crc & 0xFF) as u8);

        assert_eq!(frame.len(), 3 + 1024 + 2);
        assert_eq!(frame[0], 0x02); // STX
        assert_eq!(frame[1], 1); // block number
        assert_eq!(frame[2], 254); // complement
    }
}
