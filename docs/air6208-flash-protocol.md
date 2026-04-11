# Air6208 (XT804) 刷机协议文档

## 概述

Air6208 模组使用 XT804 芯片（联盛德），刷机流程如下：

1. **Bootloader 同步**：通过 ESC 字节握手进入 ROM bootloader
2. **波特率切换**：从 115200 切换到目标波特率（默认 2 Mbps）
3. **Flash 擦除**：发送擦除命令清除整个 Flash
4. **XMODEM-1K 传输**：通过 XMODEM 协议传输固件镜像
5. **设备复位**：发送复位命令重启设备

协议参考：wm_tool.c (luatos-soc-air101)

## 串口参数

| 参数 | 值 |
|------|-----|
| 初始波特率 | 115200 bps |
| 目标波特率 | 2000000 bps（可通过 info.json `force_br` 配置）|
| 校验位 | NONE |
| 数据位 | 8 |
| 停止位 | 1 |
| 流控 | 仅 DTR/RTS 信号（用于复位/同步）|

## SOC 文件结构

SOC 文件是 **7z** 格式的压缩包（魔数 `0x37 0x7A 0xBC 0xAF`），包含：

```
LuatOS-SoC_V2001_Air6208_101.soc (7z archive)
├── info.json              # 芯片信息、地址配置
├── luatos.bin             # 固件 ROM
├── script.bin             # Lua 脚本字节码
├── air101_flash.exe       # 备用刷机工具 (Windows)
└── [可选] .elf, .map 文件
```

### info.json 关键字段

```json
{
  "version": 2001,
  "chip": {
    "type": "air6208"
  },
  "rom": {
    "file": "luatos.bin",
    "fs": {
      "script": {
        "size": 512,
        "bkcrc": false
      }
    }
  },
  "script": {
    "file": "script.bin",
    "bitw": 64,
    "use-luac": true,
    "use-debug": false
  },
  "download": {
    "bl_addr": "0x0",
    "script_addr": "0x200000",
    "force_br": "2M"
  }
}
```

---

## 阶段一：DTR/RTS 复位序列

通过控制 DTR/RTS 信号使设备进入 bootloader 模式：

```
Phase 1: 设置启动模式
  DTR = 0 (assert reset)
  RTS = 1 (assert boot mode)
  等待 50ms

Phase 2: 释放复位
  DTR = 1 (release reset)
  RTS = 0 (release boot)
  等待 50ms

Phase 3: 最终状态
  DTR = 0
  等待 50ms
```

## 阶段二：Bootloader 同步

设备 ROM bootloader 等待 ESC 字节（0x1B）进行同步。

### 同步协议

```
1. 发送 ESC 突发: 50 × 0x1B，每次间隔 10ms（共 500ms）
2. 循环读取应答:
   ├─ 收到 'C' 或 'P': 计数器 +1
   ├─ 收到其他字节:    计数器归零
   ├─ 计数器到 3:      同步成功 ✓
   ├─ 读取超时:        发送 3 × 0x1B（30ms 突发）
   └─ 每 2 秒:         翻转 RTS 作为恢复机制
3. 总超时: 60 秒
```

## 阶段三：波特率切换

同步后发送预定义的 13 字节命令切换波特率。

### 波特率命令表

| 波特率 | 命令（hex）|
|--------|-----------|
| 115200 | `21 0a 00 97 4b 31 00 00 00 00 c2 01 00` |
| 460800 | `21 0a 00 07 00 31 00 00 00 00 08 07 00` |
| 921600 | `21 0a 00 5d 63 31 00 00 00 00 10 0e 00` |
| 1000000 | `21 0a 00 7e 1a 31 00 00 00 40 42 0f 00` |
| **2000000** | `21 0a 00 ef 2a 31 00 00 00 80 84 1e 00` |

### 切换流程

```
1. 发送 13 字节波特率命令
2. 刷新发送缓冲区
3. 等待 1 秒（让设备内部完成切换）
4. 在已打开的串口上就地修改主机端波特率
5. 清空接收缓冲区
6. 重新同步: 发送 ESC 突发，等待 C/P 应答
```

## 阶段四：Flash 擦除

擦除整个 Flash。

### 擦除命令（13 字节）

```
[0x21, 0x0a, 0x00, 0xc3, 0x35, 0x32, 0x00, 0x00, 0x00, 0x02, 0x00, 0xfe, 0x01]
```

