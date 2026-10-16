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

// ─── ROM 命令集 ───────────────────────────────────────────────────────────────
//
// 厂商文档《2.6.1 命令列表》。所有命令共用同一个帧格式：
//
//     [0x21][len][crc16_lo][crc16_hi][cmd][data...]
//
//   - `len`   = 本帧除 `[0x21][len][0x00]` 之外的字节数 = 2(CRC) + 1(cmd) + len(data)
//   - `crc16` = CRC16-CCITT-FALSE (poly 0x1021, init 0xFFFF, 不反转)，
//               覆盖范围是 `[cmd] + data`，即 **不包含 CRC 自身**，小端存放
//   - `cmd`   = 命令号
//   - `data`  = 命令数据段
//
// 该规则已用文档给出的全部 13 条命令逐字节验证（见测试模块 `rom_命令帧与厂商文档一致`）。
// 因此这里一律用 `build_rom_cmd` 现算，不再硬编码字节 —— 手写常量曾把
// 921600/1000000 两条波特率命令的 CRC 抄错，设备会回 'R'（命令校验错）。

const CMD_SET_BAUD: u8 = 0x31;
const CMD_QFLASH_ERASE: u8 = 0x32;
const CMD_GET_MAC: u8 = 0x38;
const CMD_SYSTEM_RESET: u8 = 0x3F;

/// XT804 内部 flash 的基地址。擦除命令的 `index` 就是相对它的 4KB 扇区序号。
const XT804_FLASH_BASE: u32 = 0x0800_0000;

/// 擦除/保留区边界：厂商的「全擦」从扇区 2 开始，保留扇区 0..2（8KB 参数区）。
const XT804_PRESERVED_SECTORS: u32 = 2;

/// `index` 的 bit15 是擦除粒度标志：1 = Block 擦除，0 = Sector 擦除。
/// 扇区模式下可用位只剩 15 位，即最多 32767 个扇区（128MB），足够覆盖全部 XT804 容量。
const XT804_INDEX_BLOCK_FLAG: u16 = 0x8000;

/// 构造一条 ROM 命令帧。
fn build_rom_cmd(cmd: u8, data: &[u8]) -> Vec<u8> {
    let len = 2 + 1 + data.len();
    let mut crc_input = Vec::with_capacity(1 + data.len());
    crc_input.push(cmd);
    crc_input.extend_from_slice(data);
    let crc = crc16_ccitt_false(&crc_input);

    let mut frame = Vec::with_capacity(3 + len);
    frame.push(0x21);
    frame.push(len as u8);
    frame.push(0x00); // 保留字节
    frame.push((crc & 0xFF) as u8); // 小端
    frame.push((crc >> 8) as u8);
    frame.push(cmd);
    frame.extend_from_slice(data);
    frame
}

/// 构造 QFlash 擦除命令的数据段。
///
/// 数据段固定 7 字节：3 字节保留 + 4 字节 `struct { uint16_t index; uint16_t count; }`。
/// `index` 为起始扇区（bit15=0），`count` 为扇区个数。
fn qflash_erase_data(index: u16, count: u16) -> [u8; 7] {
    let mut d = [0u8; 7];
    d[3..5].copy_from_slice(&index.to_le_bytes());
    d[5..7].copy_from_slice(&count.to_le_bytes());
    d
}

// ─── CRC16-CCITT ──────────────────────────────────────────────────────────────

/// CRC16-CCITT-FALSE for XMODEM (polynomial 0x1021, init 0).
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

