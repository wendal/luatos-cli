//! testcase 路径解析
//!
//! 支持三种输入：
//! 1. 显式路径（必须是包含 `metas.json` + `scripts/main.lua` 的目录）
//! 2. 名称（先查 `<root>/testcase/<name>/`，再递归 `<root>/testcase/**/<name>/`）
//! 3. 多名同名 testcase 命中时取字典序最小（warning 提示）

use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use walkdir::WalkDir;

use crate::metas::MetasFile;

/// testcase 解析来源，便于日志/调试
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoverySource {
    /// 用户显式传入了完整路径
    ExplicitPath,
    /// `<root>/testcase/<name>/` 顶层直接命中
    LocalTopLevel,
    /// `<root>/testcase/<sub>/<name>/` 子目录命中
    NestedSubdir,
}

impl DiscoverySource {
    pub fn as_str(&self) -> &'static str {
        match self {
            DiscoverySource::ExplicitPath => "explicit",
            DiscoverySource::LocalTopLevel => "local_top",
            DiscoverySource::NestedSubdir => "nested",
        }
    }
}

/// 解析后的 testcase
#[derive(Debug, Clone)]
pub struct ResolvedTestcase {
    /// testcase 根目录
    pub path: PathBuf,
    /// testcase 名称（目录名）
    pub name: String,
    /// 父目录名（顶层时为空字符串）
    pub category: String,
    /// metas.json 解析结果
    pub metas: MetasFile,
    /// scripts 目录
    pub scripts_dir: PathBuf,
    /// main.lua 路径
    pub main_lua: PathBuf,
    /// 解析来源
    pub discovery_source: DiscoverySource,
}

/// 解析 testcase 路径或名称
///
/// - `arg` 是目录：尝试作为显式路径（包含 metas.json + scripts/main.lua）
/// - `arg` 是文件：报错
/// - `arg` 是纯名称：先查 `<root>/testcase/<arg>/`，再递归 `<root>/testcase/**/<arg>/`
pub fn resolve_testcase(arg: &str, luatos_root: &Path) -> Result<ResolvedTestcase> {
    let arg_path = Path::new(arg);
    if arg_path.is_dir() {
        return build_resolved(arg_path, testcase_root_or_none(luatos_root, arg_path).as_deref(), DiscoverySource::ExplicitPath);
    }
    if arg_path.is_file() {
        bail!("testcase 路径不能是文件: {}", arg);
    }

    let testcase_root = luatos_root.join("testcase");
    if !testcase_root.is_dir() {
        bail!("未找到 LuatOS testcase 目录: {}（请确认 --luatos-root 指向 LuatOS 仓库根）", testcase_root.display());
    }

    // 先查顶层
    let top = testcase_root.join(arg);
    if top.is_dir() {
        return build_resolved(&top, Some(&testcase_root), DiscoverySource::LocalTopLevel);
    }

    // 递归查 subdir/<name>
    let mut matches: Vec<PathBuf> = Vec::new();
    for entry in WalkDir::new(&testcase_root).min_depth(2).max_depth(8) {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_dir() {
            continue;
        }
        if entry.file_name() == arg {
            matches.push(entry.into_path());
        }
    }
    if matches.is_empty() {
        bail!("未找到 testcase: {}（在 {} 下）", arg, testcase_root.display());
    }

    // 按相对路径字典序取第一个
    matches.sort_by(|a, b| {
        let ra = a.strip_prefix(&testcase_root).unwrap_or(a);
        let rb = b.strip_prefix(&testcase_root).unwrap_or(b);
        ra.cmp(rb)
    });

    if matches.len() > 1 {
        log::warn!("testcase '{}' 有 {} 个命中，取第一个: {}", arg, matches.len(), matches[0].display());
    }

    build_resolved(&matches[0], Some(&testcase_root), DiscoverySource::NestedSubdir)
}

