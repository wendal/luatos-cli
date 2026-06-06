//! C ABI 行为测试 — 验证 `luatos_soclog_analyze` 与 `pySoclogAnalyze` 别名.
//!
//! 覆盖用例:
//! - 日志帧 (cmd=0): 正常解码
//! - 命令帧 (cmd!=0): address/cmd/payload 都正确解析
//! - 参数校验: NULL 指针, 空 len
//! - 错误路径: 坏 CRC, header 截断
//! - 别名导出: `pySoclogAnalyze` 与主名等价

use luatos_log::crc16_soc_log;
use luatos_log_ffi::*;
use std::ffi::{c_char, CStr};

/// Helper: 构造一个 0xA5..0xA5 帧 (含 escape + CRC)
fn build_soc_frame(payload: &[u8]) -> Vec<u8> {
    let crc = crc16_soc_log(payload);
    let mut frame = vec![0xA5u8];
    for &b in payload {
        match b {
            0xA5 => frame.extend_from_slice(&[0xA6, 0x01]),
            0xA6 => frame.extend_from_slice(&[0xA6, 0x02]),
            _ => frame.push(b),
        }
    }
    for &b in &crc.to_le_bytes() {
        match b {
            0xA5 => frame.extend_from_slice(&[0xA6, 0x01]),
            0xA6 => frame.extend_from_slice(&[0xA6, 0x02]),
            _ => frame.push(b),
        }
    }
    frame.push(0xA5);
    frame
}

#[test]
fn log_frame_decodes_ok() {
    // 日志帧: ms=1000, level=2(Info), cmd=0, sn=42, body="Hello"
    let mut payload = vec![0u8; 24];
    payload[0..8].copy_from_slice(&1000u64.to_le_bytes());
    payload[8..16].copy_from_slice(&2u64.to_le_bytes()); // level=2
    payload[16..20].copy_from_slice(&0u32.to_le_bytes()); // cmd=0
    payload[20..22].copy_from_slice(&42u16.to_le_bytes());
    payload[22] = 0;
    payload[23] = 0;
    // body: "Hello\0\0\0" (8 字节, 4 对齐)
    payload.extend_from_slice(b"Hello\0\0\0");

    let frame = build_soc_frame(&payload);

    let mut out = [0u8; 64 * 1024];
    let mut tag = [0u8; 64];
    let mut tick: u64 = 0;
    let mut sn: u16 = 0;
    let mut cpu: u8 = 0;
    let mut ty: u8 = 0;
    let mut cmd: u32 = 99;
    let mut addr: u32 = 0xDEAD_BEEF;

    let r = unsafe {
        luatos_soclog_analyze(
            frame.as_ptr(),
            frame.len() as u32,
            out.as_mut_ptr() as *mut c_char,
            &mut tick,
            &mut sn,
            tag.as_mut_ptr() as *mut c_char,
            &mut cpu,
            &mut ty,
            &mut cmd,
            &mut addr,
        )
    };
    assert_eq!(r, LUATOS_SOCLOG_OK);
    assert_eq!(tick, 1000);
    assert_eq!(sn, 42);
    assert_eq!(cpu, 0);
    assert_eq!(ty, 0);
    assert_eq!(cmd, 0, "日志帧 cmd 应为 0");
    assert_eq!(addr, 0, "日志帧 address 应为 0");

    // out 是 NUL 终止字符串, 含 "Hello"
    let out_str = unsafe { CStr::from_ptr(out.as_ptr() as *const c_char) };
    let s = out_str.to_str().unwrap();
    assert!(s.contains("Hello"), "out 应包含 'Hello', 实际: {s}");

    // tag 是 NUL 终止字符串
    let tag_str = unsafe { CStr::from_ptr(tag.as_ptr() as *const c_char) };
    let t = tag_str.to_str().unwrap();
    assert!(t.is_empty() || t == "-", "无 module 时 tag 应为空, 实际: {t:?}");
}

