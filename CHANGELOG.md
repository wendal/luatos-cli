# Changelog

All notable changes to this project will be documented in this file.

## [1.0.0] - 2026-04-11

### 🎉 Initial Release

LuatOS 命令行工具集 v1.0.0 — 纯 Rust 实现的 LuatOS 开发工具。

### Features

#### 刷机 (Flash)
- **Air8101 (BK7258)** 完整刷机支持
  - 全量刷机 (`flash run`) — 原生 Rust 协议 + subprocess 模式双通道
  - 单刷脚本区 (`flash script`) — 开发迭代最常用
  - 烧录文件系统区 (`flash flash-fs`)
  - 清除文件系统区 (`flash clear-fs`)
  - 清除 FSKV 键值区 (`flash clear-kv`)
- **Air6208 (XT804)** 刷机支持
  - 全量刷机 (`flash run`) — XMODEM-1K 协议
  - 单刷脚本区 (`flash script`)
- **闭环刷机测试** (`flash test`)
  - 自动流程：刷机 → 抓取 boot log → 验证关键字 → PASS/FAIL
  - 支持自定义超时、多关键字验证
  - JSON 输出方便 CI 集成

#### 日志 (Log)
- 实时查看串口文本日志 (`log view`)
- SOC 二进制日志解码 (`log view-binary`) — 支持 Air6208 JTT 格式
- 日志录制到文件 (`log record`) — 文本 + JSON 双格式
- 已保存日志文件解析 (`log parse`)
- 可扩展日志解析器 (`LogParser` trait)
  - 内置: LuatOS 标准格式、BK72xx Boot 日志、SOC 二进制帧

#### SOC 文件管理
- SOC 文件信息查看 (`soc info`)
- SOC 文件解包 (`soc unpack`) — 支持 ZIP + 7z 格式
- SOC 文件列表 (`soc files`)
- SOC 文件打包 (`soc pack`) — 纯 Rust 7z 压缩 (sevenz-rust2)

#### 项目管理
- 项目脚手架 (`project new`) — 芯片感知的默认配置
- 项目信息查看 (`project info`)
- 项目配置管理 (`project config`) — 读取/修改 TOML 配置

#### 构建系统
- Lua 脚本编译 (`build luac`) — 自动查找 luac / luac_64bit
- LuaDB 文件系统镜像合成 (`build filesystem`)
  - 多目录脚本源支持
  - 可选 BK CRC16 适配 (Air8101)
  - 可选 luac 预编译

#### 工具
- 串口枚举 (`serial list`) — 包含 VID/PID/设备名
- 全局 `--format json` 输出格式

### Architecture

- 7-crate Rust workspace 架构
- 纯 Rust 实现，无外部可执行文件依赖 (7z 使用 sevenz-rust2)
- Git LFS 管理大型二进制文件 (*.soc, *.exe, *.dll, *.bin)
- 52 个单元测试覆盖
