// OTA package generation — LZMA block compression and OTA header assembly.
//
// Reimplements soc_tools.exe's `zip_file` and `make_ota_file` commands:
//   zip_file       → lzma_compress_file()
//   make_ota_file  → assemble_ota_package()
//
// These functions produce binary-compatible output with the original C++ tool,
// used by EC7xx/Air8000 and Air1601/CCM4211 FOTA workflows.
//
// LZMA compression uses the same LZMA SDK C code (via FFI) as soc_tools.exe,
// ensuring byte-identical compressed output.

use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

use md5::Digest;

// ── LZMA SDK FFI ────────────────────────────────────────────────────────────

extern "C" {
    fn lzma_sdk_compress(
        src: *const u8,
        src_len: u32,
        dest: *mut u8,
        dest_len: *mut u32,
        props: *mut u8,
        props_size: *mut u32,
        level: i32,
        dict_size: u32,
        lc: i32,
        lp: i32,
        pb: i32,
        fb: i32,
    ) -> i32;
}

const LZMA_SDK_OK: i32 = 0;

// ── LZMA constants ─────────────────────────────────────────────────────────

const LZMA_PROPS_SIZE: usize = 5;

// ── Binary structs (repr(C), packed for exact-layout compatibility) ────────

/// Sector header without MD5 (20 bytes) — used when UseMD5=0.
#[repr(C)]
struct SectorHeader {
    magic: u32,
    total_len: u32,
    data_len: u32,
    data_crc32: u32,
    start_address: u32,
}

/// Sector header with MD5 (36 bytes) — used when UseMD5=1.
#[repr(C)]
struct SectorMd5Header {
    magic: u32,
    total_len: u32,
    data_len: u32,
    md5: [u8; 16],
    block_len: u32,
    start_address: u32,
}

/// OTA file header (92 bytes).
#[repr(C)]
pub struct OtaFileHeader {
    pub magic: u32,
    pub crc32: u32,
    pub main_version: [u32; 5],
    pub app_version: u32,
    pub std_version: [u32; 5],
    pub common_data_len: u32,
    pub sdk_data_len: u32,
    pub common_md5: [u8; 16],
    pub sdk_md5: [u8; 16],
}

// ── CRC32 helper (matches C++ CRC32_Cal exactly, including its non-standard table) ──

/// Reflect (bit-reverse) a value of `bits` width.
fn reflect(mut val: u64, bits: u8) -> u64 {
    let mut r = 0u64;
    for _ in 0..bits {
        r = (r << 1) | (val & 1);
        val >>= 1;
    }
    r
}

/// CRC32 table pre-computed using the EXACT same algorithm as C++ CRC32_CreateTable.
/// Uses generator 0x04C11DB7 (normal poly) with reflected input AND reflected output.
/// This is non-standard but matches soc_tools.exe and device firmware.
fn crc32_table() -> &'static [u32; 256] {
    use std::sync::OnceLock;
    static TABLE: OnceLock<[u32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let gen: u64 = 0x04C11DB7;
        let mut tab = [0u32; 256];
        for i in 0..256u64 {
            let temp = reflect(i, 8);
            let mut val = temp << 24;
            for _ in 0..8 {
                if val & 0x80000000 != 0 {
                    val = (val << 1) ^ gen;
                } else {
                    val <<= 1;
                }
            }
            tab[i as usize] = reflect(val, 32) as u32;
        }
        tab
    })
}

/// Compute CRC32 exactly matching C++ CRC32_Cal(CRC32_Table, data, len, 0xffffffff).
fn crc32_matching_cpp(data: &[u8]) -> u32 {
    let table = crc32_table();
    let mut crc: u32 = 0xffffffff;
    for &byte in data {
        crc = table[((crc ^ byte as u32) & 0xff) as usize] ^ (crc >> 8);
    }
    crc
}

// ── LZMA block compress ────────────────────────────────────────────────────

/// LZMA-compress a single block of data using LZMA SDK (same as soc_tools.exe).
/// Returns (compressed_data, lzma_props_5bytes).
fn lzma_compress_block(
    input: &[u8],
    dict_size: u32,
    lc: u32,
    lp: u32,
    pb: u32,
) -> Result<(Vec<u8>, [u8; 5])> {
    // Output buffer: worst case is slightly larger than input
    let mut dest = vec![0u8; input.len() + input.len() / 3 + 128];
    let mut dest_len = dest.len() as u32;
    let mut props = [0u8; 5];
    let mut props_size = props.len() as u32;

    let ret = unsafe {
        lzma_sdk_compress(
            input.as_ptr(),
            input.len() as u32,
            dest.as_mut_ptr(),
            &mut dest_len,
            props.as_mut_ptr(),
            &mut props_size,
            9, // level
            dict_size,
            lc as i32,
            lp as i32,
            pb as i32,
            32, // fb (fast bytes)
        )
    };

    if ret != LZMA_SDK_OK {
        anyhow::bail!("LZMA SDK compression failed with code {}", ret);
    }

    dest.truncate(dest_len as usize);
    Ok((dest, props))
}

