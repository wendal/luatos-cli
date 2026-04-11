//! Lua compilation and filesystem synthesis for LuaDB images.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use walkdir::WalkDir;

use crate::{add_bk_crc, pack_luadb, LuadbEntry};

/// Search directories (relative to cwd) for luac executables.
const SEARCH_DIRS: &[&str] = &[
    "refs/origin_tools/tools",
    "../refs/origin_tools/tools",
];

/// Find the appropriate luac executable for the given bit width.
///
/// For `bitw == 64`, tries `luac_64bit.exe` first, then `luac.exe`.
/// For `bitw == 32` (or any other value), tries `luac.exe`.
/// Searches relative directories first, then the system PATH.
pub fn find_luac(bitw: u32) -> Result<PathBuf> {
    let candidates: Vec<&str> = if bitw == 64 {
        vec!["luac_64bit.exe", "luac.exe"]
    } else {
        vec!["luac.exe"]
    };

    let cwd = std::env::current_dir().context("failed to get current directory")?;

    for name in &candidates {
        // Search in well-known relative directories
        for dir in SEARCH_DIRS {
            let p = cwd.join(dir).join(name);
            if p.is_file() {
                return Ok(p);
            }
        }

        // Search relative to cwd directly
        let p = cwd.join(name);
        if p.is_file() {
            return Ok(p);
        }

        // Search system PATH
        if let Ok(output) = Command::new("where").arg(name).output() {
            if output.status.success() {
                let s = String::from_utf8_lossy(&output.stdout);
                if let Some(line) = s.lines().next() {
                    let p = PathBuf::from(line.trim());
                    if p.is_file() {
                        return Ok(p);
                    }
                }
            }
        }
    }

    bail!(
        "luac executable not found for bitw={bitw}; searched {:?} and system PATH",
        SEARCH_DIRS
    );
}

/// Compile a single `.lua` file to `.luac` using the luac subprocess.
pub fn compile_lua(src: &Path, dst: &Path, bitw: u32) -> Result<()> {
    let luac = find_luac(bitw)?;
    log::info!("compiling {} -> {}", src.display(), dst.display());

    let output = Command::new(&luac)
        .arg("-o")
        .arg(dst)
        .arg(src)
        .output()
        .with_context(|| format!("failed to execute {}", luac.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "luac compilation failed for {}:\n{}",
            src.display(),
            stderr
        );
    }
    Ok(())
}

/// Compile all `.lua` files in `src_dir` to `.luac` in `out_dir`.
///
/// Non-`.lua` files are copied as-is. The relative directory structure is
/// preserved. Returns a list of all output file paths.
pub fn compile_lua_dir(src_dir: &Path, out_dir: &Path, bitw: u32) -> Result<Vec<PathBuf>> {
    fs::create_dir_all(out_dir).context("failed to create output directory")?;

    let mut outputs = Vec::new();

    for entry in WalkDir::new(src_dir).into_iter() {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }

        let rel = entry
            .path()
            .strip_prefix(src_dir)
            .context("failed to strip source prefix")?;
        let dst_parent = out_dir.join(rel).parent().map(Path::to_path_buf);
        if let Some(ref p) = dst_parent {
            fs::create_dir_all(p)?;
        }

        let src_path = entry.path();
        if src_path.extension().and_then(|e| e.to_str()) == Some("lua") {
            let mut dst_path = out_dir.join(rel);
            dst_path.set_extension("luac");
            compile_lua(src_path, &dst_path, bitw)?;
            outputs.push(dst_path);
        } else {
            let dst_path = out_dir.join(rel);
            fs::copy(src_path, &dst_path)
                .with_context(|| format!("failed to copy {}", src_path.display()))?;
            outputs.push(dst_path);
        }
    }

    Ok(outputs)
}

/// Walk a directory and create [`LuadbEntry`] items for every file.
///
/// Only the filename (not the full path) is used as the entry name.
/// Entries are sorted so that `main.lua` / `main.luac` comes first.
pub fn collect_script_entries(dir: &Path) -> Result<Vec<LuadbEntry>> {
    let mut entries = Vec::new();

    for entry in WalkDir::new(dir).into_iter() {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }

        let filename = entry
            .path()
            .file_name()
            .context("entry has no filename")?
            .to_string_lossy()
            .into_owned();

        let data = fs::read(entry.path())
            .with_context(|| format!("failed to read {}", entry.path().display()))?;

        entries.push(LuadbEntry { filename, data });
    }

    // Sort: main.lua / main.luac first, then alphabetical.
    entries.sort_by(|a, b| {
        let a_main = is_main(&a.filename);
        let b_main = is_main(&b.filename);
        b_main.cmp(&a_main).then_with(|| a.filename.cmp(&b.filename))
    });

    Ok(entries)
}

fn is_main(name: &str) -> bool {
    name == "main.lua" || name == "main.luac"
}

/// High-level: compile (optional) → collect entries → pack LuaDB → add BK CRC (optional).
///
/// If `use_luac` is true, `.lua` files are compiled to `.luac` in a temporary
/// directory before packing. Returns the final binary image.
pub fn build_script_image(
    script_dir: &Path,
    use_luac: bool,
    bitw: u32,
    use_bkcrc: bool,
) -> Result<Vec<u8>> {
    let collect_dir = if use_luac {
        let tmp = tempfile::tempdir().context("failed to create temp directory")?;
        compile_lua_dir(script_dir, tmp.path(), bitw)?;
        // Persist the temp dir so it outlives this block; we clean it up below.
        tmp.keep()
    } else {
        script_dir.to_path_buf()
    };

    let entries = collect_script_entries(&collect_dir)?;
    log::info!("packing {} entries into LuaDB image", entries.len());

    let image = pack_luadb(&entries);

    let result = if use_bkcrc {
        add_bk_crc(&image)
    } else {
        image
    };

    // Clean up temp dir if we created one
    if use_luac && collect_dir != script_dir {
        let _ = fs::remove_dir_all(&collect_dir);
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_test_dir() -> TempDir {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("lib.lua"), b"return {}").unwrap();
        fs::write(dir.path().join("main.lua"), b"print('hello')").unwrap();
        fs::write(dir.path().join("data.bin"), b"\x00\x01\x02").unwrap();
        dir
    }

    #[test]
    fn collect_entries_main_first() {
        let dir = make_test_dir();
        let entries = collect_script_entries(dir.path()).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].filename, "main.lua");
    }

    #[test]
    fn collect_entries_reads_data() {
        let dir = make_test_dir();
        let entries = collect_script_entries(dir.path()).unwrap();
        let main = entries.iter().find(|e| e.filename == "main.lua").unwrap();
        assert_eq!(main.data, b"print('hello')");
    }

    #[test]
    fn build_image_no_luac() {
        let dir = make_test_dir();
        let image = build_script_image(dir.path(), false, 32, false).unwrap();
        // Should start with LuaDB magic
        assert_eq!(&image[0..6], &[0x01, 0x04, 0x5A, 0xA5, 0x5A, 0xA5]);
    }

    #[test]
    fn build_image_with_bkcrc() {
        let dir = make_test_dir();
        let image = build_script_image(dir.path(), false, 32, true).unwrap();
        // BK CRC adds 2 bytes per 32-byte block
        assert_eq!(image.len() % 34, 0);
    }

    #[test]
    fn main_luac_also_sorted_first() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("app.luac"), b"\x1bLua").unwrap();
        fs::write(dir.path().join("main.luac"), b"\x1bLua").unwrap();
        let entries = collect_script_entries(dir.path()).unwrap();
        assert_eq!(entries[0].filename, "main.luac");
    }
}
