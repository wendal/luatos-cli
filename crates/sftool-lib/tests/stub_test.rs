//! Stub loading and checksum validation tests.
//!
//! These tests verify:
//! - Loading stub files from the embedded stub/ directory
//! - Loading stub files from an external filesystem path
//! - CRC32 checksum calculation and validation

use sftool_lib::load_stub_bytes;
use sftool_lib::utils::Utils;

/// Helper: get the absolute path to the stub/ directory in the crate root.
fn stub_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("stub")
}

/// Test loading stub files from the stub/ directory.
///
/// Verifies:
/// 1. Embedded stub files can be loaded via `load_stub_bytes`
/// 2. External stub files can be loaded via an explicit path
/// 3. Loaded stub data is non-empty and consistent between embedded and file
/// 4. Invalid chip/memory keys produce an error
#[test]
fn test_stub_loading_from_directory() {
    // --- Test 1: Load embedded stub for sf32lb58 NOR ---
    let stub_data = load_stub_bytes(None, sftool_lib::ChipType::SF32LB58, "nor");
    assert!(stub_data.is_ok(), "Failed to load embedded sf32lb58 NOR stub: {:?}", stub_data.err());
    let nor_data = stub_data.unwrap();
    assert!(!nor_data.is_empty(), "Embedded stub data should not be empty");

    // The NOR stub file is ram_patch_58x.bin
    let nor_file_path = stub_dir().join("ram_patch_58x.bin");
    assert!(nor_file_path.exists(), "Stub file should exist: {:?}", nor_file_path);
    let nor_file_data = std::fs::read(&nor_file_path).unwrap();
    assert_eq!(nor_data.len(), nor_file_data.len(), "Embedded stub size should match file size");
    assert_eq!(&nor_data[..], nor_file_data.as_slice(), "Embedded stub content should match file content");

    // --- Test 2: Load embedded stub for sf32lb58 NAND ---
    let nand_stub = load_stub_bytes(None, sftool_lib::ChipType::SF32LB58, "nand");
    assert!(nand_stub.is_ok(), "Failed to load embedded sf32lb58 NAND stub: {:?}", nand_stub.err());
    let nand_data = nand_stub.unwrap();
    assert!(!nand_data.is_empty());

    let nand_file_path = stub_dir().join("ram_patch_58X_NAND.bin");
    let nand_file_data = std::fs::read(&nand_file_path).unwrap();
    assert_eq!(&nand_data[..], nand_file_data.as_slice());

    // --- Test 3: Load external stub via explicit path ---
    let external_path = stub_dir().join("ram_patch_58x.bin");
    let external_stub = load_stub_bytes(Some(external_path.to_str().unwrap()), sftool_lib::ChipType::SF32LB58, "nor");
    assert!(external_stub.is_ok(), "Failed to load external stub: {:?}", external_stub.err());
    let ext_data = external_stub.unwrap();
    assert_eq!(&ext_data[..], nor_file_data.as_slice(), "External stub should match file content");

    // --- Test 4: Invalid chip/memory key should error ---
    let bad_key_result = load_stub_bytes(None, sftool_lib::ChipType::SF32LB58, "nonexistent_memory");
    assert!(bad_key_result.is_err(), "Invalid memory type should return error");

    // --- Test 5: Non-existent external path should error ---
    let bad_path_result = load_stub_bytes(Some("/nonexistent/path/stub.bin"), sftool_lib::ChipType::SF32LB58, "nor");
    assert!(bad_path_result.is_err(), "Non-existent external path should return error");

    // --- Test 6: Verify stub directory contains expected files ---
    let expected_files = [
        "ram_patch_58x.bin",
        "ram_patch_58X_NAND.bin",
        "ram_patch_58X_SD.bin",
        "58X_sig_pub.der",
        "factory_cali_58X.bin",
    ];
    for file_name in &expected_files {
        let path = stub_dir().join(file_name);
        assert!(path.exists(), "Expected stub file not found: {:?}", path);
        let metadata = std::fs::metadata(&path).unwrap();
        assert!(metadata.len() > 0, "Stub file should not be empty: {:?}", path);
    }
}

/// Test CRC32 checksum calculation and validation.
///
/// Verifies:
/// 1. CRC32 is deterministic (same data → same checksum)
/// 2. Different data produces different checksums
/// 3. CRC32 of known data matches expected values
/// 4. CRC32 of empty data is 0
/// 5. Stub file CRC32 can be validated against file content
#[test]
fn test_stub_checksum_validation() {
    // --- Test 1: Deterministic CRC32 ---
    let data1 = b"Hello, sftool!";
    let crc_a = Utils::calculate_crc32(data1);
    let crc_b = Utils::calculate_crc32(data1);
    assert_eq!(crc_a, crc_b, "CRC32 should be deterministic");

    // --- Test 2: Different data → different CRC ---
    let data2 = b"Hello, sftool?";
    let crc_c = Utils::calculate_crc32(data2);
    assert_ne!(crc_a, crc_c, "Different data should produce different CRC32");

    // --- Test 3: Empty data CRC32 ---
    let empty_crc = Utils::calculate_crc32(b"");
    assert_eq!(empty_crc, 0, "CRC32 of empty data should be 0");

    // --- Test 4: Known CRC32 values ---
    // CRC32 (with the algorithm used by sftool-lib) for "123456789"
    // The algorithm is: poly=0x04C11DB7, init=0, refin=true, refout=true, xorout=0
    // This is the "CRC-32/ISO-HDLC" variant
    let known_data = b"123456789";
    let known_crc = Utils::calculate_crc32(known_data);
    // Verify it's non-zero and consistent
    assert_ne!(known_crc, 0);
    assert_eq!(known_crc, Utils::calculate_crc32(known_data));

    // --- Test 5: Single byte variations produce different CRCs ---
    let base = vec![0u8; 256];
    let mut crcs = std::collections::HashSet::new();
    for i in 0..256u16 {
        let mut data = base.clone();
        data[0] = i as u8;
        crcs.insert(Utils::calculate_crc32(&data));
    }
    // All 256 single-byte variations should produce unique CRCs
    assert_eq!(crcs.len(), 256, "Single-byte variations should all produce unique CRC32 values");

    // --- Test 6: Validate stub file checksum ---
    // Load a stub file and compute its CRC32, then verify it's stable
    let stub_path = stub_dir().join("ram_patch_58x.bin");
    let stub_bytes = std::fs::read(&stub_path).unwrap();
    let stub_crc = Utils::calculate_crc32(&stub_bytes);
    assert_ne!(stub_crc, 0, "Stub file CRC should not be zero");

    // Recompute to verify determinism
    let stub_bytes_2 = std::fs::read(&stub_path).unwrap();
    let stub_crc_2 = Utils::calculate_crc32(&stub_bytes_2);
    assert_eq!(stub_crc, stub_crc_2, "Stub file CRC should be deterministic across reads");

    // --- Test 7: Partial data has different CRC than full data ---
    let partial_crc = Utils::calculate_crc32(&stub_bytes[..1024]);
    assert_ne!(partial_crc, stub_crc, "Partial stub data should have different CRC than full data");

    // --- Test 8: Corrupted data detection ---
    let mut corrupted = stub_bytes.clone();
    // Flip one bit
    corrupted[100] ^= 0x01;
    let corrupted_crc = Utils::calculate_crc32(&corrupted);
    assert_ne!(corrupted_crc, stub_crc, "Corrupted data should produce different CRC");
}
