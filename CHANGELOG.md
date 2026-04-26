# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Features

#### SF32LB58 (Air8101/SF32) CH340X 增强 DTR 一键下载支持（luatos-flash + luatos-cli）

通过改装 CH340X 6# 脚（外接 4.7kΩ 下拉电阻）启用增强 DTR 模式，实现 SF32LB58 一键自动进入/退出 ROM BL，无需手动短接 MODE 跳线。

- **`flash run --auto-reset`** — 全量刷机前自动控制 DTR（BOOT0）/ RTS#（RESET）进入 ROM BL，刷机后自动恢复正常运行
- **`flash script --auto-reset`** — 脚本分区刷写同样支持自动复位序列
- **DTR/RTS 电平可配置** — 新增 `--dtr-boot <high|low>`、`--rts-reset <high|low>` 参数，支持调试不同极性的硬件接法
- **时序可调** — 新增 `--reset-ms <N>`（复位脉冲宽度，默认 100ms）、`--boot-wait-ms <N>`（进入 boot 后等待时长，默认 500ms）
- **向后兼容** — 不加 `--auto-reset` 时行为完全不变，仍需手动操作 MODE 跳线
- **协议文档** — 更新 `docs/sf32lb58-flash-protocol.md`，补充 CH340X 改装接线图、时序表、CLI 用法

#### 刷机进度步进控制（luatos-cli）

- **`flash --progress-step <N>`** — 控制进度输出频率，默认每 10% 输出一次，范围 1~50%
  - 新阶段切换（Connecting → Erasing → Writing 等）始终输出，无论步进值
  - `done` / `error` 事件始终输出
  - 适用于所有 flash 子命令：`run`、`script`、`clear-fs`、`flash-fs`、`clear-kv`、`ext-flash`、`ext-erase`、`test`

## [1.8.0] - 2026-04-26

### Features

#### SF32LB58 (Air8101) 刷机支持（luatos-flash）

基于 `sftool-lib`（纯 Rust 实现，Apache-2.0），无需 `ImgDownUart.exe`，跨平台支持。  
**实机验证通过**（2026-04-26，Air8101/SF32LB58 开发板）。

- **`flash run`** — 全量刷机：bootloader(NOR) + ftab(NOR) + app(NAND) + script(NAND)
  - 分区：bootloader @ `0x1C020000`，ftab @ `0x1C000000`，app @ `0x68000000`，script @ `0x69800000`
  - 实测写入：44 KB + 11 KB + 579 KB，设备成功启动
- **`flash script`** — 仅刷脚本分区（NAND @ `0x69800000`），速度更快
- **ROM BL 进入提示** — `device_enter_boot("sf32lb58")` 打印 MODE 引脚 + RESET 手动操作步骤
- **进度适配** — `SifliProgressSink` 将 sftool-lib 结构化事件映射到 `FlashProgress` 回调

#### luatos-soc schema 扩展

- **`SocFileEntry` 结构体** — `{ name, file }` 表示多 bin SOC 中的单个文件
- **`SocRom.files`** — `Option<Vec<SocFileEntry>>`，支持 SF32 等多文件 SOC 格式
- **`SocDownload.ftab_addr`** — `Option<String>`，NOR Flash 分区表地址
- **`SocInfo::ftab_addr()`** / **`SocInfo::extra_file(name)`** — 新 helper 方法

#### pack_soc.py 修复（luatos-sdk-sf32lb5x）

- 补充 `bl_addr = "0x1C020000"`、`ftab_addr = "0x1C000000"`、`force_br = "1000000"`

## [1.7.0] - 2026-04-18

### Features

#### Air6208 分区操作完整支持 (luatos-flash)
- **修复 XMODEM block 3 CAN 问题** — `xmodem_transfer` 入口添加 `serial_drain`，清除 `sync_bootloader` 阶段积累的 stale `'C'` 字节
  - 根因：设备在 XMODEM 就绪阶段持续发送 `'C'`，旧实现未清空缓冲区，导致 block 3 前后发生脏 ACK 链式消耗，设备等待超时后发送 CAN 中止传输
  - 修复后与 `wm_tool.c` 的 `uart_clear()` 行为一致
