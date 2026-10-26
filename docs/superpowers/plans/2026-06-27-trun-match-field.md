# trun 关键字匹配走真实 LogEntry 字段 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 `trun` 的关键字匹配从 "raw 字符串 contains" 升级为 "针对 `LogEntry` 结构化字段匹配", 利用 `LogDispatcher` 解析产物 (message/module/level) 而非帧头污染的原始字节。默认行为从"raw"改为"message", 并通过 `--match-field` 让调用方按需选 raw / message / any / all。

**Architecture:** `cmd_log::CaptureOutcome` 把字段从 `lines: Vec<String>` 改为 `entries: Vec<LogEntry>`;`cmd_trun` 新增 `MatchField` enum (clap ValueEnum) + `match_keyword` 函数,替代 `evaluate_keywords`;`TrunRunArgs` 加 `--match-field` (默认 `Message`);`TrunOutcome` 仍用 `boot_log_count` 报告 entries 数量。

**Tech Stack:** Rust, clap ValueEnum, `luatos_log::LogEntry` / `LogLevel`, anyhow (已存在).

## Global Constraints

- 错误处理统一用 `anyhow::Result`, 不引入新错误枚举
- 进度/事件统一走 `crate::event::emit_message` / `emit_log_entry`
- 输出格式 (`OutputFormat::Text|Json|Jsonl`) 一路透传, 不在底层模块做格式判断
- CI 强制 `cargo fmt --check` 与 `cargo clippy --workspace -- -D warnings`
- 代码注释、commit message、文档**全部中文** (CLAUDE.md 约定)
- 每次提交显式覆盖 author: `git -c user.name="Wendal Chen" -c user.email="wendal1985@gmail.com" commit ...`
- 所有改动在 worktree 里做, 不直接动 main
- 任务粒度: 1 个失败单测 → 实现 → 通过 → commit

## 现有可复用接口 (任务实现时直接调用, 不重写)

| 接口 | 位置 | 用途 |
|---|---|---|
| `luatos_log::LogEntry` | `crates/luatos-log/src/lib.rs:19` | 结构化日志条目, 字段: timestamp, device_time, level (LogLevel), module (Option<String>), message, raw |
| `luatos_log::LogLevel` | `crates/luatos-log/src/lib.rs:36` | enum: Debug, Info, Warn, Error, Trace, Unknown(char), Raw; `as_str()` 返回单字符 |
| `luatos_log::LogDispatcher::default_parsers` | `crates/luatos-log/src/lib.rs:230` | 默认 LuatosParser + BootLogParser |
| `LogDispatcher::parse(line) -> LogEntry` | `crates/luatos-log/src/lib.rs:243` | 单行解析入口 (text 模式已在 capture_log_lines 内部使用) |
| `cmd_log::CaptureOutcome` | `crates/luatos-cli/src/cmd_log.rs` | T1 上一个计划产出, 当前 `lines: Vec<String>`, 本计划改为 `entries: Vec<LogEntry>` |
| `cmd_log::push_entry` | `crates/luatos-cli/src/cmd_log.rs:584` | 当前 `out.lines.push(raw.clone())`, 本计划改为 `out.entries.push(entry.clone())` |
| `cmd_trun::evaluate_keywords` | `crates/luatos-cli/src/cmd_trun.rs:544` | 旧函数, 本计划替换为 `match_keyword` |
| `cmd_trun::TrunRunArgs` | `crates/luatos-cli/src/cmd_trun.rs:51-108` | 加 `--match-field` 参数 |
| `cmd_trun::TrunOutcome` | `crates/luatos-cli/src/cmd_trun.rs:204-225` | `boot_log_count: usize` 保留, 语义变为 entries 数量 |
| `clap::ValueEnum` | clap 0.x | 已有, `LogLevel` 也实现了 `Clone + Serialize`, 直接 derive ValueEnum |

## 设计决策

