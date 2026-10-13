//! Lua compilation and filesystem synthesis for LuaDB images.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use walkdir::WalkDir;

use crate::embedded_helpers::{ensure_embedded_helper, ensure_mklfs_helper};
use crate::romfs::{self, RomfsFile};
use crate::{add_bk_crc, pack_luadb, LuadbEntry};

/// 版本控制目录名称，遍历文件时需跳过
const VCS_DIRS: &[&str] = &[".git", ".svn", ".hg"];

/// Compile Lua source bytes to bytecode using the embedded Lua 5.3 compiler.
///
/// `chunk_name` is the name shown in error messages (typically `@filename.lua`).
/// `debug_mode` 为调试信息保留级别：0=无、1=仅行号、2=仅变量名、99=全部（非法值回退 99）。
pub fn compile_lua_bytes(source: &[u8], chunk_name: &str, debug_mode: u8, bitw: u32) -> Result<Vec<u8>> {
    let helper = ensure_embedded_helper(bitw).map_err(|e| anyhow::anyhow!("failed to prepare Lua {bitw}-bit compiler: {e}"))?;

    let mut child = Command::new(&helper)
        .arg(chunk_name)
        .arg(crate::normalize_luac_debug(debug_mode).to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn Lua {}-bit compiler from {}", bitw, helper.display()))?;

    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(source).context("failed to write source to Lua compiler")?;
    }

    let output = child.wait_with_output().context("failed to wait for Lua compiler")?;

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
///
/// `debug_mode` 为调试信息保留级别：0=无、1=仅行号、2=仅变量名、99=全部。
pub fn compile_lua(src: &Path, dst: &Path, bitw: u32, debug_mode: u8) -> Result<()> {
    let source = fs::read(src).with_context(|| format!("failed to read {}", src.display()))?;
    let chunk_name = format!(
        "@{}",
        src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| src.display().to_string())
    );
    log::info!("compiling {} -> {}", src.display(), dst.display());

    let bytecode = compile_lua_bytes(&source, &chunk_name, debug_mode, bitw)?;

    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(dst, &bytecode).with_context(|| format!("failed to write {}", dst.display()))?;
    Ok(())
}

/// Check if a directory name belongs to a version control system.
fn is_vcs_dir(name: &std::ffi::OsStr) -> bool {
    let s = name.to_string_lossy();
    VCS_DIRS.iter().any(|d| s.eq_ignore_ascii_case(d))
}

/// Compile all `.lua` files in `src_dir` to `.luac` in `out_dir`.
///
/// Non-`.lua` files are copied as-is. The relative directory structure is
/// preserved. `debug_mode` 为调试信息保留级别：0=无、1=仅行号、2=仅变量名、99=全部。
/// Returns a list of all output file paths.
/// 自动跳过 .git/.svn/.hg 等版本控制目录。
pub fn compile_lua_dir(src_dir: &Path, out_dir: &Path, bitw: u32, debug_mode: u8) -> Result<Vec<PathBuf>> {
    fs::create_dir_all(out_dir).context("failed to create output directory")?;

    let mut outputs = Vec::new();

    for entry in WalkDir::new(src_dir).into_iter().filter_entry(|e| !e.file_type().is_dir() || !is_vcs_dir(e.file_name())) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }

        let rel = entry.path().strip_prefix(src_dir).context("failed to strip source prefix")?;
        let dst_parent = out_dir.join(rel).parent().map(Path::to_path_buf);
        if let Some(ref p) = dst_parent {
            fs::create_dir_all(p)?;
        }

        let src_path = entry.path();
        if src_path.extension().and_then(|e| e.to_str()) == Some("lua") {
            let mut dst_path = out_dir.join(rel);
            dst_path.set_extension("luac");
            compile_lua(src_path, &dst_path, bitw, debug_mode)?;
            outputs.push(dst_path);
        } else {
            let dst_path = out_dir.join(rel);
            fs::copy(src_path, &dst_path).with_context(|| format!("failed to copy {}", src_path.display()))?;
            outputs.push(dst_path);
        }
    }

    Ok(outputs)
}

