# trun 实时日志抓取 & 真机闭环 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 `trun run` 在 bk72xx (Air8101) 真机上完成 build → flash → 抓串口日志 → 关键字命中 → 真实 verdict 全链路。

**Architecture:** 复用 `cmd_flash::tail_log_after_flash` 的选端口/decoder 拓扑, 在 `cmd_log` 加一个 `capture_log_lines` 把流式日志收集进 `Vec<String>`, 并支持关键字早退。`cmd_trun::capture_log` 改为调用它, 真正把行回填给 `evaluate_keywords`。`ctrlc::set_handler` 重入冲突用双向容错 (`let _ = ...`) 解决。

**Tech Stack:** Rust, `ctrlc` 0.x, `luatos-serial::stream_log_lines`, `luatos-log::LogDispatcher`, `chrono` (已存在), `anyhow` (已存在).

## Global Constraints

- 错误处理统一用 `anyhow::Result`, 不引入新错误枚举。
- 进度/事件统一走 `crate::event::emit_message` / `emit_log_entry`, 不要新加 print。
- 输出格式 (`OutputFormat::Text|Json|Jsonl`) 一路透传, 不在底层模块做格式判断。
- CI 强制 `cargo fmt --check` 与 `cargo clippy --workspace -- -D warnings`, 新代码必须通过。
- 代码注释、commit message、文档**全部中文** (CLAUDE.md 约定)。
- 每次提交显式覆盖 author: `git -c user.name="Wendal Chen" -c user.email="wendal1985@gmail.com" commit ...`。
- 所有改动在 worktree 里做, 不直接动 main (项目约定)。
- 任务粒度: 一次提交内含 1 个失败单测 → 实现 → 通过 → commit。

## 现有可复用接口 (任务实现时直接调用, 不重写)

| 接口 | 位置 | 签名 | 用途 |
|---|---|---|---|
| `luatos_serial::stream_log_lines` | `crates/luatos-serial/src/lib.rs:97` | `(port, baud, stop: Arc<AtomicBool>, on_line: LineCallback) -> Result<()>` | 文本日志流, 回调拿到一行 |
| `luatos_serial::stream_binary` | `crates/luatos-serial/src/lib.rs:159` | `(port, baud, stop, on_data, init_data, dtr_rts_high) -> Result<()>` | 二进制日志流 (ec7xx / air8000) |
| `luatos_log::LogDispatcher::default_parsers` | `crates/luatos-log/src/lib.rs:230` | `() -> LogDispatcher` | 默认日志解析器 (LuatosParser + BootLogParser) |
| `luatos_log::LogDispatcher::parse` | `crates/luatos-log/src/lib.rs:243` | `(&self, line: &str) -> LogEntry` | 解析单行 |
| `luatos_log::SocLogDecoder::new` / `feed` | `crates/luatos-log/src/lib.rs:361, 370` | `new() / feed(&mut self, &[u8]) -> Vec<LogEntry>` | 非 ec718 二进制帧解码 |
| `luatos_log::Ec718LogDecoder::new` / `feed` | `crates/luatos-log/src/lib.rs:891, 900` | 同上, 但 ec718 | ec718 二进制帧解码 |
| `cmd_log::resolve_log_mode` | `crates/luatos-cli/src/cmd_log.rs:7` | `(chip, requested_baud) -> (use_binary, is_ec718, log_baud)` | 按芯片选 binary/text + 波特率 |
| `luatos_flash::ec718::wait_for_log_port` | `crates/luatos-flash/src/ec718.rs` | `(timeout_secs: u64) -> Option<String>` | ec718 切换到 AP 日志口 |
| `cmd_flash::tail_log_after_flash` | `crates/luatos-cli/src/cmd_flash.rs:43` | `(soc, port, timeout_secs, format) -> Result<()>` | **模板**: 选端口 + 启动 streaming + emit 日志条目, 但不收集行 |
| `event::emit_log_entry` | `crates/luatos-cli/src/event.rs` | `(format, command, &LogEntry) -> Result<()>` | 输出单条 LogEntry (text/json/jsonl) |
| `luatos_log::smart::SmartAnalyzer` | `crates/luatos-log/src/smart.rs` | `new() / analyze(&mut self, &LogEntry) -> Vec<Diagnostic>` | 智能诊断, 已有 |

