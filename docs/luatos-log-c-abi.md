# luatos-log C ABI (共享库导出)

> 适用芯片: Air1601 / Air1602 / CCM4211 (CMCC IoT 模组系列)
> 共享库名: `luatos_log_ffi` (Windows: `.dll`, Linux: `.so`, macOS: `.dylib`)
> 版本: 与 luatos-cli workspace 同步 (1.8.1+)
> ABI 稳定: 是 (错误码值/函数签名只追加, 不修改)

## 1. 概述

`luatos-log-ffi` 是 `crates/luatos-log` 的 C ABI 导出层, 把 Air1601/CCM4211 SOC
二进制日志帧的解码能力以**共享库**形式发布, 供 Python (`ctypes`)、C/C++、
Go (`cgo`)、C# (P/Invoke) 等语言直接消费, 无需绑定 Rust 工具链.

**完全兼容** 现有第三方 `pySoclogAnalyze` DLL 签名, 现有用户可**无感替换** DLL
文件, 无需修改调用方代码.

## 2. 函数列表

| 函数 | 说明 |
|------|------|
| `luatos_soclog_analyze()` | 主入口, 解码一帧 SOC 日志 |
| `pySoclogAnalyze()` | 与主入口等价的别名 (兼容第三方 DLL 调用方) |
| `luatos_soclog_version()` | 查询 FFI 库版本号 (1.0.0) |

所有函数 `extern "C"` 链接, 默认 cdecl 调用约定, 跨 MSVC / gcc / clang 兼容.

## 3. 协议背景

SOC 日志帧 (从设备串口读出的字节流) 结构如下:

```
[0xA5] [escaped(payload)] [escaped(CRC16_LE)] [0xA5]
  ↑                              ↑
  帧头 (1B)                CRC16 (2B, LE)
  帧尾 (1B)
```

**转义规则** (避免与 0xA5/0xA6 冲突):
- 原始 `0xA5` → 转义为 `0xA6 0x01`
- 原始 `0xA6` → 转义为 `0xA6 0x02`
- 其他字节 → 透传

**24 字节 header** (小端序) 支持两种布局, 由 `payload[16..20]` 的 `cmd` 字段区分:

### 3.1 日志帧 (`cmd == 0`)

| 字节 | 类型 | 字段 | 含义 |
|------|------|------|------|
| 0..8 | u64 | `ms` | 设备时间戳 (毫秒) |
| 8..9 | u8 + 7b | `tag.level` | 1=Debug, 2=Info, 3=Warn, 4=Error |
| 9..16 | 7×7b | `tag.tag0..7` | 模块名, 7-bit ASCII, 最多 8 字符 |
| 16..20 | u32 | `cmd` | 固定为 0 (日志帧) |
| 20..22 | u16 | `sn` | 序列号 |
| 22 | u8 | `type` | 消息类型 (0 = printf, 其他 = raw) |
| 23 | u8 | `cpu` | CPU 编号 |
| 24.. | - | body | NUL 终止的 printf 格式串 + 4 字节对齐变参 |

### 3.2 命令帧 (`cmd != 0`)

| 字节 | 类型 | 字段 | 含义 |
|------|------|------|------|
| 0..8 | u64 | `ms` | 设备时间戳 (毫秒) |
| 8..12 | u32 | `address` | Flash 操作地址 |
| 12..16 | u32 | `len` | payload 长度 |
| 16..20 | u32 | `cmd` | 命令码 (非 0) |
| 20..22 | u16 | `sn` | 序列号 |
| 22 | u8 | `type` | 消息类型 |
| 23 | u8 | `cpu` | CPU 编号 |
| 24..24+len | - | payload | `len` 字节 raw bytes |

**CRC16 算法**: 多项式 `0xA001`, 初始值 `0`, 查表算法 (256 项).
**Body 4 字节对齐**: 变参按 4 字节对齐, 字符串用 NUL 终止 + 后续 padding 0.

## 4. 参数说明

