use crate::OutputFormat;

pub fn cmd_soc_info(path: &str, format: &OutputFormat) -> anyhow::Result<()> {
    let info = luatos_soc::read_soc_info(path)?;
    match format {
        OutputFormat::Text => {
            println!("SOC File: {path}");
            println!("  Chip:       {}", info.chip.chip_type);
            println!("  ROM:        {}", info.rom.file);
            if let Some(ref bsp) = info.rom.version_bsp {
                println!("  BSP:        {bsp}");
            }
            println!("  Flash BR:   {}", info.flash_baud_rate());
            println!("  Log BR:     {}", info.log_baud_rate());
            if let Some(ref addr) = info.download.bl_addr {
                println!("  BL Addr:    {addr}");
            }
            if let Some(ref addr) = info.download.script_addr {
                println!("  Script Addr:{addr}");
            }
            println!("  BK CRC:     {}", info.use_bkcrc());
        }
        OutputFormat::Json => {
            let json = serde_json::json!({
                "status": "ok",
                "command": "soc.info",
                "data": info,
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
    }
    Ok(())
}

pub fn cmd_soc_unpack(
    path: &str,
    output: Option<&str>,
    format: &OutputFormat,
) -> anyhow::Result<()> {
    let out_dir = output.unwrap_or(".");
    let out_path = std::path::Path::new(out_dir);
    std::fs::create_dir_all(out_path)?;
    let unpacked = luatos_soc::unpack_soc(path, out_path)?;
    match format {
        OutputFormat::Text => {
            println!("Extracted to: {}", out_path.display());
            println!("  Chip:  {}", unpacked.info.chip.chip_type);
            println!("  ROM:   {}", unpacked.rom_path.display());
            if let Some(ref exe) = unpacked.flash_exe {
                println!("  Exe:   {}", exe.display());
            }
        }
        OutputFormat::Json => {
            let json = serde_json::json!({
                "status": "ok",
                "command": "soc.unpack",
                "data": {
                    "dir": out_path.display().to_string(),
                    "chip": unpacked.info.chip.chip_type,
                    "rom": unpacked.rom_path.display().to_string(),
                    "flash_exe": unpacked.flash_exe.map(|p| p.display().to_string()),
                },
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
    }
    Ok(())
}

pub fn cmd_soc_files(path: &str, format: &OutputFormat) -> anyhow::Result<()> {
    let files = luatos_soc::list_soc_files(path)?;
    match format {
        OutputFormat::Text => {
            println!("Files in {path}:");
            for f in &files {
                println!("  {f}");
            }
        }
        OutputFormat::Json => {
            let json = serde_json::json!({
                "status": "ok",
                "command": "soc.files",
                "data": files,
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
    }
    Ok(())
}

pub fn cmd_soc_pack(dir: &str, output: &str, format: &OutputFormat) -> anyhow::Result<()> {
    let dir_path = std::path::Path::new(dir);
    anyhow::ensure!(dir_path.is_dir(), "Not a directory: {dir}");
    anyhow::ensure!(
        dir_path.join("info.json").exists(),
        "info.json not found in {dir}"
    );

    luatos_soc::pack_soc(dir_path, output)?;

    match format {
        OutputFormat::Text => {
            println!("Packed {} → {output}", dir);
        }
        OutputFormat::Json => {
            let json = serde_json::json!({
                "status": "ok",
                "command": "soc.pack",
                "data": { "output": output },
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
    }
    Ok(())
}
