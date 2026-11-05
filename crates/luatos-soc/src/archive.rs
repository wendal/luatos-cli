// In-memory .soc archive reader — ZIP/7z entries read on demand without
// extracting to a temporary directory.

use std::fs::File;
use std::io::Read;

use anyhow::{Context, Result};

use crate::unpack::detect_soc_format;
use crate::{SocFormat, SocInfo};

enum SocArchiveInner {
    Zip(zip::ZipArchive<File>),
    SevenZ(Box<sevenz_rust2::ArchiveReader<File>>),
}

/// A .soc archive opened for in-memory reads.
///
/// Flash paths use this to pull `info.json`, the ROM image and optional
/// resources straight into memory; the archive file stays open for the
/// lifetime of the value and entries are decompressed on demand.
pub struct SocArchive {
    info: SocInfo,
    names: Vec<String>,
    inner: SocArchiveInner,
}

impl SocArchive {
    /// Open a .soc archive (ZIP or 7z, auto-detected by magic bytes) and parse
    /// its `info.json`.
    pub fn open(soc_path: &str) -> Result<Self> {
        let fmt = detect_soc_format(soc_path)?;
        let file = File::open(soc_path).with_context(|| format!("Cannot open .soc: {soc_path}"))?;
        match fmt {
            SocFormat::Zip => {
                let mut archive = zip::ZipArchive::new(file).context("Not a valid ZIP")?;
                let names: Vec<String> = (0..archive.len()).filter_map(|i| archive.name_for_index(i).map(|n| n.to_string())).collect();
                let info = read_zip_entry_json(&mut archive, "info.json").context("Parse info.json")?;
                Ok(Self {
                    info,
                    names,
                    inner: SocArchiveInner::Zip(archive),
                })
            }
            SocFormat::SevenZ => {
                let mut reader = sevenz_rust2::ArchiveReader::new(file, sevenz_rust2::Password::empty()).context("Not a valid 7z")?;
                let names: Vec<String> = reader.archive().files.iter().filter(|f| !f.is_directory()).map(|f| f.name().to_string()).collect();
                let data = reader.read_file("info.json").context("info.json not found")?;
                let info: SocInfo = serde_json::from_slice(&data).context("Parse info.json")?;
                Ok(Self {
                    info,
                    names,
                    inner: SocArchiveInner::SevenZ(Box::new(reader)),
                })
            }
        }
    }

    /// Parsed `info.json` metadata.
    pub fn info(&self) -> &SocInfo {
        &self.info
    }

    /// Read an entry fully into memory.
    pub fn read(&mut self, name: &str) -> Result<Vec<u8>> {
        match &mut self.inner {
            SocArchiveInner::Zip(archive) => {
                let mut entry = archive.by_name(name).with_context(|| format!("{name} not found in .soc"))?;
                let mut data = Vec::with_capacity(entry.size() as usize);
                entry.read_to_end(&mut data).with_context(|| format!("read {name} from .soc"))?;
                Ok(data)
            }
            SocArchiveInner::SevenZ(reader) => reader.read_file(name).with_context(|| format!("read {name} from .soc")),
        }
    }

    /// Whether an entry with this exact name exists.
    pub fn exists(&self, name: &str) -> bool {
        self.names.iter().any(|n| n == name)
    }

    /// All entry names in the archive.
    pub fn names(&self) -> &[String] {
        &self.names
    }
}

fn read_zip_entry_json(archive: &mut zip::ZipArchive<File>, name: &str) -> Result<SocInfo> {
    let entry = archive.by_name(name).with_context(|| format!("{name} not found"))?;
    let info: SocInfo = serde_json::from_reader(entry)?;
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_zip_soc_in_memory() {
        let soc = r"d:\github\luatos-cli\refs\soc_files\LuatOS-SoC_V2015_Air8101.soc";
        if !std::path::Path::new(soc).exists() {
            eprintln!("Skipping: {soc} not found");
            return;
        }
        let mut archive = SocArchive::open(soc).expect("SocArchive::open");
        assert_eq!(archive.info().chip.chip_type, "bk72xx");
        assert!(archive.exists("info.json"));
        let rom = archive.read(&archive.info().rom.file.clone()).expect("read rom");
        assert!(!rom.is_empty());
        assert!(!archive.names().is_empty());
    }

    #[test]
    fn open_7z_soc_in_memory() {
        let soc = r"d:\github\luatos-cli\refs\soc_files\LuatOS-SoC_V2001_Air6208_101.soc";
        if !std::path::Path::new(soc).exists() {
            eprintln!("Skipping: {soc} not found");
            return;
        }
        let mut archive = SocArchive::open(soc).expect("SocArchive::open");
        assert_eq!(archive.info().chip.chip_type, "air6208");
        assert!(archive.exists("info.json"));
        let rom = archive.read(&archive.info().rom.file.clone()).expect("read rom");
        assert!(!rom.is_empty());
    }
}
