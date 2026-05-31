// fota build - FOTA (firmware-over-the-air) package generation.
//
// Supported chip families:
//   EC7xx / EC618 / Air8000  - differential (--new + --old) via FotaToolkit.exe + Rust OTA assembler
//   Air1601 / Air1602 / CCM4211 - full via Rust OTA (LZMA compress + assemble), --script-only for script-only update
//   Air8101 / BK72XX            - new-format full/script FOTA via Rust BK72XX packer
//   Air6208 / XT804          - full only via air101_flash.exe (bundled in the .soc)
//
// External tools required:
//   FotaToolkit.exe - delta-diff engine, must run from its own dtools directory (EC7xx/EC618)
//   air101_flash.exe - W800 OTA image builder, extracted from the .soc itself (Air6208)
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

use crate::{event, OutputFormat};

// ─── Tool discovery ──────────────────────────────────────────────────────────

const DTOOLS_SEARCH_ROOTS: &[&str] = &["dtools", "refs/origin_tools/dtools", "../refs/origin_tools/dtools", "../../refs/origin_tools/dtools"];

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
        "air780ehm" => "ec718hm.json",
        "air780epv" | "ec718pv" => "ec718pv.json",
        "air780epm" | "ec718pm" => "ec718pm.json",
        _ => "ec718p.json",
    }
}

