# Air724UG / UIS8910DM / RDA8910

## 适用型号

- Air724UG
- UIS8910DM（RDA8910，CAT1 bis）

## 推荐命令

```bash
# 全量刷机（开发更新：仅 AP/PS/LUA，保留 boot 与 NV）
luatos-cli flash run --soc firmware.soc --port auto

# 刷机后继续抓启动日志
luatos-cli flash run --soc firmware.soc --port auto --tail-log-secs 15

# 仅刷脚本区
luatos-cli flash script --soc firmware.soc --port auto --script lua/

# 文本日志（--port auto 自动识别 log 口）
luatos-cli log view --port auto --baud 921600
```

## 说明

- 下载模式：上电按键进入，USB 枚举为 SPRD U2S Diag（VID 0525:PID a4a7）
- 运行模式：UNISOC-8910 复合设备（VID 1782:PID 4d11 / 4e00），枚举多个同身份串口；AT 口靠行为探测（发 `AT` 收 "OK"）
- log 口按 USB 接口号自动识别（对齐 luatools_py3 `usb_device.py` 端口表）：PID 0x4d11 → 接口 x.6（soc_log）、PID 0x4e00 → 接口 x.2（user_log）；`--port auto` 时自动探测，接口信息缺失降级 AT 行为探测
- `--port auto`：自动探测下载口（VID 0525:a4a7）；未找到时三级软重启进下载模式（host fast `AD 00 04 FD 03 44 52 E8` / DIAG 0x7E 帧 / `AT*DOWNLOAD=1`），对齐 luatools_py3 流程；仍无则提示手动按键
- 刷机后 `--tail-log-secs` 会等待运行模式 log 口重新枚举（下载口在重启后消失）再抓启动日志
- 下载协议：PDL（ROM→FDL1，`0xAE` 帧）+ FDL（FDL1→FDL2→Flash，`0x7E` + CheckSum），细节见 [rda-flash-protocol.md](../rda-flash-protocol.md)
- 暂不支持：`flash test`、FOTA、二进制日志