/// 扫描 `<root>/testcase/` 下所有 testcase 目录（深度 <= 8，含顶层）
pub fn scan_testcases(luatos_root: &Path) -> Result<Vec<ResolvedTestcase>> {
    let testcase_root = luatos_root.join("testcase");
    if !testcase_root.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in WalkDir::new(&testcase_root).min_depth(1).max_depth(8) {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_dir() {
            continue;
        }
        // 只在含 metas.json 的目录停下
        if !entry.path().join("metas.json").is_file() {
            continue;
        }
        let source = if entry.path().parent() == Some(testcase_root.as_path()) {
            DiscoverySource::LocalTopLevel
        } else {
            DiscoverySource::NestedSubdir
        };
        match build_resolved(entry.path(), Some(&testcase_root), source) {
            Ok(r) => out.push(r),
            Err(e) => log::warn!("跳过 testcase {}: {:#}", entry.path().display(), e),
        }
    }
    // 按 category/name 字典序排序
    out.sort_by(|a, b| match a.category.cmp(&b.category) {
        Ordering::Equal => a.name.cmp(&b.name),
        other => other,
    });
    Ok(out)
}

/// 对 `arg_path` 计算 testcase_root（如果它就是 testcase 根下的路径）
///
/// 用于确定显式路径是否是顶层 testcase（不传 None 表示不关心）。
fn testcase_root_or_none(luatos_root: &Path, _arg_path: &Path) -> Option<PathBuf> {
    Some(luatos_root.join("testcase"))
}

fn build_resolved(path: &Path, testcase_root: Option<&Path>, source: DiscoverySource) -> Result<ResolvedTestcase> {
    if !path.join("metas.json").is_file() {
        bail!("testcase 目录缺少 metas.json: {}", path.display());
    }
    let scripts_dir = path.join("scripts");
    let main_lua = scripts_dir.join("main.lua");
    if !main_lua.is_file() {
        bail!("testcase 目录缺少 scripts/main.lua: {}", path.display());
    }

    let metas = MetasFile::load(path).with_context(|| format!("failed to load metas in {}", path.display()))?;

    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
    // category：父目录名。父目录 == testcase_root 时为空字符串。
    let category = match (path.parent(), testcase_root) {
        (Some(p), Some(root)) if p == root => String::new(),
        (Some(p), _) => p.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string(),
        _ => String::new(),
    };

    Ok(ResolvedTestcase {
        path: path.to_path_buf(),
        name,
        category,
        metas,
        scripts_dir,
        main_lua,
        discovery_source: source,
    })
}

/// 校验目录是否可作为 testcase
pub fn validate_testcase_dir(path: &Path) -> Result<()> {
    if !path.is_dir() {
        bail!("不是目录: {}", path.display());
    }
    if !path.join("metas.json").is_file() {
        bail!("缺少 metas.json: {}", path.display());
    }
    if !path.join("scripts").join("main.lua").is_file() {
        bail!("缺少 scripts/main.lua: {}", path.display());
    }
    Ok(())
}