/// CRC16-CCITT-FALSE (polynomial 0x1021, **init 0xFFFF**, no reflection, no final xor)。
///
/// 这是 ROM 命令帧校验用的那一个 —— 与 XMODEM 数据块的 `crc16_ccitt`（init 0）不同，
/// 两者混用会导致设备回 'R'（命令校验错）。
fn crc16_ccitt_false(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
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
fn sync_bootloader(port: &mut dyn serialport::SerialPort, cancel: &AtomicBool) -> Result<()> {
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
fn baud_cmd(baud: u32) -> Vec<u8> {
    build_rom_cmd(
        CMD_SET_BAUD,
        &[
            0x00,
            0x00,
            0x00,
            (baud & 0xFF) as u8,
            ((baud >> 8) & 0xFF) as u8,
            ((baud >> 16) & 0xFF) as u8,
            (baud >> 24) as u8,
        ],
    )
}

/// Switch the baud rate. Sends command to device, waits, then changes host side.
fn set_baud_rate(port: &mut dyn serialport::SerialPort, baud: u32) -> Result<()> {
    if baud == 115200 {
        return Ok(()); // Already at default
    }

    // 支持列表之外的波特率也要拦在前面：厂商文档明确 >2000000 会回 'S'（命令参数错），
    // 插在 115200 与 2M 之间的值同样不是本芯片的合法分频。
    if !matches!(baud, 460800 | 921600 | 1000000 | 2000000) {
        bail!("xt804 不支持 {baud} 波特率，可用值：115200 / 460800 / 921600 / 1000000 / 2000000");
    }

    let cmd = baud_cmd(baud);
    port.write_all(&cmd)?;
    port.flush()?;

    // Wait for device to switch
    std::thread::sleep(Duration::from_secs(1));

    // Switch host side
    port.set_baud_rate(baud).context("Failed to change host baud rate")?;

    Ok(())
}

// ─── Erase ───────────────────────────────────────────────────────────────────

/// ROM 对擦除命令的应答：成功连发 3 个 'C'，失败连发 3 个错误码字符。
///
/// 错误码取自厂商文档《2.7 ROM 的错误码》：
/// `R` 命令校验错 / `S` 命令参数错 / `T` 取 FT 参数失败 / `N` FLASH ID 自检失败 / …
const ROM_ACK_OK: u8 = b'C';

/// 擦除若干个 4KB 扇区并等待 ROM 应答。
///
/// 厂商文档《2.6.1》：QFlash 擦除 `0x32`，数据段
/// `struct { uint16_t index; uint16_t count; }`，`index` 的 bit15 = 0 即 Sector 擦除。
///
/// 这是 ROM 原生命令，不传输数据，比「写全 0xFF」快一个数量级，且粒度精确。
fn erase_sectors(port: &mut dyn serialport::SerialPort, index: u32, count: u32, cancel: &AtomicBool, on_progress: &ProgressCallback) -> Result<()> {
    let total = index.checked_add(count).ok_or_else(|| anyhow::anyhow!("擦除范围溢出"))?;
    // bit15 是 Block/Sector 标志位，扇区模式下可表示的扇区数上限为 0x7FFF
    if total > XT804_INDEX_BLOCK_FLAG as u32 {
        bail!(
            "xt804 擦除范围过大：从扇区 {index} 起 {count} 个（末扇区 {total}），\
             超出扇区模式可表示的上限 0x{:04X}",
            XT804_INDEX_BLOCK_FLAG
        );
    }

    on_progress(&FlashProgress::info("RawErase", 20.0, &format!("擦除扇区 {index}..{total}（{count} × 4KB）")).with_region("raw"));

    let frame = build_rom_cmd(CMD_QFLASH_ERASE, &qflash_erase_data(index as u16, count as u16));
    port.write_all(&frame)?;
    port.flush()?;

    wait_rom_ack(port, cancel)
}

/// 等待 ROM 的 3 字节应答。
///
/// 擦除耗时随扇区数增长（整片 2MB 约需数秒到数十秒），这里给足余量并按
/// 数量线性放宽，避免大范围擦除被误判为超时。
fn wait_rom_ack(port: &mut dyn serialport::SerialPort, cancel: &AtomicBool) -> Result<()> {
    let timeout = Duration::from_secs(120);
    let start = Instant::now();
    let mut consecutive: Option<(u8, u32)> = None;

    let _ = port.set_timeout(Duration::from_millis(100));

    while start.elapsed() < timeout {
        if cancel.load(Ordering::Relaxed) {
            bail!("擦除已取消");
        }

        let mut buf = [0u8; 1];
        match port.read(&mut buf) {
            Ok(1) => {
                let ch = buf[0];
                if !ch.is_ascii_uppercase() {
                    consecutive = None;
                    continue;
                }
                match consecutive {
                    Some((prev, n)) if prev == ch => {
                        let n = n + 1;
                        consecutive = Some((ch, n));
                        if n >= 3 {
                            return if ch == ROM_ACK_OK {
                                Ok(())
                            } else {
                                bail!("xt804 ROM 拒绝擦除，应答错误码 '{}'（{}）", ch as char, rom_error_hint(ch))
                            };
                        }
                    }
                    _ => consecutive = Some((ch, 1)),
                }
            }
            _ => std::thread::sleep(Duration::from_millis(10)),
        }
    }

    bail!("擦除超时：{timeout:?} 内没有收到 ROM 应答")
}

/// 把 ROM 错误码翻译成可操作的提示。
fn rom_error_hint(code: u8) -> &'static str {
    match code {
        b'R' => "命令校验错（帧格式或 CRC 不对）",
        b'S' => "命令参数错（index/count 超出范围）",
        b'T' => "获取 FT 参数失败（Mac 和 Gain 等）",
        b'N' => "FLASH ID 自检失败",
        b'Q' => "固件类型错误",
        _ => "详见厂商文档《2.7 ROM 的错误码》",
    }
}

/// 整片擦除：从保留区之后一路擦到 flash 末尾。
///
/// 对应厂商文档的「QFlash 擦除(2M)」`21 0a 00 c3 35 32 00 00 00 02 00 fe 01`：
/// `index = 2`（保留扇区 0..2 的参数区）、`count = 0x01FE`。
/// 仅用于 `flash run` 的整片重刷，绝不用于 raw 的按区间擦除。
fn erase_flash(port: &mut dyn serialport::SerialPort, cancel: &AtomicBool, on_progress: &ProgressCallback) -> Result<()> {
    const ERASE_ALL_2M_SECTORS: u32 = 0x01FE;
    erase_sectors(port, XT804_PRESERVED_SECTORS, ERASE_ALL_2M_SECTORS, cancel, on_progress)
}

// ─── XMODEM-1K transfer ──────────────────────────────────────────────────────

/// Transfer firmware image using XMODEM-1K protocol.
///
/// Frame format: [STX, blk#, 255-blk#, 1024_data, CRC16_HI, CRC16_LO]
fn xmodem_transfer(port: &mut dyn serialport::SerialPort, data: &[u8], cancel: &AtomicBool, on_progress: &ProgressCallback, region: &str) -> Result<()> {
    let total_blocks = data.len().div_ceil(XMODEM_DATA_SIZE_1K);
    let mut block_num: u8 = 1; // XMODEM block numbers start at 1

    // 清空 RX 缓冲区中残留的 'C' 字节（设备在 XMODEM 就绪阶段持续发送）。
    // wm_tool.c 在 wm_tool_xmodem_download 入口处也执行相同操作（wm_tool_uart_clear）。
    // 若不清空，这些 'C' 会被误作 ACK/NAK 的响应读取，触发重传并产生连锁的脏 ACK，
    // 导致第 3 个及后续 block 的 XMODEM 状态混乱，设备发送 CAN 终止传输。
    serial_drain(port);

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
        on_progress(&FlashProgress::info("Write", pct, &format!("Block {}/{total_blocks}", i + 1)).with_region(region));
    }

    // Send EOT to end transfer.
    // The XT804 bootloader often reboots immediately after writing the image,
    // so the EOT ACK may never arrive. Use a short timeout and treat failure
    // as success since all data blocks were already ACKed.
    for _ in 0..3 {
        port.write_all(&[XMODEM_EOT])?;
        port.flush()?;

        let mut resp = [0u8; 1];
        let n = serial_read(port, &mut resp, Duration::from_secs(2));
        if n == 1 && resp[0] == XMODEM_ACK {
            return Ok(());
        }
    }

    // All data blocks were ACKed — EOT timeout is expected when the bootloader
    // reboots before acknowledging.
    log::info!("XMODEM EOT not ACKed (normal for XT804 partition flash)");
    Ok(())
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
    port.write_all(&build_rom_cmd(CMD_GET_MAC, &[0x00, 0x00, 0x00]))?;
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
    port.write_all(&build_rom_cmd(CMD_SYSTEM_RESET, &[0x00, 0x00, 0x00]))?;
    port.flush()?;
    std::thread::sleep(Duration::from_millis(500));
    Ok(())
}

