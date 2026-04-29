// 应用状态 — 纯逻辑，不依赖 egui 渲染，便于单元测试。

use luatos_serial::PortInfo;

/// 刷机流程状态机。
#[derive(Debug, Clone, PartialEq)]
pub enum FlashStatus {
    /// 空闲，等待用户操作
    Idle,
    /// 刷机进行中
    Flashing,
    /// 刷机完成（ok=true 成功，ok=false 失败/取消）
    Done { ok: bool },
}

/// 应用全局状态（纯数据，可测试）。
pub struct AppState {
    /// 已选择的 SOC 文件路径
    pub soc_path: String,
    /// 解析出的芯片类型（如 "bk72xx"、"ec7xx"）
    pub chip_type: Option<String>,
    /// 固件版本字符串
    pub soc_version: Option<String>,
    /// SOC 文件解析错误提示
    pub soc_error: Option<String>,
    /// 当前可用串口列表
    pub ports: Vec<PortInfo>,
    /// 用户选择的串口名称
    pub selected_port: Option<String>,
    /// 当前刷机状态
    pub status: FlashStatus,
    /// 进度比例 [0.0, 1.0]
    pub progress: f32,
    /// 当前阶段描述
    pub stage: String,
    /// 日志行列表（最多 500 条）
    pub logs: Vec<String>,
    /// 是否需要自动滚动到日志底部
    pub scroll_to_bottom: bool,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            soc_path: String::new(),
            chip_type: None,
            soc_version: None,
            soc_error: None,
            ports: Vec::new(),
            selected_port: None,
            status: FlashStatus::Idle,
            progress: 0.0,
            stage: String::new(),
            logs: Vec::new(),
            scroll_to_bottom: false,
        }
    }
}

impl AppState {
    pub fn new() -> Self {
        let mut s = Self::default();
        s.refresh_ports();
        s
    }

    /// 判断指定芯片类型是否需要用户选择 UART 串口。
    ///
    /// EC718 系列（air780/air8000 等）通过 USB VID 自动检测，无需手动选串口。
    /// 其余所有芯片系列均需要 UART 串口。
    pub fn needs_uart_port(chip_type: &str) -> bool {
        !matches!(chip_type, "ec7xx" | "air8000" | "air780epm" | "air780ehm" | "air780ehv" | "air780ehg" | "air8000m")
    }

    /// 判断当前状态是否满足开始刷机的条件。
    pub fn can_start_flash(&self) -> bool {
        if self.soc_path.is_empty() || self.chip_type.is_none() {
            return false;
        }
        if self.status == FlashStatus::Flashing {
            return false;
        }
        if let Some(chip) = &self.chip_type {
            if Self::needs_uart_port(chip) && self.selected_port.is_none() {
                return false;
            }
        }
        true
    }

    /// 追加一条日志，超过 500 条时丢弃最旧的。
    pub fn push_log(&mut self, msg: &str) {
        const MAX_LINES: usize = 500;
        self.logs.push(msg.to_string());
        if self.logs.len() > MAX_LINES {
            self.logs.drain(..self.logs.len() - MAX_LINES);
        }
        self.scroll_to_bottom = true;
    }

    /// 重置刷机状态、进度和日志，保留 SOC 路径和串口选择。
    pub fn reset(&mut self) {
        self.status = FlashStatus::Idle;
        self.progress = 0.0;
        self.stage.clear();
        self.logs.clear();
        self.scroll_to_bottom = false;
    }

    /// 刷新串口列表。
    pub fn refresh_ports(&mut self) {
        self.ports = luatos_serial::list_ports();
        // 如果当前选中的串口已不在列表中，清除选择
        if let Some(sel) = &self.selected_port {
            if !self.ports.iter().any(|p| &p.port_name == sel) {
                self.selected_port = None;
            }
        }
    }

    /// 加载并解析 SOC 文件信息。
    pub fn load_soc_info(&mut self) {
        self.soc_error = None;
        self.chip_type = None;
        self.soc_version = None;

        if self.soc_path.is_empty() {
            return;
        }

        match luatos_soc::read_soc_info(&self.soc_path) {
            Ok(info) => {
                self.chip_type = Some(info.chip.chip_type);
                self.soc_version = info.rom.version_bsp;
            }
            Err(e) => {
                self.soc_error = Some(format!("读取 SOC 失败: {e}"));
            }
        }
    }
}

