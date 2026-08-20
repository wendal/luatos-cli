# Air724UG (UIS8910DM / RDA8910) 刷机协议文档

## 概述

Air724UG 模组基于紫光展锐 **UIS8910DM**（项目代号 **RDA8910**，CAT1 bis 平台）芯片，Flash 为 SPI NOR，通过下载通道（USB CDC / UART）完成刷机。

刷机采用 **分级下载（FDL）** 模型，整个流程分三阶段（**协议细节经真实硬件验证**）：

1. **PDL 阶段**：芯片固化的 ROM 通过 **PDL 协议**（`0xAE` 帧，无校验、无转义）把 **FDL1** 下载到 RAM 0x8000C0 并启动。此阶段 **USB 传输边界参与分帧**——帧头与负载必须分开发送。
2. **FDL1 阶段**：FDL1（一级下载器）通过 **FDL 协议**（`0x7E` HDLC 帧 + CheckSum + 转义）把 **FDL2** 下载到 RAM 0x810000 并启动。
3. **FDL2 阶段**：FDL2（二级下载器）响应 PC 端指令把固件镜像写入 Flash（支持擦除、写入、读取、CRC 校验、格式化等扩展操作）。此阶段同样使用 FDL 协议 + CheckSum。

```
PC  ──(PDL 0xAE)─▶  ROM ──▶ 载入 FDL1 到 RAM 0x8000C0
PC  ──(FDL 0x7E)─▶ FDL1 ──▶ 载入 FDL2 到 RAM 0x810000
PC  ──(FDL 0x7E)─▶ FDL2 ──▶ 写固件镜像到 SPI NOR Flash
```

### 与通用 UNISOC 协议的差异（RDA8910 特化）

| 项目 | 通用协议 | RDA8910 实际实现 |
|---|---|---|
| ROM→FDL1 通信协议 | FDL（`0x7E` + CRC16）| **PDL（`0xAE` 帧，无校验）** ✓ |
| FDL1/FDL2 校验算法 | CheckSum | CheckSum（小端字累加）✓ |
| PDL 分帧 | — | **按 USB 传输边界**，帧头与负载分开发 |
| FDL2 下载分块 | — | **0x210**（528B，经真机验证）|
| FDL2 CHANGE_BAUD 值 | — | **4000000**（USB CDC 下为名义值）|
| FDL2 启动应答 | `BSL_REP_INCOMPATIBLE_PARTITION` (0x96) | 启动即回 `BSL_REP_ACK` (0x80) |
| FDL1 下载目标地址 | 工具指定 | 必须为 `CONFIG_FDL2_IMAGE_START` (0x810000) |
| 进入下载模式 | 按键 / 指令 | 上电按键（枚举为 SPRD U2S Diag，VID 0525:PID a4a7）|

---

## 串口参数

| 参数 | 值 |
|------|-----|
| 默认下载 UART | UART2（`CONFIG_FDL_DEFAULT_UART`，Air724UG 调试口）|
| 初始波特率 | **115200** bps |
| 目标波特率 | **4000000** bps（FDL `BSL_CMD_CHANGE_BAUD` 下发值；USB CDC 下为名义值，主机不切换）|
| 数据位 | 8 |
| 校验位 | NONE |
| 停止位 | 1 |
| 帧格式 | PDL 阶段 `0xAE` 帧；FDL 阶段 `0x7E` HDLC |

> **重要**：经真机验证，FDL 阶段的 `BSL_CMD_CHANGE_BAUD` 下发值为 **4000000**。USB CDC（下载通道）下波特率为名义值，主机无需实际切换。
>
> **注**：`.soc` 的 `info.json` 中 `download.force_br` 常为 `1152000`（如 V1009），但真机抓包显示 FDL 阶段实际下发 **4000000**，`force_br` 仅供打包参考。

---

## SOC 文件结构

`LuatOS-SoC_V1009_Air724UG.soc` 是 **7z** 压缩包（魔数 `0x37 0x7A 0xBC 0xAF`），解压后结构与其他芯片（直接含 `luatos.bin`）不同——固件是 **RDA 私有 PAC 格式** 的包：

```
LuatOS-SoC_V1009_Air724UG.soc (7z archive)
├── info.json           # 芯片信息、分区地址、波特率（见下）
├── luatos.pac          # 主固件包（PAC 私有格式，含 FDL1/FDL2 + 全部分区镜像）
├── luatos_ap.pac       # AP 侧包（重复 HOST_FDL/FDL2/AP/LUA 四件，主包已含全部）
├── fota_all.xml        # FOTA 全量升级配置
├── fota_cp.xml         # FOTA CP 配置
├── luatos.elf          # 调试用 ELF (~19MB)
└── luatos.map          # 链接映射表 (~5MB)
```

### info.json 关键字段

