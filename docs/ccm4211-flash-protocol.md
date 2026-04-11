# CCM4211 (Air1601) 刷机协议文档

## 概述

Air1601 模组使用 CCM4211 芯片，刷机流程分为三个阶段：

1. **ISP 阶段**：通过 ROM bootloader 将 ramrun 程序加载到 RAM
2. **SOC 协议阶段**：通过 ramrun 程序执行 Flash 操作（下载、擦除、校验）
3. **日志阶段**：设备运行后输出日志（同样使用 SOC 帧协议）

## 串口参数

| 阶段 | 波特率 | 校验位 | 数据位 | 停止位 |
|------|--------|--------|--------|--------|
| ISP  | 115200 → 1000000 | EVEN | 8 | 1 |
| SOC 下载 | 2000000 | NONE | 8 | 1 |
| SOC 日志 | 2000000 | NONE | 8 | 1 |

## SOC 文件结构

SOC 文件是 7z 格式的压缩包，包含：

```
LuatOS-SoC_V1009_Air1601.soc (7z archive)
├── info.json          # 芯片信息、地址配置
├── bootloader.bin     # Bootloader 固件 (~41KB)
├── luatos.bin         # LuatOS 核心固件 (~995KB)
├── *.elf              # 调试用 ELF 文件
└── *.map              # 调试用 MAP 文件
```

### info.json 关键字段

```json
{
  "chip": { "type": "air1601" },
  "rom": {
    "file": "luatos.bin",
    "addr": "0x14000000",
    "fs": { "addr": "0x14d00000" }
  },
  "bl": { "addr": "0x10000000" },
  "script": { "addr": "0x14700000", "bitw": 64 },
  "download": { "baud_rate": 2000000 },
  "log": { "baud_rate": 2000000 }
}
```

### 地址映射

| 分区 | 起始地址 | 说明 |
|------|----------|------|
| Bootloader | 0x10000000 | 启动加载程序 |
| Core (App) | 0x14000000 | LuatOS 核心固件 |
| Script | 0x14700000 | Lua 脚本（LuaDB 格式）|
| Filesystem | 0x14D00000 | 文件系统分区 |
| NVM/FSKV | 0x14FF0000 | 键值存储分区 |

---

## 阶段一：ISP 协议

ISP (In-System Programming) 通过芯片内置 ROM bootloader 实现。

### 1.1 设备复位与握手

```
1. 设置 DTR=1, RTS=1，等待 500ms
2. 设置 DTR=0, RTS=0，等待 10ms
3. 发送同步字节 0x55555555 × 50 次（每次间隔 1ms）
4. 等待 10ms
5. 发送同步命令 CMD 0x14
6. 接收应答，提取版本字符串
```

版本字符串示例：`CC Bootloader Version 1.0:2024-09-26`

### 1.2 ISP 命令帧格式

**发送帧：**
```
┌──────┬──────┬────────┬────────┬──────────────┬──────────┐
│ 0x52 │ CMD  │ Param1 │ Param2 │ DataLen (BE) │ Data     │
│ 1B   │ 1B   │ 1B     │ 1B     │ 2B           │ 变长     │
└──────┴──────┴────────┴────────┴──────────────┴──────────┘
```

**应答帧：**
```
┌──────┬──────┬──────────────────┬──────────┐
│ 0x72 │ CMD  │ AckDataLen (BE)  │ AckData  │
│ 1B   │ 1B   │ 2B               │ 变长     │
└──────┴──────┴──────────────────┴──────────┘
```

> **注意：** DataLen 参数不一定等于 Data 的实际长度，不同命令有不同含义。
> 应答帧前可能有垃圾字节，解析时需跳过非 0x72 开头的字节。

### 1.3 ISP 命令列表