/// Walk multiple directories and create [`LuadbEntry`] items for every file.
///
/// Uses the relative path from each directory root as the entry name, with
/// forward-slash separators to support subdirectory structures.
/// If the same relative path appears in multiple directories, the last one wins.
/// Entries are sorted so that `main.lua` / `main.luac` comes first.
/// 自动跳过 .git/.svn/.hg 等版本控制目录。
pub fn collect_script_entries(dirs: &[&Path]) -> Result<Vec<LuadbEntry>> {
    // Use a map to handle deduplication: later dirs override earlier ones.
    let mut map = std::collections::HashMap::<String, Vec<u8>>::new();

    for dir in dirs {
        anyhow::ensure!(dir.exists(), "脚本目录不存在: {}", dir.display());
        for entry in WalkDir::new(dir).into_iter().filter_entry(|e| !e.file_type().is_dir() || !is_vcs_dir(e.file_name())) {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }

            // 使用相对路径作为文件名，支持子目录结构
            let rel_path = entry.path().strip_prefix(dir).context("failed to compute relative path")?;
            let filename = rel_path.to_string_lossy().into_owned().replace('\\', "/");

            let data = fs::read(entry.path()).with_context(|| format!("failed to read {}", entry.path().display()))?;

            map.insert(filename, data);
        }
    }

    let mut entries: Vec<LuadbEntry> = map.into_iter().map(|(filename, data)| LuadbEntry { filename, data }).collect();

    // Sort: main.lua / main.luac first, then alphabetical.
    entries.sort_by(|a, b| {
        let a_main = is_main(&a.filename);
        let b_main = is_main(&b.filename);
        b_main.cmp(&a_main).then_with(|| a.filename.cmp(&b.filename))
    });

    Ok(entries)
}

fn is_main(name: &str) -> bool {
    // 支持根目录的 main.lua/main.luac，以及子目录中的
    let basename = name.rsplit('/').next().unwrap_or(name);
    (basename == "main.lua" || basename == "main.luac") && !name.contains('/')
}

/// High-level: compile (optional) → collect entries → pack LuaDB → add BK CRC (optional).
///
/// Accepts multiple script source directories. If `use_luac` is true, `.lua`
/// files from all directories are compiled to `.luac` in a single temporary
/// directory before packing. `debug_mode` 为调试信息保留级别：0=无、1=仅行号、2=仅变量名、99=全部。
/// Later directories override earlier ones when filenames collide.
/// Returns the final binary image.
pub fn build_script_image(script_dirs: &[&Path], use_luac: bool, bitw: u32, use_bkcrc: bool, debug_mode: u8) -> Result<Vec<u8>> {
    // _tmp_guard 持有 TempDir：use_luac 时临时目录必须存活到函数末尾，
    // 因为 collect_script_entries 依赖其中的 luac 编译产物读取文件；
    // 函数返回时 TempDir 自动删除目录，无需（也不应）手动 remove_dir_all，避免 Windows 残留
    let (collect_dirs, _tmp_guard): (Vec<PathBuf>, Option<tempfile::TempDir>) = if use_luac {
        let tmp = tempfile::tempdir().context("failed to create temp directory")?;
        for dir in script_dirs {
            compile_lua_dir(dir, tmp.path(), bitw, debug_mode)?;
        }
        // TempDir 存活期间路径始终有效；collect_script_entries 在函数内同步完成读取，返回前数据已全部读入内存
        (vec![tmp.path().to_path_buf()], Some(tmp))
    } else {
        (script_dirs.iter().map(|d| d.to_path_buf()).collect(), None)
    };

    let dir_refs: Vec<&Path> = collect_dirs.iter().map(|p| p.as_path()).collect();
    let entries = collect_script_entries(&dir_refs)?;
    anyhow::ensure!(!entries.is_empty(), "脚本目录中没有找到任何文件");
    log::info!("packing {} entries into LuaDB image", entries.len());

    let image = pack_luadb(&entries)?;

    let result = if use_bkcrc { add_bk_crc(&image) } else { image };

    // 显式释放守卫，触发 TempDir 自动清理（collect_script_entries 已完成读取，此时删除目录是安全的）
    drop(_tmp_guard);

    Ok(result)
}

