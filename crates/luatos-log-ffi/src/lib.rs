//! C ABI 导出 — Air1601/CCM4211 SOC 二进制日志帧解码
//!
//! 详细文档参见 `docs/luatos-log-c-abi.md` 与 `include/luatos_log.h`.
//!
//! 主要导出:
//! - `luatos_soclog_analyze` (主名) 与 `pySoclogAnalyze` (别名, 兼容第三方 DLL)
//! - `luatos_soclog_version`
//!
//! 双模式 header:
//! - 日志帧 (`cmd == 0`): 文本日志, Out 写 `[device_time] L/module message`
//! - 命令帧 (`cmd != 0`): 原始 payload, Out 写 `len` 字节 raw bytes

use std::ffi::{c_char, c_int, c_uchar, c_uint, c_ulonglong, c_ushort};

use luatos_log::SocLogDecoder;
pub use luatos_log::{LogEntry, LogLevel, SocCmdFrame, SocFrame, SocLogFrameHeader};

// ─── 错误码 (与 include/luatos_log.h 保持一致) ──────────────────────────────

/// 成功
pub const LUATOS_SOCLOG_OK: c_int = 0;
/// CRC16 校验失败
pub const LUATOS_SOCLOG_ERR_CRC: c_int = -1;
/// header 长度不足 (< 26 字节)
pub const LUATOS_SOCLOG_ERR_HEADER: c_int = -2;
/// 输出 buffer 写不下 (保留码, 当前实现截断不报错)
pub const LUATOS_SOCLOG_ERR_BUFFER: c_int = -3;
/// 任一必填指针为 NULL
pub const LUATOS_SOCLOG_ERR_PARAM: c_int = -4;
/// 输入数据为空
pub const LUATOS_SOCLOG_ERR_EMPTY: c_int = -5;
/// 帧超过 8192 字节被截断
pub const LUATOS_SOCLOG_ERR_TRUNC: c_int = -6;

/// FFI 默认输出 buffer 上限 (字节, 含 NUL 终止)
const FFI_OUT_MAX: usize = 64 * 1024;

/// FFI tag buffer 上限 (字节, 含 NUL 终止)
const FFI_TAG_MAX: usize = 64;

// ─── 主入口 ──────────────────────────────────────────────────────────────────

/// 解码一帧 Air1601/CCM4211 SOC 二进制日志.
///
/// 完整协议说明见 `docs/ccm4211-flash-protocol.md` 与 `docs/luatos-log-c-abi.md`.
/// 行为与第三方 DLL 的 `pySoclogAnalyze` 完全兼容 (包括日志帧/命令帧双模式).
///
/// # Safety
///
/// - `data` 必须指向至少 `len` 字节的有效内存
/// - `out` 必须指向至少 `FFI_OUT_MAX` 字节的可写 buffer
/// - `tag` 必须指向至少 `FFI_TAG_MAX` 字节的可写 buffer
/// - `tick`/`sn`/`cpu`/`msg_type`/`cmd`/`address` 必须指向有效的可写 u64/u16/u8/u32 槽
#[no_mangle]
pub unsafe extern "C" fn luatos_soclog_analyze(
    data: *const c_uchar,
    len: c_uint,
    out: *mut c_char,
    tick: *mut c_ulonglong,
    sn: *mut c_ushort,
    tag: *mut c_char,
    cpu: *mut c_uchar,
    msg_type: *mut c_uchar,
    cmd: *mut c_uint,
    address: *mut c_uint,
) -> c_int {
    // 1. 必填指针校验
    if data.is_null() || out.is_null() || tick.is_null() || sn.is_null() || tag.is_null() || cpu.is_null() || msg_type.is_null() || cmd.is_null() || address.is_null() {
        return LUATOS_SOCLOG_ERR_PARAM;
    }
    if len == 0 {
        return LUATOS_SOCLOG_ERR_EMPTY;
    }

    // SAFETY: 调用方保证 data[..len] 有效
    let data_slice = unsafe { std::slice::from_raw_parts(data, len as usize) };

    // 2. 一次性解码一帧 (stateless)
    let frame = match SocLogDecoder::decode_one(data_slice) {
        Some(f) => f,
        None => return LUATOS_SOCLOG_ERR_CRC,
    };

    // 3. 按模式分支写输出
    match frame {
        SocFrame::Log(entry, header) => {
            // 数值输出
            unsafe {
                *tick = header.ms;
                *sn = header.sn;
                *cpu = header.cpu;
                *msg_type = header.msg_type;
                *cmd = header.cmd;
                *address = 0; // 日志帧无 address 字段
            }
            // tag: NUL-terminated 模块名
            let module = entry.module.as_deref().unwrap_or("");
            unsafe { write_nul_string(tag, module, FFI_TAG_MAX) };
            // out: [device_time] L/module message
            let line = format_log_line(&entry);
            unsafe { write_nul_string(out, &line, FFI_OUT_MAX) };
        }
        SocFrame::Cmd(c) => {
            // 数值输出
            unsafe {
                *tick = c.ms;
                *sn = c.sn;
                *cpu = c.cpu;
                *msg_type = c.msg_type;
                *cmd = c.cmd;
                *address = c.address;
            }
            // tag: 空字符串
            unsafe { *tag = 0 };
            // out: c.len 字节 raw payload, 截断到 FFI_OUT_MAX-1
            let copy = c.payload.len().min(FFI_OUT_MAX - 1);
            unsafe {
                std::ptr::copy_nonoverlapping(c.payload.as_ptr(), out as *mut u8, copy);
                *out.add(copy) = 0;
            }
        }
    }
    LUATOS_SOCLOG_OK
}

