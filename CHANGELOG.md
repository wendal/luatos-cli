# Changelog

All notable changes to this project will be documented in this file.

## [1.1.0] - 2026-04-11

### Features

#### 内嵌 Lua 编译器
- **内嵌 Lua 5.3.6 编译器** — 不再依赖外部 luac 可执行文件
  - 编译期通过 cc crate 构建 32/64 位 luac helper
  - 运行时通过 `include_bytes!()` 嵌入并缓存到用户本地目录
  - 新增 `compile_lua_bytes()` API：源码字节输入，字节码输出
  - 刷机命令自动从 SOC info.json 读取 bitw/use-luac/use-debug 配置

#### XT804 完整刷机支持
- **XT804 刷机操作全面完善**
  - 文件系统区刷写 (`flash flash-fs`) — 基于 LittleFS
  - 清除文件系统区 (`flash clear-fs`)
  - 清除 FSKV 键值区 (`flash clear-kv`)
  - XT804 镜像头构建 (`build_xt804_image`) 含 CRC32 校验
- **内嵌 mklfs** — LittleFS 镜像构建工具作为 C helper 嵌入
  - 新增 `build_littlefs_image()` API

#### 资源管理
- 新增 `resource list` / `resource download` 子命令 — CDN 资源下载
- 新增 `version` / `help` 子命令

#### CI/CD
- macOS (x86_64 + aarch64) CI 及发布工作流支持
- Unix 平台 luac 查找改用 `which` 替代 Windows-only `where`

### Bug Fixes

- **XT804 脚本/分区刷写修正** — 修正镜像头地址、CRC32 计算、payload 对齐 (1024 字节 / 0x00 填充)
- **Windows luac helper 二进制模式** — 修复 stdin/stdout 文本模式导致字节码损坏
- **Linux 并行提取竞态修复** — 添加 per-helper Mutex 防止 ETXTBSY 错误
- **resource JSON 反序列化** — 支持数组格式文件条目、可选 desc 字段

### Tests

- 测试数从 52 增加到 59
- 新增 32/64 位编译、strip 模式、语法错误等测试用例

---

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
