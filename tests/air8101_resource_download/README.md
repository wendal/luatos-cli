# Air8101B 资源目录下载闭环测试

## 资源分区格式(本轮 romfs)

资源分区不再是原始 Luadb 镜像, 而是**标准 romfs(`-rom1fs-`, genromfs 兼容)镜像,
按 BK CRC 每 32 字节数据 + 2 字节 CRC 帧化**; 设备端经 CPU flash 窗口指针直读
(`components/romfs/luat_vfs_romfs.c`, `luat_fs_mmap` 可零拷贝)。CLI 侧要求
`rom.fs.res`:

```json
"res": {"offset": "0x6D7000", "size": 1088, "type": "romfs", "bkcrc": true}
```

`type` 必须是 `"romfs"`(旧的 `"luadb"` 直接报错); `bkcrc: true` = 帧化,
`false` = 裸 romfs; `size` 单位 KiB, 地址与容量需 4KiB 对齐; 可选
`download.res_addr` 必须与 offset 一致。帧化后每 34 字节只承载 32 字节数据,
约 5.9% 的分区空间用于 CRC 帧。

本轮实机布局(临时改 SDK 的 `auto_partitions_102.csv`):

| 分区 | 地址 | 容量 | 净载荷 |
| --- | --- | --- | --- |
| luat_lfs | 0x5D2000 | 1024KiB | LFS 可写, 存放 `/cli_res_sentinel` |
| luat_res | 0x6D7000 | 1088KiB = 1114112 字节 | 1048576 字节(1MiB = 1114112/34*32) |

romfs 约束:

- 单级文件名 ≤ **64 字节**(UTF-8 计, 超限 CLI 直接报错, 不落盘);
- 目录最多 **4 层**: `a/b/c/d/keep.bin` 保留, 第 5 层及更深
  (`a/b/c/d/e/x.bin`)由 CLI 打印 `Skipped (目录深度超过 4 层): …` 告警并跳过,
  镜像里连它的目录都不会出现;
- 文件数上限(旧 Luadb 的 1–1024)已取消, 只受容量约束。

## 重复执行

需要 16MB Flash Air8101B、带 `rom.fs.res`(**`type: "romfs"`** + `bkcrc`)
的固件和 CLI。使用小于 300000 字节的真实 TTF/OTF 作为外部输入; 不提交字体或
测试镜像。例如 Linux 上可使用 NotoMono-Regular.ttf。

```sh
cargo test -p luatos-soc -p luatos-luadb -p luatos-flash -p luatos-cli
cargo build -p luatos-cli
python3 tests/air8101_resource_download/generate.py \
  --output /tmp/cli-res-hardware \
  --font /usr/share/fonts/truetype/noto/NotoMono-Regular.ttf
luatos-cli soc info firmware108.soc
# 期望: Res Addr:   0x006D7000  (1088 KB, romfs, bkcrc=true, payload 1024 KB)
luatos-cli flash script --soc firmware108.soc --port COM6 \
  --script /tmp/cli-res-hardware/script --script ../LuatOS/script/corelib
luatos-cli log view-binary --port COM6 --baud 2000000 --rts-reset
```

先确认 `CLI_RES_SETUP_PASS`，然后关闭日志窗口。初次准备时资源分区不应
已有 `profile.txt` 测试文件；若已有，先用任意不含此文件的目录执行资源下载。
验收脚本准备 LFS 哨兵 `/cli_res_sentinel`，之后仅核对其内容，不自动重建。

```sh
luatos-cli --format jsonl flash flash-res --soc firmware108.soc --port COM6 \
  --resource /tmp/cli-res-hardware/full
luatos-cli log view-binary --port COM6 --baud 2000000 --rts-reset
# 取得 CLI_RES_FULL_PASS，关闭窗口，独立复位后再次取得同一 PASS。
luatos-cli log view-binary --port COM6 --baud 2000000 --rts-reset
# 关闭日志窗口, 再下载深度用例(小镜像): CLI 必须打印
# "Skipped (目录深度超过 4 层): a/b/c/d/e/x.bin"
luatos-cli flash flash-res --soc firmware108.soc --port COM6 \
  --resource /tmp/cli-res-hardware/deep
luatos-cli log view-binary --port COM6 --baud 2000000 --rts-reset
# 取得 CLI_RES_DEEP_PASS(第 4 层 keep.bin 存在, 第 5 层 x.bin 及其目录不存在)。
# 关闭日志窗口，再换为完全不含 TTS/字体的资源目录。
luatos-cli flash flash-res --soc firmware108.soc --port COM6 \
  --resource /tmp/cli-res-hardware/generic
luatos-cli log view-binary --port COM6 --baud 2000000 --rts-reset
# 取得 CLI_RES_GENERIC_PASS 后关闭日志窗口。
luatos-cli flash flash-res --soc firmware108.soc --port COM6 \
  --resource /tmp/cli-res-hardware/oversized
luatos-cli flash flash-res --soc firmware106.soc --port COM6 \
  --resource /tmp/cli-res-hardware/generic
# 上面两条必须返回失败，分别说明超容量和缺少资源描述，且不进入 Connecting。
luatos-cli log view-binary --port COM6 --baud 2000000 --rts-reset
# 再次取得 CLI_RES_GENERIC_PASS，确认失败命令未破坏有效资源。
```

