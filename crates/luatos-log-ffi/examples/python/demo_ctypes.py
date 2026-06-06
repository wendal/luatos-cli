"""demo_ctypes.py — luatos-log-ffi Python ctypes 调用示例

演示如何用 ctypes 加载 luatos_log_ffi 共享库, 构造一帧 SOC 日志并解码.

运行:
    Linux:   LD_LIBRARY_PATH=target/release python3 demo_ctypes.py
    macOS:   DYLD_LIBRARY_PATH=target/release python3 demo_ctypes.py
    Windows: (在 dll 同目录直接运行) python demo_ctypes.py
"""

import ctypes
import struct
from ctypes import c_uint8, c_uint16, c_uint32, c_uint64, c_char_p, POINTER, byref, create_string_buffer


def crc16_soc_log(data: bytes) -> int:
    """与 luatos-log 内部一致的 CRC16 (init=0, poly=0xA001, 查表算法)."""
    # 256 项查表, 与 luatos_log::SOC_LOG_CRC_TABLE 一致
    table = [
        0x0000, 0xC0C1, 0xC181, 0x0140, 0xC301, 0x03C0, 0x0280, 0xC241,
        0xC601, 0x06C0, 0x0780, 0xC741, 0x0500, 0xC5C1, 0xC481, 0x0440,
        0xCC01, 0x0CC0, 0x0D80, 0xCD41, 0x0F00, 0xCFC1, 0xCE81, 0x0E40,
        0x0A00, 0xCAC1, 0xCB81, 0x0B40, 0xC901, 0x09C0, 0x0880, 0xC841,
        0xD801, 0x18C0, 0x1980, 0xD941, 0x1B00, 0xDBC1, 0xDA81, 0x1A40,
        0x1E00, 0xDEC1, 0xDF81, 0x1F00, 0xDD01, 0x1DC0, 0x1C80, 0xDC41,
        0x1400, 0xD4C1, 0xD581, 0x1540, 0xD701, 0x17C0, 0x1680, 0xD641,
        0xD201, 0x12C0, 0x1380, 0xD341, 0x1100, 0xD1C1, 0xD081, 0x1040,
        0xF001, 0x30C0, 0x3180, 0xF141, 0x3300, 0xF3C1, 0xF281, 0x3240,
        0x3600, 0xF6C1, 0xF781, 0x3740, 0xF501, 0x35C0, 0x3480, 0xF441,
        0x3C00, 0xFCC1, 0xFD81, 0x3D40, 0xFF01, 0x3FC0, 0x3E80, 0xFE41,
        0xFA01, 0x3AC0, 0x3B80, 0xFB41, 0x3900, 0xF9C1, 0xF881, 0x3840,
        0x2800, 0xE8C1, 0xE981, 0x2940, 0xEB01, 0x2BC0, 0x2A80, 0xEA41,
        0xEE01, 0x2EC0, 0x2F80, 0xEF41, 0x2D00, 0xEDC1, 0xEC81, 0x2C40,
        0xE401, 0x24C0, 0x2580, 0xE541, 0x2700, 0xE7C1, 0xE681, 0x2640,
        0x2200, 0xE2C1, 0xE381, 0x2340, 0xE101, 0x21C0, 0x2080, 0xE041,
        0xA001, 0x60C0, 0x6180, 0xA141, 0x6300, 0xA3C1, 0xA281, 0x6240,
        0x6600, 0xA6C1, 0xA781, 0x6740, 0xA501, 0x65C0, 0x6480, 0xA441,
        0x6C00, 0xACC1, 0xAD81, 0x6D40, 0xAF01, 0x6FC0, 0x6E80, 0xAE41,
        0xAA01, 0x6AC0, 0x6B80, 0xAB41, 0x6900, 0xA9C1, 0xA881, 0x6840,
        0x7800, 0xB8C1, 0xB981, 0x7940, 0xBB01, 0x7BC0, 0x7A80, 0xBA41,
        0xBE01, 0x7EC0, 0x7F80, 0xBF41, 0x7D00, 0xBDC1, 0xBC81, 0x7C40,
        0xB401, 0x74C0, 0x7580, 0xB541, 0x7700, 0xB7C1, 0xB681, 0x7640,
        0x7200, 0xB2C1, 0xB381, 0x7340, 0xB101, 0x71C0, 0x7080, 0xB041,
        0x5000, 0x90C1, 0x9181, 0x5140, 0x9301, 0x53C0, 0x5280, 0x9241,
        0x9601, 0x56C0, 0x5780, 0x9741, 0x5500, 0x95C1, 0x9481, 0x5440,
        0x9C01, 0x5CC0, 0x5D80, 0x9D41, 0x5F00, 0x9FC1, 0x9E81, 0x5E40,
        0x5A00, 0x9AC1, 0x9B81, 0x5B40, 0x9901, 0x59C0, 0x5880, 0x9841,
        0x8801, 0x48C0, 0x4980, 0x8941, 0x4B00, 0x8BC1, 0x8A81, 0x4A40,
        0x4E00, 0x8EC1, 0x8F81, 0x4F40, 0x8D01, 0x4DC0, 0x4C80, 0x8C41,
        0x4400, 0x84C1, 0x8581, 0x4540, 0x8701, 0x47C0, 0x4680, 0x8641,
        0x8201, 0x42C0, 0x4380, 0x8341, 0x4100, 0x81C1, 0x8081, 0x4040,
    ]
    crc = 0
    for b in data:
        x = (b ^ crc) & 0xFF
        crc = (crc >> 8) ^ table[x]
    return crc & 0xFFFF