```json
{
  "version": 1,
  "chip": { "type": "uis8910", "ram": { "total": 4000, "sys": 256, "lua": 128 } },
  "rom": {
    "file": "luatos.pac",
    "app_file": "luatos_ap.pac",
    "img_file": "csdk.img",
    "fs": { "script": { "offset": "0", "size": 512, "type": "luadb" } },
    "version-core": "v0001", "version-bsp": "v0001"
  },
  "download": {
    "bl_addr":       "00004000",
    "partition_addr": "00008000",
    "core_addr":     "00010000",
    "app_addr":      "00000000",
    "script_addr":   "00230000",
    "fs_addr":       "002c0000",
    "extra_param":   "002f0200",
    "force_br":      "1152000"
  },
  "user": { "log_br": "921600" },
  "fota": { "magic_num": "da188800", "block_len": "40000", "core_type": "diff", "ap_type": "full", "cp_type": "diff", "full_addr": "002BD000", "cp_version": "8910A002" }
}
```

> **分区地址说明**：`download.*_addr` 是 flash **偏移**（相对 SPI flash 起始）。实际刷机时 FDL2 使用的物理地址 = `0x60000000`（SPI flash 基址）+ 偏移。例如 `core_addr=0x10000` → 物理地址 `0x60010000`。`extra_param` 为 FOTA 预留区。

### PAC 私有格式

RDA/UNISOC 的 PAC 文件**不是压缩包**，而是一种私有二进制容器，由打包工具生成。

**整体布局**：

```
┌─────────────────────────────────────────────────────┐
│ PAC_HEADER (2124 B)                                  │
├─────────────────────────────────────────────────────┤
│ FILE_HEADER × N (2580 B/个)                          │
├─────────────────────────────────────────────────────┤
│ 文件数据 × N（顺序与文件头一致）                      │
└─────────────────────────────────────────────────────┘
```

**PAC 头**（`PAC_HEADER_FMT = "48sI512s512s7I200s3I800sI2H"`）：

| 字段 | 类型 | 说明 |
|---|---|---|
| szVersion | 48s UTF-16LE | 包版本，如 `BP_R1.0.0` |
| pac_size | u32 | 整个 PAC 文件大小 |
| szPrdName | 512s UTF-16LE | 产品名，如 `UIX8910_MODEM` |
| szPrdVersion | 512s UTF-16LE | 产品版本，如 `8910 MODULE` |
| file_count | u32 | 文件个数 |
| file_offset | u32 | 第一个文件头的偏移（=2124）|
| dwMode / dwFlashType / dwNandStrategy / dwIsNvBackup / dwNandPageType | 5×u32 | 下载模式 / Flash 类型 / NAND 策略 / NV 备份 / NAND 页 |
| szPrdAlias | 200s UTF-16LE | 别名，如 `csdk` |
| dwOmaDmProductFlag / dwIsOmaDm / dwIsPreload | 3×u32 | OMA-DM 标志 |
| reserved | 800s | 保留 |
| **magic** | u32 | **`0xFFFAFFFA`** |
| crc1 | u16 | 头校验 CRC16（不含 crc1/crc2 自身）|
| crc2 | u16 | 全部文件头+数据的 CRC16 |

**文件头**（`FILE_HEADER_FMT = "I512s512s512s6I5I996s"`）：

| 字段 | 类型 | 说明 |
|---|---|---|
| hdr_size | u32 | 文件头大小（=2580）|
| szFileID | 512s UTF-16LE | 文件 ID：`HOST_FDL` / `FDL2` / `BOOTLOADER` / `AP` / `PS` / `LUA` / `NV` / `PREPACK` / `FMT_FSSYS` / `FLASH` / `PhaseCheck` |
| szFileName | 512s UTF-16LE | 文件名，如 `fdl1.sign.img` |
| reserved | 512s | 保留 |
| file_size | u32 | 数据长度 |
| dwFileFlag / defaultCheck / offset / dwCanOmitFlag / always | 5×u32 | 标志 / 默认勾选 / 数据偏移 / 可省略 / 固定 |
| **dwAddress** | u32 | 下载目标地址（RAM 或 Flash 或逻辑地址）|
| reserved | 5×u32 + 996s | 保留 |

**CRC16**：表驱动 CRC16-ARC（poly `0x8005`，初值 0），仅用于 PAC 头部/内容完整性，与 FDL 线协议校验无关。

### luatos.pac 内容（实测 V1009）

解析 `LuatOS-SoC_V1009_Air724UG.soc` 内 `luatos.pac` 得到 12 个文件：

| szFileID | 文件名 | 大小 | dwAddress | 说明 |
|---|---|---|---|---|
| HOST_FDL | fdl1.sign.img | 15840 | `0x008000C0` | **FDL1**，ROM 阶段下载到 RAM |
| FDL2 | fdl2.sign.img | 59488 | `0x00810000` | **FDL2**，FDL1 阶段下载到 RAM |
| BOOTLOADER | boot.img | 47744 | `0x60000000` | boot 镜像 → flash offset 0 |
| AP | csdk.img | 1422352 | `0x60010000` | 应用核心 → flash offset 0x10000 |
| PS | airm2m.img | 3538944 | `0x60480000` | PS/文件系统 → flash offset 0x480000 |
| LUA | script.bin | 568 | `0x60230000` | Lua 脚本（LuaDB）→ flash offset 0x230000 |
| FMT_FSSYS | — | 0 | `0xFE000006` | 格式化 Flash 块设备（操作项，无数据）|
| FLASH | — | 0 | `0xFE000001` | 擦除 running NV（操作项）|
| NV | airm2m_nvitem.bin | 131072 | `0xFE000003` | 固定 NV |
| PREPACK | airm2m_prepack.cpio | 106 | `0xFE000004` | cpio 预打包文件，写入文件系统 |
| PhaseCheck | — | 0 | `0xFE000002` | PhaseCheck（操作项）|
| (XML) | UIX8910_MODEM.xml | 4706 | 0 | 内嵌 BMAConfig 配置 XML |

