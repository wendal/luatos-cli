// fota build - FOTA (firmware-over-the-air) package generation.
//
// Supported chip families:
//   EC7xx / EC618 / Air8000  - differential (--new + --old) via FotaToolkit + Rust OTA assembler
//                              script-only (--script-only) via Rust LZMA compress -> update.bin
//   Air1601 / Air1602 / CCM4211 - full via Rust OTA (LZMA compress + assemble), --script-only for script-only update
//   Air8101 / BK72XX            - new-format full/script FOTA via Rust BK72XX packer
//   Air6208 / XT804          - full only via air101_flash.exe (bundled in the .soc)
//   Air724UG / RDA8910       - differential/script via external dtools (lzmare2/fotacreate2) + Rust assembler
//
// External tools required:
//   FotaToolkit.exe - delta-diff engine, must run from its own dtools directory (EC7xx/EC618)
//   air101_flash.exe - W800 OTA image builder, extracted from the .soc itself (Air6208)
//   dtools - RDA LZMA 压缩(lzmare2) 与 CP 差分(fotacreate2)，RDA 专用格式无法纯 Rust 复现 (RDA8910)
//
// soc_tools.exe (zip_file + make_ota_file) has been replaced by pure Rust in luatos_soc::ota.
//
// Tool discovery order:
//   1. Explicit CLI flag
//   2. Siblings of executable or dtools/ subdir relative to it
//   3. refs/origin_tools/ layout (development)

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use luatos_soc::ChipFamily;

use crate::{event, OutputFormat};

// ─── Tool discovery ──────────────────────────────────────────────────────────

const DTOOLS_SEARCH_ROOTS: &[&str] = &["FotaToolkit_V3.6.4.0", "dtools", "../FotaToolkit_V3.6.4.0", "../../FotaToolkit_V3.6.4.0"];

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|pp| pp.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn chip_dtools_variant(chip: &str) -> &'static str {
    match chip {
        "ec618" | "air8850" => "ec618",
        _ => "ec7xx",
    }
}

fn chip_fota_config(chip: &str) -> &'static str {
    match chip {
        "ec618" => "ec618.json",
        "air780ehm" | "air8000" => "ec718hm.json",
        "air780epv" | "ec718pv" => "ec718pv.json",
        "air780epm" | "ec718pm" => "ec718pm.json",
        _ => "ec718p.json",
    }
}

/// 查找 EC7xx/EC618 差分工具 `FotaToolkit`（rda8910 的 `find_rda_dtools` 与之保持一致）：
/// `--fota-toolkit` 显式路径 → 可执行文件同级目录。
fn find_fota_toolkit(chip: &str, explicit: Option<&str>) -> Result<(PathBuf, PathBuf)> {
    if let Some(p) = explicit {
        let path = PathBuf::from(p);
        anyhow::ensure!(path.exists(), "FotaToolkit not found at: {p}");
        let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        return Ok((path, dir));
    }

    let base = exe_dir();
    let variant = chip_dtools_variant(chip);
    let toolkit_name = if cfg!(target_os = "windows") { "FotaToolkit.exe" } else { "FotaToolkit" };
    for root in DTOOLS_SEARCH_ROOTS {
        // Try with chip variant subdirectory (dtools/ec7xx/FotaToolkit)
        let dir = base.join(root).join(variant);
        let candidate = dir.join(toolkit_name);
        if candidate.exists() {
            return Ok((candidate, dir));
        }
        // Try without chip subdirectory (FotaToolkit_V3.6.4.0/FotaToolkit)
        let dir = base.join(root);
        let candidate = dir.join(toolkit_name);
        if candidate.exists() {
            return Ok((candidate, dir));
        }
    }

    bail!(
        "FotaToolkit not found for chip '{chip}' (variant: {variant}). \
         Provide --fota-toolkit <path> or place FotaToolkit_V3.6.4.0/ next to luatos-cli."
    )
}

// ─── Shared helpers ──────────────────────────────────────────────────────────

/// Extract a .soc archive, return the unpacked result.
fn unpack(soc_path: &str, out_dir: &Path) -> Result<luatos_soc::UnpackedSoc> {
    fs::create_dir_all(out_dir).with_context(|| format!("create dir {}", out_dir.display()))?;
    luatos_soc::unpack_soc(soc_path, out_dir)
}

/// Create an empty dummy.bin placeholder.
fn create_dummy(dir: &Path) -> Result<PathBuf> {
    let path = dir.join("dummy.bin");
    fs::write(&path, []).context("create dummy.bin")?;
    Ok(path)
}

/// Parse a hex address string (with or without "0x" prefix).
fn parse_hex_addr(s: &str) -> Option<u64> {
    let s = s.trim();
    let hex = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
    u64::from_str_radix(hex, 16).ok()
}

/// Parse `FLASH_FOTA_REGION_LEN` from mem_map.txt content and subtract the 96KB
/// FOTA reserved area, giving the maximum allowed delta.par size.
/// Uses the last occurrence in the file (matches luatools_py3 soc.py's greedy regex).
fn fota_delta_size_limit(mem_text: &str) -> Option<u64> {
    mem_text
        .lines()
        .rev()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("#define FLASH_FOTA_REGION_LEN")?;
            let value = rest.trim().trim_start_matches('(').trim_end_matches(')').trim();
            parse_hex_addr(value)
        })
        .next()
        .map(|v| v.saturating_sub(96 * 1024))
}

/// Run a command, surface stderr on failure, return an error if exit code != 0.
fn run_cmd(mut cmd: Command) -> Result<()> {
    let status = cmd.status().with_context(|| format!("failed to launch {:?}", cmd.get_program()))?;
    if !status.success() {
        bail!("{:?} exited with code {:?}", cmd.get_program(), status.code());
    }
    Ok(())
}

// ─── EC7xx / EC618 - differential FOTA ───────────────────────────────────────

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).with_context(|| format!("create {}", dst.display()))?;
    for entry in fs::read_dir(src).with_context(|| format!("read {}", src.display()))? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target).with_context(|| format!("copy {}", entry.path().display()))?;
        }
    }
    Ok(())
}

fn select_rom_path(dir: &Path, info_rom_file: &str) -> PathBuf {
    // Match dtools/main.py diff_soc() logic: prefer non-luatos binpkg when available
    let mut binpkgs: Vec<_> = std::fs::read_dir(dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|ext| ext == "binpkg").unwrap_or(false))
        .map(|e| e.path())
        .collect();
    if binpkgs.len() > 1 {
        binpkgs.retain(|p| p.file_name().and_then(|n| n.to_str()).map(|n| n != "luatos.binpkg").unwrap_or(true));
    }
    binpkgs.into_iter().next().unwrap_or_else(|| dir.join(info_rom_file))
}

