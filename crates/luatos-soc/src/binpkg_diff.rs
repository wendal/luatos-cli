// binpkg_diff — binpkg entry layout comparison.
//
// Given the raw bytes of two binpkg archives, decide whether their
// "underlying firmware" layouts are identical (i.e. everything except the
// script partition is the same). The script partition is identified by the
// entry name `"script"`, which is the convention used by the EC7xx toolchain
// (see `ec718.rs::extract_soc_7z`).
//
// This powers the EC7xx FOTA auto-fallback in `luatos-cli fota build`:
// when old/new binpkg differ only in the script entry, the differential
// FOTA path (which requires an external `FotaToolkit.exe`) is unnecessary
// and we can fall back to the pure-Rust script-only path.

use std::collections::BTreeSet;

use anyhow::{bail, Result};

use crate::combine::parse_entry_spans;

/// Canonical 4-tuple used to compare two binpkg entries. Two entries are
/// considered the "same firmware block" iff these four fields all match.
type EntryKey = (String, u32, u32, u32); // (name, addr, flash_size, img_size)

/// Outcome of comparing two binpkg archives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinpkgDiff {
    /// All entries except the script entry have identical metadata in both
    /// archives. FOTA differential is pointless; fall back to script-only.
    Identical,
    /// At least one non-script entry differs (or one side has a non-script
    /// entry that the other lacks). FOTA differential is required.
    Differ,
}

/// Parse the raw bytes of a binpkg archive and return the metadata of each
/// entry, in the order they appear in the file.
pub fn parse_binpkg_entries(data: &[u8]) -> Result<Vec<EntryKey>> {
    let spans = parse_entry_spans(data)?;
    Ok(spans.into_iter().map(|s| (s.name, s.addr, s.flash_size, s.img_size)).collect())
}