fn find_fota_toolkit(chip: &str, explicit: Option<&str>) -> Result<(PathBuf, PathBuf)> {
    if let Some(p) = explicit {
        let path = PathBuf::from(p);
        anyhow::ensure!(path.exists(), "FotaToolkit not found at: {p}");
        let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        return Ok((path, dir));
    }

    let base = exe_dir();
    let variant = chip_dtools_variant(chip);
    for root in DTOOLS_SEARCH_ROOTS {
        let dir = base.join(root).join(variant);
        let candidate = dir.join("FotaToolkit.exe");
        if candidate.exists() {
            return Ok((candidate, dir));
        }
    }

    bail!(
        "FotaToolkit.exe not found for chip '{chip}' (variant: {variant}). \
         Provide --fota-toolkit <path> or place dtools/ next to luatos-cli."
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

fn extract_script_from_rom(up: &luatos_soc::UnpackedSoc, out_path: &Path) -> Result<()> {
    let script_part = up
        .info
        .rom
        .fs
        .as_ref()
        .and_then(|fs| fs.script.as_ref())
        .ok_or_else(|| anyhow::anyhow!("info.json missing rom.fs.script"))?;

    let offset_str = script_part.offset.as_deref().ok_or_else(|| anyhow::anyhow!("info.json missing rom.fs.script.offset"))?;
    let offset = parse_hex_addr(offset_str).ok_or_else(|| anyhow::anyhow!("invalid rom.fs.script.offset: {offset_str}"))? as usize;

    let size_kb = script_part.size.ok_or_else(|| anyhow::anyhow!("info.json missing rom.fs.script.size"))? as usize;
    anyhow::ensure!(size_kb > 0, "invalid rom.fs.script.size: {size_kb}");
    let size = size_kb * 1024;

    let rom = fs::read(&up.rom_path).with_context(|| format!("read {}", up.rom_path.display()))?;
    anyhow::ensure!(offset <= rom.len(), "rom.fs.script.offset out of range: {offset}");
    anyhow::ensure!(offset + size <= rom.len(), "rom.fs.script range out of ROM bounds: {} > {}", offset + size, rom.len());

    fs::write(out_path, &rom[offset..offset + size]).with_context(|| format!("write extracted script to {}", out_path.display()))?;
    Ok(())
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

fn build_ec7xx_fota(new_soc: &str, old_soc: &str, chip: &str, toolkit_path: &Path, toolkit_dir: &Path, out_path: &Path) -> Result<()> {
    let tmp = tempfile::tempdir().context("tempdir")?;

    let new_up = unpack(new_soc, &tmp.path().join("new"))?;
    let old_up = unpack(old_soc, &tmp.path().join("old"))?;

    let config_arg = format!("config\\{}", chip_fota_config(chip));
    let work_old = toolkit_dir.join("old.binpkg");
    let work_new = toolkit_dir.join("new.binpkg");

    fs::copy(&old_up.rom_path, &work_old).context("copy old.binpkg")?;
    fs::copy(&new_up.rom_path, &work_new).context("copy new.binpkg")?;

    let delta = toolkit_dir.join("delta.par");
    log::info!("FotaToolkit: {:?} -d {} BINPKG delta.par old.binpkg new.binpkg", toolkit_path, config_arg);
    let status = Command::new(toolkit_path)
        .args(["-d", &config_arg, "BINPKG", "delta.par", "old.binpkg", "new.binpkg"])
        .current_dir(toolkit_dir)
        .status()
        .with_context(|| format!("launch {:?}", toolkit_path))?;

    let _ = fs::remove_file(&work_old);
    let _ = fs::remove_file(&work_new);
    if !status.success() {
        bail!("FotaToolkit.exe failed (exit {:?})", status.code());
    }
    anyhow::ensure!(delta.exists(), "delta.par not found after FotaToolkit");

    let dummy = create_dummy(tmp.path())?;

    luatos_soc::ota::assemble_ota_package(0, 0, "0", 0, "0", 0, &dummy, &delta, out_path).context("assemble OTA package")?;

    let _ = fs::remove_file(&delta);
    Ok(())
}

fn build_ec7xx_script_only_fota_from_unpacked(up: &luatos_soc::UnpackedSoc, out_path: &Path) -> Result<()> {
    let info = &up.info;
    let script_addr_str = info
        .download
        .script_addr
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("info.json missing download.script_addr"))?;
    let script_addr = parse_hex_addr(script_addr_str).ok_or_else(|| anyhow::anyhow!("invalid download.script_addr: {script_addr_str}"))? as u32;

    let tmp = tempfile::tempdir().context("tempdir")?;
    let script_bin = up.dir.join(&info.script.file);
    let script_input_path = if script_bin.exists() {
        script_bin
    } else {
        let extracted = tmp.path().join("script_extracted.bin");
        extract_script_from_rom(up, &extracted).with_context(|| format!("script file not found: {} - script-only FOTA requires a script partition", script_bin.display()))?;
        extracted
    };

    let script_zip = tmp.path().join("script.zip");
    luatos_soc::ota::lzma_compress_file(&script_input_path, &script_zip, 0, script_addr, 0x40000, true).context("compress script for OTA")?;

    let dummy = create_dummy(tmp.path())?;
    luatos_soc::ota::assemble_ota_package(0, 0, "0", 0, "0", 0, &script_zip, &dummy, out_path).context("assemble OTA package")?;

    Ok(())
}

fn build_ec7xx_script_only_fota(new_soc: &str, out_path: &Path) -> Result<()> {
    let tmp = tempfile::tempdir().context("tempdir")?;
    let up = unpack(new_soc, &tmp.path().join("soc"))?;
    build_ec7xx_script_only_fota_from_unpacked(&up, out_path)
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
    luatos_soc::ota::assemble_ota_package(magic, 0xFFFFFFFF, "0", 0, "0", 0, &total_zip, &dummy, out_path).context("assemble OTA package")?;

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
    luatos_soc::ota::assemble_ota_package(magic, 0xFFFFFFFF, "0", 0, "0", 0, &s_zip, &dummy, out_path).context("assemble OTA package")?;

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

// ─── Public command handler ─────────────────────────────────────────────────

pub fn cmd_fota_build(
    new_soc: &str,
    old_soc: Option<&str>,
    output: Option<&str>,
    fota_toolkit_path: Option<&str>,
    _soc_tools_path: Option<&str>,
    script_only: bool,
    format: &OutputFormat,
) -> Result<()> {
    anyhow::ensure!(Path::new(new_soc).exists(), "New SOC not found: {new_soc}");
    if let Some(old) = old_soc {
        anyhow::ensure!(Path::new(old).exists(), "Old SOC not found: {old}");
    }

    let info = luatos_soc::read_soc_info(new_soc)?;
    let chip = info.chip.chip_type.as_str();

    match chip {
        // EC7xx / EC618 - differential
        "ec7xx" | "ec618" | "air8000" | "air780epm" | "air780ehm" | "air780ehv" | "air780ehg" | "air780epv" => {
            if script_only {
                let out_path: PathBuf = output.map(PathBuf::from).unwrap_or_else(|| PathBuf::from(format!("{chip}_fota.sota")));
                build_ec7xx_script_only_fota(new_soc, &out_path)?;
                let size = fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
                print_result(format, chip, new_soc, old_soc, &[(&out_path, size)])?;
                return Ok(());
            }
            let old = old_soc.ok_or_else(|| anyhow::anyhow!("Full FOTA for EC7xx/EC618 is not yet supported. Please provide --old <old.soc> for differential FOTA."))?;
            let (toolkit, toolkit_dir) = find_fota_toolkit(chip, fota_toolkit_path)?;
            let out_path: PathBuf = output.map(PathBuf::from).unwrap_or_else(|| PathBuf::from(format!("{chip}_fota.sota")));
            build_ec7xx_fota(new_soc, old, chip, &toolkit, &toolkit_dir, &out_path)?;
            let size = fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
            print_result(format, chip, new_soc, old_soc, &[(&out_path, size)])?;
        }

        // Air1601 / Air1602 / CCM4211 - full or script-only
        "air1601" | "air1602" | "ccm4211" => {
            let out_path: PathBuf = output.map(PathBuf::from).unwrap_or_else(|| PathBuf::from(format!("{chip}_fota.sota")));
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
        "bk72xx" | "air8101" => {
            if old_soc.is_some() {
                log::warn!("--old is ignored for Air8101/BK72XX: only full/script package generation is supported");
            }
            let out_path: PathBuf = output.map(PathBuf::from).unwrap_or_else(|| PathBuf::from(format!("{chip}_fota.bin")));
            build_bk72xx_fota(new_soc, &out_path, script_only)?;
            let size = fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
            print_result(format, chip, new_soc, old_soc, &[(&out_path, size)])?;
        }

        // Air6208 / XT804 - full only
        "air6208" | "xt804" => {
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

        other => bail!(
            "FOTA not supported for chip '{other}'. \
             Supported: EC7xx/EC618/Air8000 (differential), Air1601/Air1602/CCM4211 (full or --script-only), Air8101/BK72XX (new-format full or --script-only), Air6208/XT804 (full)."
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

    use super::cmd_fota_build;
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

    #[test]
    fn ec7xx_script_only_builds_without_old_soc() {
        let tmp = tempdir().unwrap();
        let out = tmp.path().join("air780epm_script.sota");
        let soc = ec7xx_soc_path();

        let result = cmd_fota_build(
            &soc,
            None,
            Some(out.to_str().unwrap()),
            Some("C:\\definitely\\missing\\FotaToolkit.exe"),
            None,
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
            None,
            true,
            &OutputFormat::Text,
        );
        let err = result.unwrap_err().to_string();
        assert!(err.contains("script file not found"), "expected missing script error, got: {err}");
    }
}
