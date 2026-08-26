use anyhow::{bail, Context};

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
            if let Some((fs_addr, fs_size)) = info.filesystem_partition() {
                println!("  FS Addr:    0x{fs_addr:08X}  ({} KB)", fs_size / 1024);
            }
            if let Some((kv_addr, kv_size)) = info.kv_partition() {
                println!("  KV Addr:    0x{kv_addr:08X}  ({} KB)", kv_size / 1024);
            }
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

pub fn cmd_soc_combine(soc: &str, bin: &str, addr: Option<&str>, output: Option<&str>, format: &OutputFormat) -> anyhow::Result<()> {
    use std::fs;

    anyhow::ensure!(std::path::Path::new(soc).exists(), "SOC file not found: {soc}");
    anyhow::ensure!(std::path::Path::new(bin).exists(), "Binary file not found: {bin}");

    let user_data = fs::read(bin).with_context(|| format!("read {bin}"))?;
    let info = luatos_soc::read_soc_info(soc).context("read soc info")?;

    // Default output: <basename>_combined.soc next to the source
    let out_path: String = output.map(|s| s.to_string()).unwrap_or_else(|| {
        let p = std::path::Path::new(soc);
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
        let parent = p.parent().unwrap_or(std::path::Path::new("."));
        parent.join(format!("{stem}_combined.soc")).to_string_lossy().into_owned()
    });

    // 按芯片族分发：EC7xx/Air8000 注入到 flash 地址（需 --addr）；RDA8910 替换 PAC 内 LUA
    // （无需 --addr，PAC 内 LUA 条目地址为权威值）
    let (is_rda, ec_addr): (bool, Option<u32>) = match info.family() {
        luatos_soc::ChipFamily::Ec718 => {
            let addr_str = addr.ok_or_else(|| anyhow::anyhow!("EC7xx/Air8000 需要 --addr <flash 地址>（如 0x00D00000）"))?;
            let hex_addr = luatos_soc::parse_addr(addr_str.trim()).ok_or_else(|| anyhow::anyhow!("Invalid address '{addr_str}' — use hex like 0x00D00000"))? as u32;
            luatos_soc::combine_ec7xx_soc(soc, &user_data, hex_addr, &out_path)?;
            (false, Some(hex_addr))
        }
        luatos_soc::ChipFamily::Rda8910 => {
            let tmp = tempfile::tempdir().context("tempdir")?;
            let up = luatos_soc::unpack_soc(soc, tmp.path()).context("unpack soc")?;
            let pac_data = fs::read(&up.rom_path).with_context(|| format!("read PAC {}", up.rom_path.display()))?;
            if let Some(a) = luatos_flash::rda8910::parse_pac(&pac_data)?.find("LUA").map(|e| e.addr) {
                log::info!("PAC LUA 条目地址 0x{a:08X}");
            }
            let new_pac = luatos_flash::rda8910::rebuild_pac(&pac_data, "LUA", &user_data).context("rebuild PAC (替换 LUA 条目)")?;
            fs::write(&up.rom_path, &new_pac).context("write rebuilt PAC")?;
            luatos_soc::pack_soc(&up.dir, &out_path).context("repack soc")?;
            (true, None)
        }
        other => bail!("soc combine 仅支持 EC7xx/Air8000 与 RDA8910/Air724UG，当前 chip: {}（族 {other}）", info.chip.chip_type),
    };

    match format {
        OutputFormat::Text => {
            if is_rda {
                println!("Replaced PAC LUA entry in {soc}");
                println!("  Script: {bin} ({} bytes)", user_data.len());
            } else {
                println!("Combined: {} bytes at 0x{:08X}", user_data.len(), ec_addr.unwrap());
                println!("  Binary: {bin}");
            }
            println!("  Input:  {soc}");
            println!("  Output: {out_path}");
        }
        OutputFormat::Json | OutputFormat::Jsonl => event::emit_result(
            format,
            "soc.combine",
            "ok",
            serde_json::json!({
                "soc": soc,
                "bin": bin,
                "addr": ec_addr.map(|a| format!("0x{a:08X}")).unwrap_or_default(),
                "size": user_data.len(),
                "mode": if is_rda { "replace_pac_lua" } else { "inject_flash_addr" },
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