- **`flash clear-kv`** — 清除 Air6208 KV 分区（64 KB / 65 blocks，实机验证通过）
- **`flash clear-fs`** — 清除 Air6208 文件系统分区（3328 KB / 3329 blocks，实机验证通过）
- **`flash flash-fs`** — 打包 LuaDB 并烧录到 Air6208 文件系统分区（3329 blocks，实机验证通过）

#### SOC 信息展示增强 (luatos-soc / luatos-cli)
- **`soc info` 新增 FS/KV 分区信息** — 文本模式输出 FS Addr / KV Addr，JSON 模式在 `data` 中携带 `filesystem` 和 `kv` 字段

### Features

#### 环境诊断 — `doctor` 命令 (luatos-cli)
- **新增 `luatos-cli doctor` 子命令** — 一键诊断开发环境
  - 7 项自动检查：串口检测、项目配置验证、SOC 固件读取、Lua 编译器测试、资源目录、脚本目录、CDN 连通性
  - 每项检查返回 ✅/❌ + 改进建议
  - 支持 `--dir` 指定项目目录（默认当前目录）
  - 支持 Text / JSON / JSONL 三种输出格式

#### 智能日志分析 — `--smart` 标志 (luatos-log / luatos-cli)
- **新增 `log view --smart` 和 `log view-binary --smart`** — 实时智能诊断
  - 14 条诊断规则：OOM、低内存、Lua 运行时错误、require 失败、看门狗复位、SIM 卡/网络、DNS 失败、供电异常、I2C/SPI 错误、文件系统、panic、栈溢出、重复重启检测
  - 流式分析：每条日志实时检测，命中时立即输出诊断建议
  - 会话结束时输出诊断摘要（统计 + 建议列表）
  - 有状态分析：追踪启动次数，检测循环重启

#### MCP Server 重构 (luatos-mcp)
- **查询类工具重构为库直调** — serial_list、soc_info、soc_files、soc_unpack、soc_pack、project_info、resource_list 直接调用库函数，无需 spawn 子进程
  - 响应速度提升：消除进程启动开销（约 100ms → <1ms）
  - 无需 luatos-cli 二进制在 PATH 中（纯查询场景）
- **新增 `project_wizard` MCP 工具** — AI agent 可通过非交互模式创建 LuatOS 项目
- **新增 `device_reboot` / `device_boot` MCP 工具** — 设备重启和进入 bootloader 模式（库直调）
- **新增 `doctor` MCP 工具** — AI agent 可诊断用户开发环境
- 硬件操作（flash、log）仍走子进程保证进程隔离

### Tests
- 新增 4 个 doctor 单元测试
- 新增 9 个 SmartAnalyzer 单元测试（覆盖 14 条诊断规则 + 去重 + 摘要）
- 测试总数从 136 增至 159

---

## [1.6.2] - 2026-04-13

### Features

#### 向导式项目创建 (luatos-cli / luatos-project)
- **新增 `project wizard` 子命令** — 10 步交互式引导创建 LuatOS 项目
  - 从 CDN manifest 自动拉取型号列表（含 fallback 预置列表）
  - 引导选择：模组型号 → 固件版本 → 项目模板 → COM 口 → soc_script 版本
  - 可选立即下载固件和 soc_script 扩展库
  - 可选 `git init` + 自动生成 `.gitignore`
  - 全参数时跳过交互（CI/脚本友好）：  
    `project wizard --name my-app --model Air8101 --template helloworld --no-git --no-download`
