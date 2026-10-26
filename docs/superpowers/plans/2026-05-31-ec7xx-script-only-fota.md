# EC7xx `--script-only` FOTA Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add EC7xx / Air780EPM / Air780EHM / Air8000 `--script-only` FOTA generation so the CLI can build a script-only `.sota` without requiring `--old` or Python.

**Architecture:** Keep the current `fota build` entry point and split EC7xx into a dedicated script-only path that reuses `luatos-soc::ota::lzma_compress_file()` and `assemble_ota_package()`. The new path should unpack the `.soc`, validate `script.bin` and `download.script_addr`, compress only the script partition, and package it with the existing OTA header shape. Existing EC7xx differential FOTA remains unchanged.

**Tech Stack:** Rust, `clap`, `anyhow`, `tempfile`, `luatos-soc` OTA helpers, `cargo test`, `cargo clippy`

---

## File map

- `crates/luatos-cli/src/cmd_fota.rs`
  - Add the EC7xx script-only builder and unit tests.
  - Keep EC7xx differential builder intact.
- `crates/luatos-cli/src/main.rs`
  - Update the `--script-only` help text so it no longer says CCM4211 only.
- `README.md`
  - Document that EC7xx now supports `--script-only`.
- `docs/ota-guide.md`
  - Document EC7xx script-only usage and the unchanged `.sota` output shape.
- `docs/models/air8000-ec7xx.md`
  - Update the model-specific recommendation block with the new command.

### Task 1: Add EC7xx script-only tests first

**Files:**
- Modify: `crates/luatos-cli/src/cmd_fota.rs`

- [ ] **Step 1: Write the failing tests**

Add a `#[cfg(test)]` module near the bottom of `cmd_fota.rs` with two tests:

```rust
#[test]
fn ec7xx_script_only_builds_without_old_soc() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("air780epm_script.sota");
    let soc = "refs/soc_files/LuatOS-SoC_V2029_Air780EPM_1.soc";

    cmd_fota_build(
        soc,
        None,
        Some(out.to_str().unwrap()),
        Some("C:\\definitely\\missing\\FotaToolkit.exe"),
        None,
        true,
        &OutputFormat::Text,
    )
    .unwrap();

    let data = std::fs::read(&out).unwrap();
    assert!(u32::from_le_bytes(data[52..56].try_into().unwrap()) > 0);
    assert_eq!(u32::from_le_bytes(data[56..60].try_into().unwrap()), 0);
}

#[test]
fn ec7xx_script_only_rejects_missing_script_bin() {
    let tmp = tempfile::tempdir().unwrap();
    let unpack_dir = tmp.path().join("soc");
    let unpacked = unpack("refs/soc_files/LuatOS-SoC_V2029_Air780EPM_1.soc", &unpack_dir).unwrap();
    let script_path = unpacked.dir.join(&unpacked.info.script.file);
    std::fs::remove_file(&script_path).unwrap();
    let out_path = tmp.path().join("out.sota");

    let err = build_ec7xx_script_only_fota_from_unpacked(&unpacked, &out_path).unwrap_err();
    assert!(err.to_string().contains("script file not found"));
}
```

The second test should fail until the builder is split so the unpacked struct can be reused directly.

- [ ] **Step 2: Run the focused tests and confirm they fail**

Run:

```powershell
cargo test -p luatos-cli ec7xx_script_only_builds_without_old_soc -- --exact
cargo test -p luatos-cli ec7xx_script_only_rejects_missing_script_bin -- --exact
```

Expected:

- `ec7xx_script_only_builds_without_old_soc` fails because EC7xx still rejects `--script-only`
- `ec7xx_script_only_rejects_missing_script_bin` fails because `build_ec7xx_script_only_fota_from_unpacked` does not exist yet

- [ ] **Step 3: Commit the red tests**