| CMD | 名称 | Param1 | Param2 | DataLen | Data | 说明 |
|-----|------|--------|--------|---------|------|------|
| 0x10 | SET_BAUD | 0 | 0 | 3 | `\x00\x01\x24` | 切换到 1Mbps |
| 0x14 | SYNC | 0 | 0 | 0x10/0x24 | 无 | 同步/握手 |
| 0x20 | SET_RAM_BASE | 0x20 | 0 | 4 | 无 | 设置 RAM 基地址 |
| 0x31 | WRITE_RAM | addr | 0 | chunk_len | 数据 | 写入 RAM（512B 块）|
| 0x81 | EXECUTE | go1 | go2 | 0 | 无 | 跳转执行 ramrun |

### 1.4 Ramrun 加载流程

```
1. 握手: CMD 0x14 (DataLen=0x24)
2. 切波特率: CMD 0x10 → 切换到 1Mbps (就地切换波特率，不关闭/重新打开串口)
3. 同步确认: CMD 0x14 (DataLen=0x10)
4. 设 RAM 基地址: CMD 0x20 (Param1=0x20)
5. 分块写入: CMD 0x31 × N (512字节/块, Param1 从 0x30 开始, 每块 +2)
6. 跳转执行: CMD 0x81 (go_addr1=ramrun[5], go_addr2=ramrun[4])
```

> **关键：** WRITE_RAM 的 Param1 是递增的地址页码（0x30, 0x32, 0x34, ...），
> 不是固定值 0。每写入一个 512 字节块，地址 +2。

### 1.5 波特率切换注意事项

在 CH343 USB 转串口芯片上，**不能关闭/重新打开串口来切换波特率**。
必须在已打开的串口上就地修改波特率：

```python
# ✅ 正确做法：就地切换
port.baudrate = 1000000

# ❌ 错误做法：关闭再打开（CH343 上会失败）
port.close()
port = serial.Serial(port_name, 1000000, ...)
```

Rust 中 `serialport` crate 的 `drop + new` 模式可以正常工作。

---

## 阶段二：SOC 帧协议

执行 ramrun 后，设备切换到 SOC 帧协议进行 Flash 操作。

### 2.1 帧格式

```
┌──────┬──────────────────────────────┬────────────────┬──────┐
│ 0xA5 │ escaped(Header + Payload)    │ escaped(CRC16) │ 0xA5 │
│ 标记 │ 转义后的头部+数据             │ 转义后的校验    │ 标记 │
└──────┴──────────────────────────────┴────────────────┴──────┘
```

### 2.2 转义规则

发送/接收时，帧内容中的特殊字节需要转义：

| 原始字节 | 转义后 |
|----------|--------|
| 0xA5 | 0xA6 0x01 |
| 0xA6 | 0xA6 0x02 |

> CRC 计算使用**转义前**的原始数据。

### 2.3 帧头结构（24 字节，小端序）

```
┌───────────┬─────────┬─────────┬─────────┬────────┬──────┬─────┐
│ ms (u64)  │addr(u32)│len(u32) │cmd(u32) │sn(u16) │type  │cpu  │
│ 8 bytes   │ 4 bytes │ 4 bytes │ 4 bytes │ 2 bytes│ 1B   │ 1B  │
└───────────┴─────────┴─────────┴─────────┴────────┴──────┴─────┘
```

| 字段 | 类型 | 说明 |
|------|------|------|
| ms | u64 | 时间戳（毫秒），可设为当前时间 |
| address | u32 | Flash 地址 |
| len | u32 | 长度字段（写入命令设为 0）|
| cmd | u32 | 命令码 |
| sn | u16 | 序列号（从 1 开始递增）|
| type | u8 | 类型，通常为 0 |
| cpu | u8 | CPU 编号，通常为 0 |

### 2.4 CRC16 校验

- 多项式：0xA001（0x8005 的反转）
- 初始值：**0**（不是标准 Modbus 的 0xFFFF）
- 计算范围：Header + Payload（转义前的原始字节）
- 结果以小端序（2 字节）附加在帧尾