fn build_ec7xx_fota(new_soc: &str, old_soc: &str, chip: &str, toolkit_path: &Path, toolkit_dir: &Path, out_path: &Path, force_par: bool) -> Result<()> {
    let tmp = tempfile::tempdir().context("tempdir")?;
    let work_dir = tmp.path();

    let new_up = unpack(new_soc, &work_dir.join("new"))?;
    let old_up = unpack(old_soc, &work_dir.join("old"))?;

    let magic_str = new_up
        .info
        .fota
        .as_ref()
        .and_then(|f| f.get("magic_num"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("info.json missing fota.magic_num"))?;
    let magic = parse_hex_addr(magic_str).ok_or_else(|| anyhow::anyhow!("invalid fota.magic_num: {magic_str}"))? as u32;

    // Select the correct binpkg file (prefer core.binpkg over luatos.binpkg, matching dtools/main.py)
    let old_binpkg = select_rom_path(&old_up.dir, &old_up.info.rom.file);
    let new_binpkg = select_rom_path(&new_up.dir, &new_up.info.rom.file);

    // Copy binpkg to isolated temp directory (concurrent-safe)
    fs::copy(&old_binpkg, work_dir.join("old.binpkg")).context("copy old.binpkg")?;
    fs::copy(&new_binpkg, work_dir.join("new.binpkg")).context("copy new.binpkg")?;

    // Copy config/ and dep/ from toolkit_dir to work_dir so FotaToolkit can find them
    let config_src = toolkit_dir.join("config");
    if config_src.exists() {
        copy_dir_recursive(&config_src, &work_dir.join("config"))?;
    }
    let dep_src = toolkit_dir.join("dep");
    if dep_src.exists() {
        copy_dir_recursive(&dep_src, &work_dir.join("dep"))?;
    }

    let config_arg = Path::new("config").join(chip_fota_config(chip));

    // Try to detect the specific chip variant from mem_map.txt
    // (info.json only says "ec7xx", but FotaToolkit needs the right config like ec718hm.json)
    let mem_map_path = new_up.dir.join("mem_map.txt");
    let mem_text = if mem_map_path.exists() {
        fs::read_to_string(&mem_map_path).unwrap_or_default()
    } else {
        String::new()
    };
    let actual_config = if !mem_text.is_empty() {
        // Match lines like: #define TYPE_EC718HM 1
        let config_name = mem_text
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                if line.starts_with("#define TYPE_") && line.ends_with(" 1") {
                    let name = line.trim_start_matches("#define TYPE_").trim_end_matches(" 1").trim();
                    Some(name.to_lowercase())
                } else {
                    None
                }
            })
            // Prefer the more specific variant (longer name = more specific)
            .max_by_key(|n| n.len())
            .unwrap_or_default();
        if !config_name.is_empty() {
            let cfg = format!("{}.json", config_name);
            if config_src.join(&cfg).exists() {
                Path::new("config").join(&cfg)
            } else {
                config_arg.clone()
            }
        } else {
            config_arg.clone()
        }
    } else {
        config_arg.clone()
    };
    let actual_config_str = actual_config.to_string_lossy().to_string();

    log::info!("FotaToolkit: {:?} -d {} BINPKG delta.par old.binpkg new.binpkg", toolkit_path, actual_config_str);
    let output = Command::new(toolkit_path)
        .args(["-d", &actual_config_str, "BINPKG", "delta.par", "old.binpkg", "new.binpkg"])
        .current_dir(work_dir)
        .output()
        .with_context(|| format!("launch {:?}", toolkit_path))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        if !stderr.is_empty() {
            log::error!("FotaToolkit stderr: {}", stderr.trim());
        }
        if !stdout.is_empty() {
            log::error!("FotaToolkit stdout: {}", stdout.trim());
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            if let Some(sig) = output.status.signal() {
                bail!(
                    "FotaToolkit killed by signal {} (SIGSEGV=11, SIGKILL=9). Check: 1) execute permission, 2) shared libraries (ldd FotaToolkit), 3) CPU arch compatibility",
                    sig
                );
            }
        }
        bail!("FotaToolkit failed (exit {:?})", output.status.code());
    }
    let delta = work_dir.join("delta.par");
    if !delta.exists() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stdout.is_empty() {
            log::info!("FotaToolkit stdout: {}", stdout.trim());
        }
        if !stderr.is_empty() {
            log::info!("FotaToolkit stderr: {}", stderr.trim());
        }
        if stdout.contains("same images") || stderr.contains("same images") {
            if force_par {
                bail!("底层固件相同，但 --force-par 已指定，拒绝自动回落为脚本包");
            }
            log::info!("Firmware cores identical — auto-fallback to script-only FOTA");
            // Fall back to script-only: compress and assemble only the script partition
            return build_ec7xx_script_only_fota(new_soc, out_path);
        }
        bail!("delta.par not found after FotaToolkit. Check FotaToolkit logs above for details.");
    }

    // Size guard (matches luatools_py3 soc.py): delta.par must fit in the FOTA region
    // (FLASH_FOTA_REGION_LEN from mem_map.txt minus the 96KB reserved area).
    match fota_delta_size_limit(&mem_text) {
        Some(limit) => {
            let delta_size = fs::metadata(&delta).context("stat delta.par")?.len();
            if delta_size > limit {
                bail!("底层差分包大小超过了最大允许大小 {}kb > {}kb", 1 + delta_size / 1024, limit / 1024);
            }
        }
        None => log::warn!("FLASH_FOTA_REGION_LEN not found in mem_map.txt; skipping delta.par size check"),
    }

    // common_data = compressed script (full, if present), sdk_data = delta.par (firmware diff)
    let script_bin = new_up.dir.join(&new_up.info.script.file);
    let common = if script_bin.exists() {
        let script_addr = new_up
            .info
            .download
            .script_addr
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("info.json missing download.script_addr"))?;
        let script_addr = parse_hex_addr(script_addr).ok_or_else(|| anyhow::anyhow!("invalid download.script_addr: {script_addr}"))? as u32;
        let s_zip = work_dir.join("script.zip");
        luatos_soc::ota::lzma_compress_file(&script_bin, &s_zip, magic, script_addr, 0x40000, true).context("compress script for OTA")?;
        s_zip
    } else {
        create_dummy(work_dir)?
    };

    luatos_soc::ota::assemble_ota_package(magic, 0, "0", 0, "0", 0, &common, &delta, out_path).context("assemble OTA package")?;

    Ok(())
}

/// Build script-only FOTA for EC7xx/EC618 - compresses only the script partition
/// into a .bin package (92-byte header + LZMA compressed script), skipping ROM and FotaToolkit.
fn build_ec7xx_script_only_fota(new_soc: &str, out_path: &Path) -> Result<()> {
    let tmp = tempfile::tempdir().context("tempdir")?;
    let up = unpack(new_soc, &tmp.path().join("soc"))?;
    let info = &up.info;

    let magic_str = info
        .fota
        .as_ref()
        .and_then(|f| f.get("magic_num"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("info.json missing fota.magic_num"))?;
    let magic = parse_hex_addr(magic_str).ok_or_else(|| anyhow::anyhow!("invalid fota.magic_num: {magic_str}"))? as u32;

    let script_addr = info
        .download
        .script_addr
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("info.json missing download.script_addr"))?;
    let script_addr = parse_hex_addr(script_addr).ok_or_else(|| anyhow::anyhow!("invalid download.script_addr: {script_addr}"))? as u32;

    let script_bin = up.dir.join(&info.script.file);
    anyhow::ensure!(script_bin.exists(), "script file not found: {}", script_bin.display());

    let s_zip = tmp.path().join("script.zip");
    luatos_soc::ota::lzma_compress_file(&script_bin, &s_zip, magic, script_addr, 0x40000, true).context("compress script for OTA")?;

    let dummy = create_dummy(tmp.path())?;
    luatos_soc::ota::assemble_ota_package(magic, 0, "0", 0, "0", 0, &s_zip, &dummy, out_path).context("assemble OTA package")?;

    Ok(())
}

// ─── Air1601 / Air1602 / CCM4211 - full FOTA ────────────────────────────────