**刷机时对 PAC 的处理**：PAC 内文件按顺序执行——
- **FDL1/FDL2**：先下载到 RAM（`HOST_FDL` 由 ROM 载入，`FDL2` 由 FDL1 载入），随后 FDL2 处理剩余分区文件；
- **BOOTLOADER / AP / PS / LUA**：通过 FDL2 以 `START_DATA`（dwAddress 为物理地址）写入 Flash；
- **操作项**（`FMT_FSSYS`/`FLASH`/`PhaseCheck`）与 **NV/PREPACK**：通过 FDL2 的逻辑地址操作。

### FDL 镜像格式

`fdl1.sign.img` / `fdl2.sign.img` / `boot.img` 均以 **uImage 头**（`IH_MAGIC = 0x27051956`）开头，实测头 20 字节：

```
fdl1.sign.img: 27 05 19 56  8e ad 04 13  6a 82 8b ed  00 00 3b 40  00 80 01 00
                 ih_magic      ih_hcrc       ih_time       ih_size     ih_load
fdl2.sign.img: 27 05 19 56  f8 36 b3 6f  6a 82 8b ed  00 00 e5 c0  00 81 00 40
```

`.sign` 后缀表示镜像带签名（secure boot 场景使用；未使能时校验直接通过）。FDL1 的 `EXEC_DATA` 跳转目标为 `ih_ep` 字段。

---

## 进入下载模式

### 方式一：硬件（空片/上电按键）

芯片管脚在复位时必须处于正确状态（典型为音量下键或功能键 3/7 键）。复位后 Boot ROM 运行，进入下载模式，等待 PC 端开始 FDL1 下载。空片无需按键，上电即可进入。

### 方式二：软件（运行中切换）

设备正常运行时，boot2（Flash 内 bootloader）会以 **921600** 波特率监听默认 UART，收到 **连续 3 次** `0x7E`（`HDLC_FLAG`）后：

```
1. 发送版本应答（BSL_REP_VER, 0x81）
2. 延时 50ms
3. 复位进入下载模式（BOOT_RESET_FORCE_DOWNLOAD）
```

监听超时初始 50ms，收到首个 `0x7E` 后放宽到 150ms；累积 3 个 `0x7E` 即触发。

> 这是 RDA8910 的运行时软件入口：boot2 监听的是 **921600**，与 ROM 下载模式的 **115200** 不同，软件切换前后需注意波特率。UIS8910DM 的通用切换指令为通过 debughost 口发送 `AD 00 04 FD 03 44 52 E8`，两种入口在 luatos 场景均可由 boot2 收 `0x7E` 触发替代。

### 方式三：AT 自动化（可选）

运行状态通过 AT 指令让设备进入下载模式（PC 需安装专用 USB 驱动）：

```
AT+SPREF="AUTODLOADER"
```

自动化下载时设备侧由软件模拟 Boot Code，交互流程与正常下载一致。

### 运行模式 USB 端口布局

运行中 UNISOC-8910 复合设备（VID 1782）枚举多个同身份串口，功能靠 **USB 接口号**（Windows 位置 "x.N" / Linux `bInterfaceNumber`）区分，对齐 luatools_py3 `usb_device.py`：

| PID | 接口 | 功能 |
|---|---|---|
| 0x4d11（8910_soc） | x.6 | soc_log（LuatOS 日志）+ host fast 命令口 |
| 0x4d11 | x.5 | cp_log |
| 0x4d11 | x.2 | user_com |
| 0x4e00（8910_air） | x.2 | user_log（LuatOS 日志）+ host fast 命令口 |
| 0x4e00 | x.3 | at_log |
| 0x4e00 | x.4 / x.5 | ap_log / cp_log |

log 口与"host fast 重启进下载"命令口为同一接口。luatos-cli 的 `--port auto` 按此表自动识别 log 口（接口信息缺失时降级 AT 行为探测）。

---

## 数据包格式

所有指令与应答使用相同的帧格式（`BSL_CMD_CHECK_BAUD` 单字节指令除外）：

```
┌──────┬─────────┬──────────┬──────────┬───────────┬──────┐
│ 0x7E │ type    │ size     │ data     │ crc       │ 0x7E │
│ 包头 │ 包类型  │ 数据长度 │ 数据     │ 校验位    │ 包尾 │
│ 1B   │ 2B 大端 │ 2B 大端  │ N Byte   │ 2B        │ 1B   │
└──────┴─────────┴──────────┴──────────┴───────────┴──────┘
```

