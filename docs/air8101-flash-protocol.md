# Air8101 (BK7258) 刷机协议文档

## 概述

Air8101 模组使用 BK7258 芯片（博通集成），刷机流程如下：

1. **DTR/RTS 复位**：通过 5 阶段脉冲序列进入 ROM bootloader
2. **LinkCheck 握手**：快速窗口内验证 bootloader 已就绪
3. **波特率切换**：从 115200 切换到目标波特率（默认 2 Mbps）
4. **Flash 去保护**：读取 Flash MID，清除写保护位
5. **扇区擦除**：自适应 4K/64K 擦除
6. **扇区写入**：以 4K 扇区为单位写入数据
7. **设备重启**：关闭串口后设备自动重启

协议参考：BK7231GUIFlashTool

## 串口参数

| 参数 | 值 |
|------|-----|
| 初始波特率 | 115200 bps |
| 目标波特率 | 2000000 bps（可通过 info.json `force_br` 配置）|
| 校验位 | NONE |
| 数据位 | 8 |
| 停止位 | 1 |
| 端口超时 | 1-200ms（随操作变化）|

## SOC 文件结构

SOC 文件是 **ZIP** 格式的压缩包（魔数 `0x50 0x4B`，即 "PK"），包含：

```
LuatOS-SoC_V2013_Air8101.soc (ZIP archive)
├── info.json              # 芯片信息、地址配置
├── luatos.bin             # 固件 ROM
├── script.bin             # Lua 脚本字节码
├── air602_flash.exe       # 备用刷机工具 (Windows)
└── [可选] 文档
```

### info.json 关键字段

```json
{
  "version": 2013,
  "chip": {
    "type": "bk72xx"
  },
  "rom": {
    "file": "luatos.bin",
    "fs": {
      "script": {
        "size": 512,
        "bkcrc": true
      }
    }
  },
  "script": {
    "file": "script.bin",
    "bitw": 32,
    "use-luac": true,
    "use-debug": false
  },
  "download": {
    "bl_addr": "0x0",
    "script_addr": "0x200000",
    "force_br": "2000000"
  },
  "user": {
    "log_br": "921600"
  }
}
```

> **注意**：BK7258 的 `bkcrc` 默认为 `true`，表示脚本分区使用 BK CRC16 封装。

---

## 阶段一：DTR/RTS 复位序列（5 阶段脉冲）

BK7258 需要精确的 5 阶段脉冲模式才能进入 ROM bootloader。时序非常关键。

```
初始状态: DTR=0, RTS=0, 等待 50ms

Phase 1: 同时拉高
  DTR = 1, RTS = 1
  等待 50ms

Phase 2: 同时释放
  DTR = 0, RTS = 0
  等待 20ms

Phase 3: 仅拉高 DTR
  DTR = 1, RTS = 0
  等待 50ms

Phase 4: DTR 释放，RTS 拉高（进入 bootloader 模式）
  DTR = 0, RTS = 1
  等待 50ms

Phase 5: 全部释放
  DTR = 0, RTS = 0
  → 立即开始 LinkCheck 尝试
```

### 时序约束

- 各阶段等待时间必须精确（50ms, 20ms, 50ms, 50ms）
- Phase 5 后立即进入 LinkCheck 循环
- Bootloader 窗口在 Phase 5 后约 8ms 内打开
- 必须快速尝试 LinkCheck（每次复位脉冲后最多 200 次）
- 如果 200 次 LinkCheck 都失败，等待 20ms 后重复整个 5 阶段序列
- 整个过程最多重复 30 次

---

## 阶段二：LinkCheck 握手

### LinkCheck 帧格式

**发送 (Host → Device)：**
```
[0x01, 0xE0, 0xFC, 0x01, 0x00]
  ↓     ↓     ↓     ↓    ↓
 hdr1  hdr2  hdr3  len  cmd(0x00)
```

**期望应答 (Device → Host)：**
```
[0x04, 0x0E, 0x05, 0x01, 0xE0, 0xFC, 0x01, 0x00]
```

### LinkCheck 重试逻辑

