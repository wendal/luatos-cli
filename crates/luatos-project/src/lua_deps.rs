//! Lua script dependency analysis.
//!
//! Parses `require("module")` calls from Lua source files to build a dependency
//! graph. This allows filtering out unused scripts that are not reachable from
//! the entry point (`main.lua`).
//!
//! # Supported patterns
//!
//! - `require("module")` / `require('module')`
//! - `require "module"` / `require 'module'`
//! - Ignores commented-out requires (`-- require(...)`)
//!
//! # LuatOS-specific behavior
//!
//! Many `require()` calls reference firmware-built-in modules (e.g., `sys`,
//! `log`, `uart`, `gpio`). These are **not** local file dependencies and are
//! treated as external/builtin modules.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use walkdir::WalkDir;

/// A dependency graph for Lua scripts.
#[derive(Debug, Clone)]
pub struct DependencyGraph {
    /// All discovered script files: filename → full path.
    pub files: BTreeMap<String, PathBuf>,
    /// Dependencies: filename → set of required module names.
    pub deps: BTreeMap<String, Vec<String>>,
    /// Files reachable from the entry point (typically `main.lua`).
    pub reachable: BTreeSet<String>,
    /// Modules referenced by require() that are not found locally
    /// (assumed to be firmware builtins).
    pub external_modules: BTreeSet<String>,
}

/// Well-known LuatOS firmware builtin modules.
/// These are provided by the C runtime, not as `.lua` files.
const BUILTIN_MODULES: &[&str] = &[
    // Lua 标准库
    "os",
    "io",
    "string",
    "table",
    "math",
    "coroutine",
    "debug",
    "bit32",
    "utf8",
    // LuatOS 核心
    "sys",
    "log",
    "rtos",
    "pack",
    "zbuff",
    "hmeta",
    // 外设
    "uart",
    "gpio",
    "spi",
    "i2c",
    "adc",
    "pwm",
    "timer",
    "wdt",
    "rtc",
    "pin",
    "pins",
    "touchkey",
    "keyboard",
    "ir",
    "tp",
    // 网络
    "wlan",
    "socket",
    "http",
    "mqtt",
    "websocket",
    "ftp",
    "ntp",
    "httpsrv",
    "mobile",
    "sms",
    "netdrv",
    "bluetooth",
    "ble",
    "nimble",
    "lora",
    "lora2",
    "airlink",
    "voip",
    // 安全
    "crypto",
    "iotauth",
    "gmssl",
    "xxtea",
    // 存储
    "fskv",
    "fs",
    "fatfs",
    "sdio",
    "otp",
    // 显示
    "disp",
    "u8g2",
    "lvgl",
    "lcd",
    "eink",
    "lcdseg",
    "ht1621",
    "airui",
    // 多媒体
    "audio",
    "codec",
    "record",
    "multimedia",
    "camera",
    // 编解码 & 压缩
    "json",
    "protobuf",
    "iconv",
    "miniz",
    "fastlz",
    "ymodem",
    // 传感器 & 电源
    "sensor",
    "max30102",
    "pm",
    // 其他
    "w5500",
    "libcoap",
    "ercoap",
    "libgnss",
    "statem",
    "bit64",
    "errDump",
];

fn is_builtin_module(name: &str) -> bool {
    BUILTIN_MODULES.contains(&name)
}

/// Extract `require("module")` calls from Lua source code.
///
/// Returns a list of module names referenced by require statements.
pub fn extract_requires(source: &str) -> Vec<String> {
    let mut requires = Vec::new();

    for line in source.lines() {
        let trimmed = line.trim();

        // Skip single-line comments
        if trimmed.starts_with("--") {
            continue;
        }

        // Remove inline comments before processing
        let code = if let Some(pos) = find_comment_start(trimmed) { &trimmed[..pos] } else { trimmed };

        // Find all require calls in the line
        let mut search_from = 0;
        while let Some(pos) = code[search_from..].find("require") {
            let abs_pos = search_from + pos;
            let after_require = &code[abs_pos + 7..];
            let after = after_require.trim_start();

            if let Some(module) = extract_module_name(after) {
                if !module.is_empty() {
                    requires.push(module);
                }
            }
            search_from = abs_pos + 7;
        }
    }

    requires
}

