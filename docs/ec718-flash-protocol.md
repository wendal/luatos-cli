# EC718 (Air8000/Air780E系列) 刷机协议文档

## 芯片映射

| 模组型号 | 芯片型号 | chip.type | 芯片家族 | binpkg标识 |
|---------|---------|-----------|---------|-----------|
| Air8000 | EC718HM | ec7xx | ec718m | EC718HM_PRD |
| Air780EHM | EC718HM | ec7xx | ec718m | EC718HM_PRD |
| Air780EHV | EC718HM | ec7xx | ec718m | EC718HM_PRD |
| Air780EHG | EC718HM | ec7xx | ec718m | EC718HM_PRD |
| Air780EPM | EC718PM | ec7xx | ec718m | EC718PM_PRD |

## SOC文件格式

- 格式: 7z压缩包 (魔数: `0x377ABCAF`)
- 内容: info.json, luatos.binpkg, script.bin, luatos.elf 等
- info.json 关键字段:
  - `chip.type = "ec7xx"`
  - `download.script_addr = "48E000"` (脚本区起始地址)
  - `download.force_br = "1152000"` (强制波特率, 注意不是1152000, 实际USB用921600)
  - `download.log_br = "2000000"` (日志波特率)
  - `download.bitw = "64"` (位宽)
  - `download.bl_addr = "4000"` (bootloader地址)
- binpkg 使用 `pkgmode` 格式:
  - 头部偏移 0x1D8, 芯片名在 0x190
  - 每个entry: 364字节元数据 + 数据

## USB端口识别

### 正常运行模式 (App Mode)
- **VID:** `0x19D1` (Eigencomm)
- **PID:** `0x0001`
- 端口分布 (通过USB interface编号识别, **不能**按COM端口号排序!):
  - `x.2` (interface=2) → SOC Log / 命令端口 (AT+ECRST, 0x7E日志帧)
  - `x.4` (interface=4) → AP Log
  - `x.6` (interface=6) → 用户COM口
- **重要**: COM端口号与USB接口号的映射在不同PC上可能不同!
  例如: COM3=x.6, COM4=x.2, COM5=x.4 (非按COM号顺序)
- 代码中使用 `serialport` crate 的 `usb_info.interface` 字段匹配

### 下载模式 (Boot Mode)
- **VID:** `0x17D1` (Eigencomm Download)
- **PID:** `0x0001`
- 仅出现一个端口, 用于刷机

### 区分逻辑
```
运行模式: VID=0x19D1, PID=0x0001, 多端口, 按interface编号识别
下载模式: VID=0x17D1, PID=0x0001, 单端口
```

## 重启进入Boot模式 (关键!)

**仅适用于移芯(Eigencomm)模组**, 其他模组(如BK7258)通常由CH340/CH343控制重启过程.

### 自动进入Boot模式流程

当模组处于正常运行状态 (运行LuatOS固件), 需要通过以下两步命令序列使其重启进入下载模式:

#### 步骤1: AT复位命令
```
AT+ECRST=delay,799\r\n
```
- 字节序列: `41 54 2B 45 43 52 53 54 3D 64 65 6C 61 79 2C 37 39 39 0D 0A`
- 通过命令端口 (x.2) 发送, 波特率 115200
- 含义: 触发模组延迟799ms后复位
- 发送后等待 **200ms**

#### 步骤2: DIAG帧 — 强制进入下载模式
```
0x7E 0x00 0x02 0x7E
```
- `0x7E` = JTT帧定界符 (起始/结束)
- `0x00` = 长度字节
- `0x02` = 命令类型 (进入boot模式)
- 发送后等待 **800ms**

#### DIAG帧相关常量
```
JTT_PACK_FLAG       = 0x7E  // 帧定界符
JTT_PACK_CODE       = 0x7D  // 转义符
DIAG_REBOOT_MS      = 0x41  // 普通重启
DIAG_REBOOT_DOWNLOAD_MS = 0x42  // 重启到下载模式
```

### 完整流程

```
1. 枚举USB设备, 查找 VID=0x19D1, PID=0x0001
2. 找到命令端口 (x.2), 以 115200 baud 打开
3. 发送: AT+ECRST=delay,799\r\n
4. 等待 200ms
5. 发送: 0x7E 0x00 0x02 0x7E
6. 等待 800ms, 关闭端口
7. 循环枚举USB, 查找 VID=0x17D1, PID=0x0001 (下载端口)
8. 超时30秒未找到 → 提示用户手动进入boot模式
9. 找到下载端口 → 开始刷机
```

### 手动进入Boot模式 (失败回退)
当自动方式失败时, 需要用户手动操作:
1. 按住BOOT键
2. 按下RESET/POWER键
3. 松开BOOT键
4. 等待模组以 VID=0x17D1 重新枚举

### UART模式
- 使用物理UART引脚 (TX/RX), 非USB CDC
- 需要外部USB转UART适配器
- 可能需要手动控制BOOT引脚
- 波特率: 115200 (初始同步)

## 刷机协议详解

### 协议层次

