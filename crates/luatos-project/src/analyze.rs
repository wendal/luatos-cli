//! Project file analysis utilities.
//!
//! Provides file collection with raw size info, used by the `project analyze`
//! command to build a comprehensive project health report.
//!
//! Build-time steps (compilation, image packing) are intentionally left to
//! the CLI layer so this crate does not need to depend on `luatos-luadb`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Raw metadata about a single project source file.
#[derive(Debug, Clone)]
pub struct ProjectFile {
    /// Filename only (e.g. `"main.lua"`).
    pub filename: String,
    /// Absolute path on disk.
    pub path: PathBuf,
    /// File size on disk in bytes.
    pub raw_size: usize,
    /// `true` for `.lua` / `.luac` files, `false` for data/resource files.
    pub is_lua: bool,
}

/// Collect all project source files with their raw sizes.
///
/// Paths in `script_dirs` and `script_files` are resolved relative to
/// `project_dir`. Returns a map of filename → [`ProjectFile`].
pub fn collect_project_files(
    script_dirs: &[String],
    script_files: &[String],
    project_dir: &Path,
) -> Result<BTreeMap<String, ProjectFile>> {
    let abs_dirs: Vec<String> =
        script_dirs.iter().map(|d| project_dir.join(d).to_string_lossy().into_owned()).collect();
    let abs_files: Vec<String> =
        script_files.iter().map(|f| project_dir.join(f).to_string_lossy().into_owned()).collect();

    let raw = crate::lua_deps::collect_script_files(&abs_dirs, &abs_files)?;

    let mut result = BTreeMap::new();
    for (name, path) in raw {
        let raw_size = fs::metadata(&path)
            .with_context(|| format!("cannot stat {}", path.display()))?
            .len() as usize;
        let is_lua = name.ends_with(".lua") || name.ends_with(".luac");
        result.insert(name.clone(), ProjectFile { filename: name, path, raw_size, is_lua });
    }
    Ok(result)
}