1. **`LogEntry` 必须可 Clone** — `pub struct LogEntry` 当前没有 derive Clone,需要补上 (LuatosParser / BootLogParser 的解析结果被 dispatcher 持有, capture_log_lines 要 clone 一份塞进 outcome)。检查 src/lib.rs:19 周围补 `#[derive(Clone)]`。
2. **默认 `--match-field` = `Message`** — message 字段是 LuatosParser 剥掉 SOH+len+type 帧头后的内容, 是 "真解析结果"。`Raw` 模式保留为降级路径, 给老用户和帧头里恰好有关键字的场景。
3. **`Any` 模式匹配三个字段** — message/module/level 任一 contains 即 true。`level.as_str()` 是单字符, 大多数关键字不会命中 level, 但 `--match-field Any --keyword "I"` 能匹配所有 Info 级日志 (这是 feature 不是 bug)。
4. **现有 `KeywordHit` 结构保留** — 字段不变 (`keyword: String, found: bool`), 内部改由 `match_keyword` 填值。
5. **`boot_log_count` 语义不变** — 数量就是 entries 数量, 跟之前 lines 数量一样。`T1 minor #4` (entry.raw 含帧头) 自动消除: 现在默认走 message。

---

## Task 1: `CaptureOutcome` 改为 `entries: Vec<LogEntry>` + `LogEntry` Clone

**Files:**
- Modify: `crates/luatos-log/src/lib.rs:19` (`LogEntry` 加 `Clone` derive)
- Modify: `crates/luatos-cli/src/cmd_log.rs` (CaptureOutcome 字段类型, push_entry 内部 collect 改为 push entry clone)

**Interfaces:**
- `luatos_log::LogEntry`: 新增 `#[derive(Clone)]`
- `cmd_log::CaptureOutcome`:
  ```rust
  pub struct CaptureOutcome {
      pub entries: Vec<luatos_log::LogEntry>,  // 旧 lines: Vec<String>
      pub diagnostics: Vec<luatos_log::smart::Diagnostic>,
  }
  ```

### Steps

- [ ] **Step 1: 写一个失败单测: 验证 `LogEntry: Clone` 能编过** (TDD 驱动 derive)

打开 `crates/luatos-log/src/lib.rs`, 找到 `LogEntry` 定义 (L19), 加 `#[derive(Clone)]` 之前先**故意**不 derive, 让 T1 单测编译失败:

```rust
    // crates/luatos-log/src/lib.rs:19 改为
    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    pub struct LogEntry { ... }   // 没有 Clone, 单测会编译失败
```

然后在 `crates/luatos-log/src/lib.rs` 末尾的 `mod tests` 加:

```rust
    #[test]
    fn log_entry_is_clone() {
        let entry = LogEntry {
            timestamp: "2026-06-27T22:00:00.000Z".into(),
            device_time: None,
            level: LogLevel::Info,
            module: Some("user.main".into()),
            message: "hello".into(),
            raw: "`R\x00\x00I/user.main hello".into(),
        };
        let cloned = entry.clone();
        assert_eq!(cloned.message, "hello");
        assert_eq!(cloned.module.as_deref(), Some("user.main"));
        assert_eq!(cloned.level.as_str(), "I");
    }
```

跑:
```bash
cd D:/github/luatos-cli.worktrees/<branch> && cargo test -p luatos-log log_entry_is_clone
```
预期: 编译错误, `the trait bound \`LogEntry: Clone\` is not satisfied` (因为还没 derive Clone)。

- [ ] **Step 2: 给 `LogEntry` 加 `#[derive(Clone)]`**

`crates/luatos-log/src/lib.rs:19`:
```rust
    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    pub struct LogEntry { ... }
```

跑上面同一条测试, 预期: PASS.

- [ ] **Step 3: 写一个失败单测: CaptureOutcome 现在应该用 entries 而不是 lines**

`crates/luatos-cli/src/cmd_log.rs` 末尾 `mod tests`, 找到 `capture_log_lines_invalid_port_returns_err` 单测附近, 加:

```rust
    #[test]
    fn capture_outcome_uses_entries_field() {
        // 编译期断言: CaptureOutcome 必须有 entries: Vec<LogEntry> 字段
        // 这个测试如果编译过, 就证明字段名正确
        use luatos_log::LogEntry;
        let _entries: Vec<LogEntry> = crate::cmd_log::CaptureOutcome::default().entries;
    }
```

跑 `cargo test -p luatos-cli capture_outcome_uses_entries_field`, 预期: 编译错误 (`no field \`entries\` on type \`CaptureOutcome\`` 或 `lines` 而不是 `entries`)。

