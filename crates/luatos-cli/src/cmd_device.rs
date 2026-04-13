use crate::{event, OutputFormat};

pub fn cmd_device_reboot(port: Option<&str>, chip: Option<&str>, format: &OutputFormat) -> anyhow::Result<()> {
    let chip_str = chip.unwrap_or("");
    luatos_flash::device::device_reboot(port, chip_str)?;

    let port_display = port.unwrap_or("auto");
    let chip_display = chip.unwrap_or("generic");

    match format {
        OutputFormat::Text => {
            println!("设备重启信号已发送 (port={port_display}, chip={chip_display})");
        }
        OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(
            format,
            "device.reboot",
            "ok",
            serde_json::json!({
                "port": port_display,
                "chip": chip_display,
            }),
        )?,
    }
    Ok(())
}

pub fn cmd_device_boot(port: Option<&str>, chip: Option<&str>, format: &OutputFormat) -> anyhow::Result<()> {
    let chip_str = chip.unwrap_or("");
    luatos_flash::device::device_enter_boot(port, chip_str)?;

    let port_display = port.unwrap_or("auto");
    let chip_display = chip.unwrap_or("generic");

    match format {
        OutputFormat::Text => {
            println!("进入 boot 模式信号已发送 (port={port_display}, chip={chip_display})");
        }
        OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(
            format,
            "device.boot",
            "ok",
            serde_json::json!({
                "port": port_display,
                "chip": chip_display,
            }),
        )?,
    }
    Ok(())
}
