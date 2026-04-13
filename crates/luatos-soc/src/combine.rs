// EC7xx SOC firmware combine.
//
// Injects a custom binary blob into an EC7xx .soc archive at a user-specified
// flash address. The binary is added as a new "flex_combine" binpkg entry so
// the existing flash protocol burns it correctly at the given address.
//
// Supported chips: ec7xx / air8000 / air780e* family.

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::fs;

use crate::pack::pack_soc_7z;
use crate::unpack::extract_7z;

// ─── Binpkg format constants (mirrors ec718.rs internals) ───────────────────

const ENTRY_META_SIZE: usize = 364;
const PKGMODE_MAGIC: &[u8] = b"pkgmode";

// ─── Internal minimal entry descriptor (address+size only) ─────────────────

struct EntrySpan {
    name: String,
    addr: u32,
    end: u32, // addr + max(flash_size, img_size)
}

// ─── Public API ─────────────────────────────────────────────────────────────

/// Inject `user_data` into an EC7xx `.soc` archive at flash address `hex_addr`,
/// writing the result to `out_path`.
///
/// The archive is extracted, the `.binpkg` is updated with a new `flex_combine`
/// entry, then repacked. Only EC7xx / Air8000 family chips are accepted.
pub fn combine_ec7xx_soc(soc_path: &str, user_data: &[u8], hex_addr: u32, out_path: &str) -> Result<()> {
    // ── 1. Check chip ────────────────────────────────────────────────────────
    let info = crate::read_soc_info(soc_path)?;
    let chip = info.chip.chip_type.as_str();
    if !is_ec7xx(chip) {
        bail!(
            "soc combine only supports EC7xx / Air8000 chips, got '{chip}'.\n\
               Supported: ec7xx, air8000, air780epm, air780ehm, air780ehv, air780ehg, air780epv"
        );
    }

    // ── 2. Extract archive ───────────────────────────────────────────────────
    let tempdir = tempfile::tempdir().context("create temp dir")?;
    let temppath = tempdir.path();
    extract_7z(soc_path, temppath)?;

    // ── 3. Find and read .binpkg ─────────────────────────────────────────────
    let binpkg_path = fs::read_dir(temppath)
        .context("read extracted dir")?
        .filter_map(|e| e.ok())
        .find(|e| e.file_name().to_string_lossy().ends_with(".binpkg"))
        .map(|e| e.path())
        .ok_or_else(|| anyhow::anyhow!("no .binpkg found in '{soc_path}'"))?;

    let binpkg_data = fs::read(&binpkg_path).context("read binpkg")?;

    // ── 4. Parse existing entry spans for collision check ────────────────────
    let spans = parse_entry_spans(&binpkg_data)?;

    let user_end = hex_addr
        .checked_add(user_data.len() as u32)
        .ok_or_else(|| anyhow::anyhow!("address + data length overflows u32"))?;

    for span in &spans {
        if hex_addr < span.end && user_end > span.addr {
            bail!(
                "0x{:08X}..0x{:08X} overlaps existing entry '{}' (0x{:08X}..0x{:08X})",
                hex_addr,
                user_end,
                span.name,
                span.addr,
                span.end
            );
        }
    }

    // ── 5. Append new entry to binpkg bytes ──────────────────────────────────
    let modified = append_entry(&binpkg_data, "flex_combine", hex_addr, user_data);
    fs::write(&binpkg_path, &modified).context("write modified binpkg")?;

    // ── 6. Repack .soc ───────────────────────────────────────────────────────
    pack_soc_7z(temppath, out_path).context("repack soc")?;

    log::info!("combine: injected {} bytes at 0x{:08X} into '{}' → '{}'", user_data.len(), hex_addr, soc_path, out_path);
    Ok(())
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn is_ec7xx(chip: &str) -> bool {
    matches!(chip, "ec7xx" | "air8000" | "air780epm" | "air780ehm" | "air780ehv" | "air780ehg" | "air780epv")
}

/// Read only the address + size of each binpkg entry (no data allocated).
fn parse_entry_spans(data: &[u8]) -> Result<Vec<EntrySpan>> {
    let fsize = data.len();
    if fsize < 0x34 {
        bail!("binpkg too small ({fsize} bytes)");
    }

    let foffset = if fsize > 0x3F && &data[0x38..0x3F] == PKGMODE_MAGIC { 0x1D8 } else { 0x34 };

    let mut spans = Vec::new();
    let mut cursor = foffset;

    while cursor + ENTRY_META_SIZE <= fsize {
        let meta = &data[cursor..cursor + ENTRY_META_SIZE];

        let name = null_str(&meta[0..64]);
        let addr = u32::from_le_bytes(meta[64..68].try_into().unwrap());
        let flash_size = u32::from_le_bytes(meta[68..72].try_into().unwrap());
        let img_size = u32::from_le_bytes(meta[76..80].try_into().unwrap());

        cursor += ENTRY_META_SIZE;
        cursor += img_size as usize; // skip past data

        let end = addr.saturating_add(flash_size.max(img_size));
        spans.push(EntrySpan { name, addr, end });
    }

    Ok(spans)
}

/// Append a new binpkg entry (metadata + data) to the raw binpkg bytes.
///
/// The identifier is "flex_combine" so `entry_to_burn_type` in ec718.rs
/// routes it as a `FlexFile` burn at the user address.
fn append_entry(original: &[u8], name: &str, addr: u32, data: &[u8]) -> Vec<u8> {
    let hash = format!("{:x}", Sha256::digest(data));
    let img_size = data.len() as u32;

    let mut meta = vec![0u8; ENTRY_META_SIZE];

    // name: 0..64 null-terminated
    let name_bytes = name.as_bytes();
    let copy_len = name_bytes.len().min(63);
    meta[..copy_len].copy_from_slice(&name_bytes[..copy_len]);

    // addr: 64..68
    meta[64..68].copy_from_slice(&addr.to_le_bytes());
    // flash_size: 68..72
    meta[68..72].copy_from_slice(&img_size.to_le_bytes());
    // offset: 72..76 — 0
    // img_size: 76..80
    meta[76..80].copy_from_slice(&img_size.to_le_bytes());
    // hash: 80..336 null-terminated (SHA256 hex, 64 chars)
    let hash_bytes = hash.as_bytes();
    let hash_copy = hash_bytes.len().min(255);
    meta[80..80 + hash_copy].copy_from_slice(&hash_bytes[..hash_copy]);
    // image_type: 336..352 — "AP"
    meta[336..338].copy_from_slice(b"AP");

    let mut out = original.to_vec();
    out.extend_from_slice(&meta);
    out.extend_from_slice(data);
    out
}

/// Extract a null-terminated string from a byte slice.
fn null_str(bytes: &[u8]) -> String {
    bytes.split(|&b| b == 0).next().map(|s| String::from_utf8_lossy(s).to_string()).unwrap_or_default()
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_entry_round_trip() {
        // Build a minimal pkgmode-style binpkg header (0x1D8 bytes of zeros with magic)
        let mut base = vec![0u8; 0x1D8];
        base[0x38..0x3F].copy_from_slice(PKGMODE_MAGIC);

        let payload = b"HELLO_COMBINE";
        let modified = append_entry(&base, "flex_combine", 0x00D00000, payload);

        // Modified should be at least base + ENTRY_META_SIZE + payload
        assert_eq!(modified.len(), base.len() + ENTRY_META_SIZE + payload.len());

        // Name at offset 0x1D8
        assert_eq!(&modified[0x1D8..0x1D8 + 13], b"flex_combine\0");

        // Addr at offset 0x1D8 + 64
        let addr = u32::from_le_bytes(modified[0x1D8 + 64..0x1D8 + 68].try_into().unwrap());
        assert_eq!(addr, 0x00D00000);

        // img_size at offset 0x1D8 + 76
        let sz = u32::from_le_bytes(modified[0x1D8 + 76..0x1D8 + 80].try_into().unwrap());
        assert_eq!(sz, payload.len() as u32);

        // Data starts at 0x1D8 + ENTRY_META_SIZE
        let data_start = 0x1D8 + ENTRY_META_SIZE;
        assert_eq!(&modified[data_start..data_start + payload.len()], payload);
    }

    #[test]
    fn parse_spans_detects_overlap() {
        let mut base = vec![0u8; 0x1D8];
        base[0x38..0x3F].copy_from_slice(PKGMODE_MAGIC);

        // Append a dummy entry at 0x00C00000, size 0x100000
        let payload = vec![0xAAu8; 0x100];
        let with_entry = append_entry(&base, "ap", 0x00C00000, &payload);

        let spans = parse_entry_spans(&with_entry).unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].addr, 0x00C00000);
        assert_eq!(spans[0].end, 0x00C00000 + 0x100);
    }

    #[test]
    fn is_ec7xx_chip_filter() {
        assert!(is_ec7xx("ec7xx"));
        assert!(is_ec7xx("air8000"));
        assert!(is_ec7xx("air780ehm"));
        assert!(!is_ec7xx("bk72xx"));
        assert!(!is_ec7xx("air6208"));
        assert!(!is_ec7xx("air1601"));
    }
}