// ── zip_file — LZMA block-compress a binary ────────────────────────────────

/// LZMA block-compress a binary file (equivalent to `soc_tools zip_file`).
///
/// Splits the input into blocks of `max_block_len` (default 64KB), compresses
/// each with LZMA level 9, and writes a header + block stream.
///
/// When `use_md5` is true: 36-byte header (SectorMd5Header), lp=1
/// When `use_md5` is false: 20-byte header (SectorHeader), lp=0
pub fn lzma_compress_file(
    input_path: &Path,
    output_path: &Path,
    magic: u32,
    start_addr: u32,
    max_block_len: u32,
    use_md5: bool,
) -> Result<()> {
    let raw_data = fs::read(input_path)
        .with_context(|| format!("read {}", input_path.display()))?;

    let dict_size = max_block_len;
    let lc = 3u32;
    let lp = if use_md5 { 1u32 } else { 0u32 };
    let pb = 2u32;

    let total_len = raw_data.len() as u32;

    // Compress block by block
    let mut out_blocks = Vec::new();
    let mut pos = 0usize;
    while pos < raw_data.len() {
        let block_end = std::cmp::min(pos + max_block_len as usize, raw_data.len());
        let block = &raw_data[pos..block_end];
        pos = block_end;

        let (compressed, props) = match lzma_compress_block(block, dict_size, lc, lp, pb) {
            Ok((data, props)) if data.len() < block.len() => (data, props),
            _ => {
                // Compression failed or wasn't beneficial — store uncompressed
                (block.to_vec(), [0u8; 5])
            }
        };

        // Write block: [1B props_len | 5B props | 4B LE compr_size | data]
        let compr_size = compressed.len() as u32;
        let props_len = if props[0] == 0 { 0u8 } else { LZMA_PROPS_SIZE as u8 };

        out_blocks.push(props_len);
        if props_len == LZMA_PROPS_SIZE as u8 {
            out_blocks.extend_from_slice(&props);
        }
        out_blocks.extend_from_slice(&compr_size.to_le_bytes());
        out_blocks.extend_from_slice(&compressed);
    }

    // Write header + blocks
    let mut file = fs::File::create(output_path)
        .with_context(|| format!("create {}", output_path.display()))?;

    if use_md5 {
        let mut header = SectorMd5Header {
            magic,
            total_len: out_blocks.len() as u32,
            data_len: total_len,
            md5: [0u8; 16],
            block_len: max_block_len,
            start_address: start_addr,
        };
        let md5_bytes: [u8; 16] = {
            use md5::{Digest, Md5};
            let mut hasher = Md5::new();
            hasher.update(&raw_data);
            hasher.finalize().into()
        };
        header.md5 = md5_bytes;
        let header_bytes = unsafe {
            std::slice::from_raw_parts(
                &header as *const SectorMd5Header as *const u8,
                std::mem::size_of::<SectorMd5Header>(),
            )
        };
        file.write_all(header_bytes)?;
    } else {
        let header = SectorHeader {
            magic,
            total_len: out_blocks.len() as u32,
            data_len: total_len,
            data_crc32: crc32_matching_cpp(&raw_data),
            start_address: start_addr,
        };
        let header_bytes = unsafe {
            std::slice::from_raw_parts(
                &header as *const SectorHeader as *const u8,
                std::mem::size_of::<SectorHeader>(),
            )
        };
        file.write_all(header_bytes)?;
    }

    file.write_all(&out_blocks)?;

    Ok(())
}

// ── make_ota_file — assemble OTA package ────────────────────────────────────

