//! 芯片族统一的脚本镜像构建入口。
//!
//! 刷机路径只传递最终字节镜像；各芯片的目录/文件名语义由对应构建器负责：
//! - BK7258：basename 合并，必要时加 BK CRC16
//! - XT804：basename 扁平 LuaDB（不加 XT804 镜像头）
//! - CCM4211 / EC718 / RDA8910：basename 合并
//!
//! 该模块不创建任何临时文件；`ctx.json` 等附加条目以 [`LuadbEntry`] 内存条目传入。

use std::path::Path;

use anyhow::{bail, Context, Result};
use luatos_luadb::LuadbEntry;
use luatos_soc::{ChipFamily, SocInfo};

/// 各芯片族写入脚本分区时的对齐粒度。
///
/// 与 BSP 实际写入粒度一致：BK7258 按 4 KiB 扇区、XT804 按 1 KiB 扇区，
/// 其余芯片按字节写入。
pub fn script_write_alignment(family: ChipFamily) -> usize {
    match family {
        ChipFamily::Bk72xx => 4096,
        ChipFamily::Xt804 => 1024,
        _ => 1,
    }
}

/// 在打开串口前校验脚本镜像（含写入对齐膨胀）是否落在 SOC 声明的脚本分区内。
pub fn validate_script_image_for_family(family: ChipFamily, info: &SocInfo, image_len: usize) -> Result<()> {
    crate::validate_script_image(info, image_len, script_write_alignment(family))
}

/// 按芯片族构建最终脚本镜像。
///
/// `extra_entries` 是最终字节（如 `ctx.json`），在目录产物之后合入并覆盖同名条目。
/// `force_bk_crc` 仅对 BK7258 生效，用于保留 `trun` 历史上对 BK 强制 CRC 的行为；
/// 普通 CLI 刷机传 `false`，以 SOC 元数据为准。
pub fn build_script_image(family: ChipFamily, script_dirs: &[&Path], info: &SocInfo, luac_debug: u8, extra_entries: &[LuadbEntry], force_bk_crc: bool) -> Result<Vec<u8>> {
    match family {
        ChipFamily::Bk72xx => crate::bk7258::build_script_image(script_dirs, info, Some(luac_debug), extra_entries, force_bk_crc),
        ChipFamily::Xt804 => crate::xt804::build_script_image_from_dirs(script_dirs, info, Some(luac_debug), extra_entries),
        ChipFamily::Ccm4211 | ChipFamily::Ec718 | ChipFamily::Rda8910 => {
            luatos_luadb::build::build_script_image_with_entries(script_dirs, extra_entries, info.script_use_luac(), info.script_bitw(), info.use_bkcrc(), luac_debug)
        }
        ChipFamily::D12p => bail!("Air8609W 脚本镜像需要以 .soc 内的 script.bin 为基准合并 corelib，请使用 merge_d12p_scripts"),
        ChipFamily::Unknown | ChipFamily::Air6201 => bail!("脚本镜像构建不支持芯片族 {:?}", family),
    }
}

/// D126 脚本分区必须包含的 LuatOS corelib 文件。
///
/// 这些文件由 SDK 的 `build.py` 打进 `script.bin`；CLI 覆盖应用脚本时从
/// `.soc` 的现有镜像中保留它们。
pub const D12P_CORELIB: [&str; 3] = ["libnet.lua", "sys.lua", "sysplus.lua"];

