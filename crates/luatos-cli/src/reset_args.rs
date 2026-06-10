use clap::Args;

/// Shared reset arguments — can be flattened into any subcommand that needs
/// to reset the device via RTS before starting its main operation.
#[derive(Args, Debug, Clone)]
pub struct ResetArgs {
    /// Send RTS reset pulse before starting the operation
    #[arg(long)]
    pub rts_reset: bool,

    /// RTS reset pulse width in milliseconds
    #[arg(long, default_value = "100")]
    pub rts_reset_ms: u64,

    /// Wait time after reset for the device to boot (milliseconds)
    #[arg(long, default_value = "500")]
    pub boot_wait_ms: u64,
}

impl ResetArgs {
    /// Execute the RTS reset if --rts-reset is set.
    /// Opens the serial port, pulses RTS HIGH for rts_reset_ms, sets RTS LOW,
    /// closes the port, then waits boot_wait_ms for the device to boot.
    pub fn execute(&self, port: &str) -> anyhow::Result<()> {
        if !self.rts_reset {
            return Ok(());
        }
        log::info!(
            "RTS reset {} ({}ms pulse)...",
            port,
            self.rts_reset_ms
        );
        luatos_serial::rts_reset_pulse(port, self.rts_reset_ms)?;
        log::info!(
            "Reset done, waiting {}ms for device to boot...",
            self.boot_wait_ms
        );
        std::thread::sleep(std::time::Duration::from_millis(self.boot_wait_ms));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct TestCmd {
        #[command(flatten)]
        reset: ResetArgs,
        #[arg(long)]
        port: String,
    }

    #[test]
    fn test_defaults() {
        let cmd = TestCmd::try_parse_from(["test", "--port", "COM6"]).unwrap();
        assert!(!cmd.reset.rts_reset);
        assert_eq!(cmd.reset.rts_reset_ms, 100);
        assert_eq!(cmd.reset.boot_wait_ms, 500);
    }

    #[test]
    fn test_rts_reset_full() {
        let cmd = TestCmd::try_parse_from([
            "test",
            "--port",
            "COM6",
            "--rts-reset",
            "--rts-reset-ms",
            "250",
            "--boot-wait-ms",
            "800",
        ])
        .unwrap();
        assert!(cmd.reset.rts_reset);
        assert_eq!(cmd.reset.rts_reset_ms, 250);
        assert_eq!(cmd.reset.boot_wait_ms, 800);
    }

    #[test]
    fn test_rts_reset_missing_ms_uses_default() {
        let cmd =
            TestCmd::try_parse_from(["test", "--port", "COM6", "--rts-reset"]).unwrap();
        assert!(cmd.reset.rts_reset);
        assert_eq!(cmd.reset.rts_reset_ms, 100); // default
    }
}
