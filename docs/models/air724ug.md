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

# 二进制日志（--probe 自动探测）
luatos-cli log view-binary --port auto --baud 921600 --probe

# 差分升级包（--old，AP 全量 + CP 差分）
luatos-cli fota build --new v2.soc --old v1.soc

# 仅脚本热更新
luatos-cli fota build --new v2.soc --script-only

# 修改内置脚本后重打包 soc（soc combine 自动识别芯片：RDA8910 替换 PAC 内 LUA，无需 --addr）
luatos-cli soc combine --soc base.soc --bin build/script.bin -o patched.soc
```

## 说明

- 下载模式：上电按键进入，USB 枚举为 SPRD U2S Diag（VID 0525:PID a4a7）
- 运行模式：UNISOC-8910 复合设备（VID 1782:PID 4d11 / 4e00），枚举多个同身份串口；AT 口靠行为探测（发 `AT` 收 "OK"）
- log 口按 USB 接口号自动识别（对齐 luatools_py3 `usb_device.py` 端口表）：PID 0x4d11 → 接口 x.6（soc_log）、PID 0x4e00 → 接口 x.2（user_log）；`--port auto` 时自动探测，接口信息缺失降级 AT 行为探测
- `--port auto`：自动探测下载口（VID 0525:a4a7）；未找到时三级软重启进下载模式（host fast `AD 00 04 FD 03 44 52 E8` / DIAG 0x7E 帧 / `AT*DOWNLOAD=1`），对齐 luatools_py3 流程；仍无则提示手动按键
- 刷机后 `--tail-log-secs` 会等待运行模式 log 口重新枚举（下载口在重启后消失）再抓启动日志
- 下载协议：PDL（ROM→FDL1，`0xAE` 帧）+ FDL（FDL1→FDL2→Flash，`0x7E` + CheckSum），细节见 [rda-flash-protocol.md](../rda-flash-protocol.md)
- `fota build` 已支持差分（`--old`，AP 全量 + CP 差分）与仅脚本（`--script-only`）升级包，镜像 EC7xx；无 `--old` 且非脚本时报错提示提供 `--old`
- 需要外部 `dtools`（RDA 专用 LZMA 压缩 `lzmare2` 与 CP 差分 `fotacreate2`），用 `--fota-toolkit <dtools 路径>` 指定或放 `dtools/` 到 luatos-cli 同级
- 差分时 `STDVersion[4]` 取旧包 CP 版本（设备要求等于当前 CP 版本）
- `soc combine` 支持替换 PAC 内 `LUA`（脚本）条目并重打包（`rebuild_pac` 重算 size/offset/CRC16-ARC），无需 `--addr`；`--script-only` 升级包可用它配合生成自定义脚本固件
- 暂不支持：`flash test`
- 二进制日志（`log view-binary --probe`）已支持，`--port auto` 自动识别 log 口
