//! Explicit 160X storage plans, validated before entering download mode.
use anyhow::{ensure, Context, Result};
use luatos_soc::{SocInfo, SocPartition};

// Conservative whole-partition write alignment from the SDK's flash_map.h
// and 64KiB FSKV layout. Actual erase/program sequencing belongs to the
// packaged ramrun image-download protocol, not to host CMD8 stride guesses.
pub const STORAGE_ALIGNMENT: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    pub address: u32,
    pub size: usize,
}

#[derive(Debug, Default)]
pub struct StoragePlan {
    /// None preserves FS; Some without image clears it.
    pub filesystem: Option<(Partition, Option<Vec<u8>>)>,
    pub kv: Option<Partition>,
}

fn range(desc: &SocPartition) -> Result<Partition> {
    let offset = desc.offset.as_deref().context("Partition offset missing")?;
    let address = u32::from_str_radix(offset.trim().trim_start_matches("0x").trim_start_matches("0X"), 16)?;
    let size = usize::try_from(desc.size.context("Partition size missing")?)?
        .checked_mul(1024)
        .context("Partition size overflow")?;
    ensure!(size > 0 && size <= 64 * 1024 * 1024, "Invalid partition size");
    address.checked_add(u32::try_from(size)?).context("Partition end overflow")?;
    Ok(Partition { address, size })
}

fn overlap(a: &Partition, b: &Partition) -> bool {
    u64::from(a.address) < u64::from(b.address) + b.size as u64 && u64::from(b.address) < u64::from(a.address) + a.size as u64
}

fn validated(info: &SocInfo, is_kv: bool) -> Result<Partition> {
    let fs = info.rom.fs.as_ref().context("SOC partition metadata missing")?;
    let own = if is_kv { fs.kv.as_ref() } else { fs.filesystem.as_ref() }.context("SOC partition offset/size missing")?;
    let p = range(own)?;
    ensure!(
        p.address as usize % STORAGE_ALIGNMENT == 0 && p.size % STORAGE_ALIGNMENT == 0,
        "160X data partitions must be 64KiB aligned; refusing to erase adjacent data"
    );
    let address = if is_kv { info.nvm_addr() } else { info.fs_addr() };
    ensure!(address == Some(p.address), "Partition offset conflicts with download address");
    for other in [&fs.filesystem, &fs.kv, &fs.ap, &fs.fota].into_iter().flatten() {
        if !std::ptr::eq(own, other) {
            ensure!(!overlap(&p, &range(other)?), "Overlapping data partitions");
        }
    }
    if let Some(script) = &fs.script {
        if let Some(size) = script.size {
            let size = usize::try_from(size)?.checked_mul(1024).context("Script size overflow")?;
            ensure!(
                !overlap(
                    &p,
                    &Partition {
                        address: info.script_addr(),
                        size
                    }
                ),
                "Data partition overlaps script"
            );
        }
    }
    if let Some(res) = &fs.res {
        let address = res.offset.as_deref().context("Resource offset missing")?;
        let address = u32::from_str_radix(address.trim().trim_start_matches("0x"), 16)?;
        let size = usize::try_from(res.size.context("Resource size missing")?)?
            .checked_mul(1024)
            .context("Resource size overflow")?;
        ensure!(!overlap(&p, &Partition { address, size }), "Data partition overlaps resources");
    }
    Ok(p)
}

impl StoragePlan {
    pub fn prepare(info: &SocInfo, mode: &str, clear_kv: bool) -> Result<Self> {
        ensure!(
            matches!(info.chip.chip_type.to_ascii_lowercase().as_str(), "air1601" | "air1602" | "air1603" | "ccm4211"),
            "Combined FS/FSKV flashing currently supports Air1601/Air1602/Air1603 only"
        );
        ensure!(matches!(mode, "preserve" | "replace" | "clear"), "Invalid fs-mode");
        let filesystem = if mode == "preserve" {
            None
        } else {
            if mode == "replace" {
                let kind = info.rom.fs.as_ref().and_then(|f| f.filesystem.as_ref()).and_then(|d| d.fs_type.as_deref()).unwrap_or("");
                ensure!(
                    matches!(kind.to_ascii_lowercase().as_str(), "lfs" | "littlefs"),
                    "Unsupported filesystem image type: {kind}"
                );
            }
            Some((validated(info, false)?, None))
        };
        let kv = if clear_kv { Some(validated(info, true)?) } else { None };
        Ok(Self { filesystem, kv })
    }

