// Air6201 外置 SPI Flash 烧录协议
//
// 通过 UART 串口与 Air6201 上的 LuatOS 固件通信，操作外置 P25Q32 Flash。
//
// 协议帧格式:
//   请求: [SOF:0xAA] [CMD] [LEN_H] [LEN_L] [DATA...] [EOF:0x55]
//   响应: [SOF:0xAA] [CMD] [STATUS] [LEN_H] [LEN_L] [DATA...] [EOF:0x55]
//
// 分区定义:
//   Script (512KB) @ 0x000000
//   FSKV   (64KB)  @ 0x080000
//   LFS    (2MB)   @ 0x090000

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serialport::SerialPort;

use crate::{FlashProgress, ProgressCallback};

// ─── 协议常量 ────────────────────────────────────────────────────────────────

const SOF: u8 = 0xAA;
const EOF: u8 = 0x55;

// 命令码
const CMD_SYNC: u8 = 0x00;
const CMD_GET_INFO: u8 = 0x01;
const CMD_READ_ID: u8 = 0x02;
const CMD_ERASE_PARTITION: u8 = 0x03;
#[allow(dead_code)]
const CMD_ERASE_RANGE: u8 = 0x04;
const CMD_WRITE: u8 = 0x05;
#[allow(dead_code)]
const CMD_READ: u8 = 0x06;
const CMD_VERIFY: u8 = 0x07;
#[allow(dead_code)]
const CMD_GET_STATUS: u8 = 0x08;
#[allow(dead_code)]
const CMD_DATA: u8 = 0x09;
const CMD_RESET: u8 = 0x0F;

// 响应状态码
const RESP_OK: u8 = 0x00;
#[allow(dead_code)]
const RESP_ERROR: u8 = 0x01;
#[allow(dead_code)]
const RESP_VERIFY_FAIL: u8 = 0x06;

// 写入错误标记
const WRITE_ERR_MARKER: u8 = 0xEE;

// 数据传输参数
const CHUNK_SIZE: usize = 4096;
const INTER_CHUNK_DELAY_MS: u64 = 20;

// ─── 分区定义 ────────────────────────────────────────────────────────────────

/// Air6201 外置 Flash 分区
#[derive(Debug, Clone, Copy)]
pub struct Partition {
    pub id: u8,
    pub addr: u32,
    pub size: u32,
    pub name: &'static str,
}

pub const PART_SCRIPT: Partition = Partition {
    id: 0,
    addr: 0x000000,
    size: 512 * 1024,
    name: "Script (512KB)",
};

pub const PART_FSKV: Partition = Partition {
    id: 1,
    addr: 0x080000,
    size: 64 * 1024,
    name: "FSKV (64KB)",
};

pub const PART_LFS: Partition = Partition {
    id: 2,
    addr: 0x090000,
    size: 2048 * 1024,
    name: "LFS (2MB)",
};

/// 根据名称查找分区
pub fn partition_by_name(name: &str) -> Option<Partition> {
    match name.to_lowercase().as_str() {
        "script" => Some(PART_SCRIPT),
        "fskv" => Some(PART_FSKV),
        "lfs" => Some(PART_LFS),
        _ => None,
    }
}

// ─── 协议底层收发 ────────────────────────────────────────────────────────────

/// 发送命令帧
fn send_command(port: &mut dyn SerialPort, cmd: u8, data: &[u8]) -> Result<()> {
    let data_len = data.len();
    let mut packet = Vec::with_capacity(5 + data_len);
    packet.push(SOF);
    packet.push(cmd);
    packet.push((data_len >> 8) as u8);
    packet.push((data_len & 0xFF) as u8);
    packet.extend_from_slice(data);
    packet.push(EOF);

    // 分块发送，每次最多 4096 字节
    for chunk in packet.chunks(4096) {
        port.write_all(chunk)?;
    }
    port.flush()?;
    Ok(())
}

/// 响应数据
struct Response {
    #[allow(dead_code)]
    cmd: u8,
    status: u8,
    data: Vec<u8>,
}

