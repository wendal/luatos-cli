// LuatOS flash protocols.
//
// Currently supports:
//   - BK7258 (Air8101): native UART ISP protocol
//   - XT804 (Air6208/Air101): XMODEM-1K flash protocol
//   - CCM4211 (Air1601): ISP + SOC framed download protocol

pub mod bk7258;
pub mod ccm4211;
pub mod xt804;

use serde::Serialize;

/// Progress callback data for flash operations.
#[derive(Debug, Clone, Serialize)]
pub struct FlashProgress {
    pub stage: String,
    pub percent: f32,
    pub message: String,
    pub done: bool,
    pub error: bool,
}

impl FlashProgress {
    pub fn info(stage: &str, pct: f32, msg: &str) -> Self {
        Self {
            stage: stage.into(),
            percent: pct,
            message: msg.into(),
            done: false,
            error: false,
        }
    }

    pub fn done_ok(msg: &str) -> Self {
        Self {
            stage: "Done".into(),
            percent: 100.0,
            message: msg.into(),
            done: true,
            error: false,
        }
    }

    pub fn done_err(msg: &str) -> Self {
        Self {
            stage: "Error".into(),
            percent: 0.0,
            message: msg.into(),
            done: true,
            error: true,
        }
    }
}

/// Progress callback type — receives progress updates during flash operations.
pub type ProgressCallback = Box<dyn Fn(&FlashProgress) + Send>;