/// 以 `.soc` 内的 script.bin 为基准，合并用户脚本目录。
///
/// 只保留基准镜像中的 corelib 条目，其余条目全部来自 `script_dirs`；
/// 结果按文件名排序并补 0xFF 到分区大小。
pub fn merge_d12p_scripts(base_image: &[u8], script_dirs: &[&Path], partition_size: usize, luac_debug: u8) -> Result<Vec<u8>> {
    let base = luatos_luadb::parse_luadb(base_image).context("解析 .soc 内的 script.bin 失败")?;
    let fresh = luatos_luadb::build::collect_script_entries_compiled(script_dirs, false, 64, luac_debug)?;

    let mut merged: Vec<LuadbEntry> = base.into_iter().filter(|entry| D12P_CORELIB.contains(&entry.basename())).collect();
    for name in D12P_CORELIB {
        anyhow::ensure!(merged.iter().any(|entry| entry.basename() == name), "script.bin 缺少 Air8609W corelib: {name}");
    }
    for entry in fresh {
        merged.retain(|existing| existing.basename() != entry.basename());
        merged.push(entry);
    }
    merged.sort_by(|a, b| a.basename().cmp(b.basename()));

    let mut names = std::collections::HashSet::new();
    for entry in &merged {
        anyhow::ensure!(names.insert(entry.basename().to_string()), "脚本文件名重复: {}", entry.basename());
    }

    let mut image = luatos_luadb::pack_luadb(&merged)?;
    anyhow::ensure!(image.len() <= partition_size, "脚本镜像 {} 字节超过分区大小 {} 字节", image.len(), partition_size);
    image.resize(partition_size, 0xFF);
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(chip: &str) -> SocInfo {
        serde_json::from_value(serde_json::json!({
            "chip": {"type": chip},
            "rom": {"file": "rom.bin", "fs": {"script": {"size": 400, "bkcrc": false}}},
            "script": {"file": "script.bin", "use-luac": false, "bitw": 32},
            "download": {"script_addr": "200000"}
        }))
        .unwrap()
    }

    #[test]
    fn write_alignment_matches_bsp_granularity() {
        assert_eq!(script_write_alignment(ChipFamily::Bk72xx), 4096);
        assert_eq!(script_write_alignment(ChipFamily::Xt804), 1024);
        assert_eq!(script_write_alignment(ChipFamily::Ccm4211), 1);
        assert_eq!(script_write_alignment(ChipFamily::Ec718), 1);
        assert_eq!(script_write_alignment(ChipFamily::Rda8910), 1);
    }

    #[test]
    fn d12p_merge_keeps_corelib_and_replaces_app_scripts() {
        let base = luatos_luadb::pack_luadb(&[
            LuadbEntry {
                filename: "sys.lua".into(),
                data: b"-- sys".to_vec(),
            },
            LuadbEntry {
                filename: "sysplus.lua".into(),
                data: b"-- sysplus".to_vec(),
            },
            LuadbEntry {
                filename: "libnet.lua".into(),
                data: b"-- libnet".to_vec(),
            },
            LuadbEntry {
                filename: "main.lua".into(),
                data: b"print('old')".to_vec(),
            },
            LuadbEntry {
                filename: "removed.lua".into(),
                data: b"print('removed')".to_vec(),
            },
        ])
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.lua"), b"print('new')").unwrap();
        std::fs::write(dir.path().join("extra.lua"), b"return 1").unwrap();

        let image = merge_d12p_scripts(&base, &[dir.path()], 4096, 99).unwrap();
        assert_eq!(image.len(), 4096);
        assert!(image[image.len() - 1] == 0xFF);

        let entries = luatos_luadb::parse_luadb(&image).unwrap();
        let names: Vec<&str> = entries.iter().map(LuadbEntry::basename).collect();
        assert_eq!(names, vec!["extra.lua", "libnet.lua", "main.lua", "sys.lua", "sysplus.lua"]);
        assert_eq!(entries.iter().find(|e| e.basename() == "main.lua").unwrap().data, b"print('new')");
        assert!(!names.contains(&"removed.lua"));
    }

    #[test]
    fn d12p_merge_rejects_image_without_corelib() {
        let base = luatos_luadb::pack_luadb(&[LuadbEntry {
            filename: "main.lua".into(),
            data: b"print('x')".to_vec(),
        }])
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.lua"), b"print('y')").unwrap();
        let error = merge_d12p_scripts(&base, &[dir.path()], 4096, 99).unwrap_err();
        assert!(error.to_string().contains("corelib"), "{error}");
    }

    #[test]
    fn unsupported_family_is_rejected() {
        let error = build_script_image(ChipFamily::Air6201, &[], &info("air6201"), 99, &[], false).unwrap_err();
        assert!(error.to_string().contains("不支持芯片族"));
    }

    #[test]
    fn generic_family_merges_extra_entries_without_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.lua"), b"print('hi')").unwrap();
        let extra = LuadbEntry {
            filename: "ctx.json".into(),
            data: br#"{"test_id":"x"}"#.to_vec(),
        };

        let image = build_script_image(ChipFamily::Ccm4211, &[dir.path()], &info("ccm4211"), 99, &[extra], false).unwrap();
        assert!(image.windows(b"ctx.json".len()).any(|window| window == b"ctx.json"));
        assert!(image.windows(b"main.lua".len()).any(|window| window == b"main.lua"));
    }

    #[test]
    fn bk_builder_merges_ctx_and_can_force_bk_crc() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.lua"), b"print('hi')").unwrap();
        let extra = LuadbEntry {
            filename: "ctx.json".into(),
            data: br#"{"test_id":"x"}"#.to_vec(),
        };

        let info = info("bk72xx");
        let image = build_script_image(ChipFamily::Bk72xx, &[dir.path()], &info, 99, std::slice::from_ref(&extra), true).unwrap();
        assert_eq!(image.len() % 34, 0, "force_bk_crc 时必须按 34 字节帧化");
        assert!(image.windows(b"ctx.json".len()).any(|window| window == b"ctx.json"));
    }

    #[test]
    fn xt804_builder_flattens_dirs_and_merges_ctx() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("main.lua"), b"print('hi')").unwrap();
        std::fs::write(dir.path().join("sub").join("lib.lua"), b"return {}").unwrap();
        let extra = LuadbEntry {
            filename: "ctx.json".into(),
            data: br#"{"test_id":"x"}"#.to_vec(),
        };

        let image = build_script_image(ChipFamily::Xt804, &[dir.path()], &info("air6208"), 99, std::slice::from_ref(&extra), false).unwrap();
        assert!(image.windows(b"ctx.json".len()).any(|window| window == b"ctx.json"));
        assert!(image.windows(b"main.lua".len()).any(|window| window == b"main.lua"));
        assert!(image.windows(b"lib.lua".len()).any(|window| window == b"lib.lua"));
    }
}