```c
int luatos_soclog_analyze(
    uint8_t*  data,      // [in]  原始字节流 (含 0xA5 边界 + 0xA6 转义)
    uint32_t  len,       // [in]  data 字节数
    char*     out,       // [out] 日志帧: "[device_time] L/module message";
                         //       命令帧: cHead.len 字节 raw payload (截断到 64KB-1)
    uint64_t* tick,      // [out] 设备时间戳 (ms)
    uint16_t* sn,        // [out] 序列号
    char*     tag,       // [out] 日志帧: 模块名 (NUL 终止, 最长 63 字节);
                         //       命令帧: 空字符串 (tag[0] = 0)
    uint8_t*  cpu,       // [out] CPU 编号
    uint8_t*  type,      // [out] msg_type 字段
    uint32_t* cmd,       // [out] 命令码; 0 = 日志帧, 非 0 = 命令帧
    uint32_t* address    // [out] 日志帧: 固定 0; 命令帧: 帧的 address 字段
);
```

**调用方应保证**:
- `data` 指向至少 `len` 字节的有效可读内存
- `out` 至少 64B (推荐 64KB) 容量
- `tag` 至少 64B 容量
- `tick`/`sn`/`cpu`/`type`/`cmd`/`address` 各自指向有效的可写标量槽

**`address` 双模式语义** (关键):
- **日志帧** (`*cmd == 0`): `*address` 固定为 0, 因为日志帧协议无此字段
- **命令帧** (`*cmd != 0`): `*address` 为帧头 `payload[8..12]` 的 `address: u32`

## 5. 返回值与错误码

| 返回值 | 名称 | 含义 |
|--------|------|------|
| `0` | `LUATOS_SOCLOG_OK` | 成功解码一帧 |
| `-1` | `LUATOS_SOCLOG_ERR_CRC` | CRC16 校验失败 |
| `-2` | `LUATOS_SOCLOG_ERR_HEADER` | header 长度不足 26 字节 |
| `-3` | `LUATOS_SOCLOG_ERR_BUFFER` | 输出 buffer 太小 (保留码, 当前截断不返回) |
| `-4` | `LUATOS_SOCLOG_ERR_PARAM` | 任一必填指针为 NULL |
| `-5` | `LUATOS_SOCLOG_ERR_EMPTY` | `len == 0` 或数据为空 |
| `-6` | `LUATOS_SOCLOG_ERR_TRUNC` | 帧超过 8192 字节被截断 |

错误码值是 ABI 的一部分, 未来新增只能追加 (例如 `-7`, `-8`), 不会改变已定义值.

## 6. Out 缓冲区约定

`out` 的写入内容因帧模式不同:

- **日志帧**: 写入 `[device_time] L/module message`, NUL 终止.
  例如: `[5.000] I/user.main Hello world`. `device_time` 由 `ms / 1000` 转换为秒, `ms % 1000` 转换为毫秒. `L` 是级别 (D/I/W/E/T).
- **命令帧**: 写入 `cHead.len` 字节 raw payload (取自 `payload[24..]`), 末尾追加 NUL. 如果 `cHead.len` 大于实际可用字节, 截断到帧内可用字节数; 仍超 64KB-1 则截断到 64KB-1.

无论哪种情况, 末尾都有 NUL 终止 (保证 C 字符串安全).

## 7. 线程安全与性能

- **线程安全**: 每次调用独立, **不持有全局状态** (与第三方 DLL 不同 — 第三方 DLL 用 `prvCommonLogBufferRx` 全局累积, 我们改为 stateless). 多线程可并发调用同一句柄, 无需加锁.
- **单次调用性能**: 实测单核 ~1-5 微秒 (i5-8250U, 包含 0xA5 边界扫描 + 转义还原 + CRC 校验 + printf 还原). 适合高频轮询 (10k+ fps).
- **多次调用拼一帧**: 调用方自行维护 byte buffer, 每次攒够 0xA5...0xA5 后调用本函数. 库不跨调用保持状态.

## 8. 示例代码

### 8.1 C