/// Assemble an OTA package from two partition files (equivalent to `soc_tools make_ota_file`).
///
/// Constructs a `CoreUpgrade_FileHeadCalMD5Struct` header followed by common data and SDK data.
///
/// Arguments:
/// - `magic`: magic number identifier
/// - `main_version_decimal`: MainVersion[4] as a decimal u32
/// - `main_md5_prefix`: hex string (up to 32 chars) → decoded into MainVersion[0..3]
/// - `std_version_decimal`: STDVersion[4] as a decimal u32
/// - `std_md5_prefix`: hex string → decoded into STDVersion[0..3]
/// - `app_version`: app version as hex u32
/// - `common_path`: path to common partition data (e.g., user script, or dummy)
/// - `sdk_path`: path to SDK partition data (e.g., delta.par, or dummy)
/// - `output_path`: output .sota file path
pub fn assemble_ota_package(
    magic: u32,
    main_version_decimal: u32,
    main_md5_prefix: &str,
    std_version_decimal: u32,
    std_md5_prefix: &str,
    app_version: u32,
    common_path: &Path,
    sdk_path: &Path,
    output_path: &Path,
) -> Result<()> {
    // ── Read input files ────────────────────────────────────────────────────
    let common_data = if common_path.metadata().map(|m| m.len()).unwrap_or(0) < 4 {
        Vec::new()
    } else {
        fs::read(common_path)
            .with_context(|| format!("read {}", common_path.display()))?
    };
    let sdk_data = if sdk_path.metadata().map(|m| m.len()).unwrap_or(0) < 4 {
        Vec::new()
    } else {
        fs::read(sdk_path)
            .with_context(|| format!("read {}", sdk_path.display()))?
    };

    // ── Build header ────────────────────────────────────────────────────────
    let mut header = OtaFileHeader {
        magic,
        crc32: 0,
        main_version: [0u32; 5],
        app_version,
        std_version: [0u32; 5],
        common_data_len: common_data.len() as u32,
        sdk_data_len: sdk_data.len() as u32,
        common_md5: [0u8; 16],
        sdk_md5: [0u8; 16],
    };

    // MainVersion[4] = version number (decimal)
    // MainVersion[0..3] = MD5 prefix decoded from hex string (32 hex chars → 16 bytes)
    let mut main_version = header.main_version;
    main_version[4] = main_version_decimal;
    decode_md5_prefix(main_md5_prefix, &mut main_version);
    header.main_version = main_version;

    let mut std_version = header.std_version;
    std_version[4] = std_version_decimal;
    decode_md5_prefix(std_md5_prefix, &mut std_version);
    header.std_version = std_version;

    // Compute MD5 of partitions
    if !common_data.is_empty() {
        let mut hasher = md5::Md5::new();
        hasher.update(&common_data);
        let hash: [u8; 16] = hasher.finalize().into();
        header.common_md5.copy_from_slice(&hash);
    }
    if !sdk_data.is_empty() {
        let mut hasher = md5::Md5::new();
        hasher.update(&sdk_data);
        let hash: [u8; 16] = hasher.finalize().into();
        header.sdk_md5.copy_from_slice(&hash);
    }

    // Compute CRC32 over MainVersion..SDKMD5 (bytes 8..91 of the 92-byte header)
    // The C++ code: CRC32_Cal(CRC32_Table, (uint8_t *)Head.MainVersion, sizeof(Head) - 8, 0xffffffff)

    // Build explicit CRC buffer to avoid any aliasing/alignment concerns.
    let header_size = std::mem::size_of::<OtaFileHeader>();
    let mut crc_buf = Vec::with_capacity(header_size - 8);

    // MainVersion[5]: 20 bytes (u32 LE x5)
    for v in &header.main_version {
        crc_buf.extend_from_slice(&v.to_le_bytes());
    }
    // AppVersion: 4 bytes
    crc_buf.extend_from_slice(&header.app_version.to_le_bytes());
    // STDVersion[5]: 20 bytes
    for v in &header.std_version {
        crc_buf.extend_from_slice(&v.to_le_bytes());
    }
    // CommonDataLen: 4 bytes
    crc_buf.extend_from_slice(&header.common_data_len.to_le_bytes());
    // SDKDataLen: 4 bytes
    crc_buf.extend_from_slice(&header.sdk_data_len.to_le_bytes());
    // CommonMD5: 16 bytes
    crc_buf.extend_from_slice(&header.common_md5);
    // SDKMD5: 16 bytes
    crc_buf.extend_from_slice(&header.sdk_md5);

    // Should be exactly 84 bytes (= 20+4+20+4+4+16+16)
    debug_assert_eq!(crc_buf.len(), header_size - 8);

    let computed_crc = crc32_matching_cpp(&crc_buf);
    header.crc32 = computed_crc;

    // Build full header bytes for writing
    let mut header_bytes = Vec::with_capacity(header_size);
    header_bytes.extend_from_slice(&header.magic.to_le_bytes());
    header_bytes.extend_from_slice(&header.crc32.to_le_bytes());
    for v in &header.main_version {
        header_bytes.extend_from_slice(&v.to_le_bytes());
    }
    header_bytes.extend_from_slice(&header.app_version.to_le_bytes());
    for v in &header.std_version {
        header_bytes.extend_from_slice(&v.to_le_bytes());
    }
    header_bytes.extend_from_slice(&header.common_data_len.to_le_bytes());
    header_bytes.extend_from_slice(&header.sdk_data_len.to_le_bytes());
    header_bytes.extend_from_slice(&header.common_md5);
    header_bytes.extend_from_slice(&header.sdk_md5);

    // ── Write output ────────────────────────────────────────────────────────
    let mut out = fs::File::create(output_path)
        .with_context(|| format!("create {}", output_path.display()))?;
    out.write_all(&header_bytes)?;
    if !common_data.is_empty() {
        out.write_all(&common_data)?;
    }
    if !sdk_data.is_empty() {
        out.write_all(&sdk_data)?;
    }
    out.flush()?;

    Ok(())
}

