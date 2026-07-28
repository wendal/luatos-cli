//! `ctx.json` 多档合并
//!
//! 合并顺序（`--full-ctx` 短路）：
//! 1. `base = {}`（空对象）
//! 2. `<LuatOS>/testcase/local_ctx.json`（若存在）
//! 3. CLI `--ctx` 指定文件（若存在）
//! 4. CLI `--full-ctx` 指定文件（若指定，跳过 1-3，仅用此文件 + 注入 test_id）
//!
//! 数组语义：不 concat，直接覆盖。
//! 类型冲突（一 Object 一非 Object）：后者覆盖 + warn。

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Serialize;

/// 单次合并产生的告警（用于 CLI 透传给用户）
#[derive(Debug, Clone, Serialize)]
pub struct CtxMergeWarning {
    /// 发生冲突的 key 路径（点号分隔）
    pub key: String,
    /// 原始值类型描述
    pub old_kind: String,
    /// 新值类型描述
    pub new_kind: String,
}

/// ctx.json 构造结果
#[derive(Debug, Clone, Serialize)]
pub struct CtxBuildResult {
    /// 最终 ctx.json 内容
    pub value: serde_json::Value,
    /// 自动生成的 test_id
    pub test_id: String,
    /// runner_id
    pub runner_id: String,
    /// runner_mode（固定为 "cli-debug"）
    pub runner_mode: String,
    /// 监听端口（0 表示未启动）
    pub listen_port: u16,
    /// 合并过程产生的告警
    pub warnings: Vec<CtxMergeWarning>,
}

/// 深合并两个 JSON 对象
///
/// 规则：
/// - 都是 Object → 递归按 key 合并
/// - 一边 Object 一边非 Object → 后者覆盖 + 返回 warning
/// - 两边都是非 Object → 后者覆盖
/// - 数组不 concat（直接覆盖）
pub fn merge_deep(base: &serde_json::Value, overlay: &serde_json::Value) -> (serde_json::Value, Vec<CtxMergeWarning>) {
    let mut warnings = Vec::new();
    let out = merge_inner(base, overlay, "", &mut warnings);
    (out, warnings)
}

fn merge_inner(base: &serde_json::Value, overlay: &serde_json::Value, path: &str, warnings: &mut Vec<CtxMergeWarning>) -> serde_json::Value {
    use serde_json::Value;
    match (base, overlay) {
        (Value::Object(am), Value::Object(bm)) => {
            let mut out = am.clone();
            for (k, bv) in bm {
                let child_path = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
                if let Some(av) = out.get(k) {
                    out.insert(k.clone(), merge_inner(av, bv, &child_path, warnings));
                } else {
                    out.insert(k.clone(), bv.clone());
                }
            }
            Value::Object(out)
        }
        (Value::Null, _) => overlay.clone(),
        (_, Value::Null) => overlay.clone(),
        (a, b) => {
            let ak = type_kind(a);
            let bk = type_kind(b);
            if ak != bk && !path.is_empty() {
                warnings.push(CtxMergeWarning {
                    key: path.to_string(),
                    old_kind: kind_str(ak).to_string(),
                    new_kind: kind_str(bk).to_string(),
                });
            }
            b.clone()
        }
    }
}

fn _json_type_unused() {}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Kind {
    Null,
    Bool,
    Number,
    String,
    Array,
    Object,
}

fn kind_str(k: Kind) -> &'static str {
    match k {
        Kind::Null => "null",
        Kind::Bool => "bool",
        Kind::Number => "number",
        Kind::String => "string",
        Kind::Array => "array",
        Kind::Object => "object",
    }
}

fn type_kind(v: &serde_json::Value) -> Kind {
    use serde_json::Value;
    match v {
        Value::Null => Kind::Null,
        Value::Bool(_) => Kind::Bool,
        Value::Number(_) => Kind::Number,
        Value::String(_) => Kind::String,
        Value::Array(_) => Kind::Array,
        Value::Object(_) => Kind::Object,
    }
}