/// Build the resource partition image.
///
/// 设备端由 `components/romfs/luat_vfs_romfs.c` 经 CPU 窗口指针直读, 因此镜像是标准 romfs;
/// 是否按 BK CRC(每 32 字节数据 + 2 字节 CRC)帧化由 [`ResourceFsType`] 决定, 与 SOC 里的
/// `type`/`bkcrc` 一致。后一个目录会覆盖前面目录中相同相对路径的文件。
pub fn build_resource_image(dirs: &[&Path], capacity: usize, fs_type: ResourceFsType) -> Result<ResourceImage> {
    anyhow::ensure!(!dirs.is_empty(), "At least one resource directory is required");
    for dir in dirs {
        anyhow::ensure!(dir.is_dir(), "Resource directory does not exist or is not a directory: {}", dir.display());
    }
    let entries = collect_script_entries(dirs)?;
    let framed = fs_type.framed();
    match fs_type {
        ResourceFsType::Romfs | ResourceFsType::RomfsRaw => {
            let files: Vec<RomfsFile> = entries.into_iter().map(|e| RomfsFile { path: e.filename, data: e.data }).collect();
            let img = romfs::build(&files)?;
            let image = if framed { add_bk_crc(&img.data) } else { img.data };
            anyhow::ensure!(
                image.len() <= capacity,
                "Resource image ({} bytes{}) exceeds partition ({} bytes); clean payload limit is {} bytes",
                image.len(),
                if framed { " after BK CRC framing" } else { "" },
                capacity,
                payload_limit(capacity, framed)
            );
            Ok(ResourceImage {
                data: image,
                skipped: img.skipped,
            })
        }
    }
}

/// 资源分区镜像格式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceFsType {
    /// 标准 romfs + BK CRC 帧化; 设备端经 CPU 窗口虚拟地址指针直读, `mmap` 零拷贝可用
    Romfs,
    /// 标准 romfs 不帧化; 设备端经 CPU 窗口线性地址直接访问
    RomfsRaw,
}

impl ResourceFsType {
    /// SOC 里 `type: "romfs"` 配合 `bkcrc` 决定帧化与否
    pub fn from_metadata(fs_type: &str, bkcrc: bool) -> Result<Self> {
        anyhow::ensure!(fs_type == "romfs", "Unsupported resource filesystem type: {fs_type} (expected \"romfs\")");
        Ok(Self::from_framed(bkcrc))
    }

    pub fn from_framed(framed: bool) -> Self {
        if framed {
            Self::Romfs
        } else {
            Self::RomfsRaw
        }
    }

    /// 镜像是否按 BK CRC 帧化
    pub fn framed(self) -> bool {
        matches!(self, Self::Romfs)
    }

    /// SOC 里 `rom.fs.res.type` 的取值
    pub fn as_str(self) -> &'static str {
        "romfs"
    }
}

/// 打包结果: 待写入分区的字节与需要告警的路径
pub struct ResourceImage {
    /// 最终写入分区的字节(帧化格式时已按 BK CRC 帧化)
    pub data: Vec<u8>,
    /// 目录嵌套超限被跳过的相对路径
    pub skipped: Vec<String>,
}

/// 分区容量对应的净载荷上限: 帧化后每 34 字节承载 32 字节数据
pub fn payload_limit(capacity: usize, framed: bool) -> usize {
    if framed {
        capacity / 34 * 32
    } else {
        capacity
    }
}

