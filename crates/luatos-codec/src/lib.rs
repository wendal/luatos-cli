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

#[cfg(test)]
mod tests {
    use super::{bk_crc16, bk_crc_deframe, bk_crc_frame, BK_FRAME_DATA_LEN, BK_FRAME_TOTAL_LEN};

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
}
