# EC618 USB 烧录实机测试脚本

用于验证 EC618 / Air780E / Air780EG 的 Lua 32 位脚本编译、脚本分区烧录和 USB 日志。

## 使用

先手动进入 BOOT，出现下载口后运行：

```bash
luatos-cli flash script --soc firmware.soc --port auto --script tests/ec618_test_script --luac-debug 0
luatos-cli log view-binary --port auto --baud 921600 --probe
```

预期每秒输出 `EC618_SCRIPT_OK`，持续运行，不能出现 `Lua VM exit`。

也可在进入 BOOT 后验证全量覆盖与日志闭环：

```bash
luatos-cli --format jsonl flash test --soc firmware.soc --port auto --script tests/ec618_test_script --luac-debug 0 --timeout 15 --keyword EC618_SCRIPT_OK
```

测试结束后重新进入 BOOT，使用原 SOC 全量刷机恢复自带脚本：

```bash
luatos-cli flash run --soc firmware.soc --port auto
```

## 本次验证

Windows、Air780EG_A12、`helloworld_1.0.0_LuatOS-SoC_V1124_EC618_FULL.soc`：

- 仅刷脚本：0 档编译为 393 字节 LuaDB，设备端校验完成，运行日志持续出现 `EC618_SCRIPT_OK`。
- 全量原固件：四分区烧录完成，`flash test --keyword "hi, LuatOS"` 输出 PASS，之后日志持续出现原脚本的 `hi, LuatOS`。
- 自动进下载模式未成功，实机操作均采用手动 BOOT。
- 一次全量闭环在 AP 写入中遇到 USB 断连重启，同一 release 二进制重新手动 BOOT 后复测通过；未确定该次复位原因。