// ─── Public API ──────────────────────────────────────────────────────────────

/// Open port, enter bootloader, and prepare for flash operations.
/// Returns the serial port handle at the target baud rate.
fn connect_bootloader(port: &str, target_baud: u32, cancel: &Arc<AtomicBool>, on_progress: &ProgressCallback) -> Result<Box<dyn serialport::SerialPort>> {
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
        on_progress(&FlashProgress::info("Baud", 20.0, &format!("Switching to {target_baud} bps")));
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
pub fn flash_xt804(soc_path: &str, port: &str, on_progress: ProgressCallback, cancel: Arc<AtomicBool>) -> Result<()> {
    // Extract .soc
    on_progress(&FlashProgress::info("Extract", 0.0, "Unpacking .soc file"));
    let tmpdir = tempfile::tempdir().context("Create temp dir")?;
    let unpacked = luatos_soc::unpack_soc(soc_path, tmpdir.path())?;
    let info = &unpacked.info;

    let flash_br = info.flash_baud_rate();
    on_progress(&FlashProgress::info("Extract", 5.0, &format!("Chip: {}, ROM: {}", info.chip.chip_type, info.rom.file)));

    // Read firmware image
    let image_data = std::fs::read(&unpacked.rom_path).with_context(|| format!("Cannot read ROM: {:?}", unpacked.rom_path))?;

    // Verify image header
    verify_image(&image_data)?;

    on_progress(&FlashProgress::info("Extract", 8.0, &format!("Image size: {} bytes", image_data.len())));

    // Connect to bootloader
    let mut serial = connect_bootloader(port, flash_br, &cancel, &on_progress)?;

    // Erase
    on_progress(&FlashProgress::info("Erase", 30.0, "Erasing flash..."));
    erase_flash(serial.as_mut(), &cancel, &on_progress)?;
    on_progress(&FlashProgress::info("Erase", 40.0, "Flash erased"));

    // Transfer via XMODEM
    on_progress(&FlashProgress::info("Write", 40.0, "Starting XMODEM transfer...").with_region("firmware"));
    xmodem_transfer(serial.as_mut(), &image_data, &cancel, &on_progress, "firmware")?;
    on_progress(&FlashProgress::info("Write", 95.0, "Transfer complete").with_region("firmware"));

    // Reset device
    on_progress(&FlashProgress::info("Reset", 98.0, "Resetting device..."));
    reset_device(serial.as_mut())?;

    on_progress(&FlashProgress::done_ok("Flash completed successfully"));
    Ok(())
}

/// Flash a .soc firmware using the bundled air101_flash.exe subprocess.
///
/// Falls back to this when the SOC file includes the flasher executable.
pub fn flash_via_subprocess(soc_path: &str, port: &str, on_progress: ProgressCallback) -> Result<()> {
    on_progress(&FlashProgress::info("Extract", 0.0, "Unpacking .soc file"));
    let tmpdir = tempfile::tempdir().context("Create temp dir")?;
    let unpacked = luatos_soc::unpack_soc(soc_path, tmpdir.path())?;

    let flash_exe = unpacked.flash_exe.as_ref().context("No flash executable found in .soc")?;

    on_progress(&FlashProgress::info("Flash", 10.0, &format!("Using: {:?}", flash_exe.file_name().unwrap_or_default())));

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
/// Builds a LuaDB image from the given script files, wraps it in an XT804
/// image header pointing at the script address, and flashes via XMODEM.
pub fn flash_script_only(soc_path: &str, port: &str, script_files: &[String], on_progress: ProgressCallback, cancel: Arc<AtomicBool>) -> Result<()> {
    flash_script_only_with_debug(soc_path, port, script_files, on_progress, cancel, None)
}

/// 仅刷脚本并可覆盖 Lua 调试级别；未指定时保持 SOC 配置。
pub fn flash_script_only_with_debug(
    soc_path: &str,
    port: &str,
    script_files: &[String],
    on_progress: ProgressCallback,
    cancel: Arc<AtomicBool>,
    luac_debug: Option<u8>,
) -> Result<()> {
    // Parse SOC for addresses and compilation settings
    let info = luatos_soc::read_soc_info(soc_path)?;
    let script_addr = info.script_addr();
    let flash_br = info.flash_baud_rate();
    let use_luac = info.script_use_luac();
    let bitw = info.script_bitw();
    let debug_mode = luac_debug.unwrap_or_else(|| info.script_debug_mode());

    on_progress(&FlashProgress::info("Build", 0.0, &format!("Building LuaDB for {} files", script_files.len())));

    // Build LuaDB entries from file paths, compiling .lua if use_luac
    let mut entries = Vec::new();
    for path_str in script_files {
        let path = std::path::Path::new(path_str);
        let filename = path.file_name().context("Invalid file path")?.to_string_lossy().to_string();
        let data = std::fs::read(path).with_context(|| format!("Cannot read script file: {path_str}"))?;

        let (final_name, final_data) = if use_luac && filename.ends_with(".lua") && !filename.ends_with(".luac") {
            let chunk_name = format!("@{}", filename);
            let bytecode = luatos_luadb::build::compile_lua_bytes(&data, &chunk_name, debug_mode, bitw).with_context(|| format!("Failed to compile {path_str}"))?;
            let luac_name = format!("{}c", filename);
            log::info!("compiled {} (bitw={}, debug_mode={})", filename, bitw, debug_mode);
            (luac_name, bytecode)
        } else {
            (filename, data)
        };

        entries.push(luatos_luadb::LuadbEntry {
            filename: final_name,
            data: final_data,
        });
    }
    let luadb_data = luatos_luadb::pack_luadb(&entries)?;

    on_progress(&FlashProgress::info(
        "Build",
        10.0,
        &format!("Script image: {} bytes → addr 0x{:X}", luadb_data.len(), script_addr),
    ));

    // Wrap in XT804 image header (partition config: SRAM header, img_type=1)
    let final_data = build_xt804_image(&luadb_data, script_addr, &partition_image_config());

    // Connect to bootloader
    let mut serial = connect_bootloader(port, flash_br, &cancel, &on_progress)?;

    // No erase for script-only flash — only the target partition is overwritten.
    // The XT804 bootloader uses the image header's run_img_addr to determine
    // where to write, so erasing the entire flash is unnecessary and destructive.
    on_progress(&FlashProgress::info("Write", 40.0, "Writing script...").with_region("script"));
    xmodem_transfer(serial.as_mut(), &final_data, &cancel, &on_progress, "script")?;

    // Reset
    reset_device(serial.as_mut())?;
    on_progress(&FlashProgress::done_ok("Script flash completed"));
    Ok(())
}

/// Flash the LittleFS filesystem partition of an XT804 device.
///
/// Builds a LittleFS image from files in `fs_dirs`, wraps in an XT804 image
/// header, and flashes to the filesystem partition address from the SOC info.
pub fn flash_filesystem(soc_path: &str, port: &str, fs_dirs: &[String], on_progress: ProgressCallback, cancel: Arc<AtomicBool>) -> Result<()> {
    let info = luatos_soc::read_soc_info(soc_path)?;
    let (fs_addr, fs_size) = info.filesystem_partition().context("SOC info has no filesystem partition defined")?;
    let flash_br = info.flash_baud_rate();

    on_progress(&FlashProgress::info("Build", 0.0, &format!("Building LittleFS image ({} KB)", fs_size / 1024)));

    // Prepare a temp dir with all files from all fs_dirs
    let tmpdir = tempfile::tempdir().context("failed to create temp directory")?;
    for dir_str in fs_dirs {
        let src = std::path::Path::new(dir_str);
        if !src.is_dir() {
            bail!("Not a directory: {}", dir_str);
        }
        copy_dir_contents(src, tmpdir.path()).with_context(|| format!("failed to copy files from {}", dir_str))?;
    }

    let fs_image = luatos_luadb::build::build_littlefs_image(tmpdir.path(), fs_size, 4096).context("failed to build LittleFS image")?;

    on_progress(&FlashProgress::info("Build", 10.0, &format!("LFS image: {} bytes → addr 0x{:X}", fs_image.len(), fs_addr)));

    // Wrap in XT804 image header (partition config)
    let final_data = build_xt804_image(&fs_image, fs_addr, &partition_image_config());

    // Connect to bootloader
    let mut serial = connect_bootloader(port, flash_br, &cancel, &on_progress)?;

    on_progress(&FlashProgress::info("Write", 40.0, "Writing filesystem...").with_region("fs"));
    xmodem_transfer(serial.as_mut(), &final_data, &cancel, &on_progress, "fs")?;

    reset_device(serial.as_mut())?;
    on_progress(&FlashProgress::done_ok("Filesystem flash completed"));
    Ok(())
}

/// Clear (erase) the filesystem partition of an XT804 device.
///
/// Writes an all-0xFF image to the filesystem partition address.
pub fn clear_filesystem(soc_path: &str, port: &str, on_progress: ProgressCallback, cancel: Arc<AtomicBool>) -> Result<()> {
    let info = luatos_soc::read_soc_info(soc_path)?;
    let (fs_addr, fs_size) = info.filesystem_partition().context("SOC info has no filesystem partition defined")?;
    let flash_br = info.flash_baud_rate();

    on_progress(&FlashProgress::info("Build", 0.0, &format!("Preparing blank FS image ({} KB)", fs_size / 1024)));

    let blank = vec![0xFF_u8; fs_size];
    let final_data = build_xt804_image(&blank, fs_addr, &partition_image_config());

    let mut serial = connect_bootloader(port, flash_br, &cancel, &on_progress)?;

    on_progress(&FlashProgress::info("Write", 40.0, "Clearing filesystem...").with_region("fs"));
    xmodem_transfer(serial.as_mut(), &final_data, &cancel, &on_progress, "fs")?;

    reset_device(serial.as_mut())?;
    on_progress(&FlashProgress::done_ok("Filesystem cleared"));
    Ok(())
}

/// Clear (erase) the FSKV (key-value store) partition of an XT804 device.
pub fn clear_kv(soc_path: &str, port: &str, on_progress: ProgressCallback, cancel: Arc<AtomicBool>) -> Result<()> {
    let info = luatos_soc::read_soc_info(soc_path)?;
    let (kv_addr, kv_size) = info.kv_partition().context("SOC info has no KV partition defined")?;
    let flash_br = info.flash_baud_rate();

    on_progress(&FlashProgress::info("Build", 0.0, &format!("Preparing blank KV image ({} KB)", kv_size / 1024)));

    let blank = vec![0xFF_u8; kv_size];
    let final_data = build_xt804_image(&blank, kv_addr, &partition_image_config());

    let mut serial = connect_bootloader(port, flash_br, &cancel, &on_progress)?;

    on_progress(&FlashProgress::info("Write", 40.0, "Clearing KV store...").with_region("kv"));
    xmodem_transfer(serial.as_mut(), &final_data, &cancel, &on_progress, "kv")?;

    reset_device(serial.as_mut())?;
    on_progress(&FlashProgress::done_ok("KV store cleared"));
    Ok(())
}

// ─── XT804 image header ──────────────────────────────────────────────────────

/// CRC32 used by the XT804 bootloader (same polynomial as wm_tool.c).
fn xt804_crc32(data: &[u8]) -> u32 {
    // wm_tool.c uses standard CRC32 (same as Ethernet/zlib)
    let mut crc: u32 = 0xFFFFFFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB88320;
            } else {
                crc >>= 1;
            }
        }
    }
    crc
}

