//! 构建命令 — Lua 编译 + LuaDB 打包

use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
pub struct BuildResult {
    pub output_path: String,
    pub file_count: usize,
    pub size_bytes: usize,
}

/// 编译 Lua 脚本
#[tauri::command]
pub fn build_luac(src_dirs: Vec<String>, output_dir: String, bitw: u32) -> Result<BuildResult, String> {
    let out = Path::new(&output_dir);
    std::fs::create_dir_all(out).map_err(|e| format!("创建输出目录失败: {e}"))?;

    let mut total_files = Vec::new();
    for src in &src_dirs {
        let src_path = Path::new(src);
        if !src_path.is_dir() {
            return Err(format!("源目录不存在: {src}"));
        }
        let files = luatos_luadb::build::compile_lua_dir(src_path, out, bitw, true).map_err(|e| format!("编译失败: {e}"))?;
        total_files.extend(files);
    }

    Ok(BuildResult {
        output_path: output_dir,
        file_count: total_files.len(),
        size_bytes: 0,
    })
}

/// 构建 LuaDB 文件系统镜像
#[tauri::command]
pub fn build_filesystem(src_dirs: Vec<String>, output_path: String, use_luac: bool, bitw: u32, bkcrc: bool) -> Result<BuildResult, String> {
    let paths: Vec<PathBuf> = src_dirs.iter().map(PathBuf::from).collect();
    let path_refs: Vec<&Path> = paths.iter().map(|p| p.as_path()).collect();

    for p in &path_refs {
        if !p.is_dir() {
            return Err(format!("源目录不存在: {}", p.display()));
        }
    }

    let image = luatos_luadb::build::build_script_image(&path_refs, use_luac, bitw, bkcrc, true).map_err(|e| format!("构建失败: {e}"))?;

    let out = Path::new(&output_path);
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    std::fs::write(out, &image).map_err(|e| format!("写入文件失败: {e}"))?;

    Ok(BuildResult {
        output_path,
        file_count: paths.len(),
        size_bytes: image.len(),
    })
}