/// 构造最终 ctx.json
///
/// - `luatos_root`: 用于查找 `<root>/testcase/local_ctx.json`（若 `local_ctx_path` 为 None）
/// - `local_ctx_path`: 显式指定 local_ctx.json 路径（优先级高于 luatos_root）
/// - `ctx_path`: CLI `--ctx` 文件
/// - `full_ctx_path`: CLI `--full-ctx` 文件，若 Some 则跳过其它合并
/// - `listen_port`: 0 表示未启用监听
pub fn build_ctx(luatos_root: &Path, local_ctx_path: Option<&Path>, ctx_path: Option<&Path>, full_ctx_path: Option<&Path>, listen_port: u16) -> Result<CtxBuildResult> {
    let (test_id, runner_id) = gen_identifiers();
    let runner_mode = "cli-debug".to_string();

    // --full-ctx 短路
    if let Some(p) = full_ctx_path {
        let mut value = read_json_file(p)?;
        ensure_object(&mut value, p)?;
        // 优先使用用户传入的 test_id/runner_id/runner_mode，未提供再回退到自动生成
        let test_id = value.get("test_id").and_then(|v| v.as_str()).map(|s| s.to_string()).unwrap_or_else(|| gen_identifiers().0);
        let runner_id = value
            .get("runner_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| gen_identifiers().1);
        let runner_mode = value
            .get("runner_mode")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "cli-debug".to_string());
        return Ok(CtxBuildResult {
            value,
            test_id,
            runner_id,
            runner_mode,
            listen_port,
            warnings: Vec::new(),
        });
    }

    // 基础：空对象
    let mut value = serde_json::Value::Object(Default::default());
    let mut warnings = Vec::new();

    // 第二层：local_ctx.json
    let local_path: Option<PathBuf> = local_ctx_path.map(|p| p.to_path_buf()).or_else(|| {
        let p = luatos_root.join("testcase").join("local_ctx.json");
        if p.is_file() {
            Some(p)
        } else {
            None
        }
    });
    if let Some(p) = local_path {
        let overlay = read_json_file(&p)?;
        let (merged, mut w) = merge_deep(&value, &overlay);
        value = merged;
        warnings.append(&mut w);
    }

    // 第三层：--ctx
    if let Some(p) = ctx_path {
        let overlay = read_json_file(p)?;
        let (merged, mut w) = merge_deep(&value, &overlay);
        value = merged;
        warnings.append(&mut w);
    }

    Ok(CtxBuildResult {
        value,
        test_id,
        runner_id,
        runner_mode,
        listen_port,
        warnings,
    })
}

/// 把 build_ctx 的结果注入 test_id/runner_id/runner_mode 字段
pub fn inject_identifiers(result: &mut CtxBuildResult, listen_url: Option<(&str, &str)>) {
    use serde_json::json;
    if let serde_json::Value::Object(map) = &mut result.value {
        map.insert("test_id".to_string(), json!(result.test_id));
        map.insert("runner_id".to_string(), json!(result.runner_id));
        map.insert("runner_mode".to_string(), json!(result.runner_mode));
        if let Some((report, status)) = listen_url {
            map.insert("report_url".to_string(), json!(report));
            map.insert("status_url".to_string(), json!(status));
        }
    }
}

fn read_json_file(path: &Path) -> Result<serde_json::Value> {
    let content = fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let v: serde_json::Value = serde_json::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(v)
}

fn ensure_object(v: &mut serde_json::Value, path: &Path) -> Result<()> {
    if !v.is_object() {
        bail!("{} 不是 JSON 对象", path.display());
    }
    Ok(())
}

fn gen_identifiers() -> (String, String) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    // 4 字节随机 hex
    let random_hex: String = {
        let mut buf = [0u8; 2];
        // 标准库方式生成随机数（确定性需求时可替换）
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
        buf[0] = (nanos & 0xff) as u8;
        buf[1] = ((nanos >> 8) & 0xff) as u8;
        format!("{:02x}{:02x}", buf[0], buf[1])
    };

    // unix_secs 用 base36 缩短（保持可读性）
    let secs_b36 = base36_encode(now);
    let test_id = format!("test_{}_{}", secs_b36, random_hex);

    let runner_id = match std::env::var("LUATOS_RUNNER_ID") {
        Ok(s) if !s.is_empty() => s,
        _ => {
            let hostname = hostname();
            let pid = std::process::id();
            format!("cli-{}-{}", hostname, pid)
        }
    };
    (test_id, runner_id)
}

