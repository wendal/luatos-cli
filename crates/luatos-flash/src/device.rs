// 设备控制功能：重启与强制进入 bootloader 模式。
//
// 支持的芯片系列：
//   - bk72xx / air8101   : 5阶段 DTR+RTS 脉冲进入 boot；DTR 脉冲重启
//   - xt804 / air6208 / air101 / air103 / air601 : RTS+DTR 时序进入 boot；DTR 脉冲重启
//   - ec718 / ec7xx / air8000 / air780* : USB AT 口发送 AT+ECRST / DIAG boot 帧
//   - ccm4211 / air1601  : ISP 时序进入 boot；DTR+RTS 双信号复位
//   - 通用               : DTR 脉冲（最佳努力）

use std::time::Duration;

use anyhow::{Context, Result};

// ─── 内部辅助 ────────────────────────────────────────────────────────────────

/// 通用 DTR 脉冲重启（高→低 → 正常运行）。
///
/// 适用于大多数通过 DTR 连接 RESET 引脚的开发板。
fn dtr_pulse_reboot(port_name: &str) -> Result<()> {
    let mut port = serialport::new(port_name, 115200)
        .timeout(Duration::from_millis(200))
        .open()
        .with_context(|| format!("无法打开串口 {port_name}"))?;

    port.write_data_terminal_ready(true)?;
    port.write_request_to_send(false)?;
    std::thread::sleep(Duration::from_millis(100));
    port.write_data_terminal_ready(false)?;
    Ok(())
}

/// DTR+RTS 双信号复位（用于 ccm4211/air1601）。
fn dtr_rts_pulse_reboot(port_name: &str) -> Result<()> {
    let mut port = serialport::new(port_name, 115200)
        .timeout(Duration::from_millis(200))
        .open()
        .with_context(|| format!("无法打开串口 {port_name}"))?;

    port.write_data_terminal_ready(true)?;
    port.write_request_to_send(true)?;
    std::thread::sleep(Duration::from_millis(100));
    port.write_data_terminal_ready(false)?;
    port.write_request_to_send(false)?;
    Ok(())
}

/// XT804 系列进入 bootloader 的 DTR/RTS 时序。
///
/// 来自 wm_tool.c（RTS 复位模式）：
///   DTR=0, RTS=1 (50ms) → DTR=1, RTS=0 (50ms) → DTR=0
fn xt804_enter_boot(port_name: &str) -> Result<()> {
    let mut port = serialport::new(port_name, 115200)
        .timeout(Duration::from_millis(200))
        .open()
        .with_context(|| format!("无法打开串口 {port_name}"))?;

    // Phase 1: DTR=0（触发复位），RTS=1（进入 boot 模式）
    port.write_data_terminal_ready(false)?;
    port.write_request_to_send(true)?;
    std::thread::sleep(Duration::from_millis(50));

    // Phase 2: DTR=1（释放复位），RTS=0
    port.write_data_terminal_ready(true)?;
    port.write_request_to_send(false)?;
    std::thread::sleep(Duration::from_millis(50));

    // Phase 3: DTR=0（最终状态）
    port.write_data_terminal_ready(false)?;
    std::thread::sleep(Duration::from_millis(50));

    Ok(())
}

/// BK7258 系列进入 bootloader 的 5 阶段 DTR+RTS 时序（单次发送，不做握手确认）。
///
/// 时序：
///   (DTR=1,RTS=1, 50ms) → (0,0, 20ms) → (1,0, 50ms) → (0,1, 50ms) → (0,0)
fn bk7258_enter_boot(port_name: &str) -> Result<()> {
    let mut port = serialport::new(port_name, 115200)
        .timeout(Duration::from_millis(200))
        .open()
        .with_context(|| format!("无法打开串口 {port_name}"))?;

    port.write_data_terminal_ready(false)?;
    port.write_request_to_send(false)?;
    std::thread::sleep(Duration::from_millis(50));

    // Phase 1
    port.write_data_terminal_ready(true)?;
    port.write_request_to_send(true)?;
    std::thread::sleep(Duration::from_millis(50));

    // Phase 2
    port.write_data_terminal_ready(false)?;
    port.write_request_to_send(false)?;
    std::thread::sleep(Duration::from_millis(20));

    // Phase 3
    port.write_data_terminal_ready(true)?;
    port.write_request_to_send(false)?;
    std::thread::sleep(Duration::from_millis(50));

    // Phase 4: RTS=1（bootloader 模式）
    port.write_data_terminal_ready(false)?;
    port.write_request_to_send(true)?;
    std::thread::sleep(Duration::from_millis(50));

    // Phase 5: 释放
    port.write_data_terminal_ready(false)?;
    port.write_request_to_send(false)?;

    Ok(())
}

