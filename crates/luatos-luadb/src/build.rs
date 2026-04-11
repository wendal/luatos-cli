//! Lua compilation and filesystem synthesis for LuaDB images.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use walkdir::WalkDir;

use crate::embedded_helpers::ensure_embedded_helper;
use crate::{add_bk_crc, pack_luadb, LuadbEntry};

/// Compile Lua source bytes to bytecode using the embedded Lua 5.3 compiler.
///
/// `chunk_name` is the name shown in error messages (typically `@filename.lua`).
/// When `strip` is true, debug info is removed for smaller output.
pub fn compile_lua_bytes(
    source: &[u8],
    chunk_name: &str,
    strip: bool,
    bitw: u32,
) -> Result<Vec<u8>> {
    let helper = ensure_embedded_helper(bitw)
        .map_err(|e| anyhow::anyhow!("failed to prepare Lua {bitw}-bit compiler: {e}"))?;

    let mut child = Command::new(&helper)
        .arg(chunk_name)
        .arg(if strip { "1" } else { "0" })
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| {
            format!(
                "failed to spawn Lua {}-bit compiler from {}",
                bitw,
                helper.display()
            )
        })?;

    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(source)
            .context("failed to write source to Lua compiler")?;
    }

    let output = child
        .wait_with_output()
        .context("failed to wait for Lua compiler")?;

    if output.status.success() {
        Ok(output.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.is_empty() {
            bail!("Lua {}-bit compiler failed", bitw);
        } else {
            bail!("{}", stderr);
        }
    }
}

/// Compile a single `.lua` file to `.luac` using the embedded compiler.
pub fn compile_lua(src: &Path, dst: &Path, bitw: u32) -> Result<()> {
    let source = fs::read(src).with_context(|| format!("failed to read {}", src.display()))?;
    let chunk_name = format!(
        "@{}",
        src.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| src.display().to_string())
    );
    log::info!("compiling {} -> {}", src.display(), dst.display());

    let bytecode = compile_lua_bytes(&source, &chunk_name, false, bitw)?;

    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(dst, &bytecode).with_context(|| format!("failed to write {}", dst.display()))?;
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

/// Walk multiple directories and create [`LuadbEntry`] items for every file.
///
/// Only the filename (not the full path) is used as the entry name.
/// If the same filename appears in multiple directories, the last one wins.
/// Entries are sorted so that `main.lua` / `main.luac` comes first.
pub fn collect_script_entries(dirs: &[&Path]) -> Result<Vec<LuadbEntry>> {
    // Use a map to handle deduplication: later dirs override earlier ones.
    let mut map = std::collections::HashMap::<String, Vec<u8>>::new();

    for dir in dirs {
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

            map.insert(filename, data);
        }
    }

    let mut entries: Vec<LuadbEntry> = map
        .into_iter()
        .map(|(filename, data)| LuadbEntry { filename, data })
        .collect();

    // Sort: main.lua / main.luac first, then alphabetical.
    entries.sort_by(|a, b| {
        let a_main = is_main(&a.filename);
        let b_main = is_main(&b.filename);
        b_main
            .cmp(&a_main)
            .then_with(|| a.filename.cmp(&b.filename))
    });

    Ok(entries)
}

fn is_main(name: &str) -> bool {
    name == "main.lua" || name == "main.luac"
}