/// Build a LittleFS image from a directory using the embedded mklfs helper.
///
/// * `source_dir` — directory of files to pack into the LFS image.
/// * `fs_size` — total filesystem size in bytes.
/// * `block_size` — erase block size (default 4096).
///
/// Returns the raw LFS image bytes (exactly `fs_size` bytes).
pub fn build_littlefs_image(source_dir: &Path, fs_size: usize, block_size: usize) -> Result<Vec<u8>> {
    let helper = ensure_mklfs_helper().map_err(|e| anyhow::anyhow!("failed to prepare mklfs helper: {e}"))?;

    let output = Command::new(&helper)
        .arg(source_dir.to_str().context("non-UTF8 path")?)
        .arg(fs_size.to_string())
        .arg(block_size.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("failed to run mklfs helper from {}", helper.display()))?;

    if output.status.success() {
        if output.stdout.len() != fs_size {
            bail!("mklfs produced {} bytes, expected {}", output.stdout.len(), fs_size);
        }
        Ok(output.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!("mklfs failed: {}", if stderr.is_empty() { "unknown error".to_string() } else { stderr });
    }
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
        let image = build_script_image(&[dir.path()], false, 32, false, crate::LUAC_DEBUG_ALL).unwrap();
        // Should start with LuaDB magic
        assert_eq!(&image[0..6], &[0x01, 0x04, 0x5A, 0xA5, 0x5A, 0xA5]);
    }

    #[test]
    fn build_image_with_bkcrc() {
        let dir = make_test_dir();
        let image = build_script_image(&[dir.path()], false, 32, true, crate::LUAC_DEBUG_ALL).unwrap();
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

        let image = build_script_image(&[dir1.path(), dir2.path()], false, 32, false, crate::LUAC_DEBUG_ALL).unwrap();
        assert_eq!(&image[0..6], &[0x01, 0x04, 0x5A, 0xA5, 0x5A, 0xA5]);
    }

    #[test]
    fn compile_lua_bytes_32bit() {
        let source = b"print('hello')";
        let bytecode = super::compile_lua_bytes(source, "@test.lua", crate::LUAC_DEBUG_ALL, 32).unwrap();
        // Lua 5.3 bytecode starts with 0x1B 0x4C 0x75 0x61 (ESC Lua)
        assert_eq!(&bytecode[0..4], b"\x1bLua");
        // Byte 15 = sizeof(lua_Integer), 32-bit = 4
        assert_eq!(bytecode[15], 4);
    }

    #[test]
    fn compile_lua_bytes_64bit() {
        let source = b"print('hello')";
        let bytecode = super::compile_lua_bytes(source, "@test.lua", crate::LUAC_DEBUG_ALL, 64).unwrap();
        assert_eq!(&bytecode[0..4], b"\x1bLua");
        // Byte 15 = sizeof(lua_Integer), 64-bit = 8
        assert_eq!(bytecode[15], 8);
    }

    /// 回归：长字符串（≥0xFF）的 size_t 必须按 4 字节写出。
    /// 曾因 64 位宿主 sizeof(size_t)=8 写出 8 字节、而 header 声称 size_t=4，
    /// 导致设备加载错位（"memory allocation error: block too big"）。
    #[test]
    fn long_string_size_is_4_bytes() {
        let src = format!("local s = \"{}\"\nreturn s", "a".repeat(300));
        let bytecode = super::compile_lua_bytes(src.as_bytes(), "@long.lua", crate::LUAC_DEBUG_ALL, 64).unwrap();
        // header offset 13 = sizeof(size_t)，必须为 4（设备兼容）
        assert_eq!(bytecode[13], 4, "header size_t 应为 4");
        // 301 (0x12D) 长字符串：0xFF 标记 + 4 字节小端长度
        let needle: [u8; 5] = [0xFF, 0x2D, 0x01, 0x00, 0x00];
        let pos = bytecode.windows(5).position(|w| w == needle).expect("长字符串大小应按 4 字节小端写出");
        // 4 字节长度之后紧跟字符串内容 'a'（而非 8 字节 size_t 的多余 0x00）
        assert_eq!(bytecode[pos + 5], b'a', "size 后应紧跟字符串，而非 8 字节 size_t 的 0 填充");
    }

    #[test]
    fn compile_lua_bytes_debug_modes() {
        use crate::{LUAC_DEBUG_ALL, LUAC_DEBUG_LINE, LUAC_DEBUG_NONE, LUAC_DEBUG_VAR};
        // 含局部变量与函数调用的脚本，保证 4 档产物大小有区分度
        let source = b"local foo = 1\nlocal bar = 2\nlocal function add(a, b) return a + b end\nprint(add(foo, bar))\n";
        let mode0 = super::compile_lua_bytes(source, "@test.lua", LUAC_DEBUG_NONE, 32).unwrap();
        let mode1 = super::compile_lua_bytes(source, "@test.lua", LUAC_DEBUG_LINE, 32).unwrap();
        let mode2 = super::compile_lua_bytes(source, "@test.lua", LUAC_DEBUG_VAR, 32).unwrap();
        let mode99 = super::compile_lua_bytes(source, "@test.lua", LUAC_DEBUG_ALL, 32).unwrap();
        // 4 档均产出合法 Lua 5.3 字节码
        for bc in [&mode0, &mode1, &mode2, &mode99] {
            assert_eq!(&bc[0..4], b"\x1bLua");
        }
        // 体积关系：0 最小，99 最大
        assert!(mode0.len() <= mode1.len(), "mode0({}) <= mode1({})", mode0.len(), mode1.len());
        assert!(mode0.len() <= mode2.len(), "mode0({}) <= mode2({})", mode0.len(), mode2.len());
        assert!(mode1.len() < mode99.len(), "mode1({}) < mode99({})", mode1.len(), mode99.len());
        assert!(mode2.len() < mode99.len(), "mode2({}) < mode99({})", mode2.len(), mode99.len());
        // 非法值回退 99，输出与 99 完全一致
        let mode5 = super::compile_lua_bytes(source, "@test.lua", 5, 32).unwrap();
        assert_eq!(mode5, mode99);
    }

    #[test]
    fn compile_lua_bytes_syntax_error() {
        let source = b"if then end end";
        let result = super::compile_lua_bytes(source, "@bad.lua", crate::LUAC_DEBUG_ALL, 32);
        assert!(result.is_err());
    }

    #[test]
    fn build_image_with_luac() {
        let dir = make_test_dir();
        let image = build_script_image(&[dir.path()], true, 32, false, crate::LUAC_DEBUG_ALL).unwrap();
        assert_eq!(&image[0..6], &[0x01, 0x04, 0x5A, 0xA5, 0x5A, 0xA5]);
    }

    #[test]
    fn build_littlefs_image_basic() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("test.txt"), b"hello littlefs").unwrap();
        fs::write(dir.path().join("data.bin"), vec![0xAA; 100]).unwrap();

        let fs_size = 64 * 1024; // 64 KB
        let image = super::build_littlefs_image(dir.path(), fs_size, 4096).unwrap();
        assert_eq!(image.len(), fs_size);
        // LittleFS superblock is in the first block; it shouldn't be all 0xFF
        assert_ne!(&image[0..16], &[0xFF; 16]);
    }

    #[test]
    fn build_littlefs_image_empty_dir() {
        let dir = TempDir::new().unwrap();
        let fs_size = 32 * 1024;
        let image = super::build_littlefs_image(dir.path(), fs_size, 4096).unwrap();
        assert_eq!(image.len(), fs_size);
    }
}

