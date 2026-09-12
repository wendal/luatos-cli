// 设备控制功能：重启与强制进入 bootloader 模式。
//
// 支持的芯片系列：
//   - bk72xx / air8101   : 5阶段 DTR+RTS 脉冲进入 boot；UART 上 RTS+DTR 500ms 重启（对齐 LuaTools）
//   - xt804 / air6208 / air101 / air103 / air601 : RTS+DTR 时序进入 boot；UART 上 RTS+DTR 重启
//   - ec718 / ec7xx / air8000 / air780* : USB SOC 日志口 DIAG `7E 00 01 7E` + AT 兜底（`--port auto` 视为省略）
//   - ccm4211 / air1601 / air1602  : ISP 时序进入 boot；UART 上 RTS+DTR 复位
//   - sf32lb58           : ROM BL 需手动操作（MODE 引脚 + RESET），软件仅打印说明
//   - 通用               : UART 上 RTS+DTR 脉冲（最佳努力）

// 芯片分发基于 luatos_soc::ChipFamily（chip_type 字符串归一化的单一来源）。

use std::time::Duration;

use anyhow::{Context, Result};
use luatos_soc::ChipFamily;

// ─── 内部辅助 ────────────────────────────────────────────────────────────────

/// LuaTools 通用串口「重启模块」默认脉宽（`aio_uart_run`：RTS+DTR 同时拉 500ms）。
const UART_REBOOT_PULSE_MS: u64 = 500;

/// `--port auto` / 空字符串视为未指定，让 EC718 走 USB 命令口自动探测。
pub fn normalize_serial_port(port: Option<&str>) -> Option<&str> {
    let port = port.map(str::trim).filter(|s| !s.is_empty())?;
    if port.eq_ignore_ascii_case("auto") {
        None
    } else {
        Some(port)
    }
}

/// UART 硬件复位：RTS 与 DTR 同时有效，再同时释放。
///
/// 对齐 LuaTools `aio_uart_run`（许多 CH340 板 RESET 接 RTS#，只打 DTR 无效）。
fn uart_rts_dtr_reboot(port_name: &str) -> Result<()> {
    let mut port = serialport::new(port_name, 115200)
        .timeout(Duration::from_millis(200))
        .open()
        .with_context(|| format!("无法打开串口 {port_name}"))?;

    port.write_data_terminal_ready(true)?;
    port.write_request_to_send(true)?;
    std::thread::sleep(Duration::from_millis(UART_REBOOT_PULSE_MS));
    port.write_data_terminal_ready(false)?;
    port.write_request_to_send(false)?;
    Ok(())
}

/// 重启方式（纯选路，便于单测，不打开串口）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebootMethod {
    /// USB 命令口 `AT+ECRST`
    Ec718At,
    /// 指定 COM 上 RTS+DTR 脉冲
    UartRtsDtr,
}

