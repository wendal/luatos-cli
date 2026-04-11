// luatos-cli — LuatOS command-line tool.
//
// Usage:
//   luatos-cli serial list              # List serial ports
//   luatos-cli soc info <path>          # Show SOC file info
//   luatos-cli soc unpack <path> -o dir # Extract SOC file
//   luatos-cli flash run --soc <path> --port COM6
//   luatos-cli flash test --soc <path> --port COM6

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "luatos-cli",
    version,
    about = "LuatOS CLI tool — flash, log, project management"
)]
struct Cli {
    /// Output format
    #[arg(long, default_value = "text", global = true)]
    format: OutputFormat,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Clone, PartialEq, clap::ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

#[derive(Subcommand)]
enum Commands {
    /// Serial port tools
    Serial {
        #[command(subcommand)]
        action: SerialCommands,
    },
    /// SOC file operations
    Soc {
        #[command(subcommand)]
        action: SocCommands,
    },
    /// Flash firmware to device
    Flash {
        #[command(subcommand)]
        action: FlashCommands,
    },
    /// Log viewing and recording
    Log {
        #[command(subcommand)]
        action: LogCommands,
    },
    /// Project management
    Project {
        #[command(subcommand)]
        action: ProjectCommands,
    },
    /// Build Lua scripts and filesystem images
    Build {
        #[command(subcommand)]
        action: BuildCommands,
    },
    /// Download firmware resources from LuatOS CDN
    Resource {
        #[command(subcommand)]
        action: ResourceCommands,
    },
    /// Show version information
    Version,
}

#[derive(Subcommand)]
enum SerialCommands {
    /// List available serial ports
    List,
}

#[derive(Subcommand)]
enum SocCommands {
    /// Show SOC file information
    Info {
        /// Path to .soc file
        path: String,
    },
    /// Extract SOC file contents
    Unpack {
        /// Path to .soc file
        path: String,
        /// Output directory
        #[arg(short, long)]
        output: Option<String>,
    },
    /// List files in SOC archive
    Files {
        /// Path to .soc file
        path: String,
    },
    /// Pack a directory into a .soc file
    Pack {
        /// Input directory (must contain info.json)
        #[arg(long)]
        dir: String,
        /// Output .soc file path
        #[arg(short, long)]
        output: String,
    },
}

#[derive(Subcommand)]
enum FlashCommands {
    /// Full firmware flash (ROM + optional script)
    Run {
        /// Path to .soc file
        #[arg(long)]
        soc: String,
        /// Serial port (e.g. COM6)
        #[arg(long)]
        port: String,
        /// Override baud rate
        #[arg(long)]
        baud: Option<u32>,
        /// Script folder (optional, can specify multiple)
        #[arg(long)]
        script: Vec<String>,
    },
    /// Flash script partition only (most common during development)
    Script {
        /// Path to .soc file
        #[arg(long)]
        soc: String,
        /// Serial port (e.g. COM6)
        #[arg(long)]
        port: String,
        /// Script folders containing .lua files (can specify multiple)
        #[arg(long)]
        script: Vec<String>,
    },
    /// Clear filesystem partition (erase to 0xFF)
    ClearFs {
        /// Path to .soc file
        #[arg(long)]
        soc: String,
        /// Serial port (e.g. COM6)
        #[arg(long)]
        port: String,
    },
    /// Build LuaDB and flash to filesystem partition
    FlashFs {
        /// Path to .soc file
        #[arg(long)]
        soc: String,
        /// Serial port (e.g. COM6)
        #[arg(long)]
        port: String,
        /// Script folders containing files to pack (can specify multiple)
        #[arg(long)]
        script: Vec<String>,
    },
    /// Clear FSKV (key-value store) partition
    ClearKv {
        /// Path to .soc file
        #[arg(long)]
        soc: String,
        /// Serial port (e.g. COM6)
        #[arg(long)]
        port: String,
    },
    /// Closed-loop flash test: flash → capture boot log → verify keywords → PASS/FAIL
    Test {
        /// Path to .soc file
        #[arg(long)]
        soc: String,
        /// Serial port (e.g. COM6)
        #[arg(long)]
        port: String,
        /// Override baud rate
        #[arg(long)]
        baud: Option<u32>,
        /// Script folders (optional, can specify multiple)
        #[arg(long)]
        script: Vec<String>,
        /// Timeout in seconds for boot log capture (default: 15)
        #[arg(long, default_value = "15")]
        timeout: u64,
        /// Keywords to search for in boot log (default: "LuatOS@")
        #[arg(long, default_value = "LuatOS@")]
        keyword: Vec<String>,
    },
}

#[derive(Subcommand)]
enum LogCommands {
    /// View serial log in real-time (text mode)
    View {
        /// Serial port (e.g. COM6)
        #[arg(long)]
        port: String,
        /// Baud rate (default: 921600)
        #[arg(long, default_value = "921600")]
        baud: u32,
    },
    /// View serial log in binary SOC mode (Air6208, Air1601 etc.)
    ViewBinary {
        /// Serial port (e.g. COM7)
        #[arg(long)]
        port: String,
        /// Baud rate (default: 2000000)
        #[arg(long, default_value = "2000000")]
        baud: u32,
        /// Send probe command to trigger log output (required for Air1601/CCM4211)
        #[arg(long)]
        probe: bool,
    },
    /// Record serial log to file
    Record {
        /// Serial port (e.g. COM6)
        #[arg(long)]
        port: String,
        /// Baud rate (default: 921600)
        #[arg(long, default_value = "921600")]
        baud: u32,
        /// Output directory for log files
        #[arg(long, default_value = ".")]
        output: String,
        /// Also save JSON-parsed log (JSONL format)
        #[arg(long)]
        json: bool,
    },
    /// Parse a saved log file into structured output
    Parse {
        /// Path to log file
        path: String,
    },
}

#[derive(Subcommand)]
enum ProjectCommands {
    /// Create a new LuatOS project
    New {
        /// Project name
        name: String,
        /// Target chip (bk72xx, air6208, air8101, air8000, air101)
        #[arg(long, default_value = "bk72xx")]
        chip: String,
        /// Directory to create project in (default: ./<name>)
        #[arg(long)]
        dir: Option<String>,
    },
    /// Show project information
    Info {
        /// Project directory (default: current directory)
        #[arg(long, default_value = ".")]
        dir: String,
    },
    /// View or modify project configuration
    Config {
        /// Project directory (default: current directory)
        #[arg(long, default_value = ".")]
        dir: String,
        /// Config key to get/set (e.g. flash.port, build.bitw)
        key: Option<String>,
        /// Value to set (if omitted, shows current value)
        value: Option<String>,
    },
}

#[derive(Subcommand)]
enum BuildCommands {
    /// Compile Lua scripts to bytecode
    Luac {
        /// Source directories containing .lua files (can specify multiple)
        #[arg(long, default_value = "lua/")]
        src: Vec<String>,
        /// Output directory for .luac files
        #[arg(long, default_value = "build/")]
        output: String,
        /// Lua integer bit-width (32 or 64)
        #[arg(long, default_value = "32")]
        bitw: u32,
    },
    /// Build LuaDB script filesystem image
    Filesystem {
        /// Source directories containing .lua / .luac files (can specify multiple)
        #[arg(long, default_value = "lua/")]
        src: Vec<String>,
        /// Output file path for the image
        #[arg(long, default_value = "build/script.bin")]
        output: String,
        /// Compile .lua to .luac before packing
        #[arg(long)]
        luac: bool,
        /// Lua integer bit-width (32 or 64)
        #[arg(long, default_value = "32")]
        bitw: u32,
        /// Add BK CRC16 framing (required for Air8101/bk72xx)
        #[arg(long)]
        bkcrc: bool,
    },
}

#[derive(Subcommand)]
enum ResourceCommands {
    /// List available modules or versions
    List {
        /// Module name to list versions for (e.g. Air8101). If omitted, lists all modules.
        module: Option<String>,
    },
    /// Download firmware resource
    Download {
        /// Module name (e.g. Air8101)
        module: String,
        /// Version filter (e.g. V2012). If omitted, downloads latest version.
        #[arg(long)]
        version: Option<String>,
        /// Output directory (default: resource/)
        #[arg(long, default_value = "resource")]
        output: String,
    },
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Serial { action } => match action {
            SerialCommands::List => cmd_serial_list(&cli.format),
        },
        Commands::Soc { action } => match action {
            SocCommands::Info { path } => cmd_soc_info(&path, &cli.format),
            SocCommands::Unpack { path, output } => {
                cmd_soc_unpack(&path, output.as_deref(), &cli.format)
            }
            SocCommands::Files { path } => cmd_soc_files(&path, &cli.format),
            SocCommands::Pack { dir, output } => cmd_soc_pack(&dir, &output, &cli.format),
        },
        Commands::Flash { action } => match action {
            FlashCommands::Run {
                soc,
                port,
                baud,
                script,
            } => {
                let script_opt = if script.is_empty() {
                    None
                } else {
                    Some(script.as_slice())
                };
                cmd_flash_run(&soc, &port, baud, script_opt, &cli.format)
            }
            FlashCommands::Script { soc, port, script } => {
                cmd_flash_partition("script", &soc, &port, Some(&script), &cli.format)
            }
            FlashCommands::ClearFs { soc, port } => {
                cmd_flash_partition("clear-fs", &soc, &port, None, &cli.format)
            }
            FlashCommands::FlashFs { soc, port, script } => {
                cmd_flash_partition("flash-fs", &soc, &port, Some(&script), &cli.format)
            }
            FlashCommands::ClearKv { soc, port } => {
                cmd_flash_partition("clear-kv", &soc, &port, None, &cli.format)
            }
            FlashCommands::Test {
                soc,
                port,
                baud,
                script,
                timeout,
                keyword,
            } => {
                let script_opt = if script.is_empty() {
                    None
                } else {
                    Some(script.as_slice())
                };
                cmd_flash_test(
                    &soc,
                    &port,
                    baud,
                    script_opt,
                    timeout,
                    &keyword,
                    &cli.format,
                )
            }
        },
        Commands::Log { action } => match action {
            LogCommands::View { port, baud } => cmd_log_view(&port, baud, &cli.format),
            LogCommands::ViewBinary { port, baud, probe } => cmd_log_view_binary(&port, baud, probe, &cli.format),
            LogCommands::Record {
                port,
                baud,
                output,
                json,
            } => cmd_log_record(&port, baud, &output, json, &cli.format),
            LogCommands::Parse { path } => cmd_log_parse(&path, &cli.format),
        },
        Commands::Project { action } => match action {
            ProjectCommands::New { name, chip, dir } => {
                let dir = dir.unwrap_or_else(|| name.clone());
                cmd_project_new(&dir, &name, &chip, &cli.format)
            }
            ProjectCommands::Info { dir } => cmd_project_info(&dir, &cli.format),
            ProjectCommands::Config { dir, key, value } => {
                cmd_project_config(&dir, key.as_deref(), value.as_deref(), &cli.format)
            }
        },
        Commands::Build { action } => match action {
            BuildCommands::Luac { src, output, bitw } => {
                cmd_build_luac(&src, &output, bitw, &cli.format)
            }
            BuildCommands::Filesystem {
                src,
                output,
                luac,
                bitw,
                bkcrc,
            } => cmd_build_filesystem(&src, &output, luac, bitw, bkcrc, &cli.format),
        },
        Commands::Resource { action } => match action {
            ResourceCommands::List { module } => cmd_resource_list(module.as_deref(), &cli.format),
            ResourceCommands::Download {
                module,
                version,
                output,
            } => cmd_resource_download(&module, version.as_deref(), &output, &cli.format),
        },
        Commands::Version => {
            let version = env!("CARGO_PKG_VERSION");
            match cli.format {
                OutputFormat::Text => {
                    println!("luatos-cli v{version}");
                }
                OutputFormat::Json => {
                    let json = serde_json::json!({
                        "version": version,
                        "name": "luatos-cli",
                    });
                    println!("{}", serde_json::to_string_pretty(&json).unwrap());
                }
            }
            Ok(())
        }
    };

