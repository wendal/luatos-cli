// SOC file unpacking — extract and parse .soc archives.
//
// Air8101 (BK7258) uses ZIP format; Air6208 (XT804) and others use 7z.
// Auto-detection is based on the file magic bytes:
//   ZIP: PK (0x50 0x4B)
//   7z:  7z (0x37 0x7A 0xBC 0xAF)

use anyhow::{bail, Context, Result};
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
    /// Path to flash tool executable if present (e.g. air602_flash.exe, air101_flash.exe).
    pub flash_exe: Option<PathBuf>,
}

/// Archive format detected from magic bytes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SocFormat {
    Zip,
    SevenZ,
}

/// Detect the archive format from the first few bytes.
pub fn detect_soc_format(soc_path: &str) -> Result<SocFormat> {
    let mut f =
        std::fs::File::open(soc_path).with_context(|| format!("Cannot open .soc: {soc_path}"))?;
    let mut magic = [0u8; 6];
    use std::io::Read;
    f.read_exact(&mut magic)
        .context("File too small to detect format")?;
    if magic[0] == 0x50 && magic[1] == 0x4B {
        Ok(SocFormat::Zip)
    } else if magic[0] == 0x37
        && magic[1] == 0x7A
        && magic[2] == 0xBC
        && magic[3] == 0xAF
    {
        Ok(SocFormat::SevenZ)
    } else {
        bail!(
            "Unknown .soc format (magic: {:02x}{:02x}{:02x}{:02x})",
            magic[0],
            magic[1],
            magic[2],
            magic[3]
        );
    }
}

/// Read and parse info.json from a .soc archive (ZIP or 7z).
pub fn read_soc_info(soc_path: &str) -> Result<SocInfo> {
    let fmt = detect_soc_format(soc_path)?;
    match fmt {
        SocFormat::Zip => read_soc_info_zip(soc_path),
        SocFormat::SevenZ => read_soc_info_7z(soc_path),
    }
}

fn read_soc_info_zip(soc_path: &str) -> Result<SocInfo> {
    let file = std::fs::File::open(soc_path)?;
    let mut archive = zip::ZipArchive::new(file).context("Not a valid ZIP")?;
    let entry = archive
        .by_name("info.json")
        .context("info.json not found")?;
    let info: SocInfo = serde_json::from_reader(entry).context("Parse info.json")?;
    Ok(info)
}

fn read_soc_info_7z(soc_path: &str) -> Result<SocInfo> {
    let tempdir = tempfile::tempdir().context("Create temp dir")?;
    extract_7z(soc_path, tempdir.path())?;
    let info: SocInfo = serde_json::from_reader(
        std::fs::File::open(tempdir.path().join("info.json"))
            .context("info.json missing after 7z extract")?,
    )
    .context("Parse info.json")?;
    Ok(info)
}

/// Extract a .soc archive (ZIP or 7z) to `out_dir` and parse its metadata.
pub fn unpack_soc(soc_path: &str, out_dir: &Path) -> Result<UnpackedSoc> {
    let fmt = detect_soc_format(soc_path)?;
    match fmt {
        SocFormat::Zip => unpack_soc_zip(soc_path, out_dir),
        SocFormat::SevenZ => unpack_soc_7z(soc_path, out_dir),
    }
}

fn unpack_soc_zip(soc_path: &str, out_dir: &Path) -> Result<UnpackedSoc> {
    let file = std::fs::File::open(soc_path)?;
    let mut archive = zip::ZipArchive::new(file).context("Not a valid ZIP")?;
    archive.extract(out_dir).context("ZIP extraction failed")?;
    finalize_unpacked(out_dir)
}

fn unpack_soc_7z(soc_path: &str, out_dir: &Path) -> Result<UnpackedSoc> {
    extract_7z(soc_path, out_dir)?;
    finalize_unpacked(out_dir)
}

