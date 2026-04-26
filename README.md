# luatos-cli

LuatOS 命令行工具集 — 刷机、日志、项目管理、固件合成。纯 Rust 实现，无外部依赖。

[![CI](https://github.com/wendal/luatos-cli/actions/workflows/ci.yml/badge.svg)](https://github.com/wendal/luatos-cli/actions/workflows/ci.yml)

## 功能特性

- **多芯片刷机** — 支持 Air8101 (BK7258)、Air6208 (XT804)、Air1601 (CCM4211)、Air8000 (EC718)、Air8101/SF32LB58 等模组
- **分区操作** — 全量刷机、单刷脚本区、烧录/清除文件系统、清除 FSKV
- **闭环测试** — `flash test` 刷机后自动抓取 boot log 并验证关键字 (PASS/FAIL)
- **日志系统** — 实时查看/录制/解析，支持文本和 SOC 二进制两种格式，智能诊断分析
- **环境诊断** — `doctor` 一键检测串口、项目配置、固件、编译器、CDN 连通性
- **项目管理** — TOML 配置、脚手架、多路径脚本、LuaTools 项目导入、依赖分析
- **构建系统** — Lua 编译 (luac)、LuaDB 打包、BK CRC16 适配、调试信息控制
- **SOC 管理** — 解包/打包/查看 SOC 固件包 (ZIP + 7z，纯 Rust)
- **固件资源** — 从 LuatOS CDN 列出/下载固件资源 (SHA256 校验)
- **AI 友好** — 全局 `--format json|jsonl` 输出，兼容工具链集成与流式事件消费

## 安装

```bash
# 从源码构建 CLI
cargo build --release -p luatos-cli

# 二进制位于 target/release/luatos-cli.exe
```

如需 MCP server：

```bash
# 构建 MCP server
cargo build --release -p luatos-mcp

# 二进制位于 target/release/luatos-mcp.exe
```

## 快速开始

```bash
# 列出串口
luatos-cli serial list

# 查看 SOC 文件信息
luatos-cli soc info firmware.soc

# ── 刷机 ──

# 全量刷机 (Air8101/BK7258)
luatos-cli flash run --soc firmware.soc --port COM6

# 全量刷机 (Air1601/CCM4211)
luatos-cli flash run --soc firmware.soc --port COM11

# 全量刷机 (Air8000/EC718 — 自动检测USB端口并重启进入下载模式)
luatos-cli flash run --soc firmware.soc --port auto

# 单刷脚本区 (开发最常用)
luatos-cli flash script --soc firmware.soc --port COM6 --script lua/

# 多目录脚本刷写
luatos-cli flash script --soc firmware.soc --port COM6 --script lua/ --script lib/

# 清除文件系统 / FSKV
luatos-cli flash clear-fs --soc firmware.soc --port COM6
luatos-cli flash clear-kv --soc firmware.soc --port COM6

# 烧录文件系统（将脚本目录打包为 LuaDB 并写入 FS 分区）
luatos-cli flash flash-fs --soc firmware.soc --port COM7 --script lua/

# 闭环刷机测试 (刷机 → 抓日志 → 验证关键字 → PASS/FAIL)
luatos-cli flash test --soc firmware.soc --port COM6 --timeout 15 --keyword "LuatOS@"

# ── 日志 ──

# 实时查看串口日志
luatos-cli log view --port COM6 --baud 921600

# 智能分析模式 — 自动检测 OOM、Lua 错误、看门狗复位等常见问题
luatos-cli log view --port COM6 --baud 921600 --smart

# 查看 SOC 二进制日志 (Air6208 等)
luatos-cli log view-binary --port COM7 --baud 2000000

# 二进制日志 + 智能分析
luatos-cli log view-binary --port COM7 --baud 2000000 --smart

# 查看 Air1601 日志 (需要 --probe 触发固件输出)
luatos-cli log view-binary --port COM11 --baud 2000000 --probe

# 查看 Air8000/EC718 日志 (自动检测端口, --probe 触发输出)
luatos-cli log view-binary --port auto --probe

# 录制日志到文件 (含 JSON 解析)
luatos-cli log record --port COM6 --output ./logs/ --json

# ── 项目管理 ──

# 向导式创建项目（交互式，推荐）
luatos-cli project new my-project          # 省略 --chip 自动进入向导
luatos-cli project wizard                  # 显式进入向导

# 非交互式创建（全参数时跳过交互）
luatos-cli project wizard --name my-app --model Air8101 --template helloworld --no-git --no-download

# 老方式：指定 chip 直接创建（helloworld 模板）
luatos-cli project new my-project --chip air8101

# 查看项目信息
luatos-cli project info

# 导入 LuaTools 项目 (.ini)
luatos-cli project import path/to/project.ini --dir ./converted/

# 分析脚本依赖关系
luatos-cli project deps                       # 完整依赖报告
luatos-cli project deps --reachable            # 只看被引用的文件
luatos-cli project deps --unreachable          # 只看未使用的文件

# 读写项目配置
luatos-cli project config build.luac_debug     # 查看配置值
luatos-cli project config build.luac_debug true # 设置配置值

# ── 构建 ──

# 编译 Lua 脚本
luatos-cli build luac --src lua/ --bitw 32

# 合成 LuaDB 文件系统镜像
luatos-cli build filesystem --src lua/ --src lib/ --output build/script.bin --luac --bkcrc

# ── 固件资源 ──

# 列出可下载的模组
luatos-cli resource list

# 查看模组的固件版本
luatos-cli resource list Air8101

# 下载最新固件（三段式：category sub [item]）
luatos-cli resource download Air8101 V2032               # 下载整个版本
luatos-cli resource download Air8101 V2032 114           # 下载版本中含'114'的文件
luatos-cli resource download public soc_script           # 最新 soc_script 扩展库
luatos-cli resource download public soc_script v2026.04.10  # 指定版本

# ── 环境诊断 ──

# 一键诊断开发环境（串口、项目配置、固件、编译器、CDN）
luatos-cli doctor

# 指定项目目录诊断
luatos-cli doctor --dir ./my-project

# JSON 输出（适合脚本/CI 集成）
luatos-cli --format json doctor

# ── 结构化输出 (AI / MCP 友好) ──
luatos-cli --format json serial list
luatos-cli --format json flash test --soc firmware.soc --port COM6
luatos-cli --format json project deps

# JSONL 事件流输出（适合 MCP / agent 包装层）
luatos-cli --format jsonl flash run --soc firmware.soc --port COM6
luatos-cli --format jsonl log view-binary --port auto --probe
luatos-cli --format jsonl resource download Air8101 soc_script V2032 --output ./resource/
```

说明：
- `--format json`：保留原有最终结果 JSON envelope，兼容现有脚本/工具调用
- `--format jsonl`：按行输出结构化事件（进度、消息、日志条目、最终结果），适合流式集成

## MCP Server

`luatos-mcp` 是基于 LuatOS 库 crate 的 MCP server，查询类操作直接调用库函数（零延迟），硬件操作通过 `luatos-cli --format jsonl` 子进程。适合给 Copilot / Claude Desktop / Inspector 等客户端接入。

### 启动方式

```bash
# 直接运行（默认会优先寻找同目录下的 luatos-cli.exe）
target/release/luatos-mcp.exe

# 若 luatos-cli 不在同目录，可显式指定
set LUATOS_CLI_BIN=D:\path\to\luatos-cli.exe
target/release/luatos-mcp.exe
```

### 工具范围

当前内置 tools:

- `serial_list` — 库直调
- `soc_info` / `soc_files` / `soc_unpack` / `soc_pack` — 库直调
- `project_info` / `project_deps` / `project_analyze`
- `project_wizard` — 向导式创建项目（非交互模式）
- `build_luac` / `build_filesystem`
- `resource_list` — 库直调
- `resource_download`
- `device_reboot` / `device_boot` — 库直调，设备重启/进入下载模式
- `flash_run` / `flash_script` / `flash_test` — 子进程（硬件操作）
- `doctor` — 库直调，环境诊断

### 设计说明

- MCP tool 最终结果通过 `structured_content` 返回，保留 `status / command / data`
- `flash` 等长操作会把 `jsonl` 里的 `progress` 事件转发为 MCP progress notification
- 非结构化诊断信息仍保留在 stderr，不污染 MCP stdio 协议流

## 支持的模组

> 以下结果基于 2026-04-18 实机测试验证

| 模组 | 芯片 | 刷机 | 脚本区 | 文件系统 | FSKV | 日志 | 闭环测试 |
|------|------|:----:|:------:|:--------:|:----:|:----:|:--------:|
| Air8101 | BK7258 (bk72xx) | ✅ | ✅ | ✅ | ✅ | 文本 | ✅ |
| Air6208 | XT804 (air6208) | ✅ | ✅ | ✅ | ✅ | 二进制 | ✅ |
| Air101/103 | XT804 | ✅ | ✅ | — | — | 二进制 | ✅ |
| Air1601 | CCM4211 | ✅ | ✅ | ✅ | ✅ | 二进制 (--probe) | ✅ |
| Air8000 | EC718HM (ec7xx) | ✅ | ✅ | — | — | 二进制 (--probe) | ✅ |
| Air780E系列 | EC718 (ec7xx) | ✅ | ✅ | — | — | 二进制 (--probe) | ✅ |
| Air8101(SF32) | SF32LB58 | ✅ | ✅ | — | — | 文本 | — |

<details>
<summary>详细测试结果 (点击展开)</summary>

| 模组 | 操作 | 结果 | 备注 |
|------|------|:----:|------|
| Air8101 | ``flash run`` (全量刷机) | ✅ | 通过 air602_flash.exe 子进程，约 37s |
| Air8101 | ``flash script`` (刷脚本区) | ✅ | LuaDB 612B, bkcrc=true, luac bitw=64 |
| Air8101 | ``flash clear-fs`` (清文件系统) | ✅ | 638 扇区擦除 |
| Air8101 | ``flash clear-kv`` (清 FSKV) | ✅ | 16 扇区擦除 |
| Air8101 | ``flash flash-fs`` (烧文件系统) | ✅ | LuaDB 打包写入文件系统分区 |
| Air8101 | ``flash test`` (闭环测试) | ✅ | 关键字 "LuatOS@" 在启动日志中匹配 |
| Air8101 | ``log view`` (文本日志) | ✅ | 2000000 baud 实时查看 |
| Air6208 | ``flash run`` (全量刷机) | ✅ | XMODEM-1K, 1974 块, 2M baud |
| Air6208 | ``flash script`` (刷脚本区) | ✅ | LuaDB 566B, luac bitw=64 |
| Air6208 | ``flash clear-kv`` (清 FSKV) | ✅ | 65 blocks, 2M baud |
| Air6208 | ``flash clear-fs`` (清文件系统) | ✅ | 3329 blocks, 2M baud |
| Air6208 | ``flash flash-fs`` (烧文件系统) | ✅ | 3329 blocks, LuaDB 打包写入 FS 分区 |
| Air6208 | ``flash test`` (闭环测试) | ✅ | 二进制日志中部分文本匹配 |
| Air6208 | ``log view-binary`` (二进制日志) | ✅ | SOC 二进制帧解码正确 |
| Air1601 | ``flash run`` (全量刷机) | ✅ | ISP→SOC 协议，bootloader+core MD5 校验 |
| Air1601 | ``flash script`` (刷脚本区) | ✅ | LuaDB 594B, luac bitw=64 |
| Air1601 | ``flash clear-fs`` (清文件系统) | ✅ | 擦除 0x14D00000 分区 |
| Air1601 | ``flash clear-kv`` (清 FSKV) | ✅ | 擦除 0x14FF0000 分区 |
| Air1601 | ``flash test`` (闭环测试) | ✅ | SOC 二进制日志 + 探测帧，31 行日志 |
| Air1601 | ``log view-binary --probe`` | ✅ | 2Mbps, 探测帧触发日志输出 |
| Air8000 | ``flash run`` (全量刷机) | ✅ | USB自动进入boot模式, BL+AP+CP ~43s |
| Air8000 | ``flash script`` (刷脚本区) | ✅ | FlexFile类型, 594B脚本, addr=0xC8E000 |
| Air8000 | ``flash test`` (闭环测试) | ✅ | 0x7E HDLC日志解码, 921600 baud |
| Air8000 | ``log view-binary --probe`` | ✅ | USB接口2(x.2), DTR/RTS HIGH |
| Air8101(SF32) | ``flash run`` (全量刷机) | ✅ | sftool-lib, 手动进入 ROM BL, bootloader(44KB)+ftab(11KB)+app(579KB) |
| Air8101(SF32) | ``flash script`` (刷脚本区) | ✅ | sftool-lib, NAND @ 0x69800000 |
| Air8101(SF32) | ``log view`` (文本日志) | ✅ | COM12 @ 1000000 bps，LittleFS 挂载、Lua 脚本正常执行 |

</details>

## 架构

```
luatos-cli (workspace, 8 crates)
├── luatos-cli/           # 主入口 CLI (clap)
│   ├── main.rs           #   CLI 定义 + 分发 (~490 行)
│   ├── cmd_serial.rs     #   串口命令
│   ├── cmd_soc.rs        #   SOC 文件操作
│   ├── cmd_flash.rs      #   刷机 / 分区 / 闭环测试
│   ├── cmd_log.rs        #   日志查看 / 录制 / 解析
│   ├── cmd_project.rs    #   项目管理 / 导入 / 依赖分析
│   ├── cmd_build.rs      #   Lua 编译 / 文件系统打包
│   └── cmd_resource.rs   #   固件资源下载 (薄包装层)
├── luatos-flash/         # 刷机协议 (BK7258 + XT804 + CCM4211 + EC718)
├── luatos-soc/           # SOC 文件解包/打包 (ZIP + 7z)
├── luatos-luadb/         # LuaDB 脚本打包 + Lua 编译 + BK CRC16
├── luatos-serial/        # 串口枚举 + 文本/二进制日志流
├── luatos-project/       # 项目管理 (TOML 配置 + INI 导入 + 依赖分析)
│   ├── lib.rs            #   项目配置模型 (BuildConfig, FlashConfig)
│   ├── import.rs         #   LuaTools INI 解析与转换
│   └── lua_deps.rs       #   Lua require() 依赖图分析
├── luatos-resource/      # 固件资源清单 + 下载 (SHA256 校验, 回调通知)
└── luatos-log/           # 日志解析 (可扩展 LogParser trait)
```

### 关键设计

- **纯 Rust** — 7z 解压/压缩使用 `sevenz-rust2`，无需外部 exe
- **可扩展日志** — 实现 ``LogParser`` trait 即可添加新模组格式
- **多路径脚本** — ``script_dirs`` 支持多目录，``script_files`` 支持单文件路径
- **依赖分析** — 解析 ``require()`` 构建依赖图，自动识别 80+ 内置模块
- **LuaTools 兼容** — 支持导入 LuaTools 的 ``.ini`` 项目文件
- **调试信息控制** — ``luac_debug`` 控制编译时是否保留调试信息
- **进度回调** — ``ProgressCallback`` 支持 Text/JSON 双格式输出

## 日志解析器扩展

实现 ``LogParser`` trait 即可添加新模组的日志格式：

```rust
use luatos_log::{LogParser, LogEntry, LogLevel};

struct MyChipParser;
impl LogParser for MyChipParser {
    fn name(&self) -> &str { "my_chip" }
    fn parse_line(&self, line: &str) -> Option<LogEntry> {
        // 自定义解析逻辑
        todo!()
    }
}
```

内置解析器: ``LuatosParser`` (标准文本)、``BootLogParser`` (BK72xx 开机)、``SocLogDecoder`` (SOC 二进制帧)、``Ec718LogDecoder`` (EC718 0x7E HDLC 帧)

## 项目配置

项目使用 ``luatos-project.toml`` 配置文件：

```toml
[project]
name = "my-project"
chip = "bk72xx"
version = "0.1.0"

[build]
script_dirs = ["lua/", "lib/"]   # 支持多目录，也可写 script_dir = "lua/"
script_files = []                # 额外的单文件路径 (可选)
output_dir = "build/"
use_luac = true
bitw = 32                        # air6208/air101 使用 64
luac_debug = false               # true = 保留调试信息; false = 去除 (默认)
ignore_deps = false              # true = 忽略依赖分析，包含所有文件

[flash]
soc_file = "firmware.soc"
port = "COM6"
```

### 配置字段说明

| 字段 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| ``build.script_dirs`` | 字符串数组 | ``["lua/"]`` | 脚本目录列表 |
| ``build.script_files`` | 字符串数组 | ``[]`` | 额外的单文件路径 |
| ``build.luac_debug`` | 布尔 | ``false`` | 保留 Lua 调试信息 (行号、变量名) |
| ``build.ignore_deps`` | 布尔 | ``false`` | 跳过依赖分析，包含所有文件 |
| ``build.use_luac`` | 布尔 | ``true`` | 编译为 luac 字节码 |
| ``build.bitw`` | 整数 | ``32`` | Lua 整数位宽 (32 或 64) |
| ``flash.soc_file`` | 字符串 | — | SOC 固件路径 |
| ``flash.port`` | 字符串 | — | 串口号 (如 COM6) |
| ``flash.baud`` | 整数 | — | 自定义波特率 |

### LuaTools 项目导入

从 LuaTools 的 ``.ini`` 项目文件导入：

```bash
luatos-cli project import D:\LuaTools\project\air8101_demo.ini --dir ./my-project/
```

支持自动检测芯片类型、脚本目录、SOC 路径等。兼容 LuaTools 2.x/3.x 的项目格式。

### 依赖分析

分析 Lua 脚本的 ``require()`` 依赖，找出未使用的文件：

```
$ luatos-cli project deps
Dependency analysis for 'my-project'
  Total files:      5
  Reachable:        3
  Unreachable:      2
  External modules: 4

Dependencies:
  ✓ main.lua → net, ui
  ✓ net.lua → (none)
  ✓ ui.lua → (none)

Unreachable (can be excluded):
  ✗ old_test.lua
  ✗ debug_helper.lua

External/builtin modules:
  • sys
  • gpio
  • uart
  • log
```

## 开发

```bash
cargo test                                     # 运行单元测试 (99 tests)
cargo test -- --ignored --nocapture            # 运行硬件测试
cargo build --release                          # 发布构建
```

## License

MIT
