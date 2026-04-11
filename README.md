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

# 刷机 (Air8101/BK7258)
luatos-cli flash run --soc path/to/firmware.soc --port COM6

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
├── luatos-serial/    # 串口枚举
├── luatos-project/   # 项目管理 (WIP)
└── luatos-log/       # 日志解析 (WIP)
```

## 当前支持

- **Air8101 (BK7258)**: 完整刷机支持 (原生协议 + subprocess 模式)
- SOC 文件解析和解包
- 串口枚举
- LuaDB 打包

## 开发

```bash
cargo test           # 运行单元测试
cargo test -- --ignored --nocapture  # 运行硬件测试 (需 Air8101 在 COM6)
```
