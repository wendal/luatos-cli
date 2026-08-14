use clap::Args;

/// Shared reset arguments — can be flattened into any subcommand that needs
/// to reset the device via RTS before starting its main operation.
#[derive(Args, Debug, Clone, Default)]
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

/// SF32LB58 自动复位参数（可 flatten 到任意需要它的子命令）
#[derive(clap::Args, Debug, Clone)]
pub struct Sf32ResetArgs {
    /// 自动控制 DTR/RTS 进入/退出 ROM BL（适用于 CH340X 增强 DTR 改装硬件，仅 SF32LB58）
    #[arg(long)]
    pub auto_reset: bool,
    /// 进入 boot 时 DTR 的电平（high=BOOT0拉高，low=BOOT0拉低，默认 low）
    #[arg(long, value_enum, default_value = "low")]
    pub dtr_boot: crate::SignalLevel,
    /// 触发复位时 RTS 的电平（high=CH340X RTS#拉低=RESET有效，默认 high）
    #[arg(long, value_enum, default_value = "high")]
    pub sf32_rts_reset: crate::SignalLevel,
    /// 复位脉冲宽度（毫秒，默认 100）
    #[arg(long, default_value = "100")]
    pub reset_ms: u64,
    /// 进入 boot 后等待 ROM BL 初始化的时长（毫秒，默认 500）
    #[arg(long, default_value = "500")]
    pub sf32_boot_wait_ms: u64,
}

impl Sf32ResetArgs {
    /// 转换为 SF32 复位配置（auto_reset=false 时为 None）。
    pub fn to_reset_config(&self) -> Option<luatos_flash::sf32lb5x::Sf32ResetConfig> {
        if !self.auto_reset {
            return None;
        }
        Some(luatos_flash::sf32lb5x::Sf32ResetConfig {
            dtr_boot: self.dtr_boot.as_bool(),
            rts_reset: self.sf32_rts_reset.as_bool(),
            reset_ms: self.reset_ms,
            boot_wait_ms: self.sf32_boot_wait_ms,
            ..Default::default()
        })
    }
}

impl ResetArgs {
    /// Execute the RTS reset if --rts-reset is set.
    /// Opens the serial port, pulses RTS HIGH for rts_reset_ms, sets RTS LOW,
    /// closes the port, then waits boot_wait_ms for the device to boot.
    pub fn execute(&self, port: &str) -> anyhow::Result<()> {
        if !self.rts_reset {
            return Ok(());
        }
        log::info!("RTS reset {} ({}ms pulse)...", port, self.rts_reset_ms);
        luatos_serial::rts_reset_pulse(port, self.rts_reset_ms)?;
        log::info!("Reset done, waiting {}ms for device to boot...", self.boot_wait_ms);
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
        let cmd = TestCmd::try_parse_from(["test", "--port", "COM6", "--rts-reset", "--rts-reset-ms", "250", "--boot-wait-ms", "800"]).unwrap();
        assert!(cmd.reset.rts_reset);
        assert_eq!(cmd.reset.rts_reset_ms, 250);
        assert_eq!(cmd.reset.boot_wait_ms, 800);
    }

    #[test]
    fn test_rts_reset_missing_ms_uses_default() {
        let cmd = TestCmd::try_parse_from(["test", "--port", "COM6", "--rts-reset"]).unwrap();
        assert!(cmd.reset.rts_reset);
        assert_eq!(cmd.reset.rts_reset_ms, 100); // default
    }
}
