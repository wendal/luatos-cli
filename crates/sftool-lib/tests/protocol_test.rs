//! Protocol-level tests for command serialization, response parsing,
//! and handshake sequence validation.
//!
//! These tests do NOT require real hardware; they verify the protocol
//! message construction and response parsing logic directly.

use sftool_lib::common::ram_command::{Command, RESPONSE_STR_TABLE, Response};
use std::str::FromStr;

/// Test the complete handshake command/response sequence.
///
/// Verifies that:
/// 1. All RAM commands serialize to the expected wire format
/// 2. The serialized command strings end with '\r' (required by the protocol)
/// 3. Responses are correctly parsed from their wire representation
#[test]
fn test_handshake_sequence() {
    // --- Step 1: Verify command serialization for the full handshake flow ---

    // The handshake sequence after stub download involves:
    //   1) set_speed (SetBaud)
    //   2) erase_all (EraseAll)
    //   3) write data (Write / WriteAndErase)
    //   4) verify (Verify)
    //   5) soft_reset (SoftReset)

    let commands_and_expected: Vec<(Command, &str)> = vec![
        (Command::SetBaud { baud: 921600, delay: 10 }, "burn_speed 921600 10\r"),
        (Command::EraseAll { address: 0x12000000 }, "burn_erase_all 0x12000000\r"),
        (Command::WriteAndErase { address: 0x12000000, len: 0x1000 }, "burn_erase_write 0x12000000 0x00001000\r"),
        (Command::Write { address: 0x12001000, len: 0x200 }, "burn_write 0x12001000 0x00000200\r"),
        (
            Command::Verify {
                address: 0x12000000,
                len: 0x1000,
                crc: 0xDEADBEEF,
            },
            "burn_verify 0x12000000 0x00001000 0xdeadbeef\r",
        ),
        (Command::Erase { address: 0x12000000, len: 0x1000 }, "burn_erase 0x12000000 0x00001000\r"),
        (Command::Read { address: 0x12000000, len: 0x100 }, "burn_read 0x12000000 0x00000100\r"),
        (Command::SoftReset, "burn_reset\r"),
    ];

    for (cmd, expected_str) in &commands_and_expected {
        let serialized = cmd.to_string();
        assert_eq!(serialized, *expected_str, "Command {:?} serialized to '{}', expected '{}'", cmd, serialized, expected_str);
        // Every command must end with '\r'
        assert!(serialized.ends_with('\r'), "Command '{}' does not end with \\r", serialized);
    }

    // --- Step 2: Verify response parsing for handshake responses ---
    // After each command, the device responds with one of: OK, Fail, RX_WAIT

    let response_pairs: Vec<(&str, Response)> = vec![("OK", Response::Ok), ("Fail", Response::Fail), ("RX_WAIT", Response::RxWait)];

    for (wire_str, expected_response) in &response_pairs {
        let parsed = Response::from_str(wire_str).expect(&format!("Failed to parse response '{}'", wire_str));
        assert_eq!(
            parsed, *expected_response,
            "Response '{}' parsed as {:?}, expected {:?}",
            wire_str, parsed, expected_response
        );
    }

    // --- Step 3: Verify the response string table matches all known responses ---
    assert_eq!(RESPONSE_STR_TABLE.len(), 3);
    assert!(RESPONSE_STR_TABLE.contains(&"OK"));
    assert!(RESPONSE_STR_TABLE.contains(&"Fail"));
    assert!(RESPONSE_STR_TABLE.contains(&"RX_WAIT"));
}