fn build_ccm4211_fota(new_soc: &str, out_path: &Path) -> Result<()> {
    let tmp = tempfile::tempdir().context("tempdir")?;
    let up = unpack(new_soc, &tmp.path().join("soc"))?;
    let info = &up.info;

    let magic_str = info
        .fota
        .as_ref()
        .and_then(|f| f.get("magic_num"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("info.json missing fota.magic_num"))?;
    let magic = parse_hex_addr(magic_str).ok_or_else(|| anyhow::anyhow!("invalid fota.magic_num: {magic_str}"))? as u32;

    let app_addr_str = info.download.app_addr.as_deref().unwrap_or("20000000");
    let app_addr = parse_hex_addr(app_addr_str).ok_or_else(|| anyhow::anyhow!("invalid download.app_addr: {app_addr_str}"))? as u32;

    let script_addr = info.download.script_addr.as_deref();
    let rom_bin = &up.rom_path;
    let script_bin = up.dir.join(&info.script.file);

    let ap_zip = tmp.path().join("ap.zip");
    luatos_soc::ota::lzma_compress_file(rom_bin, &ap_zip, magic, app_addr, 0x40000, true).context("compress ROM for OTA")?;

    let total_zip = tmp.path().join("total.zip");
    if script_bin.exists() {
        if let Some(saddr_str) = script_addr {
            let saddr = parse_hex_addr(saddr_str).ok_or_else(|| anyhow::anyhow!("invalid download.script_addr: {saddr_str}"))? as u32;
            let s_zip = tmp.path().join("s.zip");
            luatos_soc::ota::lzma_compress_file(&script_bin, &s_zip, magic, saddr, 0x40000, true).context("compress script for OTA")?;
            let mut data = fs::read(&ap_zip).context("read ap.zip")?;
            data.extend_from_slice(&fs::read(&s_zip).context("read s.zip")?);
            fs::write(&total_zip, data).context("write total.zip")?;
        } else {
            fs::copy(&ap_zip, &total_zip).context("copy ap.zip")?;
        }
    } else {
        fs::copy(&ap_zip, &total_zip).context("copy ap.zip")?;
    }

    let dummy = create_dummy(tmp.path())?;
    luatos_soc::ota::assemble_ota_package(magic, 0, "0", 0, "0", 0, &total_zip, &dummy, out_path).context("assemble OTA package")?;

    Ok(())
}

/// Build script-only FOTA for CCM4211 - compresses only the script partition,
/// skipping ROM compression entirely. Useful for Lua script hot-update.
fn build_ccm4211_script_only_fota(new_soc: &str, out_path: &Path) -> Result<()> {
    let tmp = tempfile::tempdir().context("tempdir")?;
    let up = unpack(new_soc, &tmp.path().join("soc"))?;
    let info = &up.info;

    let magic_str = info
        .fota
        .as_ref()
        .and_then(|f| f.get("magic_num"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("info.json missing fota.magic_num"))?;
    let magic = parse_hex_addr(magic_str).ok_or_else(|| anyhow::anyhow!("invalid fota.magic_num: {magic_str}"))? as u32;

    let script_addr = info
        .download
        .script_addr
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("info.json missing download.script_addr - script-only FOTA needs a valid script address"))?;
    let script_addr = parse_hex_addr(script_addr).ok_or_else(|| anyhow::anyhow!("invalid download.script_addr: {script_addr}"))? as u32;

    let script_bin = up.dir.join(&info.script.file);
    anyhow::ensure!(
        script_bin.exists(),
        "script file not found: {} - script-only FOTA requires a script partition",
        script_bin.display()
    );

    let s_zip = tmp.path().join("script.zip");
    luatos_soc::ota::lzma_compress_file(&script_bin, &s_zip, magic, script_addr, 0x40000, true).context("compress script for OTA")?;

    let dummy = create_dummy(tmp.path())?;
    luatos_soc::ota::assemble_ota_package(magic, 0, "0", 0, "0", 0, &s_zip, &dummy, out_path).context("assemble OTA package")?;

    Ok(())
}

fn build_bk72xx_fota(new_soc: &str, out_path: &Path, script_only: bool) -> Result<()> {
    let tmp = tempfile::tempdir().context("tempdir")?;
    let up = unpack(new_soc, &tmp.path().join("soc"))?;
    let info = &up.info;

    anyhow::ensure!(
        info.use_bkcrc(),
        "BK72XX old-format FOTA is not supported in luatos-cli yet (requires info.json rom.fs.script.bkcrc=true)"
    );

    let script_bin = up.dir.join(&info.script.file);
    anyhow::ensure!(script_bin.exists(), "script file not found: {}", script_bin.display());

    if script_only {
        luatos_soc::ota::build_bk72xx_script_fota_new(&script_bin, out_path)?;
        return Ok(());
    }

    let cp_bin = up.dir.join("cp.bin");
    let ap_bin = up.dir.join("ap.bin");
    anyhow::ensure!(cp_bin.exists(), "cp.bin not found in SOC: {}", cp_bin.display());
    anyhow::ensure!(ap_bin.exists(), "ap.bin not found in SOC: {}", ap_bin.display());

    let ap_offset_str = info
        .rom
        .fs
        .as_ref()
        .and_then(|fs| fs.ap.as_ref())
        .and_then(|ap| ap.offset.as_deref())
        .ok_or_else(|| anyhow::anyhow!("info.json missing rom.fs.ap.offset"))?;
    let ap_offset = parse_hex_addr(ap_offset_str).ok_or_else(|| anyhow::anyhow!("invalid rom.fs.ap.offset: {ap_offset_str}"))? as u32;

    let script_addr_str = info
        .download
        .script_addr
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("info.json missing download.script_addr"))?;
    let script_addr = parse_hex_addr(script_addr_str).ok_or_else(|| anyhow::anyhow!("invalid download.script_addr: {script_addr_str}"))? as u32;

    luatos_soc::ota::build_bk72xx_full_fota_new(&cp_bin, &ap_bin, &script_bin, ap_offset, script_addr, out_path)?;
    Ok(())
}

// ─── Air6208 / XT804 - full FOTA ─────────────────────────────────────────────

fn build_air6208_fota(new_soc: &str, out_base: &Path) -> Result<Air6208FotaResult> {
    let tmp = tempfile::tempdir().context("tempdir")?;
    let up = unpack(new_soc, &tmp.path().join("soc"))?;
    let info = &up.info;

    let flash_exe = up
        .flash_exe
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("air101_flash.exe not found inside the Air6208 .soc archive"))?;

    let app_addr = info.download.app_addr.as_deref().or(info.download.core_addr.as_deref()).unwrap_or("8010000");
    let run_addr_computed: String = parse_hex_addr(app_addr).map(|a| format!("{:x}", a + 0x400)).unwrap_or_else(|| "8010400".to_string());
    let run_addr = info.download.run_addr.as_deref().unwrap_or(&run_addr_computed);
    let fota_addr = info
        .rom
        .fs
        .as_ref()
        .and_then(|fs| fs.fota.as_ref())
        .and_then(|f| f.offset.as_deref())
        .or(info.download.ota_addr.as_deref())
        .unwrap_or("8280000");
    let script_offset = info.rom.fs.as_ref().and_then(|fs| fs.script.as_ref()).and_then(|s| s.offset.as_deref()).unwrap_or("0");
    let compress_type: u8 = info
        .fota
        .as_ref()
        .and_then(|f| f.get("compress_type"))
        .and_then(|v| v.as_u64())
        .map(|v| v as u8)
        .unwrap_or(1);

    let rom_bin = &up.rom_path;
    let script_bin = up.dir.join(&info.script.file);

    let mid_base = tmp.path().join("fota_mid");
    run_cmd({
        let mut c = Command::new(flash_exe);
        c.args([
            "-b",
            &rom_bin.to_string_lossy(),
            "-it",
            "2",
            "-fc",
            "0",
            "-ih",
            "20008000",
            "-ra",
            script_offset,
            "-ua",
            "0",
            "-nh",
            "0",
            "-un",
            "0",
            "-o",
            &mid_base.to_string_lossy(),
        ]);
        c
    })?;

    let mid_img = mid_base.with_extension("img");
    let bin_data = fs::read(&mid_img).with_context(|| format!("read intermediate image {}", mid_img.display()))?;
    if bin_data.len() < 16 {
        bail!("Intermediate image too small to contain secboot header");
    }
    let imglen = u32::from_le_bytes(bin_data[12..16].try_into().unwrap()) as usize;
    let skip = 64usize + imglen;
    if bin_data.len() < skip {
        bail!("Intermediate image smaller than expected secboot size ({skip} bytes)");
    }
    let stripped = &bin_data[skip..];
    let stripped_path = tmp.path().join("fota_stripped.bin");
    fs::write(&stripped_path, stripped).context("write stripped binary")?;

    let fw_base = tmp.path().join("fw_fota");
    let fc_str = compress_type.to_string();
    run_cmd({
        let mut c = Command::new(flash_exe);
        c.args([
            "-b",
            &stripped_path.to_string_lossy(),
            "-it",
            "1",
            "-fc",
            &fc_str,
            "-ih",
            app_addr,
            "-ra",
            run_addr,
            "-ua",
            fota_addr,
            "-nh",
            "0",
            "-un",
            "0",
            "-o",
            &fw_base.to_string_lossy(),
        ]);
        c
    })?;

    let gz_img = PathBuf::from(format!("{}_gz.img", fw_base.display()));
    let fota_out = out_base.with_extension("fota");
    fs::rename(&gz_img, &fota_out).with_context(|| format!("rename {} -> {}", gz_img.display(), fota_out.display()))?;
    let _ = fs::remove_file(PathBuf::from(format!("{}.bin.gz", fw_base.display())));

    let mut sota_out: Option<PathBuf> = None;
    if script_bin.exists() {
        let script_addr = info.download.script_addr.as_deref().unwrap_or(fota_addr);
        let s_base = tmp.path().join("script_sota");
        let res = (|| -> Result<()> {
            run_cmd({
                let mut c = Command::new(flash_exe);
                c.args([
                    "-b",
                    &script_bin.to_string_lossy(),
                    "-it",
                    "2",
                    "-fc",
                    &fc_str,
                    "-ih",
                    script_addr,
                    "-ra",
                    script_addr,
                    "-ua",
                    fota_addr,
                    "-nh",
                    "0",
                    "-un",
                    "0",
                    "-o",
                    &s_base.to_string_lossy(),
                ]);
                c
            })?;
            let s_gz = PathBuf::from(format!("{}_gz.img", s_base.display()));
            let s_out = out_base.with_extension("sota");
            fs::rename(&s_gz, &s_out)?;
            let _ = fs::remove_file(PathBuf::from(format!("{}.bin.gz", s_base.display())));
            sota_out = Some(s_out);
            Ok(())
        })();
        if let Err(e) = res {
            log::warn!("Script FOTA generation failed (non-fatal): {e}");
        }
    }

    Ok(Air6208FotaResult { fota: fota_out, sota: sota_out })
}

