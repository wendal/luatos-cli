//! `process/preprocess.py` / `midprocess.py` Python 钩子调用
//!
//! 采用 fail-fast 语义：未指定 `--python` 或解释器不存在时直接报错。
//!
//! 调用约定：
//! ```text
//! <python> <script> --testcase-dir <DIR> --ctx <FILE>
//! ```
//! midprocess 阶段额外：
//! ```text
//! --script-bin <PATH> --soc <PATH>
//! ```
//!
//! 环境变量：`LUATOS_TESTCASE_DIR` / `LUATOS_CTX_PATH`
//!
//! 跳过条件：脚本 mtime 不新于产物时跳过（仅 preprocess），用 `--force` 强制重跑。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

use anyhow::{bail, Context, Result};
use serde::Serialize;

/// 钩子调用结果
#[derive(Debug, Clone, Serialize)]
pub struct HookResult {
    /// 是否实际执行了
    pub executed: bool,
    /// 跳过原因（仅当 executed=false）
    pub skip_reason: Option<String>,
    /// 退出码（仅当 executed=true）
    pub exit_code: Option<i32>,
    /// 用时毫秒
    pub duration_ms: u64,
}

/// 钩子调用上下文
#[derive(Debug, Clone)]
pub struct HookContext {
    /// Python 解释器路径
    pub python: PathBuf,
    /// 钩子脚本路径（preprocess.py / midprocess.py）
    pub script: PathBuf,
    /// testcase 目录
    pub testcase_dir: PathBuf,
    /// ctx.json 路径
    pub ctx_path: PathBuf,
    /// 产物路径（preprocess 阶段：<testcase>/scripts/.last_preprocess_ts；midprocess 阶段：script.bin / soc）
    pub artifacts: Vec<PathBuf>,
    /// midprocess 阶段：script.bin 路径
    pub script_bin: Option<PathBuf>,
    /// midprocess 阶段：soc 路径
    pub soc: Option<PathBuf>,
    /// 超时秒数
    pub timeout_secs: u64,
    /// 强制重跑（忽略 mtime 跳过）
    pub force: bool,
}

/// 调用 Python 钩子
///
/// - 脚本不存在：返回 `executed=false, skip_reason=script_not_found`
/// - mtime 跳过（脚本 mtime 不新于 artifacts）：返回 `executed=false, skip_reason=mtime_unchanged`
/// - python 不存在：`bail`
/// - 执行失败：`bail`（含 stderr 摘要）
pub fn run_python_hook(ctx: &HookContext) -> Result<HookResult> {
    if !ctx.script.is_file() {
        return Ok(HookResult {
            executed: false,
            skip_reason: Some("script_not_found".into()),
            exit_code: None,
            duration_ms: 0,
        });
    }

    // mtime 跳过检查（先做，避免无效 spawn）
    if !ctx.force && !ctx.artifacts.is_empty() {
        if let Some(skip) = should_skip_by_mtime(&ctx.script, &ctx.artifacts)? {
            return Ok(HookResult {
                executed: false,
                skip_reason: Some(skip),
                exit_code: None,
                duration_ms: 0,
            });
        }
    }

    if !ctx.python.is_file() {
        bail!("Python 解释器不存在: {}", ctx.python.display());
    }

    let start = std::time::Instant::now();
    let mut cmd = Command::new(&ctx.python);
    cmd.arg(&ctx.script)
        .arg("--testcase-dir")
        .arg(&ctx.testcase_dir)
        .arg("--ctx")
        .arg(&ctx.ctx_path)
        .env("LUATOS_TESTCASE_DIR", &ctx.testcase_dir)
        .env("LUATOS_CTX_PATH", &ctx.ctx_path)
        .env("PYTHONIOENCODING", "utf-8")
        .env_remove("PYTHONHOME")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdin(Stdio::null());

    if let Some(b) = &ctx.script_bin {
        cmd.arg("--script-bin").arg(b);
    }
    if let Some(s) = &ctx.soc {
        cmd.arg("--soc").arg(s);
    }

    let output = cmd.output().with_context(|| format!("failed to spawn python: {}", ctx.python.display()))?;
    let elapsed = start.elapsed();

    let exit_code = output.status.code();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr_tail = stderr.lines().rev().take(10).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
        bail!("python 钩子失败: {} (exit={:?})\nstderr:\n{}", ctx.script.display(), exit_code, stderr_tail);
    }

    Ok(HookResult {
        executed: true,
        skip_reason: None,
        exit_code,
        duration_ms: elapsed.as_millis() as u64,
    })
}

