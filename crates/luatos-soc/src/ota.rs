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

use aes::Aes256;
use anyhow::{Context, Result};
use cbc::Encryptor;
use cipher::block_padding::Pkcs7;
use cipher::{BlockEncryptMut, KeyIvInit};
use flate2::write::GzEncoder;
use flate2::Compression;

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
fn lzma_compress_block(input: &[u8], dict_size: u32, lc: u32, lp: u32, pb: u32) -> Result<(Vec<u8>, [u8; 5])> {
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
pub fn lzma_compress_file(input_path: &Path, output_path: &Path, magic: u32, start_addr: u32, max_block_len: u32, use_md5: bool) -> Result<()> {
    let raw_data = fs::read(input_path).with_context(|| format!("read {}", input_path.display()))?;

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
    let mut file = fs::File::create(output_path).with_context(|| format!("create {}", output_path.display()))?;

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
        let header_bytes = unsafe { std::slice::from_raw_parts(&header as *const SectorMd5Header as *const u8, std::mem::size_of::<SectorMd5Header>()) };
        file.write_all(header_bytes)?;
    } else {
        let header = SectorHeader {
            magic,
            total_len: out_blocks.len() as u32,
            data_len: total_len,
            data_crc32: crc32_matching_cpp(&raw_data),
            start_address: start_addr,
        };
        let header_bytes = unsafe { std::slice::from_raw_parts(&header as *const SectorHeader as *const u8, std::mem::size_of::<SectorHeader>()) };
        file.write_all(header_bytes)?;
    }

    file.write_all(&out_blocks)?;

    Ok(())
}

const BK72XX_SCRIPT_FOTA_MAGIC: u32 = 0x4C554154; // "Luat"
const BK72XX_CP_FIXED_BYTES: usize = 0xF0000;
const BK72XX_FOTA_ALGO_GZIP_AES256: u16 = 258;
const BK72XX_AES_KEY: &[u8; 32] = b"0123456789ABCDEF0123456789ABCDEF";
const BK72XX_AES_IV: &[u8; 16] = b"0123456789ABCDEF";
const BK72XX_FIXED_GZIP_HEADER: [u8; 10] = [0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00];

fn trim_trailing_ff(data: &[u8]) -> &[u8] {
    let mut end = data.len();
    while end > 0 && data[end - 1] == 0xFF {
        end -= 1;
    }
    &data[..end]
}

fn pad_to_alignment_ff(data: &mut Vec<u8>, align: usize) {
    let rem = data.len() % align;
    if rem != 0 {
        data.extend(std::iter::repeat_n(0xFF, align - rem));
    }
}

fn bk_crc16_32bytes(chunk: &[u8]) -> u16 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in chunk {
        crc ^= (b as u32) << 8;
        for _ in 0..8 {
            if (crc & 0x8000) != 0 {
                crc = (crc << 1) ^ 0x8005;
            } else {
                crc <<= 1;
            }
        }
    }
    (crc & 0xFFFF) as u16
}

fn add_bk_crc_to_data(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity((data.len() / 32 + 1) * 34);
    let mut pos = 0usize;
    while pos < data.len() {
        let end = std::cmp::min(pos + 32, data.len());
        let mut chunk = data[pos..end].to_vec();
        if chunk.len() < 32 {
            chunk.resize(32, 0xFF);
        }
        let crc = bk_crc16_32bytes(&chunk);
        out.extend_from_slice(&chunk);
        out.extend_from_slice(&crc.to_be_bytes());
        pos = end;
    }
    out
}

/// Build BK72XX new-format script-only FOTA package.
/// Format: 1KB 0xFF + magic(u32 LE) + payload_len(u32 LE) + script_with_crc16
pub fn build_bk72xx_script_fota_new(script_path: &Path, output_path: &Path) -> Result<()> {
    let script_raw = fs::read(script_path).with_context(|| format!("read {}", script_path.display()))?;
    let mut script = trim_trailing_ff(&script_raw).to_vec();
    pad_to_alignment_ff(&mut script, 32);

    let mut script_with_crc = add_bk_crc_to_data(&script);
    pad_to_alignment_ff(&mut script_with_crc, 4);

    let mut out = Vec::with_capacity(1024 + 8 + script_with_crc.len());
    out.extend(std::iter::repeat_n(0xFF, 1024));
    out.extend_from_slice(&BK72XX_SCRIPT_FOTA_MAGIC.to_le_bytes());
    out.extend_from_slice(&(script_with_crc.len() as u32).to_le_bytes());
    out.extend_from_slice(&script_with_crc);

    fs::write(output_path, out).with_context(|| format!("write {}", output_path.display()))?;
    Ok(())
}

