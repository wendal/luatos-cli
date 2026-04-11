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
            let (vid, pid, manufacturer, product, serial_number) =
                if let serialport::SerialPortType::UsbPort(ref info) = p.port_type {
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

/// Callback invoked for each complete log line read from serial.
pub type LineCallback = Box<dyn Fn(&str) + Send>;

/// Open a serial port and stream log lines until `stop` is set.
///
/// Reads bytes from the port, splits on `\n`, trims `\r`, and calls
/// `on_line` for each complete line. Blocks the calling thread.
pub fn stream_log_lines(
    port_name: &str,
    baud_rate: u32,
    stop: Arc<AtomicBool>,
    on_line: LineCallback,
) -> anyhow::Result<()> {
    let mut serial = serialport::new(port_name, baud_rate)
        .timeout(Duration::from_millis(100))
        .open()
        .map_err(|e| anyhow::anyhow!("Cannot open {port_name}: {e}"))?;

    // Release DTR/RTS so the device runs normally
    let _ = serial.write_data_terminal_ready(false);
    let _ = serial.write_request_to_send(false);

    let mut buf = vec![0u8; 4096];
    let mut line_buf: Vec<u8> = Vec::with_capacity(256);

    while !stop.load(Ordering::Relaxed) {
        match serial.read(&mut buf) {
            Ok(n) if n > 0 => {
                for &b in &buf[..n] {
                    if b == b'\n' {
                        let text = String::from_utf8_lossy(&line_buf)
                            .trim_end_matches('\r')
                            .to_string();
                        line_buf.clear();
                        if !text.is_empty() {
                            on_line(&text);
                        }
                    } else {
                        line_buf.push(b);
                    }
                }
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
        let text = String::from_utf8_lossy(&line_buf)
            .trim_end_matches('\r')
            .to_string();
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
pub fn stream_binary(
    port_name: &str,
    baud_rate: u32,
    stop: Arc<AtomicBool>,
    on_data: BinaryCallback,
) -> anyhow::Result<()> {
    let mut serial = serialport::new(port_name, baud_rate)
        .timeout(Duration::from_millis(100))
        .open()
        .map_err(|e| anyhow::anyhow!("Cannot open {port_name}: {e}"))?;

    // Release DTR/RTS so the device runs normally
    let _ = serial.write_data_terminal_ready(false);
    let _ = serial.write_request_to_send(false);

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_ports_returns_vec() {
        let ports = list_ports();
        println!("Found {} ports:", ports.len());
        for p in &ports {
            println!(
                "  {} vid={:?} pid={:?} product={:?}",
                p.port_name, p.vid, p.pid, p.product
            );
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
}