    if let Err(e) = result {
        match cli.format {
            OutputFormat::Text => {
                eprintln!("Error: {e:#}");
                std::process::exit(1);
            }
            OutputFormat::Json => {
                let json = serde_json::json!({
                    "status": "error",
                    "error": format!("{e:#}"),
                });
                println!("{}", serde_json::to_string_pretty(&json).unwrap());
                std::process::exit(1);
            }
        }
    }
}

fn cmd_serial_list(format: &OutputFormat) -> anyhow::Result<()> {
    let ports = luatos_serial::list_ports();
    match format {
        OutputFormat::Text => {
            if ports.is_empty() {
                println!("No serial ports found.");
            } else {
                println!("{:<10} {:<10} {:<10} PRODUCT", "PORT", "VID", "PID");
                for p in &ports {
                    println!(
                        "{:<10} {:<10} {:<10} {}",
                        p.port_name,
                        p.vid.as_deref().unwrap_or("-"),
                        p.pid.as_deref().unwrap_or("-"),
                        p.product.as_deref().unwrap_or("-"),
                    );
                }
            }
        }
        OutputFormat::Json => {
            let json = serde_json::json!({
                "status": "ok",
                "command": "serial.list",
                "data": ports,
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
    }
    Ok(())
}

fn cmd_soc_info(path: &str, format: &OutputFormat) -> anyhow::Result<()> {
    let info = luatos_soc::read_soc_info(path)?;
    match format {
        OutputFormat::Text => {
            println!("SOC File: {path}");
            println!("  Chip:       {}", info.chip.chip_type);
            println!("  ROM:        {}", info.rom.file);
            if let Some(ref bsp) = info.rom.version_bsp {
                println!("  BSP:        {bsp}");
            }
            println!("  Flash BR:   {}", info.flash_baud_rate());
            println!("  Log BR:     {}", info.log_baud_rate());
            if let Some(ref addr) = info.download.bl_addr {
                println!("  BL Addr:    {addr}");
            }
            if let Some(ref addr) = info.download.script_addr {
                println!("  Script Addr:{addr}");
            }
            println!("  BK CRC:     {}", info.use_bkcrc());
        }
        OutputFormat::Json => {
            let json = serde_json::json!({
                "status": "ok",
                "command": "soc.info",
                "data": info,
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
    }
    Ok(())
}

fn cmd_soc_unpack(path: &str, output: Option<&str>, format: &OutputFormat) -> anyhow::Result<()> {
    let out_dir = output.unwrap_or(".");
    let out_path = std::path::Path::new(out_dir);
    std::fs::create_dir_all(out_path)?;
    let unpacked = luatos_soc::unpack_soc(path, out_path)?;
    match format {
        OutputFormat::Text => {
            println!("Extracted to: {}", out_path.display());
            println!("  Chip:  {}", unpacked.info.chip.chip_type);
            println!("  ROM:   {}", unpacked.rom_path.display());
            if let Some(ref exe) = unpacked.flash_exe {
                println!("  Exe:   {}", exe.display());
            }
        }
        OutputFormat::Json => {
            let json = serde_json::json!({
                "status": "ok",
                "command": "soc.unpack",
                "data": {
                    "dir": out_path.display().to_string(),
                    "chip": unpacked.info.chip.chip_type,
                    "rom": unpacked.rom_path.display().to_string(),
                    "flash_exe": unpacked.flash_exe.map(|p| p.display().to_string()),
                },
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
    }
    Ok(())
}

fn cmd_soc_files(path: &str, format: &OutputFormat) -> anyhow::Result<()> {
    let files = luatos_soc::list_soc_files(path)?;
    match format {
        OutputFormat::Text => {
            println!("Files in {path}:");
            for f in &files {
                println!("  {f}");
            }
        }
        OutputFormat::Json => {
            let json = serde_json::json!({
                "status": "ok",
                "command": "soc.files",
                "data": files,
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
    }
    Ok(())
}

fn cmd_flash_run(
    soc: &str,
    port: &str,
    baud: Option<u32>,
    script_folders: Option<&[String]>,
    format: &OutputFormat,
) -> anyhow::Result<()> {
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Set up Ctrl+C handler
    let cancel_clone = cancel.clone();
    ctrlc::set_handler(move || {
        eprintln!("\nCancelling flash...");
        cancel_clone.store(true, std::sync::atomic::Ordering::Relaxed);
    })?;

    let on_progress = make_progress_callback(format);

    // Detect chip type from SOC info.json
    let info = luatos_soc::read_soc_info(soc)?;
    let chip = info.chip.chip_type.as_str();

    match chip {
        "bk72xx" | "air8101" | "air8000" => {
            let folders_refs: Option<Vec<&str>> =
                script_folders.map(|dirs| dirs.iter().map(|s| s.as_str()).collect());
            let lines = luatos_flash::bk7258::flash_bk7258(
                soc,
                folders_refs.as_deref(),
                port,
                baud,
                cancel,
                on_progress,
            )?;
            match format {
                OutputFormat::Text => {
                    if !lines.is_empty() {
                        println!("\n--- Boot Log ({} lines) ---", lines.len());
                        for line in &lines {
                            println!("{line}");
                        }
                    }
                }
                OutputFormat::Json => {
                    let json = serde_json::json!({
                        "status": "ok",
                        "command": "flash.run",
                        "data": { "boot_log": lines },
                    });
                    println!("{}", serde_json::to_string_pretty(&json)?);
                }
            }
        }
        "air6208" | "air101" | "air103" | "air601" => {
            luatos_flash::xt804::flash_xt804(soc, port, on_progress, cancel)?;
            match format {
                OutputFormat::Text => {
                    println!("XT804 flash completed successfully.");
                }
                OutputFormat::Json => {
                    let json = serde_json::json!({
                        "status": "ok",
                        "command": "flash.run",
                        "data": { "chip": chip },
                    });
                    println!("{}", serde_json::to_string_pretty(&json)?);
                }
            }
        }
        "air1601" | "ccm4211" => {
            luatos_flash::ccm4211::flash_ccm4211(soc, port, &on_progress, cancel)?;
            match format {
                OutputFormat::Text => {
                    println!("CCM4211 flash completed successfully.");
                }
                OutputFormat::Json => {
                    let json = serde_json::json!({
                        "status": "ok",
                        "command": "flash.run",
                        "data": { "chip": chip },
                    });
                    println!("{}", serde_json::to_string_pretty(&json)?);
                }
            }
        }
        _ => {
            anyhow::bail!("Unsupported chip type: {chip}. Supported: bk72xx, air6208, air101, air1601");
        }
    }

    Ok(())
}

fn make_progress_callback(format: &OutputFormat) -> luatos_flash::ProgressCallback {
    let format_clone = format.clone();
    Box::new(move |p| match format_clone {
        OutputFormat::Text => {
            if p.percent >= 0.0 {
                eprintln!("[{:>6.1}%] {} — {}", p.percent, p.stage, p.message);
            } else {
                eprintln!("          {}", p.message);
            }
        }
        OutputFormat::Json => {
            println!("{}", serde_json::to_string(p).unwrap_or_default());
        }
    })
}

fn cmd_flash_partition(
    op: &str,
    soc: &str,
    port: &str,
    script_folders: Option<&[String]>,
    format: &OutputFormat,
) -> anyhow::Result<()> {
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    let cancel_clone = cancel.clone();
    let _ = ctrlc::set_handler(move || {
        eprintln!("\nCancelling...");
        cancel_clone.store(true, std::sync::atomic::Ordering::Relaxed);
    });

    let on_progress = make_progress_callback(format);

    // Detect chip type
    let info = luatos_soc::read_soc_info(soc)?;
    let chip = info.chip.chip_type.as_str();

    match chip {
        "bk72xx" | "air8101" | "air8000" => match op {
            "script" => {
                let folders = script_folders.expect("script folder required");
                let refs: Vec<&str> = folders.iter().map(|s| s.as_str()).collect();
                luatos_flash::bk7258::flash_script_only(soc, &refs, port, cancel, on_progress)?;
            }
            "clear-fs" => {
                luatos_flash::bk7258::clear_filesystem(soc, port, cancel, on_progress)?;
            }
            "flash-fs" => {
                let folders = script_folders.expect("script folder required");
                let refs: Vec<&str> = folders.iter().map(|s| s.as_str()).collect();
                luatos_flash::bk7258::flash_filesystem(soc, &refs, port, cancel, on_progress)?;
            }
            "clear-kv" => {
                luatos_flash::bk7258::clear_fskv(soc, port, cancel, on_progress)?;
            }
            _ => unreachable!(),
        },
        "air6208" | "air101" | "air103" | "air601" => match op {
            "script" => {
                let folders = script_folders.expect("script folder required");
                let files = collect_script_files(folders)?;
                luatos_flash::xt804::flash_script_only(soc, port, &files, on_progress, cancel)?;
            }
            "clear-fs" => {
                luatos_flash::xt804::clear_filesystem(soc, port, on_progress, cancel)?;
            }
            "flash-fs" => {
                let folders = script_folders.expect("fs folder required");
                let dir_strings: Vec<String> = folders.to_vec();
                luatos_flash::xt804::flash_filesystem(
                    soc,
                    port,
                    &dir_strings,
                    on_progress,
                    cancel,
                )?;
            }
            "clear-kv" => {
                luatos_flash::xt804::clear_kv(soc, port, on_progress, cancel)?;
            }
            _ => unreachable!(),
        },
        "air1601" | "ccm4211" => match op {
            "script" => {
                let folders = script_folders.expect("script folder required");
                let folder_paths: Vec<std::path::PathBuf> =
                    folders.iter().map(std::path::PathBuf::from).collect();
                let path_refs: Vec<&std::path::Path> =
                    folder_paths.iter().map(|p| p.as_path()).collect();
                let script_data = luatos_luadb::build::build_script_image(
                    &path_refs,
                    info.script_use_luac(),
                    info.script_bitw(),
                    info.use_bkcrc(),
                )?;
                luatos_flash::ccm4211::flash_script_ccm4211(
                    soc,
                    port,
                    &script_data,
                    &on_progress,
                    cancel,
                )?;
            }
            "clear-fs" => {
                luatos_flash::ccm4211::clear_filesystem(soc, port, &on_progress, cancel)?;
            }
            "clear-kv" => {
                luatos_flash::ccm4211::clear_fskv(soc, port, &on_progress, cancel)?;
            }
            _ => unreachable!(),
        },
        _ => {
            anyhow::bail!("Unsupported chip type: {chip}");
        }
    }

    match format {
        OutputFormat::Text => {
            println!("Operation '{op}' completed successfully.");
        }
        OutputFormat::Json => {
            let json = serde_json::json!({
                "status": "ok",
                "command": format!("flash.{op}"),
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
    }
    Ok(())
}

/// Collect script files from multiple folders (*.lua, *.luac, *.json, etc.)
fn collect_script_files(folders: &[String]) -> anyhow::Result<Vec<String>> {
    let mut files = Vec::new();
    for folder in folders {
        for entry in std::fs::read_dir(folder)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                files.push(path.to_string_lossy().to_string());
            }
        }
    }
    if files.is_empty() {
        anyhow::bail!("No script files found in {:?}", folders);
    }
    Ok(files)
}

// ─── Flash test command ───────────────────────────────────────────────────────

/// Closed-loop flash test: flash firmware → capture boot log → check keywords → PASS/FAIL.
fn cmd_flash_test(
    soc: &str,
    port: &str,
    baud: Option<u32>,
    script_folders: Option<&[String]>,
    timeout_secs: u64,
    keywords: &[String],
    format: &OutputFormat,
) -> anyhow::Result<()> {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use std::time::{Duration, Instant};

    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_clone = cancel.clone();
    let _ = ctrlc::set_handler(move || {
        eprintln!("\nCancelling flash test...");
        cancel_clone.store(true, Ordering::Relaxed);
    });

    let on_progress = make_progress_callback(format);

    // Step 1: Flash the firmware
    let info = luatos_soc::read_soc_info(soc)?;
    let chip = info.chip.chip_type.clone();
    let log_br = info.log_baud_rate();

    let boot_lines_from_flash: Vec<String> = match chip.as_str() {
        "bk72xx" | "air8101" | "air8000" => {
            let folders_refs: Option<Vec<&str>> =
                script_folders.map(|dirs| dirs.iter().map(|s| s.as_str()).collect());
            luatos_flash::bk7258::flash_bk7258(
                soc,
                folders_refs.as_deref(),
                port,
                baud,
                cancel.clone(),
                on_progress,
            )?
        }
        "air6208" | "air101" | "air103" | "air601" => {
            let on_progress2 = make_progress_callback(format);
            luatos_flash::xt804::flash_xt804(soc, port, on_progress2, cancel.clone())?;
            Vec::new() // XT804 does not return boot lines from flash
        }
        "air1601" | "ccm4211" => {
            let on_progress2 = make_progress_callback(format);
            luatos_flash::ccm4211::flash_ccm4211(soc, port, &on_progress2, cancel.clone())?;
            Vec::new()
        }
        _ => {
            anyhow::bail!("Unsupported chip type for flash test: {chip}");
        }
    };

    if cancel.load(Ordering::Relaxed) {
        anyhow::bail!("Flash test cancelled by user");
    }

    // Step 2: Capture boot log (append to any lines from flash)
    let mut all_lines = boot_lines_from_flash;

    if format == &OutputFormat::Text {
        eprintln!("Capturing boot log for {timeout_secs}s on {port} @ {log_br}...");
    }

    // Determine if this chip uses binary SOC log protocol
    let use_binary_log = matches!(chip.as_str(), "air1601" | "ccm4211");

    // Open serial port and capture lines for the timeout period
    let serial = serialport::new(port, log_br)
        .timeout(Duration::from_millis(500))
        .open();

    if let Ok(mut serial) = serial {
        use std::io::{Read, Write};

        // For Air1601/CCM4211: send probe to trigger log output
        if use_binary_log {
            let probe = luatos_flash::ccm4211::build_log_probe();
            let _ = serial.write_all(&probe);
            let _ = serial.flush();
        }

        let start = Instant::now();
        let timeout = Duration::from_secs(timeout_secs);
        let mut buf = vec![0u8; 4096];

        if use_binary_log {
            // Binary SOC log: decode 0xA5 frames via SocLogDecoder
            let mut decoder = luatos_log::SocLogDecoder::new();
            while start.elapsed() < timeout && !cancel.load(Ordering::Relaxed) {
                match serial.read(&mut buf) {
                    Ok(n) if n > 0 => {
                        let entries = decoder.feed(&buf[..n]);
                        for entry in &entries {
                            let module = entry.module.as_deref().unwrap_or("-");
                            let msg = format!("[{}] {}/{} {}", entry.device_time.as_deref().unwrap_or("?"), entry.level, module, entry.message);
                            all_lines.push(msg);
                        }
                        let found_all = keywords
                            .iter()
                            .all(|kw| all_lines.iter().any(|line| line.contains(kw.as_str())));
                        if found_all {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                    Err(e) => {
                        log::warn!("Serial read error: {e}");
                        break;
                    }
                }
            }
        } else {
            // Text log: parse as newline-delimited text
            let mut line_buf = String::new();
            while start.elapsed() < timeout && !cancel.load(Ordering::Relaxed) {
                match serial.read(&mut buf) {
                    Ok(n) if n > 0 => {
                        let text = String::from_utf8_lossy(&buf[..n]);
                        for ch in text.chars() {
                            if ch == '\n' {
                                let line = line_buf.trim_end_matches('\r').to_string();
                                if !line.is_empty() {
                                    all_lines.push(line);
                                }
                                line_buf.clear();
                            } else {
                                line_buf.push(ch);
                            }
                        }

                        // Early exit if we already found all keywords
                        let found_all = keywords
                            .iter()
                            .all(|kw| all_lines.iter().any(|line| line.contains(kw.as_str())));
                        if found_all {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                    Err(e) => {
                        log::warn!("Serial read error: {e}");
                        break;
                    }
                }
            }
            // Flush remaining line buffer
            if !line_buf.is_empty() {
                all_lines.push(line_buf.trim_end_matches('\r').to_string());
            }
        }
    } else if all_lines.is_empty() {
        // Could not open serial and no lines from flash
        log::warn!("Could not open serial port for boot log capture");
    }

    // Step 3: Evaluate keywords
    let mut keyword_results: Vec<(String, bool)> = Vec::new();
    for kw in keywords {
        let found = all_lines.iter().any(|line| line.contains(kw.as_str()));
        keyword_results.push((kw.clone(), found));
    }

    let all_passed = keyword_results.iter().all(|(_, found)| *found);
    let result_str = if all_passed { "PASS" } else { "FAIL" };

    // Step 4: Output
    match format {
        OutputFormat::Text => {
            println!("\n===== Flash Test Result: {} =====", result_str);
            println!("  Chip:     {}", chip);
            println!("  SOC:      {}", soc);
            println!("  Port:     {}", port);
            println!("  Log lines: {}", all_lines.len());
            for (kw, found) in &keyword_results {
                let icon = if *found { "✓" } else { "✗" };
                println!(
                    "  [{icon}] Keyword \"{kw}\": {}",
                    if *found { "FOUND" } else { "NOT FOUND" }
                );
            }
            if !all_lines.is_empty() {
                println!("\n--- Boot Log ({} lines) ---", all_lines.len());
                for line in &all_lines {
                    println!("{line}");
                }
            }
        }
        OutputFormat::Json => {
            let json = serde_json::json!({
                "status": if all_passed { "ok" } else { "fail" },
                "command": "flash.test",
                "data": {
                    "result": result_str,
                    "chip": chip,
                    "soc": soc,
                    "port": port,
                    "keywords": keyword_results.iter().map(|(kw, found)| {
                        serde_json::json!({ "keyword": kw, "found": found })
                    }).collect::<Vec<_>>(),
                    "boot_log": all_lines,
                    "log_line_count": all_lines.len(),
                },
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
    }

    if !all_passed {
        std::process::exit(1);
    }

    Ok(())
}

// ─── Log commands ─────────────────────────────────────────────────────────────

fn cmd_log_view(port: &str, baud: u32, format: &OutputFormat) -> anyhow::Result<()> {
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_clone = stop.clone();
    let _ = ctrlc::set_handler(move || {
        stop_clone.store(true, std::sync::atomic::Ordering::Relaxed);
    });

    eprintln!("Viewing log on {port} @ {baud} bps (Ctrl+C to stop)");

    let dispatcher = luatos_log::LogDispatcher::default_parsers();
    let format_clone = format.clone();

    luatos_serial::stream_log_lines(
        port,
        baud,
        stop,
        Box::new(move |line| {
            let entry = dispatcher.parse(line);
            match format_clone {
                OutputFormat::Text => {
                    println!("{}", line);
                }
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string(&entry).unwrap_or_default());
                }
            }
        }),
    )?;

    eprintln!("\nLog viewing stopped.");
    Ok(())
}

fn cmd_log_view_binary(port: &str, baud: u32, probe: bool, format: &OutputFormat) -> anyhow::Result<()> {
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_clone = stop.clone();
    let _ = ctrlc::set_handler(move || {
        stop_clone.store(true, std::sync::atomic::Ordering::Relaxed);
    });

    let init_data = if probe {
        eprintln!("Sending SOC probe to trigger log output ...");
        Some(luatos_flash::ccm4211::build_log_probe())
    } else {
        None
    };

    eprintln!("Viewing SOC binary log on {port} @ {baud} bps (Ctrl+C to stop)");

    let decoder = std::sync::Mutex::new(luatos_log::SocLogDecoder::new());
    let format_clone = format.clone();

    luatos_serial::stream_binary(
        port,
        baud,
        stop,
        Box::new(move |data| {
            if let Ok(mut dec) = decoder.lock() {
                let entries = dec.feed(data);
                for entry in &entries {
                    match format_clone {
                        OutputFormat::Text => {
                            let module = entry.module.as_deref().unwrap_or("-");
                            let time = entry.device_time.as_deref().unwrap_or("?");
                            println!("[{}] {}/{} {}", time, entry.level, module, entry.message);
                        }
                        OutputFormat::Json => {
                            println!("{}", serde_json::to_string(&entry).unwrap_or_default());
                        }
                    }
                }
            }
        }),
        init_data.as_deref(),
    )?;

    eprintln!("\nLog viewing stopped.");
    Ok(())
}

fn cmd_log_record(
    port: &str,
    baud: u32,
    output_dir: &str,
    save_json: bool,
    format: &OutputFormat,
) -> anyhow::Result<()> {
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_clone = stop.clone();
    let _ = ctrlc::set_handler(move || {
        stop_clone.store(true, std::sync::atomic::Ordering::Relaxed);
    });

    let out_path = std::path::Path::new(output_dir);
    std::fs::create_dir_all(out_path)?;

    let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let text_path = out_path.join(format!("log_{timestamp}.txt"));
    let json_path = if save_json {
        Some(out_path.join(format!("log_{timestamp}.jsonl")))
    } else {
        None
    };

    let writer = luatos_log::LogWriter::new(Some(&text_path), json_path.as_deref())?;

    eprintln!(
        "Recording log on {port} @ {baud} bps → {}",
        text_path.display()
    );
    if let Some(ref jp) = json_path {
        eprintln!("  JSON log: {}", jp.display());
    }
    eprintln!("Press Ctrl+C to stop.");

    let dispatcher = luatos_log::LogDispatcher::default_parsers();
    let format_clone = format.clone();

    let writer = std::sync::Mutex::new(writer);
    let line_count = std::sync::atomic::AtomicUsize::new(0);

    luatos_serial::stream_log_lines(
        port,
        baud,
        stop,
        Box::new(move |line| {
            let entry = dispatcher.parse(line);

            match format_clone {
                OutputFormat::Text => {
                    println!("{}", line);
                }
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string(&entry).unwrap_or_default());
                }
            }

            if let Ok(mut w) = writer.lock() {
                let _ = w.write(&entry);
                let count = line_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if count.is_multiple_of(50) {
                    let _ = w.flush();
                }
            }
        }),
    )?;

    eprintln!("\nRecording stopped. Log saved to {}", text_path.display());
    Ok(())
}

fn cmd_log_parse(path: &str, format: &OutputFormat) -> anyhow::Result<()> {
    let dispatcher = luatos_log::LogDispatcher::default_parsers();
    let entries = luatos_log::parse_log_file(std::path::Path::new(path), &dispatcher)?;

    match format {
        OutputFormat::Text => {
            println!("Parsed {} log entries from {path}:", entries.len());
            for entry in &entries {
                let module = entry.module.as_deref().unwrap_or("-");
                let time = entry.device_time.as_deref().unwrap_or(&entry.timestamp);
                println!("[{}] {}/{} {}", time, entry.level, module, entry.message);
            }
        }
        OutputFormat::Json => {
            let json = serde_json::json!({
                "status": "ok",
                "command": "log.parse",
                "data": entries,
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
    }
    Ok(())
}

// ─── SOC pack ─────────────────────────────────────────────────────────────────

fn cmd_soc_pack(dir: &str, output: &str, format: &OutputFormat) -> anyhow::Result<()> {
    let dir_path = std::path::Path::new(dir);
    anyhow::ensure!(dir_path.is_dir(), "Not a directory: {dir}");
    anyhow::ensure!(
        dir_path.join("info.json").exists(),
        "info.json not found in {dir}"
    );

    luatos_soc::pack_soc(dir_path, output)?;

    match format {
        OutputFormat::Text => {
            println!("Packed {} → {output}", dir);
        }
        OutputFormat::Json => {
            let json = serde_json::json!({
                "status": "ok",
                "command": "soc.pack",
                "data": { "output": output },
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
    }
    Ok(())
}

// ─── Project commands ─────────────────────────────────────────────────────────

fn cmd_project_new(dir: &str, name: &str, chip: &str, format: &OutputFormat) -> anyhow::Result<()> {
    let dir_path = std::path::Path::new(dir);
    luatos_project::scaffold_project(dir_path, name, chip)?;

    match format {
        OutputFormat::Text => {
            println!("Created project '{name}' for chip '{chip}' in {dir}/");
            println!("  Config: {dir}/luatos-project.toml");
            println!("  Script: {dir}/lua/main.lua");
            println!("\nNext steps:");
            println!("  cd {dir}");
            println!("  luatos-cli build filesystem --src lua/ --output build/script.bin");
        }
        OutputFormat::Json => {
            let json = serde_json::json!({
                "status": "ok",
                "command": "project.new",
                "data": { "name": name, "chip": chip, "dir": dir },
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
    }
    Ok(())
}

fn cmd_project_info(dir: &str, format: &OutputFormat) -> anyhow::Result<()> {
    let project = luatos_project::Project::load(std::path::Path::new(dir))?;

    match format {
        OutputFormat::Text => {
            println!("Project: {}", project.project.name);
            println!("  Chip:     {}", project.project.chip);
            println!("  Version:  {}", project.project.version);
            if let Some(ref desc) = project.project.description {
                println!("  Desc:     {desc}");
            }
            println!("  Scripts:  {}", project.build.script_dirs.join(", "));
            println!("  Output:   {}", project.build.output_dir);
            println!("  Use luac: {}", project.build.use_luac);
            println!("  Bitwidth: {}", project.build.bitw);
            if let Some(ref soc) = project.flash.soc_file {
                println!("  SOC:      {soc}");
            }
            if let Some(ref port) = project.flash.port {
                println!("  Port:     {port}");
            }
        }
        OutputFormat::Json => {
            let json = serde_json::json!({
                "status": "ok",
                "command": "project.info",
                "data": project,
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
    }
    Ok(())
}

fn cmd_project_config(
    dir: &str,
    key: Option<&str>,
    value: Option<&str>,
    format: &OutputFormat,
) -> anyhow::Result<()> {
    let dir_path = std::path::Path::new(dir);
    let mut project = luatos_project::Project::load(dir_path)?;

    match (key, value) {
        (None, _) => {
            // No key: show full config
            match format {
                OutputFormat::Text => {
                    let toml_str = toml::to_string_pretty(&project)?;
                    println!("{toml_str}");
                }
                OutputFormat::Json => {
                    let json = serde_json::json!({
                        "status": "ok",
                        "command": "project.config",
                        "data": project,
                    });
                    println!("{}", serde_json::to_string_pretty(&json)?);
                }
            }
        }
        (Some(k), None) => {
            // Key only: get value
            let val = get_config_value(&project, k)?;
            match format {
                OutputFormat::Text => println!("{val}"),
                OutputFormat::Json => {
                    let json = serde_json::json!({
                        "status": "ok",
                        "command": "project.config",
                        "data": { "key": k, "value": val },
                    });
                    println!("{}", serde_json::to_string_pretty(&json)?);
                }
            }
        }
        (Some(k), Some(v)) => {
            // Key + value: set
            set_config_value(&mut project, k, v)?;
            project.save(dir_path)?;
            match format {
                OutputFormat::Text => println!("Set {k} = {v}"),
                OutputFormat::Json => {
                    let json = serde_json::json!({
                        "status": "ok",
                        "command": "project.config",
                        "data": { "key": k, "value": v },
                    });
                    println!("{}", serde_json::to_string_pretty(&json)?);
                }
            }
        }
    }
    Ok(())
}

fn get_config_value(project: &luatos_project::Project, key: &str) -> anyhow::Result<String> {
    Ok(match key {
        "project.name" => project.project.name.clone(),
        "project.chip" => project.project.chip.clone(),
        "project.version" => project.project.version.clone(),
        "project.description" => project.project.description.clone().unwrap_or_default(),
        "build.script_dir" | "build.script_dirs" => project.build.script_dirs.join(", "),
        "build.output_dir" => project.build.output_dir.clone(),
        "build.use_luac" => project.build.use_luac.to_string(),
        "build.bitw" => project.build.bitw.to_string(),
        "flash.soc_file" => project.flash.soc_file.clone().unwrap_or_default(),
        "flash.port" => project.flash.port.clone().unwrap_or_default(),
        "flash.baud" => project
            .flash
            .baud
            .map(|b| b.to_string())
            .unwrap_or_default(),
        _ => anyhow::bail!("Unknown config key: {key}"),
    })
}

fn set_config_value(
    project: &mut luatos_project::Project,
    key: &str,
    value: &str,
) -> anyhow::Result<()> {
    match key {
        "project.name" => project.project.name = value.to_string(),
        "project.chip" => project.project.chip = value.to_string(),
        "project.version" => project.project.version = value.to_string(),
        "project.description" => project.project.description = Some(value.to_string()),
        "build.script_dir" | "build.script_dirs" => {
            project.build.script_dirs = value.split(',').map(|s| s.trim().to_string()).collect();
        }
        "build.output_dir" => project.build.output_dir = value.to_string(),
        "build.use_luac" => project.build.use_luac = value.parse()?,
        "build.bitw" => project.build.bitw = value.parse()?,
        "flash.soc_file" => project.flash.soc_file = Some(value.to_string()),
        "flash.port" => project.flash.port = Some(value.to_string()),
        "flash.baud" => project.flash.baud = Some(value.parse()?),
        _ => anyhow::bail!("Unknown config key: {key}"),
    }
    Ok(())
}

// ─── Build commands ───────────────────────────────────────────────────────────

fn cmd_build_luac(
    src_dirs: &[String],
    output: &str,
    bitw: u32,
    format: &OutputFormat,
) -> anyhow::Result<()> {
    let out_path = std::path::Path::new(output);
    let mut total_files = Vec::new();

    for src in src_dirs {
        let src_path = std::path::Path::new(src);
        anyhow::ensure!(src_path.is_dir(), "Source directory not found: {src}");
        let files = luatos_luadb::build::compile_lua_dir(src_path, out_path, bitw)?;
        total_files.extend(files);
    }

    match format {
        OutputFormat::Text => {
            println!("Compiled {} files (bitw={bitw})", total_files.len());
            for f in &total_files {
                println!("  {}", f.display());
            }
        }
        OutputFormat::Json => {
            let json = serde_json::json!({
                "status": "ok",
                "command": "build.luac",
                "data": {
                    "count": total_files.len(),
                    "files": total_files.iter().map(|f| f.display().to_string()).collect::<Vec<_>>(),
                },
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
    }
    Ok(())
}

fn cmd_build_filesystem(
    src_dirs: &[String],
    output: &str,
    use_luac: bool,
    bitw: u32,
    bkcrc: bool,
    format: &OutputFormat,
) -> anyhow::Result<()> {
    let paths: Vec<std::path::PathBuf> = src_dirs.iter().map(std::path::PathBuf::from).collect();
    let path_refs: Vec<&std::path::Path> = paths.iter().map(|p| p.as_path()).collect();

    for p in &path_refs {
        anyhow::ensure!(p.is_dir(), "Source directory not found: {}", p.display());
    }

    let image = luatos_luadb::build::build_script_image(&path_refs, use_luac, bitw, bkcrc)?;

    let out_path = std::path::Path::new(output);
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(out_path, &image)?;

    match format {
        OutputFormat::Text => {
            println!("Built filesystem image: {output}");
            println!(
                "  Size:   {} bytes ({:.1} KB)",
                image.len(),
                image.len() as f64 / 1024.0
            );
            println!("  Luac:   {use_luac}");
            println!("  BK CRC: {bkcrc}");
        }
        OutputFormat::Json => {
            let json = serde_json::json!({
                "status": "ok",
                "command": "build.filesystem",
                "data": {
                    "output": output,
                    "size": image.len(),
                    "use_luac": use_luac,
                    "bkcrc": bkcrc,
                },
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
    }
    Ok(())
}

// ─── Resource commands ────────────────────────────────────────────────────────

const RESOURCE_MANIFEST_URLS: &[&str] = &[
    "http://bj02.air32.cn:10888/files/files.json",
    "http://sh.air32.cn:10888/files/files.json",
];

#[derive(serde::Deserialize, Debug)]
struct ResourceManifest {
    #[allow(dead_code)]
    version: u32,
    mirrors: Vec<Mirror>,
    resouces: Vec<ResourceCategory>, // NOTE: typo in server JSON, keep as-is
}

#[derive(serde::Deserialize, Debug, Clone)]
struct Mirror {
    url: String,
    #[allow(dead_code)]
    speed: Option<u32>,
}

#[derive(serde::Deserialize, Debug)]
struct ResourceCategory {
    name: String,
    #[serde(default)]
    desc: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    url: Option<String>,
    #[serde(default)]
    childrens: Vec<ResourceChild>,
}

#[derive(serde::Deserialize, Debug)]
struct ResourceChild {
    name: String,
    #[serde(default)]
    desc: Option<String>,
    #[serde(default)]
    versions: Vec<ResourceVersion>,
}

#[derive(serde::Deserialize, Debug)]
struct ResourceVersion {
    name: String,
    #[serde(default)]
    desc: Option<String>,
    #[serde(default)]
    files: Vec<serde_json::Value>,
}

#[allow(dead_code)]
struct FileEntry {
    desc: String,
    filename: String,
    sha256: String,
    size: u64,
    path: String,
}

/// Parse a file entry from the JSON value.
/// Format: array `["desc", "filename", "sha256", size_number, "path"]`
fn parse_file_entry(val: &serde_json::Value) -> Option<FileEntry> {
    let arr = val.as_array()?;
    if arr.len() < 5 {
        return None;
    }
    Some(FileEntry {
        desc: arr[0].as_str()?.to_string(),
        filename: arr[1].as_str()?.to_string(),
        sha256: arr[2].as_str()?.to_string(),
        size: arr[3].as_u64()?,
        path: arr[4].as_str()?.to_string(),
    })
}

fn fetch_manifest() -> anyhow::Result<ResourceManifest> {
    let mut last_err = None;
    for url in RESOURCE_MANIFEST_URLS {
        match ureq::get(url).call() {
            Ok(resp) => {
                let body = resp.into_string()?;
                let manifest: ResourceManifest = serde_json::from_str(&body)?;
                return Ok(manifest);
            }
            Err(e) => {
                log::warn!("Failed to fetch {url}: {e}");
                last_err = Some(e);
            }
        }
    }
    anyhow::bail!(
        "Failed to fetch resource manifest from all mirrors: {}",
        last_err
            .map(|e| e.to_string())
            .unwrap_or_else(|| "no URLs".into())
    );
}

fn format_size(bytes: u64) -> String {
    if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else if bytes >= 1_000 {
        format!("{:.1} KB", bytes as f64 / 1_000.0)
    } else {
        format!("{bytes} B")
    }
}

fn cmd_resource_list(module: Option<&str>, format: &OutputFormat) -> anyhow::Result<()> {
    let manifest = fetch_manifest()?;

    match module {
        None => {
            // List all modules
            match format {
                OutputFormat::Text => {
                    println!("{:<20} DESCRIPTION", "MODULE");
                    for cat in &manifest.resouces {
                        println!("{:<20} {}", cat.name, cat.desc.as_deref().unwrap_or(""));
                    }
                }
                OutputFormat::Json => {
                    let modules: Vec<_> = manifest
                        .resouces
                        .iter()
                        .map(|c| {
                            serde_json::json!({
                                "name": c.name,
                                "desc": c.desc,
                            })
                        })
                        .collect();
                    let json = serde_json::json!({
                        "status": "ok",
                        "command": "resource.list",
                        "data": modules,
                    });
                    println!("{}", serde_json::to_string_pretty(&json)?);
                }
            }
        }
        Some(name) => {
            let cat = manifest
                .resouces
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| anyhow::anyhow!("Module '{}' not found", name))?;

            match format {
                OutputFormat::Text => {
                    println!("{} — {}", cat.name, cat.desc.as_deref().unwrap_or(""));
                    for child in &cat.childrens {
                        println!(
                            "\n  {} — {}",
                            child.name,
                            child.desc.as_deref().unwrap_or("")
                        );
                        for ver in &child.versions {
                            println!("    {} — {}", ver.name, ver.desc.as_deref().unwrap_or(""));
                            for raw in &ver.files {
                                if let Some(entry) = parse_file_entry(raw) {
                                    println!(
                                        "      {}  {}  {}",
                                        entry.desc,
                                        entry.filename,
                                        format_size(entry.size)
                                    );
                                }
                            }
                        }
                    }
                }
                OutputFormat::Json => {
                    let children: Vec<_> = cat
                        .childrens
                        .iter()
                        .map(|child| {
                            let versions: Vec<_> = child
                                .versions
                                .iter()
                                .map(|ver| {
                                    let files: Vec<_> = ver
                                        .files
                                        .iter()
                                        .filter_map(|raw| {
                                            parse_file_entry(raw).map(|e| {
                                                serde_json::json!({
                                                    "desc": e.desc,
                                                    "filename": e.filename,
                                                    "sha256": e.sha256,
                                                    "size": e.size,
                                                    "path": e.path,
                                                })
                                            })
                                        })
                                        .collect();
                                    serde_json::json!({
                                        "name": ver.name,
                                        "desc": ver.desc,
                                        "files": files,
                                    })
                                })
                                .collect();
                            serde_json::json!({
                                "name": child.name,
                                "desc": child.desc,
                                "versions": versions,
                            })
                        })
                        .collect();
                    let json = serde_json::json!({
                        "status": "ok",
                        "command": "resource.list",
                        "data": {
                            "name": cat.name,
                            "desc": cat.desc,
                            "childrens": children,
                        },
                    });
                    println!("{}", serde_json::to_string_pretty(&json)?);
                }
            }
        }
    }
    Ok(())
}

fn cmd_resource_download(
    module: &str,
    version_filter: Option<&str>,
    output: &str,
    format: &OutputFormat,
) -> anyhow::Result<()> {
    use sha2::Digest;

    let manifest = fetch_manifest()?;

    let cat = manifest
        .resouces
        .iter()
        .find(|c| c.name.eq_ignore_ascii_case(module))
        .ok_or_else(|| anyhow::anyhow!("Module '{}' not found", module))?;

    // Collect all matching files
    let mut files_to_download: Vec<FileEntry> = Vec::new();

    for child in &cat.childrens {
        for ver in &child.versions {
            if let Some(filter) = version_filter {
                if !ver.name.contains(filter) {
                    continue;
                }
            }
            for raw in &ver.files {
                if let Some(entry) = parse_file_entry(raw) {
                    files_to_download.push(entry);
                }
            }
            // If no version filter, only take the first (latest) version per child
            if version_filter.is_none() {
                break;
            }
        }
    }

    if files_to_download.is_empty() {
        anyhow::bail!(
            "No files found for module '{}' with version filter {:?}",
            module,
            version_filter
        );
    }

    // Sort mirrors by speed (descending)
    let mut mirrors = manifest.mirrors.clone();
    mirrors.sort_by(|a, b| b.speed.unwrap_or(0).cmp(&a.speed.unwrap_or(0)));

    let out_path = std::path::Path::new(output);
    std::fs::create_dir_all(out_path)?;

    let mut downloaded = 0u32;
    let mut failed = 0u32;

    for entry in &files_to_download {
        let dest = out_path.join(&entry.path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }

        if format == &OutputFormat::Text {
            eprintln!(
                "Downloading {} ({})...",
                entry.filename,
                format_size(entry.size)
            );
        }

        let mut success = false;
        for mirror in &mirrors {
            let url = format!("{}{}", mirror.url, entry.path);
            match download_file(&url, &dest, entry.size) {
                Ok(()) => {
                    // Verify SHA256
                    let data = std::fs::read(&dest)?;
                    let hash = sha2::Sha256::digest(&data);
                    let hex = format!("{:X}", hash);
                    if hex.eq_ignore_ascii_case(&entry.sha256) {
                        if format == &OutputFormat::Text {
                            eprintln!("  ✓ SHA256 verified: {}", dest.display());
                        }
                        downloaded += 1;
                        success = true;
                        break;
                    } else {
                        eprintln!(
                            "  ✗ SHA256 mismatch for {} (expected {}, got {})",
                            entry.filename, entry.sha256, hex
                        );
                        let _ = std::fs::remove_file(&dest);
                    }
                }
                Err(e) => {
                    log::warn!("  Mirror {} failed: {e}", mirror.url);
                }
            }
        }
        if !success {
            eprintln!("  ✗ Failed to download {}", entry.filename);
            failed += 1;
        }
    }

    match format {
        OutputFormat::Text => {
            println!(
                "\nDownload complete: {} succeeded, {} failed",
                downloaded, failed
            );
        }
        OutputFormat::Json => {
            let json = serde_json::json!({
                "status": if failed == 0 { "ok" } else { "partial" },
                "command": "resource.download",
                "data": {
                    "module": module,
                    "downloaded": downloaded,
                    "failed": failed,
                    "output": output,
                },
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
    }

    if failed > 0 {
        anyhow::bail!("{failed} file(s) failed to download");
    }
    Ok(())
}

fn download_file(url: &str, dest: &std::path::Path, size: u64) -> anyhow::Result<()> {
    use std::io::Read;

    let resp = ureq::get(url).call()?;
    let mut reader = resp.into_reader();

    let pb = indicatif::ProgressBar::new(size);
    pb.set_style(
        indicatif::ProgressStyle::default_bar()
            .template("  [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
            .unwrap_or_else(|_| indicatif::ProgressStyle::default_bar())
            .progress_chars("##-"),
    );

    let mut file = std::fs::File::create(dest)?;
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        std::io::Write::write_all(&mut file, &buf[..n])?;
        pb.inc(n as u64);
    }
    pb.finish_and_clear();
    Ok(())
}