fn fnv1a_hash(data: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c9dc5;
    for &b in data {
        hash ^= b as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

fn gzip_with_fixed_header(data: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(data).context("gzip write")?;
    let mut compressed = encoder.finish().context("gzip finish")?;
    anyhow::ensure!(compressed.len() >= 10, "gzip output too small");
    compressed[..10].copy_from_slice(&BK72XX_FIXED_GZIP_HEADER);
    Ok(compressed)
}

fn aes256_cbc_pkcs7_encrypt(data: &[u8]) -> Result<Vec<u8>> {
    let cipher = Encryptor::<Aes256>::new_from_slices(BK72XX_AES_KEY, BK72XX_AES_IV).context("init AES-256-CBC")?;
    let mut buf = vec![0u8; data.len() + 16];
    buf[..data.len()].copy_from_slice(data);
    let encrypted = cipher
        .encrypt_padded_mut::<Pkcs7>(&mut buf, data.len())
        .map_err(|_| anyhow::anyhow!("AES-256-CBC padding/encrypt failed"))?;
    Ok(encrypted.to_vec())
}

/// Build BK72XX new-format full FOTA package (RBL).
pub fn build_bk72xx_full_fota_new(cp_bin_path: &Path, ap_bin_path: &Path, script_bin_path: &Path, ap_offset: u32, script_abs_addr: u32, output_path: &Path) -> Result<()> {
    anyhow::ensure!(script_abs_addr >= ap_offset, "script address(0x{script_abs_addr:08X}) < ap offset(0x{ap_offset:08X})");

    let cp_bin = fs::read(cp_bin_path).with_context(|| format!("read {}", cp_bin_path.display()))?;
    let ap_bin = fs::read(ap_bin_path).with_context(|| format!("read {}", ap_bin_path.display()))?;
    let script_raw = fs::read(script_bin_path).with_context(|| format!("read {}", script_bin_path.display()))?;

    let script_offset_flash = (script_abs_addr - ap_offset) as usize;
    let script_offset_rbl_raw = script_offset_flash * 32 / 34;
    let ap_script_offset = script_offset_rbl_raw.div_ceil(32) * 32;

    let mut ap_with_padding = ap_bin;
    ap_with_padding.extend(std::iter::repeat_n(0xFF, 8));
    anyhow::ensure!(
        ap_with_padding.len() <= ap_script_offset,
        "AP image size({}) exceeds script offset in AP({})",
        ap_with_padding.len(),
        ap_script_offset
    );
    ap_with_padding.resize(ap_script_offset, 0xFF);

    let mut script_data = trim_trailing_ff(&script_raw).to_vec();
    pad_to_alignment_ff(&mut script_data, 32);
    let mut ap_with_script = ap_with_padding;
    ap_with_script.extend_from_slice(&script_data);

    let cp_padded = if cp_bin.len() < BK72XX_CP_FIXED_BYTES {
        let mut v = cp_bin;
        v.resize(BK72XX_CP_FIXED_BYTES, 0xFF);
        v
    } else {
        cp_bin
    };
    let mut raw_payload = cp_padded;
    raw_payload.extend_from_slice(&ap_with_script);

    let compressed = gzip_with_fixed_header(&raw_payload)?;
    let encrypted = aes256_cbc_pkcs7_encrypt(&compressed)?;

    let body_crc32 = crc32fast::hash(&encrypted);
    let hash_val = fnv1a_hash(&raw_payload);
    let raw_size = raw_payload.len() as u32;
    let com_size = encrypted.len() as u32;

    let mut header = Vec::with_capacity(96);
    header.extend_from_slice(b"RBL\0");
    header.extend_from_slice(&BK72XX_FOTA_ALGO_GZIP_AES256.to_le_bytes());
    header.extend_from_slice(&[0u8; 6]); // ctime, fixed zero
    let mut part_name = [0u8; 16];
    part_name[..3].copy_from_slice(b"app");
    header.extend_from_slice(&part_name);
    let mut download_version = [0u8; 24];
    download_version[0] = b'2';
    header.extend_from_slice(&download_version);
    let mut current_version = [0u8; 24];
    current_version[..20].copy_from_slice(b"00010203040506070809");
    header.extend_from_slice(&current_version);
    header.extend_from_slice(&body_crc32.to_le_bytes());
    header.extend_from_slice(&hash_val.to_le_bytes());
    header.extend_from_slice(&raw_size.to_le_bytes());
    header.extend_from_slice(&com_size.to_le_bytes());
    anyhow::ensure!(header.len() == 92, "invalid RBL header size: {}", header.len());
    let head_crc32 = crc32fast::hash(&header);
    header.extend_from_slice(&head_crc32.to_le_bytes());

    let mut output = header;
    output.extend_from_slice(&encrypted);
    fs::write(output_path, output).with_context(|| format!("write {}", output_path.display()))?;
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
#[allow(clippy::too_many_arguments)]
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
        fs::read(common_path).with_context(|| format!("read {}", common_path.display()))?
    };
    let sdk_data = if sdk_path.metadata().map(|m| m.len()).unwrap_or(0) < 4 {
        Vec::new()
    } else {
        fs::read(sdk_path).with_context(|| format!("read {}", sdk_path.display()))?
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
    let mut out = fs::File::create(output_path).with_context(|| format!("create {}", output_path.display()))?;
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

// ── RDA8910 sector block ────────────────────────────────────────────────────

/// 组装 RDA8910 分区块：`CoreUpgrade_SectorCalMD5Struct`(36B) + RDA LZMA 载荷。
///
/// 布局对齐设备端 `bsp_common.h`（UIS8910 SDK）：
/// ```c
/// typedef struct {
///   uint32_t MaigcNum;      // __APP_START_MAGIC__（fota.magic_num）
///   uint32_t TotalLen;      // 压缩后载荷长度
///   uint32_t DataLen;       // 解压后数据长度（原始镜像大小）
///   uint8_t  MD5[16];       // 解压后数据的 MD5
///   uint32_t BlockLen;      // 压缩分块大小（信息性，解压以载荷内 lzmaFileHeader 为准）
///   uint32_t StartAddress;  // 烧写目标 Flash 偏移
/// } CoreUpgrade_SectorCalMD5Struct;  // 36 字节
/// ```
/// `compressed` 为 `dtools lzmare2` 产物（`lzmaFileHeader_t` + `lzmaBlockHeader_t` 块，
/// RDA 专用 LZMA，非标准格式）。一个分区块对应一个分区（AP / 脚本），多个分区直接拼接。
pub fn build_rda_sector_block(raw: &[u8], compressed: &[u8], magic: u32, start_addr: u32, block_len: u32) -> Vec<u8> {
    let md5_bytes: [u8; 16] = {
        use md5::{Digest, Md5};
        let mut hasher = Md5::new();
        hasher.update(raw);
        hasher.finalize().into()
    };
    let mut out = Vec::with_capacity(36 + compressed.len());
    out.extend_from_slice(&magic.to_le_bytes());
    out.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
    out.extend_from_slice(&(raw.len() as u32).to_le_bytes());
    out.extend_from_slice(&md5_bytes);
    out.extend_from_slice(&block_len.to_le_bytes());
    out.extend_from_slice(&start_addr.to_le_bytes());
    out.extend_from_slice(compressed);
    out
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
    for b in buf.iter_mut().skip(hex_len / 2) {
        *b = 0u8;
    }

    // Store as 4 little-endian u32
    for (i, out) in dst.iter_mut().take(4).enumerate() {
        let off = i * 4;
        *out = u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]);
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
        for v in &main_ver {
            crc_buf.extend_from_slice(&v.to_le_bytes());
        }
        crc_buf.extend_from_slice(&app_ver.to_le_bytes());
        for v in &std_ver {
            crc_buf.extend_from_slice(&v.to_le_bytes());
        }
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

        assemble_ota_package(magic, main_ver_dec, "0", 0, "0", app_ver, &common_path, &sdk_path, &out_path).unwrap();

        let data = fs::read(&out_path).unwrap();
        let expected_data_len = test_content.len();
        assert_eq!(data.len(), 92 + expected_data_len);

        let read_u32 = |off: usize| u32::from_le_bytes(data[off..off + 4].try_into().unwrap());
        assert_eq!(read_u32(0), magic);
        assert_eq!(read_u32(52), expected_data_len as u32);
        assert_eq!(read_u32(56), 0);
        assert_eq!(&data[92..92 + expected_data_len], test_content);

        let expected_md5 = md5::Md5::digest(test_content);
        let expected_hash: [u8; 16] = expected_md5.into();
        assert_eq!(&data[60..76], &expected_hash);

        let mut expected_crc_buf = Vec::new();
        for v in &[0u32, 0, 0, 0, main_ver_dec] {
            expected_crc_buf.extend_from_slice(&v.to_le_bytes());
        }
        expected_crc_buf.extend_from_slice(&app_ver.to_le_bytes());
        for _ in 0..5 {
            expected_crc_buf.extend_from_slice(&0u32.to_le_bytes());
        }
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
        if soc_tools.is_none() {
            eprintln!("SKIP");
            return;
        }
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
            .args([
                "zip_file",
                &format!("{:X}", magic),
                &format!("{:X}", addr),
                &input.to_string_lossy(),
                &soc_out.to_string_lossy(),
                "40000",
                "1",
            ])
            .status()
            .expect("run soc_tools.exe");
        assert!(status.success());

        let rust_data = fs::read(&rust_out).unwrap();
        let soc_data = fs::read(&soc_out).unwrap();

        if rust_data != soc_data {
            let min_len = rust_data.len().min(soc_data.len());
            let mut diffs = 0usize;
            for i in 0..min_len {
                if rust_data[i] != soc_data[i] {
                    if diffs < 20 {
                        eprintln!("DIFF offset {}: rust=0x{:02X} soc=0x{:02X}", i, rust_data[i], soc_data[i]);
                    }
                    diffs += 1;
                }
            }
            eprintln!("TOTAL DIFFS: {}", diffs);
            eprintln!(
                "MD5 rust={} soc={}",
                hex_str(md5::Md5::digest(&rust_data).as_slice()),
                hex_str(md5::Md5::digest(&soc_data).as_slice())
            );
        }
        assert_eq!(rust_data, soc_data);
    }

    /// Full pipeline: ROM → LZMA → assemble_ota → compare with soc_tools.exe
    #[test]
    #[ignore = "requires soc_tools.exe"]
    fn test_full_pipeline_matches_soc_tools() {
        let soc_tools = find_soc_tools_exe();
        if soc_tools.is_none() {
            eprintln!("SKIP");
            return;
        }
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
            .args([
                "zip_file",
                &format!("{:X}", magic),
                &format!("{:X}", addr),
                &rom_path.to_string_lossy(),
                &soc_zip.to_string_lossy(),
                "40000",
                "1",
            ])
            .status()
            .expect("soc_tools zip_file");
        assert!(zs.success());
        assert_eq!(fs::read(&rust_zip).unwrap(), fs::read(&soc_zip).unwrap());

        assemble_ota_package(magic, 0xFFFFFFFF, "0", 0, "0", 0, &rust_zip, &dummy_path, &rust_sota).unwrap();
        let ms = std::process::Command::new(&soc_tools)
            .args([
                "make_ota_file",
                &format!("{:X}", magic),
                "4294967295",
                "0",
                "0",
                "0",
                "0",
                &soc_zip.to_string_lossy(),
                &dummy_path.to_string_lossy(),
                &soc_sota.to_string_lossy(),
            ])
            .status()
            .expect("soc_tools make_ota_file");
        assert!(ms.success());

        let rust_data = fs::read(&rust_sota).unwrap();
        let soc_data = fs::read(&soc_sota).unwrap();

        if rust_data != soc_data {
            eprintln!("SIZE: rust={} soc={}", rust_data.len(), soc_data.len());
            let min = rust_data.len().min(soc_data.len());
            let mut diffs = 0;
            for i in 0..min {
                if rust_data[i] != soc_data[i] {
                    if diffs < 30 {
                        eprintln!("OFF {}: rust=0x{:02X} soc=0x{:02X}", i, rust_data[i], soc_data[i]);
                    }
                    diffs += 1;
                }
            }
            eprintln!("TOTAL DIFFS: {}", diffs);
        }
        assert_eq!(rust_data, soc_data);
    }

    fn find_soc_tools_exe() -> Option<std::path::PathBuf> {
        for c in &[
            "refs/origin_tools/tools/soc_tools.exe",
            "../refs/origin_tools/tools/soc_tools.exe",
            "../../refs/origin_tools/tools/soc_tools.exe",
            "tools/soc_tools.exe",
            "soc_tools.exe",
        ] {
            let p = std::path::PathBuf::from(c);
            if p.exists() {
                return Some(p);
            }
        }
        None
    }

    fn hex_str(data: &[u8]) -> String {
        data.iter().map(|b| format!("{:02x}", b)).collect()
    }

    #[test]
    fn test_bk72xx_script_fota_new_format() {
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("script.bin");
        let out = tmp.path().join("script_fota.bin");
        fs::write(&script, b"print('hello')\n\xFF\xFF\xFF").unwrap();

        build_bk72xx_script_fota_new(&script, &out).unwrap();

        let data = fs::read(&out).unwrap();
        assert!(data.len() > 1032);
        assert!(data[..1024].iter().all(|&b| b == 0xFF));
        assert_eq!(u32::from_le_bytes(data[1024..1028].try_into().unwrap()), 0x4C554154);
        let payload_len = u32::from_le_bytes(data[1028..1032].try_into().unwrap()) as usize;
        assert_eq!(payload_len, data.len() - 1032);
    }

    #[test]
    fn test_bk72xx_full_fota_new_format_header() {
        let tmp = tempfile::tempdir().unwrap();
        let cp = tmp.path().join("cp.bin");
        let ap = tmp.path().join("ap.bin");
        let script = tmp.path().join("script.bin");
        let out = tmp.path().join("full_fota.bin");

        fs::write(&cp, vec![0x11; 512]).unwrap();
        fs::write(&ap, vec![0x22; 1024]).unwrap();
        fs::write(&script, b"print('bk72xx')\n\xFF\xFF").unwrap();

        build_bk72xx_full_fota_new(&cp, &ap, &script, 0x1000, 0x1800, &out).unwrap();

        let data = fs::read(&out).unwrap();
        assert!(data.len() > 96);
        assert_eq!(&data[0..4], b"RBL\0");
        assert_eq!(u32::from_le_bytes(data[92..96].try_into().unwrap()), crc32fast::hash(&data[..92]));
    }

    #[test]
    fn test_build_rda_sector_block() {
        let raw = b"rda8910 raw image payload";
        let compressed = vec![0xAA; 64];
        let magic = 0xDA18_8800u32;
        let out = build_rda_sector_block(raw, &compressed, magic, 0x10000, 0x10000);

        assert_eq!(out.len(), 36 + compressed.len());
        assert_eq!(u32::from_le_bytes(out[0..4].try_into().unwrap()), magic);
        assert_eq!(u32::from_le_bytes(out[4..8].try_into().unwrap()), compressed.len() as u32, "TotalLen");
        assert_eq!(u32::from_le_bytes(out[8..12].try_into().unwrap()), raw.len() as u32, "DataLen");
        assert_eq!(u32::from_le_bytes(out[28..32].try_into().unwrap()), 0x10000, "BlockLen");
        assert_eq!(u32::from_le_bytes(out[32..36].try_into().unwrap()), 0x10000, "StartAddress");
        assert_eq!(&out[36..], &compressed[..]);
        let expect_md5 = md5::Md5::digest(raw);
        assert_eq!(&out[12..28], expect_md5.as_slice(), "MD5 of raw data");
    }
}