/// Decode a hex string (up to 32 chars) into the first 16 bytes of `dst[5]` (little-endian u32s).
/// The C++ code: AsciiToHex(md5_hex_str, 32, (uint8_t *)Head.MainVersion)
/// which converts 32 hex chars into 16 bytes, stored as 4 u32 LE values.
fn decode_md5_prefix(hex_str: &str, dst: &mut [u32; 5]) {
    let hex = hex_str.as_bytes();
    let hex_len = hex.len().min(32);
    let mut buf = [0u8; 16];

    for i in 0..(hex_len / 2) {
        let hi = hex_char_val(hex[i * 2]);
        let lo = hex_char_val(hex[i * 2 + 1]);
        buf[i] = (hi << 4) | lo;
    }

    // Pad remaining with '0' → 0x00
    for i in (hex_len / 2)..16 {
        buf[i] = 0u8;
    }

    // Store as 4 little-endian u32
    for i in 0..4 {
        let off = i * 4;
        dst[i] = u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]);
    }
}

fn hex_char_val(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => 0,
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crc32_consistency() {
        assert_eq!(crc32_matching_cpp(b"hello"), crc32_matching_cpp(b"hello"));
    }

    #[test]
    fn test_decode_md5_prefix() {
        let mut dst = [0u32; 5];
        decode_md5_prefix("0", &mut dst);
        for v in dst.iter().take(4) {
            assert_eq!(*v, 0);
        }
    }

    #[test]
    fn test_decode_md5_prefix_32chars() {
        let mut dst = [0u32; 5];
        decode_md5_prefix("11223344556677889900AABBCCDDEEFF", &mut dst);
        assert_eq!(dst[0], 0x44332211);
        assert_eq!(dst[1], 0x88776655);
        assert_eq!(dst[2], 0xBBAA0099);
        assert_eq!(dst[3], 0xFFEEDDCC);
    }

    #[test]
    fn test_ota_header_size() {
        assert_eq!(std::mem::size_of::<OtaFileHeader>(), 92);
    }

    #[test]
    fn test_sector_header_size() {
        assert_eq!(std::mem::size_of::<SectorHeader>(), 20);
    }

    #[test]
    fn test_sector_md5_header_size() {
        assert_eq!(std::mem::size_of::<SectorMd5Header>(), 36);
    }

    #[test]
    fn test_header_crc32_manual() {
        let _magic: u32 = 0x12345678;
        let main_ver: [u32; 5] = [0, 0, 0, 0, 0xFFFFFFFF];
        let app_ver: u32 = 0;
        let std_ver: [u32; 5] = [0; 5];
        let cd_len: u32 = 0x100;
        let sd_len: u32 = 0;
        let cm_md5 = md5::Md5::digest(b"hello");
        let cm_hash: [u8; 16] = cm_md5.into();
        let sk_md5 = [0u8; 16];

        let mut crc_buf = Vec::new();
        for v in &main_ver { crc_buf.extend_from_slice(&v.to_le_bytes()); }
        crc_buf.extend_from_slice(&app_ver.to_le_bytes());
        for v in &std_ver { crc_buf.extend_from_slice(&v.to_le_bytes()); }
        crc_buf.extend_from_slice(&cd_len.to_le_bytes());
        crc_buf.extend_from_slice(&sd_len.to_le_bytes());
        crc_buf.extend_from_slice(&cm_hash);
        crc_buf.extend_from_slice(&sk_md5);
        assert_eq!(crc_buf.len(), 84);

        let crc = crc32_matching_cpp(&crc_buf);
        assert!(crc != 0);
        let crc2 = crc32_matching_cpp(&crc_buf);
        assert_eq!(crc, crc2);
    }

    #[test]
    fn test_assemble_ota_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let common_path = tmp.path().join("common.bin");
        let sdk_path = tmp.path().join("sdk.bin");
        let out_path = tmp.path().join("output.sota");

        let test_content = b"hello common data for OTA test";
        fs::write(&common_path, test_content).unwrap();
        fs::write(&sdk_path, []).unwrap();

        let magic: u32 = 0xABCD0001;
        let app_ver: u32 = 0;
        let main_ver_dec: u32 = 0xFFFFFFFF;

        assemble_ota_package(
            magic, main_ver_dec, "0", 0, "0", app_ver,
            &common_path, &sdk_path, &out_path,
        ).unwrap();

        let data = fs::read(&out_path).unwrap();
        let expected_data_len = test_content.len();
        assert_eq!(data.len(), 92 + expected_data_len);

        let read_u32 = |off: usize| u32::from_le_bytes(data[off..off+4].try_into().unwrap());
        assert_eq!(read_u32(0), magic);
        assert_eq!(read_u32(52), expected_data_len as u32);
        assert_eq!(read_u32(56), 0);
        assert_eq!(&data[92..92+expected_data_len], test_content);

        let expected_md5 = md5::Md5::digest(test_content);
        let expected_hash: [u8; 16] = expected_md5.into();
        assert_eq!(&data[60..76], &expected_hash);

        let mut expected_crc_buf = Vec::new();
        for v in &[0u32, 0, 0, 0, main_ver_dec] { expected_crc_buf.extend_from_slice(&v.to_le_bytes()); }
        expected_crc_buf.extend_from_slice(&app_ver.to_le_bytes());
        for _ in 0..5 { expected_crc_buf.extend_from_slice(&0u32.to_le_bytes()); }
        expected_crc_buf.extend_from_slice(&(expected_data_len as u32).to_le_bytes());
        expected_crc_buf.extend_from_slice(&0u32.to_le_bytes());
        expected_crc_buf.extend_from_slice(&expected_hash);
        expected_crc_buf.extend_from_slice(&[0u8; 16]);
        assert_eq!(expected_crc_buf.len(), 84);

        let expected_crc = crc32_matching_cpp(&expected_crc_buf);
        assert_eq!(read_u32(4), expected_crc, "CRC32 mismatch");
    }

    #[test]
    fn test_lzma_compress_file_format() {
        let tmp = tempfile::tempdir().unwrap();
        let input_path = tmp.path().join("input.bin");
        let output_path = tmp.path().join("output.zip");

        let test_data = b"Hello LZMA World! This is test data for compression.";
        fs::write(&input_path, test_data).unwrap();

        lzma_compress_file(&input_path, &output_path, 0x1234, 0x20000000, 0x10000, true).unwrap();

        let data = fs::read(&output_path).unwrap();
        assert!(data.len() > 36);
        assert_eq!(u32::from_le_bytes(data[0..4].try_into().unwrap()), 0x1234);
        assert_eq!(u32::from_le_bytes(data[8..12].try_into().unwrap()), test_data.len() as u32);
        assert_eq!(u32::from_le_bytes(data[32..36].try_into().unwrap()), 0x20000000);
    }

    /// Compare LZMA output against soc_tools.exe for byte-identical compression.
    #[test]
    #[ignore = "requires soc_tools.exe"]
    fn test_lzma_output_matches_soc_tools() {
        let soc_tools = find_soc_tools_exe();
        if soc_tools.is_none() { eprintln!("SKIP"); return; }
        let soc_tools = soc_tools.unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let input = tmp.path().join("input.bin");
        let rust_out = tmp.path().join("rust.zip");
        let soc_out = tmp.path().join("soc.zip");

        let test_data = vec![0xABu8; 0x20000];
        fs::write(&input, &test_data).unwrap();

        let magic: u32 = 0x1234ABCD;
        let addr: u32 = 0x20000000;

        lzma_compress_file(&input, &rust_out, magic, addr, 0x40000, true).unwrap();

        let status = std::process::Command::new(&soc_tools)
            .args(["zip_file", &format!("{:X}", magic), &format!("{:X}", addr),
                   &input.to_string_lossy(), &soc_out.to_string_lossy(), "40000", "1"])
            .status().expect("run soc_tools.exe");
        assert!(status.success());

        let rust_data = fs::read(&rust_out).unwrap();
        let soc_data = fs::read(&soc_out).unwrap();

        if rust_data != soc_data {
            let min_len = rust_data.len().min(soc_data.len());
            let mut diffs = 0usize;
            for i in 0..min_len {
                if rust_data[i] != soc_data[i] {
                    if diffs < 20 { eprintln!("DIFF offset {}: rust=0x{:02X} soc=0x{:02X}", i, rust_data[i], soc_data[i]); }
                    diffs += 1;
                }
            }
            eprintln!("TOTAL DIFFS: {}", diffs);
            eprintln!("MD5 rust={} soc={}", hex_str(md5::Md5::digest(&rust_data).as_slice()), hex_str(md5::Md5::digest(&soc_data).as_slice()));
        }
        assert_eq!(rust_data, soc_data);
    }

    /// Full pipeline: ROM → LZMA → assemble_ota → compare with soc_tools.exe
    #[test]
    #[ignore = "requires soc_tools.exe"]
    fn test_full_pipeline_matches_soc_tools() {
        let soc_tools = find_soc_tools_exe();
        if soc_tools.is_none() { eprintln!("SKIP"); return; }
        let soc_tools = soc_tools.unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let rom_path = tmp.path().join("rom.bin");
        let dummy_path = tmp.path().join("dummy.bin");
        let rust_zip = tmp.path().join("rust_ap.zip");
        let soc_zip = tmp.path().join("soc_ap.zip");
        let rust_sota = tmp.path().join("rust_fota.sota");
        let soc_sota = tmp.path().join("soc_fota.sota");

        let rom_data = vec![0x5Au8; 0x20000];
        fs::write(&rom_path, &rom_data).unwrap();
        fs::write(&dummy_path, []).unwrap();

        let magic: u32 = 0xBEEF0001;
        let addr: u32 = 0x20000000;

        lzma_compress_file(&rom_path, &rust_zip, magic, addr, 0x40000, true).unwrap();
        let zs = std::process::Command::new(&soc_tools)
            .args(["zip_file", &format!("{:X}", magic), &format!("{:X}", addr),
                   &rom_path.to_string_lossy(), &soc_zip.to_string_lossy(), "40000", "1"])
            .status().expect("soc_tools zip_file");
        assert!(zs.success());
        assert_eq!(fs::read(&rust_zip).unwrap(), fs::read(&soc_zip).unwrap());

        assemble_ota_package(magic, 0xFFFFFFFF, "0", 0, "0", 0, &rust_zip, &dummy_path, &rust_sota).unwrap();
        let ms = std::process::Command::new(&soc_tools)
            .args(["make_ota_file", &format!("{:X}", magic), "4294967295", "0", "0", "0", "0",
                   &soc_zip.to_string_lossy(), &dummy_path.to_string_lossy(), &soc_sota.to_string_lossy()])
            .status().expect("soc_tools make_ota_file");
        assert!(ms.success());

        let rust_data = fs::read(&rust_sota).unwrap();
        let soc_data = fs::read(&soc_sota).unwrap();

        if rust_data != soc_data {
            eprintln!("SIZE: rust={} soc={}", rust_data.len(), soc_data.len());
            let min = rust_data.len().min(soc_data.len());
            let mut diffs = 0;
            for i in 0..min {
                if rust_data[i] != soc_data[i] {
                    if diffs < 30 { eprintln!("OFF {}: rust=0x{:02X} soc=0x{:02X}", i, rust_data[i], soc_data[i]); }
                    diffs += 1;
                }
            }
            eprintln!("TOTAL DIFFS: {}", diffs);
        }
        assert_eq!(rust_data, soc_data);
    }

    fn find_soc_tools_exe() -> Option<std::path::PathBuf> {
        for c in &["refs/origin_tools/tools/soc_tools.exe", "../refs/origin_tools/tools/soc_tools.exe",
                    "../../refs/origin_tools/tools/soc_tools.exe", "tools/soc_tools.exe", "soc_tools.exe"] {
            let p = std::path::PathBuf::from(c);
            if p.exists() { return Some(p); }
        }
        None
    }

    fn hex_str(data: &[u8]) -> String {
        data.iter().map(|b| format!("{:02x}", b)).collect()
    }
}
