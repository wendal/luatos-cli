# luatos-cli

LuatOS 命令行工具集 — 刷机、日志、项目管理、固件合成。纯 Rust 实现，无外部依赖。

[![CI](https://github.com/wendal/luatos-cli/actions/workflows/ci.yml/badge.svg)](https://github.com/wendal/luatos-cli/actions/workflows/ci.yml)

## 功能特性

- **多芯片刷机** — 支持 Air8101 (BK7258)、Air6208 (XT804)、Air1601 (CCM4211) 等模组
- **分区操作** — 全量刷机、单刷脚本区、烧录/清除文件系统、清除 FSKV
- **闭环测试** — `flash test` 刷机后自动抓取 boot log 并验证关键字 (PASS/FAIL)
- **日志系统** — 实时查看/录制/解析，支持文本和 SOC 二进制两种格式
- **项目管理** — TOML 配置、脚手架、多目录脚本支持
- **构建系统** — Lua 编译 (luac)、LuaDB 打包、BK CRC16 适配
- **SOC 管理** — 解包/打包/查看 SOC 固件包 (ZIP + 7z，纯 Rust)
- **AI 友好** — 全局 `--format json` 输出，方便工具链集成

## 安装

```bash
# 从源码构建
cargo build --release

# 二进制位于 target/release/luatos-cli.exe
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

# 单刷脚本区 (开发最常用)
luatos-cli flash script --soc firmware.soc --port COM6 --script lua/

# 多目录脚本刷写
luatos-cli flash script --soc firmware.soc --port COM6 --script lua/ --script lib/

# 清除文件系统 / FSKV
luatos-cli flash clear-fs --soc firmware.soc --port COM6
luatos-cli flash clear-kv --soc firmware.soc --port COM6

# 闭环刷机测试 (刷机 → 抓日志 → 验证关键字 → PASS/FAIL)
luatos-cli flash test --soc firmware.soc --port COM6 --timeout 15 --keyword "LuatOS@"

# ── 日志 ──

# 实时查看串口日志
luatos-cli log view --port COM6 --baud 921600

# 查看 SOC 二进制日志 (Air6208 等)
luatos-cli log view-binary --port COM7 --baud 2000000

# 查看 Air1601 日志 (需要 --probe 触发固件输出)
luatos-cli log view-binary --port COM11 --baud 2000000 --probe

# 录制日志到文件 (含 JSON 解析)
luatos-cli log record --port COM6 --output ./logs/ --json

# ── 项目管理 ──

# 创建新项目
luatos-cli project new my-project --chip air8101

# 查看项目信息
luatos-cli project info

# ── 构建 ──

# 编译 Lua 脚本
luatos-cli build luac --src lua/ --bitw 32

# 合成 LuaDB 文件系统镜像
luatos-cli build filesystem --src lua/ --src lib/ --output build/script.bin --luac --bkcrc

# ── JSON 输出 (AI 友好) ──
luatos-cli --format json serial list
luatos-cli --format json flash test --soc firmware.soc --port COM6
```

## 支持的模组

> 以下结果基于 2026-04-11 实机测试验证

| 模组 | 芯片 | 刷机 | 脚本区 | 文件系统 | FSKV | 日志 | 闭环测试 |
|------|------|:----:|:------:|:--------:|:----:|:----:|:--------:|
| Air8101 | BK7258 (bk72xx) | ✅ | ✅ | ✅ | ✅ | 文本 | ✅ |
| Air6208 | XT804 (air6208) | ✅ | ✅ | — | — | 二进制 | ✅ |
| Air101/103 | XT804 | ✅ | ✅ | — | — | 二进制 | ✅ |
| Air1601 | CCM4211 | ✅ | ✅ | ✅ | ✅ | 二进制 (--probe) | ✅ |

<details>
<summary>详细测试结果 (点击展开)</summary>

| 模组 | 操作 | 结果 | 备注 |
|------|------|:----:|------|
| Air8101 | `flash run` (全量刷机) | ✅ | 通过 air602_flash.exe 子进程，约 37s |
| Air8101 | `flash script` (刷脚本区) | ✅ | LuaDB 612B, bkcrc=true, luac bitw=64 |
| Air8101 | `flash clear-fs` (清文件系统) | ✅ | 638 扇区擦除 |
| Air8101 | `flash clear-kv` (清 FSKV) | ✅ | 16 扇区擦除 |
| Air8101 | `flash flash-fs` (烧文件系统) | ✅ | LuaDB 打包写入文件系统分区 |
| Air8101 | `flash test` (闭环测试) | ✅ | 关键字 "LuatOS@" 在启动日志中匹配 |
| Air8101 | `log view` (文本日志) | ✅ | 2000000 baud 实时查看 |
| Air6208 | `flash run` (全量刷机) | ✅ | XMODEM-1K, 1974 块, 2M baud |
| Air6208 | `flash script` (刷脚本区) | ✅ | LuaDB 566B, luac bitw=64 |
| Air6208 | `flash test` (闭环测试) | ✅ | 二进制日志中部分文本匹配 |
| Air6208 | `log view-binary` (二进制日志) | ✅ | SOC 二进制帧解码正确 |
| Air1601 | `flash run` (全量刷机) | ✅ | ISP→SOC 协议，bootloader+core MD5 校验 |
| Air1601 | `flash script` (刷脚本区) | ✅ | LuaDB 594B, luac bitw=64 |
| Air1601 | `flash clear-fs` (清文件系统) | ✅ | 擦除 0x14D00000 分区 |
| Air1601 | `flash clear-kv` (清 FSKV) | ✅ | 擦除 0x14FF0000 分区 |
| Air1601 | `flash test` (闭环测试) | ✅ | SOC 二进制日志 + 探测帧，31 行日志 |
| Air1601 | `log view-binary --probe` | ✅ | 2Mbps, 探测帧触发日志输出 |

</details>

## 架构

```
luatos-cli (workspace, 7 crates)
├── luatos-cli/       # 主入口 CLI (clap)
├── luatos-flash/     # 刷机协议 (BK7258 + XT804 + CCM4211)
├── luatos-soc/       # SOC 文件解包/打包 (ZIP + 7z)
├── luatos-luadb/     # LuaDB 脚本打包 + Lua 编译 + BK CRC16
├── luatos-serial/    # 串口枚举 + 文本/二进制日志流
├── luatos-project/   # 项目管理 (TOML 配置)
└── luatos-log/       # 日志解析 (可扩展 LogParser trait)
```

### 关键设计

- **纯 Rust** — 7z 解压/压缩使用 `sevenz-rust2`，无需外部 exe
- **可扩展日志** — 实现 `LogParser` trait 即可添加新模组格式
- **多目录脚本** — `script_dirs` 支持单字符串或数组，后目录覆盖前目录同名文件
- **进度回调** — `ProgressCallback` 支持 Text/JSON 双格式输出

## 日志解析器扩展

实现 `LogParser` trait 即可添加新模组的日志格式：

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

内置解析器: `LuatosParser` (标准文本)、`BootLogParser` (BK72xx 开机)、`SocLogDecoder` (SOC 二进制帧)

## 项目配置

项目使用 `luatos-project.toml` 配置文件：

```toml
[project]
name = "my-project"
chip = "bk72xx"
version = "0.1.0"

[build]
script_dirs = ["lua/", "lib/"]   # 支持多目录，也可写 script_dir = "lua/"
output_dir = "build/"
use_luac = true
bitw = 32                        # air6208/air101 使用 64

[flash]
soc_file = "firmware.soc"
port = "COM6"
```

## 开发

```bash
cargo test                                     # 运行单元测试 (52 tests)
cargo test -- --ignored --nocapture            # 运行硬件测试
cargo build --release                          # 发布构建
```

## License

MIT