/// XT804 image header configuration.
struct Xt804ImageConfig {
    /// img_type / img_attr field (0 = secboot, 1 = user image)
    img_type: u16,
    /// Where the header is stored: flash addr for firmware, SRAM 0x20008000 for partitions
    img_header_addr: u32,
    /// Upgrade image address (0x08010000 for firmware, 0 for partitions)
    upgrade_img_addr: u32,
    /// Next boot header address (for chained images, 0 for last/only image)
    next_boot: u32,
}

/// Build an XT804 firmware image with proper header.
///
/// The header tells the bootloader where to write the payload in flash.
/// Payload is padded to 1024-byte alignment (matching wm_tool.c behavior).
///
/// Layout (64 bytes, LE):
///   0x00: magic_no         (u32) = 0xA0FFFF9F
///   0x04: img_type         (u16)
///   0x06: zip_type         (u16) = 0 (uncompressed)
///   0x08: run_img_addr     (u32) = target flash address
///   0x0C: run_img_len      (u32) = payload length (padded)
///   0x10: img_header_addr  (u32)
///   0x14: upgrade_img_addr (u32)
///   0x18: run_org_checksum (u32) = CRC32 of payload (padded)
///   0x1C: upd_no           (u32) = 0
///   0x20: ver[16]          (bytes) = version string
///   0x30: reserved0        (u32) = 0
///   0x34: reserved1        (u32) = 0
///   0x38: next_boot        (u32)
///   0x3C: hd_checksum      (u32) = CRC32 of header[0..60]
fn build_xt804_image(payload: &[u8], run_addr: u32, cfg: &Xt804ImageConfig) -> Vec<u8> {
    // Pad payload to 1024-byte alignment with 0x00 (minimum 1KB).
    // wm_tool.c uses memset(buf, 0, ...) before reading, so padding is 0x00.
    let padded_len = ((payload.len() + 1023) & !1023).max(1024);
    let mut padded_payload = vec![0u8; padded_len];
    padded_payload[..payload.len()].copy_from_slice(payload);

    let mut header = [0u8; 64];

    // magic
    header[0..4].copy_from_slice(&IMAGE_MAGIC.to_le_bytes());
    // img_type
    header[4..6].copy_from_slice(&cfg.img_type.to_le_bytes());
    // zip_type = 0 (uncompressed)
    // run_img_addr
    header[8..12].copy_from_slice(&run_addr.to_le_bytes());
    // run_img_len (padded length)
    header[12..16].copy_from_slice(&(padded_len as u32).to_le_bytes());
    // img_header_addr
    header[16..20].copy_from_slice(&cfg.img_header_addr.to_le_bytes());
    // upgrade_img_addr
    header[20..24].copy_from_slice(&cfg.upgrade_img_addr.to_le_bytes());
    // run_org_checksum = CRC32 of padded payload
    let payload_crc = xt804_crc32(&padded_payload);
    header[24..28].copy_from_slice(&payload_crc.to_le_bytes());
    // ver[16] left as zeros (matching wm_tool default)
    // next_boot
    header[56..60].copy_from_slice(&cfg.next_boot.to_le_bytes());
    // hd_checksum = CRC32 of header[0..60]
    let hd_crc = xt804_crc32(&header[0..60]);
    header[60..64].copy_from_slice(&hd_crc.to_le_bytes());

    let mut image = Vec::with_capacity(64 + padded_len);
    image.extend_from_slice(&header);
    image.extend_from_slice(&padded_payload);
    image
}

