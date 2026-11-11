# 刷机链路零临时文件设计

## 目标

`flash run`、`flash script`、`flash test`、`flash flash-fs` 与 `trun` 在默认运行期间不再创建
“每次运行生成、运行结束即删除”的临时文件。所有脚本镜像在内存中构建，并以 `Vec<u8>` /
`&[u8]` 直接进入刷机协议。

不在本设计范围内：

- FOTA 打包（`fota build`）仍依赖外部工具与其临时目录，它是升级包生成，不是设备刷机；
- `soc combine` / `soc pack` / `soc unpack` 的离线归档临时目录；
- HZV 转换的临时目录。

## 数据流

```text
脚本目录 [+ ctx.json 内存条目]
        │
        ▼
按芯片族选择构建器：
  BK7258    basename 合并 + 可选 BK CRC
  XT804     basename 扁平 LuaDB（不加 XT804 头）
  CCM/EC/RDA basename 合并
        │
        ▼
Vec<u8> script_image
        │
        ├─ 全量刷机：script_overlay 传入 BSP
        └─ 仅脚本：script_data 传入 BSP
        │
        ▼
串口协议（BK ISP / XT804 XMODEM / CCM SOC / EC AgentBoot / RDA FDL）
```

## 统一入口

### `luatos-luadb`

- `build::build_script_image`：保留原签名；`use_luac=true` 时逐文件在内存编译，不再创建
  临时目录。
- `build::compile_lua_bytes`：直接调用静态链接的 32/64 位 Lua C 核心，不释放 helper、不启动子进程。
- `build::build_script_image_with_entries`：目录产物之后合入内存条目（ctx.json），同名覆盖。
- `build::build_littlefs_image_from_dirs`：直接调用静态链接的 mklfs/LittleFS C 构建器，
  后目录覆盖同名文件；不释放 helper、不写缓存、不启动子进程；单目录 `build_littlefs_image` 保留为包装。

### `luatos-flash`

新模块 `luatos_flash::script` 是 CLI/trun 的唯一脚本构建入口：

- `script_write_alignment(family)`：BK=4096、XT804=1024、其他=1；
- `validate_script_image_for_family(family, info, len)`：开串口前的容量校验；
- `build_script_image(family, dirs, info, luac_debug, extra_entries, force_bk_crc)`：
  按芯片族分派到 BK / XT804 / 通用构建器。`force_bk_crc` 仅用于保留 trun 对 BK 的历史行为，
  CLI 普通刷机以 SOC 元数据为准。

BSP 字节入口：

- `bk7258::build_script_image` / `bk7258::flash_bk7258_with_overlay` / `bk7258::flash_script_data`
- `xt804::build_script_image_from_dirs` / `xt804::flash_script_data`
- CCM4211 / EC718 / RDA8910 复用现有 `script_overlay` 与 `flash_script_*`

### `luatos-cli`

- `cmd_flash_run_with_script`：全量刷机核心，只接收预构建好的字节 overlay；
  XT804 在此内部执行“刷 ROM → 刷脚本分区”两阶段。
- `cmd_flash_script_data`：仅脚本刷机核心，统一分发五族；`flash script --bin` 也走它。
- `trun`：构建阶段生成最终 script.bin（含 ctx.json），刷机阶段只传字节；
  仅 `--keep-soc` 时把 script.bin 写到用户目录。

## 各 BSP 行为

| 芯片族 | 全量刷机脚本注入 | 仅脚本刷机 | 临时文件 |
|---|---|---|---|
| BK7258 | `flash_bk7258_with_overlay`，最后写 script 分区 | `flash_script_data` | 无 |
| XT804 | `flash_xt804` 后 `flash_script_data`（两阶段） | `flash_script_data` | 无 |
| CCM4211 | `flash_ccm4211(..., Some(bytes))` | `flash_script_ccm4211` | 无 |
| EC718/EC618/EC716S | `flash_ec718(..., Some(bytes))` 覆盖/新增 binpkg script entry | `flash_script_ec718` | 无 |
| RDA8910 | `flash_rda8910(..., Some(bytes))` 内存替换 PAC LUA | `flash_script_rda8910` | 无 |
| Air6201 | 不支持 SOC 全量 | 直接接收文件字节 | 无 |

## 校验顺序

1. 构建脚本镜像；
2. `validate_script_image_for_family` 校验容量与写入对齐；
3. 打开串口；
4. 刷机。

因此脚本超限、编译失败或目录不存在都不会占用串口，也不会在 XT804 上先擦固件后才发现脚本超限。

## 验证

```bash
cargo fmt --all -- --check
cargo clippy --workspace -- -D warnings
cargo test --workspace
cargo build --release -p luatos-cli
```

零临时文件验证：

1. 把 `TEMP` / `TMP` / `TMPDIR` 指向一个空 watch 目录；如需对照旧版本，再把旧版本专用的
   `LUATOS_CLI_HELPER_CACHE` 指向普通文件，阻断其显式 helper 缓存路径；
2. 执行 `luatos-cli build filesystem ... --luac`（离线）或带硬件执行
   `flash script` / `flash run --script` / `flash flash-fs` / `trun`；
3. luac 与 mklfs 路径均应成功且 watch 目录为空，不得创建 helper 或持久缓存。

硬件验收至少覆盖 Air8101、Air1601、Air8000、Air724UG、Air6208；`trun` 后确认设备能读到
`/ctx.json` 的 `test_id`，并对照改造前后同一脚本目录的 script.bin 哈希。
