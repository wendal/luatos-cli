// LuaDB script image packer + BK CRC16 for Air8101.
//
// LuaDB is a simple archive format for bundling Lua scripts into a flash
// partition. Reference: https://wiki.luatos.com/develop/contribute/luadb.html

/// A file entry to include in a LuaDB image.
#[derive(Debug, Clone)]
pub struct LuadbEntry {
    pub filename: String,
    pub data: Vec<u8>,
}

const LUADB_MAGIC: &[u8] = &[0x01, 0x04, 0x5A, 0xA5, 0x5A, 0xA5];

/// Pack file entries into a LuaDB binary (script.bin).
///
/// # Format
/// ```text
/// Global Header (18 bytes):
///   MAGIC[6] | version TLV[4] | head_len TLV[6] | count TLV[4] | CRC TLV[4]
/// Per-File Entry:
///   MAGIC[6] | name TLV[2+N] | size TLV[6] | CRC TLV[4] | data[N]
/// ```
pub fn pack_luadb(entries: &[LuadbEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    let count = entries.len() as u16;

    // Global header
    out.extend_from_slice(LUADB_MAGIC);
    out.extend_from_slice(&[0x02, 0x02, 0x02, 0x00]); // version = 2
    out.extend_from_slice(&[0x03, 0x04, 0x12, 0x00, 0x00, 0x00]); // head_len = 18
    out.extend_from_slice(&[0x04, 0x02, count as u8, (count >> 8) as u8]);
    out.extend_from_slice(&[0xFE, 0x02, 0xFF, 0xFF]); // CRC (unused)

    // Per-file entries
    for e in entries {
        out.extend_from_slice(LUADB_MAGIC);
        out.push(0x02);
        out.push(e.filename.len() as u8);
        out.extend_from_slice(e.filename.as_bytes());
        let sz = e.data.len() as u32;
        out.extend_from_slice(&[0x03, 0x04, sz as u8, (sz >> 8) as u8, (sz >> 16) as u8, (sz >> 24) as u8]);
        out.extend_from_slice(&[0xFE, 0x02, 0xFF, 0xFF]); // CRC (unused)
        out.extend_from_slice(&e.data);
    }
    out
}

/// Add BK CRC16 framing required by Air8101 script partition.
///
/// Every 32-byte block of `data` is followed by a 2-byte big-endian CRC16.
/// The final block is padded with 0xFF bytes to reach 32 bytes before
/// the CRC is calculated.
pub fn add_bk_crc(data: &[u8]) -> Vec<u8> {
    let blocks = data.len().div_ceil(32);
    let mut out = Vec::with_capacity(blocks * 34);
    let mut i = 0;
    while i < data.len() {
        let mut block = [0xFFu8; 32];
        let end = (i + 32).min(data.len());
        block[..end - i].copy_from_slice(&data[i..end]);
        let crc = bk_crc16(&block);
        out.extend_from_slice(&block);
        out.push((crc >> 8) as u8); // big-endian
        out.push(crc as u8);
        i += 32;
    }
    out
}

/// CRC16 used by Beken: init=0xFFFFFFFF, poly=0x8005, big-endian result.
fn bk_crc16(block: &[u8; 32]) -> u16 {
    let mut crc: u32 = 0xFFFFFFFF;
    for &b in block {
        crc ^= (b as u32) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x8005 } else { crc << 1 };
        }
    }
    (crc & 0xFFFF) as u16
}

pub mod build;
pub mod embedded_helpers;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_round_trip() {
        let entries = vec![LuadbEntry {
            filename: "main.lua".into(),
            data: b"print('hello')".to_vec(),
        }];
        let data = pack_luadb(&entries);
        assert_eq!(&data[0..6], LUADB_MAGIC);
        // File count = 1 at offset 18
        assert_eq!(data[18], 1);
    }

    #[test]
    fn bk_crc_deterministic() {
        let block = [0xFFu8; 32];
        let crc = bk_crc16(&block);
        assert_eq!(crc, bk_crc16(&block));
    }

    #[test]
    fn add_bk_crc_length() {
        let data = vec![0xAAu8; 64];
        let out = add_bk_crc(&data);
        // 2 blocks × (32 + 2)
        assert_eq!(out.len(), 68);
    }

    #[test]
    fn add_bk_crc_partial_block() {
        let data = vec![0xBBu8; 33]; // 1 full block + 1 byte
        let out = add_bk_crc(&data);
        // 2 blocks × 34
        assert_eq!(out.len(), 68);
        // Last block should be padded with 0xFF
        assert_eq!(out[34], 0xBB); // first byte of second block
        assert_eq!(out[35], 0xFF); // padding
    }

    #[test]
    fn pack_multiple_files() {
        let entries = vec![
            LuadbEntry {
                filename: "main.lua".into(),
                data: b"print('a')".to_vec(),
            },
            LuadbEntry {
                filename: "lib.lua".into(),
                data: b"return {}".to_vec(),
            },
        ];
        let data = pack_luadb(&entries);
        assert_eq!(&data[0..6], LUADB_MAGIC);
        assert_eq!(data[18], 2); // file count
    }
}