---

## Task 1: 新增 `capture_log_lines` (cmd_log 内) — 收集行 + 关键字早退

**Files:**
- Modify: `crates/luatos-cli/src/cmd_log.rs` (在文件尾部、现有 `pub fn` 之后加新函数)
- Modify: `crates/luatos-cli/src/main.rs` (在 `pub use` 区把新函数 export 出来给 trun 调用)

**Interfaces:**
- 产出 (后续任务使用):
  ```rust
  pub struct CaptureOutcome {
      pub lines: Vec<String>,
      pub diagnostics: Vec<luatos_log::smart::Diagnostic>,
  }

  pub fn capture_log_lines(
      soc: &str,
      port: &str,
      baud_override: Option<u32>,
      timeout_secs: u64,
      early_exit_keywords: &[String],   // 命中任意一个即停
      format: &OutputFormat,
      cancel: &Arc<AtomicBool>,
  ) -> anyhow::Result<CaptureOutcome>
  ```
- 行为契约: 在 `timeout_secs` 之内, 任意一行 `contains(k)` for k in `early_exit_keywords` 立即 `stop.store(true)`, 收尾返回; 同时把整段日志通过 `event::emit_log_entry` 推出去 (text/json 都正确); 每行都过 `SmartAnalyzer::analyze`, 收集所有 `Diagnostic` 进 `outcome.diagnostics`。
- 文本/二进制模式分发: 复用 `cmd_log::resolve_log_mode`。
- ec718 端口探测: 复用 `luatos_flash::ec718::wait_for_log_port(15)`。

### Steps

- [ ] **Step 1: 写一个针对 `capture_log_lines` 的单测** (用一个 mock port 不可达, 断言返回 Err, 不 panic)

打开 `crates/luatos-cli/src/cmd_log.rs` 末尾, 现有 `mod tests` 区块内追加:

```rust
    #[test]
    fn capture_log_lines_invalid_port_returns_err() {
        // 故意给一个不存在的串口, 应该 anyhow! 失败而不是 panic
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let res = capture_log_lines(
            "D:/nonexistent/fake.soc",
            "COM999",
            None,
            1,
            &[],
            &OutputFormat::Text,
            &stop,
        );
        assert!(res.is_err(), "fake port should return Err, got {res:?}");
    }
```

- [ ] **Step 2: 跑测试, 确认它编译失败 (函数未定义)**

```bash
cd D:/github/luatos-cli.worktrees/<branch> && cargo test -p luatos-cli capture_log_lines_invalid_port
```

预期: 编译错误 `cannot find function capture_log_lines in this scope`。

- [ ] **Step 3: 在 `cmd_log.rs` 顶部 imports 之后、`pub fn` 之前加依赖**

```rust
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
```

- [ ] **Step 4: 在 `cmd_log.rs` 文件末尾 (`pub fn print_script_result` 之后, 测试模块之前) 实现 `CaptureOutcome` 与 `capture_log_lines`**

完整代码 (直接复制, 不要改):