- [ ] **Step 4: 改 `CaptureOutcome` 字段**

`crates/luatos-cli/src/cmd_log.rs` 当前 `CaptureOutcome`:
```rust
pub struct CaptureOutcome {
    pub lines: Vec<String>,
    pub diagnostics: Vec<luatos_log::smart::Diagnostic>,
}
```

改为:
```rust
pub struct CaptureOutcome {
    pub entries: Vec<luatos_log::LogEntry>,
    pub diagnostics: Vec<luatos_log::smart::Diagnostic>,
}
```

- [ ] **Step 5: 改 `push_entry` 内部 push 整条 entry clone**

当前 `push_entry` (cmd_log.rs:584 附近):
```rust
    if let Ok(mut out) = outcome.lock() {
        out.lines.push(raw);
        out.diagnostics.extend(diags);
    }
```

改为:
```rust
    if let Ok(mut out) = outcome.lock() {
        out.entries.push(entry.clone());
        out.diagnostics.extend(diags);
    }
```

把上面 `let raw = entry.raw.clone();` 删掉 (不再需要 raw 局部变量)。

- [ ] **Step 6: 找 cmd_log.rs 里其他用到 `out.lines` 的地方, 同步改**

```bash
cd D:/github/luatos-cli.worktrees/<branch> && grep -n "out\.lines\|outcome\.lines\|\.lines\.push\|\.lines\.iter" crates/luatos-cli/src/cmd_log.rs
```

预期: T1 上一个计划之后只剩 push_entry 里的 1 处 (Step 5 已改)。如果有其他遗漏, 全部改为 `entries`。

- [ ] **Step 7: 跑全 workspace 测试, 确认 T1 现有测试仍过**

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all
```

预期: 全过, 无 warning. **注意**: 之前 T1 的 `capture_log_lines_invalid_port_returns_err` 单测没访问 outcome 字段, 应该不挂。但 `TrunOutcome::boot_log_count` 仍用 `outcome.lines.len()` 会有编译错 —— 这是 T2 范围, **不**在本任务修 (会先在 T2 改 evaluate_keywords 链路)。本任务跑测试时如果 build 错是因为 T2 还没改, 临时把 `cmd_trun.rs` 里的 `outcome.lines.len()` 改为 `outcome.entries.len()` 也行 (反正 T2 会一并整理)。**推荐**: 接受 cargo test 暂时挂在 trun crate, 因为 T2 紧接着会修, 不要本任务里大改 trun。

- [ ] **Step 8: 提交**

```bash
git add crates/luatos-log/src/lib.rs crates/luatos-cli/src/cmd_log.rs
git -c user.name="Wendal Chen" -c user.email="wendal1985@gmail.com" commit -m "refactor(log+cli): CaptureOutcome 改用 LogEntry, LuatosParser 解析产物透出

之前 capture_log_lines 内部虽然走了 LogDispatcher::parse 拿到
LogEntry, 但只把 entry.raw 推入 Vec<String>, 整个 LogEntry 的
module/level/message 字段被丢弃。trun::evaluate_keywords 拿到的
是 raw 字符串, 仍含 bk72xx 的 SOH+len+type 帧头, 关键字匹配如果
落在帧头位置会 false negative。

本任务把 CaptureOutcome.lines: Vec<String> 改为
entries: Vec<LogEntry>, push_entry 直接 push entry clone。
LogEntry 补 Clone derive。

