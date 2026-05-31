# luatos-cli

LuatOS 命令行工具集（纯 Rust）——刷机、日志、项目管理、固件资源与构建。

[![CI](https://github.com/wendal/luatos-cli/actions/workflows/ci.yml/badge.svg)](https://github.com/wendal/luatos-cli/actions/workflows/ci.yml)

## 功能特性

- **多芯片刷机**：Air8101(BK7258)、Air6208(XT804)、Air1601/Air1602(CCM4211)、Air8000(EC718)、SF32LB58
- **FOTA 打包**：支持 EC7xx 差分/脚本、CCM4211 全量/脚本、Air8101(BK72XX) 新格式全量/脚本
- **二级帮助入口**：`--help` 提示型号入口，`guide models` / `guide model --model <型号>` 直接给推荐命令
- **刷机后继续监听**：`flash run --tail-log-secs <N>` 刷机后自动按型号波特率续接日志，减少开机日志丢失
- **日志系统**：文本 / 二进制日志查看、录制、解析，支持智能诊断
- **项目与构建**：项目向导、配置管理、Lua 依赖分析、luac/LuaDB 构建
- **AI 友好输出**：全局 `--format text|json|jsonl`

## 安装

```bash
cargo build --release -p luatos-cli
```

## 快速开始

```bash
# 查看型号二级帮助
luatos-cli guide models
luatos-cli guide model --model air1601

# 常规刷机
luatos-cli flash run --soc firmware.soc --port COM10

# 刷机后继续监听启动日志（30 秒）
luatos-cli flash run --soc firmware.soc --port COM10 --tail-log-secs 30

# 文本日志
luatos-cli log view --port COM6 --baud 921600

# 二进制日志（Air1601/Air1602/EC718）
luatos-cli log view-binary --port COM10 --baud 2000000 --probe

# Air8101(BK72XX) 新格式脚本 FOTA
luatos-cli fota build --new air8101.soc --script-only -o air8101_script_fota.bin

# Air780EPM / Air780EHM / Air8000 仅脚本 FOTA
luatos-cli fota build --new firmware.soc --script-only
```

## 型号文档（按型号拆分）

- [Air1601 / Air1602 / CCM4211](docs/models/air1601-air1602.md)
- [Air8000 / EC7xx / Air780E 系列](docs/models/air8000-ec7xx.md)
- [Air8101 / BK72xx](docs/models/air8101-bk72xx.md)
- [Air6208 / XT804 / Air101/Air103](docs/models/air6208-xt804.md)
- [SF32LB58](docs/models/sf32lb58.md)

## 常用命令分组

```bash
luatos-cli serial list
luatos-cli soc info firmware.soc
luatos-cli flash run --help
luatos-cli flash script --help
luatos-cli flash test --help
luatos-cli log view --help
luatos-cli log view-binary --help
luatos-cli project wizard --help
luatos-cli doctor --help
```

### `flash test` 关键字判定说明

- 未传 `--keyword` 时，默认检查 `LuatOS@`
- 显式传入 `--keyword` 时，仅按传入关键字判定
- 可选 `--fail-keyword`（可重复）用于快速判失败：命中任意一项立即结束并判定 FAIL
- Air1601/Air1602 机型执行 `flash test --script <dir>` 时，会先全量刷机，再覆盖脚本分区后再抓日志判定关键字
- 结果为 `FAIL` 时会输出缺失关键字列表（JSON 输出为 `missing_keywords`）

## 支持的模组（摘要）

| 模组 | 芯片 | 刷机 | 脚本区 | 文件系统 | FSKV | FOTA | 日志 | 闭环测试 |
|------|------|:----:|:------:|:--------:|:----:|:----:|:----:|:--------:|
| Air8101 | BK7258 (bk72xx) | ✅ | ✅ | ✅ | ✅ | ✅（新格式全量/脚本） | 文本 | ✅ |
| Air6208 | XT804 (air6208) | ✅ | ✅ | ✅ | ✅ | ✅（全量） | 二进制 | ✅ |
| Air101/103 | XT804 | ✅ | ✅ | — | — | ✅（全量） | 二进制 | ✅ |
| Air1601 / Air1602 | CCM4211 | ✅ | ✅ | ✅ | ✅ | ✅（全量/脚本） | 二进制 (`--probe`) | ✅ |
| Air8000 / Air780E | EC718 (ec7xx) | ✅ | ✅ | — | — | ✅（差分/脚本） | 二进制 (`--probe`) | ✅ |
| Air8101(SF32) | SF32LB58 | ✅ | ✅ | — | ✅ | — | 文本 | — |

## 结构化输出

```bash
luatos-cli --format json serial list
luatos-cli --format json flash test --soc firmware.soc --port COM6
luatos-cli --format jsonl flash run --soc firmware.soc --port COM10 --tail-log-secs 30
```

## 开发

```bash
cargo fmt --check
cargo clippy -- -D warnings
cargo test --workspace
```

## License

MIT
