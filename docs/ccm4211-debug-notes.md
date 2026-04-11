# CCM4211 (Air1601) 调试笔记

## 背景

在 luatos-cli 中实现 Air1601 (CCM4211 芯片) 的刷机支持时，遇到了一个耗时数天的
CMD 11 (SET_CODE_DATA) 超时问题。本文记录调试过程和最终发现的根本原因。

## 问题现象

### 初始症状

- ISP 阶段（握手、波特率切换、ramrun 加载）：**100% 可靠**
- CMD 9 (GET_DOWNLOAD_INFO)：**100% 可靠**
- CMD 10 (SET_CODE_DATA_START)：**100% 可靠**
- CMD 11 (SET_CODE_DATA, 512+ 字节负载)：**间歇性失败**
  - 设备不返回任何数据（reads=0, bytes=0）
  - 不是数据损坏，是完全没有响应

### 误导性线索

在 Rust 实现中观察到一个强相关的现象：

| 日志级别 | stderr 输出量 | CMD 11 成功块数 |
|----------|--------------|----------------|
| RUST_LOG=trace | 大量 | 全部成功 |
| RUST_LOG=debug | 中等 | 4 块 |
| RUST_LOG=info + eprintln | 少量 | 15 块 |
| RUST_LOG=info（无额外输出）| 无 | 0 块 |

这个线性相关让我们误以为这是一个**时序问题**——stderr 输出引入的延迟
恰好满足了设备需要的某种时序。

### 错误的假设

1. ❌ **同步 vs 异步 I/O**：以为 `serialport` crate 使用 `FILE_ATTRIBUTE_NORMAL`
   (同步 I/O) 是根因，因为 Python LuaTools 使用 `FILE_FLAG_OVERLAPPED`
2. ❌ **USB 转串口芯片碎片化**：以为 CH343 将大帧拆分为多个 USB bulk 传输，
   导致设备 UART 空闲检测误触发
3. ❌ **设备端 UART ISR 竞态**：分析了 ramrun 的 `soc_rx` 中断处理，
   发现 `uart_rx_done` 标志可能导致丢帧
4. ❌ **各种延时/忙等**：尝试了 500μs、1ms、5ms sleep，2ms busy-wait，全部无效

### 尝试过的方案（均失败）

- 减小块大小到 512B（从 3KB）
- 写入前添加延时
- 移除 `flush()` 调用
- 添加 ClearCommError 清除串口错误
- 修改读取超时时间

## 根本原因

### 发现过程

按照用户建议，转用 Python 重写刷机逻辑来验证协议。在 Python 实现中发现：

1. **ISP 阶段的 `WRITE_RAM` 命令 `param1` 参数不正确**
2. Python 代码中 `param1=0`（固定值），但正确实现应该是 `param1=0x30` 起步，每块 +2

### 根因分析

```python
# ❌ 错误实现（Python 脚本初始版本）
isp_send_cmd(port, 0x31, 0, 0, len(chunk), chunk)
#                        ↑ param1 始终为 0

# ✅ 正确实现（Rust 代码和最终 Python 修复）
isp_send_cmd(port, 0x31, base_address, 0, len(chunk), chunk)
#                        ↑ 0x30, 0x32, 0x34, ... 递增
```

**后果：** 所有 ramrun 数据块被写入到 RAM 的同一个地址（地址页 0），
导致 ramrun 程序在 RAM 中严重损坏。虽然 ROM bootloader 的 WRITE_RAM 命令
返回了 ACK（说明写入操作本身成功），但被覆盖的 ramrun 代码执行后行为异常。

### 为什么 ISP 阶段看起来正常

1. ROM bootloader 不验证写入的数据内容
2. EXECUTE 命令不需要 ACK（ramrun 接管 UART 后无法回复）
3. 即使 ramrun 损坏，它可能部分初始化了 UART，导致：
   - 简单命令（CMD 9、10）可以被部分处理和回复
   - 复杂的数据传输命令（CMD 11）因为 ramrun 代码损坏而无法正确处理
   - trace 日志的延迟恰好让损坏的代码有时能勉强完成处理

### 为什么 Rust 代码的 ISP 阶段是正确的

Rust 代码中 `base_address` 从一开始就是正确的（0x30 起步，每块 +2）：