struct Air6208FotaResult {
    fota: PathBuf,
    sota: Option<PathBuf>,
}

// ─── Air724UG / RDA8910 - differential / script-only FOTA ───────────────────

/// RDA8910 差分/压缩外部工具 `dtools` 的搜索根（相对可执行文件）。
/// 末项为 luatos-sdk-rda8910 与 luatos-cli 同级的开发布局便利项，仅该布局存在时命中。
const RDA_DTOOLS_SEARCH_ROOTS: &[&str] = &["dtools", "../dtools", "../../dtools", "../../../luatos-sdk-rda8910/idh.code/tools/linux"];

/// 查找 RDA8910 差分工具 `dtools`（镜像 `find_fota_toolkit`，与 ec7xx 保持一致）：
/// 1. 显式 `--fota-toolkit <路径>`
/// 2. 可执行文件同级目录（`dtools/` 等，含 luatos-sdk-rda8910 同级开发布局）
fn find_rda_dtools(explicit: Option<&str>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        let path = PathBuf::from(p);
        anyhow::ensure!(path.exists(), "dtools not found at: {p}");
        return Ok(path);
    }
    let base = exe_dir();
    let tool_name = if cfg!(target_os = "windows") { "dtools.exe" } else { "dtools" };
    for root in RDA_DTOOLS_SEARCH_ROOTS {
        let dir = base.join(root);
        let candidate = dir.join(tool_name);
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    bail!(
        "dtools not found. Provide --fota-toolkit <dtools path> \
         (e.g. luatos-sdk-rda8910/idh.code/tools/linux/dtools) or place dtools/ next to luatos-cli."
    )
}

/// 运行 dtools：以工具自身目录为工作目录，Linux 下设置 LD_LIBRARY_PATH 指向其 lib/。
fn run_dtools(dtools: &Path, args: &[&str]) -> Result<()> {
    let dir = dtools.parent().context("dtools 无父目录")?;
    let mut cmd = Command::new(dtools);
    cmd.args(args).current_dir(dir);
    #[cfg(unix)]
    {
        let lib_dir = dir.join("lib");
        if lib_dir.exists() {
            let mut val = lib_dir.clone().into_os_string();
            if let Some(prev) = std::env::var_os("LD_LIBRARY_PATH") {
                val.push(":");
                val.push(prev);
            }
            cmd.env("LD_LIBRARY_PATH", val);
        }
    }
    let output = cmd.output().with_context(|| format!("launch {:?}", dtools))?;
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("dtools {args:?} 退出码 {:?}\nstdout: {}\nstderr: {}", output.status.code(), stdout.trim(), stderr.trim());
    }
    Ok(())
}

/// RDA8910 FOTA 参数（来自 info.json 的 fota.* / download.*）。
struct Rda8910FotaParams {
    magic: u32,
    core_addr: u32,
    script_addr: u32,
    cp_version: u32,
}

/// 从 info.json 提取 RDA8910 FOTA 参数。
fn rda8910_fota_params(info: &luatos_soc::SocInfo) -> Result<Rda8910FotaParams> {
    let fota = info.fota.as_ref().context("info.json 缺少 fota 配置")?;
    let magic = parse_hex_addr(fota.get("magic_num").and_then(|v| v.as_str()).context("info.json 缺少 fota.magic_num")?).context("invalid fota.magic_num")? as u32;
    let cp_version = parse_hex_addr(fota.get("cp_version").and_then(|v| v.as_str()).unwrap_or("0")).unwrap_or(0) as u32;
    let core_addr = parse_hex_addr(info.download.core_addr.as_deref().context("info.json 缺少 download.core_addr")?).context("invalid download.core_addr")? as u32;
    let script_addr = parse_hex_addr(info.download.script_addr.as_deref().unwrap_or("0")).unwrap_or(0) as u32;
    Ok(Rda8910FotaParams {
        magic,
        core_addr,
        script_addr,
        cp_version,
    })
}

/// RDA8910 升级包大小上限：`(fs.total_len - 64) * 1024` 字节（对齐 luatools_py3 soc.py）。
fn rda8910_fota_size_limit(info: &luatos_soc::SocInfo) -> Option<u64> {
    let total_len = info.fs.as_ref()?.get("total_len")?;
    let total = total_len.as_u64().or_else(|| total_len.as_str().and_then(parse_hex_addr))?;
    Some(total.saturating_sub(64).saturating_mul(1024))
}

/// 打包单个 RDA8910 分区块（AP 或脚本）：
/// `dtools lzmare2` 压缩 → 纯 Rust 的 `CoreUpgrade_SectorCalMD5Struct` 包装。
///
/// `lzmare2` 产物为 RDA 专用 LZMA 载荷（`lzmaFileHeader_t` + `lzmaBlockHeader_t` 块），
/// 设备端 `fota_decode_bsp` 为闭源实现，标准 LZMA 无法解码，故压缩必须调用外部 dtools。
/// Sector.BlockLen 取载荷内 lzmaFileHeader 声明的块大小（lzmare2 默认 64KB，单位 KB<<10）。
fn pack_rda8910_block(dtools: &Path, raw: &[u8], magic: u32, addr: u32, work_dir: &Path) -> Result<Vec<u8>> {
    let raw_path = work_dir.join("block_raw.bin");
    let zip_path = work_dir.join("block_zip.bin");
    fs::write(&raw_path, raw).context("write raw block")?;
    run_dtools(dtools, &["lzmare2", &raw_path.to_string_lossy(), &zip_path.to_string_lossy()]).context("lzmare2 压缩失败")?;
    let compressed = fs::read(&zip_path).context("read lzmare2 output")?;
    let lzma_block_len = if compressed.len() >= 6 {
        (u16::from_le_bytes([compressed[4], compressed[5]]) as u32) << 10
    } else {
        0
    };
    Ok(luatos_soc::ota::build_rda_sector_block(raw, &compressed, magic, addr, lzma_block_len))
}

/// 从 PAC 提取分区并压缩为 Sector 块（AP 可选、脚本若存在），返回拼接后的 common 数据。
fn build_rda8910_sectors(pac_path: &Path, params: &Rda8910FotaParams, dtools: &Path, include_ap: bool, work_dir: &Path) -> Result<Vec<u8>> {
    let pac_data = fs::read(pac_path).with_context(|| format!("read PAC {}", pac_path.display()))?;
    let pac = luatos_flash::rda8910::parse_pac(&pac_data).context("parse PAC")?;

    let mut blocks: Vec<u8> = Vec::new();
    let mut parts: Vec<&str> = Vec::new();
    if include_ap {
        let ap = pac.find("AP").context("PAC 缺少 AP 条目，无法生成差分升级包")?;
        let ap_data = ap.data.as_deref().context("AP 条目无数据")?;
        let block = pack_rda8910_block(dtools, ap_data, params.magic, params.core_addr, work_dir)?;
        log::info!("AP 全量块: {}B (addr=0x{:08X})", block.len(), params.core_addr);
        parts.push("AP");
        blocks.extend_from_slice(&block);
    }
    if let Some(lua) = pac.find("LUA") {
        if let Some(lua_data) = lua.data.as_deref() {
            let block = pack_rda8910_block(dtools, lua_data, params.magic, params.script_addr, work_dir)?;
            log::info!("脚本块: {}B (addr=0x{:08X})", block.len(), params.script_addr);
            parts.push("脚本");
            blocks.extend_from_slice(&block);
        }
    }
    anyhow::ensure!(!blocks.is_empty(), "PAC 中无 AP/LUA 条目可打包，无法生成升级包");
    log::info!("FOTA common 数据: {}B ({})", blocks.len(), parts.join("+"));
    Ok(blocks)
}