```
对每次 DTR/RTS 脉冲（最多 30 次）:
  对每次 LinkCheck（200 次）:
    清空接收缓冲区
    发送 LinkCheck TX (5 字节)
    刷新发送
    读取应答 (2ms 超时窗口)
    如果收到 8 字节且匹配期望值:
      → 设置端口超时为 200ms
      → 返回成功 ✓

  所有 200 次 LinkCheck 失败:
    等待 20ms
    重复 5 阶段 DTR/RTS 脉冲

30 次全部失败: → 返回错误
```

> **关键**：LinkCheck 时端口读取超时必须设为 1ms — bootloader 响应窗口仅约 8ms。

---

## 阶段三：BK7231 ISP 协议命令帧

### 短命令格式（≤255 字节负载）

**发送帧：**
```
┌────────────────┬──────┬──────┬──────────┐
│ 0x01 0xE0 0xFC │ LEN  │ CMD  │ [Data]   │
│ (3 bytes)      │(1B)  │(1B)  │ ≤255B    │
└────────────────┴──────┴──────┴──────────┘
```

**应答帧：**
```
┌───────────┬──────┬────────────────┬──────┬──────────┐
│ 0x04 0x0E │ LEN  │ 0x01 0xE0 0xFC │ CMD  │ [Data]   │
│ (2 bytes) │(1B)  │ (3 bytes)      │(1B)  │ 应答数据 │
└───────────┴──────┴────────────────┴──────┴──────────┘
```

### 长命令格式（>255 字节或 Flash 操作）

**发送帧：**
```
┌────────────────┬────────────┬──────────────────┬──────┬──────────┐
│ 0x01 0xE0 0xFC │ 0xFF 0xF4  │ LEN_LO LEN_HI    │ CMD  │ [Data]   │
│ (3 bytes)      │ (2 bytes)  │ (2 bytes, LE)    │(1B)  │ ≤65535B  │
└────────────────┴────────────┴──────────────────┴──────┴──────────┘
```

**应答帧：**
```
┌───────────┬──────┬────────────────┬────────────┬──────────────────┬──────┬──────────┐
│ 0x04 0x0E │ 0xFF │ 0x01 0xE0 0xFC │ 0xF4       │ LEN_LO LEN_HI    │ CMD  │ [Data]   │
│ (2 bytes) │(1B)  │ (3 bytes)      │ (1 byte)   │ (2 bytes, LE)    │(1B)  │ 应答数据 │
└───────────┴──────┴────────────────┴────────────┴──────────────────┴──────┴──────────┘
```

---

## 命令列表

### 1. LinkCheck — 验证握手 (CMD 0x00)

| 字段 | 值 |
|------|-----|
| 类型 | 短命令 |
| TX | `[0x01, 0xE0, 0xFC, 0x01, 0x00]` |
| RX | `[0x04, 0x0E, 0x05, 0x01, 0xE0, 0xFC, 0x01, 0x00]` |
| 超时 | 2ms |

### 2. 设置波特率 (CMD 0x0F)

| 字段 | 值 |
|------|-----|
| 类型 | 短命令 |
| 负载 | `[baud_u32_LE (4B), delay_ms (1B)]` |
| TX 长度 | 10 字节 |
| TX | `[0x01, 0xE0, 0xFC, 0x06, 0x0F, B0, B1, B2, B3, delay]` |
| RX | `[0x04, 0x0E, ?, 0x01, 0xE0, 0xFC, 0x0F, ...]` |
| 超时 | 600ms |

**示例 (2 Mbps)：**
```
baud = 2000000 = 0x001E8480
B0=0x80, B1=0x84, B2=0x1E, B3=0x00
delay = 200
TX: [0x01, 0xE0, 0xFC, 0x06, 0x0F, 0x80, 0x84, 0x1E, 0x00, 0xC8]
```

**切换流程：**
```
1. 清空接收缓冲区（两次，间隔 50ms）
2. 发送命令
3. 刷新发送缓冲区
4. 等待 10ms + delay_ms/2
5. 在已打开的串口上就地修改主机端波特率
6. 读取应答 (8 字节, 超时 600ms)
7. 验证: buf[0..2]=[0x04,0x0E] && buf[3..6]=[0x01,0xE0,0xFC] && buf[6]=0x0F
```

