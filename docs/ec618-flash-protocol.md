# EC618 USB 刷机协议

EC618（Air780E / Air780EG）与 EC718 共用 DLBOOT、AGBOOT、LPC 帧协议，基础命令、镜像头、校验和与分块顺序见 [EC718 协议](ec718-flash-protocol.md)。实现复用 `crates/luatos-flash/src/ec718.rs`，由 `EigencommChip` 区分芯片配置。

## 固件包

本次 V1124 EC618 FULL 测试固件是 7z SOC，主要包含：

- `info.json`：`chip.type=ec618`，`rom.file=luatos.binpkg`，`script.file=script.bin`。
- `luatos.binpkg`：legacy 格式，条目从 `0x34` 开始，不包含 pkgmode 的芯片名称字段。
- `agentboot_usb.bin`：与固件配套的 EC618 下载代理。
- `config_ec618_usb.ini`、`format_ec618.json`、`MergeRfTable_ec618.bin`、`mem_map.h` 等厂商配置或资源。

不能仅凭 binpkg 的产品名判断芯片。legacy 包解析出的 `unknown` 必须由 SOC 元数据补充；量产包仍严格按 `rom.file` 和 `script.file` 选择文件。

## AgentBoot

- USB 优先使用 SOC 内的 `agentboot_usb.bin`，不替换为 EC718M AgentBoot。
- 包内缺失时回退仓库已有 EC618 USB 代理：`refs/origin_tools/ec_download/image_ec618/agentboot_usb/agentboot.bin`。
- UART 使用对应的 EC618 UART 代理；本次硬件验证范围为 USB。
- USB 串口与 AgentBoot 头中的下载波特率均为 921600，不采用 SOC 中面向 UART 的 `download.force_br=1152000`。

测试 SOC 自带 USB AgentBoot 为 35,676 字节，SHA256：

```text
f6e29bd2008175aa17367817f6ce53d29cf59eb6a7596b436d12acb82c03a362
```

## AP 与 CP 存储差异

| 镜像 | EC618 | EC718 / EC7xx |
|------|-------|---------------|
| Bootloader | AP Flash，协议地址 0，实际位置由镜像自身决定 | 同左 |
| AP | AP Flash，使用原始 Flash 偏移，若带 AP XIP 基址则去掉 `0x800000` | 同左 |
| CP | **独立 CP Flash**，使用 binpkg 中 CP 地址 | AP Flash，地址去掉 `0x800000` |
| script | FlexFile / AP Flash，脚本地址补 AP XIP 基址 `0x800000` | 同左 |

`LPC_BURN_ONE` 的 CP 载荷不同：

```text
EC618：49 42 50 43 01 E1
EC7xx：49 42 50 43
```

前 4 字节是 `CIMG_IDENTIFIER=0x43504249` 的小端表示。EC618 后追加小端 `CP_FLASH_MARKER=0xE101`，否则代理会选择错误的 Flash。

## V1124 测试包分区

| 名称 | binpkg / SOC 地址 | 协议地址 | 数据长度 | 存储 / 类型 |
|------|------------------|----------|----------|-------------|
| ap_bootloader | `0x4000` | `0` | 117,524 | AP / BL |
| ap | `0x24000` | `0x24000` | 2,392,272 | AP / AP |
| cp-demo-flash | `0` | `0` | 472,684 | CP / CP |
| script | `0x2CC000` | `0xACC000` | 1,024 | AP / FlexFile |

完整烧录与 LuaTools `ec618_make_cmd` 的 `bootloader system cp_system flexfile2` 列表对应，不额外烧写 RF 校准表或清除文件系统。

## 下载模式与实机验证

USB 标识与 EC718 相同：运行 `19D1:0001`，下载 `17D1:0001`。SOC 日志 / DIAG 使用接口 2/3，用户口使用接口 6/7。

自动进下载模式前拉高 DTR/RTS，发送 `AT+ECRST=delay,799\r\n`，等待 200ms，再发送 `7E 00 02 7E`。必须检测到下载 VID/PID 才视为成功。

Windows + Air780EG_A12、V1124 EC618 FULL 实测：自动方式没有进入下载模式，手动按住 BOOT 复位后出现 COM6。原生协议完成上述四个分区、逐分区设备端校验和 LPC 复位；随后 COM3 日志输出固件型号、硬件型号和持续 `hi, LuatOS`。

串口日志沿用共享的 0x7E 解码路径，使用 `log view-binary --port auto --baud 921600 --probe`。

release 版还验证了单独刷脚本（393 字节镜像，0 档调试信息）和全量 `flash test` JSONL 闭环。最终结果为 `PASS`、`chip=ec618`、`missing_keywords=[]`，设备已恢复 SOC 原始脚本。一次闭环尝试在 AP 写入阶段出现 USB 断连重启，重新进入 BOOT 后同一二进制复测通过，该次复位原因未确定。