- **`project new` 省略 `--chip` 自动进入向导** — 对齐新手使用习惯
- **新增 `TemplateKind` 模板系统** — `helloworld` / `ui` / `empty` 三种模板
  - `helloworld`：标准 Hello World 入门模板
  - `ui`：AirUI 风格 UI 模板（仅 bk72xx/air6208/air101 芯片）
  - `empty`：空模板（仅生成配置和 README）
- **新增 manifest 缓存** — `MANIFEST_CACHE_TTL_SECS = 300`（5 分钟，源码可调）
  - 缓存路径：`~/.cache/luatos-cli/manifest.json`（Linux/macOS）/ `%LOCALAPPDATA%\luatos-cli\manifest.json`（Windows）

### Tests
- 新增 14 个单元测试（wizard 数据层 6 个、template 渲染 5 个、向导非交互 3 个）

---

## [1.6.0] - 2026-04-13

### Features

#### soc_script 扩展库版本管理 (luatos-project)
- **新增 `build.soc_script` 配置项** — 控制 LuatOS 扩展库（CDN `public/soc_script`）的版本
  - `"latest"`（默认）：自动使用 `resource_dir/public/soc_script/` 下最新版本（字典序最大）
  - `"disable"`：不使用 soc_script 扩展库
  - 具体版本号（如 `"v2026.04.10.16"`）：使用指定版本
  - 未找到时输出提示：`luatos-cli resource download public`
- **新增 `build.resource_dir` 配置项**（默认 `"resource/"`）— 对应 `resource download --output` 目标目录
- **新增 `resolve_soc_script_lib_dir()` 公共函数** — 解析 soc_script `lib/` 目录路径
- **新增 `project build` 子命令** — 读取项目配置、自动注入 soc_script lib 目录后构建脚本镜像
- `project info` 输出新增 `soc_script` 和 `resource_dir` 字段展示

#### resource 下载 zip 自动解压 (luatos-resource)
- **zip 文件自动解压** — SHA256 校验通过后，`.zip` 文件自动解压到同级 `{stem}/` 子目录
  - 与 LuaTools 行为一致（`v2026.04.10.16.zip` → `v2026.04.10.16/lib/*.lua`）
  - 内部目录结构完整保留
- **新增 `DownloadEvent::Extracted`** — 解压完成后触发，携带目标目录路径

### Tests

- 新增 7 个单元测试（luatos-project: 5 个 soc_script 场景；luatos-resource: 2 个 zip 解压场景）
- 测试总数从 113 增加到 124

---

## [1.5.0] - 2026-04-12

### Features

#### GUI 桌面版 (luatos-gui)
- **Tauri 2 + Vue 3 图形界面** — 新增 `luatos-gui` crate，功能对标 LuaTools
  - 刷机视图 — 全量刷机/刷脚本区，支持所有 4 芯片系列，实时进度条，取消操作
  - 日志视图 — 文本+二进制日志流式显示，日志级别着色，过滤搜索，错误提示
  - 项目视图 — 新建/打开/编辑保存/导入 LuaTools INI 项目，最近项目侧栏，多项目切换
  - 构建视图 — Lua 编译 + LuaDB 打包，多目录支持
  - 固件视图 — 在线 CDN 固件清单浏览、按模组下载、SHA256 校验、进度显示
  - 设置视图 — 全局偏好持久化 (JSON)
- **全局串口工具栏** — 选一次串口，所有视图共享
- **刷机→日志自动跳转** — 刷完自动切到日志视图查看启动日志
- **暗色主题** — Tailwind CSS 深色界面

#### 固件资源库 (luatos-resource)
- **新增 `luatos-resource` crate** — 将资源清单获取/下载逻辑从 CLI 中抽取为独立库
  - `fetch_manifest()` — 从 CDN 获取资源清单 (JSON)
  - `collect_files()` — 按模组名/版本过滤可下载文件
  - `download_files()` — 多镜像回退下载 + SHA256 校验
  - `DownloadCallback` — 类型化进度回调，CLI 和 GUI 各自实现展示层
  - 9 个单元测试覆盖清单解析、文件条目、大小格式化、SHA256 等

