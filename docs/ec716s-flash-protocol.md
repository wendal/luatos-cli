# EC716S USB binpkg 刷机协议

EC716S 复用 [移芯 DLBOOT / AGBOOT / LPC 协议](ec718-flash-protocol.md)，实现位于 `luatos-flash/src/ec718.rs`。与 EC718M 的主要差别是使用 EC716 专用下载代理；与 EC618 的差别是 CP 仍位于 AP Flash。

## 固件与预检查

测试文件：`AirM2M_780EL_V1001_LTE_AT_TU.binpkg`，长度 1,783,324 字节，产品 `EC716S_PRD`。

- `0x08` 为 `bpkg`，`0x38` 为 `pkgmode`，产品字符串从 `0x190` 开始。
- 条目从 `0x1D8` 开始，元数据 364 字节，随后为镜像数据。
- 只接受 BL/AP/CP 三个非空镜像、正确顺序和 SHA256，禁止超出声明分区、2MiB Flash 容量、重叠及尾部额外数据。
- 包中没有 SOC 的 `info.json`，因此直接从产品名选择芯片；未知产品拒绝烧录，不回退 EC718M。

## AgentBoot

内置路径：`refs/origin_tools/ec_download/product_sets/ec716_products/common_data/agentboot_usb/agentboot.bin`。

USB AgentBoot 为 32,424 字节，SHA256：

```text
e9aa09f95d98d80a0d73f2d262d1893b4cf80dd2869f207949b25859d5ce295e
```

来源为仓库内移芯工具的 EC716 配套文件，`config_ec716s_prd_usb.baseini` 明确指定此路径。USB 使用 921600；本次只进行 USB 真机验证。

## 分区

| 镜像 | 包内地址 | 协议地址 | 字节数 | 存储 |
|------|----------|----------|--------|------|
| ap_bootloader | `0x803000` | `0` | 73,728 | AP Flash |
| ap_at_command_tu | `0x874000` | `0x74000` | 1,339,392 | AP Flash |
| cp-demo-flash | `0x81A000` | `0x1A000` | 368,640 | AP Flash |

CP 的 `LPC_BURN_ONE` 载荷仅含 CIMG 标识，不带 EC618 的独立 CP Flash 标记 `0xE101`。逐分区传输结束后查询烧写状态，通过后 LPC 复位；不清除文件系统或额外写入 RF 校准表。

## 实机记录

Windows + Air780ET（EL/ET 固件通用）：

1. 初次 COM3 被其他程序占用，返回拒绝访问；关闭串口软件后可正常访问。
2. 运行命令口 COM3（接口 2）收到 `AT+ECRST=delay,799` 与 DIAG `7E 00 02 7E` 后自动切换到下载口 COM6，无需按 BOOT。
3. AgentBoot、BL/AP/CP 三分区传输和设备端校验通过。
4. 复位后 `AT` 返回 `OK`，`ATI` / `AT+CGMR` 返回 `AirM2M_780ET_V1001_LTE_AT_TU`。

本固件接口 2 是文本 AT 口，接口 4 是二进制诊断日志；接口 6 的 AT 写入实测超时。串口编号随电脑而变化。

最终 release 版再次使用 JSONL 输出全量烧录，返回 `status=ok`、`chip=ec716s`、`firmware_format=binpkg`；复位后逐条验证 AT/ATI/AT+CGMR 均返回 OK，版本与上述记录一致。全工作区单元测试、clippy 和格式检查通过。该验证是主机脚本组织的“烧录 + AT 版本检查”，不是针对 LuatOS 的 `flash test` 命令。
