# luatos-cli

LuatOS 命令行工具集（纯 Rust）——刷机、日志、项目管理、固件资源与构建。

[![CI](https://github.com/wendal/luatos-cli/actions/workflows/ci.yml/badge.svg)](https://github.com/wendal/luatos-cli/actions/workflows/ci.yml)

## 功能特性

- **多芯片刷机**：Air8101(BK7258)、Air6208(XT804)、Air1601/Air1602(CCM4211)、Air8000(EC718)、SF32LB58
- **FOTA 打包**：支持 EC7xx 差分/脚本、CCM4211 全量/脚本、Air8101(BK72XX) 新格式全量/脚本；EC7xx 差分时若底层固件相同自动回落到脚本更新包（`--force-par` 可强制走差分）
- **二级帮助入口**：`--help` 提示型号入口，`guide models` / `guide model --model <型号>` 直接给推荐命令
- **刷机后继续监听**：`flash run --tail-log-secs <N>` 刷机后自动按型号波特率续接日志，减少开机日志丢失
- **trun 单点调试**：`trun <name> --soc base.soc --port COM6` 一站式完成 testcase 合成 → 刷机 → 抓日志 → 关键字校验，取代 luatos-autotest-v2 在开发期的临时合成
- **日志系统**：文本 / 二进制日志查看、录制、解析，支持智能诊断
- **项目与构建**：项目向导、配置管理、Lua 依赖分析、luac/LuaDB 构建
- **AI 友好输出**：全局 `--format text|json|jsonl`

## 安装

```bash
cargo build --release -p luatos-cli
```

## C 共享库 (luatos-log-ffi)

SOC 日志解码同样以 C ABI 形式发布, 方便 Python (`ctypes`)、C/C++、Go (`cgo`)、C# (P/Invoke) 等语言直接消费. 与第三方 `pySoclogAnalyze` DLL 签名完全兼容 (含日志帧/命令帧双模式), 现有用户可**无感替换** DLL.

```bash
# C 示例
gcc -O2 -std=c99 -I crates/luatos-log-ffi/include \
    crates/luatos-log-ffi/examples/c/demo.c \
    -L target/release -lluatos_log_ffi -o demo
LD_LIBRARY_PATH=target/release ./demo

# Python ctypes
python3 crates/luatos-log-ffi/examples/python/demo_ctypes.py
```

从 [GitHub Releases](https://github.com/wendal/luatos-cli/releases) 下载预编译的 `luatos-log-ffi-<target>.{zip,tar.gz}` 包 (含 .dll/.so/.dylib + 头文件 + 示例), 4 个目标平台 (windows-msvc / linux-gnu / darwin x64+arm64) 全覆盖.

详见 [docs/luatos-log-c-abi.md](docs/luatos-log-c-abi.md).

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

# trun 单点调试（自动找 testcase，合成 script.bin + 刷机 + 抓日志 + 关键字）
luatos-cli trun list --luatos-root D:/github/LuatOS
luatos-cli trun validate exftp --luatos-root D:/github/LuatOS
luatos-cli trun exftp --luatos-root D:/github/LuatOS --soc base.soc --port COM6 --keyword "LuatOS@" --fail-keyword panic

# EC7xx 差分 FOTA：底层固件相同时自动回落到脚本更新包
luatos-cli fota build --new v2.soc --old v1.soc          # 自动回落
luatos-cli fota build --new v2.soc --old v1.soc --force-par  # 强制走 FotaToolkit 差分
```

## 型号文档（按型号拆分）

- [Air1601 / Air1602 / CCM4211](docs/models/air1601-air1602.md)
- [Air8000 / EC7xx / Air780E 系列](docs/models/air8000-ec7xx.md)
- [Air8101 / BK72xx](docs/models/air8101-bk72xx.md)
- [Air6208 / XT804 / Air101/Air103](docs/models/air6208-xt804.md)
- [SF32LB58](docs/models/sf32lb58.md)

## trun 单点调试

`trun` 子命令（test run）把 luatos-autotest-v2 在开发期的"合成 → 刷机 → 抓日志 → 关键字校验" 四步流程合成一条命令。无需启动 Flask/MQTT/Runner 多进程，开发期单点验证从分钟级降到秒级。

### 三档 ctx.json 合并规则

```text
1. base = {} (空对象)
2. <LuatOS>/testcase/local_ctx.json    (默认覆盖)
3. --ctx <FILE>                          (CLI 本地覆盖)
4. --full-ctx <FILE> 若指定 → 跳过 1-3，仅用此文件 + 注入 test_id
```

数组不 concat，直接覆盖（避免 `mqtt.status_topic` 等被意外追加）。

### 典型用法

```bash
# 列出 LuatOS 仓库下所有 testcase
luatos-cli trun list --luatos-root D:/github/LuatOS

# 按子目录过滤
luatos-cli trun list --luatos-root D:/github/LuatOS --category function_testcase_network

# 校验结构（不刷机）
luatos-cli trun validate exftp --luatos-root D:/github/LuatOS

# 快路径：仅刷 script.bin（默认）
luatos-cli trun exftp \
  --luatos-root D:/github/LuatOS \
  --soc base.soc \
  --port COM6 \
  --keyword "LuatOS@" \
  --fail-keyword panic

# 冷路径：合新 soc 后刷整个固件
luatos-cli trun exftp \
  --luatos-root D:/github/LuatOS \
  --soc base.soc \
  --port COM6 \
  --full --keep-soc ./artifacts

# 传入额外 ctx.json 字段（与 <LuatOS>/testcase/local_ctx.json 合并）
luatos-cli trun exftp \
  --luatos-root D:/github/LuatOS \
  --soc base.soc --port COM6 \
  --ctx ./my_local_ctx.json
```

### 退出码

| Verdict | 退出码 | 含义 |
|---|---|---|
| `Pass` | 0 | 所有关键字命中且未触发 fail_keyword |
| `Fail` | 1 | 关键字缺失 / 命中 fail_keyword |
| `Indeterminate` | 2 | 当前 trun 不会构造（保留以便未来重新接 ctx.json 回传监听器时使用） |
| `Error` | 3 | 端口被占 / soc 读取失败 / 解析失败 |

### 与 luatos-autotest-v2 的边界

- **不替代**：Runner 多进程池、Orchestrator 派发、relay 时序、SQLite 历史、MQTT 双向桥、飞书/钉钉 webhook、FOTA 多机差异化烧写
- **ctx.json 字段名完全兼容**：`test_id` / `runner_id` / `runner_mode` / `report_url` / `status_url` / `mqtt.*` / `wifi_ssid` 等 autotest-v2 设备端 SDK 不感知来源
- **test_id 格式一致**：`test_<unix_secs_base36>_<random_hex>`，CLI 的 `runner_id` 用 `cli-<hostname>-<pid>` 后缀避免和 autotest-v2 真实 runner_id 冲突
- **未来桥接**：autotest-v2 想用 Rust CLI 替代 Python 烧写，只需在 Orchestrator 调 `luatos-cli trun <name> --ctx <ctx.json> --full --keep-soc ./artifacts --jsonl`

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

| 模组 | 芯片 | 刷机 | 脚本区 | 文件系统 | FSKV | FOTA | 日志 | 闭环测试 | trun |
|------|------|:----:|:------:|:--------:|:----:|:----:|:----:|:--------:|:--------:|
| Air8101 | BK7258 (bk72xx) | ✅ | ✅ | ✅ | ✅ | ✅（新格式全量/脚本） | 文本 | ✅ | ✅ |
| Air6208 | XT804 (air6208) | ✅ | ✅ | ✅ | ✅ | ✅（全量） | 二进制 | ✅ | ✅ |
| Air101/103 | XT804 | ✅ | ✅ | — | — | ✅（全量） | 二进制 | ✅ | ✅ |
| Air1601 / Air1602 | CCM4211 | ✅ | ✅ | ✅ | ✅ | ✅（全量/脚本） | 二进制 (`--probe`) | ✅ | ✅ |
| Air8000 / Air780E | EC718 (ec7xx) | ✅ | ✅ | — | — | ✅（差分/脚本） | 二进制 (`--probe`) | ✅ | ✅ |
| Air8101(SF32) | SF32LB58 | ✅ | ✅ | — | ✅ | — | 文本 | — | — |

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