```
┌─────────────────────────────┐
│  LPC Commands (分区操作)     │
├─────────────────────────────┤
│  AGBOOT Commands (数据传输)  │
├─────────────────────────────┤
│  DLBOOT Commands (agentboot) │
├─────────────────────────────┤
│  Serial / USB Transport     │
└─────────────────────────────┘
```

### 同步字 (Sync Words)

| 模式 | 同步字 (LE) | 用途 |
|------|------------|------|
| DLBOOT | `0x2B02D300` | 初始同步 / agentboot下载 |
| AGBOOT | `0x2B02D3AA` | agentboot就绪后同步 |
| LPC | `0x2B02D3CD` | 分区烧录操作 |

同步过程: 循环发送同步字, 等待回声 + 额外字节. DLBOOT模式下额外字节必须为0.

### 命令帧格式

#### 发送 (8字节)
```
[cmd:u8][index:u8][order_id:u8=0xCD][norder_id:u8=0x32][len:u32le]
```
- `cmd`: 命令码
- `index`: 序号
- `order_id`: 固定 0xCD
- `norder_id`: 固定 0x32 (0xCD的取反)
- `len`: 数据长度

#### 响应 (6字节)
```
[cmd:u8][index:u8][order_id:u8][norder_id:u8][state:u8][len:u8]
```
- `state`: 0=成功, 非0=失败

### 命令码

| 命令 | 值 | 模式 | 用途 |
|------|----|------|------|
| BASE_INFO | 0x00 | DLBOOT | 基本信息 (HEAD=0x00, BL=0x01) |
| IMAGE_HEAD | 0x01 | DLBOOT | 镜像头 (272字节) |
| DOWNLOAD_DATA | 0x02 | DLBOOT/AGBOOT | 下载数据块 |
| DOWNLOAD_END | 0x03 | DLBOOT | 下载结束 |
| PACKAGE_DATA | 0x05 | AGBOOT | 分区数据块 (64KB) |
| LPC_BURN_ONE | 0x10 | LPC | 烧录一个分区 |
| LPC_GET_BURN_STATUS | 0x11 | LPC | 查询烧录状态 |
| LPC_SYS_RESET | 0x12 | LPC | 系统复位 |

### 校验算法

#### CRC8-Maxim (用于AGBOOT命令长度字段)
- 多项式: 0x31 (x^8 + x^5 + x^4 + 1)
- 初始值: 0x00
- 用法: len字段的最高字节 = CRC8(低3字节)

#### CRC32 (用于AGBOOT命令追加)
- 标准CRC32
- AGBOOT模式下, 每个命令+数据后追加4字节CRC32

#### self_def_check1 (用于DLBOOT数据下载)
- 简单加法校验和: 逐字节累加, 取低16位
- 仅用于 DOWNLOAD_DATA 命令

#### SHA256 (用于镜像头)
- 对完整数据计算SHA256, 存入272字节镜像头

### 镜像头格式 (272字节)

```
偏移  长度   内容
0x00  4     magic: "IMGH" / "AGBT" / "AIMG" / "CIMG" / "FLEX"
0x04  4     image_len (数据长度)
0x08  4     image_type (0=boot, 1=cp, 2=app, 3=flash)
0x0C  4     flags
0x10  4     baud_rate (新波特率, 0=不改变)
0x14  4     reserved
0x18  32    sha256_hash
0x38  ...   padding to 272 bytes
```

### 完整烧录序列

```
1. 打开端口 → DLBOOT sync
2. AgentBoot下载:
   a. base_info(HEAD) → 获取设备信息
   b. image_head(AGBT, baud=921600) → 发送agentboot镜像头
   c. DLBOOT sync (确认)
   d. base_info(BL) → bootloader信息
   e. package_data(agentboot_bin) → 发送agentboot二进制
3. 对每个分区:
   a. LPC sync → 进入LPC模式
   b. lpc_burn_one(partition_name, stor_type) → 开始烧录分区
   c. AGBOOT sync x2 → 进入AGBOOT模式
   d. base_info(HEAD) → 获取信息
   e. image_head(type, baud=0) → 发送分区镜像头
   f. 循环: AGBOOT sync + package_data(64KB块)
   g. lpc_get_burn_status → 确认烧录完成
4. LPC sync → lpc_sys_reset → 复位模组
```

### EC7xx CP分区特殊处理

**关键**: EC7xx系列 (EC716/EC718) 的CP分区与非EC7xx有重要区别:

| 项目 | EC7xx | 非EC7xx |
|------|-------|---------|
| stor_type | `STYPE_AP_FLASH` | `STYPE_CP_FLASH` |
| lpc_burn_one载荷 | 4字节 (img_id) | 6字节 (img_id + CP_FLASH_MARKER 0xE101) |
| CP地址偏移 | 需减去0x800000 (若addr ≥ 0x800000) | 直接使用原始地址 |

示例: `cp-demo-flash addr=0x82D000` → EC7xx实际烧录地址 = `0x82D000 - 0x800000 = 0x2D000`

### AgentBoot二进制

- 存放路径: `refs/origin_tools/ec_download/agentboot/`
- ec718m_usb.bin: 40696字节 (USB模式)
- ec718m_uart.bin: 47890字节 (UART模式)
- 来源: https://github.com/yuzhan-tech/luatos-tools