#[cfg(test)]
mod resource_tests {
    use super::*;

    /// 去掉 BK CRC 帧并以头部声明的长度截断, 同时校验尾部 0xFF 补齐。
    /// 帧化以 32 字节为单位, 因此净载荷后可能带 0xFF 填充(设备端只读 img_size 字节)。
    fn unframe(framed: &[u8]) -> Vec<u8> {
        assert_eq!(framed.len() % 34, 0, "帧化镜像长度应为 34 的整数倍");
        let mut padded = Vec::with_capacity(framed.len() / 34 * 32);
        for chunk in framed.chunks_exact(34) {
            padded.extend_from_slice(&chunk[..32]);
        }
        assert_eq!(&padded[..8], b"-rom1fs-");
        let declared = be32(&padded, 8) as usize;
        assert!(declared <= padded.len(), "头部声明的长度 {declared} 不得超过净载荷 {}", padded.len());
        assert!(padded[declared..].iter().all(|b| *b == 0xFF), "帧化尾部应以 0xFF 补齐");
        padded.truncate(declared);
        padded
    }

    fn be32(buf: &[u8], off: usize) -> u32 {
        u32::from_be_bytes(buf[off..off + 4].try_into().unwrap())
    }

    /// 遍历镜像内的兄弟链, 收集 (相对路径, 数据), 并校验头部 checksum
    fn walk(img: &[u8]) -> Vec<(String, Vec<u8>)> {
        assert_eq!(&img[..8], b"-rom1fs-");
        assert_eq!(be32(img, 8) as usize, img.len(), "头部记录的镜像长度应等于实际长度");
        let mut head = img.to_vec();
        let stored = be32(&head, 12);
        head[12..16].copy_from_slice(&[0, 0, 0, 0]);
        let mut sum: u32 = 0;
        // 头部 checksum 覆盖前 min(512, 总长) 字节; 镜像本身是 16 字节对齐的。
        // 校验和字段已清零, 因此 (区域累加 + 存储值) 必须为 0。
        for w in head.chunks_exact(4).take(512 / 4) {
            sum = sum.wrapping_add(u32::from_be_bytes(w.try_into().unwrap()));
        }
        assert_eq!(sum.wrapping_add(stored), 0, "头部 checksum 应为 BE u32 累加的补码");

        let mut out = Vec::new();
        walk_dir(img, be32(img, 36) as usize, "", &mut out);
        out
    }