/// Config for partition images (script, filesystem, KV).
/// Uses SRAM header addr so the header is not persisted to flash.
fn partition_image_config() -> Xt804ImageConfig {
    Xt804ImageConfig {
        img_type: 1,
        img_header_addr: 0x20008000,
        upgrade_img_addr: 0,
        next_boot: 0,
    }
}

/// Config for full firmware images (secboot + app chain).
#[allow(dead_code)]
fn firmware_image_config() -> Xt804ImageConfig {
    Xt804ImageConfig {
        img_type: 0,
        img_header_addr: 0x08002000,
        upgrade_img_addr: 0x08010000,
        next_boot: 0,
    }
}

// ─── Raw flash (任意地址) ────────────────────────────────────────────────────

/// XT804 上的 raw flash 会话。
///
/// 机制与 BK7258/CCM4211 完全不同：XT804 没有「写 N 字节到地址 M」的原语，
/// 只有 XMODEM-1K 整镜像传输。写入位置由我们发送的 XT804 镜像头里的
/// `run_img_addr` 字段决定（官方 `wm_tool` 以 `-ra` 参数暴露同一机制）。
///
/// 因此 `write(addr, data)` = 把 data 包成镜像头 + XMODEM 传过去。
/// 分区镜像用 `partition_image_config()`（`img_header_addr = 0x20008000` 落在
/// SRAM），头部不写 flash，不消耗 header 槽位，行为与 `flash script` 一致。
///
/// `erase(addr, len)` 则走 ROM 原生的 QFlash 擦除命令 `0x32`（Sector 模式），
/// 按 4KB 扇区精确擦除，不需要传输任何数据。
pub struct Xt804Raw {
    port: Box<dyn serialport::SerialPort>,
    cancel: Arc<AtomicBool>,
    on_progress: ProgressCallback,
}

