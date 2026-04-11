// Serial port enumeration and log streaming.

use serde::{Deserialize, Serialize};

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