```rust
let mut base_address: u8 = 0x30;
// ...
isp_send_cmd(&mut port, ISP_CMD_WRITE_RAM, base_address, 0x00, ...)?;
base_address = base_address.wrapping_add(2);
```

**所以 Rust 的 CMD 11 超时是另一个问题**（可能确实与同步 I/O 有关），
但通过添加 **重试机制** 成功解决了：

```rust
// 最多重试 10 次，间隔 10ms
for retry in 0..10 {
    match soc_send_cmd(port, parser, SOC_CMD_SET_CODE_DATA, ...) {
        Ok(_) => break,
        Err(e) => {
            std::thread::sleep(Duration::from_millis(10));
            last_err = Some(e);
        }
    }
}
```

## 两个独立问题

经过完整调试，发现实际上存在**两个独立的问题**：

### 问题 1：Python ramrun 加载地址错误（致命）

- **现象：** SOC 协议阶段完全不工作
- **原因：** WRITE_RAM 的 param1 始终为 0，ramrun 损坏
- **修复：** 正确递增 base_address

### 问题 2：Rust 同步 I/O 下 CMD 11 间歇性超时（非致命）

- **现象：** 约 30-50% 的 CMD 11 块首次发送无响应
- **原因：** 同步 I/O + CH343 USB 转串口 + 2Mbps 高波特率
  - 大帧可能被 USB 批量传输拆分
  - 设备 UART 空闲检测误触发
- **修复：** 重试机制（10 次，10ms 间隔）

## CH343 串口注意事项

### 波特率切换

在 CH343 USB 转串口芯片上，**关闭并重新打开串口来切换波特率会失败**：

```python
# ❌ 关闭再打开：CH343 上无法通信
port.close()
port = serial.Serial(port_name, 1000000, ...)

# ✅ 就地切换：正常工作
port.baudrate = 1000000
port.reset_input_buffer()
port.reset_output_buffer()
```

这可能与 CH343 驱动的 COM 端口生命周期管理有关。Rust `serialport` crate
的 `drop + new` 模式不受此影响（可能是因为底层 Win32 API 调用时序不同）。

### 高波特率可靠性

CH343 支持最高 6Mbps，但在 2Mbps 下：
- 大数据包（>3KB）可能因 USB bulk 传输拆分而出现问题
- 同步 I/O 模式下成功率约 50-70%
- 重试机制可以有效解决

## 经验教训

### 1. 优先用已知工具验证协议

当遇到难以复现的硬件通信问题时，先用**参考实现**（如 Python LuaTools）确认
协议正确性，再排查底层 I/O 问题。本次如果早点用 Python 验证，可以更快发现
ramrun 加载地址错误。

### 2. 区分致命与非致命问题

CMD 11 在 Rust 中有两个问题叠加：
- ramrun 损坏（致命）→ 需要修复地址
- 同步 I/O 超时（非致命）→ 重试即可

Trace 日志级别下 CMD 11 能成功的现象让我们误以为只有一个时序问题。

### 3. 不要假设 ISP ACK = ramrun 加载正确

ROM bootloader 的 WRITE_RAM ACK 只表示"写入操作完成"，
不验证数据是否写入了正确的地址。这种"静默失败"很难发现。

### 4. 串口重试是必要的

在 USB 转串口 + 高波特率环境下，通信不是 100% 可靠的。
参考实现（Python LuaTools、automated_testing）都包含重试逻辑：
- CMD 11: 10 次重试，10ms 间隔
- CMD 12: 3s 超时
- CMD 13: 4-10s 超时

### 5. CH343 的串口生命周期管理与常规不同

关闭/重开串口在 CH343 上可能导致通信失败。优先使用就地参数修改。

## 相关文件

| 文件 | 说明 |
|------|------|
| `crates/luatos-flash/src/ccm4211.rs` | Rust 刷机实现 |
| `docs/ccm4211-flash-protocol.md` | 刷机协议文档 |
| `refs/soc_files/ccm4211_ramrun_default.bin` | Ramrun 二进制 |
| `D:\github\luatools_py3\ccm4211_isp.py` | Python 参考（ISP）|
| `D:\github\luatools_py3\my_serial.py` | Python 参考（串口 I/O）|
| `D:\github\luatos-ext-components\automated_testing` | Python 参考（自动化测试）|
