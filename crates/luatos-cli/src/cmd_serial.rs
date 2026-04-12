use crate::OutputFormat;

pub fn cmd_serial_list(format: &OutputFormat) -> anyhow::Result<()> {
    let ports = luatos_serial::list_ports();
    match format {
        OutputFormat::Text => {
            if ports.is_empty() {
                println!("No serial ports found.");
            } else {
                println!("{:<10} {:<10} {:<10} PRODUCT", "PORT", "VID", "PID");
                for p in &ports {
                    println!(
                        "{:<10} {:<10} {:<10} {}",
                        p.port_name,
                        p.vid.as_deref().unwrap_or("-"),
                        p.pid.as_deref().unwrap_or("-"),
                        p.product.as_deref().unwrap_or("-"),
                    );
                }
            }
        }
        OutputFormat::Json => {
            let json = serde_json::json!({
                "status": "ok",
                "command": "serial.list",
                "data": ports,
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
    }
    Ok(())
}
