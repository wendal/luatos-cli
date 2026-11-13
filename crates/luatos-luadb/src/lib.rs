use anyhow::{ensure, Context, Result};

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

impl LuadbEntry {
    /// Return the final filename component for use as the LuaDB entry name.
    pub fn basename(&self) -> &str {
        self.filename.rsplit(['/', '\\']).next().unwrap_or(&self.filename)
    }
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
pub fn pack_luadb(entries: &[LuadbEntry]) -> Result<Vec<u8>> {
    pack_luadb_with_name_limit(entries, 27)
}

fn pack_luadb_with_name_limit(entries: &[LuadbEntry], name_limit: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let count = u16::try_from(entries.len())?;

    // Global header
    out.extend_from_slice(LUADB_MAGIC);
    out.extend_from_slice(&[0x02, 0x02, 0x02, 0x00]); // version = 2
    out.extend_from_slice(&[0x03, 0x04, 0x12, 0x00, 0x00, 0x00]); // head_len = 18
    out.extend_from_slice(&[0x04, 0x02, count as u8, (count >> 8) as u8]);
    out.extend_from_slice(&[0xFE, 0x02, 0xFF, 0xFF]); // CRC (unused)

    // Per-file entries
    for e in entries {
        out.extend_from_slice(LUADB_MAGIC);
        let filename = e.basename();
        ensure!(
            filename.len() <= name_limit,
            "LuaDB 文件名过长 (最大{}字节，实际{}字节): {}",
            name_limit,
            filename.len(),
            filename
        );
        out.push(0x02);
        out.push(filename.len() as u8);
        out.extend_from_slice(filename.as_bytes());
        let sz = u32::try_from(e.data.len())?;
        out.extend_from_slice(&[0x03, 0x04, sz as u8, (sz >> 8) as u8, (sz >> 16) as u8, (sz >> 24) as u8]);
        out.extend_from_slice(&[0xFE, 0x02, 0xFF, 0xFF]); // CRC (unused)
        out.extend_from_slice(&e.data);
    }
    Ok(out)
}

/// Parse a LuaDB image back into entries.
///
/// 只解析 `pack_luadb` 生成的结构；镜像尾部的 0xFF 填充会被忽略。
pub fn parse_luadb(data: &[u8]) -> Result<Vec<LuadbEntry>> {
    let mut pos = 0usize;
    let mut magic = false;
    let mut version = false;
    let mut count: Option<usize> = None;

    // 全局头：0x01 magic | 0x02 version | 0x03 head_len | 0x04 count | 0xFE end
    while pos + 2 <= data.len() {
        let kind = data[pos];
        let len = data[pos + 1] as usize;
        pos += 2;
        ensure!(pos + len <= data.len(), "LuaDB 头部 TLV 越界");
        let value = &data[pos..pos + len];
        pos += len;
        match kind {
            0x01 => {
                ensure!(len == 4 && value == [0x5A, 0xA5, 0x5A, 0xA5].as_slice(), "LuaDB magic 错误");
                magic = true;
            }
            0x02 => {
                ensure!(len == 2, "LuaDB version 长度错误");
                version = true;
            }
            0x03 => ensure!(len == 4, "LuaDB head_len 长度错误"),
            0x04 => {
                ensure!(len == 2, "LuaDB count 长度错误");
                count = Some(u16::from_le_bytes([value[0], value[1]]) as usize);
            }
            0xFE => break,
            _ => {}
        }
    }
    ensure!(magic && version, "不是有效的 LuaDB 镜像");
    let count = count.context("LuaDB 缺少文件数量")?;
    ensure!(count > 0, "LuaDB 不包含文件");

    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        // 文件头：0x01 magic | 0x02 name | 0x03 size | 0xFE end
        ensure!(pos + 2 <= data.len() && data[pos] == 0x01, "LuaDB 文件头缺少 magic");
        let magic_len = data[pos + 1] as usize;
        pos += 2;
        ensure!(pos + magic_len <= data.len(), "LuaDB magic 越界");
        ensure!(magic_len == 4 && &data[pos..pos + 4] == [0x5A, 0xA5, 0x5A, 0xA5].as_slice(), "LuaDB 文件 magic 错误");
        pos += magic_len;

        ensure!(pos + 2 <= data.len() && data[pos] == 0x02, "LuaDB 文件头缺少文件名");
        let name_len = data[pos + 1] as usize;
        pos += 2;
        ensure!(name_len > 0 && pos + name_len <= data.len(), "LuaDB 文件名越界");
        let filename = String::from_utf8(data[pos..pos + name_len].to_vec()).context("LuaDB 文件名不是 UTF-8")?;
        pos += name_len;

        ensure!(pos + 2 <= data.len() && data[pos] == 0x03, "LuaDB 文件头缺少长度");
        let size_len = data[pos + 1] as usize;
        pos += 2;
        ensure!(size_len == 4 && pos + 4 <= data.len(), "LuaDB 文件长度字段错误");
        let size = u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        pos += 4;

        ensure!(pos + 2 <= data.len() && data[pos] == 0xFE, "LuaDB 文件头缺少结束标记");
        let crc_len = data[pos + 1] as usize;
        pos += 2 + crc_len;
        ensure!(pos + size <= data.len(), "LuaDB 文件数据越界: {filename}");

        entries.push(LuadbEntry {
            filename,
            data: data[pos..pos + size].to_vec(),
        });
        pos += size;
    }
    Ok(entries)
}

