use anyhow::Context;

use crate::{event, OutputFormat};

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
        OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(format, "soc.info", "ok", &info)?,
    }
    Ok(())
}

pub fn cmd_soc_unpack(path: &str, output: Option<&str>, format: &OutputFormat) -> anyhow::Result<()> {
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
        OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(
            format,
            "soc.unpack",
            "ok",
            serde_json::json!({
                "dir": out_path.display().to_string(),
                "chip": unpacked.info.chip.chip_type,
                "rom": unpacked.rom_path.display().to_string(),
                "flash_exe": unpacked.flash_exe.map(|p| p.display().to_string()),
            }),
        )?,
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
        OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(format, "soc.files", "ok", &files)?,
    }
    Ok(())
}

pub fn cmd_soc_combine(soc: &str, bin: &str, addr: &str, output: Option<&str>, format: &OutputFormat) -> anyhow::Result<()> {
    use std::fs;

    let hex_addr = luatos_soc::parse_addr(addr.trim()).ok_or_else(|| anyhow::anyhow!("Invalid address '{addr}' — use hex like 0x00D00000"))? as u32;

    anyhow::ensure!(std::path::Path::new(soc).exists(), "SOC file not found: {soc}");
    anyhow::ensure!(std::path::Path::new(bin).exists(), "Binary file not found: {bin}");

    let user_data = fs::read(bin).with_context(|| format!("read {bin}"))?;

    // Default output: <basename>_combined.soc next to the source
    let out_path: String = output.map(|s| s.to_string()).unwrap_or_else(|| {
        let p = std::path::Path::new(soc);
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
        let parent = p.parent().unwrap_or(std::path::Path::new("."));
        parent.join(format!("{stem}_combined.soc")).to_string_lossy().into_owned()
    });

    luatos_soc::combine_ec7xx_soc(soc, &user_data, hex_addr, &out_path)?;

    match format {
        OutputFormat::Text => {
            println!("Combined: {} bytes at 0x{hex_addr:08X}", user_data.len());
            println!("  Input:  {soc}");
            println!("  Binary: {bin}");
            println!("  Output: {out_path}");
        }
        OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(
            format,
            "soc.combine",
            "ok",
            serde_json::json!({
                "soc": soc,
                "bin": bin,
                "addr": format!("0x{hex_addr:08X}"),
                "size": user_data.len(),
                "output": out_path,
            }),
        )?,
    }
    Ok(())
}

pub fn cmd_soc_pack(dir: &str, output: &str, format: &OutputFormat) -> anyhow::Result<()> {
    let dir_path = std::path::Path::new(dir);
    anyhow::ensure!(dir_path.is_dir(), "Not a directory: {dir}");
    anyhow::ensure!(dir_path.join("info.json").exists(), "info.json not found in {dir}");

    luatos_soc::pack_soc(dir_path, output)?;

    match format {
        OutputFormat::Text => {
            println!("Packed {} → {output}", dir);
        }
        OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(format, "soc.pack", "ok", serde_json::json!({ "output": output }))?,
    }
    Ok(())
}