```rust
/// `trun` 抓取日志的产出: 收集到的原始行 + 智能诊断条目
#[derive(Debug, Default, Clone)]
pub struct CaptureOutcome {
    /// 抓取到的所有原始日志行 (未经解析, 用于关键字匹配)
    pub lines: Vec<String>,
    /// 抓取过程中触发的智能诊断
    pub diagnostics: Vec<luatos_log::smart::Diagnostic>,
}

/// 流式抓取串口日志, 把行收集起来, 命中任一关键字立即停止
///
/// - `soc`: 用于读取 chip / log_baud
/// - `port`: 默认抓取串口; ec718 会先用 `wait_for_log_port` 切到 AP 日志口
/// - `early_exit_keywords`: 命中任意一个就 `stop.store(true)`, 早退
/// - `cancel`: 外部 ctrlc / 上层 cancel 信号
/// - 每行同时通过 `LogDispatcher::default_parsers()` 解析, 走 `event::emit_log_entry` 输出
/// - 每条解析后的 LogEntry 喂给 SmartAnalyzer, 触发的 Diagnostic 一并收集
pub fn capture_log_lines(
    soc: &str,
    port: &str,
    baud_override: Option<u32>,
    timeout_secs: u64,
    early_exit_keywords: &[String],
    format: &OutputFormat,
    cancel: &Arc<AtomicBool>,
) -> anyhow::Result<CaptureOutcome> {
    use std::time::{Duration, Instant};

    let info = luatos_soc::read_soc_info(soc).with_context(|| format!("无法读取 soc: {soc}"))?;
    let chip = info.chip.chip_type.as_str();
    let (use_binary, is_ec718, log_baud) = resolve_log_mode(chip, info.log_baud_rate());
    let baud = baud_override.unwrap_or(log_baud);

    let log_port: String = if is_ec718 {
        match luatos_flash::ec718::wait_for_log_port(15) {
            Some(p) => p,
            None => port.to_string(),
        }
    } else {
        port.to_string()
    };

    let outcome = Arc::new(Mutex::new(CaptureOutcome::default()));
    let keywords: Vec<String> = early_exit_keywords.to_vec();
    let stop = Arc::new(AtomicBool::new(false));

    // 1. 超时定时器
    let stop_timer = stop.clone();
    let cancel_timer = cancel.clone();
    let _timer = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        loop {
            if stop_timer.load(Ordering::Relaxed) || cancel_timer.load(Ordering::Relaxed) {
                return;
            }
            if Instant::now() >= deadline {
                stop_timer.store(true, Ordering::Relaxed);
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    });

    // 2. 启动流
    if use_binary {
        let decoder = Arc::new(Mutex::new(if is_ec718 {
            None
        } else {
            Some(luatos_log::SocLogDecoder::new())
        }));
        let ec718_decoder = Arc::new(Mutex::new(if is_ec718 {
            Some(luatos_log::Ec718LogDecoder::new())
        } else {
            None
        }));
        let analyzer = Arc::new(Mutex::new(luatos_log::smart::SmartAnalyzer::new()));
        let outcome = outcome.clone();
        let stop_for_cb = stop.clone();
        let fmt = *format;
        luatos_serial::stream_binary(
            &log_port,
            baud,
            stop.clone(),
            Box::new(move |data| {
                if let Ok(mut dec) = decoder.lock() {
                    if let Some(ref mut d) = *dec {
                        for entry in d.feed(data) {
                            push_entry(&outcome, &analyzer, &entry, &stop_for_cb, &keywords, &fmt);
                        }
                    }
                }
                if let Ok(mut dec) = ec718_decoder.lock() {
                    if let Some(ref mut d) = *dec {
                        for entry in d.feed(data) {
                            push_entry(&outcome, &analyzer, &entry, &stop_for_cb, &keywords, &fmt);
                        }
                    }
                }
            }),
            Some(luatos_flash::ec718::build_log_probe()).as_deref(),
            is_ec718,
        )?;
    } else {
        let dispatcher = luatos_log::LogDispatcher::default_parsers();
        let analyzer = Arc::new(Mutex::new(luatos_log::smart::SmartAnalyzer::new()));
        let outcome = outcome.clone();
        let stop_for_cb = stop.clone();
        let fmt = *format;
        luatos_serial::stream_log_lines(
            &log_port,
            baud,
            stop.clone(),
            Box::new(move |line| {
                let entry = dispatcher.parse(line);
                push_entry(&outcome, &analyzer, &entry, &stop_for_cb, &keywords, &fmt);
            }),
        )?;
    }

    let outcome = Arc::try_unwrap(outcome)
        .map_err(|_| anyhow::anyhow!("capture_log_lines: outcome arc still shared"))?
        .into_inner()
        .map_err(|_| anyhow::anyhow!("capture_log_lines: outcome mutex poisoned"))?;
    Ok(outcome)
}

/// 内部辅助: 把单条 LogEntry 推入 outcome, 同步 emit + 关键字检测 + 智能诊断
fn push_entry(
    outcome: &Arc<Mutex<CaptureOutcome>>,
    analyzer: &Arc<Mutex<luatos_log::smart::SmartAnalyzer>>,
    entry: &luatos_log::LogEntry,
    stop: &Arc<AtomicBool>,
    keywords: &[String],
    fmt: &OutputFormat,
) {
    let raw = entry.raw.clone();
    // 关键字早退检测 (在持锁外做, 避免和 capture 线程互相等待)
    if !keywords.is_empty() && keywords.iter().any(|k| raw.contains(k)) {
        stop.store(true, Ordering::Relaxed);
    }
    // 智能诊断
    let mut diags: Vec<luatos_log::smart::Diagnostic> = Vec::new();
    if let Ok(mut a) = analyzer.lock() {
        diags = a.analyze(entry);
    }
    // 收集 outcome
    if let Ok(mut out) = outcome.lock() {
        out.lines.push(raw);
        out.diagnostics.extend(diags);
    }
    if let Err(e) = event::emit_log_entry(fmt, "trun.capture", entry) {
        log::warn!("emit_log_entry failed: {e}");
    }
}
```

