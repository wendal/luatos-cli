use anyhow::{ensure, Context, Result};
use luatos_flash::ccm4211_storage::StoragePlan;

#[derive(clap::Args, Debug, Default)]
pub struct StorageArgs {
    /// Explicit 160X filesystem policy; omitted retains legacy CLI behavior.
    #[arg(long, value_parser = ["preserve", "replace", "clear"])]
    pub fs_mode: Option<String>,
    /// Raw filesystem directory, required only for replace.
    #[arg(long)]
    pub fs_dir: Option<std::path::PathBuf>,
    /// Clear the entire FSKV partition in this download session.
    #[arg(long)]
    pub clear_kv: bool,
    /// Validate and build inputs without connecting to hardware (160X).
    #[arg(long)]
    pub dry_run: bool,
}

impl StorageArgs {
    pub fn requested(&self) -> bool {
        self.fs_mode.is_some() || self.fs_dir.is_some() || self.clear_kv || self.dry_run
    }
    pub fn prepare(&self, info: &luatos_soc::SocInfo) -> Result<StoragePlan> {
        let mode = self.fs_mode.as_deref().unwrap_or("preserve");
        ensure!((mode == "replace") == self.fs_dir.is_some(), "--fs-dir is required only with --fs-mode replace");
        let mut plan = StoragePlan::prepare(info, mode, self.clear_kv)?;
        if let Some(source) = &self.fs_dir {
            let size = plan.filesystem.as_ref().context("FS not selected")?.0.size;
            let before = source_manifest(source, size)?;
            // luat_fs_ccm42xx.c mounts root FS with name_max=63. A generic
            // name_max=255 superblock is rejected (-22) and reformatted at boot.
            let image = luatos_luadb::build::build_littlefs_image_with_name_max(source, size, 4096, 63)?;
            ensure!(before == source_manifest(source, size)?, "Filesystem source changed during image preparation");
            plan.set_image(image)?;
        }
        Ok(plan)
    }
}

fn source_manifest(source: &std::path::Path, capacity: usize) -> Result<std::collections::BTreeMap<std::path::PathBuf, (u64, std::time::SystemTime)>> {
    ensure!(source.is_dir(), "Filesystem source is not a directory");
    let mut manifest = std::collections::BTreeMap::new();
    let mut bytes = 0u64;
    for entry in walkdir::WalkDir::new(source).follow_links(false) {
        let entry = entry?;
        if entry.depth() > 0 {
            let name = entry.file_name().to_str().context("Filesystem name is not UTF-8")?;
            ensure!(name.len() <= 63, "160X filesystem name exceeds 63 UTF-8 bytes: {name}");
        }
        let metadata = std::fs::symlink_metadata(entry.path())?;
        ensure!(!metadata.file_type().is_symlink(), "Filesystem symlinks are unsupported");
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            ensure!(metadata.file_attributes() & 0x400 == 0, "Filesystem reparse points are unsupported");
        }
        if metadata.is_file() {
            bytes = bytes.checked_add(metadata.len()).context("Source sizes overflow")?;
            ensure!(bytes <= capacity as u64, "Filesystem source exceeds partition capacity ({capacity} bytes)");
        } else {
            ensure!(metadata.is_dir(), "Not a regular file or directory");
        }
        manifest.insert(entry.path().strip_prefix(source)?.into(), (metadata.len(), metadata.modified()?));
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn real_image_handles_nested_unicode_binary_empty_files_and_rejects_overflow() {
        let source = tempfile::Builder::new().prefix("文件系统 ").tempdir().unwrap();
        std::fs::create_dir_all(source.path().join("资源/empty")).unwrap();
        std::fs::write(source.path().join("资源/数据.bin"), [0, 0xa5, 0xff]).unwrap();
        std::fs::write(source.path().join("empty.txt"), []).unwrap();
        let info:luatos_soc::SocInfo=serde_json::from_value(serde_json::json!({"chip":{"type":"air1603"},"rom":{"file":"core.bin","fs":{"filesystem":{"offset":"0x14600000","size":256,"type":"lfs"}}},"script":{"file":"script.bin"},"download":{"fs_addr":"0x14600000"}})).unwrap();
        let args = StorageArgs {
            fs_mode: Some("replace".into()),
            fs_dir: Some(source.path().into()),
            ..Default::default()
        };
        let image = args.prepare(&info).unwrap().filesystem.unwrap().1.unwrap();
        assert_eq!(image.len(), 256 * 1024);
        assert!(image.windows("数据.bin".len()).any(|s| s == "数据.bin".as_bytes()));
        std::fs::write(source.path().join("oversized.bin"), vec![0x5a; 512 * 1024]).unwrap();
        assert!(args.prepare(&info).is_err());
        assert!(StorageArgs {
            fs_mode: Some("replace".into()),
            ..Default::default()
        }
        .prepare(&info)
        .is_err());
    }
}