### 擦除流程

```
1. 发送擦除命令
2. 刷新发送缓冲区
3. 等待最多 60 秒
4. 计数连续 'C' 或 'P' 应答
5. 收到 3 个连续 C/P → 擦除完成
```

## 阶段五：XMODEM-1K 传输

使用 XMODEM-1K 协议传输固件镜像。

### 帧格式

```
┌─────────┬──────────┬────────────┬──────────────┬───────────┬──────────┐
│  STX    │ Block#   │ ~Block#    │ Data (1024B) │ CRC16 HI  │ CRC16 LO │
│  0x02   │  u8      │   u8       │ 数据块       │  (MSB)    │  (LSB)   │
│  1 byte │ 1 byte   │ 1 byte     │ 1024 bytes   │ 1 byte    │ 1 byte   │
└─────────┴──────────┴────────────┴──────────────┴───────────┴──────────┘
总长度: 3 + 1024 + 2 = 1029 字节
```

| 字段 | 说明 |
|------|------|
| STX (0x02) | 1K 块起始标记 |
| Block# | 块序号，从 1 开始，256 时回绕 |
| ~Block# | Block# 的按位取反（错误检测）|
| Data | 固定 1024 字节，不足部分填充 0x00 |
| CRC16 | 对 1024 字节数据计算 CRC16-CCITT，大端序 |

### 应答码

| 代码 | 值 | 含义 |
|------|------|------|
| ACK | 0x06 | 块已接受，继续 |
| NAK | 0x15 | 块被拒绝，重传 |
| CAN | 0x18 | 接收方取消传输 |
| EOT | 0x04 | 传输结束 |

### 传输握手

```
每个数据块:
  主机 → 设备: [STX, blk#, ~blk#, data[1024], CRC_HI, CRC_LO]
  设备 → 主机: ACK(0x06) 成功 / NAK(0x15) 重传 / CAN(0x18) 取消

  NAK: 重试，最多 100 次
  ACK: 继续下一块
  超时(5s): 重试

传输结束:
  主机 → 设备: EOT (0x04)
  设备 → 主机: ACK (0x06)

  注意: XT804 bootloader 通常在最后一个数据块 ACK 后立即重启。
  EOT 的 ACK 可能永远不会到达。如果所有数据块都已 ACK，超时是可接受的。
```

### CRC16-CCITT 算法

```rust
fn crc16_ccitt(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;  // 初始值 = 0
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;  // 多项式 0x1021
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}
```

参数：
- **多项式**: 0x1021
- **初始值**: 0x0000
- **输入**: 1024 字节数据块
- **输出**: 16 位 CRC，大端序（高字节先发）

验证值：`"123456789"` → `0x31C3`

---

## XT804 镜像头格式

固件镜像必须包含 256 字节头部（偏移 0），其中前 64 字节为有效数据。

### 头部结构（64 字节，小端序）

```
偏移    类型      字段名             说明
──────────────────────────────────────────────────────────────
0x00    u32 LE   magic_no            0xA0FFFF9F
0x04    u16 LE   img_type            0 = secboot, 1 = 用户镜像
0x06    u16 LE   zip_type            0 = 未压缩
0x08    u32 LE   run_img_addr        目标 Flash 地址
0x0C    u32 LE   run_img_len         负载长度（1024 对齐）
0x10    u32 LE   img_header_addr     头部存储地址
0x14    u32 LE   upgrade_img_addr    升级镜像地址
0x18    u32 LE   run_org_checksum    填充后负载的 CRC32
0x1C    u32 LE   upd_no              更新编号 (0)
0x20    16 bytes ver[16]             版本字符串（保留）
0x30    u32 LE   reserved0           0
0x34    u32 LE   reserved1           0
0x38    u32 LE   next_boot           下一个启动头地址（0 = 最后/唯一镜像）
0x3C    u32 LE   hd_checksum         header[0..60] 的 CRC32
```

### 镜像类型

| 类型 | img_type | img_header_addr | upgrade_img_addr | 用途 |
|------|----------|-----------------|------------------|------|
| 固件 | 0 | 0x08002000 | 0x08010000 | 完整系统启动 |
| 分区（脚本、文件系统、KV）| 1 | 0x20008000 | 0 | 运行时分区更新 |

