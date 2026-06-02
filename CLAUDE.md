# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 项目概述

luatos-cli 是 LuatOS 的纯 Rust 命令行工具集，覆盖刷机、日志、项目管理、固件资源下载与构建、FOTA 打包、多芯片支持（Air8101/BK7258、Air6208/XT804、Air1601/Air1602/CCM4211、Air8000/EC718、SF32LB58）。

## 常用命令

```bash
# 构建
cargo build --release -p luatos-cli            # 发布构建（README 推荐方式）
cargo build --workspace                        # 整个 workspace 编译

# 测试
cargo test --workspace                         # 所有单元测试
cargo test -p <crate>                          # 单个 crate
cargo test <test_name_>                        # 按测试名模式（支持子串匹配）

# 静态检查（CI 强制）
cargo fmt --all -- --check                     # 格式检查
cargo fmt                                      # 自动修复
cargo clippy --workspace -- -D warnings        # clippy（CI 用 -D warnings 严格模式）

# 快速开发
cargo check --workspace                        # 仅类型检查
```

Linux 上 `serialport` 需要系统依赖：`libudev-dev` 与 `pkg-config`（CI 也安装）。Windows 上正常。

## 项目结构

Cargo workspace（`Cargo.toml`）包含 12 个 crate（`luatos-gui` 注释禁用，不参与构建）：

- **crates/luatos-cli** — 二进制入口。`main.rs` 用 clap 派生宏定义子命令树（`serial` / `soc` / `flash` / `log` / `project` / `build` / `resource` / `device` / `doctor` / `fota` / `guide` / `version`）。各 `cmd_*.rs` 模块实现业务逻辑，全局 `--format text|json|jsonl` 决定输出形式。
- **crates/luatos-flash** — 刷机协议：BK7258（Air8101）、XT804（Air6208/Air101/Air103）、CCM4211（Air1601/Air1602）、EC718（Air8000/Air780）、SF32LB5x、Air6201 外置 SPI。每个芯片一个模块，公共类型为 `FlashProgress` 与 `ProgressCallback = Box<dyn Fn(&FlashProgress) + Send>`。`sf32lb5x` 通过 git 依赖的 `sftool-lib`（fork 仓库）实现。
- **crates/luatos-soc** — SOC 固件包（ZIP+7z）解析、解包、打包、注入（`combine.rs`）、OTA 包生成。`pack.rs` 包含 LZMA 库（`lzma_ffi.c` + `lzma_sdk/`）的 FFI 绑定。
- **crates/luatos-luadb** — LuaDB 文件系统镜像打包，含内置 Lua 5.3 编译器（`build.rs` 走 `cc` 编译 `embedded_helpers.rs`）与 BK CRC16 framing。
- **crates/luatos-serial** — 串口枚举、文本/二进制日志流。
- **crates/luatos-project** — 项目脚手架、导入（LuaTools `.ini`/`.luatos`）、Lua 依赖分析（`lua_deps.rs`）、配置 `luatos-project.toml`、向导（`wizard.rs`）、分析（`analyze.rs`）、打包（`archive.rs`）、模板（`template.rs`）。
- **crates/luatos-resource** — 从 LuatOS CDN 拉取固件清单、下载固件资源（带 SHA256 校验）。
- **crates/luatos-log** — 日志解析框架。核心 trait 是 `LogParser`（`name()` + `parse_line()`），内置 `LuatosParser` / `BootLogParser` / `SocLogDecoder`，`LogDispatcher` 按注册顺序尝试解析。`smart.rs` 提供智能诊断。
- **crates/luatos-mcp** — MCP 服务端（独立二进制 `main.rs`），供 AI 工具调用。
- **crates/luatos-mfgui** — 量产刷机 GUI（egui 0.34），多 worker 并行刷机。
- **crates/sftool-lib** — 本地 fork 路径补丁，覆盖 `sftool` git 依赖，去掉 `probe-rs` 依赖。

## 关键约定

### 错误处理
所有 crate 统一使用 `anyhow::Result`，**没有自定义错误枚举**。`bail!()` / `.context()` / `ensure!()` 即可。CI 强制 `clippy -D warnings`。

### 全局输出格式
`--format text|json|jsonl` 由 `main.rs` 的 `OutputFormat` enum 持有并传入每个 `cmd_*.rs` 函数。JSON 统一信封由 `event::emit_result` 发出：
```json
{ "status": "ok", "command": "<soc.info|flash.run|...>", "data": { ... } }
```
新增命令时必须同时支持两种输出。

### 进度回调
刷机操作通过 `ProgressCallback = Box<dyn Fn(&FlashProgress) + Send>` 上报进度。CLI 在 `cmd_flash.rs` 内构造 `make_progress_callback(format)`，按文本/JSON 两种风格渲染。`FlashProgress` 字段：`stage` / `region`（可选分区名）/ `percent` / `message` / `done` / `error`。

### 增量功能维护
每次新增功能（新命令、新芯片支持、扩展已有命令）后**必须**同步更新：
1. `README.md` 的"功能特性"小节（如有新能力）
2. `README.md` 的"快速开始"示例
3. `README.md` 的"支持的模组"表格（`—` → `✅`）
4. `CHANGELOG.md` 新增版本条目

### 新增芯片刷机协议
1. 在 `luatos-flash/src/` 下创建新模块，公开接收 `port`/`SOC info`/`ProgressCallback` 的顶层函数。
2. 在 `luatos-cli/src/main.rs` 添加 `FlashCommands` 变体和 `match` 分支。
3. 在 `docs/` 下新增 `*flash-protocol.md`（参考 `air8101-flash-protocol.md` 等）。
4. 更新 `README.md` 模组支持表。
5. **CI 不做硬件测试**，需在真实硬件上验证刷机、日志、闭环测试。

### 新增日志解析器
实现 `LogParser` trait 并向 `LogDispatcher` 注册。

### 测试
- 所有测试为内联 `#[cfg(test)]` 模块，无独立 `tests/` 单元测试目录。
- `tests/` 下放的是真实的 Lua 脚本测试夹具（`air1601_test_script/`、`air6208_test_scripts/`、`air8000_test_script/`），用于刷机后端到端验证。
- 文件系统测试用 `tempfile::TempDir`。
- CI 跑 `cargo test --workspace`，无硬件依赖。

## 模型与 CI

- `rustfmt.toml` 设置 `max_width = 180`。
- CI（`.github/workflows/ci.yml`）三作业：ubuntu/windows/macos 矩阵跑 `check` + `build` + `test`；`clippy -D warnings`；`fmt --check`。
- 文档与协议说明（`docs/air8101-flash-protocol.md`、`docs/ccm4211-flash-protocol.md` 等）是规格来源，新协议开发前先读对应文档。
- 型号文档按芯片族拆分到 `docs/models/`：`air1601-air1602.md` / `air8000-ec7xx.md` / `air8101-bk72xx.md` / `air6208-xt804.md` / `sf32lb58.md`。
- `docs/superpowers/` 下有 plans/ 与 specs/，是设计文档与实施计划。

## 语言

代码注释、提交信息、文档（README、CHANGELOG、协议文档）**全部使用中文**。新代码与新文档保持一致。