/// 判断是否因 mtime 未变化而跳过。
///
/// 语义：脚本 mtime 不新于所有 artifacts 时跳过（即所有产物都 >= 脚本时间）。
///
/// - 任一 artifact 不存在 → 返回 Ok(None) 重新跑
/// - 脚本 mtime 晚于任一 artifact → 返回 Ok(None) 重新跑
/// - 所有 artifacts mtime >= 脚本 mtime → 返回 Ok(Some("mtime_unchanged")) 跳过
fn should_skip_by_mtime(script: &Path, artifacts: &[PathBuf]) -> Result<Option<String>> {
    let script_mtime = mtime(script)?;
    for a in artifacts {
        if !a.exists() {
            return Ok(None);
        }
        let a_mtime = mtime(a)?;
        // artifact 比 script 老 → 脚本被改过 → 重新跑
        if a_mtime < script_mtime {
            return Ok(None);
        }
    }
    Ok(Some("mtime_unchanged".into()))
}

fn mtime(p: &Path) -> Result<SystemTime> {
    let md = std::fs::metadata(p).with_context(|| format!("failed to stat {}", p.display()))?;
    md.modified().with_context(|| format!("failed to get mtime of {}", p.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;
    use tempfile::TempDir;

    fn touch(p: &Path, body: &str) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, body).unwrap();
    }

    #[test]
    fn script_not_found_returns_skipped() {
        let tmp = TempDir::new().unwrap();
        let ctx = HookContext {
            python: PathBuf::from("python"),
            script: tmp.path().join("nope.py"),
            testcase_dir: tmp.path().to_path_buf(),
            ctx_path: tmp.path().join("ctx.json"),
            artifacts: Vec::new(),
            script_bin: None,
            soc: None,
            timeout_secs: 30,
            force: false,
        };
        let r = run_python_hook(&ctx).unwrap();
        assert!(!r.executed);
        assert_eq!(r.skip_reason.as_deref(), Some("script_not_found"));
    }

    #[test]
    fn python_missing_bails() {
        let tmp = TempDir::new().unwrap();
        let script = tmp.path().join("preprocess.py");
        touch(&script, "# stub");
        let ctx = HookContext {
            python: tmp.path().join("nonexistent_python"),
            script,
            testcase_dir: tmp.path().to_path_buf(),
            ctx_path: tmp.path().join("ctx.json"),
            artifacts: Vec::new(),
            script_bin: None,
            soc: None,
            timeout_secs: 30,
            force: false,
        };
        let err = run_python_hook(&ctx).unwrap_err();
        assert!(err.to_string().contains("Python 解释器不存在"));
    }

    #[test]
    fn mtime_unchanged_skipped() {
        let tmp = TempDir::new().unwrap();
        let script = tmp.path().join("preprocess.py");
        let artifact = tmp.path().join("artifact");
        touch(&script, "# stub");
        touch(&artifact, "x");
        // 让 artifact 比 script 更新：sleep 0.01s
        std::thread::sleep(Duration::from_millis(20));
        touch(&artifact, "y");

        let ctx = HookContext {
            python: PathBuf::from("python"),
            script,
            testcase_dir: tmp.path().to_path_buf(),
            ctx_path: tmp.path().join("ctx.json"),
            artifacts: vec![artifact],
            script_bin: None,
            soc: None,
            timeout_secs: 30,
            force: false,
        };
        let r = run_python_hook(&ctx).unwrap();
        // 没有真实 python，但 mtime 跳过逻辑在 spawn 之前；应该跳过
        assert!(!r.executed);
        assert_eq!(r.skip_reason.as_deref(), Some("mtime_unchanged"));
    }

    #[test]
    fn mtime_newer_executes() {
        let tmp = TempDir::new().unwrap();
        let script = tmp.path().join("preprocess.py");
        let artifact = tmp.path().join("artifact");
        touch(&artifact, "old");
        std::thread::sleep(Duration::from_millis(20));
        touch(&script, "new");

        let ctx = HookContext {
            python: PathBuf::from("definitely-not-exist-zzz"),
            script,
            testcase_dir: tmp.path().to_path_buf(),
            ctx_path: tmp.path().join("ctx.json"),
            artifacts: vec![artifact],
            script_bin: None,
            soc: None,
            timeout_secs: 30,
            force: false,
        };
        // 跳过 mtime 检验后会去 spawn python，python 不存在会 bail
        let err = run_python_hook(&ctx).unwrap_err();
        assert!(err.to_string().contains("Python 解释器不存在"));
    }

    #[test]
    fn force_bypasses_mtime_check() {
        let tmp = TempDir::new().unwrap();
        let script = tmp.path().join("preprocess.py");
        let artifact = tmp.path().join("artifact");
        touch(&script, "old");
        std::thread::sleep(Duration::from_millis(20));
        touch(&artifact, "new");

        let ctx = HookContext {
            python: PathBuf::from("definitely-not-exist-zzz"),
            script,
            testcase_dir: tmp.path().to_path_buf(),
            ctx_path: tmp.path().join("ctx.json"),
            artifacts: vec![artifact],
            script_bin: None,
            soc: None,
            timeout_secs: 30,
            force: true,
        };
        // force=true 跳过 mtime 检查，直接 spawn
        let err = run_python_hook(&ctx).unwrap_err();
        assert!(err.to_string().contains("Python 解释器不存在"));
    }
}