    fn walk_dir(img: &[u8], mut off: usize, prefix: &str, out: &mut Vec<(String, Vec<u8>)>) {
        while off != 0 {
            assert_eq!(off % 16, 0, "条目偏移应 16 字节对齐");
            let word0 = be32(img, off);
            let next = (word0 & 0xFFFF_FFF0) as usize;
            let ty = word0 & 0xF;
            let spec = be32(img, off + 4) as usize;
            let size = be32(img, off + 8) as usize;
            let scan_end = (off + 16 + 128).min(img.len());
            let name_end = img[off + 16..scan_end].iter().position(|b| *b == 0).unwrap() + off + 16;
            let name = String::from_utf8(img[off + 16..name_end].to_vec()).unwrap();
            let data_off = off + 16 + (name.len() + 1).div_ceil(16) * 16;
            match ty {
                1 => walk_dir(img, spec, &format!("{prefix}{name}/"), out),
                2 => out.push((format!("{prefix}{name}"), img[data_off..data_off + size].to_vec())),
                other => panic!("未预期的条目类型 {other}"),
            }
            off = next;
        }
    }

    fn romfs_type() -> ResourceFsType {
        ResourceFsType::Romfs
    }

    #[test]
    fn recursive_files_overrides_and_large_payload() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        fs::create_dir_all(a.path().join("sub/deep")).unwrap();
        fs::write(a.path().join("sub/raw.lua"), b"not even valid lua!\0").unwrap();
        fs::write(a.path().join("sub/deep/x.bin"), vec![0x11; 100]).unwrap();
        fs::write(a.path().join("same"), b"old").unwrap();
        fs::write(b.path().join("same"), b"new").unwrap();
        let large: Vec<u8> = (0..80000).map(|i| (i % 256) as u8).collect();
        let long = "x".repeat(64);
        fs::write(a.path().join(&long), &large).unwrap();