// ─── 单元测试 ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── needs_uart_port ───────────────────────────────────────────────────────

    #[test]
    fn needs_uart_port_ec718_returns_false() {
        assert!(!AppState::needs_uart_port("ec7xx"));
        assert!(!AppState::needs_uart_port("air8000"));
        assert!(!AppState::needs_uart_port("air780epm"));
        assert!(!AppState::needs_uart_port("air780ehm"));
        assert!(!AppState::needs_uart_port("air780ehv"));
        assert!(!AppState::needs_uart_port("air780ehg"));
        assert!(!AppState::needs_uart_port("air8000m"));
    }

    #[test]
    fn needs_uart_port_bk72xx_returns_true() {
        assert!(AppState::needs_uart_port("bk72xx"));
        assert!(AppState::needs_uart_port("air8101"));
    }

    #[test]
    fn needs_uart_port_xt804_returns_true() {
        assert!(AppState::needs_uart_port("air6208"));
        assert!(AppState::needs_uart_port("air101"));
        assert!(AppState::needs_uart_port("air103"));
        assert!(AppState::needs_uart_port("air601"));
    }

    #[test]
    fn needs_uart_port_ccm4211_returns_true() {
        assert!(AppState::needs_uart_port("ccm4211"));
        assert!(AppState::needs_uart_port("air1601"));
    }

    #[test]
    fn needs_uart_port_sf32_returns_true() {
        assert!(AppState::needs_uart_port("sf32lb58"));
    }

    #[test]
    fn needs_uart_port_unknown_returns_true() {
        // 未知芯片类型默认需要串口（安全兜底）
        assert!(AppState::needs_uart_port("unknown_chip"));
        assert!(AppState::needs_uart_port(""));
    }

    // ── can_start_flash ───────────────────────────────────────────────────────

    #[test]
    fn can_start_flash_no_soc_returns_false() {
        let state = AppState::default();
        assert!(!state.can_start_flash());
    }

    #[test]
    fn can_start_flash_uart_chip_no_port_returns_false() {
        let mut state = AppState::default();
        state.soc_path = "/path/to/fw.soc".to_string();
        state.chip_type = Some("bk72xx".to_string());
        // 没有选择串口
        assert!(!state.can_start_flash());
    }

    #[test]
    fn can_start_flash_ec718_no_port_returns_true() {
        let mut state = AppState::default();
        state.soc_path = "/path/to/fw.soc".to_string();
        state.chip_type = Some("ec7xx".to_string());
        // EC718 无需串口
        assert!(state.can_start_flash());
    }

    #[test]
    fn can_start_flash_ready_returns_true() {
        let mut state = AppState::default();
        state.soc_path = "/path/to/fw.soc".to_string();
        state.chip_type = Some("bk72xx".to_string());
        state.selected_port = Some("COM3".to_string());
        assert!(state.can_start_flash());
    }

    #[test]
    fn can_start_flash_while_flashing_returns_false() {
        let mut state = AppState::default();
        state.soc_path = "/path/to/fw.soc".to_string();
        state.chip_type = Some("bk72xx".to_string());
        state.selected_port = Some("COM3".to_string());
        state.status = FlashStatus::Flashing;
        assert!(!state.can_start_flash());
    }

    // ── push_log ──────────────────────────────────────────────────────────────

    #[test]
    fn push_log_trims_to_max_lines() {
        let mut state = AppState::default();
        for i in 0..600 {
            state.push_log(&format!("line {i}"));
        }
        assert_eq!(state.logs.len(), 500);
        // 应保留最新的 500 条
        assert_eq!(state.logs[0], "line 100");
        assert_eq!(state.logs[499], "line 599");
    }

    #[test]
    fn push_log_sets_scroll_to_bottom() {
        let mut state = AppState::default();
        assert!(!state.scroll_to_bottom);
        state.push_log("hello");
        assert!(state.scroll_to_bottom);
    }

    // ── reset ─────────────────────────────────────────────────────────────────

    #[test]
    fn reset_clears_progress() {
        let mut state = AppState::default();
        state.status = FlashStatus::Done { ok: true };
        state.progress = 0.85;
        state.stage = "完成".to_string();
        state.logs.push("some log".to_string());
        state.scroll_to_bottom = true;

        state.reset();

        assert_eq!(state.status, FlashStatus::Idle);
        assert_eq!(state.progress, 0.0);
        assert!(state.stage.is_empty());
        assert!(state.logs.is_empty());
        assert!(!state.scroll_to_bottom);
    }

    #[test]
    fn reset_preserves_soc_and_port() {
        let mut state = AppState::default();
        state.soc_path = "/fw.soc".to_string();
        state.chip_type = Some("bk72xx".to_string());
        state.selected_port = Some("COM1".to_string());
        state.status = FlashStatus::Done { ok: false };

        state.reset();

        // SOC 路径和串口选择不受 reset 影响
        assert_eq!(state.soc_path, "/fw.soc");
        assert_eq!(state.chip_type.as_deref(), Some("bk72xx"));
        assert_eq!(state.selected_port.as_deref(), Some("COM1"));
    }
}
