# Air8101B 资源目录下载闭环测试

## 重复执行

需要 16MB Flash Air8101B、带 `rom.fs.res` 的 108 固件和 CLI。
使用小于 300000 字节的真实 TTF/OTF 作为外部输入；不提交字体或测试镜像。
例如 Linux 上可使用 NotoMono-Regular.ttf。

```sh
cargo test -p luatos-soc -p luatos-luadb -p luatos-flash -p luatos-cli
cargo build -p luatos-cli
python3 tests/air8101_resource_download/generate.py \
  --output /tmp/cli-res-hardware \
  --font /usr/share/fonts/truetype/noto/NotoMono-Regular.ttf
luatos-cli soc info firmware108.soc
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

`full` 和 `generic` 两套镜像都恰好为 3145728 字节。脚本对大文件逐块
比较完整内容，对字体与 `/luadb/reference.ttf` 逐块精确比较；验证定位、
EOF、目录、独立统计、脚本交错读取、写保护以及 LFS 哨兵。`full` 还覆盖
31 字节名称、嵌套路径、保留原字节及扩展名的非法 Lua 文本。`generic`
验证旧字体、旧 Lua、旧子目录文件和 31 字节名称不可见。

## 2026-10-08 实机结果

板子为 Air8101B A12，COM6；使用正式 `LuatOS-SoC_V2021_Air8101_108.soc`
和本次构建的 Windows CLI 1.12.0。测试中没有修改正式 SOC 元数据。

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