### 3. 读取 Flash MID (CMD 0x0E)

| 字段 | 值 |
|------|-----|
| 类型 | 长命令 |
| TX | `[0x01, 0xE0, 0xFC, 0xFF, 0xF4, 0x05, 0x00, 0x0E, 0x9F, 0x00, 0x00, 0x00]` |
| RX | 15 字节 |
| 超时 | 3 秒 |

MID 提取（从应答的字节 12-14，小端序）：
```rust
let mid = buf[12] as u32 | ((buf[13] as u32) << 8) | ((buf[14] as u32) << 16);
```

### 4. 读取 Flash 状态寄存器 (CMD 0x0C)

| 字段 | 值 |
|------|-----|
| 类型 | 长命令 |
| TX | `[0x01, 0xE0, 0xFC, 0xFF, 0xF4, 0x02, 0x00, 0x0C, sr_cmd]` |
| RX | 13 字节，SR 值在 buf[11] |
| 超时 | 2 秒 |

常用 SR 命令：
- `0x05`: 读取 SR1（状态寄存器 1）
- `0x35`: 读取 SR2（状态寄存器 2）

### 5. 写入 Flash 状态寄存器 — 1 字节 (CMD 0x0D)

| 字段 | 值 |
|------|-----|
| 类型 | 长命令 |
| TX | `[0x01, 0xE0, 0xFC, 0xFF, 0xF4, 0x03, 0x00, 0x0D, wr_cmd, value]` |
| RX | 13 字节 |
| 超时 | 2 秒 |

### 6. 写入 Flash 状态寄存器 — 2 字节 (CMD 0x0D)

| 字段 | 值 |
|------|-----|
| 类型 | 长命令 |
| TX | `[0x01, 0xE0, 0xFC, 0xFF, 0xF4, 0x04, 0x00, 0x0D, wr_cmd, V0, V1]` |
| RX | 14 字节 |
| 超时 | 2 秒 |

### 7. 擦除扇区 — 4K (CMD 0x0F, 子命令 0x20)

| 字段 | 值 |
|------|-----|
| 类型 | 长命令 |
| TX | `[0x01, 0xE0, 0xFC, 0xFF, 0xF4, 0x06, 0x00, 0x0F, 0x20, A0, A1, A2, A3]` |
| RX | 16 字节 |
| 超时 | 3 秒 |

### 8. 擦除块 — 64K (CMD 0x0F, 子命令 0xD8)

| 字段 | 值 |
|------|-----|
| 类型 | 长命令 |
| TX | `[0x01, 0xE0, 0xFC, 0xFF, 0xF4, 0x06, 0x00, 0x0F, 0xD8, A0, A1, A2, A3]` |
| RX | 16 字节 |
| 超时 | 8 秒 |

### 9. 写入扇区 — 4K (CMD 0x07)

| 字段 | 值 |
|------|-----|
| 类型 | 长命令 |
| TX | `[0x01, 0xE0, 0xFC, 0xFF, 0xF4, 0x05, 0x10, 0x07, A0, A1, A2, A3, data[4096]]` |
| LEN | 4101 (LE: 0x05, 0x10) |
| 数据 | 固定 4096 字节 |
| RX | 15 字节 |
| 超时 | 5 秒 |

应答验证 — 地址回显：
```rust
let addr_echo = u32::from_le_bytes([buf[11], buf[12], buf[13], buf[14]]);
assert_eq!(addr_echo, sent_addr);  // 必须匹配发送地址
```

### 10. CRC 校验 (CMD 0x10)

| 字段 | 值 |
|------|-----|
| 类型 | 短命令 |
| TX | `[0x01, 0xE0, 0xFC, 0x09, 0x10, S0, S1, S2, S3, E0, E1, E2, E3]` |
| 参数 | start_addr (4B LE), end_addr (4B LE) |
| RX | 11 字节，CRC32 在 buf[7..11] |
| 超时 | 10 秒 |

---

## Flash 状态寄存器（SR）去保护

Flash 芯片出厂默认写保护，擦写前必须清除保护位。