- **type**：指令/应答类型，Unsigned Short 大端。
- **size**：数据域长度，Unsigned Short 大端。**必须为偶数**；若为奇数，发送方在数据末尾补 `0x00`。数据包大小一般不超过 12KB（芯片 `CONFIG_FDL_PACKET_MAX_LEN=0x3000`）。
- **crc**：校验位，见下文「校验算法」。
- 组包：先填 type/size/data → 计算校验位 → 对 type/size/data/crc 四域做转义 → 加包头包尾。
- 解包：去包头包尾 → 还原转义 → 校验 crc。

### 转义规则

包内出现 `0x7E` 或 `0x7D` 必须转义（掩码 `0x20`）：

```
0x7E → 0x7D 0x5E     (0x5E = 0x7E ^ 0x20)
0x7D → 0x7D 0x5D     (0x5D = 0x7D ^ 0x20)
```

- **组包**：先将 `0x7E` 转义为 `0x7D 0x5E`，再将 `0x7D` 转义为 `0x7D 0x5D`。
- **解包**：遇到 `0x7D` 则忽略该字节，将其后一个字节与 `0x20` 异或还原。

RDA8910 的 FDL1/FDL2 全程使用转义。芯片侧不实现 `BSL_CMD_SET_NOT_TRANSCODE`（0x21）非转义模式。

---

## 校验算法

分两个协议场景：**PDL**（ROM→FDL1，`0xAE` 帧，**无校验**）与 **FDL**（FDL1/FDL2，`0x7E` 帧 + **CheckSum**）。

### 场景一：PDL（ROM→FDL1）—— 无校验

PDL 帧（`0xAE`）**不含任何校验位/转义**，负载原样传输。帧头与负载按 USB 传输边界分开发送（模块按传输边界分帧，一次性写整帧会解析失败）。

### 场景二：FDL1 / FDL2 通信 —— CheckSum

除 Boot Code 外的所有通信（FDL1 下载 FDL2、FDL2 擦写 Flash）均使用 CheckSum。算法为 **16 位字累加 + 折叠 + 取反**。

```c
/* CheckSum：按 16 位小端字累加 */
uint16_t crc16_fdl_calc(const uint16_t *src, unsigned len)
{
    uint32_t sum = 0;
    while (len > 3) {
        sum += *src++;      /* 小端读：byte[i] | byte[i+1]<<8 */
        sum += *src++;
        len -= 4;
    }
    switch (len & 0x03) {
    case 2: sum += *src++;                        break;
    case 3: sum += *src++; sum += *(uint8_t *)src; break;
    case 1: sum += *(uint8_t *)src;               break;
    default: break;
    }
    sum = (sum >> 16) + (sum & 0x0FFFF);
    sum += (sum >> 16);
    return ~(uint16_t)sum;
}
```

> **字节序关键点**：校验和按**小端** `uint16` 读取包字节。虽然包头的 type/size 在线上是大端字节序，但校验和的字累加是按小端解读的（`word = byte[i] | byte[i+1]<<8`）。Rust 实现必须用 `u16::from_le_bytes`，不要按大端。

**校验位字节序：小端追加**（低字节 `crc&0xff` 在前，高字节 `crc>>8` 在后）。

Rust 等价实现：

```rust
/// FDL 阶段 CheckSum：输入为未转义的包体字节 [type_be(2) | size_be(2) | content(N)]
/// 按 16 位小端字累加，折叠后取反
fn fdl_checksum(pkt: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    for chunk in pkt.chunks(2) {
        sum += if chunk.len() == 2 {
            u16::from_le_bytes([chunk[0], chunk[1]]) as u32
        } else {
            chunk[0] as u32
        };
    }
    sum = (sum >> 16) + (sum & 0xffff);
    sum += sum >> 16;
    (!sum) as u16
}
```

---

## 波特率设置与 CHECK_BAUD

### BSL_CMD_CHECK_BAUD (0x7E)

**特殊指令**，不遵循数据包格式，就是发送单个字符 `0x7E`。用于对齐两端波特率，必须在所有指令之前执行。

- 设备侧：启动后循环读取 1 字节，收到 `0x7E` 后回送版本应答 `BSL_REP_VER` (0x81)。
- 主机侧：打开串口后持续发送 `0x7E`；波特率不一致时芯片收不到，主机会调整并重试，直到一致。
- **应答**：`BSL_REP_VER` (0x81)，数据域为版本字符串。
  - FDL1 回送：`"Spreadtrum Boot Block version 1.2"`（含结尾 `\0`，`sizeof = 33`）
  - 其它芯片 ROM 的版本串可能不同，如 `"UNISOC Boot Block version 1.0"`
- Boot Code/FDL1 应答一次后不再应答此指令，除非重启。
- FDL1 和 FDL2 都会在启动后等待 `0x7E`，收到后才进入正常指令循环。

### BSL_CMD_CHANGE_BAUD (0x09)

切换波特率。数据域为 DWORD 大端波特率值。

```
主机 → 设备: type=0x09, data=0x000E1000 (921600)   ← 4 字节大端
设备 → 主机: ACK (0x80)                            ← 以旧波特率应答
```

芯片在发送 ACK 后切换波特率（先回 ACK，延时 200us，再切换波特率）。主机必须在收到 ACK 后立即切换到新波特率。