`full` 与 `generic` 的干净 romfs 镜像都**正好 1048576 字节**(帧化后 1114112
字节 = 分区容量): `generate.py` 按 romfs 开销公式(头 16 + 卷名 16 + 根目录 32 +
每文件 16 + `align16(名+1)` + `align16(数据)` + 每中间目录 16 + `align16(名+1)`,
与 `crates/luatos-luadb/src/romfs.rs` 一致)反推 `asset.dat` 长度,
字体 107848 字节时 `full` 的 `asset.dat` 为 857184(随字体大小变化),
`generic` 不含字体、恒为 965264。脚本运行时会打印每个目录的文件清单、
干净镜像长度与帧化长度, 并自证两者分别等于 1048576 与 1114112。
脚本对大文件逐块比较完整内容，对字体与 `/luadb/reference.ttf` 逐块精确比较；
验证定位、EOF、目录、romfs 统计(`kind == "romfs"`、`used == total`、
`total*block` = 镜像头部声明长度按 512 字节向上取整)、脚本交错读取、写保护
以及 LFS 哨兵。`full` 还覆盖 64 字节边界名、嵌套路径、保留原字节及扩展名的
非法 Lua 文本; `generic` 验证旧字体、旧 Lua、旧子目录文件和 64 字节名不可见;
`deep` 验证第 4 层文件保留、第 5 层文件被跳过。

## 脚本未断言项

- **mmap 零拷贝**: 设备端由 `components/romfs/luat_vfs_romfs.c` 的
  `luat_fs_mmap` 提供线性指针, Lua 侧接口取决于固件(本轮固件与
  `crates/luatos-cli` 都没有 `io.mmap` 之类的绑定), 因此验收脚本不凭空造 API,
  只用 `io.open/seek/read` 逐块校验内容。指针直读的实际验证方式是内容与
  `/luadb/reference.ttf` 逐块一致 + 无内存占用异常。
- **跳过告警的归属**: "Skipped (目录深度超过 4 层)" 由 CLI 进度输出打印,
  设备侧只能验证被跳过文件(及其目录)不存在; 两个方向都要看。
- **deep 用例的 fsstat 容量**: 按核心 `components/romfs/luat_vfs_romfs.c` 的
  `romfs_info`, 「容量」是镜像头部声明的长度按 512 字节向上取整, 因此
  288 字节的 deep 镜像预期 `total*block == 512`(full/generic 正好 1MiB)。
  若实机固件改为上报分区净载荷(恒定 1MiB), 只需同步改 `main.lua` 里
  `check_res_stat` 的实参。

## 本轮(romfs 格式)实机结果: 待补

主 agent 在真机跑完 full / deep / generic / 两条负例后, 在本节补结果表与
关键日志摘录, 并把日志摘录**追加**到 [evidence.txt](evidence.txt)
(历史记录不要改写)。本节保留为空表示 romfs 一轮尚未实机验证。

## 2026-10-08 实机结果(旧 Luadb 格式, 已被本轮 romfs 取代)

板子为 Air8101B A12，COM6；使用正式 `LuatOS-SoC_V2021_Air8101_108.soc`
和本次构建的 Windows CLI 1.12.0。测试中没有修改正式 SOC 元数据。
当时资源分区还是**原始 Luadb 镜像**(`type: "luadb"`, `bkcrc: false`),
容量 3MiB; 该布局与格式已不再生效, 仅作历史记录:

| 检查 | 结果 |
| --- | --- |
| full 下载、Resource JSON 进度和最终 result | 成功 |
| 完整资源校验及 LFS 哨兵 | CLI_RES_FULL_PASS |
| 独立复位、字体 107848 字节精确比对 | CLI_RES_FULL_PASS |
| generic 目录替换、旧文件不可见 | CLI_RES_GENERIC_PASS |
| 超容量镜像、旧106 SOC、不支持芯片 | 均退出1，连接串口前拒绝 |
| 拒绝上述下载后再次复位校验 | CLI_RES_GENERIC_PASS |

资源容量 3MiB，LFS 1MiB，脚本 512KiB。full 中 `asset.dat` 为 2954534
字节，generic 中为 3062535 字节，`palette.bin` 为 80000 字节。
字体输入为 NotoMono-Regular.ttf；所有二进制资源只存在于临时目录。
板上最终保留 generic 镜像、验收脚本、字体比对参考文件和 LFS 哨兵。
关键日志摘录见 [evidence.txt](evidence.txt)，本次完整日志在
`/tmp/cli-res-hardware`，Windows 测试 CLI 在
`/tmp/luatos-cli-hardware-target/debug/luatos-cli.exe`。

相关 Rust 回归共 204 项通过，6 项原有测试忽略。两项 EC7xx FOTA 测试
因 `refs/soc_files/LuatOS-SoC_V2029_Air780EPM_1.soc` 只有 Git LFS 指针
而失败；确认原因后使用以下命令跳过这两项，其他测试全部通过：

```sh
cargo test -p luatos-soc -p luatos-luadb -p luatos-flash -p luatos-cli -- \
  --skip ec7xx_script_only_builds_without_old_soc \
  --skip ec7xx_script_only_rejects_missing_script_bin
```