```powershell
git add crates/luatos-cli/src/cmd_fota.rs
git commit -m "test: cover ec7xx script-only fota" -m "Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

### Task 2: Implement EC7xx script-only generation

**Files:**
- Modify: `crates/luatos-cli/src/cmd_fota.rs:111-147, 435-528`
- Modify: `crates/luatos-cli/src/main.rs:658-660`

- [ ] **Step 1: Add the minimal builder split**

Introduce a private helper with this shape:

```rust
fn build_ec7xx_script_only_fota_from_unpacked(
    up: &luatos_soc::UnpackedSoc,
    out_path: &Path,
) -> Result<()> {
    let info = &up.info;
    let script_addr = info
        .download
        .script_addr
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("info.json missing download.script_addr"))?;
    let script_addr = parse_hex_addr(script_addr)
        .ok_or_else(|| anyhow::anyhow!("invalid download.script_addr: {script_addr}"))? as u32;

    let script_bin = up.dir.join(&info.script.file);
    anyhow::ensure!(
        script_bin.exists(),
        "script file not found: {} - script-only FOTA requires a script partition",
        script_bin.display()
    );

    let tmp = tempfile::tempdir().context("tempdir")?;
    let script_zip = tmp.path().join("script.zip");
    let magic = 0;
    luatos_soc::ota::lzma_compress_file(&script_bin, &script_zip, magic, script_addr, 0x40000, true)?;

    let dummy = create_dummy(tmp.path())?;
    luatos_soc::ota::assemble_ota_package(magic, 0, "0", 0, "0", 0, &script_zip, &dummy, out_path)?;
    Ok(())
}
```

Then add a thin public wrapper:

```rust
fn build_ec7xx_script_only_fota(new_soc: &str, out_path: &Path) -> Result<()> {
    let tmp = tempfile::tempdir().context("tempdir")?;
    let up = unpack(new_soc, &tmp.path().join("soc"))?;
    build_ec7xx_script_only_fota_from_unpacked(&up, out_path)
}
```

In `cmd_fota_build`, change the EC7xx branch to:

```rust
if script_only {
    let out_path: PathBuf = output.map(PathBuf::from).unwrap_or_else(|| PathBuf::from(format!("{chip}_fota.sota")));
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
```

Update the help text in `main.rs` to:

```rust
/// Build a script-only FOTA package (EC7xx/Air8000, Air1601/Air1602/CCM4211, BK72XX)
```

- [ ] **Step 2: Run the focused tests until they pass**

Run:

```powershell
cargo test -p luatos-cli ec7xx_script_only_builds_without_old_soc -- --exact
cargo test -p luatos-cli ec7xx_script_only_rejects_missing_script_bin -- --exact
```

Expected:

- Both tests pass
- EC7xx script-only output is a `.sota` file with nonzero common data and zero SDK data

- [ ] **Step 3: Commit the implementation**

```powershell
git add crates/luatos-cli/src/cmd_fota.rs crates/luatos-cli/src/main.rs
git commit -m "feat: add ec7xx script-only fota" -m "Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

### Task 3: Update user-facing docs

**Files:**
- Modify: `README.md`
- Modify: `docs/ota-guide.md`
- Modify: `docs/models/air8000-ec7xx.md`

- [ ] **Step 1: Update the command docs**

Make these exact messaging changes:

- `README.md`
  - Feature list: change FOTA bullet to mention `EC7xx 脚本-only`
  - Quick start: add one `--script-only` EC7xx example
- `docs/ota-guide.md`
  - Parameter table: change the `--script-only` description from “CCM4211 only” to “EC7xx/Air8000 and CCM4211/BK72XX”
  - Chip-family table: add EC7xx script-only support as part of the EC7xx row, not a new family
  - FAQ: add a short note that EC7xx script-only still emits `.sota`
- `docs/models/air8000-ec7xx.md`
  - Recommended commands: add a `--script-only` example
  - Keep the baud-rate note unchanged

Concrete text to use in `README.md` quick start:

```bash
# Air780EPM / Air780EHM / Air8000 仅脚本 FOTA
luatos-cli fota build --new firmware.soc --script-only
```

- [ ] **Step 2: Run a docs sanity check**

Run:

```powershell
cargo test -p luatos-cli ec7xx_script_only_builds_without_old_soc -- --exact
```

Expected: still passes, proving the docs changes did not affect the implementation.

- [ ] **Step 3: Commit the docs**

```powershell
git add README.md docs/ota-guide.md docs/models/air8000-ec7xx.md
git commit -m "docs: document ec7xx script-only fota" -m "Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

### Task 4: Final verification and branch cleanup

**Files:**
- No code changes expected

- [ ] **Step 1: Run the repository checks**

Run:

```powershell
cargo fmt --check
cargo clippy -- -D warnings
cargo test --workspace
```

Expected:

- `cargo fmt --check` passes
- `cargo clippy -- -D warnings` passes
- `cargo test --workspace` passes

- [ ] **Step 2: Confirm the worktree is clean except for the new commits**

Run:

```powershell
git status --short
```

Expected: no unstaged changes.

- [ ] **Step 3: Hand off the result**

Report the final outputs:

- `fota build --new <ec7xx.soc> --script-only` works
- EC7xx differential FOTA still works
- Docs mention the new command