> 切换流程：清空收发缓冲 → 发送 CHANGE_BAUD → 收 ACK（旧波特率）→ 主机就地切换串口波特率 → 后续在新波特率下通信。

---

## 完整下载流程

### 阶段一：ROM 下载 FDL1（PDL 协议，`0xAE` 帧）

**帧格式**：帧头 `AE [payload_len LE 4] FF 00 00` + 负载。**帧头与负载必须分开发送**（模块按 USB 传输边界分帧，实测一次性写整帧解析失败）。

```
Step 1: PDL CONNECT
        帧头 + cmd=0(4 LE) + 8 字节零（负载共 12B）
        ← ACK: AE 04 00 00 00 FF 00 00 + resp=0
Step 2: PDL START_DATA
        帧头 + cmd=4(4 LE) + Addr(4 LE) + Size(4 LE) + "PDL1\0"（负载共 17B）
        Addr = 0x8000C0, Size = FDL1 镜像大小（15840）
        ← ACK
Step 3: PDL MID_DATA 循环（2048/块，最后一块为余数）
        帧头(size=12+块长) + cmd=5(4 LE) + 块索引(4 LE) + 块长(4 LE) + 数据
        帧头与负载分开发，整帧发完后再等 ← ACK
Step 4: PDL END_DATA
        帧头 + cmd=6(4 LE) + 8 字节零 + 4 字节尾（负载共 16B）
        ← ACK
Step 5: PDL EXEC_DATA
        帧头 + cmd=7(4 LE) + 8 字节零（负载共 12B）
        （无 ACK，FDL1 启动）
Step 6: 发送 0x7E 同步
        ← 收 BSL_REP_VER (0x81) + "Spreadtrum Boot Block version 1.2"
```

> **PDL 应答**：`AE 04 00 00 00 [FF|EE] 00 00` + `resp(4 LE)`，resp=0 为 ACK，flowid=0xFF 成功 / 0xEE 错误。
> **USB 传输边界**：帧头与负载必须用独立的 USB write 发送（实测一次性写整帧会解析失败）。
> **MID_DATA 整帧发送**：帧头 + cmd + 数据全发后再等 ACK（模块按帧头 size 字段等待完整负载）。
> **END_DATA 尾部 4 字节**：作用未明，实测值为 `1c 3c 6e 06`。

### 阶段二：FDL1 下载 FDL2（FDL 协议，`0x7E` + CheckSum）

FDL1 启动后等待 `0x7E` 同步，回送版本串，然后进入指令循环。

```
Step 1: 发送 0x7E（CHECK_BAUD）
        ← 收 BSL_REP_VER (0x81) + "Spreadtrum Boot Block version 1.2"
Step 2: BSL_CMD_CONNECT (0x00)
        ← ACK
Step 3: BSL_CMD_CHANGE_BAUD (0x09)
        data = 4000000（4 字节大端）   ← 真机验证值，USB CDC 下为名义值
        ← ACK（USB CDC 下主机无需切换波特率）
Step 4: BSL_CMD_CONNECT (0x00)   ← 二次 CONNECT（真机验证必需）
        ← ACK
Step 5: BSL_CMD_START_DATA (0x01)
        data = Addr + Size
        Addr = 0x810000 (CONFIG_FDL2_IMAGE_START)   ← 必须精确等于此值
        Size = FDL2 镜像大小（59488）
        ← ACK
Step 6: BSL_CMD_MIDST_DATA (0x02) 循环
        data = FDL2 数据块, 每包 0x210 (528B)   ← 真机验证分块
        ← ACK
Step 7: BSL_CMD_END_DATA (0x03)
        ← ACK
Step 8: BSL_CMD_EXEC_DATA (0x04)
        ← ACK（FDL2 启动即回）
Step 9: BSL_CMD_NORMAL_RESET (0x05) 重启（或 BSL_CMD_POWER_OFF (0x17) 关机）
        ← ACK
```

> **FDL 帧结构**：`7E [type BE 2] [size BE 2] [data] [CheckSum LE 2] 7E`。**数据含 0x7E/0x7D 时必须 HDLC 转义**（0x7E→0x7D 0x5E，0x7D→0x7D 0x5D），校验和按未转义的 body 计算、小端追加。实测未转义会导致含 0x7E 的数据块解析失败。
> **FDL2 镜像必须以 uImage header 开头**（`IH_MAGIC = 0x27051956` 大端，`ih_ep` 为入口地址），否则 EXEC 不跳转。

### 阶段三：FDL2 Flash 操作（CheckSum 校验）

FDL2 启动后**立即回送一个 ACK**，随后进入指令循环。RDA8910 的 FDL2 不主动发送 `BSL_REP_INCOMPATIBLE_PARTITION`，直接以 ACK 表明就绪，工具随后通过 `BSL_CMD_START_DATA/MIDST_DATA/END_DATA` 下载固件镜像。

