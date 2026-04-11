// SOC file packing — create and update .soc archives.
//
// ZIP format is used for bk72xx / air8101 / air8000.
// 7z format is used for air6208 / air101 / air103.

use anyhow::{Context, Result};
use std::fs;
use std::io::{Read, Write};
use std::path::Path;

use crate::unpack::{detect_soc_format, extract_7z, SocFormat};
use crate::SocInfo;

/// Create a ZIP-format .soc archive from all files in `dir`.
pub fn pack_soc_zip(dir: &Path, out_path: &str) -> Result<()> {
    let file = fs::File::create(out_path)
        .with_context(|| format!("Cannot create output: {out_path}"))?;
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored);

    add_dir_to_zip(&mut writer, dir, dir, options)?;
    writer.finish().context("Finalize ZIP")?;
    Ok(())
}

/// Recursively add directory contents to a ZIP writer.
fn add_dir_to_zip<W: Write + std::io::Seek>(
    writer: &mut zip::ZipWriter<W>,
    base: &Path,
    current: &Path,
    options: zip::write::SimpleFileOptions,
) -> Result<()> {
    let mut entries: Vec<_> = fs::read_dir(current)
        .with_context(|| format!("Cannot read dir: {}", current.display()))?
        .collect::<std::io::Result<Vec<_>>>()
        .context("Read dir entries")?;
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        let rel = path
            .strip_prefix(base)
            .context("Strip prefix")?
            .to_string_lossy()
            .replace('\\', "/");

        if path.is_dir() {
            add_dir_to_zip(writer, base, &path, options)?;
        } else {
            writer
                .start_file(&rel, options)
                .with_context(|| format!("Start ZIP entry: {rel}"))?;
            let mut f = fs::File::open(&path)
                .with_context(|| format!("Open file: {}", path.display()))?;
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)?;
            writer.write_all(&buf)?;
        }
    }
    Ok(())
}

/// Create a 7z-format .soc archive from all files in `dir` (pure Rust).
pub fn pack_soc_7z(dir: &Path, out_path: &str) -> Result<()> {
    use walkdir::WalkDir;

    let mut encoder = sevenz_rust2::ArchiveWriter::create(out_path)
        .with_context(|| format!("Cannot create 7z: {out_path}"))?;

    for entry in WalkDir::new(dir).into_iter() {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let rel = path
            .strip_prefix(dir)
            .context("Strip prefix")?
            .to_string_lossy()
            .replace('\\', "/");

        let src_file = fs::File::open(path)
            .with_context(|| format!("Read file: {}", path.display()))?;

        let archive_entry = sevenz_rust2::ArchiveEntry::from_path(path, rel);
        encoder
            .push_archive_entry(archive_entry, Some(src_file))
            .with_context(|| format!("Add 7z entry: {}", path.display()))?;
    }

    encoder.finish().context("Finalize 7z archive")?;
    Ok(())
}

/// Pack a directory into a .soc file, auto-detecting format from info.json chip type.
///
/// - bk72xx / air8101 / air8000 → ZIP
/// - air6208 / air101 / air103 → 7z
/// - No info.json → ZIP (default)
pub fn pack_soc(dir: &Path, out_path: &str) -> Result<()> {
    let info_path = dir.join("info.json");
    if info_path.exists() {
        let info: SocInfo = serde_json::from_reader(
            fs::File::open(&info_path).context("Open info.json")?,
        )
        .context("Parse info.json")?;

        let chip = info.chip.chip_type.to_lowercase();
        if matches!(chip.as_str(), "air6208" | "air101" | "air103") {
            return pack_soc_7z(dir, out_path);
        }
    }
    pack_soc_zip(dir, out_path)
}

/// Replace the script file inside an existing .soc archive with new data.
///
/// Reads `info.json` from the archive to determine the script filename,
/// then rebuilds the archive with the replacement.
pub fn update_soc_script(soc_path: &str, script_data: &[u8], out_path: &str) -> Result<()> {
    let fmt = detect_soc_format(soc_path)?;
    match fmt {
        SocFormat::Zip => update_script_zip(soc_path, script_data, out_path),
        SocFormat::SevenZ => update_script_7z(soc_path, script_data, out_path),
    }
}

/// Replace the script entry in a ZIP-format .soc.
fn update_script_zip(soc_path: &str, script_data: &[u8], out_path: &str) -> Result<()> {
    let src = fs::File::open(soc_path).with_context(|| format!("Open SOC: {soc_path}"))?;
    let mut archive = zip::ZipArchive::new(src).context("Not a valid ZIP")?;

    // Read info.json to find script filename
    let script_name = {
        let entry = archive
            .by_name("info.json")
            .context("info.json not found in SOC")?;
        let info: SocInfo = serde_json::from_reader(entry).context("Parse info.json")?;
        info.script.file
    };

    // Rebuild archive with the script entry replaced
    let out_file =
        fs::File::create(out_path).with_context(|| format!("Create output: {out_path}"))?;
    let mut writer = zip::ZipWriter::new(out_file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored);

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).context("Read ZIP entry")?;
        let name = entry.name().to_string();

        writer
            .start_file(&name, options)
            .with_context(|| format!("Start entry: {name}"))?;

        if name == script_name {
            writer.write_all(script_data)?;
        } else {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            writer.write_all(&buf)?;
        }
    }
    writer.finish().context("Finalize ZIP")?;
    Ok(())
}