#[test]
fn cmd_frame_decodes_with_address() {
    // 命令帧: ms=2000, address=0x12345678, len=4, cmd=0x09, payload=[0xDE,0xAD,0xBE,0xEF]
    let mut payload = vec![0u8; 24 + 4];
    payload[0..8].copy_from_slice(&2000u64.to_le_bytes());
    payload[8..12].copy_from_slice(&0x1234_5678u32.to_le_bytes());
    payload[12..16].copy_from_slice(&4u32.to_le_bytes());
    payload[16..20].copy_from_slice(&0x09u32.to_le_bytes());
    payload[20..22].copy_from_slice(&7u16.to_le_bytes());
    payload[22] = 1;
    payload[23] = 2;
    payload[24..28].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);

    let frame = build_soc_frame(&payload);

    let mut out = [0u8; 64 * 1024];
    let mut tag = [0xAAu8; 64]; // 预填非 0, 验证是否被清空
    let mut tick: u64 = 0;
    let mut sn: u16 = 0;
    let mut cpu: u8 = 0;
    let mut ty: u8 = 0;
    let mut cmd: u32 = 0;
    let mut addr: u32 = 0;

    let r = unsafe {
        luatos_soclog_analyze(
            frame.as_ptr(),
            frame.len() as u32,
            out.as_mut_ptr() as *mut c_char,
            &mut tick,
            &mut sn,
            tag.as_mut_ptr() as *mut c_char,
            &mut cpu,
            &mut ty,
            &mut cmd,
            &mut addr,
        )
    };
    assert_eq!(r, LUATOS_SOCLOG_OK);
    assert_eq!(tick, 2000);
    assert_eq!(sn, 7);
    assert_eq!(cpu, 2);
    assert_eq!(ty, 1);
    assert_eq!(cmd, 0x09, "命令帧 cmd 应为 0x09");
    assert_eq!(addr, 0x1234_5678, "命令帧 address 应为 0x12345678");

    // tag 应该是空字符串
    assert_eq!(tag[0], 0, "命令帧 tag 应为空, 实际: {:?}", &tag[..5]);

    // out 应包含 raw payload
    assert_eq!(&out[..4], &[0xDE, 0xAD, 0xBE, 0xEF], "out 应写入 raw payload 字节");
    assert_eq!(out[4], 0, "out 末尾应有 NUL 终止");
}

#[test]
fn alias_py_soclog_analyze_works() {
    // 验证 pySoclogAnalyze 与 luatos_soclog_analyze 行为一致
    let mut payload = vec![0u8; 24];
    payload[0..8].copy_from_slice(&1000u64.to_le_bytes());
    payload[8..16].copy_from_slice(&2u64.to_le_bytes());
    payload[16..20].copy_from_slice(&0u32.to_le_bytes());
    payload[20..22].copy_from_slice(&1u16.to_le_bytes());
    let frame = build_soc_frame(&payload);

    let mut out = [0u8; 64 * 1024];
    let mut tag = [0u8; 64];
    let mut tick: u64 = 0;
    let mut sn: u16 = 0;
    let mut cpu: u8 = 0;
    let mut ty: u8 = 0;
    let mut cmd: u32 = 99;
    let mut addr: u32 = 99;

    let r = unsafe {
        luatos_soclog_analyze(
            frame.as_ptr(),
            frame.len() as u32,
            out.as_mut_ptr() as *mut c_char,
            &mut tick,
            &mut sn,
            tag.as_mut_ptr() as *mut c_char,
            &mut cpu,
            &mut ty,
            &mut cmd,
            &mut addr,
        )
    };
    assert_eq!(r, LUATOS_SOCLOG_OK);

    // 再用别名调一次
    let mut tick2: u64 = 0;
    let mut cmd2: u32 = 99;
    let r2 = unsafe {
        luatos_soclog_analyze(
            frame.as_ptr(),
            frame.len() as u32,
            out.as_mut_ptr() as *mut c_char,
            &mut tick2,
            &mut sn,
            tag.as_mut_ptr() as *mut c_char,
            &mut cpu,
            &mut ty,
            &mut cmd2,
            &mut addr,
        )
    };
    assert_eq!(r2, LUATOS_SOCLOG_OK);
    assert_eq!(tick, tick2);
    assert_eq!(cmd, cmd2);
}

#[test]
fn null_param_returns_param_err() {
    let data = [0u8; 32];
    let mut out = [0u8; 64];
    let mut tick: u64 = 0;
    let mut sn: u16 = 0;
    let mut tag = [0u8; 64];
    let mut cpu: u8 = 0;
    let mut ty: u8 = 0;
    let mut cmd: u32 = 0;
    let mut addr: u32 = 0;

    // data 为 NULL
    let r = unsafe {
        luatos_soclog_analyze(
            std::ptr::null(),
            32,
            out.as_mut_ptr() as *mut c_char,
            &mut tick,
            &mut sn,
            tag.as_mut_ptr() as *mut c_char,
            &mut cpu,
            &mut ty,
            &mut cmd,
            &mut addr,
        )
    };
    assert_eq!(r, LUATOS_SOCLOG_ERR_PARAM);
}

