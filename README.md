# luatos-cli

LuatOS 命令行工具集 — 刷机、日志、项目管理、固件合成。

## 快速开始

```bash
# 构建
cargo build --release

# 列出串口
luatos-cli serial list

# 查看 SOC 文件信息
luatos-cli soc info path/to/firmware.soc

# 全量刷机 (Air8101/BK7258)
luatos-cli flash run --soc path/to/firmware.soc --port COM6

# 单刷脚本区 (开发最常用)
luatos-cli flash script --soc firmware.soc --port COM6 --script ./lua/

# 清除文件系统
luatos-cli flash clear-fs --soc firmware.soc --port COM6

# 清除 FSKV 键值存储
luatos-cli flash clear-kv --soc firmware.soc --port COM6

# 实时查看串口日志
luatos-cli log view --port COM6 --baud 921600

# 记录日志到文件 (含 JSON 解析)
luatos-cli log record --port COM6 --output ./logs/ --json

# 解析已保存的日志文件
luatos-cli log parse ./logs/log_20250101_120000.txt

# JSON 输出 (AI 友好)
luatos-cli --format json serial list
```

## 架构

```
crates/
├── luatos-cli/       # 主入口 CLI
├── luatos-soc/       # SOC 文件解包/打包/info.json
├── luatos-luadb/     # LuaDB 脚本打包 + BK CRC16
├── luatos-flash/     # 刷机协议 (BK7258)
├── luatos-serial/    # 串口枚举 + 日志流
├── luatos-project/   # 项目管理 (WIP)
└── luatos-log/       # 日志解析 (可扩展 trait)
```

## 当前支持

- **Air8101 (BK7258)**: 完整刷机支持 (原生协议 + subprocess 模式)
  - 全量刷机 (`flash run`)
  - 单刷脚本区 (`flash script`) — 开发最常用
  - 合成/烧录文件系统 (`flash flash-fs`)
  - 清除文件系统 (`flash clear-fs`)
  - 清除 FSKV 键值区 (`flash clear-kv`)
- **日志系统**: 实时查看 + 录制 + 解析
  - 可扩展解析器 (`LogParser` trait)
  - 内置 LuatOS 标准格式解析
  - 内置 Boot 日志解析
  - 文本 + JSON 双格式存储
- SOC 文件解析和解包
- 串口枚举 (VID/PID/设备名)
- LuaDB 打包 + BK CRC16

## 日志解析器扩展

实现 `LogParser` trait 即可添加新模组的日志格式支持:

```rust
use luatos_log::{LogParser, LogEntry};

struct MyChipParser;
impl LogParser for MyChipParser {
    fn name(&self) -> &str { "my_chip" }
    fn parse_line(&self, line: &str) -> Option<LogEntry> {
        // 自定义解析逻辑
        todo!()
    }
}
```

## 开发

```bash
cargo test           # 运行单元测试
cargo test -- --ignored --nocapture  # 运行硬件测试 (需 Air8101 在 COM6)
```