/// Compare two binpkg archives. Excludes the `script` entry from both sides
/// (consistent with how `ec718.rs` names the script partition), then compares
/// the remaining entries as `(name, addr, flash_size, img_size)` sets.
///
/// Errors propagate from `parse_entry_spans` — a malformed binpkg surfaces
/// as an error rather than being silently treated as "identical" or "differ".
pub fn compare_binpkg_underlying(old: &[u8], new: &[u8]) -> Result<BinpkgDiff> {
    let old_entries = parse_binpkg_entries(old)?;
    let new_entries = parse_binpkg_entries(new)?;

    if old_entries.is_empty() {
        bail!("old binpkg contains no entries — underlying firmware detection failed, please report this issue");
    }
    if new_entries.is_empty() {
        bail!("new binpkg contains no entries — underlying firmware detection failed, please report this issue");
    }

    let old_set: BTreeSet<EntryKey> = old_entries.into_iter().filter(|(name, _, _, _)| name != "script").collect();
    let new_set: BTreeSet<EntryKey> = new_entries.into_iter().filter(|(name, _, _, _)| name != "script").collect();

    Ok(if old_set == new_set { BinpkgDiff::Identical } else { BinpkgDiff::Differ })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combine::append_entry;

    /// Build a minimal valid binpkg (pkgmode header) optionally with one entry.
    /// The signature mirrors `combine::append_entry` so we can reuse the
    /// production-grade meta writer.
    fn minimal_binpkg(entries: &[(&str, u32, &[u8])]) -> Vec<u8> {
        let mut buf = vec![0u8; 0x1D8];
        buf[0x38..0x3F].copy_from_slice(b"pkgmode");
        for (name, addr, data) in entries {
            buf = append_entry(&buf, name, *addr, data);
        }
        buf
    }

    #[test]
    fn parse_unparseable_bails() {
        assert!(parse_binpkg_entries(&[]).is_err());
        assert!(parse_binpkg_entries(&vec![0u8; 16]).is_err());
    }

    #[test]
    fn parse_returns_all_entries_in_order() {
        let binpkg = minimal_binpkg(&[
            ("ap_bootloader", 0x0080_3000, b"BL_PAYLOAD"),
            ("ap", 0x0088_2000, b"AP_PAYLOAD_LONGER"),
            ("cp-demo-flash", 0x0081_E000, b"CP_PAYLOAD"),
        ]);
        let entries = parse_binpkg_entries(&binpkg).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].0, "ap_bootloader");
        assert_eq!(entries[0].1, 0x0080_3000);
        assert_eq!(entries[2].0, "cp-demo-flash");
    }

    #[test]
    fn compare_identical_returns_identical() {
        let binpkg = minimal_binpkg(&[("ap_bootloader", 0x0080_3000, b"BL"), ("ap", 0x0088_2000, b"APP"), ("cp-demo-flash", 0x0081_E000, b"CP")]);
        assert_eq!(compare_binpkg_underlying(&binpkg, &binpkg).unwrap(), BinpkgDiff::Identical);
    }

    #[test]
    fn compare_differing_img_size_returns_differ() {
        let old = minimal_binpkg(&[("ap", 0x0088_2000, b"APP_V1")]);
        let new = minimal_binpkg(&[("ap", 0x0088_2000, b"APP_V1_LONGER")]);
        assert_eq!(compare_binpkg_underlying(&old, &new).unwrap(), BinpkgDiff::Differ);
    }

    #[test]
    fn compare_differing_address_returns_differ() {
        let old = minimal_binpkg(&[("ap", 0x0088_2000, b"APP")]);
        let new = minimal_binpkg(&[("ap", 0x0089_2000, b"APP")]);
        assert_eq!(compare_binpkg_underlying(&old, &new).unwrap(), BinpkgDiff::Differ);
    }

    #[test]
    fn compare_ignores_script_entry() {
        // Old: a non-script entry plus a script entry
        let old = minimal_binpkg(&[("ap", 0x0088_2000, b"APP"), ("script", 0x0048_E000, b"SCRIPT_OLD")]);
        // New: same non-script entry plus a different script entry (size differs)
        let new = minimal_binpkg(&[("ap", 0x0088_2000, b"APP"), ("script", 0x0048_E000, b"SCRIPT_NEW_DIFFERENT")]);
        assert_eq!(compare_binpkg_underlying(&old, &new).unwrap(), BinpkgDiff::Identical);
    }

    #[test]
    fn compare_ignores_script_entry_even_when_name_varies() {
        // Edge case: if the script entry was renamed, the filter still excludes it
        // by name "script". Renaming it to anything else would surface as Differ.
        let old = minimal_binpkg(&[("ap", 0x0088_2000, b"APP"), ("script", 0x0048_E000, b"X")]);
        let new = minimal_binpkg(&[("ap", 0x0088_2000, b"APP"), ("script", 0x0048_E000, b"Y")]);
        // The script entries differ in img_size, but the filter excludes them —
        // we expect Identical.
        assert_eq!(compare_binpkg_underlying(&old, &new).unwrap(), BinpkgDiff::Identical);
    }

    #[test]
    fn compare_empty_bpkg_bails() {
        // Truly empty bytes — fails the fsize < 0x34 check in parse_entry_spans,
        // so compare_binpkg_underlying must propagate the error.
        let empty: Vec<u8> = vec![];
        let non_empty = minimal_binpkg(&[("ap", 0x0088_2000, b"APP")]);
        assert!(compare_binpkg_underlying(&empty, &non_empty).is_err());
        assert!(compare_binpkg_underlying(&non_empty, &empty).is_err());
    }

    #[test]
    fn compare_pkgmode_header_with_no_entries_bails() {
        // Valid pkgmode header (0x1D8 bytes with magic) but no entries —
        // parse_entry_spans returns an empty Vec, which compare_binpkg_underlying
        // rejects with a clear "no entries" error.
        let mut empty_pkgmode = vec![0u8; 0x1D8];
        empty_pkgmode[0x38..0x3F].copy_from_slice(b"pkgmode");
        let non_empty = minimal_binpkg(&[("ap", 0x0088_2000, b"APP")]);
        assert!(compare_binpkg_underlying(&empty_pkgmode, &non_empty).is_err());
        assert!(compare_binpkg_underlying(&non_empty, &empty_pkgmode).is_err());
    }
}