/// Build differential FOTA for RDA8910：AP 全量 + 脚本（common）+ CP 差分（SDK，`fotacreate2`）。
///
/// `STDVersion[4]` 取旧包 CP 版本（设备要求等于当前 CP 版本）；CP 版本相同则无 SDK 数据。
fn build_rda8910_diff_fota(new_soc: &str, old_soc: &str, dtools: &Path, out_path: &Path) -> Result<()> {
    let tmp = tempfile::tempdir().context("tempdir")?;
    let up = unpack(new_soc, &tmp.path().join("new"))?;
    let info = &up.info;
    let params = rda8910_fota_params(info)?;
    let old_up = unpack(old_soc, &tmp.path().join("old"))?;
    let old_params = rda8910_fota_params(&old_up.info)?;

    // ── CP 差分（SDK 数据）──
    let mut cp_version = params.cp_version;
    let mut sdk_path: Option<PathBuf> = None;
    if old_params.cp_version != params.cp_version {
        anyhow::ensure!(
            old_params.cp_version >> 12 == params.cp_version >> 12,
            "新旧固件包芯片或 modem 类型不一致 0x{:08X} -> 0x{:08X}",
            old_params.cp_version,
            params.cp_version
        );
        let fota_cp_xml = old_up.dir.join("fota_cp.xml");
        anyhow::ensure!(fota_cp_xml.exists(), "旧包缺少 fota_cp.xml，无法生成 CP 差分");
        log::info!("CP 差分 0x{:08X} -> 0x{:08X}（fotacreate2）", old_params.cp_version, params.cp_version);
        let pac_arg = format!("{},{},{}", old_up.rom_path.display(), up.rom_path.display(), fota_cp_xml.display());
        let delta = tmp.path().join("delta.bin");
        run_dtools(dtools, &["fotacreate2", "--pac", &pac_arg, &delta.to_string_lossy()]).context("fotacreate2 生成 CP 差分包失败")?;
        anyhow::ensure!(delta.exists(), "CP 差分包 delta.bin 未生成");
        sdk_path = Some(delta);
        cp_version = old_params.cp_version;
    } else {
        log::info!("CP 版本相同 0x{:08X}，无 CP 差分", params.cp_version);
    }

    // ── AP 全量 + 脚本 sectors（common）──
    let blocks = build_rda8910_sectors(&up.rom_path, &params, dtools, true, tmp.path())?;
    let fota_zip = tmp.path().join("fota.zip");
    fs::write(&fota_zip, &blocks).context("write fota.zip")?;

    let dummy = create_dummy(tmp.path())?;
    let sdk_ref = sdk_path.as_deref().unwrap_or(&dummy);
    luatos_soc::ota::assemble_ota_package(params.magic, 0, "0", cp_version, "0", 0, &fota_zip, sdk_ref, out_path).context("assemble OTA package")?;

    // 大小检查：fs.total_len（KB）− 64KB 预留（对齐 luatools_py3）
    if let Some(limit) = rda8910_fota_size_limit(info) {
        let size = fs::metadata(out_path).map(|m| m.len()).unwrap_or(0);
        anyhow::ensure!(size <= limit, "升级包太大 {size}B > 上限 {limit}B (fs.total_len−64KB)");
    } else {
        log::warn!("info.json 缺少 fs.total_len，跳过升级包大小检查");
    }
    Ok(())
}

/// Build script-only FOTA for RDA8910：仅压缩 PAC 内 LUA 脚本分区，无 AP、无 CP 差分。
fn build_rda8910_script_only_fota(new_soc: &str, dtools: &Path, out_path: &Path) -> Result<()> {
    let tmp = tempfile::tempdir().context("tempdir")?;
    let up = unpack(new_soc, &tmp.path().join("new"))?;
    let info = &up.info;
    let params = rda8910_fota_params(info)?;

    let blocks = build_rda8910_sectors(&up.rom_path, &params, dtools, false, tmp.path())?;
    let fota_zip = tmp.path().join("fota.zip");
    fs::write(&fota_zip, &blocks).context("write fota.zip")?;

    let dummy = create_dummy(tmp.path())?;
    luatos_soc::ota::assemble_ota_package(params.magic, 0, "0", params.cp_version, "0", 0, &fota_zip, &dummy, out_path).context("assemble OTA package")?;

    if let Some(limit) = rda8910_fota_size_limit(info) {
        let size = fs::metadata(out_path).map(|m| m.len()).unwrap_or(0);
        anyhow::ensure!(size <= limit, "升级包太大 {size}B > 上限 {limit}B (fs.total_len−64KB)");
    } else {
        log::warn!("info.json 缺少 fs.total_len，跳过升级包大小检查");
    }
    Ok(())
}

// ─── Public command handler ─────────────────────────────────────────────────

// 7-arg CLI entry point — mirrors the FotaCommands::Build clap struct directly
// rather than threading a config object. Acceptable to silence the lint here.
#[allow(clippy::too_many_arguments)]
pub fn cmd_fota_build(
    new_soc: &str,
    old_soc: Option<&str>,
    output: Option<&str>,
    fota_toolkit_path: Option<&str>,
    force_par: bool,
    script_only: bool,
    format: &OutputFormat,
) -> Result<()> {
    anyhow::ensure!(Path::new(new_soc).exists(), "New SOC not found: {new_soc}");
    if let Some(old) = old_soc {
        anyhow::ensure!(Path::new(old).exists(), "Old SOC not found: {old}");
    }

    let info = luatos_soc::read_soc_info(new_soc)?;
    // 保留原始 chip 字符串用于输出文件名与结果展示（行为与改动前一致）
    let chip = info.chip.chip_type.as_str();

    match info.family() {
        // EC7xx / EC618 - differential or script-only
        ChipFamily::Ec718 => {
            if script_only {
                let out_path: PathBuf = output.map(PathBuf::from).unwrap_or_else(|| PathBuf::from(format!("{chip}_script_fota.bin")));
                build_ec7xx_script_only_fota(new_soc, &out_path)?;
                let size = fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
                print_result(format, chip, new_soc, old_soc, &[(&out_path, size)])?;
            } else {
                let old = old_soc.ok_or_else(|| anyhow::anyhow!("Full FOTA for EC7xx/EC618 is not yet supported. Please provide --old <old.soc> for differential FOTA."))?;
                let (toolkit, toolkit_dir) = find_fota_toolkit(chip, fota_toolkit_path)?;
                let out_path: PathBuf = output.map(PathBuf::from).unwrap_or_else(|| PathBuf::from(format!("{chip}_fota.bin")));

                build_ec7xx_fota(new_soc, old, chip, &toolkit, &toolkit_dir, &out_path, force_par)?;
                let size = fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
                print_result(format, chip, new_soc, old_soc, &[(&out_path, size)])?;
            }
        }

        // Air1601 / Air1602 / CCM4211 - full or script-only
        ChipFamily::Ccm4211 => {
            let out_path: PathBuf = output.map(PathBuf::from).unwrap_or_else(|| PathBuf::from(format!("{chip}_fota.bin")));
            if script_only {
                build_ccm4211_script_only_fota(new_soc, &out_path)?;
            } else {
                if old_soc.is_some() {
                    log::warn!("--old is ignored for Air1601/Air1602/CCM4211: only full FOTA is supported");
                }
                build_ccm4211_fota(new_soc, &out_path)?;
            }
            let size = fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
            print_result(format, chip, new_soc, old_soc, &[(&out_path, size)])?;
        }

        // Air8101 / BK72XX - full or script-only (new format only)
        ChipFamily::Bk72xx => {
            if old_soc.is_some() {
                log::warn!("--old is ignored for Air8101/BK72XX: only full/script package generation is supported");
            }
            let out_path: PathBuf = output.map(PathBuf::from).unwrap_or_else(|| PathBuf::from(format!("{chip}_fota.bin")));
            build_bk72xx_fota(new_soc, &out_path, script_only)?;
            let size = fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
            print_result(format, chip, new_soc, old_soc, &[(&out_path, size)])?;
        }

        // Air6208 / XT804 - full only
        ChipFamily::Xt804 => {
            if old_soc.is_some() {
                log::warn!("--old is ignored for Air6208/XT804: only full FOTA is supported");
            }
            if script_only {
                log::warn!("--script-only is not supported for Air6208/XT804; ignored");
            }
            let out_base: PathBuf = output
                .map(|p| {
                    let pb = PathBuf::from(p);
                    if pb.extension().map(|e| e == "fota" || e == "sota").unwrap_or(false) {
                        pb.with_extension("")
                    } else {
                        pb
                    }
                })
                .unwrap_or_else(|| PathBuf::from(chip));

            let result = build_air6208_fota(new_soc, &out_base)?;
            let mut outputs = vec![];
            let fota_size = fs::metadata(&result.fota).map(|m| m.len()).unwrap_or(0);
            outputs.push((result.fota.as_path(), fota_size));

            if let Some(ref sota) = result.sota {
                let sota_size = fs::metadata(sota).map(|m| m.len()).unwrap_or(0);
                outputs.push((sota.as_path(), sota_size));
            }

            print_result(format, chip, new_soc, old_soc, &outputs)?;
        }

        // Air724UG / RDA8910 - differential or script-only（需外部 dtools，镜像 EC7xx）
        ChipFamily::Rda8910 => {
            let dtools = find_rda_dtools(fota_toolkit_path)?;
            if script_only {
                if old_soc.is_some() {
                    log::warn!("--old 在 --script-only 模式下忽略");
                }
                let out_path: PathBuf = output.map(PathBuf::from).unwrap_or_else(|| PathBuf::from(format!("{chip}_script_fota.sota")));
                build_rda8910_script_only_fota(new_soc, &dtools, &out_path)?;
                let size = fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
                print_result(format, chip, new_soc, old_soc, &[(&out_path, size)])?;
            } else {
                let old = old_soc.ok_or_else(|| {
                    anyhow::anyhow!(
                        "Full FOTA for RDA8910 is not yet supported. Please provide --old <old.soc> for differential FOTA, or use --script-only."
                    )
                })?;
                let out_path: PathBuf = output.map(PathBuf::from).unwrap_or_else(|| PathBuf::from(format!("{chip}_fota.sota")));
                build_rda8910_diff_fota(new_soc, old, &dtools, &out_path)?;
                let size = fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
                print_result(format, chip, new_soc, old_soc, &[(&out_path, size)])?;
            }
        }

        // 其余族（Sf32lb58 / Air6201 / Unknown）不支持 FOTA
        ChipFamily::Sf32lb58 | ChipFamily::Air6201 | ChipFamily::Unknown => bail!(
            "FOTA not supported for chip '{chip}'. \
             Supported: EC7xx/EC618/Air8000 (differential), Air1601/Air1602/CCM4211 (full or --script-only), Air8101/BK72XX (new-format full or --script-only), Air6208/XT804 (full), Air724UG/RDA8910 (full or --script-only)."
        ),
    }

    Ok(())
}

