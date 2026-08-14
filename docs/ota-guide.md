# OTA 固件打包功能指导文档

## 概述

OTA（Over-The-Air，空中升级）是 LuatOS 模组固件远程升级的核心机制。本工具提供的 `fota build` 命令用于将 `.soc` 固件包转换成模组可识别的升级包（如 `.sota` / BK72XX 新格式 `.bin`）。

该功能已用纯 Rust 重新实现，完全替代了原 `soc_tools.exe` 中的 `zip_file` 和 `make_ota_file` 命令，产物与原 C++ 工具二进制兼容。

---

## 架构

```
┌───────────────────────────────────────────────────────────┐
│  CLI 层: luatos-cli/src/cmd_fota.rs                       │
│  ├── EC7xx / Air8000 → 差分FOTA (FotaToolkit + Rust)      │
│  ├── Air1601 / CCM4211 → 全量FOTA (纯Rust) / --script-only│
│  ├── Air8101 / BK72XX → 新格式全量/脚本FOTA (纯Rust)       │
│  └── Air6208 / XT804  → 全量FOTA (air101_flash)           │
├───────────────────────────────────────────────────────────┤
│  库层: luatos-soc/src/ota.rs                              │
│  ├── lzma_compress_file()   → LZMA分块压缩                │
│  └── assemble_ota_package() → OTA包组装                   │
└───────────────────────────────────────────────────────────┘
```

---

## CLI 使用方法

### 基础语法

```bash
luatos-cli fota build --new <新固件.soc> [--old <旧固件.soc>] [--output <输出路径>] [--script-only]
```

### 参数说明

| 参数 | 必需 | 说明 |
|------|------|------|
| `--new` | 是 | 新固件 `.soc` 文件路径 |
| `--old` | 否 | 旧固件 `.soc` 文件路径（提供时为差分FOTA，否则为全量FOTA） |
| `-o` `--output` | 否 | 输出路径，默认按芯片生成（如 `<芯片名>_fota.sota` / `bk72xx_fota.bin`） |
| `--fota-toolkit` | 否 | `FotaToolkit.exe` 路径（仅差分FOTA需要，默认自动搜索） |
| `--script-only` | 否 | 生成仅脚本的升级包（EC7xx/Air8000、CCM4211、BK72XX 支持） |
| `--force-par` | 否 | EC7xx/Air8000 差分 FOTA：关闭自动回落，强制调用 FotaToolkit 生成差分包 |

### 使用示例

```bash
# EC7xx 差分 FOTA（提供新旧两版固件）
luatos-cli fota build --new v2.0.soc --old v1.0.soc

# EC7xx / Air8000 仅脚本 FOTA（不需要 --old）
luatos-cli fota build --new air780epm_v2.0.soc --script-only

# EC7xx 差分 FOTA：底层固件相同时自动回落到脚本更新包
luatos-cli fota build --new v2.0.soc --old v1.0.soc
# 命中回落时输出: WARN old/new binpkg 底层固件相同...自动回落到 --script-only 路径

# EC7xx 强制走差分 FOTA（关闭自动回落）
luatos-cli fota build --new v2.0.soc --old v1.0.soc --force-par

# Air1601 全量 FOTA（仅提供新版固件）
luatos-cli fota build --new air1601_v1.0.soc

# CCM4211 仅脚本热更新（跳过 ROM 压缩，产物体积小）
luatos-cli fota build --new ccm4211_v1.0.soc --script-only

# Air8101(BK72XX) 新格式全量 FOTA
luatos-cli fota build --new air8101_v1.0.soc -o air8101_full_fota.bin

# Air8101(BK72XX) 新格式脚本 FOTA
luatos-cli fota build --new air8101_v1.0.soc --script-only -o air8101_script_fota.bin

# 指定输出文件名
luatos-cli fota build --new firmware.soc --output upgrade.sota
```

### EC7xx 差分 FOTA 自动回落

EC7xx / Air8000 / Air780E 差分 FOTA 默认会检测 old/new `.soc` 内部 `binpkg` 的**底层固件**是否相同：

- 解析两个 binpkg 的 entry 列表
- 剔除 `name == "script"` 的 entry（脚本分区不属于"底层固件"）
- 比较其余 entry 的 `(name, addr, flash_size, img_size)` 元数据集合
- 集合完全一致 → 视为"底层固件相同"，**自动跳过 FotaToolkit**，改走纯 Rust 的脚本更新包路径（`--script-only` 同样的产物）
- 不一致 → 正常调用 FotaToolkit 生成 `delta.par`