TrunRunArgs / evaluate_keywords / TrunOutcome 的对应调整在 T2
完成, 本任务不涉及 trun crate 改动。"
```

---

## Task 2: `cmd_trun` 新增 `MatchField` enum + `match_keyword` + `--match-field` 参数

**Files:**
- Modify: `crates/luatos-cli/src/cmd_trun.rs` (`TrunRunArgs` 加 `--match-field`, `evaluate_keywords` 替换为 `match_keyword`, `cmd_trun_run` 调用方改用新 API)
- Modify: `crates/luatos-cli/src/cmd_trun.rs:518-542` 附近 (`capture_log` 返回 Vec<LogEntry> 而不是 Vec<String>)

**Interfaces:**
- 新增:
  ```rust
  /// 关键字匹配模式: 选择在哪几个 LogEntry 字段上做 contains
  #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, clap::ValueEnum)]
  #[serde(rename_all = "lowercase")]
  pub enum MatchField {
      /// 仅 message 字段 (LuatosParser 剥掉帧头后的内容, **默认**)
      #[default]
      Message,
      /// 仅 raw 字段 (含帧头, 旧行为, 降级路径)
      Raw,
      /// message / module / level 任一字段命中
      Any,
      /// message / module / level 全部字段命中
      All,
  }
  ```

- 新增函数:
  ```rust
  /// 在 entries 上按 field 模式匹配单个 keyword
  pub fn match_keyword(entries: &[luatos_log::LogEntry], keyword: &str, field: MatchField) -> bool {
      entries.iter().any(|e| match field {
          MatchField::Raw => e.raw.contains(keyword),
          MatchField::Message => e.message.contains(keyword),
          MatchField::Any => {
              e.message.contains(keyword)
                  || e.module.as_deref().unwrap_or("").contains(keyword)
                  || e.level.as_str().contains(keyword)
          }
          MatchField::All => {
              e.message.contains(keyword)
                  && e.module.as_deref().unwrap_or("").contains(keyword)
                  && e.level.as_str().contains(keyword)
          }
      })
  }
  ```

- `TrunRunArgs` 加字段:
  ```rust
  /// 关键字匹配字段 (默认 message, 真实解析结果)
  #[arg(long, value_enum, default_value_t = MatchField::Message)]
  pub match_field: MatchField,
  ```

- `cmd_trun_run` 内, `evaluate_keywords(&all_lines, &keywords)` 改为:
  ```rust
  let keyword_hits: Vec<KeywordHit> = keywords
      .iter()
      .map(|k| KeywordHit {
          keyword: k.clone(),
          found: match_keyword(&entries, k, args.match_field),
      })
      .collect();
  let fail_keyword_hits: Vec<KeywordHit> = fail_keywords
      .iter()
      .map(|k| KeywordHit {
          keyword: k.clone(),
          found: match_keyword(&entries, k, args.match_field),
      })
      .collect();
  ```

- `capture_log` 签名改为返回 `Vec<LogEntry>` (而不是 `Vec<String>`):
  ```rust
  fn capture_log(
      args: &TrunRunArgs,
      _boot_lines: &[String],
      timeout_secs: u64,
      format: &OutputFormat,
      cancel: &Arc<AtomicBool>,
  ) -> Result<(Vec<luatos_log::LogEntry>, Vec<SmartDiagnosticEntry>)> {
      // ...
      Ok((outcome.entries, smart_entries))
  }
  ```

- `TrunOutcome.boot_log_count: usize` 语义不变, 填 `entries.len()` 即可。

### Steps

- [ ] **Step 1: 写 4 个失败单测覆盖 `match_keyword` 的 4 种模式**

打开 `crates/luatos-cli/src/cmd_trun.rs`, 找到现有 `mod tests` (或加 `#[cfg(test)] mod tests`), 加:

```rust
    use luatos_log::{LogEntry, LogLevel};

    fn entry(msg: &str, module: Option<&str>, level: LogLevel) -> LogEntry {
        LogEntry {
            timestamp: "2026-06-27T22:00:00.000Z".into(),
            device_time: None,
            level,
            module: module.map(String::from),
            message: msg.into(),
            raw: format!("`R\x00\x00{}{}{}", level.as_str(), module.unwrap_or(""), msg),
        }
    }

    #[test]
    fn match_keyword_message_only() {
        let entries = vec![entry("hello world", Some("user.main"), LogLevel::Info)];
        assert!(match_keyword(&entries, "hello", MatchField::Message));
        assert!(!match_keyword(&entries, "user.main", MatchField::Message),
            "module 不应被 message 模式命中");
        assert!(!match_keyword(&entries, "I", MatchField::Message),
            "level 不应被 message 模式命中");
    }

    #[test]
    fn match_keyword_raw_only() {
        let entries = vec![entry("hello", Some("user.main"), LogLevel::Info)];
        // raw 包含帧头, 也包含 module 名称
        assert!(match_keyword(&entries, "user.main", MatchField::Raw));
        assert!(match_keyword(&entries, "hello", MatchField::Raw));
    }

    #[test]
    fn match_keyword_any_field() {
        let entries = vec![entry("hello", Some("user.testrunner"), LogLevel::Info)];
        // message 命中
        assert!(match_keyword(&entries, "hello", MatchField::Any));
        // module 命中
        assert!(match_keyword(&entries, "testrunner", MatchField::Any));
        // level 命中 (单字符 "I")
        assert!(match_keyword(&entries, "I", MatchField::Any));
        // 不存在的关键字
        assert!(!match_keyword(&entries, "nonexistent", MatchField::Any));
    }

    #[test]
    fn match_keyword_all_fields() {
        // 构造一个 module="user.main" level=Info message="hello" 的 entry
        let entries = vec![entry("hello", Some("user.main"), LogLevel::Info)];
        // 三个字段都包含 "main": message? no. level? no. module? yes.
        assert!(!match_keyword(&entries, "main", MatchField::All),
            "仅 module 命中不应触发 All");
        // 三个字段都包含 "user.main" 是不可能的 (只有 module 有 user.main)
        assert!(!match_keyword(&entries, "user.main", MatchField::All),
            "All 必须三字段都包含, 仅 module 不够");
        // 构造一个三字段都包含同一字符串的: 用 "I" 不行 (message 不含 I 除非巧合)
        // 改用 entry("I-info", Some("user.I"), LogLevel::Info) 三个都含 "I"
        let triple = vec![entry("I-info", Some("user.I"), LogLevel::Info)];
        assert!(match_keyword(&triple, "I", MatchField::All),
            "三字段都含 I, All 应命中");
    }
```

跑 `cargo test -p luatos-cli match_keyword`, 预期: 编译错误 (`cannot find function match_keyword`)。

- [ ] **Step 2: 实现 `MatchField` enum + `match_keyword` 函数**

在 `cmd_trun.rs` 顶部 imports 之后, 现有 `evaluate_keywords` 之前加:

```rust
use luatos_log::{LogEntry, LogLevel};
```

(注意: `luatos_log` 已经是 workspace dep, 检查 Cargo.toml 确认。)

然后**保留** `evaluate_keywords` 旧函数先**不删**, 在它之前加 `match_keyword` 和 `MatchField`:

```rust
/// 关键字匹配字段模式
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum MatchField {
    /// 仅 message 字段 (LuatosParser 剥掉帧头后的内容, 默认)
    #[default]
    Message,
    /// 仅 raw 字段 (含 SOH+len+type 帧头, 旧行为, 降级路径)
    Raw,
    /// message / module / level 任一字段包含 keyword 即命中
    Any,
    /// message / module / level 全部字段都包含 keyword 才命中
    All,
}

/// 在 LogEntry 列表上按 field 模式匹配单个 keyword
pub fn match_keyword(entries: &[LogEntry], keyword: &str, field: MatchField) -> bool {
    entries.iter().any(|e| match field {
        MatchField::Raw => e.raw.contains(keyword),
        MatchField::Message => e.message.contains(keyword),
        MatchField::Any => {
            e.message.contains(keyword)
                || e.module.as_deref().unwrap_or("").contains(keyword)
                || e.level.as_str().contains(keyword)
        }
        MatchField::All => {
            e.message.contains(keyword)
                && e.module.as_deref().unwrap_or("").contains(keyword)
                && e.level.as_str().contains(keyword)
        }
    })
}
```

跑 `cargo test -p luatos-cli match_keyword`, 预期: 4 passed.

- [ ] **Step 3: 把 `TrunRunArgs` 加 `--match-field` 字段**

`crates/luatos-cli/src/cmd_trun.rs:51-108` `TrunRunArgs` 内, 在 `keywords` 字段附近加:

```rust
    /// 关键字匹配字段 (默认 message, 真实解析结果)
    #[arg(long, value_enum, default_value_t = MatchField::Message)]
    pub match_field: MatchField,