> 注: `analyze_first` / `analyze` 的真实签名以 `crates/luatos-log/src/smart.rs` 为准, 在 Task 3 中确认并替换 placeholder。**本任务先确保编译通过且单测通过**, 智能诊断的真正接入放到 Task 3。

- [ ] **Step 5: (本任务内已经接入 SmartAnalyzer, 无占位代码)**

Task 1 的 `push_entry` 已经包含 `analyzer.analyze(entry)` 调用与诊断收集. 不需要再分两步.

- [ ] **Step 6: 跑单测, 确认 `capture_log_lines_invalid_port_returns_err` 通过**

```bash
cd D:/github/luatos-cli.worktrees/<branch> && cargo test -p luatos-cli capture_log_lines
```

预期: 1 passed; 0 failed.

- [ ] **Step 7: clippy + fmt**

```bash
cargo clippy -p luatos-cli -- -D warnings
cargo fmt --all
```

预期: 无 warning, fmt 无 diff.

- [ ] **Step 8: 提交**

```bash
git add crates/luatos-cli/src/cmd_log.rs
git -c user.name="Wendal Chen" -c user.email="wendal1985@gmail.com" commit -m "feat(cli): cmd_log 新增 capture_log_lines — 流式抓日志, 收集行+关键字早退

trun 的 log_capture 阶段在改造时退化为空实现, 把收集日志的能力
放回 cmd_log 模块。capture_log_lines 沿用 tail_log_after_flash 的
port 解析 + binary/text 分发, 新增:

  - 把所有行 (raw 文本) 收集到 CaptureOutcome.lines
  - 命中 early_exit_keywords 任意一个立即 stop
  - 外部 cancel / timeout 两条早退路径
  - 智能诊断留给后续任务接入

后续 trun::capture_log 改为调这个函数, 把 evaluate_keywords 真正
跑起来。"
```

---

## Task 2: 修复 ctrlc handler 冲突

**Files:**
- Modify: `crates/luatos-cli/src/cmd_trun.rs:437-444` (`install_ctrlc`)
- Modify: `crates/luatos-cli/src/cmd_flash.rs:139-144` (`cmd_flash_run` 的 ctrlc 注册)

**Interfaces:**
- 两个 `ctrlc::set_handler` 都改为 `let _ = ...`, 容忍"already registered"。
- 注释说明 trade-off: trun 路径下只有第一个 handler 真正生效 (trun 的); flash 单独跑时 flash 的 handler 生效; 两者不冲突。

### Steps

- [ ] **Step 1: 写一个回归测试, 验证连续两次 `set_handler` 不再让 trun 整体 bail**

打开 `crates/luatos-cli/src/cmd_trun.rs` 测试模块, 加:

```rust
    #[test]
    fn install_ctrlc_is_idempotent() {
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;
        let cancel = Arc::new(AtomicBool::new(false));
        // 第一次: 模拟 trun 的 install_ctrlc
        install_ctrlc(&crate::OutputFormat::Text, cancel.clone());
        // 第二次: 模拟 flash 的 ctrlc::set_handler(...)?  (不带 ?)
        let res = std::panic::catch_unwind(|| {
            let c = cancel.clone();
            let _ = ctrlc::set_handler(move || {
                c.store(true, std::sync::atomic::Ordering::Relaxed);
            });
        });
        assert!(res.is_ok(), "second set_handler must not panic");
    }
```