/// 进入 XT804 bootloader 并返回 raw 会话。
pub fn open_raw(port: &str, flash_br: u32, cancel: Arc<AtomicBool>, on_progress: ProgressCallback) -> Result<Box<dyn crate::raw::RawFlashDevice>> {
    cancel.store(false, Ordering::Relaxed);
    let serial = connect_bootloader(port, flash_br, &cancel, &on_progress)?;
    Ok(Box::new(Xt804Raw {
        port: serial,
        cancel,
        on_progress,
    }))
}

impl crate::raw::RawFlashDevice for Xt804Raw {
    fn chip(&self) -> crate::raw::RawChip {
        crate::raw::RawChip::Xt804
    }

    /// XT804 flash 的扇区大小 4KB（`INSIDE_FLS_SECTOR_SIZE`）。
    fn erase_granularity(&self) -> u64 {
        crate::raw::RawChip::Xt804.erase_granularity().unwrap_or(0)
    }

    /// XMODEM-1K 按 1024 字节块传输。
    fn write_alignment(&self) -> u64 {
        crate::raw::RawChip::Xt804.write_alignment()
    }

    fn progress(&self) -> &ProgressCallback {
        &self.on_progress
    }

    fn write(&mut self, addr: u32, data: &[u8]) -> Result<()> {
        // 镜像头的 run_img_addr 决定 bootloader 把负载写到哪
        let image = build_xt804_image(data, addr, &partition_image_config());
        (self.on_progress)(&FlashProgress::info("RawWrite", 40.0, &format!("XMODEM 写入 0x{addr:08X}，{} 字节", data.len())).with_region("raw"));
        // bootloader 依据 run_img_addr 自行处理目标区域：
        // secboot 的 flash_write() 对每个受影响的 4K 扇区先擦后写（读-改-写），
        // 因此写入本身已隐含扇区擦除，无需额外下发擦除命令。
        xmodem_transfer(&mut *self.port, &image, &self.cancel, &self.on_progress, "raw")
    }

    /// 按 4K 扇区擦除区间 —— 厂商 ROM 的 QFlash 擦除命令 `0x32`（Sector 模式）。
    ///
    /// 这是 XT804 的**原生擦除原语**：数据段 `struct { uint16_t index; uint16_t count; }`，
    /// `index` 的 bit15 = 0 表示按 Sector（4KB）擦除，`count` 是扇区个数，
    /// `index` 是相对 `0x08000000` 的扇区序号。
    ///
    /// 注意与 `erase_flash()` 的区别：后者是厂商的「QFlash 擦除(2M)」全擦
    /// （`index=2, count=0x01FE`），保留扇区 0..2 的参数区但抹掉其余全部内容，
    /// 仅供 `flash run` 整片重刷使用，raw 路径绝不调用。
    fn erase(&mut self, addr: u32, len: u64) -> Result<()> {
        let sector = crate::raw::RawChip::Xt804.erase_granularity().unwrap_or(0x1000);
        if sector == 0 {
            bail!("xt804 擦除粒度未知，拒绝擦除");
        }
        // 命令只接受扇区粒度，起点必须已对齐（共享驱动已用 check_alignment 预检，
        // 这里再兜一次，避免绕过护栏直接调用时擦到相邻扇区）。
        anyhow::ensure!((addr as u64).is_multiple_of(sector), "xt804 擦除地址必须按 {sector} 字节对齐，当前 0x{addr:08x}");
        anyhow::ensure!(len > 0, "擦除长度必须大于 0");
        anyhow::ensure!(len.is_multiple_of(sector), "xt804 擦除长度必须是 {sector} 的整数倍，当前 {len}");

        anyhow::ensure!(addr >= XT804_FLASH_BASE, "xt804 flash 基地址为 0x{XT804_FLASH_BASE:08x}，0x{addr:08x} 在其之下");
        let start_sector = (addr - XT804_FLASH_BASE) / sector as u32;
        let count = (len / sector) as u32;

        erase_sectors(&mut *self.port, start_sector, count, &self.cancel, &self.on_progress)
    }

    fn verify(&mut self, _addr: u32, _data: &[u8]) -> Result<crate::raw::VerifyResult> {
        // bootloader 命令集只有：切波特率、读 MAC、复位、整片擦除、XMODEM 下载。
        // 没有读回 flash 或查询 CRC 的命令，无法判定写入结果。
        bail!("xt804 bootloader 未提供 flash 读回或校验命令，无法校验写入结果")
    }

    fn read(&mut self, _addr: u32, _len: u64) -> Result<Vec<u8>> {
        bail!("xt804 bootloader 未提供 flash 读回命令，无法 dump")
    }

    fn reset(&mut self) {
        let _ = reset_device(&mut *self.port);
    }
}