fn print_result(format: &OutputFormat, chip: &str, new_soc: &str, old_soc: Option<&str>, outputs: &[(&Path, u64)]) -> anyhow::Result<()> {
    match format {
        OutputFormat::Text => {
            println!("FOTA package built:");
            println!("  Chip:    {chip}");
            println!("  New SOC: {new_soc}");
            if let Some(old) = old_soc {
                println!("  Old SOC: {old}");
            }
            for (path, size) in outputs {
                println!("  Output:  {}  ({size} bytes)", path.display());
            }
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            let out_list: Vec<serde_json::Value> = outputs
                .iter()
                .map(|(p, s)| serde_json::json!({ "path": p.display().to_string(), "size_bytes": s }))
                .collect();
            event::emit_result(
                format,
                "fota.build",
                "ok",
                serde_json::json!({
                    "chip": chip,
                    "new_soc": new_soc,
                    "old_soc": old_soc,
                    "outputs": out_list,
                }),
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use serde_json::Value;
    use tempfile::tempdir;

    use super::{cmd_fota_build, fota_delta_size_limit};
    use crate::OutputFormat;

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    fn ec7xx_soc_path() -> String {
        repo_root()
            .join("refs")
            .join("soc_files")
            .join("LuatOS-SoC_V2029_Air780EPM_1.soc")
            .to_string_lossy()
            .to_string()
    }

    /// Helper: unpack a SOC, inject a dummy script.bin, and repack it.
    /// Returns the path to the repacked SOC file.
    fn inject_dummy_script_bin(src_soc: &str, tmp_dir: &std::path::Path) -> std::path::PathBuf {
        let unpack_dir = tmp_dir.join("unpacked");
        let unpacked = luatos_soc::unpack_soc(src_soc, &unpack_dir).unwrap();
        // Write a dummy script.bin (1 KB of zeros)
        let script_path = unpacked.dir.join(&unpacked.info.script.file);
        fs::write(&script_path, vec![0u8; 1024]).unwrap();
        // Repack
        let repacked = tmp_dir.join("repacked.soc");
        luatos_soc::pack_soc(&unpacked.dir, &repacked.to_string_lossy()).unwrap();
        repacked
    }

    #[test]
    fn ec7xx_script_only_builds_without_old_soc() {
        let tmp = tempdir().unwrap();
        // The real ec7xx SOC does not ship script.bin inside the archive,
        // so inject a dummy one before running the build.
        let repacked_soc = inject_dummy_script_bin(&ec7xx_soc_path(), tmp.path());
        let out = tmp.path().join("air780epm_script.sota");

        let result = cmd_fota_build(
            &repacked_soc.to_string_lossy(),
            None,
            Some(out.to_str().unwrap()),
            Some("C:\\definitely\\missing\\FotaToolkit.exe"),
            false,
            true,
            &OutputFormat::Text,
        );
        assert!(result.is_ok(), "expected script-only build to succeed, got: {result:?}");

        let data = fs::read(&out).unwrap();
        assert!(u32::from_le_bytes(data[52..56].try_into().unwrap()) > 0);
        assert_eq!(u32::from_le_bytes(data[56..60].try_into().unwrap()), 0);
    }

    #[test]
    fn ec7xx_script_only_rejects_missing_script_bin() {
        let tmp = tempdir().unwrap();
        let src_soc = ec7xx_soc_path();
        let unpack_dir = tmp.path().join("unpacked");
        let unpacked = luatos_soc::unpack_soc(&src_soc, &unpack_dir).unwrap();
        let info_path = unpacked.dir.join("info.json");
        let mut info_json: Value = serde_json::from_slice(&fs::read(&info_path).unwrap()).unwrap();
        info_json["script"]["file"] = Value::String("missing_script.bin".to_string());
        info_json["rom"]["fs"]["script"] = Value::Null;
        fs::write(&info_path, serde_json::to_vec(&info_json).unwrap()).unwrap();

        let broken_soc = tmp.path().join("broken.zip");
        luatos_soc::pack_soc(&unpacked.dir, &broken_soc.to_string_lossy()).unwrap();

        let out = tmp.path().join("out.sota");
        let result = cmd_fota_build(
            &broken_soc.to_string_lossy(),
            None,
            Some(out.to_str().unwrap()),
            Some("C:\\definitely\\missing\\FotaToolkit.exe"),
            false,
            true,
            &OutputFormat::Text,
        );
        let err = result.unwrap_err().to_string();
        assert!(err.contains("script file not found"), "expected missing script error, got: {err}");
    }

    #[test]
    fn fota_delta_size_limit_parses_region_len() {
        // Real value from LuatOS-SoC_V2029_Air780EPM: 0x96000 - 96KB = 516096
        let text = "#define FLASH_FOTA_REGION_LEN (0x96000)\n";
        assert_eq!(fota_delta_size_limit(text), Some(0x96000 - 96 * 1024));
    }

    #[test]
    fn fota_delta_size_limit_uses_last_occurrence() {
        // Matches luatools_py3 soc.py's greedy regex: last match wins
        let text = "#define FLASH_FOTA_REGION_LEN (0x96000)\n#define FLASH_FOTA_REGION_LEN (0x106000)\n";
        assert_eq!(fota_delta_size_limit(text), Some(0x106000 - 96 * 1024));
    }

    #[test]
    fn fota_delta_size_limit_returns_none_when_missing_or_malformed() {
        assert_eq!(fota_delta_size_limit(""), None);
        assert_eq!(fota_delta_size_limit("#define TYPE_EC718HM 1\n"), None);
        assert_eq!(fota_delta_size_limit("#define FLASH_FOTA_REGION_LEN (nonsense)\n"), None);
    }

    #[test]
    fn fota_delta_size_limit_saturates_below_reserved_area() {
        let text = "#define FLASH_FOTA_REGION_LEN (0x1000)\n";
        assert_eq!(fota_delta_size_limit(text), Some(0));
    }

    // ── RDA8910 合成 PAC 与测试 soc ─────────────────────────────────────────

    const RDA_PAC_HEADER_SIZE: usize = 2124;
    const RDA_PAC_FILE_HEADER_SIZE: usize = 2580;
    const RDA_PAC_MAGIC: u32 = 0xFFFA_FFFA;

    fn utf16le(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
    }

    /// 构造合成 PAC：entries 为 (id, name, addr, data)，整数域小端、字符串 UTF-16LE。
    fn make_test_pac(entries: &[(&str, &str, u32, &[u8])]) -> Vec<u8> {
        let file_offset = RDA_PAC_HEADER_SIZE;
        let mut data_area = Vec::new();
        let mut offsets = Vec::new();
        let mut cursor = file_offset + RDA_PAC_FILE_HEADER_SIZE * entries.len();
        for (_, _, _, data) in entries {
            offsets.push(cursor);
            data_area.extend_from_slice(data);
            cursor += data.len();
        }
        let total = cursor;

        let mut pac = vec![0u8; file_offset];
        let ver = utf16le("BP_R1.0.0");
        pac[0..ver.len()].copy_from_slice(&ver);
        pac[48..52].copy_from_slice(&(total as u32).to_le_bytes()); // pac_size
        let prd = utf16le("UIX8910_MODEM");
        pac[52..52 + prd.len()].copy_from_slice(&prd); // prd_name
        pac[1076..1080].copy_from_slice(&(entries.len() as u32).to_le_bytes()); // file_count
        pac[1080..1084].copy_from_slice(&(file_offset as u32).to_le_bytes()); // file_offset
        pac[2116..2120].copy_from_slice(&RDA_PAC_MAGIC.to_le_bytes()); // magic

        for (i, (id, name, addr, data)) in entries.iter().enumerate() {
            let mut fh = vec![0u8; RDA_PAC_FILE_HEADER_SIZE];
            fh[0..4].copy_from_slice(&(RDA_PAC_FILE_HEADER_SIZE as u32).to_le_bytes()); // hdr_size
            let idb = utf16le(id);
            fh[4..4 + idb.len()].copy_from_slice(&idb); // szFileID
            let nameb = utf16le(name);
            fh[516..516 + nameb.len()].copy_from_slice(&nameb); // szFileName
            fh[1540..1544].copy_from_slice(&(data.len() as u32).to_le_bytes()); // file_size
            fh[1552..1556].copy_from_slice(&(offsets[i] as u32).to_le_bytes()); // offset
            fh[1564..1568].copy_from_slice(&addr.to_le_bytes()); // dwAddress
            pac.extend_from_slice(&fh);
        }
        pac.extend_from_slice(&data_area);
        pac
    }

    /// 构造合成 .soc：目录内放 info.json + luatos.pac + fota_cp.xml，再 pack_soc 成归档。
    /// 每次调用用原子计数生成唯一子目录/文件名（差分测试需要新旧两个 soc）。
    fn make_test_soc(tmp: &std::path::Path, fs_total_len: Option<u64>, ap: Option<&[u8]>, lua: Option<&[u8]>, cp_version: &str) -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let name = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = tmp.join(format!("soc_{name}_dir"));
        std::fs::create_dir_all(&dir).unwrap();

        let mut entries: Vec<(&str, &str, u32, &[u8])> = Vec::new();
        if let Some(ap_data) = ap {
            entries.push(("AP", "csdk.img", 0x6001_0000, ap_data));
        }
        if let Some(lua_data) = lua {
            entries.push(("LUA", "script.bin", 0x6023_0000, lua_data));
        }
        std::fs::write(dir.join("luatos.pac"), make_test_pac(&entries)).unwrap();
        // CP 差分需要旧包的 fota_cp.xml
        std::fs::write(dir.join("fota_cp.xml"), b"<pacdiff/>\n").unwrap();

        let fs_json = match fs_total_len {
            Some(n) => serde_json::json!({ "total_len": n, "format_len": "1000" }),
            None => serde_json::json!({}),
        };
        let info = serde_json::json!({
            "version": 1,
            "chip": {"type": "uis8910", "ram": {"total": 4000, "sys": 256, "lua": 128}},
            "rom": {"file": "luatos.pac", "img_file": "csdk.img", "fs": {"script": {"offset": "0", "size": 512, "type": "luadb"}}},
            "script": {"file": "script.bin", "lua": "5.3", "bitw": 64},
            "fs": fs_json,
            "download": {"core_addr": "00010000", "script_addr": "00230000"},
            "fota": {"magic_num": "da188800", "block_len": "40000", "cp_version": cp_version}
        });
        std::fs::write(dir.join("info.json"), serde_json::to_vec(&info).unwrap()).unwrap();

        let soc_path = tmp.join(format!("soc_{name}.soc"));
        luatos_soc::pack_soc(&dir, &soc_path.to_string_lossy()).unwrap();
        soc_path.to_string_lossy().to_string()
    }

    fn read_u32(data: &[u8], off: usize) -> u32 {
        u32::from_le_bytes(data[off..off + 4].try_into().unwrap())
    }

    /// 确定性伪随机字节（LCG），避免 LZMA 把重复数据压得过小。
    fn pseudo_random(len: usize) -> Vec<u8> {
        let mut seed: u32 = 0x1234_5678;
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            out.push((seed >> 24) as u8);
        }
        out
    }

    /// 写一个模拟 `dtools` 的可执行脚本（unix）：`lzmare2` 写固定载荷（8B lzmaFileHeader +
    /// 无内容块），`fotacreate2` 写固定 delta。产物不保证设备兼容，仅验证打包结构。
    #[cfg(unix)]
    fn fake_dtools_path(tmp: &std::path::Path) -> String {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmp.join("fake_dtools");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dtools");
        let script = r#"#!/bin/sh
case "$1" in
  lzmare2) printf '\x10\xb4\x15\x00\x40\x00\x01\x00' > "$3"; cat "$2" >> "$3" ;;
  fotacreate2) printf 'RDAOTA02fake-delta' > "$4" ;;
  *) exit 1 ;;