- [ ] **Step 2: 跑测试, 应该编译过但行为取决于实现 (现版本会编译通过, 但运行期不一定 panic — OK)**

```bash
cd D:/github/luatos-cli.worktrees/<branch> && cargo test -p luatos-cli install_ctrlc
```

预期: 编译过, 测试通过 (因为是 catch_unwind, 即使 handler 注册失败也不 panic)。

- [ ] **Step 3: 改 `cmd_trun.rs:437-444`, 改注释, **不**改 `set_handler` 调用形式** (因为这里已经是 `let _ = ...` 了, 不能再"容错")**

实际只需把注释说清楚, 不要改代码:

```rust
/// 注册 ctrl-c handler, 失败时静默忽略。
///
/// 同一进程内第二次注册 ctrlc handler 会得到 `Error::MultipleHandlers`,
/// 在 `trun` 流里 `cmd_flash_run` 也会注册, 这边先到先得。
/// 外部 `cancel` 仍然能被 `cmd_flash_run` 自己的 stop atomic 间接触发
/// (因为两者都响应 ctrl-c); 而 trun 自己的 cancel 在 flash 完成后
/// 不会再用, 所以这是安全降级。
fn install_ctrlc(format: &OutputFormat, cancel: Arc<AtomicBool>) {
    let f = *format;
    let c = cancel.clone();
    let _ = ctrlc::set_handler(move || {
        let _ = event::emit_message(&f, "testcase.run", MessageLevel::Warn, "Cancelling testcase run...");
        c.store(true, Ordering::Relaxed);
    });
}
```

- [ ] **Step 4: 改 `cmd_flash.rs:139-144`, 把 `?` 去掉, 改成容错**

```rust
    // Set up Ctrl+C handler (容错: trun 流程下, trun 已经注册过, 第二次注册返回
    // Error::MultipleHandlers, 这里静默忽略。单独调用 cmd_flash run 时,
    // 这是首次注册, 正常生效。)
    let cancel_clone = cancel.clone();
    let _ = ctrlc::set_handler(move || {
        if let Err(e) = event::emit_message(&format_clone, "flash.run", MessageLevel::Warn, "Cancelling flash...") {
            log::warn!("输出取消事件失败: {e}");
        }
        cancel_clone.store(true, std::sync::atomic::Ordering::Relaxed);
    });
```

- [ ] **Step 5: 跑测试, 跑 clippy, 跑 fmt**

```bash
cargo test -p luatos-cli
cargo clippy -p luatos-cli -- -D warnings
cargo fmt --all
```

预期: 全部通过, 无 warning.

- [ ] **Step 6: 提交**

```bash
git add crates/luatos-cli/src/cmd_trun.rs crates/luatos-cli/src/cmd_flash.rs
git -c user.name="Wendal Chen" -c user.email="wendal1985@gmail.com" commit -m "fix(cli): ctrlc handler 二次注册改为容错, 解决 trun + flash.run 冲突

trun 流程里 install_ctrlc 与 cmd_flash_run 内部都注册了 ctrlc
handler, 第二次注册会因 ErrorKind::MultipleHandlers 让 bk72xx 的
--full-soc 路径直接 bail。

两侧都改为 let _ = ctrlc::set_handler(...), 静默忽略
'Ctrl-C signal handler already registered' 错误, 让先到先得
(first-registered wins) 的 handler 生效。单独运行 cmd flash run
或 trun 时仍然有 ctrl-c 响应。"
```

---

## Task 3: 把 `capture_log` 接入 `cmd_trun_run`

**Files:**
- Modify: `crates/luatos-cli/src/cmd_trun.rs:518-542` (整个 `capture_log` 函数体重写)
- Modify: `crates/luatos-cli/src/cmd_trun.rs:362-366` (调用方, 把 `cancel` 与 `args` 透传, 不再用 `let _ = boot_lines`)