```rust
fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;  // init = 0
    for &byte in data {
        crc ^= byte as u16;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xA001;
            } else {
                crc >>= 1;
            }
        }
    }
    crc
}
```

### 2.5 SOC 命令列表

| CMD | 名称 | Address | Payload | 应答 | 超时 |
|-----|------|---------|---------|------|------|
| 0x08 | FLASH_ERASE_BLOCK | 分区地址 | 空 | 确认 | 10s |
| 0x09 | GET_DOWNLOAD_INFO | 0 | 空 | block_len (u32 LE) | 500ms |
| 0x0A | SET_CODE_DATA_START | Flash 地址 | orig_len (u32 LE) | 确认 | 1s |
| 0x0B | SET_CODE_DATA | 0 | 数据块 (≤3KB) | 确认 | 500ms |
| 0x0C | SET_CODE_END | 0 | is_lzma (u8, 0=无压缩) | 确认 | 3s |
| 0x0D | CHECK_CODE | 起始地址 | total_len (u32 LE) | MD5 (16B) | 10s |
| 0x0F | FORCE_RESET | 0 | 空 | 无 | — |

### 2.6 SN（序列号）管理

- 每个命令使用唯一递增的 SN（u16，从 1 开始）
- SN 溢出后跳过 0（0 → 1）
- 应答帧中的 SN 不用于匹配（通过 CMD 匹配应答）

---

## 下载流程

### 完整固件刷机

```
ISP 阶段:
  1. 复位设备（DTR/RTS 翻转）
  2. 握手（0x55 同步 + CMD 0x14）
  3. 切换波特率到 1Mbps (CMD 0x10)
  4. 加载 ramrun 到 RAM (CMD 0x20 + CMD 0x31 × N)
  5. 执行 ramrun (CMD 0x81)

SOC 阶段:
  6. 切换到 2Mbps, PARITY_NONE
  7. 查询 block_len (CMD 0x09) → 返回 262144 (0x40000)
  8. 对每个文件 (bootloader, core, script):
     a. CMD 0x0A: 设置下载起始地址 + 原始数据长度
     b. CMD 0x0B × N: 发送数据（3KB 子块，最多重试 10 次/块）
     c. CMD 0x0C: 结束标记 (is_lzma=0)
  9. 对每个文件: CMD 0x0D: MD5 校验（地址 + 总长度）
  10. 复位设备（DTR/RTS 翻转）
```

### 仅刷脚本

与完整刷机相同，但只下载 script 分区（地址 0x14700000）。

### 擦除分区

```
ISP + ramrun 加载（同上）
→ CMD 0x08: FLASH_ERASE_BLOCK（指定分区地址）
→ 复位设备
```

### 块大小与分块

- **block_len**: 设备返回的块大小，通常为 262144 (256KB)
- 每个 block_len 大小的数据块是一个独立的 CMD 0x0A→0x0B→0x0C 周期
- CMD 0x0B 的子块大小为 **3KB**（3072 字节）
- 大文件会被拆分为多个 block_len 块

### 重试机制

CMD 0x0B (SET_CODE_DATA) 可能因串口时序问题失败，需要重试：

- 最多重试 **10 次**
- 重试间隔 **10ms**
- 同步 I/O 模式下约 30-50% 的块需要 1-2 次重试
- 超过 10 次重试则视为致命错误

---

## MD5 校验

CMD 0x0D (CHECK_CODE) 的应答包含设备计算的 MD5 值（16 字节）。

```
请求: address=文件起始地址, payload=total_len (u32 LE)
应答: payload=MD5_digest (16 bytes)
```

主机端对比本地计算的 MD5 与设备返回的 MD5，不一致则报错。

---

## 日志解码

设备运行时输出的日志也使用 SOC 帧协议（0xA5 帧），但帧内容是格式化的日志文本。
使用 `luatos-log` crate 的 `SocLogDecoder` 解码。

日志格式示例：
```
[2026-04-11 14:23:45.123] I/user.main Hello, LuatOS!
D/sys tick=12345
```