适用场景：只改了 Lua 脚本但 ROM 底层未变的"热更新"打包，省掉 FotaToolkit 外部依赖与启动开销。

命中回落时输出日志（示例）：

```
WARN old/new binpkg 底层固件相同(剔除 script entry 后元数据完全一致),
     自动回落到 --script-only 路径。如需强制走差分 FOTA,请加 --force-par。
```

边界说明：

- **luadb-in-AP 形态**（`info.json` 中 `rom.fs.script.offset == 0`、脚本嵌入 `ap` entry）：binpkg 中没有独立的 `script` entry，整个 `ap` entry 都参与比较；改脚本必改 `ap.img_size` → 自然识别为"底层不同"→ 走 FotaToolkit 差分，行为正确
- **检测失败**（binpkg 不可读、单边无 entry 等）一律 `bail!` 报错，不静默走错路径

如需关闭自动回落强制走差分，加 `--force-par`：

```bash
luatos-cli fota build --new v2.0.soc --old v1.0.soc --force-par
```

---

## 支持的芯片族与 FOTA 模式

| 芯片族 | FOTA模式 | 核心流程 |
|--------|----------|----------|
| **EC7xx / EC618 / Air8000** | 差分FOTA / 仅脚本升级 | 差分：`FotaToolkit.exe` 生成差分包；脚本：直接压缩脚本分区；两者均由 Rust 组装 `.sota` |
| **Air1601 / CCM4211** | 全量FOTA | 纯Rust：LZMA 压缩固件块 → 组装 `.sota` |
| | **仅脚本升级** | `--script-only`：仅压缩脚本分区，跳过 ROM，适合 Lua 热更新 |
| **Air8101 / BK72XX** | 新格式全量FOTA | 纯Rust：CP/AP/Script 合成 RBL 载荷 → 固定 gzip 头 + AES256-CBC → 输出 `.bin` |
| | 新格式脚本升级 | `--script-only`：1KB 头 + BK CRC16 脚本块（仅新格式） |
| **Air6208 / XT804** | 全量FOTA | `air101_flash.exe` 生成镜像 → 剥离 secboot 头 → 输出 |

---

## 输出格式

### OTA 文件头部 (92 字节)

```
Offset  Size  Field
─────────────────────────────────────
 0       4    magic           — 魔数标识
 4       4    crc32           — 头部校验（覆盖 Offset 8 以后所有字节）
 8      20    main_version    — 5个u32，[0..3]=MD5前缀 [4]=版本号
28       4    app_version     — 应用版本号
32      20    std_version     — 5个u32，[0..3]=MD5前缀 [4]=版本号
52       4    common_data_len — 公共数据长度
56       4    sdk_data_len    — SDK数据长度
60      16    common_md5      — 公共数据MD5
76      16    sdk_md5         — SDK数据MD5
```

### LZMA 块格式

每个固件块按以下格式压缩存储（用于 Air1601/CCM4211）：

```
[1B props_len] [5B lzma_props] [4B LE compr_size] [data]
```

- `props_len`：LZMA 属性长度，0 表示未压缩块，5 表示压缩块
- `lzma_props`：`[lc + lp*9 + pb*45, dict_size_le32]`
- `compr_size`：压缩后数据长度（小端序）
- `data`：压缩后负载

压缩参数：`lc=3, lp=1(有MD5)或0(无MD5), pb=2, level=9`

---

## 内部函数参考

### `luatos_soc::ota::lzma_compress_file()`

对标 `soc_tools zip_file`，对单个二进制文件做分块 LZMA 压缩。

```rust
pub fn lzma_compress_file(
    input_path: &Path,       // 输入文件路径
    output_path: &Path,      // 输出文件路径
    magic: u32,              // 魔数
    start_addr: u32,         // 起始地址
    max_block_len: u32,      // 最大块大小（通常 256KB）
    use_md5: bool,           // 是否在头部包含 MD5 校验
) -> Result<()>
```

### `luatos_soc::ota::assemble_ota_package()`

对标 `soc_tools make_ota_file`，组装多分区 OTA 包。

```rust
pub fn assemble_ota_package(
    magic: u32,                // 魔数标识
    main_version_decimal: u32, // MainVersion[4] 版本号
    main_md5_prefix: &str,     // MainVersion[0..3] MD5前缀（hex字符串）
    std_version_decimal: u32,  // STDVersion[4] 版本号
    std_md5_prefix: &str,      // STDVersion[0..3] MD5前缀（hex字符串）
    app_version: u32,          // 应用版本号
    common_path: &Path,        // 公共分区数据路径
    sdk_path: &Path,           // SDK分区数据路径
    output_path: &Path,        // 输出 .sota 路径
) -> Result<()>
```

