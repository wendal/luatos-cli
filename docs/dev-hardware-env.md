# luatos-cli 硬件测试环境搭建指南

本文档面向希望在真实硬件上开发、测试 `luatos-cli` 的贡献者，涵盖设备准备、线缆与驱动安装、串口接线、以及各芯片系列的 boot/reboot 方法。

---

## 一、支持的模组型号

| 芯片系列 | 代表模组 | CLI 中的 chip 值 |
|---------|---------|----------------|
| BK7258 (博通) | Air8101 | `bk72xx` / `air8101` |
| XT804 (联盛德) | Air6208、Air101、Air103、Air601 | `xt804` / `air6208` / `air101` / `air103` / `air601` |
| EC718 (展锐) | Air8000、Air780E、Air780EPM、Air780EHM/EHV/EHG | `ec718` / `ec7xx` / `air8000` |
| CCM4211 (翱捷) | Air1601 | `ccm4211` / `air1601` |

---

## 二、所需硬件

### 通用（串口刷机 / 日志）

- **USB-TTL 串口模块**（任选一款）：
  - CH340/CH341（国产，便宜，Windows 需装驱动）
  - CP2102 / CP2104（Silicon Labs，稳定，多平台免驱或有官方驱动）
  - FT232RL / FT2232（FTDI，高质量，对 DTR/RTS 支持最好）
- **杜邦线若干**（公-母、母-母）

### EC718 系列（Air8000 等）

- **USB-C 数据线**（模组自带 USB，无需 USB-TTL）
- 或 UART 串口线（仅 UART 刷机时需要）

---

## 三、驱动安装

### Windows

| 芯片 | 驱动下载地址 |
|------|------------|
| CH340/CH341 | https://www.wch.cn/downloads/CH341SER_EXE.html |
| CP2102/CP2104 | https://www.silabs.com/developers/usb-to-uart-bridge-vcp-drivers |
| FT232/FT2232 | https://ftdichip.com/drivers/vcp-drivers/ |

安装完成后，在"设备管理器 → 端口 (COM 和 LPT)"中可看到新增 COM 口。

### Linux

多数驱动已内置于内核，无需额外安装。接入后：

```bash
ls /dev/ttyUSB* /dev/ttyACM*
```

若出现权限问题，将当前用户加入 `dialout` 组：

```bash
sudo usermod -aG dialout $USER
# 重新登录后生效
```

### macOS

- CH340：https://github.com/WCHSoftGroup/ch34xser_macos
- CP210x：安装 Silicon Labs 官方 macOS 驱动
- FTDI：一般无需安装，系统自带

---

## 四、串口 DTR/RTS 接线说明

`luatos-cli` 依赖 DTR/RTS 信号控制设备进入 bootloader 或执行复位。**请务必使用支持 DTR/RTS 的 USB-TTL 模块**（FT232/CP2102/CH340 均支持）。

### 常见接线（以 4 线接法为例）

```
USB-TTL 模块         开发板/模组
──────────           ────────────
  GND       ───────  GND
  3.3V/5V   ───────  VCC（按模组电压选择）
  TX        ───────  RX
  RX        ───────  TX
  DTR       ───────  RESET（部分板：NRST）
  RTS       ───────  BOOT0 / DOWNLOAD（芯片进 boot 的控制引脚）
```

> ⚠ **注意**：不同模组的 RESET / BOOT 引脚名称可能不同，请参考对应模组硬件手册。

### 各芯片 DTR/RTS 极性对应

| 芯片系列 | DTR 对应 | RTS 对应 |
|---------|---------|---------|
| BK7258 (Air8101) | RESET（低电平触发） | BOOT0（高电平进 boot） |
| XT804 (Air6208 等) | RESET（低电平触发） | BOOT（高电平进 boot） |
| CCM4211 (Air1601) | RESET | BOOT |
| EC718 (Air8000 等) | 通过 USB AT 口控制，无需 DTR/RTS | — |

> CH340G 的 DTR 输出为 **电平取反**（软件设 DTR=true → 引脚输出低电平）。FT232/CP2102 为**非反转**（DTR=true → 引脚高电平）。若使用 CH340G，实际电平与上表相反，但 luatos-cli 已按 serialport 库的逻辑统一处理，**通常无需手动调整**。