/// High-level: compile (optional) → collect entries → pack LuaDB → add BK CRC (optional).
///
/// Accepts multiple script source directories. If `use_luac` is true, `.lua`
/// files from all directories are compiled to `.luac` in a single temporary
/// directory before packing. Later directories override earlier ones when
/// filenames collide. Returns the final binary image.
pub fn build_script_image(
    script_dirs: &[&Path],
    use_luac: bool,
    bitw: u32,
    use_bkcrc: bool,
) -> Result<Vec<u8>> {
    let (collect_dirs, _tmp_guard): (Vec<PathBuf>, Option<PathBuf>) = if use_luac {
        let tmp = tempfile::tempdir().context("failed to create temp directory")?;
        for dir in script_dirs {
            compile_lua_dir(dir, tmp.path(), bitw)?;
        }
        let kept: PathBuf = tmp.keep();
        (vec![kept.clone()], Some(kept))
    } else {
        (script_dirs.iter().map(|d| d.to_path_buf()).collect(), None)
    };

    let dir_refs: Vec<&Path> = collect_dirs.iter().map(|p| p.as_path()).collect();
    let entries = collect_script_entries(&dir_refs)?;
    log::info!("packing {} entries into LuaDB image", entries.len());

    let image = pack_luadb(&entries);

    let result = if use_bkcrc { add_bk_crc(&image) } else { image };

    // Clean up temp dir if we created one
    if let Some(tmp_path) = _tmp_guard {
        let _ = fs::remove_dir_all(&tmp_path);
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
        let entries = collect_script_entries(&[dir.path()]).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].filename, "main.lua");
    }

    #[test]
    fn collect_entries_reads_data() {
        let dir = make_test_dir();
        let entries = collect_script_entries(&[dir.path()]).unwrap();
        let main = entries.iter().find(|e| e.filename == "main.lua").unwrap();
        assert_eq!(main.data, b"print('hello')");
    }

    #[test]
    fn build_image_no_luac() {
        let dir = make_test_dir();
        let image = build_script_image(&[dir.path()], false, 32, false).unwrap();
        // Should start with LuaDB magic
        assert_eq!(&image[0..6], &[0x01, 0x04, 0x5A, 0xA5, 0x5A, 0xA5]);
    }

    #[test]
    fn build_image_with_bkcrc() {
        let dir = make_test_dir();
        let image = build_script_image(&[dir.path()], false, 32, true).unwrap();
        // BK CRC adds 2 bytes per 32-byte block
        assert_eq!(image.len() % 34, 0);
    }

    #[test]
    fn main_luac_also_sorted_first() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("app.luac"), b"\x1bLua").unwrap();
        fs::write(dir.path().join("main.luac"), b"\x1bLua").unwrap();
        let entries = collect_script_entries(&[dir.path()]).unwrap();
        assert_eq!(entries[0].filename, "main.luac");
    }

    #[test]
    fn multi_dir_collect() {
        let dir1 = TempDir::new().unwrap();
        let dir2 = TempDir::new().unwrap();
        fs::write(dir1.path().join("main.lua"), b"print('v1')").unwrap();
        fs::write(dir1.path().join("lib.lua"), b"return {}").unwrap();
        fs::write(dir2.path().join("extra.lua"), b"--extra").unwrap();
        // Override lib.lua from dir2
        fs::write(dir2.path().join("lib.lua"), b"return {v=2}").unwrap();

        let entries = collect_script_entries(&[dir1.path(), dir2.path()]).unwrap();
        assert_eq!(entries.len(), 3); // main.lua, lib.lua (overridden), extra.lua
        assert_eq!(entries[0].filename, "main.lua");
        // lib.lua should have dir2's content
        let lib = entries.iter().find(|e| e.filename == "lib.lua").unwrap();
        assert_eq!(lib.data, b"return {v=2}");
    }

    #[test]
    fn multi_dir_build_image() {
        let dir1 = TempDir::new().unwrap();
        let dir2 = TempDir::new().unwrap();
        fs::write(dir1.path().join("main.lua"), b"print('hello')").unwrap();
        fs::write(dir2.path().join("util.lua"), b"return 42").unwrap();

        let image = build_script_image(&[dir1.path(), dir2.path()], false, 32, false).unwrap();
        assert_eq!(&image[0..6], &[0x01, 0x04, 0x5A, 0xA5, 0x5A, 0xA5]);
    }

    #[test]
    fn compile_lua_bytes_32bit() {
        let source = b"print('hello')";
        let bytecode = super::compile_lua_bytes(source, "@test.lua", false, 32).unwrap();
        // Lua 5.3 bytecode starts with 0x1B 0x4C 0x75 0x61 (ESC Lua)
        assert_eq!(&bytecode[0..4], b"\x1bLua");
        // Byte 17 = sizeof(lua_Integer), 32-bit = 4
        assert_eq!(bytecode[17], 4);
    }

    #[test]
    fn compile_lua_bytes_64bit() {
        let source = b"print('hello')";
        let bytecode = super::compile_lua_bytes(source, "@test.lua", false, 64).unwrap();
        assert_eq!(&bytecode[0..4], b"\x1bLua");
        // Byte 17 = sizeof(lua_Integer), 64-bit = 8
        assert_eq!(bytecode[17], 8);
    }

    #[test]
    fn compile_lua_bytes_strip() {
        let source = b"local x = 1; print(x)";
        let full = super::compile_lua_bytes(source, "@test.lua", false, 32).unwrap();
        let stripped = super::compile_lua_bytes(source, "@test.lua", true, 32).unwrap();
        // Stripped should be smaller (no debug info)
        assert!(stripped.len() < full.len());
    }

    #[test]
    fn compile_lua_bytes_syntax_error() {
        let source = b"if then end end";
        let result = super::compile_lua_bytes(source, "@bad.lua", false, 32);
        assert!(result.is_err());
    }

    #[test]
    fn build_image_with_luac() {
        let dir = make_test_dir();
        let image = build_script_image(&[dir.path()], true, 32, false).unwrap();
        assert_eq!(&image[0..6], &[0x01, 0x04, 0x5A, 0xA5, 0x5A, 0xA5]);
    }
}
