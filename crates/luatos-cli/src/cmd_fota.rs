// fota build — FOTA (firmware-over-the-air) package generation.
//
// Supported chip families:
//   EC7xx / EC618 / Air8000  — differential (--new + --old) via FotaToolkit.exe + soc_tools.exe
//   Air1601 / CCM4211        — full only via soc_tools.exe zip_file + make_ota_file
//   Air6208 / XT804          — full only via air101_flash.exe (bundled in the .soc)
//
// External tools required:
//   FotaToolkit.exe — delta-diff engine, must run from its own dtools directory (EC7xx/EC618)
//   soc_tools.exe   — assembles .sota containers (EC7xx/EC618 and Air1601/CCM4211)
//   air101_flash.exe — W800 OTA image builder, extracted from the .soc itself (Air6208)
//
// Tool discovery order:
//   1. Explicit CLI flag
//   2. Siblings of executable or dtools/ subdir relative to it
//   3. refs/origin_tools/ layout (development)

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::OutputFormat;

// ─── Tool discovery ──────────────────────────────────────────────────────────

const SEARCH_ROOTS: &[&str] = &[
    ".",
    "tools",
    "refs/origin_tools/tools",
    "../refs/origin_tools/tools",
    "../../refs/origin_tools/tools",
];

const DTOOLS_SEARCH_ROOTS: &[&str] = &[
    "dtools",
    "refs/origin_tools/dtools",
    "../refs/origin_tools/dtools",
    "../../refs/origin_tools/dtools",
];

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|pp| pp.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn find_soc_tools(explicit: Option<&str>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        let path = PathBuf::from(p);
        anyhow::ensure!(path.exists(), "soc_tools not found at: {p}");
        return Ok(path);
    }
    let base = exe_dir();
    for root in SEARCH_ROOTS {
        let candidate = base.join(root).join("soc_tools.exe");
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    bail!("soc_tools.exe not found. Provide --soc-tools <path> or place it next to luatos-cli.")
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
    bail!("FotaToolkit.exe not found for chip '{chip}' (variant: {variant}). \
           Provide --fota-toolkit <path> or place dtools/ next to luatos-cli.")
}

// ─── Shared helpers ──────────────────────────────────────────────────────────

/// Extract a .soc archive, return the unpacked result.
fn unpack(soc_path: &str, out_dir: &Path) -> Result<luatos_soc::UnpackedSoc> {
    fs::create_dir_all(out_dir)
        .with_context(|| format!("create dir {}", out_dir.display()))?;
    luatos_soc::unpack_soc(soc_path, out_dir)
}

/// Create an empty dummy.bin placeholder.
fn create_dummy(dir: &Path) -> Result<PathBuf> {
    let path = dir.join("dummy.bin");
    fs::write(&path, &[]).context("create dummy.bin")?;
    Ok(path)
}

/// Parse a hex address string (with or without "0x" prefix).
fn parse_hex_addr(s: &str) -> Option<u64> {
    let s = s.trim();
    let hex = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
    u64::from_str_radix(hex, 16).ok()
}

/// Run a command, surface stderr on failure, return an error if exit code ≠ 0.
fn run_cmd(mut cmd: Command) -> Result<()> {
    let status = cmd.status().with_context(|| {
        format!("failed to launch {:?}", cmd.get_program())
    })?;
    if !status.success() {
        bail!("{:?} exited with code {:?}", cmd.get_program(), status.code());
    }
    Ok(())
}

// ─── EC7xx / EC618 — differential FOTA ───────────────────────────────────────

fn build_ec7xx_fota(
    new_soc: &str,
    old_soc: &str,
    chip: &str,
    toolkit_path: &Path,
    toolkit_dir: &Path,
    soc_tools: &Path,
    out_path: &Path,
) -> Result<()> {
    let tmp = tempfile::tempdir().context("tempdir")?;

    let new_up = unpack(new_soc, &tmp.path().join("new"))?;
    let old_up = unpack(old_soc, &tmp.path().join("old"))?;

    // Run FotaToolkit.exe in its own directory
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

    run_cmd({
        let mut c = Command::new(soc_tools);
        c.args(["make_ota_file", "0", "0", "0", "0", "0",
            &dummy.to_string_lossy(), &delta.to_string_lossy(), &out_path.to_string_lossy()]);
        c
    })?;

    let _ = fs::remove_file(&delta);
    Ok(())
}

// ─── Air1601 / CCM4211 — full FOTA ───────────────────────────────────────────