### Flash SR 参数查找表

不同 Flash MID 对应不同的 SR 操作参数：

```
MID              | SR大小 | 读命令        | 清除掩码
─────────────────┼────────┼──────────────┼──────────
0x144051,0x134051| 1 byte | [0x05, 0xFF] | 0x007C
0x14405E,0x13405E| 1 byte | [0x05, 0xFF] | 0x007C
0x13311C         | 1 byte | [0x05, 0xFF] | 0x007C
0x1464C8         | 1 byte | [0x05, 0xFF] | 0x007C
0x15701C         | 1 byte | [0x05, 0xFF] | 0x003C
0x1423C2,0x1523C2| 2 byte | [0x05, 0x15] | 0x3012
0x1560C4         | 2 byte | [0x05, 0x35] | 0x407C
未知 (cap≥0x14)  | 2 byte | [0x05, 0x35] | 0x407C
未知 (cap<0x14)  | 1 byte | [0x05, 0x35] | 0x007C
```

其中：
- `0x407C` = 清除 CMP + BP0-BP4（2 字节 SR）
- `0x007C` = 清除 BP0-BP4（1 字节 SR）
- `cap` = MID 的高 8 位：`(mid >> 16) & 0xFF`

### 去保护流程

```
1. 读取 Flash MID (CMD 0x0E)
   → 获取 Manufacturer ID (3 字节)

2. 查找 SR 参数
   → 确定: sr_size, read_cmds, clear_mask

3. 读取状态寄存器:
   if sr_size >= 2:
       sr1 = ReadFlashSR(read_cmds[0])  // 通常 0x05
       sr2 = ReadFlashSR(read_cmds[1])  // 通常 0x35 或 0x15
       sr_val = (sr2 << 8) | sr1
   else:
       sr_val = ReadFlashSR(read_cmds[0])

4. 计算新 SR:
   new_sr = sr_val & ~clear_mask

5. 如果 new_sr == sr_val:
       → 已经去保护，跳过
   否则:
       → 写入新的状态寄存器
       → 等待 20ms 完成
```

---

## 扇区擦除策略

BK7258 使用自适应擦除以优化性能：

```
给定: start_addr, num_sectors (每扇区 4K)

SECTOR_SIZE = 0x1000 (4 KiB)
SECTORS_PER_BLOCK = 16 (64 KiB / 4 KiB)

1. 4K 擦除对齐到 64K 边界:
   while current < end && current % 16 != 0:
       擦除 4K 扇区 (CMD 0x0F, 子命令 0x20)
       current += 1

2. 64K 整块擦除:
   while end - current >= 16:
       擦除 64K 块 (CMD 0x0F, 子命令 0xD8)
       current += 16

3. 剩余 4K 擦除:
   while current < end:
       擦除 4K 扇区
       current += 1
```

每次擦除操作失败时重试，最多 5 次，每次间隔 50ms。

---

## 扇区写入流程

```
对每个 4K 扇区:
    1. 准备 4096 字节缓冲区
       ├─ 复制数据
       └─ 不足部分填充 0xFF
    2. 构建长命令帧
       ├─ 命令: 0x07
       ├─ 长度: 4101 (LE: 0x05, 0x10)
       ├─ 地址: 4 字节 LE
       └─ 数据: 4096 字节
    3. 清空接收缓冲区
    4. 发送帧（共 4108 字节）
    5. 读取应答 (15 字节, 超时 5s)
    6. 验证应答前缀 [0x04, 0x0E, 0xFF]
    7. 提取地址回显 buf[11..15]
    8. 校验地址回显 == 发送地址
    9. 失败则重试，最多 3 次，间隔 100ms
```

---

## 完整刷机流程

### 固件刷机（子进程模式，优先路径）

如果 .soc 中包含 `air602_flash.exe`，优先使用子进程方式：

```
Step 1: 解压 .soc (ZIP), 解析 info.json
Step 2: 启动子进程
  └─ air602_flash.exe download -p <COM#> -b 2000000 -s 0 -i <rom.bin>
Step 3: 等待子进程完成
Step 4: 捕获启动日志
  ├─ 打开日志端口 @ log_baud_rate (921600)
  ├─ 捕获 20 秒
  ├─ 搜索关键词: "luat:", "ap0:", "LuatOS", "EasyFlash"
  ├─ 关键词命中 → PASS
  └─ 未命中 → FAIL
```