---

## 五、验证串口连接

连接好硬件后，运行：

```bash
luatos-cli serial list
```

预期输出示例：

```
PORT       VID        PID        PRODUCT
COM6       1a86       7523       USB Serial
COM7       10c4       ea60       CP2102 USB to UART Bridge Controller
```

若列表为空，检查：
1. 驱动是否已正确安装
2. USB 线是否为数据线（非纯充电线）
3. 设备管理器中是否有"未知设备"或感叹号

---

## 六、各芯片手工进入 boot 模式方法

### BK7258 / Air8101

**自动（推荐）**：

```bash
luatos-cli device boot --port COM6 --chip bk72xx
```

**手动**：
1. 按住 **BOOT** 按钮不放
2. 按下 **RESET** 按钮后松开
3. 松开 **BOOT** 按钮

模组进入 bootloader 后，COM 口仍存在但不输出日志。

---

### XT804 / Air6208 / Air101

**自动（推荐）**：

```bash
luatos-cli device boot --port COM6 --chip air6208
```

**手动**：
1. 断电，悬空 BOOT 引脚（或拉高至 3.3V）
2. 重新上电

---

### EC718 / Air8000 / Air780E 等

EC718 通过 USB AT 口自动控制，**无需手动操作**：

```bash
# 自动检测 USB 命令口（VID=0x19D1）
luatos-cli device boot --chip ec718

# 或指定串口
luatos-cli device boot --port COM8 --chip ec718
```

**手动**：
1. 按住 **BOOT** 按钮不放
2. 短按 **RESET** 或重新上电
3. 松开 **BOOT** 按钮

进入 boot 模式后，USB 设备会重枚举为 VID=0x17D1。

---

### CCM4211 / Air1601

**自动（推荐）**：

```bash
luatos-cli device boot --port COM6 --chip air1601
```

**手动**：
1. 按住 **BOOT** 按钮不放
2. 重新上电或按 **RESET**
3. 松开 **BOOT** 按钮

---

## 七、设备重启

所有芯片都支持通过命令重启：

```bash
# BK7258 / XT804 / CCM4211
luatos-cli device reboot --port COM6 --chip bk72xx

# EC718（自动检测 USB 命令口）
luatos-cli device reboot --chip ec718

# 通用（未知芯片，使用 DTR 脉冲）
luatos-cli device reboot --port COM6
```

---

## 八、运行硬件闭环测试

`flash test` 命令完整测试刷机 + 启动日志验证：

```bash
# Air8101 闭环测试
luatos-cli flash test \
  --soc ./firmware/Air8101.soc \
  --port COM6 \
  --keyword "LuatOS@" \
  --timeout 20

# Air8000 闭环测试
luatos-cli flash test \
  --soc ./firmware/Air8000.soc \
  --port COM7 \
  --keyword "LuatOS@" \
  --timeout 30
```

输出 `PASS` 表示刷机成功且设备正常启动，`FAIL` 则显示未匹配到关键字。

---

## 九、常见问题

| 现象 | 可能原因 | 解决方法 |
|------|---------|---------|
| 串口列表为空 | 驱动未安装 / USB 线不支持数据 | 安装驱动，更换 USB 数据线 |
| 无法进入 boot 模式 | DTR/RTS 未接线 / USB-TTL 不支持控制信号 | 确认接线，换 FT232 或 CP2102 |
| 刷机成功但无日志 | 波特率不对 / RX 线未接 | 检查接线，确认 RX↔TX 交叉接法 |
| EC718 自动检测失败 | 模组未连 USB 或 USB 驱动未装 | 确认 USB 数据线已连接，安装 EC718 USB 驱动 |
| Linux 串口无权限 | 用户不在 dialout 组 | `sudo usermod -aG dialout $USER` 后重新登录 |

---

## 十、CI 说明

CI 环境（GitHub Actions）**没有物理硬件**，因此：

- 所有硬件相关测试（刷机、日志、闭环测试）**不在 CI 中运行**
- CI 只运行 `cargo test --workspace`（纯单元测试）
- 硬件测试需在本地拥有真实模组的开发者机器上手工执行
