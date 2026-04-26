# SF32LB58 刷机协议文档

## 概述

SF32LB58（思澈科技）是一款双核 ARM Cortex-M33 SoC，用于 Air8101 等模组。  
刷机方式：通过 **ROM BL 串口协议** 写入固件。协议由 `sftool-lib`（纯 Rust 实现）处理。

---

## 分区布局

### NOR Flash（MPI5，SPI）

| 分区 | 地址 | 大小 | 说明 |
|------|------|------|------|
| ftab | `0x1C000000` | 20 KB | Flash 分区表（SiFli 格式） |
| bootloader | `0x1C020000` | 128 KB | 主 Bootloader |

### NAND Flash（MPI4，SPI）

| 分区 | 地址 | 大小 | 说明 |
|------|------|------|------|
| app（main.bin） | `0x68000000` | 4 MB | 应用固件 |
| KVDB_DFU | `0x68400000` | 16 KB | 硬件 DFU KV（非 LuatOS fskv） |
| FS_REGION（LittleFS） | `0x68B60000` | ~12.6 MB | 文件系统，含 LuatOS fskv(/kv/) |
| FS_EX（script） | `0x69800000` | 1 MB | LuaDB 脚本分区 |

> **注意**：LuatOS fskv 以文件形式存于 LittleFS（FS_REGION），而非 KVDB_DFU_REGION。

---

## ROM BL 进入方式

### 方式一：手动操作（默认，无需改装硬件）

1. **短接 MODE 跳线**（3-pin 排针中间与 VCC 之间的 2 个 pin）
2. **按下 RESET 按键**后松开
3. 等待约 1 秒，**拔掉 MODE 短接帽**
4. 此时模组进入 ROM BL 模式，串口（通常为 UART1/115200）等待刷机命令

> 进入 ROM BL 后，LED 通常不亮，串口无输出——这是正常现象。

### 方式二：自动进入 ROM BL（CH340X 增强 DTR 模式，需改装硬件）

通过改装 CH340X USB 转串口芯片，可实现软件一键进入/退出 ROM BL，无需手动操作 MODE 跳线。

#### 硬件改装

在 CH340X 6# 脚（DTR 引脚）外接 **4.7 kΩ 下拉电阻**到地：
- 启用 CH340X 增强 DTR 功能（引脚变为可控输出）
- 同时作为 MCU BOOT0 引脚的下拉电阻（平时保持低电平，正常启动）

| CH340X 引脚 | 连接目标 | 说明 |
|-------------|----------|------|
| 6# (DTR) | MCU BOOT0 + 4.7kΩ 下拉到 GND | DTR HIGH → BOOT0 HIGH → 进入 ROM BL |
| RTS# | MCU RESET（低有效） | RTS# LOW → RESET 拉低 |

#### 时序说明

**进入 ROM BL**：

| 步骤 | serialport API | CH340X 引脚 | MCU 引脚 | 说明 |
|------|----------------|-------------|----------|------|
| 1 | `write_data_terminal_ready(true)` | DTR 输出 HIGH | BOOT0 = HIGH | 预置引导模式 |
| 2 | `write_request_to_send(true)` | RTS# 输出 LOW | RESET = LOW | 拉低复位 |
| 3 | sleep 100ms | — | RESET 保持低 | — |
| 4 | `write_request_to_send(false)` | RTS# 输出 HIGH | RESET = HIGH | 释放复位，MCU 进 ROM BL |
| 5 | sleep 500ms | — | — | 等待 ROM BL 初始化完成 |
| 6 | 关闭串口 | — | — | 让 sftool-lib 接管 |

**刷机完成后恢复正常运行**：

| 步骤 | serialport API | CH340X 引脚 | MCU 引脚 | 说明 |
|------|----------------|-------------|----------|------|
| 1 | `write_data_terminal_ready(false)` | DTR 输出 LOW | BOOT0 = LOW | 预置正常启动 |
| 2 | `write_request_to_send(true)` | RTS# 输出 LOW | RESET = LOW | 拉低复位 |
| 3 | sleep 100ms | — | RESET 保持低 | — |
| 4 | `write_request_to_send(false)` | RTS# 输出 HIGH | RESET = HIGH | 释放复位，MCU 正常启动 |
| 5 | sleep 200ms | — | — | 等待稳定 |

> **RTS# 极性说明**：CH340X 的 RTS# 为倒相输出，软件设置 RTS=HIGH 时引脚输出 LOW（RESET 有效）；  
> 软件设置 RTS=LOW 时引脚输出 HIGH（RESET 释放）。

#### CLI 用法

在 `flash run` / `flash script` 命令中添加 `--auto-reset` 标志：