```

跑 `cargo check -p luatos-cli`, 预期: 编译过, MatchField 已被 clap 接受。

- [ ] **Step 4: 把 `capture_log` 返回值改为 `Vec<LogEntry>`**

当前 `capture_log` (cmd_trun.rs:518):
```rust
fn capture_log(...) -> Result<(Vec<String>, Vec<SmartDiagnosticEntry>)> {
    // ...
    Ok((outcome.lines, smart_entries))
}
```

改为:
```rust
fn capture_log(...) -> Result<(Vec<luatos_log::LogEntry>, Vec<SmartDiagnosticEntry>)> {
    // ...
    Ok((outcome.entries, smart_entries))
}
```

并删掉 `outcome.lines` 的引用, 同步用 `outcome.entries`。

- [ ] **Step 5: 改 `cmd_trun_run` 的调用方, 用 `match_keyword` 替代 `evaluate_keywords`**

`cmd_trun_run` (cmd_trun.rs:362-374) 当前:
```rust
    let (extra_lines, smart_diag) = capture_log(args, &boot_lines, timeout, format, &cancel)?;
    let mut all_lines = boot_lines;
    all_lines.extend(extra_lines);
    phase_durations.insert(Phase::LogCapture.as_str().to_string(), start.elapsed().as_millis() as u64);

    // 8. 阶段：finalize
    let start = Instant::now();
    let keyword_hits = evaluate_keywords(&all_lines, &keywords);
    let fail_keyword_hits = evaluate_keywords(&all_lines, &fail_keywords);
```

改为:
```rust
    let (extra_entries, smart_diag) = capture_log(args, &boot_lines, timeout, format, &cancel)?;
    let mut all_entries = boot_entries;
    all_entries.extend(extra_entries);
    phase_durations.insert(Phase::LogCapture.as_str().to_string(), start.elapsed().as_millis() as u64);

    // 8. 阶段：finalize
    let start = Instant::now();
    let keyword_hits: Vec<KeywordHit> = keywords
        .iter()
        .map(|k| KeywordHit {
            keyword: k.clone(),
            found: match_keyword(&all_entries, k, args.match_field),
        })
        .collect();
    let fail_keyword_hits: Vec<KeywordHit> = fail_keywords
        .iter()
        .map(|k| KeywordHit {
            keyword: k.clone(),
            found: match_keyword(&all_entries, k, args.match_field),
        })
        .collect();
```

注意: `boot_entries: Vec<LogEntry>` 这个变量需要从 `flash_device` 返回值来。当前 `flash_device` (cmd_trun.rs:500) 返回 `Vec<String>`, 也需要改:

```rust
fn flash_device(...) -> Result<Vec<luatos_log::LogEntry>> {
    if cancel.load(Ordering::Relaxed) {
        bail!("cancelled before flash");
    }
    // ... flash 调用不变 ...
    // 简化: boot_entries 暂时为空, 由后续 log_capture 阶段抓取
    Ok(Vec::new())
}
```

把 `cmd_trun_run` 里 `let boot_lines = flash_device(...)?;` 改为 `let boot_entries = flash_device(...)?;`, 同步用 `boot_entries`。

- [ ] **Step 6: 改 `boot_log_count` 填 `all_entries.len()`**

`TrunOutcome` 构造处:
```rust
        boot_log_count: all_lines.len(),  // 旧
```
改为:
```rust
        boot_log_count: all_entries.len(),
```

- [ ] **Step 7: 删 `evaluate_keywords` 旧函数** (cmd_trun.rs:544)

```rust
fn evaluate_keywords(lines: &[String], keywords: &[String]) -> Vec<KeywordHit> { ... }
```

整段删掉, 因为没人再调它。**搜索**:
```bash
grep -n "evaluate_keywords" crates/luatos-cli/src/cmd_trun.rs
```
预期: 0 匹配 (已删干净)。

- [ ] **Step 8: 跑全 workspace 测试 + clippy + fmt**

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all
```

预期: 全过, 无 warning.

- [ ] **Step 9: 烟测 `trun run` 不接硬件, 确认 CLI 参数被 clap 接住**

```bash
cd D:/github/luatos-cli.worktrees/<branch> && \
  ./target/release/luatos-cli.exe trun run --help 2>&1 | grep -A1 "match-field"
```

预期: 看到 `--match-field <MATCH_FIELD>` 帮助行, 列出 `message / raw / any / all`。

- [ ] **Step 10: 提交**