## 日志输出

- 协议: 0x7E HDLC帧 (与Air1601/CCM4211的0xA5帧不同!)
- 波特率: **921600** (info.json中标注2000000, 但Windows USB CDC不支持, 实际使用921600)
- 需要发送探测命令才开始输出 (探测帧格式与ccm4211相同, 0xA5帧)
- 日志端口: USB interface 2 (x.2), 与AT命令端口相同
- DTR/RTS: 打开端口后需设为 HIGH

### USB端口分配

**重要: COM端口号与USB接口号的映射不固定! 必须通过USB interface编号识别.**

| USB接口 (interface) | 功能 | 说明 |
|---------|------|------|
| interface=2 (x.2) | SOC日志 + AT命令 | 0x7E帧日志, AT+ECRST重启命令 |
| interface=4 (x.4) | AP日志 | AP子系统日志 |
| interface=6 (x.6) | 用户串口 | 用户自定义通信 |

示例: COM3=x.6, COM4=x.2, COM5=x.4 (非按COM号递增!)

### 日志协议详情

EC718使用 **0x7E HDLC帧格式**, 与Air1601/CCM4211的0xA5帧完全不同.

#### 帧格式
```
[0x7E 起始] [HDLC转义载荷] [0x7E 结束]
```

连续帧之间共享定界符: `... payload1 0x7E payload2 0x7E payload3 ...`

#### HDLC转义规则
| 原始字节 | 转义后 |
|---------|--------|
| `0x7E` | `0x7D, 0x5E` |
| `0x7D` | `0x7D, 0x5D` |
| 其他 | 不变 |

反转义: `escaped_byte ^ 0x20`

#### 12字节帧头
| 偏移 | 长度 | 字段 | 说明 |
|------|------|------|------|
| 0 | 4 | ms | 设备毫秒时间戳 (u32 LE) |
| 4 | 4 | reserved | 保留, 始终为0 |
| 8 | 4 | tag | 源模块标识符 (u32 LE) |

#### printf消息体 (字节12起)

- NUL结尾的格式字符串
- 参数从 `(nul_pos + 4) & ~3` 偏移开始 (4字节对齐)
- 日志级别/模块名嵌入在格式字符串文本中: `"I/http ..."`, `"D/net ..."`

#### 参数编码 (与0xA5协议有差异!)

| 格式符 | 编码 |
|--------|------|
| `%d`, `%u`, `%x` | i32/u32 LE (4字节) |
| `%lld` | i64 LE (8字节) |
| `%s` | `[u32 长度] [字符串字节]`, 4字节对齐 |
| `%.*s` | `[u32 精度] [字符串字节]`, 4字节对齐 |
| `%p` | u32 LE (4字节) |
| `%f` | f64 LE (8字节) |

**注意**: `%s` 使用长度前缀, 非NUL结尾, 这与0xA5协议不同.

#### 探测命令 (Probe)

固件缓冲日志输出, 需发送探测帧触发日志开始. 探测帧使用0xA5格式:
```
build_soc_frame(cmd=1, address=0, payload=[], sn=1)
```
即发送 SOC_CMD_GET_BASE_INFO (cmd=1) 帧, 与CCM4211完全相同.
设备收到后以0x7E帧格式输出日志.

#### DTR/RTS 控制

打开日志端口后, 需要设置:
```
DTR = HIGH (True)
RTS = HIGH (True)
```
参考 luatools_py3 的 usb_device.py, 所有日志端口打开后均设置 DTR/RTS HIGH.

### 刷机后日志端口变化

EC718刷机后模组复位, USB重新枚举:
1. 刷机使用下载端口: VID=0x17D1 (boot模式)
2. 复位后变为运行模式: VID=0x19D1 (3个端口)
3. 日志端口为 **第二个COM号** (x.4接口, 非最低COM号!)
4. 需要等待USB重新枚举 (约5-15秒)
5. 设置 DTR=HIGH, RTS=HIGH
6. 发送探测命令后才开始接收日志

### CLI 使用

```bash
# 自动检测EC718日志端口 (自动找到第二个COM口)
luatos-cli log view-binary --port auto --probe

# 指定端口 (COM4 = 日志口, 非COM3)
luatos-cli log view-binary --port COM4 --baud 2000000 --probe

# 刷机+日志测试 (自动处理端口变化)
luatos-cli flash test --soc firmware.soc --port COM3 --keyword "LuatOS@"
```

## FlashToolCLI.exe 回退方案

保留通过命令行调用移芯官方 FlashToolCLI.exe 的方式作为调试后备:
```
FlashToolCLI.exe --cfgfile config_pkg_product_usb.ini
```
- USB模式使用 `config_pkg_product_usb.ini`
- UART模式使用 `config_pkg_product_uart.ini`

## 参考资料

- 参考项目: https://github.com/yuzhan-tech/luatos-tools
- Python参考: D:\github\luatools_py3 (soc.py, usb_device.py)
- 固件源码: luatos-soc-2024 代码库