fn build_ccm4211_fota(
    new_soc: &str,
    soc_tools: &Path,
    out_path: &Path,
) -> Result<()> {
    let tmp = tempfile::tempdir().context("tempdir")?;
    let up = unpack(new_soc, &tmp.path().join("soc"))?;
    let info = &up.info;

    // Extract magic and addresses from info.json
    let magic = info
        .fota
        .as_ref()
        .and_then(|f| f.get("magic_num"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("info.json missing fota.magic_num"))?;

    let app_addr = info.download.app_addr.as_deref().unwrap_or("20000000");
    let script_addr = info.download.script_addr.as_deref();

    let rom_bin = &up.rom_path;
    let script_bin = up.dir.join(&info.script.file);

    // Compress ROM binary → ap.zip
    let ap_zip = tmp.path().join("ap.zip");
    run_cmd({
        let mut c = Command::new(soc_tools);
        c.args(["zip_file", magic, app_addr,
            &rom_bin.to_string_lossy(), &ap_zip.to_string_lossy(), "40000", "1"]);
        c
    })?;

    // Optionally compress script → s.zip, then concatenate
    let total_zip = tmp.path().join("total.zip");
    if script_bin.exists() {
        if let Some(saddr) = script_addr {
            let s_zip = tmp.path().join("s.zip");
            run_cmd({
                let mut c = Command::new(soc_tools);
                c.args(["zip_file", magic, saddr,
                    &script_bin.to_string_lossy(), &s_zip.to_string_lossy(), "40000", "1"]);
                c
            })?;
            let mut data = fs::read(&ap_zip).context("read ap.zip")?;
            data.extend_from_slice(&fs::read(&s_zip).context("read s.zip")?);
            fs::write(&total_zip, data).context("write total.zip")?;
        } else {
            fs::copy(&ap_zip, &total_zip).context("copy ap.zip → total.zip")?;
        }
    } else {
        fs::copy(&ap_zip, &total_zip).context("copy ap.zip → total.zip")?;
    }

    let dummy = create_dummy(tmp.path())?;

    run_cmd({
        let mut c = Command::new(soc_tools);
        // args: make_ota_file <magic> <old_cp_sentinel=0xFFFFFFFF> 0 0 0 <version=0> <ap.zip> <dummy> <out>
        c.args(["make_ota_file", magic, "4294967295", "0", "0", "0", "0",
            &total_zip.to_string_lossy(), &dummy.to_string_lossy(),
            &out_path.to_string_lossy()]);
        c
    })?;

    Ok(())
}

// ─── Air6208 / XT804 — full FOTA ─────────────────────────────────────────────

fn build_air6208_fota(
    new_soc: &str,
    out_base: &Path, // base path (no extension); produces <base>.fota and <base>.sota
) -> Result<Air6208FotaResult> {
    let tmp = tempfile::tempdir().context("tempdir")?;
    let up = unpack(new_soc, &tmp.path().join("soc"))?;
    let info = &up.info;

    let flash_exe = up
        .flash_exe
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("air101_flash.exe not found inside the Air6208 .soc archive"))?;

    // ── Resolve addresses ──────────────────────────────────────────────────────
    let app_addr = info
        .download
        .app_addr
        .as_deref()
        .or(info.download.core_addr.as_deref())
        .unwrap_or("8010000");

    let run_addr_computed: String = parse_hex_addr(app_addr)
        .map(|a| format!("{:x}", a + 0x400))
        .unwrap_or_else(|| "8010400".to_string());
    let run_addr = info.download.run_addr.as_deref().unwrap_or(&run_addr_computed);

    let fota_addr = info
        .rom
        .fs
        .as_ref()
        .and_then(|fs| fs.fota.as_ref())
        .and_then(|f| f.offset.as_deref())
        .or(info.download.ota_addr.as_deref())
        .unwrap_or("8280000");

    let script_offset = info
        .rom
        .fs
        .as_ref()
        .and_then(|fs| fs.script.as_ref())
        .and_then(|s| s.offset.as_deref())
        .unwrap_or("0");

    let compress_type: u8 = info
        .fota
        .as_ref()
        .and_then(|f| f.get("compress_type"))
        .and_then(|v| v.as_u64())
        .map(|v| v as u8)
        .unwrap_or(1);

    let rom_bin = &up.rom_path;
    let script_bin = up.dir.join(&info.script.file);

    // ── Step 1: generate intermediate binary (type=2, no compression) ─────────
    let mid_base = tmp.path().join("fota_mid");
    run_cmd({
        let mut c = Command::new(flash_exe);
        c.args([
            "-b", &rom_bin.to_string_lossy(),
            "-it", "2", "-fc", "0",
            "-ih", "20008000",
            "-ra", script_offset,
            "-ua", "0",
            "-nh", "0", "-un", "0",
            "-o", &mid_base.to_string_lossy(),
        ]);
        c
    })?;

    // ── Step 2: strip secboot header from the intermediate image ──────────────
    let mid_img = mid_base.with_extension("img");
    let bin_data = fs::read(&mid_img)
        .with_context(|| format!("read intermediate image {}", mid_img.display()))?;

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

    // ── Step 3: package firmware FOTA (type=1, compressed) ────────────────────
    let fw_base = tmp.path().join("fw_fota");
    let fc_str = compress_type.to_string();
    run_cmd({
        let mut c = Command::new(flash_exe);
        c.args([
            "-b", &stripped_path.to_string_lossy(),
            "-it", "1", "-fc", &fc_str,
            "-ih", app_addr,
            "-ra", run_addr,
            "-ua", fota_addr,
            "-nh", "0", "-un", "0",
            "-o", &fw_base.to_string_lossy(),
        ]);
        c
    })?;

    // Rename {fw_base}_gz.img → <out_base>.fota
    let gz_img = PathBuf::from(format!("{}_gz.img", fw_base.display()));
    let fota_out = out_base.with_extension("fota");
    fs::rename(&gz_img, &fota_out)
        .with_context(|| format!("rename {} → {}", gz_img.display(), fota_out.display()))?;
    let _ = fs::remove_file(PathBuf::from(format!("{}.bin.gz", fw_base.display())));

    // ── Step 4 (optional): script-only FOTA (type=2, compressed) ─────────────
    let mut sota_out: Option<PathBuf> = None;
    if script_bin.exists() {
        let script_addr = info.download.script_addr.as_deref().unwrap_or(fota_addr);
        let s_base = tmp.path().join("script_sota");
        let res = (|| -> Result<()> {
            run_cmd({
                let mut c = Command::new(flash_exe);
                c.args([
                    "-b", &script_bin.to_string_lossy(),
                    "-it", "2", "-fc", &fc_str,
                    "-ih", script_addr,
                    "-ra", script_addr,
                    "-ua", fota_addr,
                    "-nh", "0", "-un", "0",
                    "-o", &s_base.to_string_lossy(),
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
    soc_tools_path: Option<&str>,
    format: &OutputFormat,
) -> Result<()> {
    anyhow::ensure!(Path::new(new_soc).exists(), "New SOC not found: {new_soc}");
    if let Some(old) = old_soc {
        anyhow::ensure!(Path::new(old).exists(), "Old SOC not found: {old}");
    }

    let info = luatos_soc::read_soc_info(new_soc)?;
    let chip = info.chip.chip_type.as_str();

    match chip {
        // ── EC7xx / EC618 — differential ─────────────────────────────────────
        "ec7xx" | "ec618" | "air8000"
        | "air780epm" | "air780ehm" | "air780ehv" | "air780ehg" | "air780epv" => {
            let old = old_soc.ok_or_else(|| {
                anyhow::anyhow!("Full FOTA for EC7xx/EC618 is not yet supported. Please provide --old <old.soc> for differential FOTA.")
            })?;

            let (toolkit, toolkit_dir) = find_fota_toolkit(chip, fota_toolkit_path)?;
            let soc_tools = find_soc_tools(soc_tools_path)?;

            let out_path: PathBuf = output
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(format!("{chip}_fota.sota")));

            build_ec7xx_fota(new_soc, old, chip, &toolkit, &toolkit_dir, &soc_tools, &out_path)?;

            let size = fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
            print_result(format, chip, new_soc, old_soc, &[(&out_path, size)]);
        }

        // ── Air1601 / CCM4211 — full only ─────────────────────────────────────
        "air1601" | "ccm4211" => {
            if old_soc.is_some() {
                log::warn!("--old is ignored for Air1601/CCM4211: only full FOTA is supported");
            }
            let soc_tools = find_soc_tools(soc_tools_path)?;
            let out_path: PathBuf = output
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(format!("{chip}_fota.sota")));

            build_ccm4211_fota(new_soc, &soc_tools, &out_path)?;

            let size = fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
            print_result(format, chip, new_soc, old_soc, &[(&out_path, size)]);
        }

        // ── Air6208 / XT804 — full only ────────────────────────────────────────
        "air6208" | "xt804" => {
            if old_soc.is_some() {
                log::warn!("--old is ignored for Air6208/XT804: only full FOTA is supported");
            }
            let out_base: PathBuf = output
                .map(|p| {
                    // Strip extension if user provides "foo.fota" → use "foo" as base
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

            let sota_size = result
                .sota
                .as_ref()
                .and_then(|p| fs::metadata(p).ok())
                .map(|m| m.len())
                .unwrap_or(0);
            let sota_tmp;
            if let Some(ref sota) = result.sota {
                sota_tmp = (sota.as_path(), sota_size);
                outputs.push(sota_tmp);
            }

            print_result(format, chip, new_soc, old_soc, &outputs);
        }

        other => bail!("FOTA not supported for chip '{other}'. \
            Supported: EC7xx/EC618/Air8000 (differential), Air1601/CCM4211 (full), Air6208/XT804 (full)."),
    }

    Ok(())
}

fn print_result(
    format: &OutputFormat,
    chip: &str,
    new_soc: &str,
    old_soc: Option<&str>,
    outputs: &[(&Path, u64)],
) {
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
        OutputFormat::Json => {
            let out_list: Vec<serde_json::Value> = outputs
                .iter()
                .map(|(p, s)| serde_json::json!({ "path": p.display().to_string(), "size_bytes": s }))
                .collect();
            let json = serde_json::json!({
                "status": "ok",
                "command": "fota.build",
                "data": {
                    "chip": chip,
                    "new_soc": new_soc,
                    "old_soc": old_soc,
                    "outputs": out_list,
                },
            });
            println!("{}", serde_json::to_string_pretty(&json).unwrap_or_default());
        }
    }
}