```bash
git add crates/luatos-cli/src/cmd_trun.rs
git -c user.name="Wendal Chen" -c user.email="wendal1985@gmail.com" commit -m "feat(trun): 关键字匹配升级为 LogEntry 多字段模式, 默认走 message

evaluate_keywords 旧实现只对 raw 字符串 .contains, 无法利用
LogDispatcher 解析产物 (message/module/level). bk72xx 文本日志
的 SOH+len+type 帧头在 raw 里, 关键字若落在帧头位置会 false
negative。

本任务:
  - 新增 MatchField enum (clap ValueEnum): message / raw / any / all
  - 新增 match_keyword(entries, keyword, field) 函数, 按字段模式匹配
  - TrunRunArgs 加 --match-field 参数, 默认 message
  - capture_log 改返回 Vec<LogEntry> 替代 Vec<String>
  - TrunOutcome.boot_log_count 填 entries.len()

默认走 message 立即解决 bk72xx 帧头 false-negative, raw 模式
保留为降级路径, any/all 模式给按 module/level 过滤留口子。"
```

---

## Task 3: V2017 真机回归, 验证 match-field 默认 message 生效

**Files:** 不改代码, 纯执行验证.

**前置条件:**
- Air8101 已上电, USB 串口 COM7
- SOC: `D:/bk7258/LuatOS-SoC_V2017_Air8101.soc` (V2017 上轮验证过, 269 行, gmssl 整体 pass)
- gmssl testcase: `D:/github/LuatOS/testcase/unit_testcase_tools/gmssl`
- 编译 release: `target/release/luatos-cli.exe`

### Steps

- [ ] **Step 1: 跑默认 (message 模式) 一遍**

```bash
cd D:/github/luatos-cli.worktrees/<branch> && \
  ./target/release/luatos-cli.exe trun run gmssl \
    --luatos-root "D:/github/LuatOS" \
    --soc    "D:/bk7258/LuatOS-SoC_V2017_Air8101.soc" \
    --port   COM7 \
    --keep-soc "D:/github/luatos-cli/logs/trun_match_field" \
    --keyword     "### OVERALL_PASS ###" \
    --fail-keyword "### OVERALL_FAIL ###" \
    --timeout 90 \
    --full-soc \
    --rts-reset \
    --format json 2>&1 | tee logs/trun_match_field/default.log | tail -30
```

预期: `verdict=pass`, `keywords[0].found=true`, `boot_log_count` 在 269 附近 (上轮 269, 这次可能略多略少但量级一致)。

- [ ] **Step 2: 跑 raw 模式对比, 证明 message 更干净**

```bash
cd D:/github/luatos-cli.worktrees/<branch> && \
  ./target/release/luatos-cli.exe trun run gmssl \
    --luatos-root "D:/github/LuatOS" \
    --soc    "D:/bk7258/LuatOS-SoC_V2017_Air8101.soc" \
    --port   COM7 \
    --keep-soc "D:/github/luatos-cli/logs/trun_match_field" \
    --keyword     "### OVERALL_PASS ###" \
    --fail-keyword "### OVERALL_FAIL ###" \
    --timeout 90 \
    --full-soc \
    --rts-reset \
    --match-field raw \
    --format json 2>&1 | tee logs/trun_match_field/raw.log | tail -30
```

预期: 同样 `verdict=pass`, 关键字也命中 (因为 "### OVERALL_PASS ###" 在 raw 末尾, message 也有)。这一遍是"应该不挂"——证明 raw 降级路径仍工作。

- [ ] **Step 3: 跑 any 模式 + 匹配 module name, 证明 any 真在用 module 字段**

```bash
cd D:/github/luatos-cli.worktrees/<branch> && \
  ./target/release/luatos-cli.exe trun run gmssl \
    --luatos-root "D:/github/LuatOS" \
    --soc    "D:/bk7258/LuatOS-SoC_V2017_Air8101.soc" \
    --port   COM7 \
    --keep-soc "D:/github/luatos-cli/logs/trun_match_field" \
    --keyword     "user.testrunner" \
    --fail-keyword "### OVERALL_FAIL ###" \
    --timeout 90 \
    --full-soc \
    --rts-reset \
    --match-field any \
    --format json 2>&1 | tee logs/trun_match_field/any_module.log | tail -30
```

预期: `verdict=pass`, `keywords[0].found=true` (因为 module 字段含 "user.testrunner", any 模式命中 module)。