fn finalize_unpacked(out_dir: &Path) -> Result<UnpackedSoc> {
    let info: SocInfo = serde_json::from_reader(
        std::fs::File::open(out_dir.join("info.json")).context("info.json missing")?,
    )
    .context("Parse info.json")?;

    let rom_path = out_dir.join(&info.rom.file);

    // Look for known flash executables
    let flash_exe = ["air602_flash.exe", "air101_flash.exe"]
        .iter()
        .map(|name| out_dir.join(name))
        .find(|p| p.exists());

    Ok(UnpackedSoc {
        info,
        dir: out_dir.to_path_buf(),
        rom_path,
        flash_exe,
    })
}

/// Extract a 7z archive using the sevenz-rust2 crate (pure Rust, no subprocess).
pub(crate) fn extract_7z(soc_path: &str, out_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("Cannot create output dir: {}", out_dir.display()))?;
    sevenz_rust2::decompress_file(soc_path, out_dir)
        .with_context(|| format!("7z extraction failed: {soc_path}"))?;
    Ok(())
}

/// List files inside a .soc archive (ZIP or 7z).
pub fn list_soc_files(soc_path: &str) -> Result<Vec<String>> {
    let fmt = detect_soc_format(soc_path)?;
    match fmt {
        SocFormat::Zip => list_soc_files_zip(soc_path),
        SocFormat::SevenZ => list_soc_files_7z(soc_path),
    }
}

fn list_soc_files_zip(soc_path: &str) -> Result<Vec<String>> {
    let file = std::fs::File::open(soc_path)?;
    let archive = zip::ZipArchive::new(file).context("Not a valid ZIP")?;
    let names: Vec<String> = (0..archive.len())
        .filter_map(|i| archive.name_for_index(i).map(|n| n.to_string()))
        .collect();
    Ok(names)
}

fn list_soc_files_7z(soc_path: &str) -> Result<Vec<String>> {
    let mut file = std::fs::File::open(soc_path)
        .with_context(|| format!("Cannot open: {soc_path}"))?;
    let password = sevenz_rust2::Password::empty();
    let archive = sevenz_rust2::Archive::read(&mut file, &password)
        .context("Failed to read 7z archive")?;
    let names: Vec<String> = archive
        .files
        .iter()
        .filter(|f| !f.is_directory())
        .map(|f| f.name().to_string())
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
        let fmt = detect_soc_format(soc).unwrap();
        assert_eq!(fmt, SocFormat::Zip);
        let info = read_soc_info(soc).expect("read_soc_info");
        assert_eq!(info.chip.chip_type, "bk72xx");
        println!("chip: {}", info.chip.chip_type);
    }

    #[test]
    fn read_air6208_soc() {
        let soc = r"d:\github\luatos-cli\refs\soc_files\LuatOS-SoC_V2001_Air6208_101.soc";
        if !Path::new(soc).exists() {
            eprintln!("Skipping: {soc} not found");
            return;
        }
        let fmt = detect_soc_format(soc).unwrap();
        assert_eq!(fmt, SocFormat::SevenZ);
        let info = read_soc_info(soc).expect("read_soc_info");
        assert_eq!(info.chip.chip_type, "air6208");
        println!("chip: {}", info.chip.chip_type);
        println!("rom: {}", info.rom.file);
        println!("log_br: {}", info.log_baud_rate());
    }

    #[test]
    fn list_air8101_files() {
        let soc = r"d:\github\luatos-cli\refs\soc_files\LuatOS-SoC_V2013_Air8101.soc";
        if !Path::new(soc).exists() {
            return;
        }
        let files = list_soc_files(soc).expect("list_soc_files");
        assert!(!files.is_empty());
        assert!(files.iter().any(|f| f == "info.json"));
    }

    #[test]
    fn list_air6208_files() {
        let soc = r"d:\github\luatos-cli\refs\soc_files\LuatOS-SoC_V2001_Air6208_101.soc";
        if !Path::new(soc).exists() {
            return;
        }
        let files = list_soc_files(soc).expect("list_soc_files");
        assert!(!files.is_empty());
        assert!(files.iter().any(|f| f == "info.json"));
        println!("Air6208 files: {:?}", files);
    }
}