```c
#include "luatos_log.h"
#include <stdint.h>
#include <stdio.h>
#include <string.h>

int main(void) {
    /* 假设从串口读到 frame, len 字节 */
    uint8_t frame[256];
    uint32_t frame_len = /* ...read from serial... */ 36;

    char out[64 * 1024] = {0};
    char tag[64] = {0};
    uint64_t tick = 0;
    uint16_t sn = 0;
    uint8_t cpu = 0, type = 0;
    uint32_t cmd = 0, address = 0;

    int rc = luatos_soclog_analyze(
        frame, frame_len, out, &tick, &sn, tag,
        &cpu, &type, &cmd, &address);

    if (rc == LUATOS_SOCLOG_OK) {
        if (cmd == 0) {
            /* 日志帧 */
            printf("[%llu.%03llu] %s/%s %s\n",
                   (unsigned long long)(tick / 1000),
                   (unsigned long long)(tick % 1000),
                   out, tag, "");
        } else {
            /* 命令帧 */
            printf("CMD 0x%08X addr=0x%08X payload=...\n", cmd, address);
        }
    } else {
        fprintf(stderr, "decode failed: rc=%d\n", rc);
    }
    return 0;
}
```

完整可编译示例: `crates/luatos-log-ffi/examples/c/demo.c` + `Makefile` / `build.bat`.

### 8.2 Python (ctypes)

```python
import ctypes
from ctypes import c_uint8, c_uint16, c_uint32, c_uint64, c_char_p, POINTER, byref, create_string_buffer

lib = ctypes.CDLL("libluatos_log_ffi.so")  # Linux
# lib = ctypes.WinDLL("luatos_log_ffi.dll")  # Windows

ANALYZE = lib.luatos_soclog_analyze
ANALYZE.argtypes = [
    POINTER(c_uint8), c_uint32,
    c_char_p,
    POINTER(c_uint64), POINTER(c_uint16),
    c_char_p,
    POINTER(c_uint8), POINTER(c_uint8),
    POINTER(c_uint32), POINTER(c_uint32),
]
ANALYZE.restype = ctypes.c_int

# 假设 frame 是从串口读到的 bytes
data = (c_uint8 * len(frame)).from_buffer_copy(bytes(frame))
out = create_string_buffer(64 * 1024)
tag = create_string_buffer(64)
tick, sn = c_uint64(), c_uint16()
cpu, type_v = c_uint8(), c_uint8()
cmd, addr = c_uint32(), c_uint32()

rc = ANALYZE(
    data, len(frame), out,
    byref(tick), byref(sn), tag,
    byref(cpu), byref(type_v),
    byref(cmd), byref(addr),
)

if rc == 0:
    print(f"OK: tick={tick.value} sn={sn.value} tag={tag.value.decode()!r} out={out.value.decode()!r}")
else:
    print(f"FAILED: rc={rc}")
```

完整示例: `crates/luatos-log-ffi/examples/python/demo_ctypes.py`.

### 8.3 C# (P/Invoke)

```csharp
using System;
using System.Runtime.InteropServices;

class Program {
    [DllImport("luatos_log_ffi.dll", CallingConvention = CallingConvention.Cdecl)]
    static extern int luatos_soclog_analyze(
        byte[] data, uint len,
        byte[] outBuf,
        out ulong tick, out ushort sn,
        byte[] tagBuf,
        out byte cpu, out byte msgType,
        out uint cmd, out uint address);

    static void Main() {
        byte[] frame = /* ... 从串口读 ... */;
        byte[] outBuf = new byte[64 * 1024];
        byte[] tagBuf = new byte[64];

        int rc = luatos_soclog_analyze(
            frame, (uint)frame.Length, outBuf,
            out ulong tick, out ushort sn, tagBuf,
            out byte cpu, out byte type, out uint cmd, out uint address);

        if (rc == 0) {
            string outStr = System.Text.Encoding.UTF8.GetString(outBuf).TrimEnd('\0');
            string tagStr = System.Text.Encoding.UTF8.GetString(tagBuf).TrimEnd('\0');
            Console.WriteLine($"OK: tick={tick} sn={sn} tag={tagStr} out={outStr}");
        } else {
            Console.WriteLine($"FAILED: rc={rc}");
        }
    }
}
```

### 8.4 Go (cgo)

