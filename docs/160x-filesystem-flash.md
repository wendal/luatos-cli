# 160X 文件系统与 FSKV 组合烧录

`flash script` 和 `flash run` 支持以下显式策略，当前限定 Air1601、Air1602、Air1603：

| 参数 | 行为 |
| --- | --- |
| `--fs-mode preserve` | 不改 FS，并跳过 SOC 内置 fs.bin |
| `--fs-mode replace --fs-dir <目录>` | 将目录按原始内容及相对路径制作 LittleFS 镜像，整区替换 FS |
| `--fs-mode clear` | 清空整个 FS，不再写回 SOC 内置 fs.bin |
| `--clear-kv` | 独立清空整个 FSKV，可与上述任一策略组合 |
| `--dry-run` | 完成本地构建与分区预检，不打开串口、不执行擦写或尾随日志采集 |

未提供这些参数的调用保留原有 CLI 行为。`--luac-debug` 继续作用于脚本；FS 文件不经过 Lua 编译，也不采用脚本 LuaDB 的文件名扁平化规则。

```text
luatos-cli --format jsonl flash script --soc firmware.soc --port COM7 --script scripts --fs-mode replace --fs-dir data --clear-kv
luatos-cli --format jsonl flash run --soc firmware.soc --port COM7 --fs-mode clear --clear-kv
luatos-cli --format jsonl flash run --soc firmware.soc --port COM7 --fs-mode replace --fs-dir data --dry-run
```

构建/容量/布局检查在写入设备之前完成，SOC 资源保持内存读取，下载保留现有 LZMA 和设备 MD5 校验。分区操作与脚本在同一次下载会话完成，最后统一复位。取消或失败不会恢复已清除或写入的数据。

`rom.fs.filesystem` / `rom.fs.kv` 必须提供完整地址和长度，并与 `download.fs_addr` / `download.nvm_addr` 一致；FS 镜像类型暂限 `lfs` / `littlefs`。拒绝源目录中的符号链接/reparse point，支持中文源路径、中文文件名、空格、空文件和多级目录。

FS/FSKV 采用完整镜像下载通道执行擦写：replace 写完整 LittleFS 镜像，clear 写分区等长的全 FF 镜像，由实际打包的 ramrun 完成擦写，并对完整内容做设备 MD5 校验。64KiB 是来自 SDK 分区表的保守对齐检查，不再用于自制 CMD8 擦除循环。`GET_DOWNLOAD_INFO` 是下载缓存大小，不能当物理擦除依据；独立 CMD8 的 ACK 也不能替代数据校验。SOC 的 ramrun 来自 `csdk/tools/ramrun/ramrun.bin`，须按实际入包文件验证。

自动化验证包括策略组合、地址冲突/重叠/长度、不足一个物理擦除块的布局、每块清除与整段校验、取消、中文 LittleFS 构建及容量超限。Air1603_A10 / V1031 104 / COM11 已通过当前项目的 4.5MiB FS 写入、全 FF 清空、再次恢复资源镜像及启动测试；其他型号和 FSKV 本轮仍未做对应实机测试。

## 固件挂载参数

160X 数据 FS 必须使用 `name_max=63`（SDK `luat_fs_ccm42xx.c`）。通用 LittleFS 默认值为 255，超级块若声明 255，固件会拒绝挂载并自动格式化，表现为下载校验成功但启动后资源文件不存在。当前 160X 构建使用 63，并在写入前按该目标配置重挂载验证；路径每一级按 UTF-8 字节校验长度，不作截断。通用及其他芯片构建的默认值保持不变。

V1031 104 实机已进一步确认数据 FS 上的 `/Heat_exchange_station/main.xml` 可读取、FS 例程输出 `UI_READY` 和心跳。`/luadb` 启动 Logo 或 SD 例程成功均不能单独作为数据 FS 可用的证据。