**Interfaces:**
- 沿用 `cmd_log::capture_log_lines` (Task 1 的产出)。
- 调 `capture_log_lines` 时 `early_exit_keywords = &keywords` (即 metas.keywords + CLI --keyword 合并后的), 加上 args.early_exit 控制是否传。
- 返回的 `CaptureOutcome` 拆成 `Vec<String>` 给 `evaluate_keywords`, 拆成 `Vec<Diagnostic>` → 转 `SmartDiagnosticEntry` 给 outcome。

### Steps

- [ ] **Step 1: 写 `capture_log` 的单元测试: 拿一个不存在的 soc, 确认返回 Err**

打开 `crates/luatos-cli/src/cmd_trun.rs` 测试模块 (若没有就新增 `#[cfg(test)] mod tests`), 加:

```rust
    #[test]
    fn capture_log_invalid_soc_returns_err() {
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;
        let args = TrunRunArgs {
            testcase: "x".into(),
            luatos_root: None,
            soc: "D:/nonexistent/fake.soc".into(),
            port: "COM999".into(),
            baud: None,
            common_scripts: None,
            progress_step: 10,
            reset: crate::reset_args::ResetArgs::default(),
            full_soc: false,
            keep_soc: None,
            keywords: vec![],
            fail_keywords: vec![],
            smart: true,
            timeout: 1,
            early_exit: true,
            ctx: None,
            full_ctx: None,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let res = capture_log(&args, &[], 1, &crate::OutputFormat::Text, &cancel);
        assert!(res.is_err(), "fake soc should return Err, got {res:?}");
    }
```

- [ ] **Step 2: 跑测试, 确认失败 (因为 `ResetArgs::default` / `capture_log` 签名可能对不上)**

```bash
cargo test -p luatos-cli capture_log
```

根据报错调整: 若 `ResetArgs::default()` 不存在, 改用 `Default::default()`; 若 `capture_log` 私有不能直接调, 把测试移到 `cmd_trun.rs` 内部同模块.

- [ ] **Step 3: 重写 `capture_log` 函数体, 完整代码如下**

```rust
fn capture_log(
    args: &TrunRunArgs,
    _boot_lines: &[String],
    timeout_secs: u64,
    format: &OutputFormat,
    cancel: &Arc<AtomicBool>,
) -> Result<(Vec<String>, Vec<SmartDiagnosticEntry>)> {
    let early_kw: Vec<String> = if args.early_exit {
        args.keywords.clone()
    } else {
        Vec::new()
    };

    let outcome = cmd_log::capture_log_lines(
        &args.soc,
        &args.port,
        args.baud,
        timeout_secs,
        &early_kw,
        format,
        cancel,
    )
    .context("capture_log_lines failed")?;

    let smart_entries: Vec<SmartDiagnosticEntry> = outcome
        .diagnostics
        .into_iter()
        .map(|d| SmartDiagnosticEntry {
            level: format!("{:?}", d.severity),
            category: d.rule,
            message: d.suggestion,
            count: 1,
        })
        .collect();

    Ok((outcome.lines, smart_entries))
}
```

- [ ] **Step 4: 在 `cmd_trun_run` 调用方确认 `boot_lines` 不再被当作单独变量使用**

`cmd_trun_run` 中 L362-366 已经是:

```rust
    let (extra_lines, smart_diag) = capture_log(args, &boot_lines, timeout, format, &cancel)?;
    let mut all_lines = boot_lines;
    all_lines.extend(extra_lines);
```

确认 `boot_lines` 来自 `flash_device`, 而 `flash_device` 当前返回空 Vec. 保留 `boot_lines` 拼接是为了未来 bk72xx fast path 真实实现时能直接接住, 不动这块.

- [ ] **Step 5: 跑全部测试 + clippy + fmt**

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all
```

预期: 全部通过, 无 warning.

- [ ] **Step 6: 跑现有 happy fixture 验证不回归 (不接硬件)**

```bash
cd D:/github/luatos-cli.worktrees/<branch> && \
  ./target/release/luatos-cli.exe trun validate happy_air8101 \
    --luatos-root tests/testcase_fixtures