/// 接收响应帧，带超时
fn recv_response(port: &mut dyn SerialPort, timeout: Duration) -> Result<Option<Response>> {
    let deadline = Instant::now() + timeout;
    let mut byte = [0u8; 1];

    // 等待 SOF
    loop {
        if Instant::now() > deadline {
            return Ok(None);
        }
        match port.read(&mut byte) {
            Ok(1) if byte[0] == SOF => break,
            Ok(_) => continue,
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(e) => return Err(e.into()),
        }
    }

    // 读取头部: CMD + STATUS + LEN_H + LEN_L
    let mut header = [0u8; 4];
    read_exact_deadline(port, &mut header, deadline)?;

    let cmd = header[0];
    let status = header[1];
    let data_len = ((header[2] as usize) << 8) | (header[3] as usize);

    // 读取数据
    let mut data = vec![0u8; data_len];
    if data_len > 0 {
        read_exact_deadline(port, &mut data, deadline)?;
    }

    // 读取 EOF
    let mut eof = [0u8; 1];
    read_exact_deadline(port, &mut eof, deadline)?;
    if eof[0] != EOF {
        log::warn!("Air6201: 响应 EOF 不匹配: 0x{:02X}", eof[0]);
    }

    Ok(Some(Response { cmd, status, data }))
}

