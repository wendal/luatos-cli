# `flash raw`：任意 Flash 地址刷机

## 概述

`flash raw` 绕过 `info.json` 的分区定义，直接按 `--addr` 给出的地址擦写 Flash 镜像。

它面向的是**分区表覆盖不到**的场景，例如：

- 固件声明了外置资源地址，但 `.soc` 包里没有对应文件（Air1601 的 `include.txt` 声明了字库/TTS 地址却缺文件）
- 想单独替换某个外置资源（字库、TTS、fs 镜像）而不重刷整个固件
- 产线/实验室按固定地址烧字库、校准数据、资产编号
- 排障：把可疑分区单独重刷一遍，验证是否分区损坏

**当前支持 Air8101(BK7258) 与 Air1601/Air1602(CCM4211)。** 其他芯片会明确报错，不做静默降级。

> ⚠️ 这是低阶操作。写坏 bootloader/core 会导致模组变砖，且**无法用本 CLI 恢复**。

## 用法

```bash
# Air8101：往脚本分区（0x200000）刷一个 4K 对齐的二进制
luatos-cli flash raw \
  --soc Air8101.soc --port COM6 \
  --addr 0x200000 --file font.bin \
  --erase --verify

# Air1601：地址无需对齐
luatos-cli flash raw \
  --soc Air1602.soc --port COM7 \
  --addr 0x10040000 --file hzfont_builtin_ttf.bin \
  --erase --verify
```

| 参数 | 说明 |
|---|---|
| `--soc` | `.soc` 路径。用于确定芯片族、下载波特率与受保护区间 |
| `--port` | 串口号 |
| `--addr` | 目标起始地址，支持 `0x` 前缀或十进制 |
| `--file` | 待写入的二进制文件 |
| `--erase` | 写入前先擦除。NOR Flash 不能把 0 写回 1，**绝大多数情况必须开** |
| `--verify` | 写完回读校验（能力因芯片而异，见下表） |
| `--force` | 允许写入 bootloader/core 等受保护区间 |
| `--flash-size` | Flash 总容量，用于越界校验。不传则跳过该校验 |
| `--no-reboot` | 写完不复位。设备将停留在 ROM 下载模式（Air8101 表现为黑屏） |

## 芯片能力差异

这是使用前最需要知道的一点：**两颗芯片的写入与校验能力并不对等。**

| 维度 | Air8101 / BK7258 | Air1601 / CCM4211 |
|---|---|---|
| 写入地址对齐 | **必须 4K 对齐**，否则拒绝执行 | 无要求（真机已验证未对齐写入可用） |
| 擦除粒度 | 4K / 64K 自适应 | **实测 256KB，向下对齐整块擦**（见下） |
| 写后校验 | 只能拿到设备端 CRC32，**主机端无法复现该算法**，因此只能记录、不能判定对错 | `CHECK_CODE` 返回设备回读 MD5，**可真正判定**（真机已验证） |
| 读回 dump | ❌ 协议文档未覆盖读 flash 命令 | ❌ **实测 `GET_FLASH_DATA`(0x06) 无应答**，此路不通 |

CLI 会在 Air8101 上使用 `--verify` 时主动告警，避免用户把一个 CRC 数值误当成"校验通过"：

```
[WARN] BK7258 的 ISP 协议只能返回设备端 CRC32，主机端无法复现该算法，
       因此 --verify 只能记录数值，不能判定写入是否正确
```

JSON 输出里用 `verified` 字段区分：`true` 仅在真正比对成功时出现。

```json
{
  "status": "ok",
  "command": "flash.raw",
  "data": {
    "chip": "ccm4211",
    "addr": "0x14700000",
    "length": 4096,
    "erased": true,
    "verify": { "verified": true, "digest": "8f1d..." }
  }
}
```

## 安全护栏

以下检查全部在**连接硬件之前**完成，报错立即返回，不会占用串口：

1. **受保护区间**：写入范围与 `bootloader+core` 相交时拒绝，需 `--force`。
   区间由 `info.json` 推出 —— BK72xx 为 `[0, script_addr)`，CCM4211 为 `[bl_addr, script_addr)`。
2. **写入对齐**：BK7258 地址非 4K 对齐直接拒绝（会连带覆盖所在扇区其它数据）。
3. **擦除对齐**：CCM4211 的 `--erase` 地址必须 256KB 对齐（理由见下方实测结论）。
4. **容量越界**：设了 `--flash-size` 时，写入末端不得超出容量。
5. **空文件**：拒绝刷 0 字节。

另有两条运行期告警：未加 `--erase` 时提示 0→1 写入风险；`--no-reboot` 时提示设备会停在下载模式。

---

## 实测结论（Air1601 真机）

验证环境：CH343（VID `1a86` / PID `55d3`），COM23，`+HW: Air1601_A11`，
设备固件 `LuatOS_V1027_Air160X`，刷机包 `LuatOS-SoC_V1021_Air1601.soc`。
复现命令：

