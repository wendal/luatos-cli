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
    /// View serial log in real-time
    View {
        /// Serial port (e.g. COM6)
        #[arg(long)]
        port: String,
        /// Baud rate (default: 921600)
        #[arg(long, default_value = "921600")]
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
            LogCommands::Record {
                port,
                baud,
                output,
                json,
            } => cmd_log_record(&port, baud, &output, json, &cli.format),
            LogCommands::Parse { path } => cmd_log_parse(&path, &cli.format),
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

    let format_clone = format.clone();
    let on_progress: luatos_flash::ProgressCallback = Box::new(move |p| match format_clone {
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
    });

    let lines = luatos_flash::bk7258::flash_bk7258(soc, script, port, baud, cancel, on_progress)?;

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
                "data": {
                    "boot_log": lines,
                },
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
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

    match op {
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
