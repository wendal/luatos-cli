# Air8000 / EC7xx / Air780E 系列

## 适用型号

- Air8000
- ec7xx
- Air780EPM / Air780EHM / Air780EHV / Air780EHG

## 推荐命令

```bash
# 全量刷机（推荐自动端口）
luatos-cli flash run --soc firmware.soc --port auto

# 全量刷机并覆盖脚本区
luatos-cli flash run --soc firmware.soc --port auto --script lua/

# 刷机后继续监听日志
luatos-cli flash run --soc firmware.soc --port auto --tail-log-secs 30

# 刷脚本区
luatos-cli flash script --soc firmware.soc --port auto --script lua/

# 仅脚本 FOTA 包
luatos-cli fota build --new firmware.soc --script-only

# 差分 FOTA：底层固件相同时自动回落到脚本更新包（默认行为）
luatos-cli fota build --new v2.soc --old v1.soc

# 差分 FOTA：强制走 FotaToolkit 生成 delta.par（关闭自动回落）
luatos-cli fota build --new v2.soc --old v1.soc --force-par

# 日志查看（EC718 建议 921600 + probe）
luatos-cli log view-binary --port auto --baud 921600 --probe
```

## 说明

- EC718 USB CDC 在 Windows 下 2000000 常被改成 `921600`；**仅当打开的口就是 USB 日志口 x.2** 才 remap。CH340 调试口保持用户波特率 + 0xA5
- USB CDC `--probe` 发 `7E 00 00 7E`；CH340 仍发 0xA5 探测帧
- 运行时重启：`luatos-cli device reboot --chip air8000`（优先 x.2 DIAG `7E 00 01 7E`，不是进下载模式）
- 刷机与日志端口可能重枚举，`auto` 更稳妥
- `flash run --script` 会递归收集所选目录并生成 script.bin，覆盖 SOC 包内自带脚本