```bash
cargo test -p luatos-flash hw_probe -- --ignored --nocapture
cargo test -p luatos-flash hw_measure_erase -- --ignored --nocapture
```

两个探针以 `#[ignore]` 标记，CI 的 `cargo test --workspace` 不会执行，需显式 `--ignored` 且接真机。

### 1. `CHECK_CODE` 能真正回读校验 ✅

向 `0x14700000` 写入 4096 字节已知图案，设备回读 MD5 与主机侧
`md5(probe4k.bin)` **完全一致**（`8ec06cd4bf75cb84df3c6eca3045af7e`）。
所以 Air1601 的 `--verify` 是真校验，不是走过场。

### 2. `block_len = 262144`（256KB）

由 `GET_DOWNLOAD_INFO`(0x09) 返回，与协议文档一致。

### 3. ⚠️ 擦除会向下对齐整块擦掉 —— 最需要注意的一条

对 `0x14700000` 发一次 `FLASH_ERASE_BLOCK`，实测 `[0x14700000, 0x14740000)` 全部变成 `0xFF`，
块外 `0x14740000` 不受影响。**擦除范围正好是一个 256KB 块。**

更关键的是：**对块内非对齐地址发擦除命令，同样会向下取整整块擦掉。**
实测对 `0x14701000` 发擦除，`0x14700000`（用户并未请求）也被清成 `0xFF`。

实际影响：

```bash
# 只想刷 4KB，但会连带清掉下方 252KB
luatos-cli flash raw --addr 0x14701000 --file small.bin --erase
#   ^^^^^^^^^^^ 用户请求擦 4KB
#   实际擦掉 0x14700000..0x14740000（256KB），其中 0x14700000..0x14701000 属于意外损失
```

因此 CLI 在**连硬件之前**就会拒绝未对齐的擦除地址，并在报错中明确指出会被连带擦除的区间：

```
Error: ccm4211 擦除地址必须按 262144 字节（0x40000）对齐，当前 0x14701000。
实测该协议的擦除命令会**向下对齐到 0x14700000 并整块擦除**（约 262144 字节），
也就是说你没请求的 0x14700000..0x14701000 也会被清掉。
请把 --addr 对齐到 0x40000 边界，或改用不擦除的写法。
```

对齐擦除时同样有告警，提醒实际擦除量远大于写入量。

### 4. `GET_FLASH_DATA`(0x06) 不可用 ❌

分别以 `len=64` / `len=256` 探测，3 秒超时**均无应答**。
所以 Air1601 目前**没有可用的 flash 读回通道**，`flash raw dump` 在该芯片上无法实现。

### 5. Air8101 侧仍未验证

BK7258 的四项（4K 对齐、4K/64K 擦除、CRC-only 校验、是否存在读 flash 命令）
都需要 Air8101 真机才能确认。本次没有对应硬件，相关描述仍为文档推断。
特别是"是否存在读 flash 命令"这一点，需要逆向 `BK7231GUIFlashTool.exe` 才能定论。

## 恢复路径

## 架构

`luatos-flash` 新增 `raw` 模块，它**不实现任何协议**，只是抽象层：

```rust
pub trait RawFlashDevice {
    fn chip(&self) -> RawChip;
    fn erase_granularity(&self) -> u64;
    fn write_alignment(&self) -> u64;
    fn progress(&self) -> &ProgressCallback;
    fn write(&mut self, addr: u32, data: &[u8]) -> Result<()>;
    fn erase(&mut self, addr: u32, len: u64) -> Result<()>;
    fn verify(&mut self, addr: u32, data: &[u8]) -> Result<VerifyResult>;
    fn reset(&mut self);
}
```

职责划分：

| 层 | 文件 | 负责 |
|---|---|---|
| 抽象与驱动 | `luatos-flash/src/raw.rs` | trait、护栏校验、擦→写→校验→复位的公共流程 |
| 协议实现 | `bk7258.rs` / `ccm4211.rs` | 只负责发协议帧，复用各自已有的原语 |
| CLI | `luatos-cli/src/cmd_flash.rs` | 解析参数、从 `info.json` 推导受保护区间 |

因为护栏集中在 `raw.rs`，新增芯片时只需实现一份 trait，CLI 不用改。

### 校验能力的表达

刻意用 `VerifyResult` 区分"拿到一个数"和"数据写对了"：

```rust
pub enum VerifyResult {
    DigestMatched { digest: String },   // 设备回读比对一致 —— 真的写对了
    DeviceCrcOnly { crc32: u32 },       // 设备算的 CRC，主机端无法预测 —— 仅记录
}
```

`DeviceCrcOnly.is_verified()` 返回 `false`，不会被误判为通过。

## 恢复路径

`flash raw` 写坏之后：

- **只擦了数据区**（script / fs / fskv / 外置资源）→ 用 `flash script` / `flash flash-fs` / `flash clear-fs` 重刷对应分区即可
- **写了 bootloader / core** → 需要外部工具（`BK7231GUIFlashTool` / `dtools`）重新烧全量固件

这条边界正是默认保护 bootloader/core 的原因。