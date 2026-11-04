use std::sync::{atomic::AtomicBool, Arc};

use luatos_flash::{validate_script_image, ProgressCallback};
use luatos_soc::SocInfo;

fn info(chip: &str, capacity: serde_json::Value) -> SocInfo {
    serde_json::from_value(serde_json::json!({
        "chip": {"type": chip},
        "rom": {"file": "rom.bin", "fs": {"script": {"size": capacity, "bkcrc": false}}},
        "script": {"file": "script.bin", "use-luac": false, "bitw": 32},
        "download": {"script_addr": "200000"}
    }))
    .unwrap()
}

#[test]
fn script_capacity_boundaries_and_padding() {
    let soc = info("ec7xx", 400.into());
    for alignment in [1, 1024, 4096] {
        validate_script_image(&soc, 400 * 1024 - 1, alignment).unwrap();
        validate_script_image(&soc, 400 * 1024, alignment).unwrap();
        for size in [400 * 1024 + 1, 500 * 1024] {
            let error = validate_script_image(&soc, size, alignment).unwrap_err().to_string();
            assert!(error.contains("超过分区容量（409600 字节"), "{error}");
            assert!(error.contains(&size.to_string()), "{error}");
        }
    }
    // 401 KB fits as bytes but its final 4 KB sector would cross the boundary.
    let soc = info("bk72xx", 401.into());
    validate_script_image(&soc, 401 * 1024, 1).unwrap();
    assert!(validate_script_image(&soc, 401 * 1024, 4096).is_err());
}

#[test]
fn missing_zero_or_overflowing_capacity_is_not_assumed_safe() {
    assert!(validate_script_image(&info("ec7xx", serde_json::Value::Null), 1, 1)
        .unwrap_err()
        .to_string()
        .contains("rom.fs.script.size"));
    assert!(validate_script_image(&info("ec7xx", 0.into()), 1, 1).is_err());
    assert!(validate_script_image(&info("ec7xx", u64::MAX.into()), 1, 1).is_err());
    assert!(validate_script_image(&info("ec7xx", 400.into()), usize::MAX, 4096).is_err());
}

#[test]
fn oversized_binary_is_rejected_by_protocols_before_loading_agents_or_opening_serial() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    std::fs::create_dir(&source).unwrap();
    let data = vec![0u8; 500 * 1024];
    let progress: ProgressCallback = Box::new(|_| {});
    for chip in ["ec7xx", "ccm4211", "uis8910"] {
        std::fs::write(source.join("info.json"), serde_json::to_vec(&info(chip, 400.into())).unwrap()).unwrap();
        let soc = dir.path().join(format!("{chip}.soc"));
        luatos_soc::pack_soc_zip(&source, soc.to_str().unwrap()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let result = match chip {
            "ec7xx" => luatos_flash::ec718::flash_script_ec718(soc.to_str().unwrap(), "INVALID_PORT", &data, &progress, cancel),
            "ccm4211" => luatos_flash::ccm4211::flash_script_ccm4211(soc.to_str().unwrap(), "INVALID_PORT", &data, &progress, cancel),
            _ => luatos_flash::rda8910::flash_script_rda8910(soc.to_str().unwrap(), "INVALID_PORT", &data, &progress, cancel),
        };
        let error = result.unwrap_err().to_string();
        assert!(error.contains("超过分区容量（409600 字节"), "{chip}: {error}");
    }
}

