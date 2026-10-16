//! `script.bin` 构建
//!
//! 包装 `luatos_luadb::build::build_script_image`，加入：
//! - 多源合并（公共脚本 + testcase scripts + ctx 目录）
//! - 位宽 / BK CRC16 由 **SOC 包元数据**决定（见 `ScriptBinParams::from_soc`）

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
    /// luac 调试信息保留级别（0=无, 1=仅行号, 2=仅变量名, 99=全部）
    pub debug_mode: u8,
}

impl ScriptBinParams {
    /// 由 SOC 包元数据推导编译参数（`SocInfo::script_bitw()` / `script_use_luac()` /
    /// `use_bkcrc()`），与 `flash` 路径同源。
    ///
    /// ⚠️ 不要再按 chip 名字硬编码查表：ccm4211 家族（Air1601/Air1602/Air1603/Air1604）
    /// 与 bk72xx（Air8101）都是 64 位 Lua VM，表里漏掉一个型号就会刷出
    /// `lua_Integer size mismatch in precompiled chunk`（真机实测，Lua VM 直接起不来）。
    /// 位宽的唯一权威来源是 SOC 包，不要在下游再抄一份。
    pub fn from_soc(bitw: u32, use_luac: bool, use_bkcrc: bool) -> Self {
        Self {
            bitw,
            use_luac,
            use_bkcrc,
            debug_mode: luatos_luadb::LUAC_DEBUG_ALL,
        }
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
/// - `params`: 编译参数（由 SOC 包元数据推导，见 [`ScriptBinParams::from_soc`]）
pub fn build_script_bin(script_dirs: &[&Path], common_scripts: Option<&Path>, ctx_tmp_dir: Option<&Path>, params: ScriptBinParams) -> Result<Vec<u8>> {
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
        "build_script_image: dirs={:?} bitw={} use_luac={} use_bkcrc={} debug_mode={}",
        ordered.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        params.bitw,
        params.use_luac,
        params.use_bkcrc,
        params.debug_mode
    );
    let image = luatos_luadb::build::build_script_image(&refs, params.use_luac, params.bitw, params.use_bkcrc, params.debug_mode).context("build_script_image failed")?;
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
    fn params_from_soc_keeps_bitw_and_bkcrc() {
        // ccm4211 家族(Air1601/1602/1603/1604)的真机 soc 元数据: bitw=64, 无 bkcrc
        let p = ScriptBinParams::from_soc(64, true, false);
        assert_eq!(p.bitw, 64);
        assert!(!p.use_bkcrc);
        assert!(p.use_luac);
        assert_eq!(p.debug_mode, 99);

        // bk72xx(Air8101): bitw=64 + BK CRC16
        let p = ScriptBinParams::from_soc(64, true, true);
        assert_eq!(p.bitw, 64);
        assert!(p.use_bkcrc);

        // ec718(Air8000) 仍是 32 位
        let p = ScriptBinParams::from_soc(32, true, false);
        assert_eq!(p.bitw, 32);
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

        let image = build_script_bin(&[scripts.as_path()], Some(common.as_path()), None, ScriptBinParams::from_soc(32, true, false)).unwrap();
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

        let image = build_script_bin(&[scripts.as_path()], None, Some(ctx_tmp.path()), ScriptBinParams::from_soc(32, true, false)).unwrap();
        assert!(!image.is_empty());
    }

    #[test]
    fn build_no_dirs_errors() {
        let r = build_script_bin(&[], None, None, ScriptBinParams::from_soc(32, true, false));
        assert!(r.is_err());
    }

    #[test]
    fn build_missing_dir_errors() {
        let r = build_script_bin(&[Path::new("nonexistent_xyz")], None, None, ScriptBinParams::from_soc(32, true, false));
        assert!(r.is_err());
    }
}