```
Step 1: （EXEC 后）收 FDL2 的 ACK (0x80)
Step 2: BSL_CMD_CONNECT → ACK
Step 3: 对每个固件分区（boot / app / appimg / fs 等）:
        3a. BSL_CMD_START_DATA: Addr=Flash物理地址, Size=镜像大小 → ACK
        3b. BSL_CMD_MIDST_DATA: 帧(无尾 0x7E) + 单独 0x7E 分开发 → ACK
        3c. BSL_CMD_END_DATA → ACK（内部做 CRC32 回读校验，失败回 0x8B VERIFY_ERROR）
Step 4: [可选] BSL_CMD_ERASE_FLASH 擦除 / BSL_CMD_READ_FLASH 回读
Step 5: 收尾发 BSL_CMD_NORMAL_RESET (0x05) 重启
```

> **MIDST 帧尾 0x7E 单独发送（经真机验证）**：FDL2 写 Flash 的 `MIDST_DATA` 帧必须**不含尾 0x7E**，随后**单独发送一个 `0x7E`**（抓包见工具如此）。一次性发完整帧（含尾 0x7E）会导致设备等待尾标志而超时。此规则同样适用于 FDL2 下载阶段，按字节流解析的接收方不受影响。

---

## FDL1 支持指令

FDL1 仅支持下列指令（未列出的指令会直接 panic，不复用）：

| 指令 | Type | 数据域 | 应答 |
|---|---|---|---|
| CONNECT | 0x00 | — | ACK (0x80) |
| CHANGE_BAUD | 0x09 | 波特率 DWORD 大端 | ACK（旧波特率）|
| START_DATA | 0x01 | Addr(4)+Size(4) 大端，Addr 必须 = 0x810000 | ACK / DOWN_DEST_ERROR / DOWN_SIZE_ERROR |
| MIDST_DATA | 0x02 | FDL2 数据，包 ≤ 0x840 | ACK / DOWN_NOT_START / DOWN_SIZE_ERROR |
| END_DATA | 0x03 | — | ACK / SEC_VERIFY_ERROR |
| EXEC_DATA | 0x04 | — | 跳转 ih_ep 启动 FDL2 |

---

## FDL2 支持指令

| 指令 | Type | 数据域 | 应答 |
|---|---|---|---|
| CONNECT | 0x00 | — | ACK |
| START_DATA | 0x01 | Addr(4)+Size(4) 大端 | ACK / DOWN_SIZE_ERROR / DOWN_DEST_ERROR / INCOMPATIBLE_PARTITION / OPERATION_FAILED / PHONE_NOT_ENOUGH_MEMORY |
| MIDST_DATA | 0x02 | 数据块 | ACK / DOWN_NOT_START / DOWN_SIZE_ERROR |
| END_DATA | 0x03 | — | ACK / DOWN_NOT_START / DOWN_SIZE_ERROR / VERIFY_ERROR / OPERATION_FAILED |
| NORMAL_RESET | 0x05 | — | ACK（2000us 后复位正常启动）|
| READ_FLASH | 0x06 | Addr(4)+Size(4)+Offset(4) 大端 | READ_FLASH (0x93) + 数据 / INVALID_CMD / OPERATION_FAILED / INCOMPATIBLE_PARTITION |
| ERASE_FLASH | 0x0A | Addr(4)+Size(4) 大端 | ACK / INCOMPATIBLE_PARTITION / OPERATION_FAILED / INVALID_CMD |
| REPARTITION | 0x0B | — | ACK（RDA8910 为空操作）|
| POWER_OFF | 0x17 | — | ACK（延时关机）|
| CHANGE_BAUD | 0x09 | 波特率 DWORD 大端 | ACK（旧波特率）/ NOT_SUPPORT_BAUDRATE |
| ENABLE_DEBUG_MODE | 0x25 | `enable[:level=n]` | ACK |
| 其它 | — | — | INVALID_CMD (0x82) |

### FDL2 Flash 写入行为

关键实现细节：

- **分页写入**：`FLASH_PAGE_SIZE = 256` 字节，数据边收边写（数据放入环形缓冲，逐 256B 页写入）。`FLASH_SECTOR_SIZE = 0x1000`（4KB）。
- **擦除策略**：`START_DATA` 时按 4KB 对齐擦除目标扇区；若目标区域内容全为 `0xFF` 则跳过擦除。
- **CRC 校验**：`MIDST_DATA` 累计 CRC32，`END_DATA` 时把 Flash 回读内容 CRC32 与累计值比对，不一致回 `BSL_REP_VERIFY_ERROR` (0x8B)。
- **整体写模式**：secure boot 使能时 boot/app 镜像整包缓存、签名校验后一次性擦写。

### 逻辑地址（伪分区）

FDL2 把下列地址视为特殊逻辑分区，`START_DATA/READ_FLASH/ERASE_FLASH` 传入即可操作：

| 逻辑地址 | 含义 | 支持操作 |
|---|---|---|
| `0xFE000001` | 擦除 running NV | ERASE_FLASH |
| `0xFE000002` | PhaseCheck | START/READ/ERASE |
| `0xFE000003` | FixedNV (固定 NV bin) | START/READ/ERASE |
| `0xFE000004` | PrePack 文件（cpio 流）| START（边收边解 cpio 写文件系统）|
| `0xFE000005` | 删除 appimg 文件 | ERASE_FLASH |
| `0xFE000006` | 格式化 Flash 块设备 | ERASE_FLASH |
| `0x00000000` + Size=`0xFFFFFFFF` | 擦除整片 Flash（chip erase）| ERASE_FLASH |