/// Find the start of a `--` comment, accounting for strings.
fn find_comment_start(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut in_string: Option<u8> = None;

    while i < bytes.len() {
        let ch = bytes[i];
        match in_string {
            Some(quote) => {
                if ch == b'\\' {
                    i += 1; // skip escaped char
                } else if ch == quote {
                    in_string = None;
                }
            }
            None => {
                if ch == b'"' || ch == b'\'' {
                    in_string = Some(ch);
                } else if ch == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'-' {
                    return Some(i);
                }
            }
        }
        i += 1;
    }
    None
}

/// Extract module name from text after "require".
/// Handles: `("module")`, `('module')`, `"module"`, `'module'`
fn extract_module_name(after: &str) -> Option<String> {
    let after = after.trim_start();
    if let Some(inner) = after.strip_prefix('(') {
        // require("module") or require('module')
        let inner = inner.trim_start();
        extract_quoted_string(inner)
    } else if after.starts_with('"') || after.starts_with('\'') {
        // require "module" or require 'module'
        extract_quoted_string(after)
    } else {
        None
    }
}

/// Extract a quoted string value.
fn extract_quoted_string(s: &str) -> Option<String> {
    let quote = s.as_bytes().first()?;
    if *quote != b'"' && *quote != b'\'' {
        return None;
    }
    let end = s[1..].find(*quote as char)?;
    Some(s[1..1 + end].to_string())
}

/// 版本控制目录名称，遍历文件时需跳过
const VCS_DIRS: &[&str] = &[".git", ".svn", ".hg"];

fn is_vcs_dir(name: &std::ffi::OsStr) -> bool {
    let s = name.to_string_lossy();
    VCS_DIRS.iter().any(|d| s.eq_ignore_ascii_case(d))
}

/// Collect all script files from directories and individual file paths.
///
/// Returns a map of filename → full path. For directories, all `.lua` files
/// are collected recursively. For individual files, they are added directly.
/// 自动跳过 .git/.svn/.hg 等版本控制目录。
pub fn collect_script_files(script_dirs: &[String], script_files: &[String]) -> Result<HashMap<String, PathBuf>> {
    let mut files = HashMap::new();

    // Collect from directories
    for dir_str in script_dirs {
        let dir = Path::new(dir_str);
        if !dir.exists() {
            log::warn!("Script directory not found: {}", dir.display());
            continue;
        }
        if dir.is_file() {
            // If a "directory" is actually a file, treat it as a file
            if let Some(name) = dir.file_name() {
                files.insert(name.to_string_lossy().into_owned(), dir.to_path_buf());
            }
            continue;
        }
        for entry in WalkDir::new(dir).into_iter().filter_entry(|e| !e.file_type().is_dir() || !is_vcs_dir(e.file_name())) {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }
            let filename = entry.path().file_name().context("entry has no filename")?.to_string_lossy().into_owned();
            files.insert(filename, entry.path().to_path_buf());
        }
    }

    // Collect individual files
    for file_str in script_files {
        let path = Path::new(file_str);
        if !path.exists() {
            log::warn!("Script file not found: {}", path.display());
            continue;
        }
        if let Some(name) = path.file_name() {
            files.insert(name.to_string_lossy().into_owned(), path.to_path_buf());
        }
    }

    Ok(files)
}