### 固件刷机（原生模式，备用路径）

当 .soc 中不含 `air602_flash.exe` 时使用原生 Rust 协议：

```
Step 1: 解压 .soc (ZIP) 并解析 info.json
  ├─ 检测格式: ZIP (magic 0x50 0x4B)
  ├─ 提取文件
  └─ 解析 ROM 路径、波特率、地址

Step 2: 打开串口 @ 115200
  └─ 设置超时 200ms

Step 3: 进入 bootloader
  ├─ 5 阶段 DTR/RTS 脉冲
  ├─ LinkCheck × 200 (2ms 窗口)
  └─ 最多重复 30 次

Step 4: 切换波特率 (默认 2 Mbps)
  ├─ 发送 set_baud 命令
  ├─ 等待 delay_ms/2
  ├─ 主机切换波特率
  └─ 验证应答

Step 5: 读取 Flash MID
  ├─ 发送 GetFlashMID 长命令
  └─ 提取 MID (3 字节 LE)

Step 6: Flash 去保护
  ├─ 查找 SR 参数
  ├─ 读取状态寄存器
  ├─ 清除保护位
  └─ 写回新值

Step 7: 擦除固件区域
  ├─ 计算需要擦除的扇区数
  ├─ 自适应 4K + 64K 策略
  └─ 每次擦除最多重试 5 次

Step 8: 写入固件
  ├─ 每个 4K 扇区:
  │   ├─ 准备数据 (不足填充 0xFF)
  │   ├─ 发送写入命令
  │   ├─ 验证地址回显
  │   └─ 最多重试 3 次
  └─ 报告进度

Step 9: [可选] 构建并刷写脚本
  ├─ 从 Lua 文件构建 LuaDB
  ├─ 可选: 添加 BK CRC16 封装
  ├─ 擦除脚本分区
  └─ 写入脚本数据

Step 10: 关闭串口
  └─ 设备自动重启
```

### 仅刷脚本

```
1. 从 .soc 中解析 info.json 获取 script_addr, flash_br, 编译配置
2. 从 Lua 文件构建 LuaDB 脚本镜像
3. 如果 bkcrc=true，添加 BK CRC16 封装
4. 检查脚本大小不超过分区限制
5. 连接 bootloader (get_bus → set_baud → unprotect)
6. 擦除脚本分区 + 写入数据
7. 关闭串口，设备自动重启
```

### 清除文件系统/FSKV

```
1. 连接 bootloader
2. 计算分区的扇区数
3. 使用自适应策略擦除所有扇区
4. 关闭串口
```

---

## 重试机制

| 操作 | 最大重试 | 超时 | 间隔 |
|------|---------|------|------|
| DTR/RTS + LinkCheck 组合 | 30 次 | — | 20ms |
| 单次 LinkCheck | 200 次 | 2ms 读取窗口 | — |
| 波特率切换 | 1 次 | 600ms | — |
| Flash MID 读取 | 1 次 | 3s | — |
| 扇区擦除 (4K/64K) | 5 次 | 3s/8s | 50ms |
| 扇区写入 (4K) | 3 次 | 5s | 100ms |
| 启动日志捕获 | — | 20s 总计 | — |

---

## BK CRC16 封装

当 info.json 中 `bkcrc=true` 时，脚本分区的 LuaDB 数据会被 BK CRC16 封装。
封装由 `luatos_luadb::add_bk_crc()` 函数完成。

---

## 启动日志验证

刷机完成后，可通过捕获设备启动日志来验证固件是否正常运行：

```
1. 关闭刷机串口
2. 重新打开串口 @ log_baud_rate (默认 921600)
3. 持续读取 20 秒
4. 搜索关键词: "luat:", "ap0:", "ap1:", "LuatOS", "EasyFlash"
5. 如果命中任一关键词 → 验证通过 (PASS)
6. 未命中 → 验证失败 (FAIL)
```
