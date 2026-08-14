// Serial port enumeration and log streaming.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Metadata about a serial port visible on the host system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortInfo {
    pub port_name: String,
    pub vid: Option<String>,
    pub pid: Option<String>,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub serial_number: Option<String>,
}

/// Return a list of all serial ports currently visible on the host system.
pub fn list_ports() -> Vec<PortInfo> {
    let raw = serialport::available_ports().unwrap_or_else(|e| {
        log::warn!("serialport::available_ports() failed: {}", e);
        Vec::new()
    });

    raw.into_iter()
        .map(|p| {
            let (vid, pid, manufacturer, product, serial_number) = if let serialport::SerialPortType::UsbPort(ref info) = p.port_type {
                (
                    Some(format!("{:04x}", info.vid)),
                    Some(format!("{:04x}", info.pid)),
                    info.manufacturer.clone(),
                    info.product.clone(),
                    info.serial_number.clone(),
                )
            } else {
                (None, None, None, None, None)
            };
            PortInfo {
                port_name: p.port_name,
                vid,
                pid,
                manufacturer,
                product,
                serial_number,
            }
        })
        .collect()
}

/// A simple ring-buffer wrapper for accumulating serial bytes.
pub struct SerialBuffer {
    inner: Vec<u8>,
    capacity: usize,
}

impl SerialBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Vec::with_capacity(capacity),
            capacity,
        }
    }

    /// Push raw bytes, discarding oldest when capacity is exceeded.
    pub fn push(&mut self, data: &[u8]) {
        if self.inner.len() + data.len() > self.capacity {
            let overflow = (self.inner.len() + data.len()) - self.capacity;
            self.inner.drain(..overflow);
        }
        self.inner.extend_from_slice(data);
    }

    /// Drain all accumulated bytes.
    pub fn drain(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.inner)
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

// ─── Log streaming ────────────────────────────────────────────────────────────

/// 单行日志的最大字节数：超过该长度仍无换行符时丢弃该行（内存保护）。
const MAX_LINE_BYTES: usize = 16 * 1024;

/// Callback invoked for each complete log line read from serial.
pub type LineCallback = Box<dyn Fn(&str) + Send>;

/// 处理一段串口字节流：按 `\n` 切分完整行并通过回调输出（自动去掉 `\r`）。
///
/// 当行缓冲达到 [`MAX_LINE_BYTES`] 仍无换行符时，丢弃超长行并清空缓冲，
/// 防止无换行的数据流让 `line_buf` 无限增长耗尽内存；超长丢弃只警告一次。
fn feed_log_bytes(data: &[u8], line_buf: &mut Vec<u8>, overflow_warned: &mut bool, mut on_line: impl FnMut(&str)) {
    for &b in data {
        if b == b'\n' {
            let text = String::from_utf8_lossy(line_buf).trim_end_matches('\r').to_string();
            line_buf.clear();
            if !text.is_empty() {
                on_line(&text);
            }
        } else if line_buf.len() >= MAX_LINE_BYTES {
            // 超长且无换行：丢弃已积累的字节，避免内存无限增长
            if !*overflow_warned {
                log::warn!("丢弃超长日志行（>{MAX_LINE_BYTES} 字节，无换行）");
                *overflow_warned = true;
            }
            line_buf.clear();
            line_buf.push(b);
        } else {
            line_buf.push(b);
        }
    }
}

/// Open a serial port and stream log lines until `stop` is set.
///
/// Reads bytes from the port, splits on `\n`, trims `\r`, and calls
/// `on_line` for each complete line. Blocks the calling thread.
pub fn stream_log_lines(port_name: &str, baud_rate: u32, stop: Arc<AtomicBool>, on_line: LineCallback) -> anyhow::Result<()> {
    let mut serial = serialport::new(port_name, baud_rate)
        .timeout(Duration::from_millis(100))
        .open()
        .map_err(|e| anyhow::anyhow!("Cannot open {port_name}: {e}"))?;

    // Release DTR/RTS so the device runs normally
    let _ = serial.write_data_terminal_ready(false);
    let _ = serial.write_request_to_send(false);

    let mut buf = vec![0u8; 4096];
    let mut line_buf: Vec<u8> = Vec::with_capacity(256);
    let mut overflow_warned = false;

    while !stop.load(Ordering::Relaxed) {
        match serial.read(&mut buf) {
            Ok(n) if n > 0 => {
                feed_log_bytes(&buf[..n], &mut line_buf, &mut overflow_warned, |t| on_line(t));
            }
            Ok(_) => {}
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {
                // Normal timeout, continue
            }
            Err(e) => {
                if !stop.load(Ordering::Relaxed) {
                    log::warn!("Serial read error: {e}");
                }
                break;
            }
        }
    }

    // Flush remaining partial line
    if !line_buf.is_empty() {
        let text = String::from_utf8_lossy(&line_buf).trim_end_matches('\r').to_string();
        if !text.is_empty() {
            on_line(&text);
        }
    }

    Ok(())
}

/// Callback invoked for each raw chunk of binary data from serial.
pub type BinaryCallback = Box<dyn Fn(&[u8]) + Send>;

