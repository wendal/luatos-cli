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
    /// Flash firmware to device
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