def push_escaped(buf: bytearray, b: int):
    """处理 0xA5/0xA6 转义."""
    if b == 0xA5:
        buf.extend(b"\xA6\x01")
    elif b == 0xA6:
        buf.extend(b"\xA6\x02")
    else:
        buf.append(b)


def build_log_frame(ms: int, level: int, sn: int, body: str) -> bytearray:
    """构造一个日志帧 (cmd=0)."""
    # 24B header + body (4 字节对齐)
    body_bytes = body.encode("utf-8") + b"\x00"  # NUL 终止
    while len(body_bytes) % 4 != 0:
        body_bytes += b"\x00"

    payload = bytearray(24)
    # ms
    struct.pack_into("<Q", payload, 0, ms)
    # tag: level 在 byte 8 (低位), module tag 暂时空
    payload[8] = level & 0xFF
    # cmd = 0
    # sn
    struct.pack_into("<H", payload, 20, sn)
    # type = 0 (printf), cpu = 0

    payload.extend(body_bytes)
    crc = crc16_soc_log(bytes(payload))
    payload.extend(struct.pack("<H", crc))

    # 0xA5 framing + 转义
    frame = bytearray(b"\xA5")
    for b in payload:
        push_escaped(frame, b)
    frame.append(0xA5)
    return frame


def main():
    # 1. 加载共享库
    import sys
    import os

    # Windows 需要显式指定 dll 目录, 否则 ctypes 找不到
    if sys.platform == "win32":
        # 优先查找脚本所在目录的 dll, 然后 PATH, 然后 target/release
        script_dir = os.path.dirname(os.path.abspath(__file__))
        candidates = [
            os.path.join(script_dir, "luatos_log_ffi.dll"),
            os.path.join(os.getcwd(), "luatos_log_ffi.dll"),
        ]
        for c in candidates:
            if os.path.exists(c):
                # 添加 dll 所在目录到 PATH (解决依赖查找)
                os.environ["PATH"] = os.path.dirname(c) + os.pathsep + os.environ.get("PATH", "")
                lib = ctypes.CDLL(c)
                break
        else:
            lib = ctypes.CDLL("luatos_log_ffi.dll")
    elif sys.platform == "darwin":
        lib = ctypes.CDLL("libluatos_log_ffi.dylib")
    else:
        lib = ctypes.CDLL("libluatos_log_ffi.so")

    # 2. 设置函数签名
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

    VERSION = lib.luatos_soclog_version
    VERSION.argtypes = [POINTER(c_uint32), POINTER(c_uint32), POINTER(c_uint32)]
    VERSION.restype = None

    # 3. 查询版本
    major, minor, patch = c_uint32(), c_uint32(), c_uint32()
    VERSION(byref(major), byref(minor), byref(patch))
    print(f"luatos-log-ffi version {major.value}.{minor.value}.{patch.value}")

    # 4. 构造测试帧
    frame = build_log_frame(ms=5000, level=2, sn=42, body="Hello from Python!")
    print(f"Built {len(frame)} byte frame")

    # 5. 调用主入口
    data = (c_uint8 * len(frame)).from_buffer_copy(bytes(frame))
    out = create_string_buffer(64 * 1024)
    tag = create_string_buffer(64)
    tick, sn_v = c_uint64(), c_uint16()
    cpu_v, type_v = c_uint8(), c_uint8()
    cmd_v, addr_v = c_uint32(), c_uint32()

    rc = ANALYZE(
        data, len(frame), out,
        byref(tick), byref(sn_v), tag,
        byref(cpu_v), byref(type_v),
        byref(cmd_v), byref(addr_v),
    )

    if rc == 0:  # LUATOS_SOCLOG_OK
        print(f"OK: tick={tick.value} sn={sn_v.value} cpu={cpu_v.value} "
              f"type={type_v.value} cmd={cmd_v.value} address=0x{addr_v.value:08x}")
        print(f"    tag='{tag.value.decode()}'")
        print(f"    out='{out.value.decode()}'")
    elif rc == -1:
        print("CRC error")
    elif rc == -2:
        print("Header too short")
    elif rc == -4:
        print("NULL parameter")
    elif rc == -5:
        print("Empty data")
    else:
        print(f"Error code: {rc}")

    # 6. 用别名再试一次
    ALIAS = lib.pySoclogAnalyze
    ALIAS.argtypes = ANALYZE.argtypes
    ALIAS.restype = ANALYZE.restype
    rc2 = ALIAS(
        data, len(frame), out,
        byref(tick), byref(sn_v), tag,
        byref(cpu_v), byref(type_v),
        byref(cmd_v), byref(addr_v),
    )
    assert rc2 == 0, "pySoclogAnalyze should also succeed"
    print("pySoclogAnalyze alias works.")


if __name__ == "__main__":
    main()
