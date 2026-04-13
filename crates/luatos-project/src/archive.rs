// .luatos archive — ZIP bundle of a LuatOS project.
//
// Format:
//   luatos-project.toml          — project config (always at root)
//   lua/<filename>               — script files, relative to project dir
//   <other-script-dirs>/<file>   — same for additional script_dirs
//
// The archive is a standard ZIP file with the `.luatos` extension.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use walkdir::WalkDir;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::{Project, CONFIG_FILE_NAME};

// ─── Export ──────────────────────────────────────────────────────────────────

/// Pack a LuatOS project directory into a `.luatos` archive.
///
/// `dir` is the project root (must contain `luatos-project.toml`).
/// `output_path` is the destination `.luatos` file.
///
/// The archive always contains `luatos-project.toml` at the root,
/// plus every file under each `build.script_dirs` directory and
/// every `build.script_files` entry, each at its path relative to `dir`.
pub fn export_project(dir: &Path, output_path: &Path) -> Result<ExportResult> {
    let project = Project::load(dir)?;

    let file = fs::File::create(output_path)
        .with_context(|| format!("create archive {}", output_path.display()))?;
    let mut zip = ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    // ── 1. Config ────────────────────────────────────────────────────────────
    let config_path = dir.join(CONFIG_FILE_NAME);
    let config_bytes = fs::read(&config_path)
        .with_context(|| format!("read {}", config_path.display()))?;
    zip.start_file(CONFIG_FILE_NAME, opts).context("zip: start config")?;
    zip.write_all(&config_bytes).context("zip: write config")?;

    // ── 2. Script directories ────────────────────────────────────────────────
    let mut files_added: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    for script_dir in &project.build.script_dirs {
        let abs_dir = dir.join(script_dir);
        if !abs_dir.exists() {
            continue;
        }
        for entry in WalkDir::new(&abs_dir)
            .min_depth(1)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            let abs_path = entry.path().to_path_buf();
            if !seen.insert(abs_path.clone()) {
                continue;
            }
            let rel = abs_path
                .strip_prefix(dir)
                .with_context(|| format!("strip prefix {}", abs_path.display()))?;
            let zip_path = rel.to_string_lossy().replace('\\', "/");
            zip.start_file(&zip_path, opts)
                .with_context(|| format!("zip: start {zip_path}"))?;
            let data = fs::read(&abs_path)
                .with_context(|| format!("read {}", abs_path.display()))?;
            zip.write_all(&data)
                .with_context(|| format!("zip: write {zip_path}"))?;
            files_added.push(zip_path);
        }
    }

    // ── 3. Individual script_files ───────────────────────────────────────────
    for script_file in &project.build.script_files {
        let abs_path = dir.join(script_file);
        if !abs_path.exists() || !seen.insert(abs_path.clone()) {
            continue;
        }
        let rel = abs_path
            .strip_prefix(dir)
            .with_context(|| format!("strip prefix {}", abs_path.display()))?;
        let zip_path = rel.to_string_lossy().replace('\\', "/");
        zip.start_file(&zip_path, opts)
            .with_context(|| format!("zip: start {zip_path}"))?;
        let data = fs::read(&abs_path)
            .with_context(|| format!("read {}", abs_path.display()))?;
        zip.write_all(&data)
            .with_context(|| format!("zip: write {zip_path}"))?;
        files_added.push(zip_path);
    }

    zip.finish().context("zip: finish")?;

    Ok(ExportResult {
        project_name: project.project.name,
        chip: project.project.chip,
        output: output_path.to_path_buf(),
        files_added,
    })
}

/// Result returned by [`export_project`].
pub struct ExportResult {
    pub project_name: String,
    pub chip: String,
    pub output: PathBuf,
    pub files_added: Vec<String>,
}

// ─── Import ───────────────────────────────────────────────────────────────────

/// Extract a `.luatos` archive into `output_dir`.
///
/// Existing files are overwritten. Parent directories are created on demand.
/// Returns the deserialized project config after extraction.
pub fn import_archive(archive_path: &Path, output_dir: &Path) -> Result<ImportResult> {
    let file = fs::File::open(archive_path)
        .with_context(|| format!("open archive {}", archive_path.display()))?;
    let mut zip = ZipArchive::new(file)
        .with_context(|| format!("parse ZIP {}", archive_path.display()))?;

    let mut files_extracted: Vec<String> = Vec::new();

    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).with_context(|| format!("zip entry {i}"))?;
        let zip_name = entry.name().to_owned();

        // Skip directories
        if zip_name.ends_with('/') {
            continue;
        }

        let dest = output_dir.join(&zip_name);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create dir {}", parent.display()))?;
        }

        let mut data = Vec::new();
        entry.read_to_end(&mut data)
            .with_context(|| format!("read zip entry {zip_name}"))?;
        fs::write(&dest, &data)
            .with_context(|| format!("write {}", dest.display()))?;

        files_extracted.push(zip_name);
    }

    let project = Project::load(output_dir)
        .context("archive does not contain a valid luatos-project.toml")?;

    Ok(ImportResult {
        project,
        output_dir: output_dir.to_path_buf(),
        files_extracted,
    })
}

/// Result returned by [`import_archive`].
pub struct ImportResult {
    pub project: Project,
    pub output_dir: PathBuf,
    pub files_extracted: Vec<String>,
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_test_project(dir: &Path) {
        crate::scaffold_project(dir, "archive_test", "bk72xx").unwrap();
        // Add a second file
        let lua_dir = dir.join("lua");
        fs::write(lua_dir.join("helper.lua"), "-- helper\n").unwrap();
    }

    #[test]
    fn export_import_round_trip() {
        let tmp = std::env::temp_dir().join("luatos_archive_rt");
        let _ = fs::remove_dir_all(&tmp);
        let proj_dir = tmp.join("project");
        let out_dir = tmp.join("imported");
        fs::create_dir_all(&proj_dir).unwrap();

        make_test_project(&proj_dir);

        let archive = tmp.join("test.luatos");
        let export = export_project(&proj_dir, &archive).unwrap();
        assert!(archive.exists());
        assert_eq!(export.project_name, "archive_test");
        assert!(!export.files_added.is_empty());

        fs::create_dir_all(&out_dir).unwrap();
        let import = import_archive(&archive, &out_dir).unwrap();
        assert_eq!(import.project.project.name, "archive_test");
        assert_eq!(import.project.project.chip, "bk72xx");

        // Verify main.lua was round-tripped
        assert!(out_dir.join("lua").join("main.lua").exists());

        let _ = fs::remove_dir_all(&tmp);
    }
}