    pub fn set_image(&mut self, image: Vec<u8>) -> Result<()> {
        let (p, value) = self.filesystem.as_mut().context("FS not selected")?;
        ensure!(p.size == image.len(), "FS image length must equal partition size");
        *value = Some(image);
        Ok(())
    }

    pub fn validate_again(&self, info: &SocInfo) -> Result<()> {
        let mode = match self.filesystem.as_ref() {
            None => "preserve",
            Some((_, None)) => "clear",
            _ => "replace",
        };
        let actual = Self::prepare(info, mode, self.kv.is_some())?;
        ensure!(
            self.filesystem.as_ref().map(|(p, _)| p) == actual.filesystem.as_ref().map(|(p, _)| p) && self.kv == actual.kv,
            "SOC partition layout changed during preparation"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn info() -> SocInfo {
        serde_json::from_value(serde_json::json!({"chip":{"type":"air1603"},"rom":{"file":"core.bin","fs":{
        "filesystem":{"offset":"0x14600000","size":512,"type":"lfs"},"kv":{"offset":"0x101f0000","size":64,"type":"fskv"},"script":{"size":512}}},
        "script":{"file":"script.bin"},"download":{"fs_addr":"0x14600000","nvm_addr":"0x101f0000","script_addr":"0x14500000"}}))
        .unwrap()
    }
    #[test]
    fn accepts_real_v1031_fs_and_64k_internal_fskv_but_rejects_partial_erase_blocks() {
        let mut data = info();
        data.rom.fs.as_mut().unwrap().filesystem.as_mut().unwrap().offset = Some("14b80000".into());
        data.rom.fs.as_mut().unwrap().filesystem.as_mut().unwrap().size = Some(4608);
        data.download.fs_addr = Some("14b80000".into());
        let plan = StoragePlan::prepare(&data, "clear", true).unwrap();
        assert_eq!(plan.kv.as_ref().unwrap().address, 0x101f0000);
        assert_eq!(plan.kv.as_ref().unwrap().size, 65536);
        assert_eq!(plan.filesystem.as_ref().unwrap().0.size, 4718592);
        data.rom.fs.as_mut().unwrap().kv.as_mut().unwrap().size = Some(32);
        assert!(StoragePlan::prepare(&data, "preserve", true).is_err());
    }

    #[test]
    fn six_storage_combinations_and_image_size() {
        for mode in ["preserve", "clear", "replace"] {
            for clear in [false, true] {
                let mut plan = StoragePlan::prepare(&info(), mode, clear).unwrap();
                assert_eq!(plan.kv.is_some(), clear);
                assert_eq!(plan.filesystem.is_some(), mode != "preserve");
                if mode == "replace" {
                    assert!(plan.set_image(vec![0; 2]).is_err());
                    plan.set_image(vec![0; 512 * 1024]).unwrap();
                }
                plan.validate_again(&info()).unwrap();
            }
        }
    }
    #[test]
    fn rejects_missing_mismatched_unaligned_and_overlapping_partitions() {
        let mut data = info();
        data.download.fs_addr = Some("0x14700000".into());
        assert!(StoragePlan::prepare(&data, "clear", false).is_err());
        data = info();
        data.rom.fs.as_mut().unwrap().filesystem.as_mut().unwrap().size = Some(513);
        assert!(StoragePlan::prepare(&data, "clear", false).is_err());
        data = info();
        data.rom.fs.as_mut().unwrap().kv.as_mut().unwrap().offset = Some("0x14600000".into());
        assert!(StoragePlan::prepare(&data, "clear", false).is_err());
        data = info();
        data.rom.fs.as_mut().unwrap().kv = None;
        assert!(StoragePlan::prepare(&data, "preserve", true).is_err());
    }
}