/// Replace the script file in a 7z-format .soc by extracting, replacing, and repacking.
fn update_script_7z(soc_path: &str, script_data: &[u8], out_path: &str) -> Result<()> {
    let tempdir = tempfile::tempdir().context("Create temp dir")?;

    // Extract using pure Rust
    extract_7z(soc_path, tempdir.path())?;

    // Read info.json to find script filename
    let info: SocInfo = serde_json::from_reader(
        fs::File::open(tempdir.path().join("info.json")).context("info.json missing")?,
    )
    .context("Parse info.json")?;

    // Replace script file
    let script_path = tempdir.path().join(&info.script.file);
    fs::write(&script_path, script_data)
        .with_context(|| format!("Write script: {}", script_path.display()))?;

    // Repack
    pack_soc_7z(tempdir.path(), out_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn pack_zip_creates_valid_archive() {
        let tempdir = tempfile::tempdir().unwrap();
        let dir = tempdir.path();

        // Create some test files
        let info = r#"{"version":1,"chip":{"type":"bk72xx"},"rom":{"file":"rom.bin"},"script":{"file":"script.bin"},"download":{}}"#;
        fs::write(dir.join("info.json"), info).unwrap();
        fs::write(dir.join("rom.bin"), b"ROMDATA").unwrap();
        fs::write(dir.join("script.bin"), b"SCRIPTDATA").unwrap();

        let out = tempdir.path().join("test_out.soc");
        let out_str = out.to_string_lossy().to_string();
        pack_soc_zip(dir, &out_str).unwrap();

        // Verify it's a valid ZIP with the right entries
        let file = fs::File::open(&out).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect();
        assert!(names.contains(&"info.json".to_string()));
        assert!(names.contains(&"rom.bin".to_string()));
        assert!(names.contains(&"script.bin".to_string()));

        // Verify content
        let mut entry = archive.by_name("rom.bin").unwrap();
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).unwrap();
        assert_eq!(buf, b"ROMDATA");
    }

    #[test]
    fn update_script_replaces_file() {
        let tempdir = tempfile::tempdir().unwrap();
        let dir = tempdir.path();

        // Create a ZIP SOC with a script entry
        let info = r#"{"version":1,"chip":{"type":"bk72xx"},"rom":{"file":"rom.bin"},"script":{"file":"script.bin"},"download":{}}"#;
        fs::write(dir.join("info.json"), info).unwrap();
        fs::write(dir.join("rom.bin"), b"ROMDATA").unwrap();
        fs::write(dir.join("script.bin"), b"OLD_SCRIPT").unwrap();

        let soc_path = dir.join("original.soc");
        let soc_str = soc_path.to_string_lossy().to_string();
        pack_soc_zip(dir, &soc_str).unwrap();

        // Update the script
        let updated_path = dir.join("updated.soc");
        let updated_str = updated_path.to_string_lossy().to_string();
        update_soc_script(&soc_str, b"NEW_SCRIPT", &updated_str).unwrap();

        // Verify the updated archive has new script data
        let file = fs::File::open(&updated_path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        {
            let mut entry = archive.by_name("script.bin").unwrap();
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).unwrap();
            assert_eq!(buf, b"NEW_SCRIPT");
        }

        // Verify other files are unchanged
        {
            let mut rom_entry = archive.by_name("rom.bin").unwrap();
            let mut rom_buf = Vec::new();
            rom_entry.read_to_end(&mut rom_buf).unwrap();
            assert_eq!(rom_buf, b"ROMDATA");
        }
    }

    #[test]
    fn pack_soc_auto_detects_zip() {
        let tempdir = tempfile::tempdir().unwrap();
        let dir = tempdir.path();

        let info = r#"{"version":1,"chip":{"type":"bk72xx"},"rom":{"file":"rom.bin"},"script":{"file":"script.bin"},"download":{}}"#;
        fs::write(dir.join("info.json"), info).unwrap();
        fs::write(dir.join("rom.bin"), b"ROM").unwrap();
        fs::write(dir.join("script.bin"), b"SCRIPT").unwrap();

        let out = dir.join("auto.soc");
        let out_str = out.to_string_lossy().to_string();
        pack_soc(dir, &out_str).unwrap();

        // Should be ZIP format (PK magic)
        let data = fs::read(&out).unwrap();
        assert_eq!(data[0], 0x50); // 'P'
        assert_eq!(data[1], 0x4B); // 'K'
    }
}
