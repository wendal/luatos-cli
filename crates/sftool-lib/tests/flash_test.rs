//! Hardware-dependent flash tests.
//!
//! These tests require a real device connected via serial port and are
//! therefore marked with `#[ignore]` so they are skipped in CI.
//! Run them manually with: `cargo test -p sftool-lib -- --ignored`

/// Test flash erase operation on real hardware.
///
/// Requires a connected SF32LB58 device. Set the `SFTOOL_TEST_PORT`
/// environment variable to the serial port name (e.g. "COM3").
#[test]
#[ignore]
fn test_flash_erase() {
    todo!("Requires real hardware: connect an SF32LB58 device and implement flash erase test")
}

/// Test flash read-back and verification on real hardware.
///
/// Requires a connected SF32LB58 device. Writes known data to flash,
/// reads it back, and verifies the content matches.
#[test]
#[ignore]
fn test_flash_read_back() {
    todo!("Requires real hardware: connect an SF32LB58 device and implement flash read-back test")
}