        let image = build_resource_image(&[a.path(), b.path()], 200000, romfs_type()).unwrap();
        assert!(image.skipped.is_empty());
        let files: std::collections::HashMap<_, _> = walk(&unframe(&image.data)).into_iter().collect();
        assert_eq!(files.len(), 4);
        assert_eq!(files["sub/raw.lua"], b"not even valid lua!\0");
        assert_eq!(files["sub/deep/x.bin"], vec![0x11; 100]);
        assert_eq!(files["same"], b"new");
        assert_eq!(files[&long], large);
    }

    #[test]
    fn file_and_directory_with_same_name_is_rejected() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        fs::write(a.path().join("n"), b"file").unwrap();
        fs::create_dir(b.path().join("n")).unwrap();
        fs::write(b.path().join("n/x"), b"nested").unwrap();
        assert!(build_resource_image(&[a.path(), b.path()], 8192, romfs_type()).is_err());
    }

    #[test]
    fn capacity_is_measured_after_framing() {
        let d = tempfile::tempdir().unwrap();
        // 空资源目录打包成合法的空 romfs, 不是错误
        let empty = build_resource_image(&[d.path()], 4096, romfs_type()).unwrap();
        assert!(empty.skipped.is_empty());
        assert_eq!(unframe(&empty.data).len(), 64); // 头 16 + 卷名 16 + 根目录头 32
        assert_eq!(empty.data.len(), 68); // 2 帧

        assert!(build_resource_image(&[], 4096, romfs_type()).is_err());
        assert!(build_resource_image(&[&d.path().join("missing")], 4096, romfs_type()).is_err());
        assert!(build_resource_image(&[&d.path().join("a")], 8192, romfs_type()).is_err());

        // 净载荷上限 = 容量/34*32 = 3840; 逐字节逼近边界
        assert_eq!(payload_limit(4096, true), 3840);
        assert_eq!(payload_limit(4096, false), 4096);
        assert_eq!(payload_limit(3_080_192, true), 2_898_976);
        // 镜像 = 64(头+卷名+根目录) + 16(条目固定) + 16(名字 "a" 补齐) + a16(数据)
        fs::write(d.path().join("a"), vec![7; 3744]).unwrap();
        let fits = build_resource_image(&[d.path()], 4096, romfs_type()).unwrap();
        assert_eq!(unframe(&fits.data).len(), 3840);
        assert_eq!(fits.data.len(), 4080); // 120 帧
        fs::write(d.path().join("a"), vec![7; 3745]).unwrap();
        assert!(build_resource_image(&[d.path()], 4096, romfs_type()).is_err());
    }

    #[test]
    fn depth_over_limit_warns_and_skips() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("a/b/c/d")).unwrap();
        fs::create_dir_all(d.path().join("a/b/c/d/e")).unwrap();
        fs::write(d.path().join("a/b/c/d/keep.bin"), b"keep").unwrap();
        fs::write(d.path().join("a/b/c/d/e/drop.bin"), b"drop").unwrap();

        let image = build_resource_image(&[d.path()], 4096, romfs_type()).unwrap();
        assert_eq!(image.skipped, vec!["a/b/c/d/e/drop.bin".to_string()]);
        let files: std::collections::HashMap<_, _> = walk(&unframe(&image.data)).into_iter().collect();
        assert_eq!(files.len(), 1);
        assert_eq!(files["a/b/c/d/keep.bin"], b"keep");
    }

    #[test]
    fn name_boundaries() {
        let ok = tempfile::tempdir().unwrap();
        fs::write(ok.path().join("x".repeat(64)), b"ok").unwrap();
        assert!(build_resource_image(&[ok.path()], 8192, romfs_type()).is_ok());

        let too_long = tempfile::tempdir().unwrap();
        fs::write(too_long.path().join("x".repeat(65)), b"too long").unwrap();
        assert!(build_resource_image(&[too_long.path()], 8192, romfs_type()).is_err());

        // 汉字按 UTF-8 字节计算: 22 个汉字 = 66 字节, 超限
        let cjk = tempfile::tempdir().unwrap();
        fs::write(cjk.path().join("汉".repeat(22)), b"too long").unwrap();
        assert!(build_resource_image(&[cjk.path()], 8192, romfs_type()).is_err());
    }

    #[test]
    fn file_count_above_old_luadb_limit_is_allowed() {
        let d = tempfile::tempdir().unwrap();
        for i in 0..1100 {
            fs::write(d.path().join(format!("f{i:04}")), b"x").unwrap();
        }
        let image = build_resource_image(&[d.path()], 200000, romfs_type()).unwrap();
        assert_eq!(walk(&unframe(&image.data)).len(), 1100);
    }

    /// 脚本分区仍在用 Luadb, 其 27 字节名字上限不能因子资源分区改造而变化
    #[test]
    fn script_name_limit_is_unchanged() {
        let entry = |name: String| LuadbEntry { filename: name, data: vec![] };
        assert!(crate::pack_luadb(&[entry("x".repeat(27))]).is_ok());
        assert!(crate::pack_luadb(&[entry("x".repeat(28))]).is_err());
    }

    #[test]
    fn only_romfs_is_accepted_as_resource_type() {
        assert!(ResourceFsType::from_metadata("romfs", true).is_ok());
        assert!(ResourceFsType::from_metadata("romfs", false).is_ok());
        assert!(ResourceFsType::from_metadata("luadb", false).is_err());
        assert!(ResourceFsType::from_metadata("lfs", false).is_err());
        assert_eq!(romfs_type().as_str(), "romfs");
    }

    /// 未帧化(裸 romfs)模式下容量按原始镜像计算, 且不加 BK CRC
    #[test]
    fn raw_romfs_mode_skips_framing() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("a.bin"), b"raw mode").unwrap();
        let raw = build_resource_image(&[d.path()], 4096, ResourceFsType::from_framed(false)).unwrap();
        assert!(raw.data.len() < 4096);
        assert!(!raw.data.len().is_multiple_of(34), "裸镜像不应是 34 字节帧的整数倍");
        assert_eq!(raw.data.len(), unframe(&build_resource_image(&[d.path()], 8192, romfs_type()).unwrap().data).len());
        assert_eq!(payload_limit(4096, false), 4096);
    }
}
