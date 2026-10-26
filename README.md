# luatos-cli

LuatOS 命令行工具集（纯 Rust）——刷机、日志、项目管理、固件资源与构建。

[![CI](https://github.com/wendal/luatos-cli/actions/workflows/ci.yml/badge.svg)](https://github.com/wendal/luatos-cli/actions/workflows/ci.yml)

## 功能特性

- **多芯片刷机**：Air8101(BK7258)、Air6208(XT804)、Air1601/Air1602/Air1603/Air1604(CCM4211)、EC618/Air780E/Air780EG、Air8000/Air780EPM(EC718)、SF32LB58、Air724UG(UIS8910DM/RDA8910)；EC618 优先使用 SOC 内的专用 AgentBoot，CP 写入独立 CP Flash；Air8101 走原生 ISP，写完 RTS+DTR 复位；`flash run --script` 支持 EC618 / EC718 / CCM4211 / RDA8910 脚本覆盖
- **CCM4211 下载提速**：Air160x 系列固件分区按设备上报的块长（通常 256KB）做 LZMA 压缩后下发，与 LuaTools 行为一致；压缩无收益或失败自动回落明文，设备端 MD5 校验不通过则自动改明文重传一次（`LUATOS_CCM_LZMA=0` 可强制关闭压缩）；装载 ramrun 前会先关掉其他串口的时钟门控（IPSCGTCR），避免 PCBA_TYPE2 板子上多出的串口干扰下载口
- **任意地址刷机**：`flash raw` 绕过 `info.json` 分区定义，按 `--addr` 直接擦写 Flash（Air8101 / Air6208 / Air1601），适合单独补刷外置字库/TTS/fs 等分区表覆盖不到的资源。bootloader/core 默认受保护需 `--force`。校验能力因芯片而异：Air8101 读回逐字节比对、Air1601 用设备回读 MD5，Air6208(XT804) 无读回命令故不支持 `--verify`。擦除三颗都支持，粒度各异：Air8101 4K 精确、Air1601 256KB 向下对齐整块擦、Air6208(XT804) 走 ROM `QFlash 擦除 0x32` 的 Sector 模式，4KB 精确
- **任意地址读回**：`flash dump` 把任意 Flash 区间导出到文件并打印 md5（仅 Air8101 支持）
- **FOTA 打包**：支持 EC7xx 差分/脚本、CCM4211 全量/脚本、Air8101(BK72XX) 新格式全量/脚本、Air724UG(RDA8910) 差分/脚本；EC7xx 差分时若底层固件相同自动回落到脚本更新包（`--force-par` 可强制走差分）
- **二级帮助入口**：`--help` 提示型号入口，`guide models` / `guide model --model <型号>` 直接给推荐命令
- **刷机后继续监听**：`flash run --tail-log-secs <N>` 刷机后自动按型号波特率续接日志，减少开机日志丢失；EC718 / RDA8910 会等待运行模式 log 口重新枚举
- **脚本调试信息可控**：用 `--script <dir>` 打包脚本时支持 `--luac-debug 0|1|2|99`（默认 `99`=全部调试信息，非法值直接报错），覆盖 BK7258 / XT804 / CCM4211 / EC718 / RDA8910 / SF32LB58 的脚本编译路径；脚本偏大时可降到 `0`/`1` 缩小镜像。XT804 请使用 `flash script`，其 `flash run/test` 暂不支持 `--script` 覆盖
- **trun 单点调试**：`trun <name> --soc base.soc --port COM6` 一站式完成 testcase 合成 → 刷机 → 抓日志 → 关键字校验，取代 luatos-autotest-v2 在开发期的临时合成
- **日志系统**：文本 / 二进制日志查看、录制、解析，支持智能诊断；`--port auto` 按 USB 接口号自动识别 EC718 / RDA8910 的 log 口；`log view-binary` 按实际打开的口选 0x7E（EC718 USB CDC）或 0xA5（CH340 / SOC UART），不因同机插着 4G USB 而误切协议
- **项目与构建**：项目向导、配置管理、Lua 依赖分析、luac/LuaDB 构建
- **AI 友好输出**：全局 `--format text|json|jsonl`

## 安装

```bash
cargo build --release -p luatos-cli
```

SF32LB 系列刷机功能默认不编译（可省约 2.6MB），需要时显式开启：

```bash
cargo build --release -p luatos-cli --features sf32lb58
```

## Air8101 通用资源分区下载

使用带 `rom.fs.res` 描述的固件（例如 Air8101B 108），指定资源目录即可
打包标准 romfs 镜像并写入资源分区：

```bash
luatos-cli soc info LuatOS-SoC_V2021_Air8101_108.soc
luatos-cli flash flash-res --soc LuatOS-SoC_V2021_Air8101_108.soc --port COM6 --resource ./resources
# 可重复指定；相同相对路径由后面的目录覆盖
luatos-cli flash flash-res --soc firmware.soc --port COM6 --resource ./base --resource ./overrides
```

资源分区镜像是**标准 romfs（`-rom1fs-`，与核心 `tools/romfs/mkromfs.py`
逐字节一致）**，并按 BK CRC 每 32 字节数据 + 2 字节 CRC 帧化写入；
`rom.fs.res` 元数据形如：

```json
{"offset": "0x6D7000", "size": 1088, "type": "romfs", "bkcrc": true}
```

`type` 必须是 `"romfs"`（旧的 `"luadb"` 直接报错），`bkcrc: true` 表示帧化、
`false` 表示不帧化的裸 romfs，`size` 单位为 KiB，地址与容量按 4KiB 对齐，
可选 `download.res_addr` 必须与 offset 一致。设备端经 CPU flash 窗口指针
直读镜像（`luat_fs_mmap` 零拷贝可用），不再把资源拷入 RAM，只读挂载 `/res/`。

递归保留文件相对路径，跳过 `.git/.svn/.hg`，不编译 `.lua`、不改变文件
内容或扩展名，也不添加脚本分区使用的 Beken CRC。资源目录可以完全不含
TTS 或字体。romfs 约束：单级文件名 ≤ **64 个 UTF-8 字节**（超限直接报错），
目录最多 **4 层**（第 5 层及更深的文件打印告警后跳过），文件数不再有旧
Luadb 的 1–1024 上限。空目录、错误元数据和超容量镜像在连接串口前报错。

帧化让每 34 字节只承载 32 字节数据，净载荷上限 = `size / 34 * 32`（约 5.9%
的容量用于 CRC 帧；3MiB 分区净载荷 2891KiB，3008KiB 分区净载荷 2831KiB =
2,898,976 字节）。`soc info` 按实际元数据打印：

```
Res Addr:   0x006D7000  (1088 KB, romfs, bkcrc=true, payload 1024 KB)
```

只擦写镜像覆盖的资源扇区，末尾扇区以 FF 补齐；
更新后旧文件是否可见由新镜像文件清单决定，尾部旧数据不作安全擦除。
写入后自动复位，由固件只读挂载 `/res/`。目前下载后端支持 BK72xx。

可重复生成的测试目录、验收脚本及实机记录见
[资源下载测试](tests/air8101_resource_download/README.md)。

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

# Air780EPM：自动重启进入 USB 下载模式，烧录 SOC 中的底层与脚本
luatos-cli flash run --soc LuatOS-SoC_V2054_Air780EPM_1.soc --port auto

# EC618 / Air780E / Air780EG：免 BOOT 失败时先按住 BOOT 复位，再运行
luatos-cli flash run --soc helloworld_1.0.0_LuatOS-SoC_V1124_EC618_FULL.soc --port auto

# 往任意 Flash 地址刷机（低阶操作，bootloader/core 需 --force）
luatos-cli flash raw --soc firmware.soc --port COM10 \
  --addr 0x200000 --file font.bin --erase --verify

# 读回任意 Flash 区间到文件（Air8101）
luatos-cli flash dump --soc firmware.soc --port COM10 \
  --addr 0x40d000 --len 0x1000 -o dump.bin

# 刷机后继续监听启动日志（30 秒）
luatos-cli flash run --soc firmware.soc --port COM10 --tail-log-secs 30

# 仅刷脚本分区；脚本偏大超出脚本分区时，用 --luac-debug 0 去掉调试信息（默认 99）
luatos-cli flash script --soc firmware.soc --port auto --script lua/ --luac-debug 0

# Air6208 / XT804：全量刷机与脚本编译分两步执行
luatos-cli flash run --soc air6208.soc --port COM7
luatos-cli flash script --soc air6208.soc --port COM7 --script lua/ --luac-debug 0

# 文本日志（EC718 可用 --port auto 自动识别 log 口）
luatos-cli log view --port auto --baud 921600

# 二进制日志（Air1601/Air1602/Air1603/Air1604/EC718）
luatos-cli log view-binary --port COM10 --baud 6000000 --probe

# Air1603 刷机（与 Air1601/1602 完全同路径，直接吃 .soc，无需 --chip）
luatos-cli flash run --soc LuatOS-SoC_V1027_Air1603_101.soc --port COM10

# Air8101(BK72XX) 新格式脚本 FOTA
luatos-cli fota build --new air8101.soc --script-only -o air8101_script_fota.bin

# Air780EPM / Air780EHM / Air8000 仅脚本 FOTA
luatos-cli fota build --new firmware.soc --script-only

# Air724UG(RDA8910) 差分 FOTA（需 dtools，自动发现或 --fota-toolkit 指定）
luatos-cli fota build --new v2.soc --old v1.soc

# Air724UG(RDA8910) 仅脚本热更新
luatos-cli fota build --new v2.soc --script-only

# 修改内置脚本后重打包 soc（soc combine 自动识别芯片，RDA8910 替换 PAC 内 LUA，无需 --addr）
luatos-cli soc combine --soc base.soc --bin build/script.bin -o patched.soc

# trun 单点调试（自动找 testcase，合成 script.bin + 刷机 + 抓日志 + 关键字）
luatos-cli trun list --luatos-root D:/github/LuatOS
luatos-cli trun validate exftp --luatos-root D:/github/LuatOS
luatos-cli trun exftp --luatos-root D:/github/LuatOS --soc base.soc --port COM6 --keyword "LuatOS@" --fail-keyword panic

# EC7xx 差分 FOTA：底层固件相同时自动回落到脚本更新包
luatos-cli fota build --new v2.soc --old v1.soc          # 自动回落
luatos-cli fota build --new v2.soc --old v1.soc --force-par  # 强制走 FotaToolkit 差分
```

## 型号文档（按型号拆分）

- [Air1601 / Air1602 / Air1603 / Air1604 / CCM4211](docs/models/air1601-air1604.md)
- [Air8000 / EC7xx / Air780E 系列](docs/models/air8000-ec7xx.md)
- [EC618 / Air780E / Air780EG](docs/models/ec618.md)
- [Air8101 / BK72xx](docs/models/air8101-bk72xx.md)
- [Air6208 / XT804 / Air101/Air103](docs/models/air6208-xt804.md)
- [SF32LB58](docs/models/sf32lb58.md)
- [Air724UG (UIS8910DM / RDA8910)](docs/models/air724ug.md)

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

# testcase 依赖 testrunner/common 之外的脚本库时（如 require "libnet"），
# 用 --libs 追加脚本库目录（可重复传入；建议只放所需子集，脚本分区容量有限）
luatos-cli trun run func/network/TCP \
  --luatos-root D:/github/LuatOS \
  --soc base.soc --port COM6 \
  --libs D:/github/LuatOS/script/libs_min \
  --keyword '### OVERALL_PASS ###' --fail-keyword '### OVERALL_FAIL ###'
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
- Air1601/Air1602/Air1603/Air1604/EC718 执行 `flash test --script <dir>` 时，会在全量刷机阶段覆盖脚本分区后再抓日志判定关键字
- 结果为 `FAIL` 时会输出缺失关键字列表（JSON 输出为 `missing_keywords`）

## 支持的模组（摘要）

| 模组 | 芯片 | 刷机 | 脚本区 | 文件系统 | FSKV | FOTA | 日志 | 任意地址<br>`flash raw` | 闭环测试 | trun |
|------|------|:----:|:------:|:--------:|:----:|:----:|:----:|:------:|:--------:|:--------:|
| Air8101 | BK7258 (bk72xx) | ✅ | ✅ | ✅ | ✅ | ✅（新格式全量/脚本） | 文本 | ✅（真校验 + 可读回） | ✅ | ✅ |
| Air6208 | XT804 (air6208) | ✅ | ✅ | ✅ | ✅ | ✅（全量） | 二进制 | ✅（仅写，无擦除/校验/读回） | ✅ | ✅ |
| Air101/103 | XT804 | ✅ | ✅ | — | — | ✅（全量） | 二进制 | ✅（仅写，无擦除/校验/读回） | ✅ | ✅ |
| Air1601 / Air1602 / Air1603 / Air1604 | CCM4211 | ✅ | ✅ | ✅ | ✅ | ✅（全量/脚本） | 二进制 (`--probe`) | ✅（MD5 真校验，无读回） | ✅ | ✅ |
| Air8000 / Air780EPM / Air780EHx | EC718 (ec7xx) | ✅ | ✅ | — | — | ✅（差分/脚本） | 二进制 (`--probe`) | — | ✅ | ✅ |
| EC618 / Air780E / Air780EG | EC618 | ✅ | ✅ | — | — | ✅（差分/脚本） | 二进制 (`--probe`) | — | ✅ | ✅ |
| Air8101(SF32) | SF32LB58 | ✅ | ✅ | — | ✅ | — | 文本 | — | — | — |
| Air724UG | UIS8910DM (rda8910) | ✅ | ✅ | — | — | ✅（差分/脚本） | 二进制 (`--probe`) | — | — | — |

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