> FDL2 会先备份 NV 与 PhaseCheck 再做整片擦除，擦除后回写，避免出厂校准数据丢失。

---

## RDA8910 关键常量

关键常量：

| 常量 | 值 | 说明 |
|---|---|---|
| `CONFIG_FDL1_IMAGE_START` | `0x8000C0` | FDL1 下载 RAM 地址（ROM 阶段 START_DATA 用）|
| `CONFIG_FDL1_IMAGE_SIZE` | `0xFF40` | FDL1 镜像大小上限 |
| `CONFIG_FDL2_IMAGE_START` | `0x810000` | FDL2 下载 RAM 地址（FDL1 阶段 START_DATA 必须用）|
| `CONFIG_FDL2_IMAGE_SIZE` | `0x30000` | FDL2 镜像大小上限 |
| `CONFIG_FDL1_PACKET_MAX_LEN` | `0x840` (2112) | FDL1 引擎最大包长（FDL2 数据包上限）|
| `CONFIG_FDL_PACKET_MAX_LEN` | `0x3000` (12288) | FDL2 引擎最大包长（固件镜像数据包上限）|
| `CONFIG_FDL_UART_BAUD` | `921600` | 切换后的默认波特率 |
| `CONFIG_FDL_DEFAULT_UART` | `DRV_NAME_UART2` | 默认下载 UART |
| `CONFIG_NVBIN_FIXED_SIZE` | `0x20000` | 固定 NV bin 大小 |
| `BOOT_SIGN_SIZE` | `0xBD40` | secure boot 签名检查大小（boot 镜像）|
| `HDLC_FLAG` / `HDLC_ESCAPE` | `0x7E` / `0x7D` | 帧定界 / 转义字符 |
| `IH_MAGIC` | `0x27051956` | uImage 头魔数（FDL2/固件镜像头）|

---

## 应答码参考

| 应答 | 值 | 含义 |
|---|---|---|
| ACK | 0x80 | 通用成功 |
| VER | 0x81 | CHECK_BAUD 应答（带版本串）|
| INVALID_CMD | 0x82 | 无效指令 |
| UNKNOW_CMD | 0x83 | 未知指令 |
| OPERATION_FAILED | 0x84 | 操作失败 |
| NOT_SUPPORT_BAUDRATE | 0x85 | 波特率不支持 |
| DOWN_NOT_START | 0x86 | 未发 START_DATA |
| DOWN_DEST_ERROR | 0x89 | 目标地址错误/超出 Flash |
| DOWN_SIZE_ERROR | 0x8A | 下载大小超分区/超限 |
| VERIFY_ERROR | 0x8B | 数据校验错误 |
| PHONE_NOT_ENOUGH_MEMORY | 0x8D | 设备内存不足 |
| READ_FLASH | 0x93 | READ_FLASH 应答（带数据）|
| INCOMPATIBLE_PARTITION | 0x96 | 分区不匹配 |
| SEC_VERIFY_ERROR | 0xAA | 安全数据校验错误 |
| UNSUPPROT_COMMAND | 0xFE | 软件不支持该功能 |
| LOG | 0xFF | FDL 主动输出的 log 信息 |

---

## 固件镜像要求

### uImage 头

FDL1 的 `EXEC_DATA` 校验下载到 RAM 的镜像头（`image_header.h`，全字段大端）：

```c
typedef struct image_header {
    uint32_t ih_magic;   /* = 0x27051956 (IH_MAGIC) */
    uint32_t ih_hcrc;    /* 头 CRC */
    uint32_t ih_time;    /* 时间戳 */
    uint32_t ih_size;    /* 镜像数据大小 */
    uint32_t ih_load;    /* 数据加载地址 */
    uint32_t ih_ep;      /* 入口地址（EXEC 跳转目标）*/
    uint32_t ih_dcrc;    /* 数据 CRC */
    uint8_t  ih_os, ih_arch, ih_type, ih_comp;
    uint8_t  ih_name[32];
} image_header_t;
```

### 安全启动

- 安全启动通过 efuse 中安全使能位（3 位中 ≥2 位置位）判定。
- **默认（LuatOS/Air724UG）未使能**：签名校验直接通过，刷机无需签名镜像。
- 若使能：FDL1 的 `END_DATA` 会对 FDL2 做 uImage 签名校验（0xAA 失败）；FDL2 写 boot/app 镜像时做内嵌签名校验（镜像末尾 512B 为证书，排除后检查），失败回 `0x8B`。

---

## 刷机流程（luatos-cli 落地要点）