/// 在 deadline 之前读满 buf
fn read_exact_deadline(port: &mut dyn SerialPort, buf: &mut [u8], deadline: Instant) -> Result<()> {
    let mut filled = 0;
    while filled < buf.len() {
        if Instant::now() > deadline {
            bail!("Air6201: 读取超时 ({}/{} 字节)", filled, buf.len());
        }
        match port.read(&mut buf[filled..]) {
            Ok(n) => filled += n,
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

// ─── 高层协议操作 ────────────────────────────────────────────────────────────

/// 同步连接（发送 EXT_PROG 命令进入烧录模式，再进行协议同步）
fn sync_device(port: &mut dyn SerialPort, send_ext_prog: bool) -> Result<()> {
    let _ = port.clear(serialport::ClearBuffer::Input);

    // 发送 EXT_PROG 命令进入外置 Flash 编程模式
    if send_ext_prog {
        for attempt in 0..5 {
            log::info!("Air6201: 发送 EXT_PROG 命令 (尝试 {}/5)", attempt + 1);
            port.write_all(b"EXT_PROG\n")?;
            port.flush()?;

            // 等待设备返回 "OK\r\n"
            if wait_for_ok(port, Duration::from_secs(2)) {
                break;
            }
            if attempt < 4 {
                std::thread::sleep(Duration::from_secs(1));
            } else {
                bail!("Air6201: 发送 EXT_PROG 后未收到 OK 确认");
            }
        }
    }

    // 协议同步
    for attempt in 0..10 {
        log::debug!("Air6201: 同步尝试 {}/10", attempt + 1);
        send_command(port, CMD_SYNC, &[])?;

        match recv_response(port, Duration::from_secs(1))? {
            Some(resp) if resp.status == RESP_OK => {
                log::info!("Air6201: 同步成功");
                return Ok(());
            }
            _ => {
                let _ = port.clear(serialport::ClearBuffer::Input);
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }

    bail!("Air6201: 同步失败，请检查设备是否已启动并运行正确的固件");
}

/// 等待设备发送 "OK\r\n"
fn wait_for_ok(port: &mut dyn SerialPort, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];

    while Instant::now() < deadline {
        match port.read(&mut byte) {
            Ok(1) => {
                buf.push(byte[0]);
                if buf.len() >= 4 && buf[buf.len() - 4..] == *b"OK\r\n" {
                    return true;
                }
                // 限制缓冲区大小
                if buf.len() > 64 {
                    buf.drain(..buf.len() - 20);
                }
            }
            _ => continue,
        }
    }
    false
}

/// 获取设备信息
fn get_device_info(port: &mut dyn SerialPort) -> Result<String> {
    send_command(port, CMD_GET_INFO, &[])?;
    match recv_response(port, Duration::from_secs(5))? {
        Some(resp) if resp.status == RESP_OK => Ok(String::from_utf8_lossy(&resp.data).to_string()),
        Some(resp) => bail!("Air6201: 获取信息失败，状态码: 0x{:02X}", resp.status),
        None => bail!("Air6201: 获取信息超时"),
    }
}

/// 读取 Flash ID
fn read_flash_id(port: &mut dyn SerialPort) -> Result<u32> {
    send_command(port, CMD_READ_ID, &[])?;
    match recv_response(port, Duration::from_secs(5))? {
        Some(resp) if resp.status == RESP_OK && resp.data.len() >= 3 => {
            let id = ((resp.data[0] as u32) << 16) | ((resp.data[1] as u32) << 8) | (resp.data[2] as u32);
            Ok(id)
        }
        Some(resp) => bail!("Air6201: 读取 Flash ID 失败，状态码: 0x{:02X}", resp.status),
        None => bail!("Air6201: 读取 Flash ID 超时"),
    }
}

/// 擦除分区
fn erase_partition(port: &mut dyn SerialPort, part: &Partition) -> Result<()> {
    send_command(port, CMD_ERASE_PARTITION, &[part.id])?;
    // 擦除可能需要较长时间（大分区 ~60s）
    match recv_response(port, Duration::from_secs(60))? {
        Some(resp) if resp.status == RESP_OK => Ok(()),
        Some(resp) => bail!("Air6201: 擦除分区 {} 失败，状态码: 0x{:02X}", part.name, resp.status),
        None => bail!("Air6201: 擦除分区 {} 超时", part.name),
    }
}

/// 写入数据到指定地址
#[allow(clippy::too_many_arguments)]
fn write_data(port: &mut dyn SerialPort, addr: u32, data: &[u8], cancel: &AtomicBool, on_progress: &ProgressCallback, pct_start: f32, pct_end: f32, region: &str) -> Result<()> {
    let total = data.len();
    let mut offset = 0;

    while offset < total {
        if cancel.load(Ordering::Relaxed) {
            bail!("用户取消");
        }

        let chunk_len = std::cmp::min(CHUNK_SIZE, total - offset);
        let write_addr = addr + offset as u32;

        // 构建写入包: addr(4, big-endian) + len(4, big-endian) + data
        let mut packet = Vec::with_capacity(8 + chunk_len);
        packet.extend_from_slice(&write_addr.to_be_bytes());
        packet.extend_from_slice(&(chunk_len as u32).to_be_bytes());
        packet.extend_from_slice(&data[offset..offset + chunk_len]);

        send_command(port, CMD_WRITE, &packet)?;

        match recv_response(port, Duration::from_secs(10))? {
            Some(resp) if resp.status == RESP_OK => {}
            Some(resp) => {
                // 解析写入错误
                if resp.data.len() >= 12 && resp.data[8] == WRITE_ERR_MARKER {
                    let err_code = resp.data[9];
                    let flash_status = resp.data[10];
                    let err_name = match err_code {
                        0xFF => "未初始化",
                        0xFE => "SPI失败",
                        0xFD => "忙超时",
                        _ => "未知错误",
                    };
                    bail!("Air6201: 写入失败在 0x{:06X}: {} (Flash状态: 0x{:02X})", write_addr, err_name, flash_status);
                }
                bail!("Air6201: 写入失败在 0x{:06X}，状态码: 0x{:02X}", write_addr, resp.status);
            }
            None => bail!("Air6201: 写入超时在 0x{:06X}", write_addr),
        }

        offset += chunk_len;
        let pct = pct_start + (pct_end - pct_start) * (offset as f32 / total as f32);
        on_progress(&FlashProgress::info("Writing", pct, &format!("写入中 {}/{} ({:.0}%)", offset, total, offset as f64 / total as f64 * 100.0)).with_region(region));

        // 块间延时，等待 Flash 页编程完成
        if offset < total {
            std::thread::sleep(Duration::from_millis(INTER_CHUNK_DELAY_MS));
        }
    }

    Ok(())
}

/// 校验数据
#[allow(clippy::too_many_arguments)]
fn verify_data(port: &mut dyn SerialPort, addr: u32, data: &[u8], cancel: &AtomicBool, on_progress: &ProgressCallback, pct_start: f32, pct_end: f32, region: &str) -> Result<()> {
    let total = data.len();
    let mut offset = 0;

    while offset < total {
        if cancel.load(Ordering::Relaxed) {
            bail!("用户取消");
        }

        let chunk_len = std::cmp::min(CHUNK_SIZE, total - offset);
        let verify_addr = addr + offset as u32;

        // 构建校验包: addr(4, big-endian) + len(4, big-endian) + data
        let mut packet = Vec::with_capacity(8 + chunk_len);
        packet.extend_from_slice(&verify_addr.to_be_bytes());
        packet.extend_from_slice(&(chunk_len as u32).to_be_bytes());
        packet.extend_from_slice(&data[offset..offset + chunk_len]);

        // 重试机制
        let mut last_err = None;
        for retry in 0..3 {
            let _ = port.clear(serialport::ClearBuffer::Input);
            send_command(port, CMD_VERIFY, &packet)?;

            match recv_response(port, Duration::from_secs(10))? {
                Some(resp) if resp.status == RESP_OK => {
                    last_err = None;
                    break;
                }
                Some(resp) if resp.status == RESP_VERIFY_FAIL => {
                    bail!("Air6201: 校验失败在 0x{:06X}", verify_addr);
                }
                Some(resp) => {
                    last_err = Some(format!("状态码: 0x{:02X}", resp.status));
                    if retry < 2 {
                        std::thread::sleep(Duration::from_millis(200));
                    }
                }
                None => {
                    last_err = Some("超时".to_string());
                    if retry < 2 {
                        log::warn!("Air6201: 校验超时，重试 {}/3", retry + 1);
                        std::thread::sleep(Duration::from_millis(200));
                    }
                }
            }
        }
        if let Some(err) = last_err {
            bail!("Air6201: 校验失败在 0x{:06X}: {}", verify_addr, err);
        }

        offset += chunk_len;
        let pct = pct_start + (pct_end - pct_start) * (offset as f32 / total as f32);
        on_progress(&FlashProgress::info("Verify", pct, &format!("校验中 {}/{} ({:.0}%)", offset, total, offset as f64 / total as f64 * 100.0)).with_region(region));
    }

    Ok(())
}

/// 复位设备
fn reset_device(port: &mut dyn SerialPort) -> Result<()> {
    port.flush()?;
    send_command(port, CMD_RESET, &[])?;
    port.flush()?;
    // 设备收到 RESET 后立即重启，通常不会返回响应
    std::thread::sleep(Duration::from_millis(500));
    Ok(())
}

// ─── 公开 API ────────────────────────────────────────────────────────────────

/// 烧录 Air6201 外置 Flash 指定分区
///
/// 流程: 连接 → 同步 → 获取信息 → 擦除分区 → 写入 → 校验 → 复位
pub fn flash_partition(
    port_name: &str,
    baud_rate: u32,
    partition_name: &str,
    data: &[u8],
    send_ext_prog: bool,
    on_progress: &ProgressCallback,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    let part = partition_by_name(partition_name).with_context(|| format!("未知分区: {partition_name}（可选: script, fskv, lfs）"))?;

    if data.len() > part.size as usize {
        bail!("数据大小 ({} 字节) 超过分区 {} 容量 ({} 字节)", data.len(), part.name, part.size);
    }

    on_progress(&FlashProgress::info("Connect", 0.0, &format!("连接 {port_name} @ {baud_rate}")));

    let mut port = serialport::new(port_name, baud_rate)
        .timeout(Duration::from_millis(200))
        .open()
        .with_context(|| format!("无法打开串口 {port_name}"))?;

    // 禁用 DTR/RTS 避免影响设备
    let _ = port.write_data_terminal_ready(false);
    let _ = port.write_request_to_send(false);
    std::thread::sleep(Duration::from_millis(200));
    let _ = port.clear(serialport::ClearBuffer::All);

    // 同步
    on_progress(&FlashProgress::info("Sync", 5.0, "同步中…"));
    sync_device(port.as_mut(), send_ext_prog)?;

    if cancel.load(Ordering::Relaxed) {
        bail!("用户取消");
    }

    // 获取设备信息
    on_progress(&FlashProgress::info("Info", 10.0, "获取设备信息…"));
    match get_device_info(port.as_mut()) {
        Ok(info) => log::info!("Air6201 设备信息: {}", info),
        Err(e) => log::warn!("获取设备信息失败（非致命）: {}", e),
    }
    match read_flash_id(port.as_mut()) {
        Ok(id) => log::info!("Flash ID: 0x{:06X}", id),
        Err(e) => log::warn!("读取 Flash ID 失败（非致命）: {}", e),
    }

    if cancel.load(Ordering::Relaxed) {
        bail!("用户取消");
    }

    // 擦除分区
    on_progress(&FlashProgress::info("Erase", 15.0, &format!("擦除 {}…", part.name)));
    erase_partition(port.as_mut(), &part)?;
    on_progress(&FlashProgress::info("Erase", 25.0, "擦除完成"));

    if cancel.load(Ordering::Relaxed) {
        bail!("用户取消");
    }

    // 写入数据
    on_progress(&FlashProgress::info("Write", 25.0, &format!("写入 {} 字节…", data.len())));
    write_data(port.as_mut(), part.addr, data, &cancel, on_progress, 25.0, 75.0, part.name)?;
    on_progress(&FlashProgress::info("Write", 75.0, "写入完成"));

    if cancel.load(Ordering::Relaxed) {
        bail!("用户取消");
    }

    // 校验
    on_progress(&FlashProgress::info("Verify", 75.0, "校验数据…"));
    verify_data(port.as_mut(), part.addr, data, &cancel, on_progress, 75.0, 95.0, part.name)?;
    on_progress(&FlashProgress::info("Verify", 95.0, "校验通过"));

    // 复位设备
    on_progress(&FlashProgress::info("Reset", 98.0, "复位设备…"));
    reset_device(port.as_mut())?;

    on_progress(&FlashProgress::done_ok(&format!("{} 烧录完成", part.name)));
    Ok(())
}

/// 擦除 Air6201 外置 Flash 指定分区
pub fn erase_ext_partition(port_name: &str, baud_rate: u32, partition_name: &str, send_ext_prog: bool, on_progress: &ProgressCallback, cancel: Arc<AtomicBool>) -> Result<()> {
    let part = partition_by_name(partition_name).with_context(|| format!("未知分区: {partition_name}（可选: script, fskv, lfs）"))?;

    on_progress(&FlashProgress::info("Connect", 0.0, &format!("连接 {port_name} @ {baud_rate}")));

    let mut port = serialport::new(port_name, baud_rate)
        .timeout(Duration::from_millis(200))
        .open()
        .with_context(|| format!("无法打开串口 {port_name}"))?;

    let _ = port.write_data_terminal_ready(false);
    let _ = port.write_request_to_send(false);
    std::thread::sleep(Duration::from_millis(200));
    let _ = port.clear(serialport::ClearBuffer::All);

    on_progress(&FlashProgress::info("Sync", 10.0, "同步中…"));
    sync_device(port.as_mut(), send_ext_prog)?;

    if cancel.load(Ordering::Relaxed) {
        bail!("用户取消");
    }

    on_progress(&FlashProgress::info("Erase", 30.0, &format!("擦除 {}…", part.name)));
    erase_partition(port.as_mut(), &part)?;

    on_progress(&FlashProgress::info("Reset", 90.0, "复位设备…"));
    reset_device(port.as_mut())?;

    on_progress(&FlashProgress::done_ok(&format!("{} 擦除完成", part.name)));
    Ok(())
}