/// Test protocol version negotiation logic.
///
/// The sftool-lib protocol uses text-based commands. "Version negotiation"
/// in this context means:
/// - Commands with different parameter values produce distinct wire formats
/// - The same command type with different parameters serializes correctly
/// - Edge cases (zero addresses, max values) are handled
#[test]
fn test_protocol_version_negotiation() {
    // Test that the same command type with different parameters produces
    // different but correctly formatted strings.

    // EraseAll with different addresses
    let cmd1 = Command::EraseAll { address: 0x00000000 };
    let cmd2 = Command::EraseAll { address: 0xFFFFFFFF };
    assert_ne!(cmd1.to_string(), cmd2.to_string());
    assert_eq!(cmd1.to_string(), "burn_erase_all 0x00000000\r");
    assert_eq!(cmd2.to_string(), "burn_erase_all 0xffffffff\r");

    // SetBaud with different baud rates and delays
    let baud_rates: Vec<(u32, u32, &str)> = vec![
        (115200, 10, "burn_speed 115200 10\r"),
        (921600, 10, "burn_speed 921600 10\r"),
        (1000000, 5, "burn_speed 1000000 5\r"),
        (2000000, 0, "burn_speed 2000000 0\r"),
    ];
    for (baud, delay, expected) in &baud_rates {
        let cmd = Command::SetBaud { baud: *baud, delay: *delay };
        assert_eq!(cmd.to_string(), *expected, "SetBaud({}, {}) should produce '{}'", baud, delay, expected);
    }

    // Verify with different CRC values
    let verify_cmds: Vec<(u32, u32, u32, &str)> = vec![
        (0, 0, 0, "burn_verify 0x00000000 0x00000000 0x00000000\r"),
        (0x12000000, 0x10000, 0xABCD1234, "burn_verify 0x12000000 0x00010000 0xabcd1234\r"),
    ];
    for (addr, len, crc, expected) in &verify_cmds {
        let cmd = Command::Verify {
            address: *addr,
            len: *len,
            crc: *crc,
        };
        assert_eq!(cmd.to_string(), *expected);
    }

    // Verify that Response enum round-trips correctly
    for resp_str in RESPONSE_STR_TABLE.iter() {
        let parsed = Response::from_str(resp_str).unwrap();
        let back_to_str = parsed.to_string();
        assert_eq!(back_to_str, *resp_str, "Response round-trip failed: '{}' -> {:?} -> '{}'", resp_str, parsed, back_to_str);
    }

    // Invalid response strings should fail to parse
    assert!(Response::from_str("INVALID").is_err());
    assert!(Response::from_str("").is_err());
    assert!(Response::from_str("ok").is_err()); // case-sensitive
    assert!(Response::from_str("FAIL").is_err());
}

/// Test error response handling.
///
/// Verifies that:
/// 1. All error types in the Error enum can be constructed and display correctly
/// 2. Protocol errors carry the expected messages
/// 3. CRC mismatch errors format hex values properly
/// 4. Response parsing failures produce appropriate errors
#[test]
fn test_error_response_handling() {
    use sftool_lib::Error;

    // --- Protocol errors ---
    let proto_err = Error::protocol("unexpected response from device");
    assert!(
        proto_err.to_string().contains("unexpected response from device"),
        "Protocol error message not found: {}",
        proto_err
    );

    let proto_err2 = Error::protocol("handshake failed after 3 retries");
    assert!(proto_err2.to_string().contains("handshake failed"));

    // --- Invalid input errors ---
    let input_err = Error::invalid_input("bad address format");
    assert!(input_err.to_string().contains("bad address format"));

    // --- Timeout errors ---
    let timeout_err = Error::timeout("waiting for RAM command response");
    assert!(timeout_err.to_string().contains("waiting for RAM command response"));

    // --- CRC mismatch errors ---
    let crc_err = Error::CrcMismatch {
        expected: 0xDEADBEEF,
        actual: 0xCAFEBABE,
    };
    let crc_msg = crc_err.to_string();
    assert!(
        crc_msg.contains("0xDEADBEEF") || crc_msg.contains("0xdeadbeef"),
        "CRC error should contain expected hex: {}",
        crc_msg
    );
    assert!(
        crc_msg.contains("0xCAFEBABE") || crc_msg.contains("0xcafebabe"),
        "CRC error should contain actual hex: {}",
        crc_msg
    );

    // --- Config errors ---
    let config_err = Error::Config("unsupported memory type".into());
    assert!(config_err.to_string().contains("unsupported memory type"));

    // --- Unsupported chip/memory errors ---
    let chip_err = Error::UnsupportedChip("SF32LB99".into());
    assert!(chip_err.to_string().contains("SF32LB99"));

    let mem_err = Error::UnsupportedMemory("qspi".into());
    assert!(mem_err.to_string().contains("qspi"));

    // --- Missing embedded asset ---
    let asset_err = Error::MissingEmbeddedAsset("ram_patch_99X.bin");
    assert!(asset_err.to_string().contains("ram_patch_99X.bin"));

    // --- Response parsing errors mapped to library errors ---
    let parse_result = Response::from_str("UNKNOWN_RESPONSE");
    assert!(parse_result.is_err());

    // Verify that the "Fail" response is parseable (not an error condition in parsing,
    // but the caller should handle it as a protocol failure)
    let fail_response = Response::from_str("Fail").unwrap();
    assert_eq!(fail_response, Response::Fail);

    // --- IO error conversion ---
    let io_err = std::io::Error::new(std::io::ErrorKind::TimedOut, "serial read timeout");
    let lib_err: Error = io_err.into();
    assert!(lib_err.to_string().contains("serial read timeout"));
}