/// `pySoclogAnalyze` 别名 (与 `luatos_soclog_analyze` 完全相同), 兼容第三方 DLL 调用方.
///
/// 通过 `#[export_name]` 把 Rust 函数的链接名改为 `pySoclogAnalyze`, 现有 Python
/// `ctypes.CDLL("luatos_log_ffi").pySoclogAnalyze(...)` 用户可无感替换.
///
/// # Safety
///
/// 与 [`luatos_soclog_analyze`] 相同.
#[export_name = "pySoclogAnalyze"]
pub unsafe extern "C" fn py_soclog_analyze(
    data: *const c_uchar,
    len: c_uint,
    out: *mut c_char,
    tick: *mut c_ulonglong,
    sn: *mut c_ushort,
    tag: *mut c_char,
    cpu: *mut c_uchar,
    msg_type: *mut c_uchar,
    cmd: *mut c_uint,
    address: *mut c_uint,
) -> c_int {
    unsafe { luatos_soclog_analyze(data, len, out, tick, sn, tag, cpu, msg_type, cmd, address) }
}

/// 查询 FFI 库版本号 (ABI 0x0001_0000 = 1.0.0).
///
/// # Safety
///
/// `major`/`minor`/`patch` 必须指向有效的可写 u32 槽.
#[no_mangle]
pub unsafe extern "C" fn luatos_soclog_version(major: *mut c_uint, minor: *mut c_uint, patch: *mut c_uint) {
    if major.is_null() || minor.is_null() || patch.is_null() {
        return;
    }
    *major = 1;
    *minor = 0;
    *patch = 0;
}

// ─── 内部 helper ─────────────────────────────────────────────────────────────

/// 把 Rust 字符串写入 C 端 `dst`, 末尾追加 NUL. 截断到 `cap-1` 字节.
unsafe fn write_nul_string(dst: *mut c_char, s: &str, cap: usize) {
    let bytes = s.as_bytes();
    // 先按字节截断到 cap-1, 再用 floor_char_boundary 回退到 UTF-8 字符边界,
    // 避免切断多字节字符导致 C 端收到无效 UTF-8
    let n = s.floor_char_boundary(bytes.len().min(cap.saturating_sub(1)));
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst as *mut u8, n);
    *dst.add(n) = 0;
}

fn format_log_line(e: &LogEntry) -> String {
    let lvl = e.level.to_string();
    let mod_ = e.module.as_deref().unwrap_or("-");
    let dt = e.device_time.as_deref().unwrap_or("-");
    format!("[{}] {}/{} {}", dt, lvl, mod_, e.message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{c_char, CStr};

    #[test]
    fn write_nul_string_keeps_short_string_intact() {
        let mut buf = [0u8; 32];
        let s = "你好，LuatOS";
        unsafe { write_nul_string(buf.as_mut_ptr() as *mut c_char, s, buf.len()) };
        let out = unsafe { CStr::from_ptr(buf.as_ptr() as *const c_char) };
        assert_eq!(out.to_str().unwrap(), s, "短字符串应原样写入");
        assert_eq!(buf[s.len()], 0, "末尾应追加 NUL 终止");
    }

    #[test]
    fn write_nul_string_truncates_on_char_boundary() {
        // cap=16 会把 19 字节的字符串截到 15 字节, 恰好落在多字节字符中间,
        // 应回退到完整字符边界 "abcdefg日志" (13 字节) 并以 NUL 结尾
        let mut buf = [0xFFu8; 32];
        let s = "abcdefg日志信息";
        unsafe { write_nul_string(buf.as_mut_ptr() as *mut c_char, s, 16) };
        let end = buf.iter().position(|&b| b == 0).unwrap();
        let out = std::str::from_utf8(&buf[..end]).unwrap();
        assert_eq!(out, "abcdefg日志", "截断必须停在完整字符边界");
        assert!(out.ends_with('志'), "输出必须以完整字符结尾");
        assert_eq!(end, 13);
        assert_eq!(buf[13], 0, "末尾应追加 NUL 终止");
    }
}