```bash
# 全量刷机（CH340X 自动进入/退出 ROM BL）
luatos-cli flash run --soc LuatOS-SoC_V0001_SF32LB58.soc --port COM13 --auto-reset

# 仅刷脚本分区（CH340X 自动复位）
luatos-cli flash script --soc LuatOS-SoC_V0001_SF32LB58.soc --port COM13 --script lua/ --auto-reset
```

> 不加 `--auto-reset` 时行为不变，仍需手动操作 MODE 跳线（向后兼容）。

---

## 刷机参数

| 参数 | 值 |
|------|-----|
| 串口波特率 | 1,000,000 bps（ROM BL 握手阶段会自动切换） |
| Flash 模式 | NAND（sftool-lib 自动加载对应 RAM stub） |
| 连接重试 | 3 次 |
| 软件触发进入 ROM BL | ✅（CH340X 改装 + `--auto-reset`）/ ❌（原版硬件） |
| 刷机后自动复位 | ✅（soft_reset + CH340X exit 序列） |

---

## 刷机顺序

全量刷机（`flash run`）按以下顺序写入：

1. `bootloader.bin` → NOR @ `0x1C020000`
2. `ftab/ftab.bin` → NOR @ `0x1C000000`
3. `main.bin` → NAND @ `0x68000000`
4. `script.bin` → NAND @ `0x69800000`

仅刷脚本（`flash script`）：

1. `script.bin` → NAND @ `0x69800000`

---

## SOC 文件格式（info.json）

SF32LB58 SOC 为 **ZIP 格式**，`info.json` 包含以下关键字段：

```json
{
  "chip": { "type": "sf32lb58" },
  "download": {
    "app_addr": "0x68000000",
    "script_addr": "0x69800000",
    "fs_addr": "0x68B60000",
    "nvm_addr": "0x68400000",
    "bl_addr": "0x1C020000",
    "ftab_addr": "0x1C000000",
    "force_br": "1000000"
  },
  "rom": {
    "file": "main.bin",
    "files": [
      { "name": "bootloader", "file": "bootloader/bootloader.bin" },
      { "name": "main",       "file": "main.bin" },
      { "name": "ftab",       "file": "ftab/ftab.bin" },
      { "name": "script",     "file": "script.bin" }
    ]
  }
}
```

> 旧版 SOC 文件缺少 `bl_addr`/`ftab_addr`/`force_br`，请用最新 `pack_soc.py` 重新打包。

---

## CLI 使用示例

```bash
# 全量刷机（需先手动进入 ROM BL 模式）
luatos-cli flash run --soc LuatOS-SoC_V0001_SF32LB58.soc --port COM13

# 携带脚本目录全量刷机
luatos-cli flash run --soc LuatOS-SoC_V0001_SF32LB58.soc --port COM13 --script ./scripts

# 仅刷脚本分区（速度更快）
luatos-cli flash script --soc LuatOS-SoC_V0001_SF32LB58.soc --port COM13 --script ./scripts

# ── CH340X 增强 DTR 自动复位（改装硬件专用）──

# 全量刷机（自动进入/退出 ROM BL，无需手动操作）
luatos-cli flash run --soc LuatOS-SoC_V0001_SF32LB58.soc --port COM13 --auto-reset

# 仅刷脚本分区（自动复位）
luatos-cli flash script --soc LuatOS-SoC_V0001_SF32LB58.soc --port COM13 --script ./scripts --auto-reset
```

---

## 已知限制

| 功能 | 状态 | 说明 |
|------|------|------|
| 全量刷机 | ✅ | 通过 sftool-lib 实现 |
| 仅刷脚本 | ✅ | 通过 sftool-lib 实现 |
| 软件进入 ROM BL | ✅（改装）/ ❌（原版）| CH340X 增强 DTR 改装后，通过 `--auto-reset` 支持；原版硬件需手动操作 |
| clear-kv | ❌ | LuatOS fskv 在 LittleFS，不支持独立清空 |
| clear-fs | ❌ | 暂不支持，需手动操作 |
| flash-fs | ❌ | 暂不支持 |
| 刷机后自动复位 | ✅ | soft_reset 指令 |
| 跨平台 | ✅ | sftool-lib 纯 Rust，无平台限制 |

---

## 参考资料

- `D:\github\luatos-sdk-sf32lb5x\src\include\ptab.h` — 分区地址定义
- `D:\github\luatos-sdk-sf32lb5x\tools\pack_soc.py` — SOC 打包脚本
- [wendal/sftool](https://github.com/wendal/sftool)（`remove-probe-rs-dep` 分支）— 刷机协议实现
