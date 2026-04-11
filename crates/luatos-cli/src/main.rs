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
#[command(name = "luatos-cli", version, about = "LuatOS CLI tool — flash, log, project management")]
struct Cli {
    /// Output format
    #[arg(long, default_value = "text", global = true)]
    format: OutputFormat,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Clone, clap::ValueEnum)]
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
        /// Script folder (optional)
        #[arg(long)]
        script: Option<String>,
    },
    /// Flash script partition only (most common during development)
    Script {
        /// Path to .soc file
        #[arg(long)]
        soc: String,
        /// Serial port (e.g. COM6)
        #[arg(long)]
        port: String,
        /// Script folder containing .lua files
        #[arg(long)]
        script: String,
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
        /// Script folder containing files to pack
        #[arg(long)]
        script: String,
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
    /// View serial log in binary SOC mode (Air6208 etc.)
    ViewBinary {
        /// Serial port (e.g. COM7)
        #[arg(long)]
        port: String,
        /// Baud rate (default: 2000000)
        #[arg(long, default_value = "2000000")]
        baud: u32,
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
        /// Source directory containing .lua files
        #[arg(long, default_value = "lua/")]
        src: String,
        /// Output directory for .luac files
        #[arg(long, default_value = "build/")]
        output: String,
        /// Lua integer bit-width (32 or 64)
        #[arg(long, default_value = "32")]
        bitw: u32,
    },
    /// Build LuaDB script filesystem image
    Filesystem {
        /// Source directory containing .lua / .luac files
        #[arg(long, default_value = "lua/")]
        src: String,
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
            SocCommands::Unpack { path, output } => cmd_soc_unpack(&path, output.as_deref(), &cli.format),
            SocCommands::Files { path } => cmd_soc_files(&path, &cli.format),
            SocCommands::Pack { dir, output } => cmd_soc_pack(&dir, &output, &cli.format),
        },
        Commands::Flash { action } => match action {
            FlashCommands::Run {
                soc,
                port,
                baud,
                script,
            } => cmd_flash_run(&soc, &port, baud, script.as_deref(), &cli.format),
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
        },
        Commands::Log { action } => match action {
            LogCommands::View { port, baud } => cmd_log_view(&port, baud, &cli.format),
            LogCommands::ViewBinary { port, baud } => cmd_log_view_binary(&port, baud, &cli.format),
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
                println!("{:<10} {:<10} {:<10} {}", "PORT", "VID", "PID", "PRODUCT");
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
    script: Option<&str>,
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
            let lines = luatos_flash::bk7258::flash_bk7258(
                soc, script, port, baud, cancel, on_progress,
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
        _ => {
            anyhow::bail!("Unsupported chip type: {chip}. Supported: bk72xx, air6208, air101");
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
    script_folder: Option<&str>,
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
                let folder = script_folder.expect("script folder required");
                luatos_flash::bk7258::flash_script_only(soc, folder, port, cancel, on_progress)?;
            }
            "clear-fs" => {
                luatos_flash::bk7258::clear_filesystem(soc, port, cancel, on_progress)?;
            }
            "flash-fs" => {
                let folder = script_folder.expect("script folder required");
                luatos_flash::bk7258::flash_filesystem(soc, folder, port, cancel, on_progress)?;
            }
            "clear-kv" => {
                luatos_flash::bk7258::clear_fskv(soc, port, cancel, on_progress)?;
            }
            _ => unreachable!(),
        },
        "air6208" | "air101" | "air103" | "air601" => match op {
            "script" => {
                let folder = script_folder.expect("script folder required");
                let files = collect_script_files(folder)?;
                luatos_flash::xt804::flash_script_only(
                    soc, port, &files, on_progress, cancel,
                )?;
            }
            _ => {
                anyhow::bail!(
                    "Operation '{op}' is not supported for XT804 devices ({})",
                    chip
                );
            }
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

/// Collect script files from a folder (*.lua, *.luac, *.json, etc.)
fn collect_script_files(folder: &str) -> anyhow::Result<Vec<String>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(folder)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() {
            files.push(path.to_string_lossy().to_string());
        }
    }
    if files.is_empty() {
        anyhow::bail!("No script files found in {folder}");
    }
    Ok(files)
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

fn cmd_log_view_binary(port: &str, baud: u32, format: &OutputFormat) -> anyhow::Result<()> {
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_clone = stop.clone();
    let _ = ctrlc::set_handler(move || {
        stop_clone.store(true, std::sync::atomic::Ordering::Relaxed);
    });

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
                            let time = entry
                                .device_time
                                .as_deref()
                                .unwrap_or("?");
                            println!(
                                "[{}] {}/{} {}",
                                time, entry.level, module, entry.message
                            );
                        }
                        OutputFormat::Json => {
                            println!(
                                "{}",
                                serde_json::to_string(&entry).unwrap_or_default()
                            );
                        }
                    }
                }
            }
        }),
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

    let writer =
        luatos_log::LogWriter::new(Some(&text_path), json_path.as_deref())?;

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
                if count % 50 == 0 {
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
                let time = entry
                    .device_time
                    .as_deref()
                    .unwrap_or(&entry.timestamp);
                println!(
                    "[{}] {}/{} {}",
                    time,
                    entry.level,
                    module,
                    entry.message
                );
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
            println!("  Scripts:  {}", project.build.script_dir);
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
        "project.description" => project
            .project
            .description
            .clone()
            .unwrap_or_default(),
        "build.script_dir" => project.build.script_dir.clone(),
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
        "build.script_dir" => project.build.script_dir = value.to_string(),
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
    src: &str,
    output: &str,
    bitw: u32,
    format: &OutputFormat,
) -> anyhow::Result<()> {
    let src_path = std::path::Path::new(src);
    let out_path = std::path::Path::new(output);

    anyhow::ensure!(src_path.is_dir(), "Source directory not found: {src}");

    let files = luatos_luadb::build::compile_lua_dir(src_path, out_path, bitw)?;

    match format {
        OutputFormat::Text => {
            println!("Compiled {} files (bitw={bitw})", files.len());
            for f in &files {
                println!("  {}", f.display());
            }
        }
        OutputFormat::Json => {
            let json = serde_json::json!({
                "status": "ok",
                "command": "build.luac",
                "data": {
                    "count": files.len(),
                    "files": files.iter().map(|f| f.display().to_string()).collect::<Vec<_>>(),
                },
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
    }
    Ok(())
}

fn cmd_build_filesystem(
    src: &str,
    output: &str,
    use_luac: bool,
    bitw: u32,
    bkcrc: bool,
    format: &OutputFormat,
) -> anyhow::Result<()> {
    let src_path = std::path::Path::new(src);
    anyhow::ensure!(src_path.is_dir(), "Source directory not found: {src}");

    let image = luatos_luadb::build::build_script_image(src_path, use_luac, bitw, bkcrc)?;

    let out_path = std::path::Path::new(output);
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(out_path, &image)?;

    match format {
        OutputFormat::Text => {
            println!("Built filesystem image: {output}");
            println!("  Size:   {} bytes ({:.1} KB)", image.len(), image.len() as f64 / 1024.0);
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