- [ ] **Step 4: 跑 message 模式 + 故意不存在的关键字, 证明 false positive 不会再出**

```bash
cd D:/github/luatos-cli.worktrees/<branch> && \
  ./target/release/luatos-cli.exe trun run gmssl \
    --luatos-root "D:/github/LuatOS" \
    --soc    "D:/bk7258/LuatOS-SoC_V2017_Air8101.soc" \
    --port   COM7 \
    --keep-soc "D:/github/luatos-cli/logs/trun_match_field" \
    --keyword     "DEFINITELY_NOT_IN_ANY_LOG_2026" \
    --fail-keyword "### OVERALL_FAIL ###" \
    --timeout 90 \
    --full-soc \
    --rts-reset \
    --format json 2>&1 | tee logs/trun_match_field/negative.log | tail -30
```

预期: `verdict=fail`, `keywords[0].found=false` (默认 message 模式找不到这个字符串)。这说明 message 模式没把所有行都当作"包含任意字符"误判, 行为是精确的。

- [ ] **Step 5: 写 Verification Log 追加到本计划文件**

在本文件 `## Verification Log` 一节追加:

```markdown
### 2026-06-27 V2017 match-field 验证 (Task 3)

- 默认 (--match-field message, 隐式):
  - 命令: `trun run gmssl --soc V2017 --match-field <default> ...`
  - verdict: pass
  - boot_log_count: N
  - 关键字命中: `### OVERALL_PASS ###` = true

- --match-field raw:
  - verdict: pass
  - 关键字命中: `### OVERALL_PASS ###` = true (降级路径仍工作)

- --match-field any --keyword "user.testrunner":
  - verdict: pass
  - 关键字命中: true (module 字段命中, 证明 any 真用 module)

- --match-field message --keyword "DEFINITELY_NOT_IN_ANY_LOG_2026":
  - verdict: fail
  - 关键字命中: false (精确匹配, 无 false positive)
```

- [ ] **Step 6: 合到 main, 清理 worktree**

```bash
cd D:/github/luatos-cli && \
  git -c user.name="Wendal Chen" -c user.email="wendal1985@gmail.com" \
    merge --no-ff <branch> -m "merge: trun 关键字匹配走 LogEntry 字段 (message/raw/any/all)" && \
  git worktree remove --force D:/github/luatos-cli.worktrees/<branch> && \
  git branch -d <branch>
```

---

## Verification Log

### 2026-06-28 V2017 match-field 验证 (Task 3)

设备: Air8101 @ COM7, SOC = `D:/bk7258/LuatOS-SoC_V2017_Air8101.soc`, testcase = gmssl, branch = `fix/trun-match-field` (on T2 4a0c929).

- 默认 (--match-field message, 隐式):
  - 命令: `trun run gmssl --soc V2017 ... --keyword "### OVERALL_PASS ###" --fail-keyword "### OVERALL_FAIL ###"`
  - verdict: pass
  - boot_log_count: 273
  - 关键字命中: `### OVERALL_PASS ###` = true
  - fail_keywords[0].found (OVERALL_FAIL) = false

- --match-field raw:
  - verdict: pass
  - boot_log_count: 269
  - 关键字命中: `### OVERALL_PASS ###` = true (降级路径仍工作)
  - fail_keywords[0].found (OVERALL_FAIL) = false

- --match-field any --keyword "user.testrunner":
  - verdict: pass
  - boot_log_count: 137 (因为 keyword 早命中, 提早收尾)
  - 关键字命中: `user.testrunner` = true (module 字段命中, 证明 any 真用 module)
  - fail_keywords[0].found (OVERALL_FAIL) = false

- --match-field message --keyword "DEFINITELY_NOT_IN_ANY_LOG_2026":
  - verdict: fail
  - boot_log_count: 288
  - 关键字命中: `DEFINITELY_NOT_IN_ANY_LOG_2026` = false (精确匹配, 无 false positive)
  - fail_keywords[0].found (OVERALL_FAIL) = false

结论: 4 个 case 全部符合预期, message 默认值生效, any 真在用 module 字段, message 模式无 false positive。
日志: `D:/github/luatos-cli/logs/trun_match_field/{default,raw,any_module,negative}.log`