/// Analyze dependencies starting from `main.lua` and build a dependency graph.
///
/// Walks the dependency tree from the entry point, resolving `require()` calls
/// to local files. Modules that don't resolve to local files are recorded as
/// external (assumed to be firmware builtins).
pub fn analyze_deps(script_dirs: &[String], script_files: &[String]) -> Result<DependencyGraph> {
    let files = collect_script_files(script_dirs, script_files)?;

    // Build a module-name → filename lookup
    // A module "foo" can be resolved to "foo.lua"
    let mut module_to_file: HashMap<String, String> = HashMap::new();
    for filename in files.keys() {
        if let Some(stem) = Path::new(filename).file_stem() {
            let module_name = stem.to_string_lossy().into_owned();
            module_to_file.insert(module_name, filename.clone());
        }
        // Also register by full filename for `require("foo.lua")` patterns
        module_to_file.insert(filename.clone(), filename.clone());
    }

    // Parse all Lua files for dependencies
    let mut deps: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (filename, path) in &files {
        if filename.ends_with(".lua") || filename.ends_with(".luac") {
            if filename.ends_with(".lua") {
                let source = fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
                let requires = extract_requires(&source);
                deps.insert(filename.clone(), requires);
            } else {
                deps.insert(filename.clone(), Vec::new());
            }
        }
    }

    // BFS from main.lua to find reachable files
    let mut reachable = BTreeSet::new();
    let mut external_modules = BTreeSet::new();
    let mut queue = Vec::new();

    // Find entry point
    let entry = if files.contains_key("main.lua") {
        "main.lua".to_string()
    } else if files.contains_key("main.luac") {
        "main.luac".to_string()
    } else {
        // No main.lua found; mark all files as reachable
        log::warn!("No main.lua found; all files treated as reachable");
        let all_files: BTreeSet<String> = files.keys().cloned().collect();
        return Ok(DependencyGraph {
            files: files.into_iter().collect(),
            deps,
            reachable: all_files,
            external_modules,
        });
    };

    queue.push(entry.clone());
    reachable.insert(entry);

    while let Some(current) = queue.pop() {
        if let Some(required_modules) = deps.get(&current) {
            for module in required_modules {
                // Try to resolve the module to a local file
                if let Some(filename) = module_to_file.get(module) {
                    if reachable.insert(filename.clone()) {
                        queue.push(filename.clone());
                    }
                } else if is_builtin_module(module) {
                    external_modules.insert(module.clone());
                } else {
                    // Unknown module — could be a dynamic require or typo
                    external_modules.insert(module.clone());
                    log::debug!("Module '{}' required by '{}' not found locally", module, current);
                }
            }
        }
    }

    // Include all non-Lua files as reachable (data files, configs, etc.)
    for filename in files.keys() {
        if !filename.ends_with(".lua") && !filename.ends_with(".luac") {
            reachable.insert(filename.clone());
        }
    }

    Ok(DependencyGraph {
        files: files.into_iter().collect(),
        deps,
        reachable,
        external_modules,
    })
}