fn base36_encode(mut n: u64) -> String {
    const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".to_string();
    }
    let mut out = Vec::new();
    while n > 0 {
        out.push(ALPHABET[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn write_json(dir: &Path, name: &str, v: serde_json::Value) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, serde_json::to_string_pretty(&v).unwrap()).unwrap();
        p
    }

    #[test]
    fn merge_disjoint() {
        let a = json!({"a": 1});
        let b = json!({"b": 2});
        let (out, warnings) = merge_deep(&a, &b);
        assert_eq!(out, json!({"a": 1, "b": 2}));
        assert!(warnings.is_empty());
    }

    #[test]
    fn merge_nested() {
        let a = json!({"mqtt": {"broker": "a", "port": 1}});
        let b = json!({"mqtt": {"broker": "b"}});
        let (out, _) = merge_deep(&a, &b);
        assert_eq!(out, json!({"mqtt": {"broker": "b", "port": 1}}));
    }

    #[test]
    fn merge_scalar_override() {
        let a = json!({"x": {"a": 1}});
        let b = json!({"x": "scalar"});
        let (out, warnings) = merge_deep(&a, &b);
        assert_eq!(out, json!({"x": "scalar"}));
        // 类型冲突应产生 warning
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].key, "x");
        assert_eq!(warnings[0].old_kind, "object");
        assert_eq!(warnings[0].new_kind, "string");
    }

    #[test]
    fn merge_array_override_not_concat() {
        let a = json!({"tags": ["a", "b"]});
        let b = json!({"tags": ["c"]});
        let (out, _) = merge_deep(&a, &b);
        assert_eq!(out, json!({"tags": ["c"]}));
    }

    #[test]
    fn merge_null_overlay_drops() {
        let a = json!({"x": 1});
        let b = json!({"x": null});
        let (out, _) = merge_deep(&a, &b);
        // overlay 是 null 时取 overlay（语义为删除该 key 不太合理；用 null 表示删除容易引歧义）
        // 当前实现：null overlay 替换为 null（保留 key）
        assert_eq!(out, json!({"x": null}));
    }

    #[test]
    fn full_ctx_skips_merge() {
        let tmp = TempDir::new().unwrap();
        let p = write_json(tmp.path(), "full.json", json!({"only": "value"}));
        let r = build_ctx(tmp.path(), None, None, Some(&p), 0).unwrap();
        assert_eq!(r.value, json!({"only": "value"}));
        assert!(!r.test_id.is_empty());
    }

    #[test]
    fn full_ctx_must_be_object() {
        let tmp = TempDir::new().unwrap();
        let p = write_json(tmp.path(), "bad.json", json!("not an object"));
        let r = build_ctx(tmp.path(), None, None, Some(&p), 0);
        assert!(r.is_err());
    }

    #[test]
    fn build_ctx_merges_local_and_cli() {
        let tmp = TempDir::new().unwrap();
        let local = write_json(tmp.path(), "local_ctx.json", json!({"report_url": "from-local", "extra": 1}));
        let cli = write_json(tmp.path(), "cli_ctx.json", json!({"report_url": "from-cli", "runner": "x"}));
        let r = build_ctx(tmp.path(), Some(&local), Some(&cli), None, 0).unwrap();
        assert_eq!(r.value["report_url"], json!("from-cli"));
        assert_eq!(r.value["extra"], json!(1));
        assert_eq!(r.value["runner"], json!("x"));
    }

    #[test]
    fn build_ctx_local_defaults_to_luatos_testcase() {
        let tmp = TempDir::new().unwrap();
        let luatos = tmp.path().join("LuatOS");
        let tc = luatos.join("testcase");
        fs::create_dir_all(&tc).unwrap();
        let p = write_json(&tc, "local_ctx.json", json!({"from": "default-local"}));
        let r = build_ctx(&luatos, None, None, None, 0).unwrap();
        assert_eq!(r.value["from"], json!("default-local"));
        // sanity: 也能找到文件
        assert!(p.is_file());
    }

    #[test]
    fn build_ctx_no_local_no_cli() {
        let tmp = TempDir::new().unwrap();
        let r = build_ctx(tmp.path(), None, None, None, 0).unwrap();
        assert_eq!(r.value, json!({}));
    }

    #[test]
    fn test_id_format() {
        let (id, _runner) = gen_identifiers();
        assert!(id.starts_with("test_"));
        let rest = &id[5..];
        let parts: Vec<_> = rest.splitn(2, '_').collect();
        assert_eq!(parts.len(), 2);
        // base36 字符
        for c in parts[0].chars() {
            assert!(c.is_ascii_alphanumeric());
        }
        // random hex
        assert_eq!(parts[1].len(), 4);
    }

    #[test]
    fn inject_identifiers_adds_keys() {
        let mut r = CtxBuildResult {
            value: json!({"a": 1}),
            test_id: "test_x_y".into(),
            runner_id: "cli-h-1".into(),
            runner_mode: "cli-debug".into(),
            listen_port: 1234,
            warnings: Vec::new(),
        };
        inject_identifiers(&mut r, Some(("http://127.0.0.1:1234/result", "http://127.0.0.1:1234/status")));
        assert_eq!(r.value["test_id"], json!("test_x_y"));
        assert_eq!(r.value["runner_id"], json!("cli-h-1"));
        assert_eq!(r.value["runner_mode"], json!("cli-debug"));
        assert_eq!(r.value["report_url"], json!("http://127.0.0.1:1234/result"));
        assert_eq!(r.value["status_url"], json!("http://127.0.0.1:1234/status"));
    }

    #[test]
    fn base36_zero() {
        assert_eq!(base36_encode(0), "0");
        assert_eq!(base36_encode(35), "z");
        assert_eq!(base36_encode(36), "10");
    }
}
