//! LuatOS 协议编解码基础算法。
//!
//! 提供 Beken CRC16 校验，以及脚本分区 / FOTA 包所用的「32 字节数据 + 2 字节 CRC」
//! 帧化与解帧算法。本 crate 为纯 Rust 实现，零外部依赖，与 `luatos-luadb`、
//! `luatos-soc` 中的旧实现逐字节兼容。

/// 单个帧内有效数据块的长度（字节）。
pub const BK_FRAME_DATA_LEN: usize = 32;

/// 单个帧的总长度：32 字节数据加 2 字节 CRC16。
pub const BK_FRAME_TOTAL_LEN: usize = 34;

/// 计算 Beken CRC16 校验值。
///
/// 以 `u32` 的 `0xFFFF_FFFF` 为初值，多项式 `0x8005`，按大端逐位处理：
/// 每个字节先与 `crc` 的高字节异或，再重复 8 次「左移 1 位，若移位前
/// `crc & 0x8000 != 0` 则额外异或 `0x8005`」。最终返回 `crc` 的低 16 位。
pub fn bk_crc16(data: &[u8]) -> u16 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= (b as u32) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x8005 } else { crc << 1 };
        }
    }
    (crc & 0xFFFF) as u16
}

/// 将裸数据按 Beken 规则帧化。
///
/// 每 [`BK_FRAME_DATA_LEN`] 字节为一块，尾部不足一整块时用 `0xFF` 补齐；
/// 对补齐后的 32 字节块计算 [`bk_crc16`]，依次输出
/// `[32 字节数据][CRC 高字节][CRC 低字节]`（CRC 为大端序）。
/// 空输入返回空 `Vec`；输出长度恒为 `data.len().div_ceil(32) * 34`。
pub fn bk_crc_frame(data: &[u8]) -> Vec<u8> {
    let blocks = data.len().div_ceil(BK_FRAME_DATA_LEN);
    let mut out = Vec::with_capacity(blocks * BK_FRAME_TOTAL_LEN);
    for chunk in data.chunks(BK_FRAME_DATA_LEN) {
        let mut block = [0xFFu8; BK_FRAME_DATA_LEN];
        block[..chunk.len()].copy_from_slice(chunk);
        let crc = bk_crc16(&block);
        out.extend_from_slice(&block);
        out.push((crc >> 8) as u8); // 大端：高字节在前
        out.push(crc as u8);
    }
    out
}

/// 解除 Beken CRC16 帧化，恢复裸数据。
///
/// 每 [`BK_FRAME_TOTAL_LEN`] 字节取前 32 字节数据，丢弃末尾 2 字节 CRC；
/// 尾部不足一整帧的残帧整体丢弃（合法的帧化输入长度恒为 34 的整数倍）。
/// 注意：帧化时对尾块的 `0xFF` 补齐会一并保留在解帧结果中，因此非 32 对齐
/// 的输入解帧后长度为其向上取整到 32 的倍数。
pub fn bk_crc_deframe(framed: &[u8]) -> Vec<u8> {
    let groups = framed.len() / BK_FRAME_TOTAL_LEN;
    let mut out = Vec::with_capacity(groups * BK_FRAME_DATA_LEN);
    let mut pos = 0usize;
    while pos + BK_FRAME_TOTAL_LEN <= framed.len() {
        out.extend_from_slice(&framed[pos..pos + BK_FRAME_DATA_LEN]);
        pos += BK_FRAME_TOTAL_LEN;
    }
    out
}