/// Filter a list of files to only include those reachable from main.lua.
///
/// When `ignore_deps` is true, all files are returned unchanged.
pub fn filter_by_deps(graph: &DependencyGraph, ignore_deps: bool) -> Vec<String> {
    if ignore_deps {
        graph.files.keys().cloned().collect()
    } else {
        graph.reachable.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn extract_basic_require() {
        let source = r#"
local sys = require("sys")
local log = require('log')
"#;
        let reqs = extract_requires(source);
        assert_eq!(reqs, vec!["sys", "log"]);
    }

    #[test]
    fn extract_require_no_parens() {
        let source = r#"
local sys = require "sys"
local log = require 'log'
"#;
        let reqs = extract_requires(source);
        assert_eq!(reqs, vec!["sys", "log"]);
    }

    #[test]
    fn skip_commented_require() {
        let source = r#"
-- local sys = require("sys")
local log = require("log")
"#;
        let reqs = extract_requires(source);
        assert_eq!(reqs, vec!["log"]);
    }

    #[test]
    fn skip_inline_comment_require() {
        let source = r#"
local x = 1 -- require("hidden")
local log = require("log")
"#;
        let reqs = extract_requires(source);
        assert_eq!(reqs, vec!["log"]);
    }

    #[test]
    fn multiple_requires_on_one_line() {
        let source = r#"local a,b = require("foo"), require("bar")"#;
        let reqs = extract_requires(source);
        assert_eq!(reqs, vec!["foo", "bar"]);
    }

    #[test]
    fn analyze_simple_deps() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("main.lua"),
            r#"
local sys = require("sys")
local helper = require("helper")
sys.run()
"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("helper.lua"),
            r#"
local M = {}
return M
"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("unused.lua"),
            r#"
-- this file is not required by anything
print("unused")
"#,
        )
        .unwrap();

        let dirs = vec![dir.path().to_string_lossy().into_owned()];
        let graph = analyze_deps(&dirs, &[]).unwrap();

        assert!(graph.reachable.contains("main.lua"));
        assert!(graph.reachable.contains("helper.lua"));
        assert!(!graph.reachable.contains("unused.lua"));
        assert!(graph.external_modules.contains("sys"));
    }

    #[test]
    fn analyze_transitive_deps() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("main.lua"), r#"local a = require("a")"#).unwrap();
        fs::write(dir.path().join("a.lua"), r#"local b = require("b"); return {}"#).unwrap();
        fs::write(dir.path().join("b.lua"), r#"return {}"#).unwrap();
        fs::write(dir.path().join("c.lua"), r#"return {}"#).unwrap();

        let dirs = vec![dir.path().to_string_lossy().into_owned()];
        let graph = analyze_deps(&dirs, &[]).unwrap();

        assert!(graph.reachable.contains("main.lua"));
        assert!(graph.reachable.contains("a.lua"));
        assert!(graph.reachable.contains("b.lua"));
        assert!(!graph.reachable.contains("c.lua"));
    }

    #[test]
    fn filter_with_ignore_deps() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("main.lua"), r#"print("hi")"#).unwrap();
        fs::write(dir.path().join("extra.lua"), r#"print("extra")"#).unwrap();

        let dirs = vec![dir.path().to_string_lossy().into_owned()];
        let graph = analyze_deps(&dirs, &[]).unwrap();

        let filtered = filter_by_deps(&graph, false);
        assert!(filtered.contains(&"main.lua".to_string()));
        assert!(!filtered.contains(&"extra.lua".to_string()));

        let all = filter_by_deps(&graph, true);
        assert!(all.contains(&"main.lua".to_string()));
        assert!(all.contains(&"extra.lua".to_string()));
    }

    #[test]
    fn non_lua_files_always_reachable() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("main.lua"), r#"print("hi")"#).unwrap();
        fs::write(dir.path().join("config.json"), r#"{}"#).unwrap();
        fs::write(dir.path().join("data.bin"), &[0u8; 10]).unwrap();

        let dirs = vec![dir.path().to_string_lossy().into_owned()];
        let graph = analyze_deps(&dirs, &[]).unwrap();

        assert!(graph.reachable.contains("main.lua"));
        assert!(graph.reachable.contains("config.json"));
        assert!(graph.reachable.contains("data.bin"));
    }

    #[test]
    fn builtin_modules_detected() {
        assert!(is_builtin_module("sys"));
        assert!(is_builtin_module("gpio"));
        assert!(is_builtin_module("uart"));
        assert!(!is_builtin_module("my_custom_lib"));
    }

    #[test]
    fn collect_files_from_dirs_and_files() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("main.lua"), b"print('hello')").unwrap();

        let extra_dir = TempDir::new().unwrap();
        fs::write(extra_dir.path().join("single.lua"), b"return 1").unwrap();

        let dirs = vec![dir.path().to_string_lossy().into_owned()];
        let files_list = vec![extra_dir.path().join("single.lua").to_string_lossy().into_owned()];

        let collected = collect_script_files(&dirs, &files_list).unwrap();
        assert!(collected.contains_key("main.lua"));
        assert!(collected.contains_key("single.lua"));
    }
}