esac
exit 0
"#;
        std::fs::write(&path, script).unwrap();
        let mut perm = std::fs::metadata(&path).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&path, perm).unwrap();
        path.to_string_lossy().to_string()
    }

    // ── 纯逻辑测试（无外部工具，跨平台）──────────────────────────────────

    #[test]
    fn rda8910_fota_params_parsing() {
        let tmp = tempdir().unwrap();
        let soc = make_test_soc(tmp.path(), Some(1664), Some(&[0xAB; 8]), Some(&[0xCD; 8]), "8910A002");
        let up = luatos_soc::unpack_soc(&soc, &tmp.path().join("up")).unwrap();
        let params = super::rda8910_fota_params(&up.info).unwrap();
        assert_eq!(params.magic, 0xDA18_8800);
        assert_eq!(params.cp_version, 0x8910_A002);
        assert_eq!(params.core_addr, 0x10000);
        assert_eq!(params.script_addr, 0x230000);
    }

    #[test]
    /// `soc combine` 在 RDA8910 上替换 PAC 内 LUA 条目并重打包（跨平台，无需外部工具）。
    #[test]
    fn soc_combine_replaces_rda_pac_lua() {
        use crate::cmd_soc::cmd_soc_combine;
        let tmp = tempdir().unwrap();
        let soc = make_test_soc(tmp.path(), Some(1664), Some(&[0xAB; 4096]), Some(&[0xCD; 1024]), "8910A002");
        let new_script = tmp.path().join("new_script.bin");
        fs::write(&new_script, vec![0xEE; 200]).unwrap();
        let out = tmp.path().join("patched.soc");

        // RDA8910 无需 --addr（PAC 内 LUA 地址为权威值）
        cmd_soc_combine(&soc, &new_script.to_string_lossy(), None, Some(out.to_str().unwrap()), &OutputFormat::Text).unwrap();

        // 解包结果，验证 PAC 内 LUA 被替换、AP 原样保留、CRC 重算正确
        let up = luatos_soc::unpack_soc(&out.to_string_lossy(), &tmp.path().join("up")).unwrap();
        let pac_data = fs::read(&up.rom_path).unwrap();
        let pac = luatos_flash::rda8910::parse_pac(&pac_data).unwrap();
        assert_eq!(pac.find("LUA").unwrap().data.as_deref().unwrap(), &[0xEE; 200][..]);
        assert_eq!(pac.find("AP").unwrap().data.as_deref().unwrap(), &[0xAB; 4096][..]);
        // 官方工具会校验的 crc1/crc2 与重算一致
        assert_eq!(
            u16::from_le_bytes(pac_data[2120..2122].try_into().unwrap()),
            luatos_flash::rda8910::pac_crc16_arc(&pac_data[..2120])
        );
        assert_eq!(
            u16::from_le_bytes(pac_data[2122..2124].try_into().unwrap()),
            luatos_flash::rda8910::pac_crc16_arc(&pac_data[2124..])
        );
    }

    fn rda8910_size_limit_parses_total_len() {
        let tmp = tempdir().unwrap();
        let soc = make_test_soc(tmp.path(), Some(1664), Some(&[0xAB; 8]), None, "8910A002");
        let up = luatos_soc::unpack_soc(&soc, &tmp.path().join("up")).unwrap();
        assert_eq!(super::rda8910_fota_size_limit(&up.info), Some((1664 - 64) * 1024));

        let soc2 = make_test_soc(tmp.path(), None, Some(&[0xAB; 8]), None, "8910A002");
        let up2 = luatos_soc::unpack_soc(&soc2, &tmp.path().join("up2")).unwrap();
        assert_eq!(super::rda8910_fota_size_limit(&up2.info), None);
    }

    // ── 集成测试（需要 fake dtools，unix）─────────────────────────────────

    /// 无 --old 且非脚本 → 全量不支持，提示提供 --old（镜像 EC7xx）。
    #[cfg(unix)]
    #[test]
    fn rda8910_full_requires_old() {
        let tmp = tempdir().unwrap();
        let soc = make_test_soc(tmp.path(), Some(1664), Some(&[0xAB; 4096]), Some(&[0xCD; 1024]), "8910A002");
        let dtools = fake_dtools_path(tmp.path());
        let out = tmp.path().join("uis8910_fota.sota");
        let result = cmd_fota_build(&soc, None, Some(out.to_str().unwrap()), Some(&dtools), false, false, &OutputFormat::Text);
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Please provide --old"), "expected --old hint, got: {err}");
    }

    /// 脚本-only：只有 1 个 Sector 块。
    #[cfg(unix)]
    #[test]
    fn rda8910_script_only_fota_builds() {
        let tmp = tempdir().unwrap();
        let soc = make_test_soc(tmp.path(), Some(1664), Some(&[0xAB; 4096]), Some(&[0xCD; 1024]), "8910A002");
        let dtools = fake_dtools_path(tmp.path());
        let out = tmp.path().join("uis8910_script_fota.sota");
        let result = cmd_fota_build(&soc, None, Some(out.to_str().unwrap()), Some(&dtools), false, true, &OutputFormat::Text);
        assert!(result.is_ok(), "expected success, got: {result:?}");

        let data = fs::read(&out).unwrap();
        assert_eq!(read_u32(&data, 0), 0xDA18_8800);
        let common_len = read_u32(&data, 52) as usize;
        assert_eq!(common_len, data.len() - 92);
        let head = &data[92..];
        assert_eq!(read_u32(head, 0), 0xDA18_8800);
        assert_eq!(36 + read_u32(head, 4) as usize, head.len(), "script-only 不应有第二个块");
        assert_eq!(read_u32(head, 32), 0x230000, "脚本 StartAddress=script_addr");
    }

    /// 差分：--old 提供不同 CP 版本 → SDK 数据为 fotacreate2 产物，STDVersion[4]=旧版本。
    #[cfg(unix)]
    #[test]
    fn rda8910_differential_fota_builds() {
        let tmp = tempdir().unwrap();
        let new_soc = make_test_soc(tmp.path(), Some(1664), Some(&[0xAB; 4096]), Some(&[0xCD; 1024]), "8910A002");
        // 旧包 cp_version 不同但高 12 位一致
        let old_soc = make_test_soc(tmp.path(), Some(1664), Some(&[0x11; 4096]), None, "8910A001");
        let dtools = fake_dtools_path(tmp.path());
        let out = tmp.path().join("uis8910_diff.sota");
        let result = cmd_fota_build(&new_soc, Some(&old_soc), Some(out.to_str().unwrap()), Some(&dtools), false, false, &OutputFormat::Text);
        assert!(result.is_ok(), "expected success, got: {result:?}");

        let data = fs::read(&out).unwrap();
        // STDVersion[4] = 旧 CP 版本（设备要求等于当前版本）
        assert_eq!(read_u32(&data, 48), 0x8910_A001, "std_version[4]=旧 cp_version");
        let common_len = read_u32(&data, 52) as usize;
        let sdk_len = read_u32(&data, 56) as usize;
        assert_eq!(common_len + sdk_len, data.len() - 92);
        assert_eq!(sdk_len, 18, "SDK 数据为 fotacreate2 的 delta.bin");
    }

    /// 差分：CP 版本高 12 位不一致 → 芯片/modem 类型不匹配报错。
    #[cfg(unix)]
    #[test]
    fn rda8910_differential_rejects_modem_mismatch() {
        let tmp = tempdir().unwrap();
        let new_soc = make_test_soc(tmp.path(), Some(1664), Some(&[0xAB; 8]), None, "8910A002");
        let old_soc = make_test_soc(tmp.path(), Some(1664), Some(&[0x11; 8]), None, "8810A002");
        let dtools = fake_dtools_path(tmp.path());
        let out = tmp.path().join("out.sota");
        let result = cmd_fota_build(&new_soc, Some(&old_soc), Some(out.to_str().unwrap()), Some(&dtools), false, false, &OutputFormat::Text);
        let err = result.unwrap_err().to_string();
        assert!(err.contains("modem 类型不一致"), "expected mismatch error, got: {err}");
    }

    /// 差分路径：新包无 AP 条目 → 报"缺少 AP"。
    #[cfg(unix)]
    #[test]
    fn rda8910_full_missing_ap_rejected() {
        let tmp = tempdir().unwrap();
        let new_soc = make_test_soc(tmp.path(), Some(1664), None, Some(&[0xCD; 1024]), "8910A002");
        let old_soc = make_test_soc(tmp.path(), Some(1664), Some(&[0x11; 4096]), None, "8910A002");
        let dtools = fake_dtools_path(tmp.path());
        let out = tmp.path().join("out.sota");
        let result = cmd_fota_build(&new_soc, Some(&old_soc), Some(out.to_str().unwrap()), Some(&dtools), false, false, &OutputFormat::Text);
        let err = result.unwrap_err().to_string();
        assert!(err.contains("缺少 AP"), "expected AP missing error, got: {err}");
    }

    /// 差分路径：fs.total_len 过小 → 大小检查报错。
    #[cfg(unix)]
    #[test]
    fn rda8910_size_limit_enforced() {
        let tmp = tempdir().unwrap();
        // fs.total_len=65 → 上限 (65-64)*1024 = 1KB，伪随机数据经 fake lzmare2 拷贝后输出必然超限
        let new_soc = make_test_soc(tmp.path(), Some(65), Some(&pseudo_random(4096)), Some(&pseudo_random(1024)), "8910A002");
        let old_soc = make_test_soc(tmp.path(), Some(1664), Some(&[0x11; 4096]), None, "8910A002");
        let dtools = fake_dtools_path(tmp.path());
        let out = tmp.path().join("out.sota");
        let result = cmd_fota_build(&new_soc, Some(&old_soc), Some(out.to_str().unwrap()), Some(&dtools), false, false, &OutputFormat::Text);
        let err = result.unwrap_err().to_string();
        assert!(err.contains("升级包太大"), "expected size error, got: {err}");
    }
}