// 防止警告（fs 模块在 build_resolved 中已经用 file_name，无需额外 fs 引用）
#[allow(dead_code)]
fn _unused() -> fs::DirEntry {
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// 在 tempdir 构造一个最小可用的 testcase 目录
    fn make_testcase(parent: &Path, name: &str) -> PathBuf {
        let dir = parent.join(name);
        fs::create_dir_all(dir.join("scripts")).unwrap();
        fs::write(
            dir.join("metas.json"),
            r#"{"timeout": 30, "model": {"x": ["1"]}, "action_count": 1, "priority": 1, "description": "x"}"#,
        )
        .unwrap();
        fs::write(dir.join("scripts").join("main.lua"), "print('hi')").unwrap();
        dir
    }

    #[test]
    fn resolve_explicit_path() {
        let tmp = TempDir::new().unwrap();
        let tc = make_testcase(tmp.path(), "foo");
        let resolved = resolve_testcase(tc.to_str().unwrap(), tmp.path()).unwrap();
        assert_eq!(resolved.name, "foo");
        assert_eq!(resolved.discovery_source, DiscoverySource::ExplicitPath);
    }

    #[test]
    fn resolve_nested_name() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let luatos = root.join("LuatOS");
        let category = luatos.join("testcase").join("cat_a");
        fs::create_dir_all(&category).unwrap();
        make_testcase(&category, "exftp");

        let resolved = resolve_testcase("exftp", &luatos).unwrap();
        assert_eq!(resolved.name, "exftp");
        assert_eq!(resolved.category, "cat_a");
        assert_eq!(resolved.discovery_source, DiscoverySource::NestedSubdir);
    }

    #[test]
    fn resolve_local_top_level() {
        let tmp = TempDir::new().unwrap();
        let luatos = tmp.path().join("LuatOS");
        let top = luatos.join("testcase");
        fs::create_dir_all(&top).unwrap();
        make_testcase(&top, "bare");

        let resolved = resolve_testcase("bare", &luatos).unwrap();
        assert_eq!(resolved.name, "bare");
        assert_eq!(resolved.discovery_source, DiscoverySource::LocalTopLevel);
    }

    #[test]
    fn resolve_ambiguous_name_takes_first() {
        let tmp = TempDir::new().unwrap();
        let luatos = tmp.path().join("LuatOS");
        let cat_a = luatos.join("testcase").join("aaa");
        let cat_z = luatos.join("testcase").join("zzz");
        fs::create_dir_all(&cat_a).unwrap();
        fs::create_dir_all(&cat_z).unwrap();
        make_testcase(&cat_a, "dup");
        make_testcase(&cat_z, "dup");

        let resolved = resolve_testcase("dup", &luatos).unwrap();
        assert_eq!(resolved.category, "aaa"); // 字典序最小
    }

    #[test]
    fn resolve_missing_returns_err() {
        let tmp = TempDir::new().unwrap();
        let luatos = tmp.path().join("LuatOS");
        let top = luatos.join("testcase");
        fs::create_dir_all(&top).unwrap();
        make_testcase(&top, "exists");

        let err = resolve_testcase("nope", &luatos).unwrap_err();
        assert!(err.to_string().contains("nope"));
    }

    #[test]
    fn resolve_missing_testcase_dir_returns_err() {
        let tmp = TempDir::new().unwrap();
        let err = resolve_testcase("any", tmp.path()).unwrap_err();
        assert!(err.to_string().contains("testcase"));
    }

    #[test]
    fn resolve_file_arg_returns_err() {
        let tmp = TempDir::new().unwrap();
        let f = tmp.path().join("file.txt");
        fs::write(&f, "x").unwrap();
        let err = resolve_testcase(f.to_str().unwrap(), tmp.path()).unwrap_err();
        assert!(err.to_string().contains("不能是文件"));
    }

    #[test]
    fn explicit_path_missing_metas_fails() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("broken");
        fs::create_dir_all(dir.join("scripts")).unwrap();
        fs::write(dir.join("scripts").join("main.lua"), "x").unwrap();
        // 没有 metas.json
        let err = resolve_testcase(dir.to_str().unwrap(), tmp.path()).unwrap_err();
        assert!(err.to_string().contains("metas.json"));
    }

    #[test]
    fn explicit_path_missing_main_lua_fails() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("no_main");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("metas.json"),
            r#"{"timeout": 1, "model": {"x": ["1"]}, "action_count": 1, "priority": 1, "description": "x"}"#,
        )
        .unwrap();
        let err = resolve_testcase(dir.to_str().unwrap(), tmp.path()).unwrap_err();
        assert!(err.to_string().contains("main.lua"));
    }

    #[test]
    fn scan_testcases_finds_all() {
        let tmp = TempDir::new().unwrap();
        let luatos = tmp.path().join("LuatOS");
        let top = luatos.join("testcase");
        let cat = top.join("cat");
        fs::create_dir_all(&cat).unwrap();
        make_testcase(&top, "a");
        make_testcase(&top, "b");
        make_testcase(&cat, "c");
        // 一个无效目录（缺 metas）
        fs::create_dir_all(top.join("invalid")).unwrap();

        let found = scan_testcases(&luatos).unwrap();
        let names: Vec<_> = found.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"a"));
        assert!(names.contains(&"b"));
        assert!(names.contains(&"c"));
        assert_eq!(found.len(), 3);
    }

    #[test]
    fn ignores_process_directory() {
        // process/ 子目录（含 preprocess.py / midprocess.py）不再被识别
        let tmp = TempDir::new().unwrap();
        let tc = make_testcase(tmp.path(), "with_hooks");
        let process = tc.join("process");
        fs::create_dir_all(&process).unwrap();
        fs::write(process.join("preprocess.py"), "# preprocess").unwrap();
        fs::write(process.join("midprocess.py"), "# midprocess").unwrap();

        // 仍然能正常解析 testcase,process/ 目录被忽略
        let resolved = resolve_testcase(tc.to_str().unwrap(), tmp.path()).unwrap();
        assert_eq!(resolved.name, "with_hooks");
    }
}