---

## 文件格式示意

**`.sota` 文件结构**（全量 FOTA）：

```
┌──────────────────────────────┐
│  OTA 文件头部 (92 bytes)     │
│  ─ magic, crc32              │
│  ─ main_version, std_version │
│  ─ app_version               │
│  ─ common_data_len           │
│  ─ sdk_data_len              │
│  ─ common_md5, sdk_md5       │
├──────────────────────────────┤
│  Common 分区数据              │
│  (用户脚本/资源)              │
├──────────────────────────────┤
│  SDK 分区数据                 │
│  (固件差分包 delta.par)       │
└──────────────────────────────┘
```

**`.zip` 文件结构**（Air1601/CCM4211 压缩固件）：

```
┌──────────────────────────────┐
│  SectorMd5Header / SectorHeader (20或36 bytes) │
│  ─ magic, total_len, data_len                  │
│  ─ md5(16B) 或 crc32(4B)                       │
│  ─ block_len, start_address                     │
├──────────────────────────────┤
│  Block 0: [1B len] [5B props] [4B size] [data] │
│  Block 1: [1B len] [5B props] [4B size] [data] │
│  ...                                            │
│  Block N:                                      │
└──────────────────────────────────────────────┘
```

---

## 常见问题

### Q: 什么时候用 `--old` 参数？

**差分 FOTA**（提供 `--old`）：新旧版本差异较小时使用，产物体积小，适合 EC7xx/EC618/Air8000 芯片。

**全量 FOTA**（不提供 `--old`）：Air1601/CCM4211 会压缩 ROM+脚本打包；Air6208/XT804 通过 air101_flash 生成镜像。

**仅脚本 FOTA**（`--script-only`）：EC7xx/Air8000 与 Air1601/CCM4211 跳过 ROM，只压缩脚本分区；BK72XX 生成新格式脚本升级包。

### Q: 如何验证生成的 OTA 包？

产物与原始 C++ 工具 `soc_tools.exe` 的产物二进制兼容。可通过比对 CRC32 和 MD5 校验值验证一致性。

### Q: `--script-only` 适用于什么场景？

当只修改了 Lua 脚本而无需更新固件时使用。相比全量 FOTA：

- ✅ 产物体积小（仅脚本分区，通常几十KB）
- ✅ 升级速度快
- ✅ 不涉及 ROM 压缩，构建更快

**限制**：

- BK72XX 仅支持 `rom.fs.script.bkcrc=true` 的新格式（不支持旧 `LFTA` 格式）
- BK72XX `.soc` 需包含 `cp.bin`、`ap.bin`、`script.bin` 以及有效 `rom.fs.ap.offset`/`download.script_addr`
- EC7xx/Air8000 的脚本-only 产物仍为 `.sota`（与差分包后缀一致）

### Q: 运行单元测试

```bash
cargo test -p luatos-soc
```

测试覆盖：CRC32 计算、MD5 前缀解码、结构体大小校验。

---

## 源码参考

| 文件 | 说明 |
|------|------|
| [crates/luatos-cli/src/cmd_fota.rs](../crates/luatos-cli/src/cmd_fota.rs) | CLI 命令层：`cmd_fota_build()` 入口，工具自动发现，芯片分派逻辑 |
| [crates/luatos-soc/src/ota.rs](../crates/luatos-soc/src/ota.rs) | OTA 库层：`lzma_compress_file()`、`assemble_ota_package()` 及 LZMA 块压缩核心 |
| [docs/air8101-fota-format.md](./air8101-fota-format.md) | Air8101/BK72XX 新格式 FOTA 结构与字段定义 |
| [crates/luatos-soc/Cargo.toml](../crates/luatos-soc/Cargo.toml) | 依赖声明：`crc32fast`、`flate2`、`aes/cbc`、`md-5` |

### 代码中的关键常量

| 常量 | 值 | 说明 |
|------|-----|------|
| `LZMA_PROPS_SIZE` | 5 | LZMA 属性头部大小 |
| `DEFAULT_BLOCK_LEN` | `0x10000`（64KB） | 默认压缩块大小 |
| `CRC32_SEED` | `0xFFFFFFFF` | CRC32 初始值 |
| 压缩 level | 9 | LZMA 最高压缩等级 |
| 压缩块大小（CCM4211） | `0x40000`（256KB） | CCM4211 全量 FOTA 块大小 |

### 构建

```bash
# debug 构建
cargo build

# release 构建（带 LTO 优化）
cargo build --release

# 单独测试 OTA 模块
cargo test -p luatos-soc
```