/// Add BK CRC16 framing required by Air8101 script partition.
///
/// Every 32-byte block of `data` is followed by a 2-byte big-endian CRC16.
/// The final block is padded with 0xFF bytes to reach 32 bytes before
/// the CRC is calculated.
pub fn add_bk_crc(data: &[u8]) -> Vec<u8> {
    luatos_codec::bk_crc_frame(data)
}

pub mod build;
mod luac;
mod mklfs;
pub mod romfs;

/// luac 调试信息保留级别：无调试信息（source 字段也不写）
pub const LUAC_DEBUG_NONE: u8 = 0;
/// luac 调试信息保留级别：仅行号（lineinfo）
pub const LUAC_DEBUG_LINE: u8 = 1;
/// luac 调试信息保留级别：仅局部变量名 + upvalue 名
pub const LUAC_DEBUG_VAR: u8 = 2;
/// luac 调试信息保留级别：全部调试信息
pub const LUAC_DEBUG_ALL: u8 = 99;

/// 规范化 luac 调试级别：0/1/2/99 原样返回，其余一律回退 99。
pub fn normalize_luac_debug(mode: u8) -> u8 {
    match mode {
        LUAC_DEBUG_NONE | LUAC_DEBUG_LINE | LUAC_DEBUG_VAR | LUAC_DEBUG_ALL => mode,
        _ => LUAC_DEBUG_ALL,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_luac_debug_whitelist() {
        assert_eq!(normalize_luac_debug(LUAC_DEBUG_NONE), 0);
        assert_eq!(normalize_luac_debug(LUAC_DEBUG_LINE), 1);
        assert_eq!(normalize_luac_debug(LUAC_DEBUG_VAR), 2);
        assert_eq!(normalize_luac_debug(LUAC_DEBUG_ALL), 99);
        // 非法值一律回退 99
        assert_eq!(normalize_luac_debug(3), 99);
        assert_eq!(normalize_luac_debug(5), 99);
        assert_eq!(normalize_luac_debug(255), 99);
    }

    #[test]
    fn pack_round_trip() {
        let entries = vec![LuadbEntry {
            filename: r"scripts\parent_directory_longer_than_the_name_limit\main.lua".into(),
            data: b"print('hello')".to_vec(),
        }];
        let data = pack_luadb(&entries).unwrap();
        assert_eq!(&data[0..6], LUADB_MAGIC);
        // File count = 1 at offset 18
        assert_eq!(data[18], 1);
        assert_eq!(&data[30..40], b"\x02\x08main.lua");
    }

    #[test]
    fn parse_luadb_round_trip() {
        let entries = vec![
            LuadbEntry {
                filename: "sys.lua".into(),
                data: b"-- corelib".to_vec(),
            },
            LuadbEntry {
                filename: "main.lua".into(),
                data: b"print('hello')".to_vec(),
            },
        ];
        let mut image = pack_luadb(&entries).unwrap();
        image.resize(image.len() + 64, 0xFF);
        let parsed = parse_luadb(&image).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].filename, "sys.lua");
        assert_eq!(parsed[0].data, b"-- corelib");
        assert_eq!(parsed[1].filename, "main.lua");
        assert_eq!(parsed[1].data, b"print('hello')");
    }

    #[test]
    fn parse_luadb_rejects_bad_magic() {
        let mut image = pack_luadb(&[LuadbEntry {
            filename: "main.lua".into(),
            data: b"x".to_vec(),
        }])
        .unwrap();
        image[2] ^= 0xFF;
        assert!(parse_luadb(&image).is_err());
    }

    #[test]
    fn bk_crc_deterministic() {
        let block = [0xFFu8; 32];
        let crc = luatos_codec::bk_crc16(&block);
        assert_eq!(crc, luatos_codec::bk_crc16(&block));
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
        let data = pack_luadb(&entries).unwrap();
        assert_eq!(&data[0..6], LUADB_MAGIC);
        assert_eq!(data[18], 2); // file count
    }
}