#### EC718 (Air8000/Air780E) 完整刷机支持
- **EC718 刷机协议** — 纯 Rust 实现，支持 Air8000/Air780E/Air201 系列
  - 全量刷机 (`flash run`) — 自动进入 boot 模式，多串口自动检测
  - 单刷脚本区 (`flash script`) — CP 协议修正，测试脚本验证
  - USB 端口自动检测 — VID/PID 匹配 + 端口映射修正
- **EC718 日志解析**
  - 0x7E HDLC 帧解码器 — DTR/RTS 控制
  - 自动端口检测 + 串口日志抓取

#### 项目管理增强
- **完善项目管理功能** — 对标 LuaTools 功能集

#### 代码重构
- **CLI 模块拆分** — main.rs 拆分为 `cmd_serial.rs`、`cmd_soc.rs`、`cmd_flash.rs`、`cmd_log.rs`、`cmd_project.rs`、`cmd_build.rs`、`cmd_resource.rs` 功能模块
- **BUILTIN_MODULES 整理** — 统一内置模块注册

### Bug Fixes

- **EC718 USB 端口检测修正** — 修正 USB 端口映射和 CP 刷机协议
- **EC718 脚本区刷写修正** — 修正 script flash 协议，添加测试脚本
- **修复 cargo fmt 和 clippy 警告** — 清理 rustfmt.toml

### Documentation

- `docs/air6208-flash-protocol.md` — Air6208 刷机协议文档
- `docs/air8101-flash-protocol.md` — Air8101 刷机协议文档
- `docs/ec718-flash-protocol.md` — EC718 刷机协议文档（含端口映射、CP 修正）
- 更新 copilot-instructions，添加提交前检查清单

### Tests

- 测试数从 ~80 增加到 99（+2 ignored）

---

## [1.2.0] - 2026-04-11

### Features

#### Air1601 (CCM4211) 完整刷机支持
- **ISP + SOC 双阶段刷机协议** — 纯 Rust 实现
  - ISP 阶段：115200→1Mbps 切换，ramrun 加载，WRITE_RAM 分片传输
  - SOC 阶段：2Mbps，0xA5 帧协议，CRC16 校验，MD5 完整性验证
- **全量刷机** (`flash run`) — bootloader + core 固件下载
- **脚本区刷写** (`flash script`) — LuaDB 格式，Lua 5.3 64-bit
- **清除文件系统** (`flash clear-fs`) — 擦除 0x14D00000 分区
- **清除 FSKV** (`flash clear-kv`) — 擦除 0x14FF0000 分区
- **闭环测试** (`flash test`) — 自动刷机 + SOC 二进制日志抓取 + 关键字验证
- **CMD 11 重试机制** — 10 次重试、10ms 退避，应对 CH343 USB-Serial 丢帧

#### SOC 日志探测
- **日志探测帧** (`--probe`) — Air1601 固件内部有缓存机制，需发送 CMD_GET_BASE_INFO 探测帧触发日志输出
- **`log view-binary --probe`** — 打开串口后自动发送探测帧
- **`flash test` 自动探测** — Air1601/CCM4211 芯片自动发送探测 + SocLogDecoder 解码
- **`stream_binary()` 扩展** — 支持 `init_data` 参数

#### 文档
- `docs/ccm4211-flash-protocol.md` — ISP/SOC 刷机协议完整文档
- `docs/ccm4211-debug-notes.md` — CMD 11 超时调试经验和教训

### Changes

- **移除实验性 GUI** — 移除 `gui/` 目录及 workspace 成员
- **擦除超时增加** — erase_partition_ccm4211 超时从 2s 增加到 10s
- **修复 clippy 警告** — 消除所有 `-D warnings` 报错

### Dependencies

- 新增 `md5 = "0.7"` (CCM4211 固件校验)
- 新增 `windows-sys` (仅 Windows, overlapped I/O 支持)

---

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
