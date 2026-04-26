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

## ROM BL 进入方式（手动操作，必须）

SF32LB58 **不支持软件触发** ROM BL 模式，必须手动操作：

1. **短接 MODE 跳线**（3-pin 排针中间与 VCC 之间的 2 个 pin）
2. **按下 RESET 按键**后松开
3. 等待约 1 秒，**拔掉 MODE 短接帽**
4. 此时模组进入 ROM BL 模式，串口（通常为 UART1/115200）等待刷机命令

> 进入 ROM BL 后，LED 通常不亮，串口无输出——这是正常现象。

---

## 刷机参数

| 参数 | 值 |
|------|-----|
| 串口波特率 | 1,000,000 bps（ROM BL 握手阶段会自动切换） |
| Flash 模式 | NAND（sftool-lib 自动加载对应 RAM stub） |
| 连接重试 | 3 次 |
| 软件触发进入 ROM BL | ❌ 不支持 |
| 刷机后自动复位 | ✅（soft_reset） |

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
luatos-cli flash run --soc LuatOS-SoC_V0001_SF32LB58.soc --port COM13 --scripts ./scripts

# 仅刷脚本分区（速度更快）
luatos-cli flash script --soc LuatOS-SoC_V0001_SF32LB58.soc --port COM13 --scripts ./scripts
```

---

## 已知限制

| 功能 | 状态 | 说明 |
|------|------|------|
| 全量刷机 | ✅ | 通过 sftool-lib 实现 |
| 仅刷脚本 | ✅ | 通过 sftool-lib 实现 |
| 软件进入 ROM BL | ❌ | 硬件限制，不支持 |
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
