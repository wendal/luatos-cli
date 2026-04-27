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
mod cmd_doctor;
mod cmd_flash;
mod cmd_fota;
mod cmd_log;
mod cmd_project;
mod cmd_project_wizard;
mod cmd_resource;
mod cmd_serial;
mod cmd_soc;
mod event;

#[derive(Parser)]
#[command(name = "luatos-cli", version, about = "LuatOS CLI tool — flash, log, project management")]
struct Cli {
    /// Output format
    #[arg(long, default_value = "text", global = true)]
    format: OutputFormat,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Clone, Copy, PartialEq, clap::ValueEnum)]
enum OutputFormat {
    Text,
    Json,
    Jsonl,
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
        /// 进度输出步进（1-50%，默认每 10% 输出一次）
        #[arg(long, default_value = "10", value_parser = clap::value_parser!(u8).range(1..=50))]
        progress_step: u8,
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
    /// Diagnose development environment (serial ports, project, firmware, tools)
    Doctor {
        /// Project directory to check (default: current directory)
        #[arg(long, default_value = ".")]
        dir: String,
    },
    /// Show version information
    Version,
    /// Build a FOTA (firmware over-the-air) update package
    Fota {
        #[command(subcommand)]
        action: FotaCommands,
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
    /// Inject a binary into an EC7xx .soc at a given flash address (EC7xx/Air8000 only)
    Combine {
        /// Path to source .soc file
        #[arg(long)]
        soc: String,
        /// Binary file to inject
        #[arg(long)]
        bin: String,
        /// Flash address (hex, e.g. 0x00D00000)
        #[arg(long)]
        addr: String,
        /// Output .soc path (default: <source>_combined.soc)
        #[arg(short, long)]
        output: Option<String>,
    },
}

/// DTR/RTS 引脚电平选择（用于 SF32LB58 CH340X 改装硬件调试）
#[derive(Clone, Copy, clap::ValueEnum)]
enum SignalLevel {
    /// 高电平
    High,
    /// 低电平
    Low,
}

impl SignalLevel {
    fn as_bool(self) -> bool {
        matches!(self, SignalLevel::High)
    }
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
        /// 自动控制 DTR/RTS 进入/退出 ROM BL（适用于 CH340X 增强 DTR 改装硬件，仅 SF32LB58）
        #[arg(long)]
        auto_reset: bool,
        /// 进入 boot 时 DTR 的电平（high=BOOT0拉高，low=BOOT0拉低，默认 low）
        #[arg(long, value_enum, default_value = "low")]
        dtr_boot: SignalLevel,
        /// 触发复位时 RTS 的电平（high=CH340X RTS#拉低=RESET有效，默认 high）
        #[arg(long, value_enum, default_value = "high")]
        rts_reset: SignalLevel,
        /// 复位脉冲宽度（毫秒，默认 100）
        #[arg(long, default_value = "100")]
        reset_ms: u64,
        /// 进入 boot 后等待 ROM BL 初始化的时长（毫秒，默认 500）
        #[arg(long, default_value = "500")]
        boot_wait_ms: u64,
    },
    /// Flash script partition only (most common during development)
    Script {
        /// Path to .soc file
        #[arg(long)]
        soc: String,
        /// Serial port (e.g. COM6)
        #[arg(long)]
        port: String,
        /// 刷写波特率（stub 加载后协商，CH342K 支持最高 3000000；默认不切换）
        #[arg(long)]
        baud: Option<u32>,
        /// Script folders containing .lua files (can specify multiple)
        #[arg(long)]
        script: Vec<String>,
        /// 自动控制 DTR/RTS 进入/退出 ROM BL（适用于 CH340X 增强 DTR 改装硬件，仅 SF32LB58）
        #[arg(long)]
        auto_reset: bool,
        /// 进入 boot 时 DTR 的电平（high=BOOT0拉高，low=BOOT0拉低，默认 low）
        #[arg(long, value_enum, default_value = "low")]
        dtr_boot: SignalLevel,
        /// 触发复位时 RTS 的电平（high=CH340X RTS#拉低=RESET有效，默认 high）
        #[arg(long, value_enum, default_value = "high")]
        rts_reset: SignalLevel,
        /// 复位脉冲宽度（毫秒，默认 100）
        #[arg(long, default_value = "100")]
        reset_ms: u64,
        /// 进入 boot 后等待 ROM BL 初始化的时长（毫秒，默认 500）
        #[arg(long, default_value = "500")]
        boot_wait_ms: u64,
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
        /// 刷写波特率（stub 加载后协商，仅 SF32LB58；默认不切换）
        #[arg(long)]
        baud: Option<u32>,
        /// 自动控制 DTR/RTS 进入/退出 ROM BL（适用于 CH340X 增强 DTR 改装硬件，仅 SF32LB58）
        #[arg(long)]
        auto_reset: bool,
        /// 进入 boot 时 DTR 的电平（high=BOOT0拉高，low=BOOT0拉低，默认 low）
        #[arg(long, value_enum, default_value = "low")]
        dtr_boot: SignalLevel,
        /// 触发复位时 RTS 的电平（high=CH340X RTS#拉低=RESET有效，默认 high）
        #[arg(long, value_enum, default_value = "high")]
        rts_reset: SignalLevel,
        /// 复位脉冲宽度（毫秒，默认 100）
        #[arg(long, default_value = "100")]
        reset_ms: u64,
        /// 进入 boot 后等待 ROM BL 初始化的时长（毫秒，默认 500）
        #[arg(long, default_value = "500")]
        boot_wait_ms: u64,
    },
    /// Air6201 external SPI flash programming (write to script/fskv/lfs partition)
    ExtFlash {
        /// Serial port (e.g. COM3)
        #[arg(long)]
        port: String,
        /// Baud rate (default: 2000000)
        #[arg(long, default_value = "2000000")]
        baud: u32,
        /// Partition to flash: script, fskv, lfs
        #[arg(long)]
        partition: String,
        /// Data file to write (binary)
        #[arg(long)]
        file: String,
        /// Send EXT_PROG command to enter programming mode (for initial connection)
        #[arg(long)]
        ext_prog: bool,
    },
    /// Air6201 external SPI flash partition erase
    ExtErase {
        /// Serial port (e.g. COM3)
        #[arg(long)]
        port: String,
        /// Baud rate (default: 2000000)
        #[arg(long, default_value = "2000000")]
        baud: u32,
        /// Partition to erase: script, fskv, lfs
        #[arg(long)]
        partition: String,
        /// Send EXT_PROG command to enter programming mode
        #[arg(long)]
        ext_prog: bool,
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
        /// Enable smart analysis: auto-detect common issues and show suggestions
        #[arg(long)]
        smart: bool,
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
        /// Save raw binary log to directory (rolling 200 MB files with timestamp injection)
        #[arg(long)]
        save: Option<String>,
        /// Enable smart analysis: auto-detect common issues and show suggestions
        #[arg(long)]
        smart: bool,
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
    /// 向导式创建 LuatOS 项目（交互式引导选择型号、版本、模板等）
    ///
    /// 从 CDN 拉取固件清单，引导用户选择模组型号、固件版本、项目模板、
    /// COM 口、soc_script 版本，并可选择立即下载固件和初始化 Git 仓库。
    ///
    /// 全参数时跳过交互，例如：
    ///   project wizard --name my-app --model Air8101 --template helloworld --no-git
    Wizard {
        /// 项目名称（省略则交互输入）
        #[arg(long)]
        name: Option<String>,
        /// 创建目录（默认 ./<name>）
        #[arg(long)]
        dir: Option<String>,
        /// 模组型号（省略则交互选择，如 Air8101、Air780EPM）
        #[arg(long)]
        model: Option<String>,
        /// 固件版本号（省略则交互选择，如 V2001）
        #[arg(long)]
        version: Option<String>,
        /// 项目模板（helloworld / ui / empty，省略则交互选择）
        #[arg(long)]
        template: Option<String>,
        /// 串口（省略则交互选择，"none" 表示不选）
        #[arg(long)]
        port: Option<String>,
        /// soc_script 版本（latest / disable / 版本号，省略则交互选择）
        #[arg(long)]
        soc_script: Option<String>,
        /// 跳过 git 初始化
        #[arg(long)]
        no_git: bool,
        /// 跳过固件和 soc_script 下载
        #[arg(long)]
        no_download: bool,
    },
    /// 创建新 LuatOS 项目（非交互式，需指定 --chip；省略 --chip 时自动进入向导）
    ///
    /// 示例：
    ///   project new my-app --chip air8101
    ///   project new my-app                 # 省略 --chip 进入向导
    New {
        /// 项目名称
        name: String,
        /// 目标芯片族（bk72xx / air6208 / air8101 / air8000 / air101 / ec7xx）。
        /// 省略时自动进入向导（等同于 project wizard --name <name>）。
        #[arg(long)]
        chip: Option<String>,
        /// 项目目录（默认 ./<name>）
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
    /// Import a LuaTools .ini project or a .luatos archive
    Import {
        /// Path to the .ini or .luatos file
        file: String,
        /// Output directory for the project (default: current directory)
        #[arg(long, default_value = ".")]
        dir: String,
    },
    /// Export project to a .luatos archive
    Export {
        /// Project directory (default: current directory)
        #[arg(long, default_value = ".")]
        dir: String,
        /// Output archive path (default: <name>.luatos)
        #[arg(short, long)]
        output: Option<String>,
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
    /// Full project analysis: syntax check, deps, space usage, partition info
    Analyze {
        /// Project directory (default: current directory)
        #[arg(long, default_value = ".")]
        dir: String,
        /// Path to .soc file (overrides project config; used for partition size info)
        #[arg(long)]
        soc: Option<String>,
    },
    /// Build script filesystem image using project configuration
    ///
    /// Reads luatos-project.toml, resolves soc_script lib directory,
    /// and builds the script image. Use `build.soc_script = "disable"` to skip
    /// the soc_script library, or set a specific version like `"v2026.04.10.16"`.
    Build {
        /// Project directory (default: current directory)
        #[arg(long, default_value = ".")]
        dir: String,
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
        /// 分区最大容量（KB），超出则报错
        #[arg(long)]
        max_size_kb: Option<u32>,
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
    ///
    /// Examples:
    ///   resource download public soc_script                   # 最新版
    ///   resource download public soc_script v2026.04.10.16   # 指定版本
    ///   resource download Air8000 V2032                       # 指定大版本全部文件
    ///   resource download Air8000 V2032 114                   # 指定大版本中包含 "114" 的文件
    Download {
        /// 资源大类名称（如 Air8000、public）
        category: String,
        /// 子项名称 或 版本过滤器（如 soc_script、V2032）
        sub: Option<String>,
        /// 版本名（当 sub 为子项名时），或文件名过滤器（当 sub 为版本过滤时）
        item: Option<String>,
        /// 输出目录（默认 resource/）
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

#[derive(Subcommand)]
enum FotaCommands {
    /// Build a FOTA update package (.sota) from one or two .soc files
    Build {
        /// New firmware .soc file
        #[arg(long)]
        new: String,
        /// Old firmware .soc file (differential FOTA; omit for full FOTA)
        #[arg(long)]
        old: Option<String>,
        /// Output .sota file path (default: <chip>_fota.sota)
        #[arg(short, long)]
        output: Option<String>,
        /// Path to FotaToolkit.exe (auto-detected if omitted)
        #[arg(long)]
        fota_toolkit: Option<String>,
        /// Path to soc_tools.exe (auto-detected if omitted)
        #[arg(long)]
        soc_tools: Option<String>,
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
            SocCommands::Combine { soc, bin, addr, output } => cmd_soc::cmd_soc_combine(&soc, &bin, &addr, output.as_deref(), &cli.format),
        },
        Commands::Flash { action, progress_step } => match action {
            FlashCommands::Run {
                soc,
                port,
                baud,
                script,
                auto_reset,
                dtr_boot,
                rts_reset,
                reset_ms,
                boot_wait_ms,
            } => {
                let script_opt = if script.is_empty() { None } else { Some(script.as_slice()) };
                let reset_config = if auto_reset {
                    Some(luatos_flash::sf32lb5x::Sf32ResetConfig {
                        dtr_boot: dtr_boot.as_bool(),
                        rts_reset: rts_reset.as_bool(),
                        reset_ms,
                        boot_wait_ms,
                        ..Default::default()
                    })
                } else {
                    None
                };
                cmd_flash::cmd_flash_run(&soc, &port, baud, script_opt, progress_step, &cli.format, reset_config)
            }
            FlashCommands::Script {
                soc,
                port,
                baud,
                script,
                auto_reset,
                dtr_boot,
                rts_reset,
                reset_ms,
                boot_wait_ms,
            } => {
                let reset_config = if auto_reset {
                    Some(luatos_flash::sf32lb5x::Sf32ResetConfig {
                        dtr_boot: dtr_boot.as_bool(),
                        rts_reset: rts_reset.as_bool(),
                        reset_ms,
                        boot_wait_ms,
                        ..Default::default()
                    })
                } else {
                    None
                };
                cmd_flash::cmd_flash_partition("script", &soc, &port, Some(&script), progress_step, &cli.format, reset_config, baud)
            }
            FlashCommands::ClearFs { soc, port } => cmd_flash::cmd_flash_partition("clear-fs", &soc, &port, None, progress_step, &cli.format, None, None),
            FlashCommands::FlashFs { soc, port, script } => cmd_flash::cmd_flash_partition("flash-fs", &soc, &port, Some(&script), progress_step, &cli.format, None, None),
            FlashCommands::ClearKv {
                soc,
                port,
                baud,
                auto_reset,
                dtr_boot,
                rts_reset,
                reset_ms,
                boot_wait_ms,
            } => {
                let reset_config = if auto_reset {
                    Some(luatos_flash::sf32lb5x::Sf32ResetConfig {
                        dtr_boot: dtr_boot.as_bool(),
                        rts_reset: rts_reset.as_bool(),
                        reset_ms,
                        boot_wait_ms,
                        ..Default::default()
                    })
                } else {
                    None
                };
                cmd_flash::cmd_flash_partition("clear-kv", &soc, &port, None, progress_step, &cli.format, reset_config, baud)
            }
            FlashCommands::ExtFlash {
                port,
                baud,
                partition,
                file,
                ext_prog,
            } => cmd_flash::cmd_flash_ext_flash(&port, baud, &partition, &file, ext_prog, progress_step, &cli.format),
            FlashCommands::ExtErase { port, baud, partition, ext_prog } => cmd_flash::cmd_flash_ext_erase(&port, baud, &partition, ext_prog, progress_step, &cli.format),
            FlashCommands::Test {
                soc,
                port,
                baud,
                script,
                timeout,
                keyword,
            } => {
                let script_opt = if script.is_empty() { None } else { Some(script.as_slice()) };
                cmd_flash::cmd_flash_test(&soc, &port, baud, script_opt, timeout, &keyword, progress_step, &cli.format)
            }
        },
        Commands::Log { action } => match action {
            LogCommands::View { port, baud, smart } => cmd_log::cmd_log_view(&port, baud, smart, &cli.format),
            LogCommands::ViewBinary { port, baud, probe, save, smart } => cmd_log::cmd_log_view_binary(&port, baud, probe, save.as_deref(), smart, &cli.format),
            LogCommands::Record { port, baud, output, json } => cmd_log::cmd_log_record(&port, baud, &output, json, &cli.format),
            LogCommands::Parse { path } => cmd_log::cmd_log_parse(&path, &cli.format),
        },
        Commands::Project { action } => match action {
            ProjectCommands::Wizard {
                name,
                dir,
                model,
                version,
                template,
                port,
                soc_script,
                no_git,
                no_download,
            } => cmd_project_wizard::run_wizard(
                cmd_project_wizard::WizardArgs {
                    project_name: name,
                    project_dir: dir,
                    model,
                    firmware_version: version,
                    template,
                    port,
                    soc_script,
                    no_git,
                    no_download,
                },
                &cli.format,
            ),
            ProjectCommands::New { name, chip, dir } => {
                let dir = dir.unwrap_or_else(|| name.clone());
                if let Some(chip) = chip {
                    // 指定了 chip：非交互式创建
                    cmd_project::cmd_project_new(&dir, &name, &chip, &cli.format)
                } else {
                    // 未指定 chip：进入向导（prefill 项目名）
                    cmd_project_wizard::run_wizard(
                        cmd_project_wizard::WizardArgs {
                            project_name: Some(name),
                            project_dir: Some(dir),
                            model: None,
                            firmware_version: None,
                            template: None,
                            port: None,
                            soc_script: None,
                            no_git: false,
                            no_download: false,
                        },
                        &cli.format,
                    )
                }
            }
            ProjectCommands::Info { dir } => cmd_project::cmd_project_info(&dir, &cli.format),
            ProjectCommands::Config { dir, key, value } => cmd_project::cmd_project_config(&dir, key.as_deref(), value.as_deref(), &cli.format),
            ProjectCommands::Import { file, dir } => cmd_project::cmd_project_import(&file, &dir, &cli.format),
            ProjectCommands::Export { dir, output } => cmd_project::cmd_project_export(&dir, output.as_deref(), &cli.format),
            ProjectCommands::Deps { dir, reachable, unreachable } => cmd_project::cmd_project_deps(&dir, reachable, unreachable, &cli.format),
            ProjectCommands::Analyze { dir, soc } => cmd_project::cmd_project_analyze(&dir, soc.as_deref(), &cli.format),
            ProjectCommands::Build { dir } => cmd_project::cmd_project_build(&dir, &cli.format),
        },
        Commands::Build { action } => match action {
            BuildCommands::Luac { src, output, bitw } => cmd_build::cmd_build_luac(&src, &output, bitw, &cli.format),
            BuildCommands::Filesystem {
                src,
                output,
                luac,
                bitw,
                bkcrc,
                max_size_kb,
            } => cmd_build::cmd_build_filesystem(&src, &output, luac, bitw, bkcrc, max_size_kb, &cli.format),
        },
        Commands::Resource { action } => match action {
            ResourceCommands::List { module } => cmd_resource::cmd_resource_list(module.as_deref(), &cli.format),
            ResourceCommands::Download { category, sub, item, output } => cmd_resource::cmd_resource_download(&category, sub.as_deref(), item.as_deref(), &output, &cli.format),
        },
        Commands::Device { action } => match action {
            DeviceCommands::Reboot { port, chip } => cmd_device::cmd_device_reboot(port.as_deref(), chip.as_deref(), &cli.format),
            DeviceCommands::Boot { port, chip } => cmd_device::cmd_device_boot(port.as_deref(), chip.as_deref(), &cli.format),
        },
        Commands::Fota { action } => match action {
            FotaCommands::Build {
                new,
                old,
                output,
                fota_toolkit,
                soc_tools,
            } => cmd_fota::cmd_fota_build(&new, old.as_deref(), output.as_deref(), fota_toolkit.as_deref(), soc_tools.as_deref(), &cli.format),
        },
        Commands::Doctor { dir } => cmd_doctor::cmd_doctor(&dir, &cli.format),
        Commands::Version => {
            let version = env!("CARGO_PKG_VERSION");
            match cli.format {
                OutputFormat::Text => {
                    println!("luatos-cli v{version}");
                }
                OutputFormat::Json | OutputFormat::Jsonl => {
                    if let Err(err) = event::emit_result(
                        &cli.format,
                        "version",
                        "ok",
                        serde_json::json!({
                            "version": version,
                            "name": "luatos-cli",
                        }),
                    ) {
                        eprintln!("Error: {err:#}");
                        std::process::exit(1);
                    }
                }
            }
            Ok(())
        }
    };

    if let Err(e) = result {
        if let Err(render_err) = event::emit_error(&cli.format, None, &format!("{e:#}")) {
            eprintln!("Error: {render_err:#}");
        }
        std::process::exit(1);
    }
}