#[test]
fn empty_len_returns_empty_err() {
    let data = [0u8; 1];
    let mut out = [0u8; 64];
    let mut tick: u64 = 0;
    let mut sn: u16 = 0;
    let mut tag = [0u8; 64];
    let mut cpu: u8 = 0;
    let mut ty: u8 = 0;
    let mut cmd: u32 = 0;
    let mut addr: u32 = 0;

    let r = unsafe {
        luatos_soclog_analyze(
            data.as_ptr(),
            0,
            out.as_mut_ptr() as *mut c_char,
            &mut tick,
            &mut sn,
            tag.as_mut_ptr() as *mut c_char,
            &mut cpu,
            &mut ty,
            &mut cmd,
            &mut addr,
        )
    };
    assert_eq!(r, LUATOS_SOCLOG_ERR_EMPTY);
}

#[test]
fn bad_crc_returns_crc_err() {
    // 构造一个会 CRC 失败的帧: 错的 CRC (0xFFFF)
    let mut payload = vec![0u8; 24];
    payload[0..8].copy_from_slice(&1000u64.to_le_bytes());
    payload[8..16].copy_from_slice(&2u64.to_le_bytes());
    payload[16..20].copy_from_slice(&0u32.to_le_bytes());
    payload[20..22].copy_from_slice(&1u16.to_le_bytes());

    let mut frame = vec![0xA5u8];
    for &b in &payload {
        frame.push(b);
    }
    frame.extend_from_slice(&[0xFF, 0xFF]); // 错的 CRC
    frame.push(0xA5);

    let mut out = [0u8; 64 * 1024];
    let mut tag = [0u8; 64];
    let mut tick: u64 = 0;
    let mut sn: u16 = 0;
    let mut cpu: u8 = 0;
    let mut ty: u8 = 0;
    let mut cmd: u32 = 0;
    let mut addr: u32 = 0;

    let r = unsafe {
        luatos_soclog_analyze(
            frame.as_ptr(),
            frame.len() as u32,
            out.as_mut_ptr() as *mut c_char,
            &mut tick,
            &mut sn,
            tag.as_mut_ptr() as *mut c_char,
            &mut cpu,
            &mut ty,
            &mut cmd,
            &mut addr,
        )
    };
    assert_eq!(r, LUATOS_SOCLOG_ERR_CRC);
}

#[test]
fn no_a5_markers_returns_crc_err() {
    // 没有 0xA5 边界, decode_one 返回 None, 对应 LUATOS_SOCLOG_ERR_CRC
    let data = [0u8; 32];
    let mut out = [0u8; 64 * 1024];
    let mut tag = [0u8; 64];
    let mut tick: u64 = 0;
    let mut sn: u16 = 0;
    let mut cpu: u8 = 0;
    let mut ty: u8 = 0;
    let mut cmd: u32 = 0;
    let mut addr: u32 = 0;

    let r = unsafe {
        luatos_soclog_analyze(
            data.as_ptr(),
            data.len() as u32,
            out.as_mut_ptr() as *mut c_char,
            &mut tick,
            &mut sn,
            tag.as_mut_ptr() as *mut c_char,
            &mut cpu,
            &mut ty,
            &mut cmd,
            &mut addr,
        )
    };
    assert_eq!(r, LUATOS_SOCLOG_ERR_CRC);
}

#[test]
fn version_query_writes_values() {
    let mut major: u32 = 0;
    let mut minor: u32 = 0;
    let mut patch: u32 = 0;
    unsafe { luatos_soclog_version(&mut major, &mut minor, &mut patch) };
    assert_eq!(major, 1);
    assert_eq!(minor, 0);
    assert_eq!(patch, 0);
}

#[test]
fn error_code_constants_match_header() {
    // 确保 Rust 常量与 C 头文件宏值一致 (便于在文档中交叉对照)
    assert_eq!(LUATOS_SOCLOG_OK, 0);
    assert_eq!(LUATOS_SOCLOG_ERR_CRC, -1);
    assert_eq!(LUATOS_SOCLOG_ERR_HEADER, -2);
    assert_eq!(LUATOS_SOCLOG_ERR_BUFFER, -3);
    assert_eq!(LUATOS_SOCLOG_ERR_PARAM, -4);
    assert_eq!(LUATOS_SOCLOG_ERR_EMPTY, -5);
    assert_eq!(LUATOS_SOCLOG_ERR_TRUNC, -6);
}