/// CCM4211/Air1601 进入 ISP 模式的 RTS+DTR 时序（单次发送，不做握手确认）。
///
/// ISP 入口：RTS=1,DTR=1（500ms）→ RTS=0,DTR=0
fn ccm4211_enter_boot(port_name: &str) -> Result<()> {
    let mut port = serialport::new(port_name, 9600)
        .timeout(Duration::from_millis(200))
        .open()
        .with_context(|| format!("无法打开串口 {port_name}"))?;

    port.write_request_to_send(true)?;
    port.write_data_terminal_ready(true)?;
    std::thread::sleep(Duration::from_millis(500));
    port.write_request_to_send(false)?;
    port.write_data_terminal_ready(false)?;
    Ok(())
}

/// EC718 系列重启：向 CMD 口发送 AT+ECRST。
///
/// port_name 为 None 时自动检测 VID=0x19D1 的 CMD 口。
fn ec718_reboot(port_name: Option<&str>) -> Result<()> {
    use std::io::Write;

    let port = match port_name {
        Some(p) => p.to_string(),
        None => crate::ec718::find_ec718_cmd_port().ok_or_else(|| anyhow::anyhow!("未找到 EC718 命令口（VID=0x19D1），请检查模组是否连接"))?,
    };

    let mut serial = serialport::new(&port, 115200)
        .timeout(Duration::from_millis(500))
        .open()
        .with_context(|| format!("无法打开串口 {port}"))?;

    serial.write_all(b"AT+ECRST\r\n")?;
    serial.flush()?;
    Ok(())
}

/// EC718 系列强制进入 boot 模式（复用现有的 try_reboot_to_download）。
///
/// port_name 为 None 时自动检测。
fn ec718_enter_boot(port_name: Option<&str>) -> Result<()> {
    use std::io::Write;

    let port = match port_name {
        Some(p) => p.to_string(),
        None => crate::ec718::find_ec718_cmd_port().ok_or_else(|| anyhow::anyhow!("未找到 EC718 命令口（VID=0x19D1），请检查模组是否连接"))?,
    };

    let mut serial = serialport::new(&port, 115200)
        .timeout(Duration::from_millis(500))
        .open()
        .with_context(|| format!("无法打开串口 {port}"))?;

    // AT+ECRST=delay,799 延迟重启
    serial.write_all(b"AT+ECRST=delay,799\r\n")?;
    serial.flush()?;
    std::thread::sleep(Duration::from_millis(200));

    // DIAG 帧：强制进入 boot 模式
    serial.write_all(b"\x7e\x00\x02\x7e")?;
    serial.flush()?;
    Ok(())
}

// ─── 公共 API ────────────────────────────────────────────────────────────────

/// 重启设备。
///
/// `port_name` 为串口名称（如 COM6 / /dev/ttyUSB0）。
/// EC718 系列可传 None 由函数自动检测 USB 命令口。
///
/// `chip` 为芯片类型字符串，可选；省略时使用通用 DTR 脉冲。
pub fn device_reboot(port_name: Option<&str>, chip: &str) -> Result<()> {
    match chip {
        "ec718" | "ec7xx" | "air8000" | "air780epm" | "air780ehm" | "air780ehv" | "air780ehg" | "air8000m" => ec718_reboot(port_name),
        "ccm4211" | "air1601" => {
            let port = port_name.ok_or_else(|| anyhow::anyhow!("ccm4211/air1601 需要指定 --port"))?;
            dtr_rts_pulse_reboot(port)
        }
        _ => {
            // bk72xx/air8101/xt804/air6208/air101/air103/air601 及通用：DTR 脉冲
            let port = port_name.ok_or_else(|| anyhow::anyhow!("请使用 --port 指定串口"))?;
            dtr_pulse_reboot(port)
        }
    }
}

/// 强制设备进入 bootloader 模式。
///
/// `port_name` 为串口名称。EC718 系列可传 None。
///
/// 只发送信号，不等待设备响应或确认。
pub fn device_enter_boot(port_name: Option<&str>, chip: &str) -> Result<()> {
    match chip {
        "bk72xx" | "air8101" => {
            let port = port_name.ok_or_else(|| anyhow::anyhow!("bk72xx/air8101 需要指定 --port"))?;
            bk7258_enter_boot(port)
        }
        "xt804" | "air6208" | "air101" | "air103" | "air601" => {
            let port = port_name.ok_or_else(|| anyhow::anyhow!("xt804 系列需要指定 --port"))?;
            xt804_enter_boot(port)
        }
        "ec718" | "ec7xx" | "air8000" | "air780epm" | "air780ehm" | "air780ehv" | "air780ehg" | "air8000m" => ec718_enter_boot(port_name),
        "ccm4211" | "air1601" => {
            let port = port_name.ok_or_else(|| anyhow::anyhow!("ccm4211/air1601 需要指定 --port"))?;
            ccm4211_enter_boot(port)
        }
        _ => {
            // 通用：DTR+RTS 双信号脉冲
            let port = port_name.ok_or_else(|| anyhow::anyhow!("请使用 --port 指定串口"))?;
            dtr_rts_pulse_reboot(port)
        }
    }
}