#[test]
fn oversized_script_directory_is_rejected_by_bk_and_xt_before_serial() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    let scripts = dir.path().join("scripts");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&scripts).unwrap();
    let file = scripts.join("data.bin");
    std::fs::write(&file, vec![0u8; 500 * 1024]).unwrap();
    // BK full flash requires a ROM file; the capacity error must precede its download.
    std::fs::write(source.join("rom.bin"), b"rom").unwrap();
    for chip in ["bk72xx", "air6208"] {
        std::fs::write(source.join("info.json"), serde_json::to_vec(&info(chip, 400.into())).unwrap()).unwrap();
        let soc = dir.path().join(format!("{chip}.soc"));
        luatos_soc::pack_soc_zip(&source, soc.to_str().unwrap()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let progress: ProgressCallback = Box::new(|_| {});
        let result = if chip == "bk72xx" {
            luatos_flash::bk7258::flash_script_only(soc.to_str().unwrap(), &[scripts.to_str().unwrap()], "INVALID_PORT", cancel, &progress)
        } else {
            luatos_flash::xt804::flash_script_only(soc.to_str().unwrap(), "INVALID_PORT", &[file.to_str().unwrap().to_owned()], progress, cancel)
        };
        let error = result.unwrap_err().to_string();
        assert!(error.contains("超过分区容量（409600 字节"), "{chip}: {error}");
        if chip == "bk72xx" {
            let error = luatos_flash::bk7258::flash_bk7258(
                soc.to_str().unwrap(),
                Some(&[scripts.to_str().unwrap()]),
                "INVALID_PORT",
                None,
                Arc::new(AtomicBool::new(false)),
                Box::new(|_| {}),
                false,
            )
            .unwrap_err()
            .to_string();
            assert!(error.contains("超过分区容量（409600 字节"), "full flash: {error}");
        }
    }
}

#[test]
fn ccm_full_flash_checks_bundled_script_and_overlay_before_loading_ramrun() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("info.json"), serde_json::to_vec(&info("ccm4211", 400.into())).unwrap()).unwrap();
    let data = vec![0u8; 500 * 1024];
    std::fs::write(source.join("script.bin"), &data).unwrap();
    let soc = dir.path().join("test.soc");
    luatos_soc::pack_soc_zip(&source, soc.to_str().unwrap()).unwrap();
    for overlay in [None, Some(data.as_slice())] {
        let progress: ProgressCallback = Box::new(|_| {});
        let error = luatos_flash::ccm4211::flash_ccm4211(soc.to_str().unwrap(), "INVALID_PORT", &progress, Arc::new(AtomicBool::new(false)), overlay)
            .unwrap_err()
            .to_string();
        assert!(error.contains("超过分区容量（409600 字节"), "{error}");
    }
}

#[test]
fn ec_full_flash_checks_overlay_before_loading_firmware() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("info.json"), serde_json::to_vec(&info("ec7xx", 400.into())).unwrap()).unwrap();
    let soc = dir.path().join("test.soc");
    luatos_soc::pack_soc_zip(&source, soc.to_str().unwrap()).unwrap();
    let progress: ProgressCallback = Box::new(|_| {});
    let error = luatos_flash::ec718::flash_ec718(
        soc.to_str().unwrap(),
        "INVALID_PORT",
        &progress,
        Arc::new(AtomicBool::new(false)),
        Some(&vec![0u8; 500 * 1024]),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("超过分区容量（409600 字节"), "{error}");
}

#[cfg(feature = "sf32lb58")]
#[test]
fn sf32_script_checks_capacity_before_reset_or_serial() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    let scripts = dir.path().join("scripts");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&scripts).unwrap();
    std::fs::write(scripts.join("data.bin"), vec![0u8; 500 * 1024]).unwrap();
    std::fs::write(source.join("info.json"), serde_json::to_vec(&info("sf32lb58", 400.into())).unwrap()).unwrap();
    let soc = dir.path().join("test.soc");
    luatos_soc::pack_soc_zip(&source, soc.to_str().unwrap()).unwrap();
    let error = luatos_flash::sf32lb5x::flash_script_sf32lb5x(
        soc.to_str().unwrap(),
        "INVALID_PORT",
        &[scripts.to_str().unwrap()],
        Box::new(|_| {}),
        Arc::new(AtomicBool::new(false)),
        None,
        None,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("超过分区容量（409600 字节"), "{error}");
}