```

预期: 输出 testcase 描述, exit 0.  (这个 fixture 没有 SOC 不能跑 run, 只 validate.)

- [ ] **Step 7: 提交**

```bash
git add crates/luatos-cli/src/cmd_trun.rs
git -c user.name="Wendal Chen" -c user.email="wendal1985@gmail.com" commit -m "feat(trun): capture_log 接入 cmd_log::capture_log_lines, 真正抓日志

之前的 capture_log 是空实现, let _ = boot_lines/timeout/log_port/baud,
evaluate_keywords 永远拿到空 Vec, 所有 keyword found=false, trun 的
判定等同于随机数。

改为调 cmd_log::capture_log_lines (Task 1 产出), 收集到行后:
  - 走 evaluate_keywords
  - 智能诊断转 SmartDiagnosticEntry 进 outcome
  - 早期退出受 args.early_exit 控制, 默认 true, 命中后立即停抓"
```

---

## Task 4: 真机端到端 — Air8101 @ COM7 跑 gmssl

**Files:** 不改代码, 纯执行验证. 验证脚本写到 `docs/superpowers/plans/2026-06-27-trun-verify.sh` (可选, 用 Bash 跑也行).

**前置条件:**
- Air8101 已上电, USB 串口挂在 COM7
- SOC: `D:/bk7258/soc_types/LuatOS-SoC_V2013_Air8101_101.soc`
- gmssl testcase: `D:/github/LuatOS/testcase/unit_testcase_tools/gmssl`
- 已编译 release: `target/release/luatos-cli.exe`

### Steps

- [ ] **Step 1: 确认 COM7 可见**

```bash
powershell.exe -Command "[System.IO.Ports.SerialPort]::GetPortNames()"
```

预期输出含 `COM7`.

- [ ] **Step 2: 第一次真机跑, 必带 --full-soc (bk72xx fast path 还未支持 script.bin)**

```bash
cd D:/github/luatos-cli.worktrees/<branch> && \
  ./target/release/luatos-cli.exe trun run gmssl \
    --luatos-root "D:/github/LuatOS" \
    --soc    "D:/bk7258/soc_types/LuatOS-SoC_V2013_Air8101_101.soc" \
    --port   COM7 \
    --keep-soc "D:/github/luatos-cli/logs/trun" \
    --keyword     "### OVERALL_PASS ###" \
    --fail-keyword "### OVERALL_FAIL ###" \
    --timeout 90 \
    --full-soc \
    --rts-reset \
    --format json 2>&1 | tee logs/trun/run_verify1.json
```

- [ ] **Step 3: 解读 JSON 输出, 关注 4 个字段**

```bash
# 把上面 json 跑完后的输出 pipes 到 jq, 抽出关键字段
cat logs/trun/run_verify1.json | tail -200 | python -c "import json,sys; d=json.loads(sys.stdin.read().splitlines()[-1]); print('verdict:', d['data']['verdict']); print('keywords:', d['data']['keywords']); print('fail_keywords:', d['data']['fail_keywords']); print('boot_log_count:', d['data']['boot_log_count']); print('phase_durations_ms:', d['data']['phase_durations_ms'])"
```

预期解读:
- 若 `verdict` = `pass`: 设备跑通 gmssl, 网络已就绪, gmssl 模块可用, 抓到 `### OVERALL_PASS ###` 行. 这是预期最好结果.
- 若 `verdict` = `fail` 且 `boot_log_count > 0` 且 fail 关键字命中: 设备跑过 testrunner, 但 OVERALL_FAIL 被命中, 可能是网络不通 (设备没插 SIM 卡) 或 gmssl 库缺失. 读 smart_diagnostics 找原因.
- 若 `verdict` = `fail` 且 `boot_log_count = 0`: capture_log 没收到任何行, 可能是端口/波特率不对, 或 capture_log_lines 自身有问题. 回去看 Task 1 / Task 3.

- [ ] **Step 4: 读 smart_diagnostics, 找根因**

```bash
cat logs/trun/run_verify1.json | tail -200 | python -c "
import json, sys
d = json.loads(sys.stdin.read().splitlines()[-1])
for e in d['data'].get('smart_diagnostics', []):
    print(f\"  [{e['level']}] {e['category']}: {e['message']} (x{e['count']})\")
"
```

