# Copilot Instructions — luatos-cli

## Build & Test

```bash
cargo build --release                # Release build
cargo test --workspace               # All unit tests (~52)
cargo test -p luatos-luadb           # Single crate
cargo test pack_luadb_               # Single test by name pattern
cargo clippy -- -D warnings          # Lint (CI enforces this)
cargo fmt -- --check                 # Format check
```

Linux requires `libudev-dev` and `pkg-config` for serial port support.

## Architecture

Cargo workspace with 7 crates — the CLI binary orchestrates 6 library crates:

- **luatos-cli** — Binary entry point. Clap-based CLI with nested subcommands (`serial`, `soc`, `flash`, `log`, `project`, `build`). Owns the `--format json|text` global flag and progress display.
- **luatos-flash** — Flash protocols for BK7258, XT804 (Air6208), and CCM4211 (Air1601). Each chip module exposes a top-level function that accepts a `ProgressCallback`.
- **luatos-soc** — Parse/unpack/pack SOC firmware archives (ZIP + 7z via `sevenz-rust2`).
- **luatos-luadb** — LuaDB filesystem image packer, embedded Lua 5.3 compiler (C, built via `build.rs`), BK CRC16 adapter. The `build.rs` compiles C code into `luac32_helper`, `luac64_helper`, and `mklfs_helper` executables that get embedded.
- **luatos-serial** — Serial port enumeration and log streaming (text + binary).
- **luatos-project** — Project scaffolding and `luatos-project.toml` config loader.
- **luatos-log** — Log parsing framework built around the `LogParser` trait. Includes `LuatosParser`, `BootLogParser`, `SocLogDecoder`. `LogDispatcher` tries parsers in registration order.

## Key Conventions

### Error handling
All crates use `anyhow::Result` exclusively — no custom error enums. Use `anyhow::bail!()`, `.context()`, and `ensure!()` for errors.

### JSON output pattern
The global `--format` flag produces a consistent JSON envelope:
```json
{ "status": "ok", "command": "soc.info", "data": { ... } }
```
When adding commands, branch on `OutputFormat::Text` vs `OutputFormat::Json` and follow this structure.

### Progress callback
Flash operations report progress via `ProgressCallback = Box<dyn Fn(&FlashProgress) + Send>`. The CLI creates one via `make_progress_callback(format)` that formats for text or JSON.

### Adding a new CLI command
1. Add a variant to the relevant `*Commands` enum in `main.rs` with `#[derive(Subcommand)]`.
2. Add `#[arg(...)]` fields for options.
3. Handle the variant in the existing `match` block, accepting `&OutputFormat` for dual-format output.

### Adding a new chip flash protocol
1. Create a new module under `luatos-flash/src/` exposing a public flash function that takes serial port config, SOC info, and a `ProgressCallback`.
2. Wire it in through `luatos-cli` (add subcommand variant + match arm).
3. 更新 `docs/` 下的刷机协议文档（参考现有的 `air8101-flash-protocol.md` 等）。
4. 更新 `README.md` 中的"支持的模组"表格。
5. 需结合实际硬件进行刷机、日志、闭环测试验证，CI 中无硬件测试。

### Adding a new log parser
Implement the `LogParser` trait (`name()` + `parse_line()`) and register with `LogDispatcher`.

### Tests
All tests are inline (`#[cfg(test)]` modules). Use `tempfile::TempDir` for filesystem tests. No hardware-dependent tests in CI.

### Language
Code comments, commit messages, and documentation are in **Chinese (中文)**. The README, CHANGELOG, and protocol docs are all Chinese.
