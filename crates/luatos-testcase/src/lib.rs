//! LuatOS testcase 工具库
//!
//! 提供 testcase 目录发现、`metas.json` 解析、`ctx.json` 合并、
//! `script.bin` 构建等可独立测试的能力。
//!
//! ## 模块
//!
//! - [`metas`] — `metas.json` 解析与校验
//! - [`discovery`] — testcase 路径解析（路径 / 名称递归匹配）
//! - [`ctx`] — `ctx.json` 多档合并（深合并 / 完全覆盖）
//! - [`lua_bin`] — `script.bin` 构建（多源合并 + 64bit 检测）

#![allow(clippy::needless_return)]

pub mod ctx;
pub mod discovery;
pub mod lua_bin;
pub mod metas;

pub use ctx::{build_ctx, inject_identifiers, merge_deep, CtxBuildResult, CtxMergeWarning};
pub use discovery::{resolve_testcase, scan_testcases, DiscoverySource, ResolvedTestcase};
pub use lua_bin::{build_script_bin, script_bin_params_for_chip, write_ctx_to_temp, ScriptBinParams};
pub use metas::MetasFile;
