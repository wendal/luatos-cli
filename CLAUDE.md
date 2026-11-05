# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 项目概述

luatos-cli 是 LuatOS 的纯 Rust 命令行工具集，覆盖刷机、日志、项目管理、固件资源下载与构建、FOTA 打包、多芯片支持（Air8101/BK7258、Air6208/XT804、Air1601/Air1602/Air1603/Air1604/CCM4211、Air8000/EC718）。

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

Cargo workspace（`Cargo.toml`）包含 12 个 crate：

- **crates/luatos-cli** — 二进制入口。`main.rs` 用 clap 派生宏定义子命令树（`serial` / `soc` / `flash` / `log` / `project` / `build` / `resource` / `device` / `doctor` / `fota` / `guide` / `trun` / `hzv` / `version`）。各 `cmd_*.rs` 模块实现业务逻辑，全局 `--format text|json|jsonl` 决定输出形式；关键模块：`keyword.rs`（`flash test` 与 `trun` 共用的关键字判定）、`cmd_trun.rs`（testcase 单点调试）、`cmd_doctor.rs`（环境诊断）、`cmd_guide.rs`（型号二级帮助）、`cmd_hzv.rs`（HZV 独立子命令入口）。
- **crates/luatos-flash** — 刷机协议：BK7258（Air8101）、XT804（Air6208/Air101/Air103）、CCM4211（Air1601/Air1602/Air1603/Air1604）、EC718（Air8000/Air780）、Air6201 外置 SPI。每个芯片一个模块，公共类型为 `FlashProgress` 与 `ProgressCallback = Box<dyn Fn(&FlashProgress) + Send>`。`raw.rs` 的 `RawChip` 是任意地址读写协议的能力表；`RawChip::from_family` 是 `ChipFamily → raw 协议` 的唯一映射入口。SOC 包内资源（rom.bin、fdl、ramrun 等）一律经 `luatos_soc::SocArchive` 纯内存读取，不再解压到临时目录或调用外部刷机 exe。
- **crates/luatos-soc** — SOC 固件包（ZIP+7z）解析、解包、打包、注入（`combine.rs`）、OTA 包生成。`ota.rs` 含 LZMA SDK 的 FFI 绑定（`lzma_sdk_compress`），`pack.rs` 为纯 Rust ZIP/7z 打包。
- **crates/luatos-luadb** — LuaDB 文件系统镜像打包，含内置 Lua 5.3 编译器（`build.rs` 走 `cc` 编译 `embedded_helpers.rs`）与 BK CRC16 framing。
- **crates/luatos-codec** — 零依赖的协议基础算法（Beken CRC16 / 32+2 帧化 / 解帧）。`luatos-luadb` 与 `luatos-soc` 共用，避免同一算法两份实现。
- **crates/luatos-serial** — 串口枚举、文本/二进制日志流。
- **crates/luatos-project** — 项目脚手架、导入（LuaTools `.ini`/`.luatos`）、Lua 依赖分析（`lua_deps.rs`）、配置 `luatos-project.toml`、向导（`wizard.rs`）、分析（`analyze.rs`）、打包（`archive.rs`）、模板（`template.rs`）。
- **crates/luatos-resource** — 从 LuatOS CDN 拉取固件清单、下载固件资源（带 SHA256 校验）。
- **crates/luatos-log** — 日志解析框架。核心 trait 是 `LogParser`（`name()` + `parse_line()`），内置 `LuatosParser` / `BootLogParser` / `SocLogDecoder`，`LogDispatcher` 按注册顺序尝试解析。`smart.rs` 提供智能诊断。
- **crates/luatos-log-ffi** — C ABI 动态库（cdylib），导出 `luatos_soclog_analyze()` 与 `pySoclogAnalyze()` 别名，供 Python/C/Go 等语言直接消费 SOC 日志解码。
- **crates/luatos-testcase** — testcase 解析与发现（metas / discovery / ctx.json 合并 / script.bin 构建），承载 `trun` 的独立可测逻辑（path crate, lib only）。
- **crates/luatos-hzv** — HZV v1 音视频容器（MJPEG + MP3/PCM）的转换、封装与校验。作为**独立子命令 `hzv`** 保留，不并入 flash/project 链路。

## 关键约定

### 错误处理
所有 crate 统一使用 `anyhow::Result`，**没有自定义错误枚举**。`bail!()` / `.context()` / `ensure!()` 即可。CI 强制 `clippy -D warnings`。

### 芯片族判定的边界
`luatos_soc::ChipFamily`（`crates/luatos-soc/src/chip.rs`）是 **chip_type → 芯片族** 归一化的唯一来源，新增型号别名只加在这里。但下列字符串表**有意不经过 ChipFamily**，不要顺手「收敛」：

- `luatos-soc/src/combine.rs::is_ec7xx`：binpkg 注入的**格式能力白名单**，只覆盖 EC7xx 新格式；EC618 / EC716S 的 binpkg 结构不同，换成 `ChipFamily::Ec718` 会放行不支持的包。
- `luatos-flash/src/ec718.rs::EigencommChip::from_soc`：区分 EC618 / EC716S / EC718 三种协议变体，还参考 `package_chip`；ChipFamily 把三者都归为 `Ec718`，无法表达。
- `luatos-cli/src/cmd_flash.rs`（显示名）、`cmd_fota.rs`（dtools 变体与配置文件名）、`cmd_guide.rs`（型号→文档映射）、`luatos-project/src/wizard.rs`（从**文件名**推断）：都是「同族细分」或展示层映射，不是芯片族判定。
- **不要合并 `RawChip` 与 `ChipFamily`**：前者只覆盖支持任意地址读写的三套协议，后者是芯片归一化。`ChipFamily → RawChip` 只走 `luatos_flash::raw::RawChip::from_family`；`flash raw` / `flash dump` 的能力校验（尤其 `supports_read()`）必须在开串口之前完成。
- 端口归一化统一走 `luatos_flash::device::normalize_serial_port`（`auto` / 空串 / 大小写 / trim 的单一入口）。
- 脚本镜像容量校验统一走 `luatos_flash::ensure_image_fits`：刷机路径传芯片实际写入对齐，离线构建 `build filesystem` 传 1。
- BK CRC16 帧化统一走 `luatos_codec`，不要在 `luatos-luadb` 与 `luatos-soc` 里各写一份。

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
- 型号文档按芯片族拆分到 `docs/models/`：`air1601-air1604.md` / `air8000-ec7xx.md` / `air8101-bk72xx.md` / `air6208-xt804.md`。
- `docs/superpowers/` 下有 plans/ 与 specs/，是设计文档与实施计划。

## 语言

代码注释、提交信息、文档（README、CHANGELOG、协议文档）**全部使用中文**。新代码与新文档保持一致。