/// Copy all files/dirs from src to dst (non-recursive merge).
fn copy_dir_contents(src: &std::path::Path, dst: &std::path::Path) -> Result<()> {
    for entry in walkdir::WalkDir::new(src).min_depth(1) {
        let entry = entry?;
        let rel = entry.path().strip_prefix(src).context("strip_prefix failed")?;
        let target = dst.join(rel);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&target)?;
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_debug_override_changes_image_before_serial_connection() {
        let dir = tempfile::tempdir().unwrap();
        let soc_dir = dir.path().join("soc");
        std::fs::create_dir(&soc_dir).unwrap();
        let info = serde_json::json!({
            "chip": {"type": "air6208"},
            "rom": {"file": "firmware.fls"},
            "script": {"file": "script.bin", "bitw": 64, "use-luac": true, "use-debug": true},
            "download": {"script_addr": "08430000"}
        });
        std::fs::write(soc_dir.join("info.json"), info.to_string()).unwrap();
        let soc = dir.path().join("firmware.soc");
        luatos_soc::pack_soc_zip(&soc_dir, soc.to_str().unwrap()).unwrap();
        let script = dir.path().join("main.lua");
        std::fs::write(&script, "local named_debug_variable = 42\nreturn named_debug_variable\n").unwrap();
        let mut sizes = Vec::new();
        for mode in [0, 99] {
            let observed = Arc::new(std::sync::Mutex::new(None));
            let observed_cb = observed.clone();
            let on_progress: ProgressCallback = Box::new(move |p| {
                if let Some(message) = p.message.strip_prefix("Script image: ") {
                    *observed_cb.lock().unwrap() = Some(message.split(' ').next().unwrap().parse::<usize>().unwrap());
                }
            });
            let error = flash_script_only_with_debug(
                soc.to_str().unwrap(),
                "PR_TEST_NO_SERIAL_PORT",
                &[script.to_str().unwrap().to_string()],
                on_progress,
                Arc::new(AtomicBool::new(false)),
                Some(mode),
            )
            .unwrap_err();
            assert!(error.to_string().contains("PR_TEST_NO_SERIAL_PORT"), "{error:#}");
            sizes.push(observed.lock().unwrap().expect("连接串口前应完成脚本编译"));
        }
        assert!(sizes[0] < sizes[1], "XT804 必须应用调试级别覆盖: {sizes:?}");
    }

    // 厂商命令表里尚未接入的只读命令，仅用于验证帧构造器对整张表都成立。
    const CMD_GET_FLASH_ID: u8 = 0x3C;
    const CMD_GET_LAST_ERROR: u8 = 0x3B;
    const CMD_GET_ROM_VERSION: u8 = 0x3E;

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

    /// 帧构造器必须逐字节复现厂商文档《2.6.2 常用指令集合》里的全部 13 条命令。
    ///
    /// 这是整个 ROM 命令实现的信任根：帧格式、CRC 范围、字节序任何一处理解错了，
    /// 设备都会静默回 'R'（命令校验错）。硬编码字节曾把 921600/1000000 两条
    /// 波特率命令的 CRC 抄错，所以这里改成现算 + 逐条比对。
    #[test]
    fn rom_命令帧与厂商文档一致() {
        let cases: &[(&str, Vec<u8>)] = &[
            ("波特率 2M", build_rom_cmd(CMD_SET_BAUD, &[0x00, 0x00, 0x00, 0x80, 0x84, 0x1e, 0x00])),
            ("波特率 1M", build_rom_cmd(CMD_SET_BAUD, &[0x00, 0x00, 0x00, 0x40, 0x42, 0x0f, 0x00])),
            ("波特率 921600", build_rom_cmd(CMD_SET_BAUD, &[0x00, 0x00, 0x00, 0x00, 0x10, 0x0e, 0x00])),
            ("波特率 460800", build_rom_cmd(CMD_SET_BAUD, &[0x00, 0x00, 0x00, 0x00, 0x08, 0x07, 0x00])),
            ("波特率 115200", build_rom_cmd(CMD_SET_BAUD, &[0x00, 0x00, 0x00, 0x00, 0xc2, 0x01, 0x00])),
            ("QFlash 擦除(1M)", build_rom_cmd(CMD_QFLASH_ERASE, &qflash_erase_data(2, 0x00FE))),
            ("QFlash 擦除(2M)", build_rom_cmd(CMD_QFLASH_ERASE, &qflash_erase_data(2, 0x01FE))),
            ("获取 BT MAC", build_rom_cmd(0x34, &[0x00, 0x00, 0x00])),
            ("获取 MAC", build_rom_cmd(CMD_GET_MAC, &[0x00, 0x00, 0x00])),
            ("获取上一个错误", build_rom_cmd(CMD_GET_LAST_ERROR, &[0x00, 0x00, 0x00])),
            ("获取 QFlash ID", build_rom_cmd(CMD_GET_FLASH_ID, &[0x00, 0x00, 0x00])),
            ("获取 ROM 版本", build_rom_cmd(CMD_GET_ROM_VERSION, &[0x00, 0x00, 0x00])),
            ("系统重启", build_rom_cmd(CMD_SYSTEM_RESET, &[0x00, 0x00, 0x00])),
        ];
        let expected: &[&[u8]] = &[
            &[0x21, 0x0a, 0x00, 0xef, 0x2a, 0x31, 0x00, 0x00, 0x00, 0x80, 0x84, 0x1e, 0x00],
            &[0x21, 0x0a, 0x00, 0x5e, 0x3d, 0x31, 0x00, 0x00, 0x00, 0x40, 0x42, 0x0f, 0x00],
            &[0x21, 0x0a, 0x00, 0x5d, 0x50, 0x31, 0x00, 0x00, 0x00, 0x00, 0x10, 0x0e, 0x00],
            &[0x21, 0x0a, 0x00, 0x07, 0x00, 0x31, 0x00, 0x00, 0x00, 0x00, 0x08, 0x07, 0x00],
            &[0x21, 0x0a, 0x00, 0x97, 0x4b, 0x31, 0x00, 0x00, 0x00, 0x00, 0xc2, 0x01, 0x00],
            &[0x21, 0x0a, 0x00, 0xe2, 0x25, 0x32, 0x00, 0x00, 0x00, 0x02, 0x00, 0xfe, 0x00],
            &[0x21, 0x0a, 0x00, 0xc3, 0x35, 0x32, 0x00, 0x00, 0x00, 0x02, 0x00, 0xfe, 0x01],
            &[0x21, 0x06, 0x00, 0xd8, 0x62, 0x34, 0x00, 0x00, 0x00],
            &[0x21, 0x06, 0x00, 0xea, 0x2d, 0x38, 0x00, 0x00, 0x00],
            &[0x21, 0x06, 0x00, 0x36, 0xb6, 0x3b, 0x00, 0x00, 0x00],
            &[0x21, 0x06, 0x00, 0x1b, 0xe7, 0x3c, 0x00, 0x00, 0x00],
            &[0x21, 0x06, 0x00, 0x73, 0x0a, 0x3e, 0x00, 0x00, 0x00],
            &[0x21, 0x06, 0x00, 0xc7, 0x7c, 0x3f, 0x00, 0x00, 0x00],
        ];
        assert_eq!(cases.len(), expected.len());
        for ((name, got), want) in cases.iter().zip(expected.iter()) {
            assert_eq!(got.as_slice(), *want, "{name} 帧与厂商文档不一致");
        }
    }

    /// 波特率分频值是大端无关的 u32 LE，且取值受厂商列表约束。
    #[test]
    fn 波特率命令按厂商列表构造() {
        for baud in [115200u32, 460800, 921600, 1000000, 2000000] {
            let f = baud_cmd(baud);
            assert_eq!(f[0], 0x21);
            assert_eq!(f[2], 0x00, "len 之后是保留字节");
            assert_eq!(f[5], CMD_SET_BAUD);
            // 数据段：3 字节保留 + 4 字节 u32 LE == 波特率
            assert_eq!(&f[6..9], &[0x00, 0x00, 0x00]);
            assert_eq!(u32::from_le_bytes([f[9], f[10], f[11], f[12]]), baud, "baud={baud}");
            assert_eq!(f[1], 0x0a, "len 应为 10");
        }
    }

    /// 擦除数据段必须把 index/count 按小端放进末 4 字节，前 3 字节保留为 0。
    #[test]
    fn 擦除数据段为_index_count_小端结构() {
        assert_eq!(qflash_erase_data(2, 0x01FE), [0x00, 0x00, 0x00, 0x02, 0x00, 0xfe, 0x01]);
        // index = 0x1234 时 bit15 为 0，属于 Sector 擦除
        assert_eq!(qflash_erase_data(0x1234, 3), [0x00, 0x00, 0x00, 0x34, 0x12, 0x03, 0x00]);
    }

    /// bit15 = 0 → Sector 擦除，这是 raw 按 4K 擦除依赖的前提。
    #[test]
    fn 擦除数据段的_block_标志位默认清零() {
        let d = qflash_erase_data(0x7FFF, 1);
        assert_eq!(d[4] & 0x80, 0, "扇区模式下 index 的 bit15 必须为 0");
    }

    #[test]
    fn xmodem_frame_structure() {
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

    // ─── 真机探针（需硬件，CI 不执行）──────────────────────────────────────────

    /// 只读探针：确认 ROM 命令帧（含 CRC16-CCITT-FALSE）在真机上被接受。
    ///
    /// 这几条命令都不改变 flash 内容，可以安全地反复跑。用法：
    ///   set HW_PORT=COM12 && cargo test -p luatos-flash hw_probe_rom -- --ignored --nocapture
    #[test]
    #[ignore = "需要 XT804 真机"]
    fn hw_probe_rom() {
        let port_name = std::env::var("HW_PORT").expect("set HW_PORT=COMxx");
        let cancel = Arc::new(AtomicBool::new(false));
        let on_progress: ProgressCallback = Box::new(|p| println!("  [{}] {}", p.stage, p.message));

        let mut serial = connect_bootloader(&port_name, 115200, &cancel, &on_progress).expect("进入 ROM 下载模式失败");

        for (label, cmd) in [
            ("获取 QFlash ID 与容量 0x3C", 0x3C),
            ("获取 ROM 版本 0x3E", 0x3E),
            ("获取上一个错误 0x3B", 0x3B),
            ("获取 WiFi MAC 0x38", 0x38),
        ] {
            let frame = build_rom_cmd(cmd, &[0x00, 0x00, 0x00]);
            println!("\n[{label}]");
            println!("  tx: {}", frame.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" "));
            serial_drain(serial.as_mut());
            serial.write_all(&frame).expect("write");
            serial.flush().ok();

            let mut buf = [0u8; 128];
            let n = serial_read(serial.as_mut(), &mut buf, Duration::from_secs(3));
            let txt = String::from_utf8_lossy(&buf[..n]);
            println!("  rx: {:?}", txt.trim_end());
            if txt.trim().is_empty() {
                println!("  !! 无应答 —— 该命令号可能不被当前 ROM 版本支持");
            }
        }

        // MAC 单独走解析路径，确认格式
        serial_drain(serial.as_mut());
        match read_mac(serial.as_mut()) {
            Ok(mac) => println!("\nMAC 解析成功: {mac}"),
            Err(e) => println!("\nMAC 解析失败: {e}"),
        }

        reset_device(serial.as_mut()).ok();
    }

    /// 只读探针：确认波特率切换命令的 CRC 修正确实生效。
    ///
    /// 修复前 921600 / 1000000 两档的 CRC 字节是错的，设备会回 'R'（命令校验错）、
    /// 随后 XMODEM 同步失败。修复后应能正常切换并重新同步。
    #[test]
    #[ignore = "需要 XT804 真机"]
    fn hw_probe_baud_crc() {
        let port_name = std::env::var("HW_PORT").expect("set HW_PORT=COMxx");
        let cancel = Arc::new(AtomicBool::new(false));
        let on_progress: ProgressCallback = Box::new(|p| println!("  [{}] {}", p.stage, p.message));

        for baud in [460800u32, 921600, 1000000, 2000000] {
            println!("\n=== 测试 {baud} bps ===");
            // 注意：串口只能由 connect_bootloader 打开一次，这里不能再额外 open，
            // 否则会撞上「端口被占用」。
            match connect_bootloader(&port_name, 115200, &cancel, &on_progress) {
                Ok(mut s) => {
                    set_baud_rate(s.as_mut(), baud).expect("set_baud_rate");
                    serial_drain(s.as_mut());
                    match sync_bootloader(s.as_mut(), &cancel) {
                        Ok(()) => println!("  => {baud} bps 切换并重新同步成功"),
                        Err(e) => println!("  => {baud} bps 重新同步失败: {e}"),
                    }
                    reset_device(s.as_mut()).ok();
                }
                Err(e) => println!("  => 进入 bootloader 失败: {e}"),
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }
}
