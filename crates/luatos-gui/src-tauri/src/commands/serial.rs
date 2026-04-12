//! 串口枚举命令

use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct PortInfo {
    pub port_name: String,
    pub vid: Option<String>,
    pub pid: Option<String>,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub serial_number: Option<String>,
}

/// 列出系统可用串口
#[tauri::command]
pub fn serial_list() -> Vec<PortInfo> {
    let ports = match serialport::available_ports() {
        Ok(p) => p,
        Err(e) => {
            log::warn!("枚举串口失败: {e}");
            return vec![];
        }
    };

    ports
        .into_iter()
        .map(|p| {
            let (vid, pid, manufacturer, product, serial_number) = match &p.port_type {
                serialport::SerialPortType::UsbPort(usb) => (
                    Some(format!("{:04x}", usb.vid)),
                    Some(format!("{:04x}", usb.pid)),
                    usb.manufacturer.clone(),
                    usb.product.clone(),
                    usb.serial_number.clone(),
                ),
                _ => (None, None, None, None, None),
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