```
0. 解压 .soc (7z)，解析 PAC:
   ├─ 读取 info.json (chip.type="uis8910")
   └─ 按 PAC 格式解析 luatos.pac:
      ├─ HOST_FDL / FDL2 → 待下载到 RAM 的 FDL 镜像
      └─ BOOTLOADER/AP/PS/LUA + NV/操作项 → 按 dwAddress 排序的刷写队列
1. 打开串口 @ 115200, 8N1
2. 进入下载模式（硬件按键进入，枚举为 SPRD U2S Diag，VID 0525:PID a4a7）
3. 阶段一（PDL）: 下载 HOST_FDL —— 帧头与负载分开发送
   CONNECT(cmd=0) → START_DATA(cmd=4, addr=0x8000C0, "PDL1") → MID_DATA×N(2048/块)
   → END_DATA(cmd=6) → EXEC_DATA(cmd=7) → 发 0x7E 收版本
4. 阶段二（FDL 0x7E+CheckSum）: 下载 FDL2
   发 0x7E 收 BSL_REP_VER
   CONNECT → CHANGE_BAUD(4000000) → CONNECT → START_DATA(0x810000, fdl2_len)
   → MIDST_DATA×N(0x210/块, 需转义) → END_DATA → EXEC_DATA → NORMAL_RESET(0x05) 重启
5. 阶段三（FDL 0x7E+CheckSum）: 按 PAC 顺序刷写
   CONNECT
   对每个文件（dwAddress 分类处理）:
     ├─ 物理地址 (0x60000000+偏移): START_DATA(addr,size) → MIDST×N(≤12288B) → END_DATA
     ├─ 逻辑地址 (0xFE0000xx):      START_DATA / ERASE_FLASH 等操作
     └─ 操作项 (size=0):            直接 ERASE_FLASH / FMT_FLASH
   [可选] READ_FLASH 回读
6. 收尾: NORMAL_RESET (0x05) 重启
```

## CLI 使用与备用工具

### luatos-cli（目标形态）

实现完成后，按仓库通用模式调用：

```bash
# 全量刷机（自动解析 .soc → PAC → FDL1/FDL2 → 分区镜像）
luatos-cli flash run --soc LuatOS-SoC_V1009_Air724UG.soc --port /dev/ttyUSB0

# 仅刷脚本分区
luatos-cli flash script --soc LuatOS-SoC_V1009_Air724UG.soc --port /dev/ttyUSB0 --script ./scripts
```

> FDL 阶段波特率固定使用 **921600**（芯片仅支持 115200/921600），不采用 info.json 的 `force_br`(1152000)。

### 官方 CmdDloader.exe（备用/调试回退）

`Download_R27.26.2304` 提供官方下载框架的命令行封装 `CmdDloader.exe`（依赖 `Bin/App/` 下的 DLL），可用于调试后备：

```
CmdDloader.exe -pac luatos.pac -Port 5 -Baudrate 921600
```

| 参数 | 说明 |
|---|---|
| `-pac <path>` | PAC 文件路径 |
| `-path <path>` | 直接指定固件目录（非 PAC）|
| `-Port <n>` | COM 端口号 |
| `-Baudrate <n>` | 波特率（默认 115200）|
| `-Timeout <ms>` | 超时 |
| `-EZMode` | 简单模式 |
| `-Help` / `-Version` | 帮助 / 版本 |

> 其余参数见 `cmd_def.h`：`-SkipFileID`、`-WriteSN1/2`、`-ReadNV`、`-Reset`、`-PowerOff`、`-Policy`、`-WriteEfuse`、`-WriteTimeStamp`、`-SendFlag`、`-project`、`-regionid`、`-MutiFileID` 等。图形界面版本为 `ResearchDownload.exe` / `FactoryDownload.exe` / `UpgradeDownload.exe`。

---

### 重试与超时建议

| 操作 | 建议超时 | 备注 |
|---|---|---|
| CHECK_BAUD 收版本串 | 每次 10~60ms，重试 | 波特率对齐阶段 |
| CONNECT / 普通 ACK | 1~2s | — |
| MIDST_DATA | 随包长 | 数据量大时加长 |
| END_DATA | 10s+ | 芯片内部做 CRC 校验/擦写，耗时随镜像增大 |
| CHANGE_BAUD 收 ACK | 1s | 旧波特率下应答 |

> 固件镜像 `END_DATA` 因包含全量 CRC 回读，超时应放宽到 30s 以上。

---

## 注意事项与已知坑

1. **校验字节序**：FDL 阶段 CheckSum 必须按**小端**字累加、**小端**追加；ROM 阶段 CRC16 按**大端**追加。两者不同，勿混用。
2. **FDL1 START_DATA 地址**：必须是 `0x810000`，否则回 `0x89 DOWN_DEST_ERROR` 并 panic。
3. **FDL2 镜像头**：FDL2 必须以 `IH_MAGIC=0x27051956` 开头，EXEC 才跳转。
4. **波特率受限**：只支持 115200 / 921600；若从非 115200 进入（如 boot2 软件切换），需先对齐到 115200 再做 ROM 阶段。
5. **FDL2 启动即 ACK**：收到 ACK 即认为 FDL2 就绪。
6. **0x7E 同步时机**：FDL1 启动后等待一次 `0x7E` 才回版本串、进入指令循环；FDL2 启动后**直接回 ACK**，不需要 `0x7E`。各阶段进入条件不同，勿照搬通用流程。
7. **包长单位**：FDL 数据包长为字节数（如 `0x210`=528B、`0x840`=2112B、`0x3000`=12288B），非 KB。
8. **REPARTITION 为空操作**：RDA8910 的 FDL2 对 `0x0B` 直接回 ACK，分区表由 PAC/工程配置决定，无需真分区。