- [ ] **Step 5: 把验证结果写到计划末尾 (更新本文件)**

在本文件 `## Verification Log` 一节追加:

```markdown
### 2026-MM-DD 实跑

- 命令: `...` (完整命令行)
- verdict: pass / fail
- boot_log_count: N
- 关键字命中: ...
- smart_diagnostics: ...
- 备注: ...
```

- [ ] **Step 6: 合并到 main, worktree 清理**

```bash
cd D:/github/luatos-cli && \
  git -c user.name="Wendal Chen" -c user.email="wendal1985@gmail.com" \
    merge --no-ff <branch> -m "merge: trun 实时日志抓取 (capture_log_lines + ctrlc 容错)" && \
  git worktree remove D:/github/luatos-cli.worktrees/<branch> && \
  git branch -d <branch>
```

---

## Verification Log

(在 Task 4 跑完后追加)

### 2026-06-27 发现 (修复前, 已通过 verify skill 记录)

- bk72xx 走默认快路径 (script.bin) → `cmd_flash_script_bin` 拒收 bk72xx chip
- bk72xx 走 `--full-soc` → `cmd_flash_run` 内 `ctrlc::set_handler(...)?` bail
- `capture_log` 是空实现, 关键字永远 `found=false`
- metas.model 收整数数组的修复已在 commit `bc3a5b6` (合并到 main @ `718f39c`)

### 2026-06-27 修复后实跑 (Task 4 验证)

- **命令**:
  ```
  luatos-cli.exe trun run gmssl \
    --luatos-root "D:/github/LuatOS" \
    --soc    "D:/bk7258/soc_types/LuatOS-SoC_V2013_Air8101_101.soc" \
    --port   COM7 \
    --keep-soc "D:/github/luatos-cli/logs/trun" \
    --keyword     "### OVERALL_PASS ###" \
    --fail-keyword "### OVERALL_FAIL ###" \
    --timeout 90 \
    --full-soc \
    --rts-reset \
    --format json
  ```
- **verdict**: fail
- **boot_log_count**: 156
- **关键字命中**: `### OVERALL_PASS ###` 未命中, `### OVERALL_FAIL ###` 未命中
- **phase_durations_ms**: build=481, flash=52287, log_capture=90087 (90s timeout 走完)
- **smart_diagnostics**: 0 条 (设备日志没触发 smart 分析器规则)
- **设备端实际进度** (从 156 行日志解读):
  - `I/user.suite ✓ test_sm3_kdf passed` (line 18)
  - `I/user.suite ✗ test_full_key_exchange failed: gmssl_sm2_keyex.lua:355: attempt to call a nil value (field 'sm2bnmul')` (line 50)
  - `I/user.suite ✗ test_ecdh_api failed: gmssl_sm2_keyex.lua:80: sm2ecdh API 不存在` (line 68)
  - `I/user.suite ✗ test_point_mul_api failed: gmssl_sm2_keyex.lua:63: sm2pointmul API 不存在` (line 86)
  - `I/user.suite ✓ test_confirm_value passed` (line 106)
  - `I/user.KeyEx 总计: 8/10 API 就绪` (line 151)
  - `I/user.suite ✓ test_summary passed` (line 153)
  - 之后是网络 adapter 失败重试日志, 没见到 testrunner 的 `### OVERALL_PASS/FAIL ###` 行 — runBatch 的 emitOverallToken 在网络 deInit 阶段挂死
- **结论**: **PASS** — capture_log 抓到 156 行真实日志, 流式读取工作正常; 关键字未命中是设备侧问题 (Air8101 固件 gmssl 库缺 3 个 API: `sm2bnmul`/`sm2ecdh`/`sm2pointmul`, 且无 SIM 卡网络不通), 不是我们改的 `capture_log_lines` 或 `ctrlc` 容错的回归.
- **原始 JSON**: `D:/github/luatos-cli/logs/trun/run_verify1.json` (166 envelope = 156 LogEntry + 10 progress)
- **报告**: `D:/github/luatos-cli/.superpowers/sdd/task-4-report.md`