/// CRC16-Modbus（又称 SOC log CRC16）查表法所用的 256 项反射查找表。
///
/// 对应反射多项式 `0xA001`（`0x8005` 的位反转）；表项 `i` 为以 `i` 为 16 位初值、
/// 右移 8 次逐位迭代得到的余数。设备侧初值为 `0`（等效 CRC-16/ARC 变体），
/// 与标准 CRC-16/MODBUS 的 `0xFFFF` 初值不同，但二者共用同一张表。
pub const CRC16_MODBUS_TABLE: [u16; 256] = [
    0x0000, 0xC0C1, 0xC181, 0x0140, 0xC301, 0x03C0, 0x0280, 0xC241, 0xC601, 0x06C0, 0x0780, 0xC741, 0x0500, 0xC5C1, 0xC481, 0x0440, 0xCC01, 0x0CC0, 0x0D80, 0xCD41, 0x0F00, 0xCFC1,
    0xCE81, 0x0E40, 0x0A00, 0xCAC1, 0xCB81, 0x0B40, 0xC901, 0x09C0, 0x0880, 0xC841, 0xD801, 0x18C0, 0x1980, 0xD941, 0x1B00, 0xDBC1, 0xDA81, 0x1A40, 0x1E00, 0xDEC1, 0xDF81, 0x1F40,
    0xDD01, 0x1DC0, 0x1C80, 0xDC41, 0x1400, 0xD4C1, 0xD581, 0x1540, 0xD701, 0x17C0, 0x1680, 0xD641, 0xD201, 0x12C0, 0x1380, 0xD341, 0x1100, 0xD1C1, 0xD081, 0x1040, 0xF001, 0x30C0,
    0x3180, 0xF141, 0x3300, 0xF3C1, 0xF281, 0x3240, 0x3600, 0xF6C1, 0xF781, 0x3740, 0xF501, 0x35C0, 0x3480, 0xF441, 0x3C00, 0xFCC1, 0xFD81, 0x3D40, 0xFF01, 0x3FC0, 0x3E80, 0xFE41,
    0xFA01, 0x3AC0, 0x3B80, 0xFB41, 0x3900, 0xF9C1, 0xF881, 0x3840, 0x2800, 0xE8C1, 0xE981, 0x2940, 0xEB01, 0x2BC0, 0x2A80, 0xEA41, 0xEE01, 0x2EC0, 0x2F80, 0xEF41, 0x2D00, 0xEDC1,
    0xEC81, 0x2C40, 0xE401, 0x24C0, 0x2580, 0xE541, 0x2700, 0xE7C1, 0xE681, 0x2640, 0x2200, 0xE2C1, 0xE381, 0x2340, 0xE101, 0x21C0, 0x2080, 0xE041, 0xA001, 0x60C0, 0x6180, 0xA141,
    0x6300, 0xA3C1, 0xA281, 0x6240, 0x6600, 0xA6C1, 0xA781, 0x6740, 0xA501, 0x65C0, 0x6480, 0xA441, 0x6C00, 0xACC1, 0xAD81, 0x6D40, 0xAF01, 0x6FC0, 0x6E80, 0xAE41, 0xAA01, 0x6AC0,
    0x6B80, 0xAB41, 0x6900, 0xA9C1, 0xA881, 0x6840, 0x7800, 0xB8C1, 0xB981, 0x7940, 0xBB01, 0x7BC0, 0x7A80, 0xBA41, 0xBE01, 0x7EC0, 0x7F80, 0xBF41, 0x7D00, 0xBDC1, 0xBC81, 0x7C40,
    0xB401, 0x74C0, 0x7580, 0xB541, 0x7700, 0xB7C1, 0xB681, 0x7640, 0x7200, 0xB2C1, 0xB381, 0x7340, 0xB101, 0x71C0, 0x7080, 0xB041, 0x5000, 0x90C1, 0x9181, 0x5140, 0x9301, 0x53C0,
    0x5280, 0x9241, 0x9601, 0x56C0, 0x5780, 0x9741, 0x5500, 0x95C1, 0x9481, 0x5440, 0x9C01, 0x5CC0, 0x5D80, 0x9D41, 0x5F00, 0x9FC1, 0x9E81, 0x5E40, 0x5A00, 0x9AC1, 0x9B81, 0x5B40,
    0x9901, 0x59C0, 0x5880, 0x9841, 0x8801, 0x48C0, 0x4980, 0x8941, 0x4B00, 0x8BC1, 0x8A81, 0x4A40, 0x4E00, 0x8EC1, 0x8F81, 0x4F40, 0x8D01, 0x4DC0, 0x4C80, 0x8C41, 0x4400, 0x84C1,
    0x8581, 0x4540, 0x8701, 0x47C0, 0x4680, 0x8641, 0x8201, 0x42C0, 0x4380, 0x8341, 0x4100, 0x81C1, 0x8081, 0x4040,
];

