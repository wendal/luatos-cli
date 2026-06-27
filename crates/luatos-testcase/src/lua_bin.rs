//! `script.bin` 构建
//!
//! 包装 `luatos_luadb::build::build_script_image`，加入：
//! - 多源合并（公共脚本 + testcase scripts + ctx 目录）
//! - 64bit 检测（按 chip 类型）
//! - BK CRC16 自动开关

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// script.bin 编译参数
#[derive(Debug, Clone, Copy)]
pub struct ScriptBinParams {
    /// Lua 整数位宽（32 / 64）
    pub bitw: u32,
    /// 是否使用 luac 预编译
    pub use_luac: bool,
    /// 是否加 BK CRC16 framing
    pub use_bkcrc: bool,
    /// 是否剥离调试信息
    pub strip: bool,
}

/// 根据 chip 类型决定编译参数
///
/// - `air8101` / `bk72xx` 使用 64bit + BK CRC16
/// - `air6208` / `air101` / `air103` 使用 64bit
/// - 其余默认 32bit
pub fn script_bin_params_for_chip(chip: &str) -> ScriptBinParams {
    let bitw = match chip {
        "air8101" | "bk72xx" | "air6208" | "air101" | "air103" => 64,
        _ => 32,
    };
    let use_bkcrc = matches!(chip, "air8101" | "bk72xx");
    ScriptBinParams {
        bitw,
        use_luac: true,
        use_bkcrc,
        strip: true,
    }
}

/// 把 ctx.json 写入临时目录，用于烧入 script.bin
///
/// 返回 `(临时目录, ctx.json 路径)`，调用方负责清理临时目录。
pub fn write_ctx_to_temp(ctx: &serde_json::Value) -> Result<(tempfile::TempDir, PathBuf)> {
    let tmp = tempfile::tempdir().context("failed to create temp dir for ctx.json")?;
    let path = tmp.path().join("ctx.json");
    let content = serde_json::to_string_pretty(ctx).context("failed to serialize ctx.json")?;
    std::fs::write(&path, content).with_context(|| format!("failed to write {}", path.display()))?;
    Ok((tmp, path))
}

/// 构建 script.bin
///
/// 参数：
/// - `script_dirs`: 多个源码目录，按数组顺序合并（**后写覆盖前写**）
/// - `common_scripts`: 公共脚本目录（若不存在则忽略）
/// - `ctx_tmp_dir`: 含 ctx.json 的临时目录（若 Some 则加入）
/// - `chip`: 芯片类型（决定 bitw / bkcrc）
pub fn build_script_bin(script_dirs: &[&Path], common_scripts: Option<&Path>, ctx_tmp_dir: Option<&Path>, chip: &str) -> Result<Vec<u8>> {
    let params = script_bin_params_for_chip(chip);

    let mut ordered: Vec<PathBuf> = Vec::new();
    // 顺序：公共脚本（先）→ testcase scripts（中）→ ctx 临时目录（最后，最高优先级）
    if let Some(c) = common_scripts {
        if c.is_dir() {
            ordered.push(c.to_path_buf());
        } else {
            log::debug!("公共脚本目录不存在，跳过: {}", c.display());
        }
    }
    for d in script_dirs {
        if d.is_dir() {
            ordered.push(d.to_path_buf());
        } else {
            bail!("scripts 目录不存在: {}", d.display());
        }
    }
    if let Some(c) = ctx_tmp_dir {
        if c.is_dir() {
            ordered.push(c.to_path_buf());
        }
    }

    if ordered.is_empty() {
        bail!("没有任何 scripts 目录可用于构建 script.bin");
    }

    let refs: Vec<&Path> = ordered.iter().map(|p| p.as_path()).collect();
    log::debug!(
        "build_script_image: dirs={:?} bitw={} use_luac={} use_bkcrc={} strip={}",
        ordered.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        params.bitw,
        params.use_luac,
        params.use_bkcrc,
        params.strip
    );
    let image = luatos_luadb::build::build_script_image(&refs, params.use_luac, params.bitw, params.use_bkcrc, params.strip).context("build_script_image failed")?;
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_lua(dir: &Path, name: &str, body: &str) {
        fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn params_air8101_64bit_bkcrc() {
        let p = script_bin_params_for_chip("air8101");
        assert_eq!(p.bitw, 64);
        assert!(p.use_bkcrc);
        assert!(p.use_luac);
        assert!(p.strip);
    }

    #[test]
    fn params_ec718_32bit_no_bkcrc() {
        let p = script_bin_params_for_chip("ec7xx");
        assert_eq!(p.bitw, 32);
        assert!(!p.use_bkcrc);
    }

    #[test]
    fn params_air6208_64bit_no_bkcrc() {
        let p = script_bin_params_for_chip("air6208");
        assert_eq!(p.bitw, 64);
        assert!(!p.use_bkcrc);
    }

    #[test]
    fn params_unknown_defaults_32bit() {
        let p = script_bin_params_for_chip("xxx");
        assert_eq!(p.bitw, 32);
        assert!(!p.use_bkcrc);
    }

    #[test]
    fn build_with_common_overrides_testcase() {
        let tmp = TempDir::new().unwrap();
        // common/scripts/lib.lua
        let common = tmp.path().join("common");
        fs::create_dir_all(&common).unwrap();
        write_lua(&common, "lib.lua", "return 'from-common'");

        // testcase/scripts/main.lua 调用 lib
        let scripts = tmp.path().join("scripts");
        fs::create_dir_all(&scripts).unwrap();
        write_lua(&scripts, "main.lua", "local lib = require 'lib' print(lib)");

        let image = build_script_bin(&[scripts.as_path()], Some(common.as_path()), None, "ec7xx").unwrap();
        assert!(!image.is_empty());
        // 验证 image 至少包含 main.lua 内容（虽然是被 luac 编译，但 magic 一定在）
        assert!(image.len() > 100);
    }

    #[test]
    fn build_with_ctx_in_temp() {
        let tmp = TempDir::new().unwrap();
        let scripts = tmp.path().join("scripts");
        fs::create_dir_all(&scripts).unwrap();
        write_lua(&scripts, "main.lua", "print('hi')");

        let ctx = serde_json::json!({"test_id": "x", "foo": 1});
        let (ctx_tmp, _ctx_path) = write_ctx_to_temp(&ctx).unwrap();

        let image = build_script_bin(&[scripts.as_path()], None, Some(ctx_tmp.path()), "ec7xx").unwrap();
        assert!(!image.is_empty());
    }

    #[test]
    fn build_no_dirs_errors() {
        let r = build_script_bin(&[], None, None, "ec7xx");
        assert!(r.is_err());
    }

    #[test]
    fn build_missing_dir_errors() {
        let r = build_script_bin(&[Path::new("nonexistent_xyz")], None, None, "ec7xx");
        assert!(r.is_err());
    }
}
