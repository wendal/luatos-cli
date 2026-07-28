//! `metas.json` 解析与校验
//!
//! 描述单个 testcase 的元信息：超时、支持的型号、关键字、钩子等。

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// `metas.json` 的反序列化结构
///
/// 字段定义参考 luatos-autotest-v2 的使用习惯，必填字段缺失会校验失败。
/// 详见：[`MetasFile::load`]。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetasFile {
    /// testcase 超时时间（秒）
    pub timeout: u64,
    /// 支持的型号 -> SIM 卡 ID 列表。
    ///
    /// 元素接受任意 JSON 值（字符串或整数），与 luatos-autotest-v2
    /// 产线约定一致：常见写法 `"air8101": [1, 101]`，历史 fixture
    /// 也有 `"air8101": ["1"]` 字符串写法。
    pub model: BTreeMap<String, Vec<serde_json::Value>>,
    /// 动作次数（设备端 SDK 用）
    pub action_count: u32,
    /// 调度优先级（数字越小优先级越高）
    pub priority: u32,
    /// 人类可读描述
    pub description: String,
    /// 刷机前是否需要先刷底层固件（可选）
    #[serde(default)]
    pub flush_core: Option<String>,
    /// 内置 PASS 关键字（与 CLI --keyword 合并）
    #[serde(default)]
    pub keywords: Option<Vec<String>>,
    /// 内置 FAIL 关键字（与 CLI --fail-keyword 合并）
    #[serde(default)]
    pub fail_keywords: Option<Vec<String>>,
    /// 透传给 `preprocess.py` 的额外字段
    #[serde(default)]
    pub extra: serde_json::Value,
}

impl MetasFile {
    /// 读取并校验 `{dir}/metas.json`
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join("metas.json");
        let content = fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
        let metas: MetasFile = serde_json::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))?;
        metas.validate().with_context(|| format!("invalid metas in {}", path.display()))?;
        Ok(metas)
    }

    /// 校验必填字段与值域
    pub fn validate(&self) -> Result<()> {
        if self.timeout == 0 {
            bail!("metas.timeout 必须为正整数");
        }
        if self.model.is_empty() {
            bail!("metas.model 不能为空");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_metas(dir: &Path, json: &str) {
        fs::write(dir.join("metas.json"), json).unwrap();
    }

    #[test]
    fn parse_ok() {
        let tmp = TempDir::new().unwrap();
        write_metas(
            tmp.path(),
            r#"{
                "timeout": 600,
                "model": {"air8101": ["sim-001"]},
                "action_count": 1,
                "priority": 5,
                "description": "test",
                "keywords": ["LuatOS@"],
                "fail_keywords": ["panic"]
            }"#,
        );
        let metas = MetasFile::load(tmp.path()).unwrap();
        assert_eq!(metas.timeout, 600);
        assert_eq!(metas.priority, 5);
        assert_eq!(metas.description, "test");
        assert_eq!(metas.keywords.as_deref(), Some(&["LuatOS@".to_string()][..]));
        assert_eq!(metas.fail_keywords.as_deref(), Some(&["panic".to_string()][..]));
        assert!(metas.flush_core.is_none());
        assert_eq!(metas.extra, serde_json::Value::Null);
        // 字符串 ID
        assert_eq!(metas.model.get("air8101").unwrap(), &vec![serde_json::Value::String("sim-001".into())]);
    }

    #[test]
    fn parse_model_int_ids_ok() {
        // 产线 testcase 常用整数数组, 例如 "air8101": [1, 101]
        let tmp = TempDir::new().unwrap();
        write_metas(
            tmp.path(),
            r#"{
                "timeout": 60,
                "model": {"air8101": [1, 101], "air8000": [1, 101]},
                "action_count": 1,
                "priority": 5,
                "description": "gmssl"
            }"#,
        );
        let metas = MetasFile::load(tmp.path()).unwrap();
        assert_eq!(metas.model.get("air8101").unwrap(), &vec![serde_json::json!(1), serde_json::json!(101)]);
        assert_eq!(metas.model.get("air8000").unwrap(), &vec![serde_json::json!(1), serde_json::json!(101)]);
    }

    #[test]
    fn parse_with_extras() {
        let tmp = TempDir::new().unwrap();
        write_metas(
            tmp.path(),
            r#"{
                "timeout": 30,
                "model": {"air8101": ["1"]},
                "action_count": 1,
                "priority": 1,
                "description": "x",
                "flush_core": "true",
                "extra": {"foo": 1, "bar": [1,2,3]}
            }"#,
        );
        let metas = MetasFile::load(tmp.path()).unwrap();
        assert_eq!(metas.flush_core.as_deref(), Some("true"));
        assert_eq!(metas.extra["foo"], serde_json::json!(1));
        assert_eq!(metas.extra["bar"], serde_json::json!([1, 2, 3]));
    }

    #[test]
    fn parse_missing_timeout_fails() {
        let tmp = TempDir::new().unwrap();
        write_metas(
            tmp.path(),
            r#"{
                "model": {"air8101": ["1"]},
                "action_count": 1,
                "priority": 1,
                "description": "x"
            }"#,
        );
        let err = MetasFile::load(tmp.path()).unwrap_err();
        assert!(err.to_string().contains("timeout") || err.chain().any(|e| e.to_string().contains("missing field")));
    }

    #[test]
    fn validate_zero_timeout_rejected() {
        let metas = MetasFile {
            timeout: 0,
            model: BTreeMap::new(),
            action_count: 1,
            priority: 1,
            description: "x".into(),
            flush_core: None,
            keywords: None,
            fail_keywords: None,
            extra: serde_json::Value::Null,
        };
        assert!(metas.validate().is_err());
    }

    #[test]
    fn validate_empty_model_rejected() {
        let metas = MetasFile {
            timeout: 1,
            model: BTreeMap::new(),
            action_count: 1,
            priority: 1,
            description: "x".into(),
            flush_core: None,
            keywords: None,
            fail_keywords: None,
            extra: serde_json::Value::Null,
        };
        let err = metas.validate().unwrap_err();
        assert!(err.to_string().contains("model"));
    }

    #[test]
    fn missing_file_returns_error() {
        let tmp = TempDir::new().unwrap();
        let err = MetasFile::load(tmp.path()).unwrap_err();
        assert!(err.to_string().contains("metas.json"));
    }
}