/// 以查表法计算 CRC16-Modbus（SOC log CRC16）校验值。
///
/// 初值为 `0`；逐字节以「当前字节异或 `crc` 低 8 位」为下标查
/// [`CRC16_MODBUS_TABLE`]，将 `crc` 右移 8 位后与表项异或，末尾不做额外异或。
/// 空输入返回 `0`。与 `luatos-log` / `luatos-flash` 中收敛前的旧实现逐字节兼容。
pub fn crc16_modbus(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &byte in data {
        let idx = (byte ^ (crc as u8)) as usize;
        crc = (crc >> 8) ^ CRC16_MODBUS_TABLE[idx];
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::{bk_crc16, bk_crc_deframe, bk_crc_frame, crc16_modbus, BK_FRAME_DATA_LEN, BK_FRAME_TOTAL_LEN};

    /// 旧算法的逐位等价 oracle，独立转录用于交叉验证。
    fn oracle_crc16(data: &[u8]) -> u16 {
        let mut crc: u32 = 0xFFFF_FFFF;
        for &b in data {
            crc ^= (b as u32) << 8;
            for _ in 0..8 {
                if crc & 0x8000 != 0 {
                    crc = (crc << 1) ^ 0x8005;
                } else {
                    crc <<= 1;
                }
            }
        }
        (crc & 0xFFFF) as u16
    }

    /// 旧版 `add_bk_crc` / `add_bk_crc_to_data` 的等价 oracle。
    fn oracle_frame(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut pos = 0usize;
        while pos < data.len() {
            let end = (pos + 32).min(data.len());
            let mut block = [0xFFu8; 32];
            block[..end - pos].copy_from_slice(&data[pos..end]);
            let crc = oracle_crc16(&block);
            out.extend_from_slice(&block);
            out.extend_from_slice(&crc.to_be_bytes());
            pos = end;
        }
        out
    }

    /// 旧版 `bk72xx_deframe_framed` 的等价 oracle。
    fn oracle_deframe(framed: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut pos = 0usize;
        while pos + 34 <= framed.len() {
            out.extend_from_slice(&framed[pos..pos + 32]);
            pos += 34;
        }
        out
    }

    /// CRC16-Modbus（init=0、反射多项式 0xA001）的逐位参考实现，
    /// 不依赖查表，用于与 [`crc16_modbus`] 交叉验证。
    fn oracle_crc16_modbus(data: &[u8]) -> u16 {
        let mut crc: u16 = 0;
        for &b in data {
            crc ^= b as u16;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xA001 } else { crc >> 1 };
            }
        }
        crc
    }

    /// 生成长度为 `n` 的确定性测试数据。
    fn sample(n: usize) -> Vec<u8> {
        (0..n).map(|i| ((i * 37 + 11) % 256) as u8).collect()
    }

    #[test]
    fn algorithms_are_deterministic() {
        let data = sample(70);
        assert_eq!(bk_crc16(&data), bk_crc16(&data));
        let framed = bk_crc_frame(&data);
        assert_eq!(framed, bk_crc_frame(&data));
        assert_eq!(bk_crc_deframe(&framed), bk_crc_deframe(&framed));
    }

    #[test]
    fn frame_and_deframe_length_formula() {
        for &n in &[0usize, 33, 64, 100] {
            let data = sample(n);
            let framed = bk_crc_frame(&data);
            assert_eq!(framed.len(), n.div_ceil(BK_FRAME_DATA_LEN) * BK_FRAME_TOTAL_LEN, "frame n={n}");
            let deframed = bk_crc_deframe(&framed);
            assert_eq!(deframed.len(), n.div_ceil(BK_FRAME_DATA_LEN) * BK_FRAME_DATA_LEN, "deframe n={n}");
        }
    }

    #[test]
    fn tail_block_is_padded_with_ff() {
        let data = [0x01u8, 0x02, 0x03];
        let framed = bk_crc_frame(&data);
        assert_eq!(framed.len(), BK_FRAME_TOTAL_LEN);
        assert_eq!(&framed[..3], &data[..]);
        assert!(framed[3..BK_FRAME_DATA_LEN].iter().all(|&b| b == 0xFF));
        // 末尾 2 字节为补齐后整块的 CRC16，大端序。
        let crc = bk_crc16(&framed[..BK_FRAME_DATA_LEN]);
        assert_eq!(framed[BK_FRAME_DATA_LEN], (crc >> 8) as u8);
        assert_eq!(framed[BK_FRAME_DATA_LEN + 1], crc as u8);
    }

    #[test]
    fn frame_deframe_roundtrip() {
        for &n in &[0usize, 1, 31, 32, 33, 64, 96, 100] {
            let data = sample(n);
            // 帧化对尾块做 0xFF 补齐，解帧会保留这些补齐字节。
            let mut expected = data.clone();
            expected.resize(n.div_ceil(BK_FRAME_DATA_LEN) * BK_FRAME_DATA_LEN, 0xFFu8);
            assert_eq!(bk_crc_deframe(&bk_crc_frame(&data)), expected, "n={n}");
        }
    }

    #[test]
    fn deframe_drops_trailing_partial_frame() {
        // 不足一整帧的输入直接返回空。
        assert!(bk_crc_deframe(&[]).is_empty());
        assert!(bk_crc_deframe(&sample(33)).is_empty());

        let data = sample(2 * BK_FRAME_DATA_LEN);
        let base = bk_crc_frame(&data);
        for tail in 1..BK_FRAME_TOTAL_LEN {
            let mut framed = base.clone();
            framed.resize(framed.len() + tail, 0xAAu8);
            assert_eq!(bk_crc_deframe(&framed), data, "tail={tail}");
        }
    }

    #[test]
    fn matches_legacy_algorithm() {
        for &n in &[0usize, 1, 7, 31, 32, 33, 64, 95, 100, 256] {
            let data = sample(n);
            assert_eq!(bk_crc16(&data), oracle_crc16(&data), "crc n={n}");
            assert_eq!(bk_crc_frame(&data), oracle_frame(&data), "frame n={n}");
            assert_eq!(bk_crc_deframe(&bk_crc_frame(&data)), oracle_deframe(&oracle_frame(&data)), "deframe n={n}");
        }
    }

    #[test]
    fn crc16_modbus_device_variant_check_vector() {
        // 设备侧协议用 init=0 变体（反射多项式 0xA001，等效 CRC-16/ARC），
        // 与标准 CRC-16/MODBUS（init=0xFFFF，对 b"123456789" 得 0x4B37）不同。
        // 必须锁定 init=0 的结果，改成标准初值会让刷机/日志帧校验与设备不一致。
        assert_eq!(crc16_modbus(b"123456789"), 0xBB3D);
    }

    #[test]
    fn crc16_modbus_empty_input_is_zero() {
        assert_eq!(crc16_modbus(&[]), 0x0000);
    }

    #[test]
    fn crc16_modbus_matches_bitwise_oracle() {
        for &n in &[0usize, 1, 2, 3, 7, 9, 15, 16, 31, 32, 33, 64, 95, 100, 256] {
            let data = sample(n);
            assert_eq!(crc16_modbus(&data), oracle_crc16_modbus(&data), "n={n}");
        }
    }
}