```go
package main

/*
#cgo CFLAGS: -I./include
#cgo LDFLAGS: -L./target/release -lluatos_log_ffi
#include "luatos_log.h"
#include <stdlib.h>
*/
import "C"
import "fmt"

func main() {
    // 假设 frame []byte 来自串口
    var frame [256]byte
    var frameLen C.uint32_t = 36

    out := make([]C.char, 64*1024)
    tag := make([]C.char, 64)
    var tick C.uint64_t
    var sn C.uint16_t
    var cpu, msgType C.uint8_t
    var cmd, address C.uint32_t

    rc := C.luatos_soclog_analyze(
        &frame[0], frameLen, &out[0],
        &tick, &sn, &tag[0],
        &cpu, &msgType, &cmd, &address,
    )

    if rc == 0 {
        outStr := C.GoString(&out[0])
        tagStr := C.GoString(&tag[0])
        fmt.Printf("OK: tick=%d sn=%d tag=%q out=%q\n", tick, sn, tagStr, outStr)
    } else {
        fmt.Printf("FAILED: rc=%d\n", rc)
    }
}
```

## 9. 编译与安装

### 9.1 下载发布包

从 [GitHub Releases](https://github.com/wendal/luatos-cli/releases) 下载对应平台:

| 平台 | 文件名 | 包含 |
|------|--------|------|
| Windows x64 | `luatos-log-ffi-x86_64-pc-windows-msvc.zip` | `luatos_log_ffi.dll`, `luatos_log.h`, `demo.c`, `demo_ctypes.py` |
| Linux x64 | `luatos-log-ffi-x86_64-unknown-linux-gnu.tar.gz` | `libluatos_log_ffi.so`, 同上 |
| macOS x64 | `luatos-log-ffi-x86_64-apple-darwin.tar.gz` | `libluatos_log_ffi.dylib`, 同上 |
| macOS arm64 | `luatos-log-ffi-aarch64-apple-darwin.tar.gz` | 同上 |

### 9.2 运行时配置

**Windows**: 把 `luatos_log_ffi.dll` 放到可执行文件同目录, 或加入 `PATH`.

**Linux**:
```bash
export LD_LIBRARY_PATH=/path/to/luatos_log_ffi:$LD_LIBRARY_PATH
./your_program
```
或安装到系统库目录:
```bash
sudo cp libluatos_log_ffi.so /usr/local/lib/
sudo ldconfig
```

**macOS**:
```bash
export DYLD_LIBRARY_PATH=/path/to/luatos_log_ffi:$DYLD_LIBRARY_PATH
./your_program
```

注意: macOS Gatekeeper 首次运行未签名 dylib 会弹警告, 处理:
```bash
xattr -d com.apple.quarantine libluatos_log_ffi.dylib
```

## 10. 调试

库内部不使用 `eprintln!` (避免污染 stdout), 静默运行. 调试 SOC 协议本身请参考
`docs/ccm4211-flash-protocol.md` 第 5 节"日志解码", 或使用 `luatos-cli log view-binary`
命令.

## 11. 已知限制

- **单帧入口**: 每次调用只解码一帧. 流式多帧需调用方自行维护 byte buffer,
  在攒够 0xA5...0xA5 后调用本函数. (库内部不跨调用保持状态, 与第三方 DLL 行为不同.)
- **帧长上限 8192 字节**: 超过此长度的帧会被判为 `LUATOS_SOCLOG_ERR_TRUNC`.
  Air1601 正常日志帧远低于此限制.
- **`address` 字段**: 日志帧固定为 0, 命令帧才有意义. 详见 §4.
- **非 printf 格式** (`type != 0`): 写入原始 body 字节 (按 UTF-8 解释), 不做参数还原.
- **不导出 LogDispatcher**: 文本日志解析 (`LuatosParser`/`BootLogParser`)
  不在本 FFI 范围, 仍是 Rust 内部 API.

## 12. 版本兼容

| FFI 版本 | luatos-log 版本 | 引入版本 | 备注 |
|----------|----------------|----------|------|
| 1.0.0 | 1.8.1+ | v1.9.0 | 初始发布 |

**ABI 稳定承诺**:
- 函数签名 (参数顺序/类型) 不变
- 错误码值不变
- 头文件 `luatos_log.h` 不变
- 新增函数/错误码只能追加, 不会修改或删除现有导出
- dylib/so/dylib 名称 (即 `luatos_log_ffi`) 永久不变