### 负载填充规则

```
padded_len = ((original_len + 1023) & ~1023).max(1024)
填充字节: 0x00
最小大小: 1024 字节
```

### CRC32 算法

```rust
fn xt804_crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFFFFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB88320;  // Ethernet/zlib 多项式
            } else {
                crc >>= 1;
            }
        }
    }
    crc
}
```

CRC32 校验字段：
- `run_org_checksum`: 填充后整个负载的 CRC32
- `hd_checksum`: header[0..60]（不含校验字段本身）的 CRC32

---

## 其他命令

### 读取 MAC 地址（9 字节）

```
[0x21, 0x06, 0x00, 0xea, 0x2d, 0x38, 0x00, 0x00, 0x00]
```

应答格式：`MAC:AABBCCDDEEFF\n`（纯文本）

### 复位设备（9 字节）

```
[0x21, 0x06, 0x00, 0xc7, 0x7c, 0x3f, 0x00, 0x00, 0x00]
```

发送后等待 500ms，设备会自动重启。

---

## 完整刷机流程

### 固件刷机

```
Step 1: 解压 .soc (7z) 并解析 info.json
  ├─ 检测格式: 7z (magic 0x37 0x7A 0xBC 0xAF)
  ├─ 提取文件
  ├─ 解析 info.json
  └─ 读取 ROM 路径和波特率配置

Step 2: 打开串口 @ 115200
  └─ 等待 500ms 串口稳定

Step 3: 复位进入 bootloader
  └─ DTR/RTS 三阶段翻转序列

Step 4: Bootloader 同步
  ├─ 发送 50 × ESC (500ms)
  ├─ 循环等待 3 个连续 C/P
  └─ 总超时 60 秒

Step 5: 切换波特率 (默认 2 Mbps)
  ├─ 发送 13 字节波特率命令
  ├─ 等待 1 秒
  ├─ 主机切换波特率
  ├─ 清空缓冲区
  └─ 重新同步

Step 6: 擦除 Flash
  ├─ 发送 13 字节擦除命令
  └─ 等待 3 个连续 C/P (最多 60 秒)

Step 7: 验证镜像
  └─ 检查头部 magic 0xA0FFFF9F

Step 8: XMODEM-1K 传输
  ├─ 分块 1024 字节
  ├─ 每块: [STX, blk#, ~blk#, data, CRC16_HI, CRC16_LO]
  ├─ 等待 ACK (超时 5s)
  ├─ NAK 重试 (最多 100 次)
  ├─ 发送 EOT (0x04)
  └─ EOT ACK 超时可接受

Step 9: 复位设备
  ├─ 发送 9 字节复位命令
  └─ 等待 500ms
```

### 仅刷脚本

```
1. 解析 .soc 获取 script_addr 和编译配置
2. 构建 LuaDB 脚本镜像 (编译 .lua → .luac)
3. 包装 XT804 镜像头 (img_type=1, img_header_addr=0x20008000)
4. 连接 bootloader (复位 → 同步 → 切波特率)
5. XMODEM 传输（不需要全盘擦除，bootloader 根据 run_img_addr 定位写入）
6. 复位设备
```

### 刷文件系统

```
1. 解析 .soc 获取 fs_addr 和 fs_size
2. 构建 LittleFS 镜像
3. 包装 XT804 镜像头 (img_type=1)
4. 连接 bootloader
5. XMODEM 传输
6. 复位设备
```

### 清除文件系统/KV

```
1. 构造全 0xFF 数据（大小等于分区大小）
2. 包装 XT804 镜像头
3. XMODEM 传输
4. 复位设备
```

---

## 重试机制

| 操作 | 最大重试 | 超时 |
|------|---------|------|
| Bootloader 同步 | 无限制（60s 总超时）| 10ms 读取超时 |
| 波特率切换 | 1 次 | 1s 等待 |
| Flash 擦除 | 无限制（60s 总超时）| 100ms 读取超时 |
| XMODEM 数据块 | 100 次/块 | 5s ACK 超时 |
| XMODEM EOT | 3 次 | 2s ACK 超时 |

---

## 备用刷机方式

如果 .soc 包中包含 `air101_flash.exe`，可使用子进程方式刷机：

```
air101_flash.exe -ds <baud> -p <COM> -rs at -eo all -dl <fls_file>
```