pub fn select_reboot_method(chip: &str, _port: Option<&str>) -> RebootMethod {
    match ChipFamily::from_chip_type(chip) {
        ChipFamily::Ec718 => RebootMethod::Ec718At,
        _ => RebootMethod::UartRtsDtr,
    }
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

/// LuaTools USB SOC 日志口先发 cmd=0x00 打开打印通道，再发 cmd=0x01 重启。
const EC718_DIAG_HANDSHAKE: &[u8] = b"\x7e\x00\x00\x7e";
const EC718_DIAG_REBOOT: &[u8] = b"\x7e\x00\x01\x7e";

fn open_ec718_cdc(port: &str, baud: u32) -> Result<Box<dyn serialport::SerialPort>> {
    let mut serial = serialport::new(port, baud)
        .timeout(Duration::from_millis(500))
        .open()
        .with_context(|| format!("无法打开串口 {port}"))?;
    serial.write_data_terminal_ready(true)?;
    serial.write_request_to_send(true)?;
    std::thread::sleep(Duration::from_millis(80));
    Ok(serial)
}

/// 用户 AT 口（x.6）：LuaTools `aio_at_trace` 用 `AT+RESET`。
fn ec718_at_reboot(port: &str) -> Result<()> {
    use std::io::Write;
    let mut serial = open_ec718_cdc(port, 115200)?;
    serial.write_all(b"AT\r")?;
    serial.flush()?;
    std::thread::sleep(Duration::from_millis(50));
    serial.write_all(b"AT+RESET\r\n")?;
    serial.flush()?;
    std::thread::sleep(Duration::from_millis(80));
    serial.write_all(b"AT+ECRST\r\n")?;
    serial.flush()?;
    std::thread::sleep(Duration::from_millis(200));
    Ok(())
}

/// SOC 日志口（x.2）：先握手打开 0x7E 通道，再发重启帧，并保持句柄一段时间。
///
/// 真机验证（Air8000）：只对 x.6 写 `AT+RESET` 不会复位；对 x.2 发
/// `7E 00 00 7E` + `7E 00 01 7E`（DTR/RTS HIGH，921600）会 USB 重枚举。
fn ec718_diag_reboot(port: &str) -> Result<()> {
    use std::io::{Read, Write};
    let mut serial = open_ec718_cdc(port, 921600)?;
    serial.set_timeout(Duration::from_millis(50))?;
    serial.write_all(EC718_DIAG_HANDSHAKE)?;
    serial.flush()?;

    // 等打印通道出数（刚才成功那次握手后立刻读到 600+ 字节），再发重启帧。
    let deadline = std::time::Instant::now() + Duration::from_millis(300);
    let mut buf = [0u8; 512];
    let mut got = 0usize;
    while std::time::Instant::now() < deadline {
        match serial.read(&mut buf) {
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => break,
        }
        if got >= 64 {
            break;
        }
    }
    log::info!("EC718 DIAG handshake drained {got} bytes from {port}");

    serial.write_all(EC718_DIAG_REBOOT)?;
    serial.flush()?;
    std::thread::sleep(Duration::from_millis(1500));
    Ok(())
}

/// EC718 / Air8000 4G USB 重启。
///
/// 优先走 SOC 日志口 x.2 的 DIAG（LuaTools `aio_soc_usb_trace`）。
/// x.6 用户口在 LuatOS 上常常没有 modem AT 解析，写成功 ≠ 模组复位。
fn ec718_reboot(port_name: Option<&str>) -> Result<()> {
    let log_port = port_name
        .map(str::to_string)
        .or_else(crate::ec718::find_ec718_cmd_port)
        .or_else(crate::ec718::find_ec718_log_port);
    if let Some(port) = log_port {
        log::info!("EC718 USB reboot via SOC log/DIAG port {port}");
        return ec718_diag_reboot(&port);
    }

    if let Some(user) = crate::ec718::find_ec718_user_port() {
        log::warn!("未找到 EC718 日志口，回退用户口 {user} AT+RESET");
        return ec718_at_reboot(&user);
    }

    anyhow::bail!("未找到 EC718 USB 口（VID=0x19D1 日志口 x.2 / 用户口 x.6），请检查模组是否连接")
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

    serial.write_data_terminal_ready(true)?;
    serial.write_request_to_send(true)?;
    std::thread::sleep(Duration::from_millis(80));

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
/// `--port auto` 或空视为未指定。EC718 系列未指定端口时自动检测 USB 命令口。
///
/// `chip` 为芯片类型字符串，可选；省略时对指定 UART 做 RTS+DTR 脉冲。
pub fn device_reboot(port_name: Option<&str>, chip: &str) -> Result<()> {
    let port_name = normalize_serial_port(port_name);
    match select_reboot_method(chip, port_name) {
        RebootMethod::Ec718At => ec718_reboot(port_name),
        RebootMethod::UartRtsDtr => {
            let port = port_name.ok_or_else(|| anyhow::anyhow!("请使用 --port 指定串口"))?;
            uart_rts_dtr_reboot(port)
        }
    }
}

/// 强制设备进入 bootloader 模式。
///
/// `port_name` 为串口名称。`--port auto` 视为省略。EC718 系列可传 None。
///
/// 只发送信号，不等待设备响应或确认。
pub fn device_enter_boot(port_name: Option<&str>, chip: &str) -> Result<()> {
    let port_name = normalize_serial_port(port_name);
    match ChipFamily::from_chip_type(chip) {
        ChipFamily::Bk72xx => {
            let port = port_name.ok_or_else(|| anyhow::anyhow!("bk72xx/air8101 需要指定 --port"))?;
            bk7258_enter_boot(port)
        }
        ChipFamily::Xt804 => {
            let port = port_name.ok_or_else(|| anyhow::anyhow!("xt804 系列需要指定 --port"))?;
            xt804_enter_boot(port)
        }
        ChipFamily::Ec718 => ec718_enter_boot(port_name),
        ChipFamily::Ccm4211 => {
            let port = port_name.ok_or_else(|| anyhow::anyhow!("ccm4211/air1601/air1602 需要指定 --port"))?;
            ccm4211_enter_boot(port)
        }
        ChipFamily::Sf32lb58 => {
            // SF32LB58 ROM BL 进入需手动操作，无法通过软件触发
            eprintln!("SF32LB58 需要手动进入 ROM BL 模式：");
            eprintln!("  1. 短接 MODE 跳线（3-pin 排针）");
            eprintln!("  2. 按下 RESET 按键后松开");
            eprintln!("  3. 拔掉 MODE 短接帽");
            Ok(())
        }
        _ => {
            // Air6201/Unknown 及通用：与重启相同的 RTS+DTR 脉冲
            let port = port_name.ok_or_else(|| anyhow::anyhow!("请使用 --port 指定串口"))?;
            uart_rts_dtr_reboot(port)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_and_empty_port_normalize_to_none() {
        assert_eq!(normalize_serial_port(None), None);
        assert_eq!(normalize_serial_port(Some("")), None);
        assert_eq!(normalize_serial_port(Some("  ")), None);
        assert_eq!(normalize_serial_port(Some("auto")), None);
        assert_eq!(normalize_serial_port(Some("AUTO")), None);
        assert_eq!(normalize_serial_port(Some(" COM6 ")), Some("COM6"));
    }

    #[test]
    fn reboot_method_follows_chip_family() {
        assert_eq!(select_reboot_method("ec718", None), RebootMethod::Ec718At);
        assert_eq!(select_reboot_method("air8000", Some("auto")), RebootMethod::Ec718At);
        assert_eq!(select_reboot_method("air780epm", Some("COM8")), RebootMethod::Ec718At);
        assert_eq!(select_reboot_method("air8101", Some("COM6")), RebootMethod::UartRtsDtr);
        assert_eq!(select_reboot_method("bk72xx", Some("COM6")), RebootMethod::UartRtsDtr);
        assert_eq!(select_reboot_method("", Some("COM6")), RebootMethod::UartRtsDtr);
    }

    #[test]
    fn ec718_usb_soc_reboot_frame_is_diag_cmd1_not_boot() {
        assert_eq!(EC718_DIAG_HANDSHAKE, b"\x7e\x00\x00\x7e");
        assert_eq!(EC718_DIAG_REBOOT, b"\x7e\x00\x01\x7e");
        assert_ne!(EC718_DIAG_REBOOT, b"\x7e\x00\x02\x7e");
    }
}
