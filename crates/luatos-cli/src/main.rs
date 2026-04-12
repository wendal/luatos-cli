// luatos-cli — LuatOS command-line tool.
//
// Usage:
//   luatos-cli serial list              # List serial ports
//   luatos-cli soc info <path>          # Show SOC file info
//   luatos-cli soc unpack <path> -o dir # Extract SOC file
//   luatos-cli flash run --soc <path> --port COM6
//   luatos-cli flash test --soc <path> --port COM6

use clap::{Parser, Subcommand};

mod cmd_build;
mod cmd_device;
mod cmd_flash;
mod cmd_log;
mod cmd_project;
mod cmd_resource;
mod cmd_serial;
mod cmd_soc;

#[derive(Parser)]
#[command(name = "luatos-cli", version, about = "LuatOS CLI tool — flash, log, project management")]
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
    /// Device control (reboot, enter boot mode)
    Device {
        #[command(subcommand)]
        action: DeviceCommands,
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
    /// View serial log in binary SOC mode (Air1601, Air8000/EC718, etc.)
    ViewBinary {
        /// Serial port (e.g. COM7, or "auto" for EC718 auto-detect)
        #[arg(long)]
        port: String,
        /// Baud rate (default: 2000000)
        #[arg(long, default_value = "2000000")]
        baud: u32,
        /// Send probe command to trigger log output (required for Air1601/CCM4211/EC718)
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
        /// Target chip (bk72xx, air6208, air8101, air8000, air101, ec7xx)
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
    /// Import a LuaTools .ini project file
    Import {
        /// Path to the .ini file
        ini: String,
        /// Output directory for the converted project (default: current directory)
        #[arg(long, default_value = ".")]
        dir: String,
    },
    /// Analyze Lua script dependencies
    Deps {
        /// Project directory (default: current directory)
        #[arg(long, default_value = ".")]
        dir: String,
        /// Show only reachable files (filtered by dependency analysis)
        #[arg(long)]
        reachable: bool,
        /// Show only unreachable files (not needed by main.lua)
        #[arg(long)]
        unreachable: bool,
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

#[derive(Subcommand)]
enum DeviceCommands {
    /// Reboot the device via DTR/RTS or AT command
    Reboot {
        /// Serial port (e.g. COM6). EC718 series can be omitted for auto-detect.
        #[arg(long)]
        port: Option<String>,
        /// Chip type (bk72xx, air8101, xt804, air6208, air101, ec718, air8000, air1601, ccm4211, ...)
        /// If omitted, generic DTR pulse is used.
        #[arg(long)]
        chip: Option<String>,
    },
    /// Force device into bootloader (boot) mode via DTR/RTS or AT command
    Boot {
        /// Serial port (e.g. COM6). EC718 series can be omitted for auto-detect.
        #[arg(long)]
        port: Option<String>,
        /// Chip type (bk72xx, air8101, xt804, air6208, air101, ec718, air8000, air1601, ccm4211, ...)
        /// If omitted, generic DTR+RTS pulse is used.
        #[arg(long)]
        chip: Option<String>,
    },
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Serial { action } => match action {
            SerialCommands::List => cmd_serial::cmd_serial_list(&cli.format),
        },
        Commands::Soc { action } => match action {
            SocCommands::Info { path } => cmd_soc::cmd_soc_info(&path, &cli.format),
            SocCommands::Unpack { path, output } => cmd_soc::cmd_soc_unpack(&path, output.as_deref(), &cli.format),
            SocCommands::Files { path } => cmd_soc::cmd_soc_files(&path, &cli.format),
            SocCommands::Pack { dir, output } => cmd_soc::cmd_soc_pack(&dir, &output, &cli.format),
        },
        Commands::Flash { action } => match action {
            FlashCommands::Run { soc, port, baud, script } => {
                let script_opt = if script.is_empty() { None } else { Some(script.as_slice()) };
                cmd_flash::cmd_flash_run(&soc, &port, baud, script_opt, &cli.format)
            }
            FlashCommands::Script { soc, port, script } => cmd_flash::cmd_flash_partition("script", &soc, &port, Some(&script), &cli.format),
            FlashCommands::ClearFs { soc, port } => cmd_flash::cmd_flash_partition("clear-fs", &soc, &port, None, &cli.format),
            FlashCommands::FlashFs { soc, port, script } => cmd_flash::cmd_flash_partition("flash-fs", &soc, &port, Some(&script), &cli.format),
            FlashCommands::ClearKv { soc, port } => cmd_flash::cmd_flash_partition("clear-kv", &soc, &port, None, &cli.format),
            FlashCommands::Test {
                soc,
                port,
                baud,
                script,
                timeout,
                keyword,
            } => {
                let script_opt = if script.is_empty() { None } else { Some(script.as_slice()) };
                cmd_flash::cmd_flash_test(&soc, &port, baud, script_opt, timeout, &keyword, &cli.format)
            }
        },
        Commands::Log { action } => match action {
            LogCommands::View { port, baud } => cmd_log::cmd_log_view(&port, baud, &cli.format),
            LogCommands::ViewBinary { port, baud, probe } => cmd_log::cmd_log_view_binary(&port, baud, probe, &cli.format),
            LogCommands::Record { port, baud, output, json } => cmd_log::cmd_log_record(&port, baud, &output, json, &cli.format),
            LogCommands::Parse { path } => cmd_log::cmd_log_parse(&path, &cli.format),
        },
        Commands::Project { action } => match action {
            ProjectCommands::New { name, chip, dir } => {
                let dir = dir.unwrap_or_else(|| name.clone());
                cmd_project::cmd_project_new(&dir, &name, &chip, &cli.format)
            }
            ProjectCommands::Info { dir } => cmd_project::cmd_project_info(&dir, &cli.format),
            ProjectCommands::Config { dir, key, value } => cmd_project::cmd_project_config(&dir, key.as_deref(), value.as_deref(), &cli.format),
            ProjectCommands::Import { ini, dir } => cmd_project::cmd_project_import(&ini, &dir, &cli.format),
            ProjectCommands::Deps { dir, reachable, unreachable } => cmd_project::cmd_project_deps(&dir, reachable, unreachable, &cli.format),
        },
        Commands::Build { action } => match action {
            BuildCommands::Luac { src, output, bitw } => cmd_build::cmd_build_luac(&src, &output, bitw, &cli.format),
            BuildCommands::Filesystem { src, output, luac, bitw, bkcrc } => cmd_build::cmd_build_filesystem(&src, &output, luac, bitw, bkcrc, &cli.format),
        },
        Commands::Resource { action } => match action {
            ResourceCommands::List { module } => cmd_resource::cmd_resource_list(module.as_deref(), &cli.format),
            ResourceCommands::Download { module, version, output } => cmd_resource::cmd_resource_download(&module, version.as_deref(), &output, &cli.format),
        },
        Commands::Device { action } => match action {
            DeviceCommands::Reboot { port, chip } => cmd_device::cmd_device_reboot(port.as_deref(), chip.as_deref(), &cli.format),
            DeviceCommands::Boot { port, chip } => cmd_device::cmd_device_boot(port.as_deref(), chip.as_deref(), &cli.format),
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