/// Open a serial port and stream raw binary data until `stop` is set.
///
/// Used for SOC binary log protocol (0xA5 framed) and other binary protocols.
/// When `init_data` is provided, it is written to the port before reading
/// begins (e.g. a probe command to trigger log output on Air1601).
/// When `dtr_rts_high` is true, DTR and RTS are set HIGH after opening
/// (required for EC718 log port).
pub fn stream_binary(port_name: &str, baud_rate: u32, stop: Arc<AtomicBool>, on_data: BinaryCallback, init_data: Option<&[u8]>, dtr_rts_high: bool) -> anyhow::Result<()> {
    let mut serial = serialport::new(port_name, baud_rate)
        .timeout(Duration::from_millis(100))
        .open()
        .map_err(|e| anyhow::anyhow!("Cannot open {port_name}: {e}"))?;

    if dtr_rts_high {
        let _ = serial.write_data_terminal_ready(true);
        let _ = serial.write_request_to_send(true);
    } else {
        // Release DTR/RTS so the device runs normally
        let _ = serial.write_data_terminal_ready(false);
        let _ = serial.write_request_to_send(false);
    }

    // Send initial probe / handshake data if provided
    if let Some(data) = init_data {
        use std::io::Write;
        serial.write_all(data)?;
        serial.flush()?;
    }

    let mut buf = vec![0u8; 4096];

    while !stop.load(Ordering::Relaxed) {
        match serial.read(&mut buf) {
            Ok(n) if n > 0 => {
                on_data(&buf[..n]);
            }
            Ok(_) => {}
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => {
                if !stop.load(Ordering::Relaxed) {
                    log::warn!("Serial read error: {e}");
                }
                break;
            }
        }
    }

    Ok(())
}

/// 通过 RTS 脚发送复位脉冲，使模组重启。
///
/// 典型时序（适用于 CH340X 改装硬件）：
///   RTS=HIGH（RTS# 拉低 → RESET 有效）→ reset_ms → RTS=LOW（RTS# 释放 → RESET 释放）
///
/// 函数返回后串口已释放，可重新打开进行日志采集。
pub fn rts_reset_pulse(port_name: &str, reset_ms: u64) -> anyhow::Result<()> {
    let mut port = serialport::new(port_name, 115200)
        .timeout(Duration::from_millis(200))
        .open()
        .map_err(|e| anyhow::anyhow!("打开串口 {port_name} 失败（RTS 复位）: {e}"))?;

    // 拉高 RTS 触发复位（CH340X RTS# 为倒相输出：软件 HIGH → 引脚 LOW → RESET 有效）
    port.write_request_to_send(true).map_err(|e| anyhow::anyhow!("设置 RTS 失败: {e}"))?;
    std::thread::sleep(Duration::from_millis(reset_ms));

    // 释放 RTS，模组开始启动
    port.write_request_to_send(false).map_err(|e| anyhow::anyhow!("释放 RTS 失败: {e}"))?;

    // port drop → 串口释放
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_ports_returns_vec() {
        let ports = list_ports();
        println!("Found {} ports:", ports.len());
        for p in &ports {
            println!("  {} vid={:?} pid={:?} product={:?}", p.port_name, p.vid, p.pid, p.product);
        }
    }

    #[test]
    fn serial_buffer_overflow() {
        let mut buf = SerialBuffer::new(8);
        buf.push(&[1, 2, 3, 4]);
        buf.push(&[5, 6, 7, 8, 9]); // overflow
        assert_eq!(buf.len(), 8);
        let data = buf.drain();
        assert_eq!(data, vec![2, 3, 4, 5, 6, 7, 8, 9]);
    }

    #[test]
    fn feed_log_bytes_splits_lines() {
        let mut line_buf: Vec<u8> = Vec::new();
        let mut warned = false;
        let mut lines: Vec<String> = Vec::new();
        feed_log_bytes(b"first\r\nsecond\nthird", &mut line_buf, &mut warned, |t| lines.push(t.to_string()));
        assert_eq!(lines, vec!["first".to_string(), "second".to_string()]);
        assert_eq!(line_buf, b"third", "未换行的残留字节应保留在缓冲中");
    }

    #[test]
    fn feed_log_bytes_drops_overlong_no_newline_stream() {
        let mut line_buf: Vec<u8> = Vec::new();
        let mut warned = false;
        let mut lines: Vec<String> = Vec::new();

        // 灌入远超上限且无换行的数据流：应被丢弃而非无限积累
        let overlong = vec![b'x'; MAX_LINE_BYTES * 3];
        feed_log_bytes(&overlong, &mut line_buf, &mut warned, |t| lines.push(t.to_string()));
        assert!(lines.is_empty(), "超长无换行数据不应被当作完整行输出");
        assert!(line_buf.len() <= MAX_LINE_BYTES, "行缓冲必须保持有界，防止内存耗尽");
        assert!(warned, "超长行应触发一次警告");

        // 结束残留半行后，正常行仍能被正确解析
        feed_log_bytes(b"\n", &mut line_buf, &mut warned, |t| lines.push(t.to_string()));
        feed_log_bytes(b"hello world\n", &mut line_buf, &mut warned, |t| lines.push(t.to_string()));
        assert_eq!(lines.last().map(String::as_str), Some("hello world"), "丢弃超长行后正常行仍应被解析");
    }
}
