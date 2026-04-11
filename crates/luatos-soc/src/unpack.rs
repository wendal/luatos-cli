// SOC file unpacking — extract and parse .soc archives.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::SocInfo;

/// Result of unpacking a .soc file.
pub struct UnpackedSoc {
    /// Parsed info.json metadata.
    pub info: SocInfo,
    /// Temporary directory containing extracted files.
    pub dir: PathBuf,
    /// Path to the ROM binary.
    pub rom_path: PathBuf,
    /// Path to air602_flash.exe if present (BK7258 subprocess mode).
    pub flash_exe: Option<PathBuf>,
}

/// Read and parse info.json from a .soc ZIP archive without full extraction.
pub fn read_soc_info(soc_path: &str) -> Result<SocInfo> {
    let file =
        std::fs::File::open(soc_path).with_context(|| format!("Cannot open .soc: {soc_path}"))?;
    let mut archive = zip::ZipArchive::new(file).context("Not a valid .soc file (expected zip)")?;
    let entry = archive
        .by_name("info.json")
        .context("info.json not found inside .soc")?;
    let info: SocInfo =
        serde_json::from_reader(entry).context("Failed to parse info.json")?;
    Ok(info)
}

/// Extract a .soc ZIP archive to `out_dir` and parse its metadata.
pub fn unpack_soc(soc_path: &str, out_dir: &Path) -> Result<UnpackedSoc> {
    let file =
        std::fs::File::open(soc_path).with_context(|| format!("Cannot open .soc: {soc_path}"))?;
    let mut archive = zip::ZipArchive::new(file).context("Not a valid .soc file (expected zip)")?;
    archive.extract(out_dir).context("Extraction failed")?;

    let info: SocInfo = serde_json::from_reader(
        std::fs::File::open(out_dir.join("info.json")).context("info.json missing after extract")?,
    )
    .context("Parse info.json")?;

    let rom_path = out_dir.join(&info.rom.file);
    let flash_exe = {
        let p = out_dir.join("air602_flash.exe");
        if p.exists() {
            Some(p)
        } else {
            None
        }
    };

    Ok(UnpackedSoc {
        info,
        dir: out_dir.to_path_buf(),
        rom_path,
        flash_exe,
    })
}

/// List files inside a .soc ZIP archive.
pub fn list_soc_files(soc_path: &str) -> Result<Vec<String>> {
    let file =
        std::fs::File::open(soc_path).with_context(|| format!("Cannot open .soc: {soc_path}"))?;
    let archive = zip::ZipArchive::new(file).context("Not a valid .soc file")?;
    let names: Vec<String> = (0..archive.len())
        .filter_map(|i| {
            archive
                .name_for_index(i)
                .map(|n| n.to_string())
        })
        .collect();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_air8101_soc() {
        let soc = r"d:\github\luatos-cli\refs\soc_files\LuatOS-SoC_V2013_Air8101.soc";
        if !Path::new(soc).exists() {
            eprintln!("Skipping: {soc} not found");
            return;
        }
        let info = read_soc_info(soc).expect("read_soc_info");
        assert_eq!(info.chip.chip_type, "bk72xx");
        println!("chip: {}", info.chip.chip_type);
        println!("rom: {}", info.rom.file);
        println!("flash_br: {}", info.flash_baud_rate());
        println!("log_br: {}", info.log_baud_rate());
    }

    #[test]
    fn list_air8101_files() {
        let soc = r"d:\github\luatos-cli\refs\soc_files\LuatOS-SoC_V2013_Air8101.soc";
        if !Path::new(soc).exists() {
            eprintln!("Skipping: {soc} not found");
            return;
        }
        let files = list_soc_files(soc).expect("list_soc_files");
        assert!(!files.is_empty());
        println!("Files in SOC:");
        for f in &files {
            println!("  {f}");
        }
        assert!(files.iter().any(|f| f == "info.json"));
    }
}
