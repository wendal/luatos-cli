// fota build - FOTA (firmware-over-the-air) package generation.
//
// Supported chip families:
//   EC7xx / EC618 / Air8000  - differential (--new + --old) via FotaToolkit + Rust OTA assembler
//                              script-only (--script-only) via Rust LZMA compress -> update.bin
//   Air1601 / Air1602 / CCM4211 - full via Rust OTA (LZMA compress + assemble), --script-only for script-only update
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

const DTOOLS_SEARCH_ROOTS: &[&str] = &[
    "dtools",
    "FotaToolkit_V3.6.4.0",
    "refs/origin_tools/dtools",
    "../refs/origin_tools/dtools",
    "../../refs/origin_tools/dtools",
    "../FotaToolkit_V3.6.4.0",
    "../../FotaToolkit_V3.6.4.0",
];

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

fn build_ec7xx_fota(new_soc: &str, old_soc: &str, chip: &str, toolkit_path: &Path, toolkit_dir: &Path, out_path: &Path) -> Result<()> {
    let tmp = tempfile::tempdir().context("tempdir")?;
    let work_dir = tmp.path();

    let new_up = unpack(new_soc, &work_dir.join("new"))?;
    let old_up = unpack(old_soc, &work_dir.join("old"))?;

    // Copy binpkg to isolated temp directory (concurrent-safe)
    fs::copy(&old_up.rom_path, work_dir.join("old.binpkg")).context("copy old.binpkg")?;
    fs::copy(&new_up.rom_path, work_dir.join("new.binpkg")).context("copy new.binpkg")?;

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
    let config_arg_str = config_arg.to_string_lossy().to_string();

    log::info!("FotaToolkit: {:?} -d {} BINPKG delta.par old.binpkg new.binpkg", toolkit_path, config_arg_str);
    let status = Command::new(toolkit_path)
        .args(["-d", &config_arg_str, "BINPKG", "delta.par", "old.binpkg", "new.binpkg"])
        .current_dir(work_dir)
        .status()
        .with_context(|| format!("launch {:?}", toolkit_path))?;

    if !status.success() {
        bail!("FotaToolkit failed (exit {:?})", status.code());
    }
    let delta = work_dir.join("delta.par");
    anyhow::ensure!(delta.exists(), "delta.par not found after FotaToolkit");

    let dummy = create_dummy(work_dir)?;

    luatos_soc::ota::assemble_ota_package(0, 0, "0", 0, "0", 0, &dummy, &delta, out_path).context("assemble OTA package")?;

    Ok(())
}

/// Build script-only FOTA for EC7xx/EC618 - compresses only the script partition
/// into update.bin (LZMA + SectorMd5Header), skipping ROM and FotaToolkit.
fn build_ec7xx_script_only_fota(new_soc: &str, out_path: &Path) -> Result<()> {
    let tmp = tempfile::tempdir().context("tempdir")?;
    let up = unpack(new_soc, &tmp.path().join("soc"))?;
    let info = &up.info;

    let script_addr = info.script_addr();
    let magic = info.fota.as_ref()
        .and_then(|f| f.get("magic_num"))
        .and_then(|v| v.as_str())
        .and_then(|s| parse_hex_addr(s))
        .unwrap_or(0) as u32;

    let script_bin = up.dir.join(&info.script.file);
    anyhow::ensure!(script_bin.exists(), "script file not found: {}", script_bin.display());

    luatos_soc::ota::lzma_compress_file(
        &script_bin, out_path,
        magic, script_addr, 0x40000, true,
    ).context("compress script for OTA")?;

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
        // EC7xx / EC618 - differential or script-only
        "ec7xx" | "ec618" | "air8000" | "air780epm" | "air780ehm" | "air780ehv" | "air780ehg" | "air780epv" => {
            if script_only {
                let out_path: PathBuf = output.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("update.bin"));
                build_ec7xx_script_only_fota(new_soc, &out_path)?;
                let size = fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
                print_result(format, chip, new_soc, old_soc, &[(&out_path, size)])?;
            } else {
                let old = old_soc.ok_or_else(|| anyhow::anyhow!("Full FOTA for EC7xx/EC618 is not yet supported. Please provide --old <old.soc> for differential FOTA."))?;
                let (toolkit, toolkit_dir) = find_fota_toolkit(chip, fota_toolkit_path)?;
                let out_path: PathBuf = output.map(PathBuf::from).unwrap_or_else(|| PathBuf::from(format!("{chip}_fota.sota")));
                build_ec7xx_fota(new_soc, old, chip, &toolkit, &toolkit_dir, &out_path)?;
                let size = fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
                print_result(format, chip, new_soc, old_soc, &[(&out_path, size)])?;
            }
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
             Supported: EC7xx/EC618/Air8000 (differential), Air1601/Air1602/CCM4211 (full or --script-only), Air6208/XT804 (full)."
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
