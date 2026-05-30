# Air8101 / BK72xx

## 适用型号

- Air8101
- bk72xx

## 推荐命令

```bash
# 全量刷机
luatos-cli flash run --soc firmware.soc --port COM6

# 刷机后继续监听 20 秒
luatos-cli flash run --soc firmware.soc --port COM6 --tail-log-secs 20

# 刷脚本区
luatos-cli flash script --soc firmware.soc --port COM6 --script lua/ --script lib/

# 文本日志
luatos-cli log view --port COM6 --baud 921600

# 新格式脚本 FOTA（仅脚本）
luatos-cli fota build --new firmware.soc --script-only -o air8101_script_fota.bin

# 新格式全量 FOTA
luatos-cli fota build --new firmware.soc -o air8101_full_fota.bin
```

## 说明

- BK72xx 默认文本日志流程
- `flash test` 会自动抓启动日志并验证关键字
- `fota build` 已支持 BK72XX 新格式全量/脚本包
- 当前仅支持 `rom.fs.script.bkcrc=true` 的新格式，旧 `LFTA` 不在 luatos-cli 支持范围内
