use crate::cmd_flash;
use crate::cmd_log;
use crate::reset_args::ResetArgs;
use crate::OutputFormat;

/// Pipeline: flash firmware, reset device, then view logs.
/// This combines the common debug workflow into a single command.
pub fn cmd_pipeline_flash_log(
    soc: &str,
    port: &str,
    baud: u32,
    probe: bool,
    smart: bool,
    save: Option<&str>,
    reset: &ResetArgs,
    format: &OutputFormat,
) -> anyhow::Result<()> {
    // Step 1: Flash
    eprintln!("[pipeline] [1/3] Flashing {} -> {}...", soc, port);

    // Read SOC info to determine chip type
    let info = luatos_soc::read_soc_info(soc)?;
    let chip = info.chip.chip_type.as_str();

    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let step = 10u8;

    match chip {
        "air6208" | "air101" | "air103" | "air601" => {
            let progress = cmd_flash::make_progress_callback(format, "pipeline.flash-log", step);
            luatos_flash::xt804::flash_xt804(soc, port, progress, cancel)?;
        }
        other => {
            anyhow::bail!(
                "Pipeline flash-log not yet supported for chip: {}. Use individual flash/log commands instead.",
                other
            );
        }
    }

    try_stdin();
    println!();

    // Step 2: Reset
    eprintln!("[pipeline] [2/3] Resetting device on {}...", port);
    reset.execute(port)?;

    // Step 3: Log
    eprintln!("[pipeline] [3/3] Starting log capture on {} @ {} bps...", port, baud);

    // Use binary log mode if probe is set, else text mode
    if probe || smart {
        // Reset already done above, don't reset again
        let no_reset = ResetArgs {
            rts_reset: false,
            rts_reset_ms: 0,
            boot_wait_ms: 0,
        };
        cmd_log::cmd_log_view_binary(port, baud, probe, save, smart, &no_reset, format)
    } else {
        let no_reset = ResetArgs {
            rts_reset: false,
            rts_reset_ms: 0,
            boot_wait_ms: 0,
        };
        cmd_log::cmd_log_view(port, baud, smart, &no_reset, format)
    }
}

fn try_stdin() {
    // help with Ctrl+C detection
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct TestPipelineCmd {
        #[arg(long)]
        soc: String,
        #[arg(long)]
        port: String,
        #[arg(long, default_value = "2000000")]
        baud: u32,
        #[arg(long)]
        probe: bool,
        #[arg(long)]
        smart: bool,
        #[arg(long)]
        save: Option<String>,
        #[command(flatten)]
        reset: ResetArgs,
    }

    #[test]
    fn test_pipeline_flash_log_defaults() {
        let cmd =
            TestPipelineCmd::try_parse_from(["test", "--soc", "firmware.soc", "--port", "COM6"])
                .unwrap();
        assert_eq!(cmd.soc, "firmware.soc");
        assert_eq!(cmd.port, "COM6");
        assert_eq!(cmd.baud, 2000000);
        assert!(!cmd.probe);
        assert!(!cmd.smart);
        assert!(cmd.save.is_none());
        assert!(!cmd.reset.rts_reset);
    }

    #[test]
    fn test_pipeline_flash_log_with_reset() {
        let cmd = TestPipelineCmd::try_parse_from([
            "test",
            "--soc",
            "firmware.soc",
            "--port",
            "COM6",
            "--rts-reset",
            "--rts-reset-ms",
            "200",
            "--boot-wait-ms",
            "1000",
        ])
        .unwrap();
        assert!(cmd.reset.rts_reset);
        assert_eq!(cmd.reset.rts_reset_ms, 200);
        assert_eq!(cmd.reset.boot_wait_ms, 1000);
    }

    #[test]
    fn test_pipeline_flash_log_with_probe_and_save() {
        let cmd = TestPipelineCmd::try_parse_from([
            "test",
            "--soc",
            "firmware.soc",
            "--port",
            "COM6",
            "--probe",
            "--save",
            "/tmp/logs",
        ])
        .unwrap();
        assert!(cmd.probe);
        assert_eq!(cmd.save, Some("/tmp/logs".to_string()));
    }

    #[test]
    fn test_pipeline_flash_log_with_smart() {
        let cmd = TestPipelineCmd::try_parse_from([
            "test",
            "--soc",
            "firmware.soc",
            "--port",
            "COM6",
            "--smart",
        ])
        .unwrap();
        assert!(cmd.smart);
    }
}
