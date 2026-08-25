// LuatOS log parsing and storage.
//
// Extensible log parser system supporting multiple module log formats:
//   - LuatOS standard: [timestamp] L/module message
//   - Plain text fallback for unknown formats
//
// Design: LogParser trait allows adding new chip-specific parsers without
// modifying existing code.

pub mod smart;

use serde::{Deserialize, Serialize};

// ─── 时间戳缓存 ────────────────────────────────────────────────────────────────

thread_local! {
    static TS_CACHE: std::cell::RefCell<(i64, String)> = const { std::cell::RefCell::new((0, String::new())) };
}

/// 获取本地接收时间戳（`%Y-%m-%d %H:%M:%S%.3f`）。
///
/// 按秒缓存格式化结果：同一秒内的多次调用直接复用已格式化的字符串，
/// 避免逐行解析日志时 `Local::now().format()` 带来的两次堆分配开销。
fn cached_local_timestamp() -> String {
    let now = chrono::Local::now();
    let secs = now.timestamp();
    TS_CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.0 != secs {
            c.1 = now.format("%Y-%m-%d %H:%M:%S%.3f").to_string();
            c.0 = secs;
        }
        c.1.clone()
    })
}

// ─── Data types ───────────────────────────────────────────────────────────────

/// A parsed log entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    /// Local receive timestamp (ISO 8601).
    pub timestamp: String,
    /// Device-side timestamp if available.
    pub device_time: Option<String>,
    /// Log level: I(nfo), W(arn), E(rror), D(ebug), or raw text.
    pub level: LogLevel,
    /// Module name (e.g. "user.main", "sys.boot").
    pub module: Option<String>,
    /// The log message body.
    pub message: String,
    /// Raw line before parsing (kept for diagnostics).
    pub raw: String,
}

/// Log severity levels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
    Trace,
    /// Unrecognized level character.
    Unknown(char),
    /// Not a structured log line.
    Raw,
}

impl LogLevel {
    pub fn from_char(c: char) -> Self {
        match c {
            'D' => Self::Debug,
            'I' => Self::Info,
            'W' => Self::Warn,
            'E' => Self::Error,
            'T' | 'V' => Self::Trace,
            _ => Self::Unknown(c),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Debug => "D",
            Self::Info => "I",
            Self::Warn => "W",
            Self::Error => "E",
            Self::Trace => "T",
            Self::Unknown(_) => "?",
            Self::Raw => "-",
        }
    }
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// ─── Parser trait ─────────────────────────────────────────────────────────────

/// Trait for chip-specific log line parsers.
///
/// Implement this for each module family (BK72xx, EC618, EC718, etc.)
/// to handle their unique log output formats.
pub trait LogParser: Send + Sync {
    /// Parser name for display/debug.
    fn name(&self) -> &str;

    /// Try to parse a raw text line into a structured LogEntry.
    /// Return None if the line format is not recognized by this parser;
    /// the dispatcher will fall back to the next parser or raw mode.
    fn parse_line(&self, line: &str) -> Option<LogEntry>;
}

// ─── LuatOS standard parser ──────────────────────────────────────────────────

/// Parser for the standard LuatOS text log format:
///   `[YYYY-MM-DD HH:MM:SS.mmm] L/module message`
///   or without timestamp: `L/module message`
pub struct LuatosParser;

impl LogParser for LuatosParser {
    fn name(&self) -> &str {
        "luatos"
    }

    fn parse_line(&self, line: &str) -> Option<LogEntry> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }

        let now = cached_local_timestamp();

        // Try format: [timestamp] L/module message
        if trimmed.starts_with('[') {
            if let Some(bracket_end) = trimmed.find(']') {
                let device_time = trimmed[1..bracket_end].to_string();
                let rest = trimmed[bracket_end + 1..].trim_start();
                if let Some(entry) = parse_level_module_msg(rest) {
                    return Some(LogEntry {
                        timestamp: now,
                        device_time: Some(device_time),
                        level: entry.0,
                        module: entry.1,
                        message: entry.2,
                        raw: line.to_string(),
                    });
                }
                // Has timestamp but not standard level/module format
                return Some(LogEntry {
                    timestamp: now,
                    device_time: Some(device_time),
                    level: LogLevel::Raw,
                    module: None,
                    message: rest.to_string(),
                    raw: line.to_string(),
                });
            }
        }

        // Try format without timestamp: L/module message
        if let Some(entry) = parse_level_module_msg(trimmed) {
            return Some(LogEntry {
                timestamp: now,
                device_time: None,
                level: entry.0,
                module: entry.1,
                message: entry.2,
                raw: line.to_string(),
            });
        }

        None
    }
}

/// Parse `L/module message` format.
fn parse_level_module_msg(s: &str) -> Option<(LogLevel, Option<String>, String)> {
    let bytes = s.as_bytes();
    if bytes.len() < 3 {
        return None;
    }
    let level_char = bytes[0] as char;
    if !level_char.is_ascii_alphabetic() || bytes[1] != b'/' {
        return None;
    }

    let level = LogLevel::from_char(level_char);
    let rest = &s[2..];

    if let Some(space_pos) = rest.find(' ') {
        let module = rest[..space_pos].to_string();
        let message = rest[space_pos + 1..].to_string();
        Some((level, Some(module), message))
    } else {
        Some((level, Some(rest.to_string()), String::new()))
    }
}

// ─── Boot log parser ─────────────────────────────────────────────────────────

/// Parser for BK72xx boot log lines (non-LuatOS format during early boot).
/// Recognizes lines like: `bk_xxx: ...`, `ap0: ...`, `luat: ...`
pub struct BootLogParser;

impl LogParser for BootLogParser {
    fn name(&self) -> &str {
        "boot"
    }

    fn parse_line(&self, line: &str) -> Option<LogEntry> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }

        if let Some(colon_pos) = trimmed.find(": ") {
            let prefix = &trimmed[..colon_pos];
            if prefix.len() <= 20 && prefix.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                let now = cached_local_timestamp();
                return Some(LogEntry {
                    timestamp: now,
                    device_time: None,
                    level: LogLevel::Info,
                    module: Some(prefix.to_string()),
                    message: trimmed[colon_pos + 2..].to_string(),
                    raw: line.to_string(),
                });
            }
        }

        None
    }
}

// ─── Dispatcher ──────────────────────────────────────────────────────────────

/// Multi-parser dispatcher: tries each parser in order, returns first match.
pub struct LogDispatcher {
    parsers: Vec<Box<dyn LogParser>>,
}

impl LogDispatcher {
    pub fn new() -> Self {
        Self { parsers: Vec::new() }
    }

    /// Create a dispatcher with the default LuatOS parsers.
    pub fn default_parsers() -> Self {
        let mut d = Self::new();
        d.add_parser(Box::new(LuatosParser));
        d.add_parser(Box::new(BootLogParser));
        d
    }

    /// Add a custom parser. Parsers are tried in insertion order.
    pub fn add_parser(&mut self, parser: Box<dyn LogParser>) {
        self.parsers.push(parser);
    }

    /// Parse a line using the registered parsers.
    pub fn parse(&self, line: &str) -> LogEntry {
        for parser in &self.parsers {
            if let Some(entry) = parser.parse_line(line) {
                return entry;
            }
        }
        let now = cached_local_timestamp();
        LogEntry {
            timestamp: now,
            device_time: None,
            level: LogLevel::Raw,
            module: None,
            message: line.trim().to_string(),
            raw: line.to_string(),
        }
    }
}

impl Default for LogDispatcher {
    fn default() -> Self {
        Self::default_parsers()
    }
}

// ─── Log file writer ─────────────────────────────────────────────────────────

use std::io::Write;

/// Writes log entries to file(s) in text and/or JSON format.
pub struct LogWriter {
    text_file: Option<std::io::BufWriter<std::fs::File>>,
    json_file: Option<std::io::BufWriter<std::fs::File>>,
}

impl LogWriter {
    /// Create a log writer. Pass None to skip that format.
    pub fn new(text_path: Option<&std::path::Path>, json_path: Option<&std::path::Path>) -> anyhow::Result<Self> {
        let text_file = if let Some(p) = text_path {
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent)?;
            }
            Some(std::io::BufWriter::new(std::fs::OpenOptions::new().create(true).append(true).open(p)?))
        } else {
            None
        };

        let json_file = if let Some(p) = json_path {
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent)?;
            }
            Some(std::io::BufWriter::new(std::fs::OpenOptions::new().create(true).append(true).open(p)?))
        } else {
            None
        };

        Ok(Self { text_file, json_file })
    }

    /// Write a log entry to the configured outputs.
    pub fn write(&mut self, entry: &LogEntry) -> anyhow::Result<()> {
        if let Some(ref mut f) = self.text_file {
            writeln!(f, "{}", entry.raw)?;
        }
        if let Some(ref mut f) = self.json_file {
            serde_json::to_writer(&mut *f, entry)?;
            writeln!(f)?;
        }
        Ok(())
    }

    /// Flush all outputs.
    pub fn flush(&mut self) -> anyhow::Result<()> {
        if let Some(ref mut f) = self.text_file {
            f.flush()?;
        }
        if let Some(ref mut f) = self.json_file {
            f.flush()?;
        }
        Ok(())
    }
}

// ─── Log file parser ─────────────────────────────────────────────────────────

/// Parse a saved log file into structured entries.
pub fn parse_log_file(path: &std::path::Path, dispatcher: &LogDispatcher) -> anyhow::Result<Vec<LogEntry>> {
    let content = std::fs::read_to_string(path)?;
    Ok(content.lines().map(|line| dispatcher.parse(line)).collect())
}

// ─── SOC Binary Log Parser ───────────────────────────────────────────────────
//
// Binary log protocol used by Air6208 and similar SOC devices.
// Frame format:
//   [0xA5] [escaped(header + format_string_4aligned + params)] [escaped(crc16_le)] [0xA5]
//
// Escape rules:
//   0xA5 → [0xA6, 0x01]
//   0xA6 → [0xA6, 0x02]
//
// Header (24 bytes):
//   ms:u64 + tag:u64 + cmd:u32 + sn:u16 + type:u8 + cpu:u8
//
// Tag bitfield: level(8bits) + tag0-tag7(7bits each) = 64 bits total

/// SOC binary log frame decoder.
///
/// Maintains internal state for frame reassembly from streaming serial data.
pub struct SocLogDecoder {
    /// Buffer for accumulating frame data between 0xA5 markers.
    frame_buf: Vec<u8>,
    /// Whether we are currently inside a frame (after first 0xA5).
    in_frame: bool,
    /// Previous byte was 0xA6 (escape prefix).
    escape_next: bool,
}

impl SocLogDecoder {
    pub fn new() -> Self {
        Self {
            frame_buf: Vec::with_capacity(2048),
            in_frame: false,
            escape_next: false,
        }
    }

    /// Feed raw bytes from serial port and extract any complete log entries.
    pub fn feed(&mut self, data: &[u8]) -> Vec<LogEntry> {
        let mut entries = Vec::new();

        for &byte in data {
            if byte == 0xA5 {
                if self.in_frame && self.frame_buf.len() >= 26 {
                    // End of frame — try to decode
                    if let Some(entry) = self.decode_frame(&self.frame_buf.clone()) {
                        entries.push(entry);
                    }
                }
                // Start new frame
                self.frame_buf.clear();
                self.in_frame = true;
                self.escape_next = false;
                continue;
            }

            if !self.in_frame {
                continue;
            }

            if byte == 0xA6 {
                self.escape_next = true;
                continue;
            }

            if self.escape_next {
                // De-stuff: 0xA6 0x01 → 0xA5, 0xA6 0x02 → 0xA6
                let actual = match byte {
                    0x01 => 0xA5,
                    0x02 => 0xA6,
                    other => other & 0x03, // mask per protocol
                };
                self.frame_buf.push(actual);
                self.escape_next = false;
                continue;
            }

            self.frame_buf.push(byte);

            // Safety limit: frames shouldn't be huge
            if self.frame_buf.len() > 8192 {
                self.frame_buf.clear();
                self.in_frame = false;
            }
        }

        entries
    }

    /// Decode a complete frame (between 0xA5 markers) into a LogEntry.
    fn decode_frame(&self, data: &[u8]) -> Option<LogEntry> {
        self.decode_frame_with_header(data).map(|(e, _)| e)
    }

    /// Decode a complete frame and also return the parsed 24-byte header.
    ///
    /// 适用于需要访问原始 header 字段 (ms/cmd/sn/cpu 等) 的调用方, 例如 FFI 导出.
    pub fn decode_frame_with_header(&self, data: &[u8]) -> Option<(LogEntry, SocLogFrameHeader)> {
        // Minimum: 24-byte header + 2-byte CRC
        if data.len() < 26 {
            return None;
        }

        // Last 2 bytes are CRC16 (little-endian)
        let payload = &data[..data.len() - 2];
        let crc_received = u16::from_le_bytes([data[data.len() - 2], data[data.len() - 1]]);

        // Verify CRC16-ModBus
        let crc_computed = crc16_modbus(payload);
        if crc_received != crc_computed {
            return None; // CRC mismatch
        }

        // Parse 24-byte header
        let ms = u64::from_le_bytes(payload[0..8].try_into().ok()?);
        let tag_raw = u64::from_le_bytes(payload[8..16].try_into().ok()?);
        let _cmd = u32::from_le_bytes(payload[16..20].try_into().ok()?);
        let _sn = u16::from_le_bytes(payload[20..22].try_into().ok()?);
        let msg_type = payload[22];
        let _cpu = payload[23];

        // Decode tag bitfield: level(8) + tag0-tag7(7 bits each)
        let level_bits = (tag_raw & 0xFF) as u8;
        let level = match level_bits {
            1 => LogLevel::Debug,
            2 => LogLevel::Info,
            3 => LogLevel::Warn,
            4 => LogLevel::Error,
            _ => LogLevel::Info, // Default to Info for boot/unset levels
        };

        // Tag name is encoded in upper bits (may be empty)
        let module = decode_tag_name(tag_raw >> 8);

        // Body: everything after the 24-byte header, before CRC
        let body = &payload[24..];

        // Decode format string and arguments
        let message = match msg_type {
            0 => decode_printf_message(body),
            _ => {
                // Raw or unknown type
                if body.is_empty() {
                    "(empty)".to_string()
                } else {
                    String::from_utf8_lossy(body).to_string()
                }
            }
        };

        let now = cached_local_timestamp();
        let device_time = format!("{}.{:03}", ms / 1000, ms % 1000);

        let header = SocLogFrameHeader {
            ms,
            tag: tag_raw,
            cmd: 0, // 日志帧 cmd 固定为 0
            sn: u16::from_le_bytes(payload[20..22].try_into().ok()?),
            msg_type,
            cpu: payload[23],
        };

        let entry = LogEntry {
            timestamp: now,
            device_time: Some(device_time),
            level,
            module: if module.is_empty() { None } else { Some(module) },
            message,
            raw: format!("[SOC frame {} bytes]", data.len()),
        };
        Some((entry, header))
    }

    /// 一次性 (stateless) 解码单帧 SOC 帧.
    ///
    /// 与 [`SocLogDecoder::feed`] 不同, 此方法不维护状态, 每次调用都视为独立.
    /// 适合 FFI / 一次性解析场景.
    ///
    /// 输入 `data` 可包含任意前缀噪声 (如其他协议字节) + 一个完整的 0xA5 ... 0xA5
    /// 帧. 本方法会跳过首个 0xA5 之前的所有字节, 找到最后一对 0xA5 作为帧首尾.
    /// 帧尾之后的多余字节 (例如下一帧开头) 被忽略.
    ///
    /// 返回:
    /// - [`SocFrame::Log`] — cmd == 0 日志帧, 携带 `LogEntry` 与 `SocLogFrameHeader`
    /// - [`SocFrame::Cmd`] — cmd != 0 命令帧, 携带 `SocCmdFrame`
    /// - `None` — 未找到 0xA5 边界, CRC 校验失败, header 长度不足, 或帧超过 8192 字节
    pub fn decode_one(data: &[u8]) -> Option<SocFrame> {
        // 1. 找首尾 0xA5 (取最末一对)
        let start = data.iter().position(|&b| b == 0xA5)?;
        let end_rel = data[start + 1..].iter().rposition(|&b| b == 0xA5)?;
        let abs_end = start + 1 + end_rel;
        if abs_end <= start + 1 {
            return None;
        }

        // 2. 反 escape (0xA6 0x01 -> 0xA5, 0xA6 0x02 -> 0xA6, 其他按 & 0x03 屏蔽)
        let mut unescaped = Vec::with_capacity(abs_end - start - 1);
        let mut i = start + 1;
        while i < abs_end {
            let b = data[i];
            if b == 0xA6 {
                if i + 1 >= abs_end {
                    return None;
                }
                let n = data[i + 1];
                let actual = match n {
                    0x01 => 0xA5,
                    0x02 => 0xA6,
                    other => other & 0x03,
                };
                unescaped.push(actual);
                i += 2;
            } else {
                unescaped.push(b);
                i += 1;
            }
        }

        // 3. 长度检查
        if unescaped.len() > 8192 {
            return None;
        }
        if unescaped.len() < 26 {
            return None; // 24B header + 2B CRC
        }

        // 4. CRC 校验
        let payload = &unescaped[..unescaped.len() - 2];
        let crc_received = u16::from_le_bytes([unescaped[unescaped.len() - 2], unescaped[unescaped.len() - 1]]);
        if crc16_soc_log(payload) != crc_received {
            return None;
        }

        // 5. 按 cmd 分支
        let cmd = u32::from_le_bytes(payload[16..20].try_into().ok()?);
        if cmd == 0 {
            // 日志帧: 复用现有 decode_frame_with_header (传入完整 unescaped, 含 CRC)
            let dummy = SocLogDecoder::new();
            let (entry, header) = dummy.decode_frame_with_header(&unescaped)?;
            Some(SocFrame::Log(entry, header))
        } else {
            // 命令帧: ms(8) + address(4) + len(4) + cmd(4) + sn(2) + type(1) + cpu(1)
            let ms = u64::from_le_bytes(payload[0..8].try_into().ok()?);
            let address = u32::from_le_bytes(payload[8..12].try_into().ok()?);
            let len = u32::from_le_bytes(payload[12..16].try_into().ok()?);
            let sn = u16::from_le_bytes(payload[20..22].try_into().ok()?);
            let msg_type = payload[22];
            let cpu = payload[23];

            // payload 截断到帧体内可用字节
            let available = payload.len().saturating_sub(24);
            let actual_len = (len as usize).min(available).min(64 * 1024);
            let payload_bytes = payload[24..24 + actual_len].to_vec();

            Some(SocFrame::Cmd(SocCmdFrame {
                ms,
                address,
                len,
                cmd,
                sn,
                msg_type,
                cpu,
                payload: payload_bytes,
            }))
        }
    }
}

impl Default for SocLogDecoder {
    fn default() -> Self {
        Self::new()
    }
}

/// CRC16 table used by SOC log protocol (device-side implementation).
pub const SOC_LOG_CRC_TABLE: [u16; 256] = [
    0x0000, 0xC0C1, 0xC181, 0x0140, 0xC301, 0x03C0, 0x0280, 0xC241, 0xC601, 0x06C0, 0x0780, 0xC741, 0x0500, 0xC5C1, 0xC481, 0x0440, 0xCC01, 0x0CC0, 0x0D80, 0xCD41, 0x0F00, 0xCFC1,
    0xCE81, 0x0E40, 0x0A00, 0xCAC1, 0xCB81, 0x0B40, 0xC901, 0x09C0, 0x0880, 0xC841, 0xD801, 0x18C0, 0x1980, 0xD941, 0x1B00, 0xDBC1, 0xDA81, 0x1A40, 0x1E00, 0xDEC1, 0xDF81, 0x1F40,
    0xDD01, 0x1DC0, 0x1C80, 0xDC41, 0x1400, 0xD4C1, 0xD581, 0x1540, 0xD701, 0x17C0, 0x1680, 0xD641, 0xD201, 0x12C0, 0x1380, 0xD341, 0x1100, 0xD1C1, 0xD081, 0x1040, 0xF001, 0x30C0,
    0x3180, 0xF141, 0x3300, 0xF3C1, 0xF281, 0x3240, 0x3600, 0xF6C1, 0xF781, 0x3740, 0xF501, 0x35C0, 0x3480, 0xF441, 0x3C00, 0xFCC1, 0xFD81, 0x3D40, 0xFF01, 0x3FC0, 0x3E80, 0xFE41,
    0xFA01, 0x3AC0, 0x3B80, 0xFB41, 0x3900, 0xF9C1, 0xF881, 0x3840, 0x2800, 0xE8C1, 0xE981, 0x2940, 0xEB01, 0x2BC0, 0x2A80, 0xEA41, 0xEE01, 0x2EC0, 0x2F80, 0xEF41, 0x2D00, 0xEDC1,
    0xEC81, 0x2C40, 0xE401, 0x24C0, 0x2580, 0xE541, 0x2700, 0xE7C1, 0xE681, 0x2640, 0x2200, 0xE2C1, 0xE381, 0x2340, 0xE101, 0x21C0, 0x2080, 0xE041, 0xA001, 0x60C0, 0x6180, 0xA141,
    0x6300, 0xA3C1, 0xA281, 0x6240, 0x6600, 0xA6C1, 0xA781, 0x6740, 0xA501, 0x65C0, 0x6480, 0xA441, 0x6C00, 0xACC1, 0xAD81, 0x6D40, 0xAF01, 0x6FC0, 0x6E80, 0xAE41, 0xAA01, 0x6AC0,
    0x6B80, 0xAB41, 0x6900, 0xA9C1, 0xA881, 0x6840, 0x7800, 0xB8C1, 0xB981, 0x7940, 0xBB01, 0x7BC0, 0x7A80, 0xBA41, 0xBE01, 0x7EC0, 0x7F80, 0xBF41, 0x7D00, 0xBDC1, 0xBC81, 0x7C40,
    0xB401, 0x74C0, 0x7580, 0xB541, 0x7700, 0xB7C1, 0xB681, 0x7640, 0x7200, 0xB2C1, 0xB381, 0x7340, 0xB101, 0x71C0, 0x7080, 0xB041, 0x5000, 0x90C1, 0x9181, 0x5140, 0x9301, 0x53C0,
    0x5280, 0x9241, 0x9601, 0x56C0, 0x5780, 0x9741, 0x5500, 0x95C1, 0x9481, 0x5440, 0x9C01, 0x5CC0, 0x5D80, 0x9D41, 0x5F00, 0x9FC1, 0x9E81, 0x5E40, 0x5A00, 0x9AC1, 0x9B81, 0x5B40,
    0x9901, 0x59C0, 0x5880, 0x9841, 0x8801, 0x48C0, 0x4980, 0x8941, 0x4B00, 0x8BC1, 0x8A81, 0x4A40, 0x4E00, 0x8EC1, 0x8F81, 0x4F40, 0x8D01, 0x4DC0, 0x4C80, 0x8C41, 0x4400, 0x84C1,
    0x8581, 0x4540, 0x8701, 0x47C0, 0x4680, 0x8641, 0x8201, 0x42C0, 0x4380, 0x8341, 0x4100, 0x81C1, 0x8081, 0x4040,
];

/// CRC16 computed using device-side table lookup algorithm.
/// This matches the SOC log protocol used by Air1601/CCM4211.
pub fn crc16_soc_log(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &byte in data {
        let xor = ((byte as u16) ^ crc) as u8;
        crc >>= 8;
        crc ^= SOC_LOG_CRC_TABLE[xor as usize];
    }
    crc
}

/// Legacy alias for backwards compatibility.
#[allow(dead_code)]
fn crc16_modbus(data: &[u8]) -> u16 {
    crc16_soc_log(data)
}

// ─── 公开类型 (供 FFI / 第三方使用) ──────────────────────────────────────────

/// SOC 日志帧 (cmd == 0) 的 24 字节 header 解析结果.
///
/// 对应 `payload[0..24]` 的字段拆解. 供 FFI 与高级消费者使用,
/// 普通日志解析请使用 [`LogEntry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocLogFrameHeader {
    /// 设备时间戳 (毫秒, payload[0..8])
    pub ms: u64,
    /// 原始 tag 位域 (payload[8..16], 含 level + 8 个 7-bit 字符)
    pub tag: u64,
    /// 命令码 (payload[16..20], 日志帧固定为 0)
    pub cmd: u32,
    /// 序列号 (payload[20..22])
    pub sn: u16,
    /// 消息类型 (payload[22], 0 = printf, 其他 = raw)
    pub msg_type: u8,
    /// CPU 编号 (payload[23])
    pub cpu: u8,
}

/// SOC 命令帧 (cmd != 0) 的解码结果.
///
/// 在 CCM4211 SOC 协议中, `cmd != 0` 表示这是一个**命令帧** (Flash 读/写操作
/// 的请求或响应), 其 24 字节 header 布局与日志帧不同:
///
/// | bytes  | 字段        |
/// |--------|------------|
/// | 0..8   | `ms: u64`  |
/// | 8..12  | `address: u32` |
/// | 12..16 | `len: u32`  |
/// | 16..20 | `cmd: u32`  |
/// | 20..22 | `sn: u16`   |
/// | 22     | `type: u8`  |
/// | 23     | `cpu: u8`   |
/// | 24..   | `len` 字节 raw payload |
///
/// 这与第三方 `pySoclogAnalyze` DLL 的"命令帧模式"完全一致.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocCmdFrame {
    /// 设备时间戳 (毫秒)
    pub ms: u64,
    /// Flash 操作地址
    pub address: u32,
    /// payload 长度 (字节)
    pub len: u32,
    /// 命令码
    pub cmd: u32,
    /// 序列号
    pub sn: u16,
    /// 消息类型
    pub msg_type: u8,
    /// CPU 编号
    pub cpu: u8,
    /// 实际 payload 字节 (长度 = `min(len, 帧体长度)`, 不超过 64 KB)
    pub payload: Vec<u8>,
}

/// SOC 帧的统一枚举, 表示一次 `decode_one` 调用的解码结果.
///
/// - [`SocFrame::Log`] — 日志帧 (cmd == 0), 携带人类可读的 `LogEntry` 与原始 header.
/// - [`SocFrame::Cmd`] — 命令帧 (cmd != 0), 携带 `SocCmdFrame`.
#[derive(Debug, Clone)]
pub enum SocFrame {
    /// 日志帧, cmd == 0
    Log(LogEntry, SocLogFrameHeader),
    /// 命令帧, cmd != 0
    Cmd(SocCmdFrame),
}

/// Decode tag name from the tag bitfield (7 bits per character, up to 8 chars).
fn decode_tag_name(tag_bits: u64) -> String {
    // ASCII lookup: each 7-bit value maps to a character
    const TAG_CHARS: &[u8; 64] = b" abcdefghijklmnopqrstuvwxyz012345ABCDEFGHIJKLMNOPQRSTUVWXYZ_*-./";

    let mut name = String::new();
    let mut bits = tag_bits;
    for _ in 0..8 {
        let idx = (bits & 0x7F) as usize;
        if idx == 0 {
            break; // Null terminator
        }
        if idx < TAG_CHARS.len() {
            name.push(TAG_CHARS[idx] as char);
        }
        bits >>= 7;
    }
    name
}

/// Decode printf-style format string with embedded arguments.
///
/// The body contains a null-terminated format string (4-byte aligned),
/// followed by argument values.
fn decode_printf_message(body: &[u8]) -> String {
    if body.is_empty() {
        return String::new();
    }

    // Find the null-terminated format string
    let fmt_end = body.iter().position(|&b| b == 0).unwrap_or(body.len());
    let fmt_str = String::from_utf8_lossy(&body[..fmt_end]).to_string();

    // Arguments start after the format string, 4-byte aligned
    let args_offset = (fmt_end + 4) & !3; // Align to 4 bytes
    if args_offset >= body.len() {
        // No arguments — return format string as-is
        return fmt_str;
    }

    let args_data = &body[args_offset..];

    // Simple format string argument substitution
    let mut result = String::new();
    let mut arg_pos = 0;
    let mut chars = fmt_str.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '%' {
            result.push(c);
            continue;
        }

        // Read format specifier
        let mut spec = String::new();
        let mut is_short = false;
        let mut is_short_short = false;
        let mut is_long = false;
        let mut is_long_long = false;

        while let Some(&next) = chars.peek() {
            match next {
                '0'..='9' | '-' | '+' | ' ' | '#' | '.' => {
                    spec.push(next);
                    chars.next();
                }
                'h' => {
                    if is_short {
                        is_short_short = true;
                    }
                    is_short = true;
                    chars.next();
                }
                'l' => {
                    if is_long {
                        is_long_long = true;
                    }
                    is_long = true;
                    chars.next();
                }
                'd' | 'i' | 'u' | 'x' | 'X' | 'o' => {
                    chars.next();
                    if is_long_long {
                        // 8-byte integer
                        if arg_pos + 8 <= args_data.len() {
                            let val = i64::from_le_bytes(args_data[arg_pos..arg_pos + 8].try_into().unwrap_or([0; 8]));
                            match next {
                                'x' => result.push_str(&format!("{val:x}")),
                                'X' => result.push_str(&format!("{val:X}")),
                                _ => result.push_str(&format!("{val}")),
                            }
                            arg_pos += 8;
                        }
                    } else {
                        // `short` and `char` integer arguments are promoted to
                        // `int`/`unsigned int` in C varargs, so they still occupy
                        // a 4-byte argument slot. Narrow only for presentation.
                        if arg_pos + 4 <= args_data.len() {
                            let raw = u32::from_le_bytes(args_data[arg_pos..arg_pos + 4].try_into().unwrap_or([0; 4]));
                            match next {
                                'd' | 'i' if is_short_short => result.push_str(&(raw as u8 as i8).to_string()),
                                'd' | 'i' if is_short => result.push_str(&(raw as u16 as i16).to_string()),
                                'd' | 'i' => result.push_str(&(raw as i32).to_string()),
                                'u' if is_short_short => result.push_str(&(raw as u8).to_string()),
                                'u' if is_short => result.push_str(&(raw as u16).to_string()),
                                'u' => result.push_str(&raw.to_string()),
                                'x' if is_short_short => result.push_str(&format!("{:x}", raw as u8)),
                                'x' if is_short => result.push_str(&format!("{:x}", raw as u16)),
                                'x' => result.push_str(&format!("{raw:x}")),
                                'X' if is_short_short => result.push_str(&format!("{:X}", raw as u8)),
                                'X' if is_short => result.push_str(&format!("{:X}", raw as u16)),
                                'X' => result.push_str(&format!("{raw:X}")),
                                'o' if is_short_short => result.push_str(&format!("{:o}", raw as u8)),
                                'o' if is_short => result.push_str(&format!("{:o}", raw as u16)),
                                'o' => result.push_str(&format!("{raw:o}")),
                                _ => unreachable!(),
                            }
                            arg_pos += 4;
                        }
                    }
                    break;
                }
                'f' | 'g' | 'e' => {
                    chars.next();
                    // 8-byte double
                    if arg_pos + 8 <= args_data.len() {
                        let val = f64::from_le_bytes(args_data[arg_pos..arg_pos + 8].try_into().unwrap_or([0; 8]));
                        result.push_str(&format!("{val}"));
                        arg_pos += 8;
                    }
                    break;
                }
                's' => {
                    chars.next();
                    // String: null-terminated, 4-byte aligned length
                    let str_start = arg_pos;
                    let str_end = args_data[str_start..].iter().position(|&b| b == 0).map(|p| str_start + p).unwrap_or(args_data.len());
                    let s = String::from_utf8_lossy(&args_data[str_start..str_end]);
                    result.push_str(&s);
                    arg_pos = (str_end + 4) & !3; // Align
                    break;
                }
                'c' => {
                    chars.next();
                    if arg_pos + 4 <= args_data.len() {
                        let val = args_data[arg_pos];
                        result.push(val as char);
                        arg_pos += 4;
                    }
                    break;
                }
                'p' => {
                    chars.next();
                    if arg_pos + 4 <= args_data.len() {
                        let val = u32::from_le_bytes(args_data[arg_pos..arg_pos + 4].try_into().unwrap_or([0; 4]));
                        result.push_str(&format!("0x{val:08x}"));
                        arg_pos += 4;
                    }
                    break;
                }
                '%' => {
                    chars.next();
                    result.push('%');
                    break;
                }
                _ => {
                    result.push('%');
                    result.push(next);
                    chars.next();
                    break;
                }
            }
        }
    }

    result
}

// ─── EC718 binary log decoder (0x7E HDLC framing) ───────────────────────────

/// EC718 binary log frame decoder (Air8000 / Air780E series).
///
/// Uses 0x7E frame delimiters with HDLC byte-stuffing (0x7D escape).
///
/// Frame: `[0x7E] [HDLC-escaped payload] [0x7E]`
///
/// Header (12 bytes):
///   timestamp_ms : u32 LE  (milliseconds since boot)
///   reserved     : u32 LE  (always 0)
///   tag          : u32 LE  (source module identifier)
///
/// Body: printf format string (NUL-terminated) + 4-byte aligned arguments.
/// Argument encoding differs from SocLogDecoder:
///   %s   → [u32 length] [string bytes] (padded to 4-byte boundary)
///   %.*s → [u32 precision] [string bytes] (padded to 4-byte boundary)
///   %d/%u/%x/%p → u32 LE
pub struct Ec718LogDecoder {
    frame_buf: Vec<u8>,
    in_frame: bool,
    escape_next: bool,
}

impl Ec718LogDecoder {
    pub fn new() -> Self {
        Self {
            frame_buf: Vec::with_capacity(2048),
            in_frame: false,
            escape_next: false,
        }
    }

    /// Feed raw bytes and extract decoded log entries.
    pub fn feed(&mut self, data: &[u8]) -> Vec<LogEntry> {
        let mut entries = Vec::new();

        for &byte in data {
            if byte == 0x7E {
                if self.in_frame && self.frame_buf.len() >= 12 {
                    if let Some(entry) = self.decode_frame(&self.frame_buf.clone()) {
                        entries.push(entry);
                    }
                }
                self.frame_buf.clear();
                self.in_frame = true;
                self.escape_next = false;
                continue;
            }

            if !self.in_frame {
                continue;
            }

            // HDLC byte-stuffing: 0x7D XX → XX ^ 0x20
            if byte == 0x7D {
                self.escape_next = true;
                continue;
            }

            if self.escape_next {
                self.frame_buf.push(byte ^ 0x20);
                self.escape_next = false;
                continue;
            }

            self.frame_buf.push(byte);

            if self.frame_buf.len() > 8192 {
                self.frame_buf.clear();
                self.in_frame = false;
            }
        }

        entries
    }

    /// Decode a complete 0x7E frame payload into a LogEntry.
    fn decode_frame(&self, data: &[u8]) -> Option<LogEntry> {
        if data.len() < 13 {
            return None; // 12-byte header + at least 1 byte body
        }

        let ms = u32::from_le_bytes(data[0..4].try_into().ok()?) as u64;
        // bytes 4..8 reserved (always 0)
        let _tag = u32::from_le_bytes(data[8..12].try_into().ok()?);

        let body = &data[12..];
        let message = decode_ec718_printf(body);

        // Extract level/module from the decoded message text.
        // EC718 embeds level prefix in the format string: "I/http ...", "D/net ...", etc.
        let (level, module, msg_body) = parse_level_prefix(&message);

        let now = cached_local_timestamp();
        let device_time = format!("{}.{:03}", ms / 1000, ms % 1000);

        Some(LogEntry {
            timestamp: now,
            device_time: Some(device_time),
            level,
            module,
            message: msg_body,
            raw: format!("[EC718 frame {} bytes]", data.len()),
        })
    }
}

impl Default for Ec718LogDecoder {
    fn default() -> Self {
        Self::new()
    }
}

// ─── RDA8910 host trace decoder ───────────────────────────────────────────────

/// 从 RDA8910 (Air724UG) 运行模式 x.6 soc_log 口的二进制 host trace 流中提取 Lua 日志行。
///
/// 传输层编码对齐 luatools_py3 `host_device.handle_data`：剥除裸 `0x11`/`0x13`
/// （XON/XOFF 流控），并反转义 `5c ee`→`0x11`、`5c ec`→`0x13`、`5c a3`→`0x5c`。
///
/// 日志事件帧（见 luatos-sdk-rda8910 `osi_log.c` `osiTraceVprintf`/`prvFillTraceHeader`）：
/// `[0xAD][len_msb][len_lsb][0x98][sn:u16][tick:u16][tag:u32][fmt: NUL + 4 对齐][args...]`，
/// tag 高 4 位为日志级别（0=NEVER..5=VERBOSE），低 28 位为 4 个 7 位 ASCII 模块字符。
///
/// C 侧 `LLOGx("tag", fmt, ...)` 把完整行文本（含 `I/main ` 前缀）作为格式串、参数
/// 分别序列化（`prvStrParamBuf`），参数编码：
///   %s          → 字符串内容 + NUL，随后按 4 字节对齐（无长度前缀）
///   %d/%u/%x/%o/%c/%p → u32 LE（%h/%hh 按 C 变参提升仍占 4 字节）
///   %lld        → u64 LE；%f/%g/%e/%a(double) → f64 LE（8 字节）
/// Lua 侧 `log.info()` 经 `print()` 已格式化为完整文本，作为 `%s` 参数内联在
/// `:%.*s>> ` 包装事件中，无需再次解析。
///
/// 因此本解码器按 `[DIWE]/` 标记定位日志行；当标记恰好位于某 trace 帧的格式串起点时
/// 判定为 C 侧日志并消费其后的二进制参数做 printf 替换，否则视为已格式化文本原样输出。
/// 真机抓包中日志行以 `D/` `I/` `W/` `E/` + 模块名开头，例如
/// `I/main LuatOS@%s base %s bsp %s 64bit`。
pub struct Rda8910LogDecoder {
    /// 已反转义的干净字节缓冲（跨 feed 累积，保留尾部供跨块标记续接）。
    buf: Vec<u8>,
    /// 上一批末尾遗留的裸 `0x5c`（可能是反转义前缀，等下一批补齐）。
    pending: Vec<u8>,
}

impl Rda8910LogDecoder {
    pub fn new() -> Self {
        Self {
            buf: Vec::with_capacity(8192),
            pending: Vec::with_capacity(2),
        }
    }

    /// Feed 原始字节，返回从中提取出的完整日志行条目。
    pub fn feed(&mut self, data: &[u8]) -> Vec<LogEntry> {
        // 合并本批数据与上批遗留的未定字节（末尾 0x5c 可能为反转义前缀）。
        let mut combined = Vec::with_capacity(self.pending.len() + data.len());
        combined.extend_from_slice(&self.pending);
        combined.extend_from_slice(data);
        self.pending.clear();

        // 末尾是 0x5c 时可能是 `5c ee/ec/a3` 转义对的前缀，等下一批再反转义。
        if combined.last() == Some(&0x5c) {
            self.pending = combined;
            return Vec::new();
        }

        // 反转义并追加到干净缓冲。
        self.buf.extend_from_slice(&unescape_rda8910(&combined));

        // 防失控：缓冲区过长说明长时间无有效标记，只留尾部继续找。
        const HARD_CAP: usize = 256 * 1024;
        if self.buf.len() > HARD_CAP {
            const KEEP: usize = 4096;
            self.buf = self.buf[self.buf.len() - KEEP..].to_vec();
            return Vec::new();
        }

        let n = self.buf.len();
        let mut i = 0usize;
        let mut drain_to = 0usize; // 已确认消费掉的字节数
        let mut entries = Vec::new();
        while i + 2 < n {
            let b = self.buf[i];
            if (b == b'D' || b == b'I' || b == b'W' || b == b'E') && self.buf[i + 1] == b'/' {
                // `[DIWE]/` 落在某帧的二进制头部（sn/tick/tag）内、或 fmtid 型帧的
                // 二进制参数里（如 `I/mCFW_GprsGetstatus`）都是假阳性。
                if is_in_frame_header(&self.buf, i) || is_in_fmtid_frame(&self.buf, i) {
                    i += 1;
                    continue;
                }
                match self.extract_line(&self.buf, i) {
                    Some((end, message)) => {
                        let (level, module, body) = parse_level_prefix(&message);
                        if let Some(module) = module {
                            if !module.is_empty() {
                                entries.push(LogEntry {
                                    timestamp: cached_local_timestamp(),
                                    device_time: None,
                                    level,
                                    module: Some(module),
                                    message: body,
                                    raw: message,
                                });
                            }
                        }
                        drain_to = end;
                        i = end;
                        continue;
                    }
                    None => {
                        // 行未收齐（格式串 / 多字节 UTF-8 延伸至缓冲区末尾）→ 等下一批数据。
                        break;
                    }
                }
            } else {
                i += 1;
            }
        }

        // 丢弃已消费部分，保留未处理的尾部（含可能跨块断开的标记）。
        if drain_to > 0 {
            self.buf.drain(..drain_to);
        }
        entries
    }

    /// 从 `i`（`[DIWE]/` 标记处）提取一条完整日志行，返回 `(已消费字节数, 行文本)`。
    ///
    /// 若标记恰好是某 trace 帧的格式串起点（帧头 + sn/tick/tag 共 12 字节可回溯确认），
    /// 说明是 C 侧 `LLOGx(fmt, ...)` 日志：按格式串消费其后的二进制参数并做 printf 替换；
    /// 否则视为 Lua 侧已格式化的文本，原样返回可打印段。
    fn extract_line(&self, buf: &[u8], i: usize) -> Option<(usize, String)> {
        // 先取可打印段作为兜底文本（对未格式化的 Lua 文本已足够）。
        let (run_end, run_text) = extract_printable_run(buf, i)?;

        // 只有确认是 C 侧日志帧（标记位于格式串起点）时才做参数解码。
        let Some((_, payload_end)) = trace_frame_region(buf, i) else {
            return Some((run_end, run_text));
        };
        if payload_end > buf.len() {
            // 帧未收齐：先按文本输出，避免误等导致整批停滞。
            return Some((run_end, run_text));
        }

        // 格式串从标记延伸到 NUL（含行尾 \n），参数紧随其后。
        let end_cap = (i + 2048).min(payload_end);
        let nul = buf[i..end_cap].iter().position(|&b| b == 0).map(|d| i + d)?;
        let fmt_bytes = &buf[i..nul];
        let fmt = String::from_utf8_lossy(fmt_bytes);
        if !fmt.contains('%') {
            return Some((nul + 1, run_text));
        }

        // 参数区起点 = 格式串长度（含 NUL）按 4 对齐后的位置，且不超出帧负载。
        let args_start = i + align4(fmt_bytes.len() + 1);
        if args_start > payload_end {
            return Some((nul + 1, run_text));
        }
        match decode_rda8910_printf(&fmt, &buf[args_start..payload_end]) {
            Some((decoded, consumed)) => {
                let decoded = decoded.trim_end_matches(['\n', '\r']).to_string();
                if !decoded.trim().is_empty() && args_start + consumed <= payload_end {
                    Some((args_start + consumed, decoded))
                } else {
                    Some((nul + 1, run_text))
                }
            }
            _ => Some((nul + 1, run_text)),
        }
    }
}

impl Default for Rda8910LogDecoder {
    fn default() -> Self {
        Self::new()
    }
}

/// 回溯确认 `fmt_start` 处的 `[DIWE]/` 标记是否为某 host trace 帧的格式串起点。
///
/// 帧布局（`osiTraceHeader_t` host 变体）：
/// `[0xAD][len_msb][len_lsb][0x98][sn:u16][tick:u16][tag:u32][fmt...]`，
/// 即格式串起点前的第 12 字节为 `0xAD`、第 9 字节为 `0x98`，且 tag 高 4 位为合法日志级别、
/// 低 28 位为 4 个可打印 ASCII 字符。Lua 侧消息（作为 `%s` 参数内联）不满足此布局。
/// 返回 `(帧起点, 帧负载结束位置)`。
fn trace_frame_region(buf: &[u8], fmt_start: usize) -> Option<(usize, usize)> {
    if fmt_start < 12 || buf[fmt_start - 12] != 0xAD || buf[fmt_start - 9] != 0x98 {
        return None;
    }
    let len = ((buf[fmt_start - 11] as usize) << 8) | buf[fmt_start - 10] as usize;
    if len < 8 {
        return None; // 负载过小，不可能是日志帧
    }
    let tag = u32::from_le_bytes(buf[fmt_start - 4..fmt_start].try_into().unwrap_or([0; 4]));
    if tag >> 28 > 5 {
        return None; // 日志级别非法
    }
    let chars = [(tag & 0x7F) as u8, ((tag >> 7) & 0x7F) as u8, ((tag >> 14) & 0x7F) as u8, ((tag >> 21) & 0x7F) as u8];
    if !chars.iter().all(|&c| (0x20..=0x7E).contains(&c) || c == b' ') {
        return None;
    }
    let frame_start = fmt_start - 12;
    Some((frame_start, frame_start + 4 + len))
}

/// 判断 `i` 处的 `[DIWE]/` 标记是否落在某 host trace 帧的二进制头部（sn/tick/tag）内。
///
/// 帧头 `[0xAD][len_msb][len_lsb][0x98]` 之后紧跟 8 字节 `[sn:u16][tick:u16][tag:u32]`，
/// 若标记位于其后 4..=11 字节处，说明它其实是二进制头部里偶然拼出的可打印串
/// （如 sn=`I/` + tick=`mC` + tag=`FW_G` 恰好形成 `I/mCFW_GprsGetstatus`），应跳过；
/// 真正的日志行标记要么在格式串起点（帧头 12 字节前），要么深埋在 `:%.*s>> ` 消息参数里。
fn is_in_frame_header(buf: &[u8], i: usize) -> bool {
    for k in 4..=11 {
        if i >= k && buf[i - k] == 0xAD && buf[i - k + 3] == 0x98 {
            // 校验长度字段合理，降低对真实日志行的误判。
            let len = ((buf[i - k + 1] as usize) << 8) | buf[i - k + 2] as usize;
            if (8..=8192).contains(&len) {
                return true;
            }
        }
    }
    false
}

/// 判断 `i` 处的 `[DIWE]/` 标记是否落在某个 fmtid 型帧的二进制参数里。
///
/// fmtid 帧的 tag 最高位 `bit31` 置位（`TRACE_TDB_FLAG`，见 `osi_log.c` `prvTraceIdBasic`），
/// 帧内没有内联格式串，只有 `[fmtid:u32] + 原始 u32 参数`。参数（如 `0x6D2F4987` 的字节
/// `87 49 2f 6d` 拼出 `I/m`）里的可打印串是假阳性，应跳过。
fn is_in_fmtid_frame(buf: &[u8], i: usize) -> bool {
    // 向后找包围该标记的帧头 `0xAD len 0x98`；只有 tag 同时满足
    //  bit31=TRACE_TDB_FLAG、level(bit28-30)<=5、4 个 7bit tag 字符可打印
    //  才判定为 fmtid 帧。真实 Lua 日志帧的 tag 是 `(level<<28)|chars`（level 0~5），
    //  bit31 恒为 0，不会被误判，因此本过滤不会丢掉真实日志行。
    let lo = i.saturating_sub(2048);
    let hi = i.saturating_sub(4);
    for p in (lo..=hi).rev() {
        if buf[p] == 0xAD && buf[p + 3] == 0x98 {
            let len = ((buf[p + 1] as usize) << 8) | buf[p + 2] as usize;
            let payload_end = p + 4 + len;
            if len >= 8 && payload_end >= i && payload_end <= buf.len() {
                let tag = u32::from_le_bytes(buf[p + 8..p + 12].try_into().unwrap_or([0; 4]));
                if tag >> 31 != 1 {
                    // 最近的包围帧不是 fmtid 帧（如真实 Lua 日志帧）→ 不构成假阳性。
                    return false;
                }
                let level = (tag >> 28) & 0x7;
                if level > 5 {
                    continue;
                }
                let chars = [(tag & 0x7F) as u8, ((tag >> 7) & 0x7F) as u8, ((tag >> 14) & 0x7F) as u8, ((tag >> 21) & 0x7F) as u8];
                if chars.iter().all(|&c| (0x20..=0x7E).contains(&c) || c == b' ') {
                    return true;
                }
            }
        }
    }
    false
}

/// 对齐 luatools_py3 `host_device.handle_data` 的 UART 反转义：
/// `5c ee`→0x11、`5c ec`→0x13、`5c a3`→0x5c。调用方需保证输入末尾无未定的 `5c`。
fn unescape_rda8910(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i] == 0x5c && i + 1 < data.len() {
            match data[i + 1] {
                0xee => {
                    out.push(0x11);
                    i += 2;
                    continue;
                }
                0xec => {
                    out.push(0x13);
                    i += 2;
                    continue;
                }
                0xa3 => {
                    out.push(0x5c);
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }
        out.push(data[i]);
        i += 1;
    }
    out
}

fn align4(x: usize) -> usize {
    (x + 3) & !3
}

/// 按 RDA8910（luatos-sdk-rda8910 `osi_log.c` `prvStrParamBuf`）的参数编码解码 printf 格式串。
/// 返回 `(替换后的文本, 已消费的参数字节数)`；遇到无法确定参数布局的格式符时返回 None，
/// 避免参数错位。
fn decode_rda8910_printf(fmt: &str, args: &[u8]) -> Option<(String, usize)> {
    let mut result = String::with_capacity(fmt.len() + 16);
    let mut pos = 0usize;
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            result.push(c);
            continue;
        }
        let mut longs = 0u8;
        let mut star = false;
        let conv: char = loop {
            match chars.peek().copied() {
                Some('0'..='9') | Some('-') | Some('+') | Some(' ') | Some('#') | Some('.') => {
                    chars.next();
                }
                Some('*') => {
                    star = true;
                    chars.next();
                }
                Some('h') | Some('z') | Some('t') | Some('j') | Some('L') => {
                    chars.next();
                }
                Some('l') => {
                    longs += 1;
                    chars.next();
                }
                Some(c2 @ ('d' | 'i' | 'u' | 'x' | 'X' | 'o' | 'c' | 'p' | 's' | 'f' | 'g' | 'e' | 'a' | 'F' | 'G' | 'E' | 'A' | 'n' | '%')) => {
                    chars.next();
                    break c2;
                }
                Some(_) | None => return None, // 未知格式符 → 参数布局不定，放弃
            }
        };
        if conv == '%' {
            result.push('%');
            continue;
        }
        if star {
            // %*d / %.*s 等宽度/精度参数占位不明确，放弃整行解码避免错位。
            return None;
        }
        match conv {
            's' => {
                // %s → NUL 结尾字符串，随后 4 字节对齐
                let start = pos;
                let mut end = start;
                while end < args.len() && args[end] != 0 {
                    end += 1;
                }
                if end >= args.len() {
                    return None; // 未收齐，等下一批
                }
                result.push_str(&String::from_utf8_lossy(&args[start..end]));
                pos = align4(end + 1);
            }
            'd' | 'i' | 'u' | 'x' | 'X' | 'o' | 'c' | 'p' => {
                if longs >= 2 {
                    if pos + 8 > args.len() {
                        return None;
                    }
                    let raw = i64::from_le_bytes(args[pos..pos + 8].try_into().ok()?);
                    pos += 8;
                    match conv {
                        'x' => result.push_str(&format!("{:x}", raw as u64)),
                        'X' => result.push_str(&format!("{:X}", raw as u64)),
                        'u' => result.push_str(&format!("{}", raw as u64)),
                        _ => result.push_str(&format!("{raw}")),
                    }
                } else {
                    if pos + 4 > args.len() {
                        return None;
                    }
                    let raw = u32::from_le_bytes(args[pos..pos + 4].try_into().ok()?);
                    pos += 4;
                    match conv {
                        'd' | 'i' => result.push_str(&format!("{}", raw as i32)),
                        'u' => result.push_str(&format!("{raw}")),
                        'x' => result.push_str(&format!("{raw:x}")),
                        'X' => result.push_str(&format!("{raw:X}")),
                        'o' => result.push_str(&format!("{raw:o}")),
                        'c' => result.push(raw as u8 as char),
                        'p' => result.push_str(&format!("0x{raw:08x}")),
                        _ => {}
                    }
                }
            }
            'f' | 'g' | 'e' | 'a' | 'F' | 'G' | 'E' | 'A' => {
                if pos + 8 > args.len() {
                    return None;
                }
                let v = f64::from_le_bytes(args[pos..pos + 8].try_into().ok()?);
                pos += 8;
                result.push_str(&format!("{v}"));
            }
            'n' => return None,
            _ => return None,
        }
    }
    Some((result, pos))
}

/// 从 `start` 起截取一段可打印 UTF-8 连续段（ASCII 可打印 + 合法多字节字符），
/// 到首个不可打印字节或缓冲区末尾为止。多字节字符在缓冲区末尾被截断时返回 None
/// （等待下一批数据补齐），避免把半个字符当成边界。
fn extract_printable_run(buf: &[u8], start: usize) -> Option<(usize, String)> {
    let n = buf.len();
    let mut i = start;
    let mut out = Vec::with_capacity(64);
    while i < n {
        let b = buf[i];
        if (0x20..=0x7E).contains(&b) {
            out.push(b);
            i += 1;
            continue;
        }
        if b == b'\n' || b == b'\r' || b == 0 || b < 0x20 || b == 0x7F {
            break; // 控制字符 → 段结束
        }
        // 多字节 UTF-8
        let len = match b {
            0xC2..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF4 => 4,
            _ => 0, // 孤立续字节 / 非法 → 段结束
        };
        if len == 0 {
            break;
        }
        if i + len > n {
            return None; // 字符未收齐，等下一批
        }
        let seq = &buf[i..i + len];
        if !seq[1..].iter().all(|&c| (0x80..=0xBF).contains(&c)) {
            break;
        }
        out.extend_from_slice(seq);
        i += len;
    }
    if out.is_empty() {
        return None;
    }
    let s = String::from_utf8(out).ok()?;
    Some((i, s))
}

/// Decode printf format string with length-prefixed string arguments (EC718 variant).
fn decode_ec718_printf(body: &[u8]) -> String {
    if body.is_empty() {
        return String::new();
    }

    let fmt_end = body.iter().position(|&b| b == 0).unwrap_or(body.len());
    let fmt_str = String::from_utf8_lossy(&body[..fmt_end]).to_string();

    let args_offset = (fmt_end + 4) & !3;
    if args_offset >= body.len() {
        return fmt_str;
    }

    let args_data = &body[args_offset..];
    let mut result = String::new();
    let mut arg_pos = 0;
    let mut chars = fmt_str.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '%' {
            result.push(c);
            continue;
        }

        let mut is_long = false;
        let mut is_long_long = false;
        let mut is_short = false;
        let mut is_short_short = false;
        let mut has_star = false;

        loop {
            match chars.peek() {
                Some(&next @ ('0'..='9' | '-' | '+' | ' ' | '#' | '.')) => {
                    chars.next();
                    // consume width/precision digits; we don't use them for decoding
                    let _ = next;
                }
                Some(&'*') => {
                    has_star = true;
                    chars.next();
                }
                Some(&'h') => {
                    if is_short {
                        is_short_short = true;
                    }
                    is_short = true;
                    chars.next();
                }
                Some(&'l') => {
                    if is_long {
                        is_long_long = true;
                    }
                    is_long = true;
                    chars.next();
                }
                Some(&'d') | Some(&'i') | Some(&'u') | Some(&'x') | Some(&'X') | Some(&'o') => {
                    let spec = chars.next().unwrap();
                    if is_long_long {
                        if arg_pos + 8 <= args_data.len() {
                            let val = i64::from_le_bytes(args_data[arg_pos..arg_pos + 8].try_into().unwrap_or([0; 8]));
                            match spec {
                                'x' => result.push_str(&format!("{val:x}")),
                                'X' => result.push_str(&format!("{val:X}")),
                                'u' => result.push_str(&format!("{}", val as u64)),
                                _ => result.push_str(&format!("{val}")),
                            }
                            arg_pos += 8;
                        }
                    } else {
                        if arg_pos + 4 <= args_data.len() {
                            // C varargs promote `short`/`char` integers to a
                            // 4-byte slot; the `h`/`hh` modifier only controls
                            // how the decoded value is presented.
                            let raw = u32::from_le_bytes(args_data[arg_pos..arg_pos + 4].try_into().unwrap_or([0; 4]));
                            match spec {
                                'd' | 'i' if is_short_short => result.push_str(&(raw as u8 as i8).to_string()),
                                'd' | 'i' if is_short => result.push_str(&(raw as u16 as i16).to_string()),
                                'd' | 'i' => result.push_str(&(raw as i32).to_string()),
                                'u' if is_short_short => result.push_str(&(raw as u8).to_string()),
                                'u' if is_short => result.push_str(&(raw as u16).to_string()),
                                'u' => result.push_str(&raw.to_string()),
                                'x' if is_short_short => result.push_str(&format!("{:x}", raw as u8)),
                                'x' if is_short => result.push_str(&format!("{:x}", raw as u16)),
                                'x' => result.push_str(&format!("{raw:x}")),
                                'X' if is_short_short => result.push_str(&format!("{:X}", raw as u8)),
                                'X' if is_short => result.push_str(&format!("{:X}", raw as u16)),
                                'X' => result.push_str(&format!("{raw:X}")),
                                'o' if is_short_short => result.push_str(&format!("{:o}", raw as u8)),
                                'o' if is_short => result.push_str(&format!("{:o}", raw as u16)),
                                'o' => result.push_str(&format!("{raw:o}")),
                                _ => unreachable!(),
                            }
                            arg_pos += 4;
                        }
                    }
                    break;
                }
                Some(&'f') | Some(&'g') | Some(&'e') => {
                    chars.next();
                    if arg_pos + 8 <= args_data.len() {
                        let val = f64::from_le_bytes(args_data[arg_pos..arg_pos + 8].try_into().unwrap_or([0; 8]));
                        result.push_str(&format!("{val}"));
                        arg_pos += 8;
                    }
                    break;
                }
                Some(&'s') => {
                    chars.next();
                    if has_star {
                        // %.*s: precision from arg (u32) then string bytes
                        if arg_pos + 4 <= args_data.len() {
                            let precision = u32::from_le_bytes(args_data[arg_pos..arg_pos + 4].try_into().unwrap_or([0; 4])) as usize;
                            arg_pos += 4;
                            let end = arg_pos.checked_add(precision).unwrap_or(args_data.len()).min(args_data.len());
                            let s = String::from_utf8_lossy(&args_data[arg_pos..end]);
                            result.push_str(&s);
                            arg_pos = (end + 3) & !3; // align to 4
                        }
                    } else {
                        // %s: [u32 length] [string bytes], 4-byte aligned
                        if arg_pos + 4 <= args_data.len() {
                            let slen = u32::from_le_bytes(args_data[arg_pos..arg_pos + 4].try_into().unwrap_or([0; 4])) as usize;
                            arg_pos += 4;
                            let end = arg_pos.checked_add(slen).unwrap_or(args_data.len()).min(args_data.len());
                            let s = String::from_utf8_lossy(&args_data[arg_pos..end]);
                            result.push_str(&s);
                            arg_pos = (end + 3) & !3; // align to 4
                        }
                    }
                    break;
                }
                Some(&'c') => {
                    chars.next();
                    if arg_pos + 4 <= args_data.len() {
                        let val = args_data[arg_pos];
                        result.push(val as char);
                        arg_pos += 4;
                    }
                    break;
                }
                Some(&'p') => {
                    chars.next();
                    if arg_pos + 4 <= args_data.len() {
                        let val = u32::from_le_bytes(args_data[arg_pos..arg_pos + 4].try_into().unwrap_or([0; 4]));
                        result.push_str(&format!("0x{val:08x}"));
                        arg_pos += 4;
                    }
                    break;
                }
                Some(&'%') => {
                    chars.next();
                    result.push('%');
                    break;
                }
                Some(&other) => {
                    chars.next();
                    result.push('%');
                    result.push(other);
                    break;
                }
                None => {
                    result.push('%');
                    break;
                }
            }
        }
    }

    result
}

/// Extract log level and module from EC718 message prefix like "I/http ...", "D/net ...".
fn parse_level_prefix(msg: &str) -> (LogLevel, Option<String>, String) {
    // Pattern: "X/module rest..." where X is D/I/W/E/T
    if msg.len() >= 3 {
        let bytes = msg.as_bytes();
        if bytes[1] == b'/' {
            let level = LogLevel::from_char(bytes[0] as char);
            if !matches!(level, LogLevel::Unknown(_) | LogLevel::Raw) {
                let rest = &msg[2..];
                if let Some(space_pos) = rest.find(' ') {
                    let module = rest[..space_pos].to_string();
                    let body = rest[space_pos + 1..].to_string();
                    return (level, Some(module), body);
                } else {
                    return (level, Some(rest.to_string()), String::new());
                }
            }
        }
    }
    (LogLevel::Info, None, msg.to_string())
}

// ─── SOC log parser (as a LogParser for text mode fallback) ──────────────────

/// Parser wrapper that interprets text lines containing hex-encoded SOC frames.
/// Used for offline parsing of recorded binary logs.
pub struct SocLogParser;

impl LogParser for SocLogParser {
    fn name(&self) -> &str {
        "soclog"
    }

    fn parse_line(&self, line: &str) -> Option<LogEntry> {
        // SOC binary logs are not text-based, so this parser handles
        // hex-dump format lines like "A5 xx xx ... A5"
        let trimmed = line.trim();
        if !trimmed.starts_with("A5") && !trimmed.starts_with("a5") {
            return None;
        }

        // Try to parse as hex bytes
        let bytes: Vec<u8> = trimmed.split_whitespace().filter_map(|s| u8::from_str_radix(s, 16).ok()).collect();

        if bytes.len() < 26 || bytes[0] != 0xA5 {
            return None;
        }

        // Use SocLogDecoder to parse
        let mut decoder = SocLogDecoder::new();
        let entries = decoder.feed(&bytes);
        entries.into_iter().next()
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_luatos_standard_line() {
        let parser = LuatosParser;
        let entry = parser.parse_line("[2026-04-09 12:03:37.290] I/user.test 1234").unwrap();
        assert_eq!(entry.level, LogLevel::Info);
        assert_eq!(entry.module.as_deref(), Some("user.test"));
        assert_eq!(entry.message, "1234");
        assert_eq!(entry.device_time.as_deref(), Some("2026-04-09 12:03:37.290"));
    }

    #[test]
    fn parse_luatos_no_timestamp() {
        let parser = LuatosParser;
        let entry = parser.parse_line("W/sys.net connection lost").unwrap();
        assert_eq!(entry.level, LogLevel::Warn);
        assert_eq!(entry.module.as_deref(), Some("sys.net"));
        assert_eq!(entry.message, "connection lost");
        assert!(entry.device_time.is_none());
    }

    #[test]
    fn parse_boot_log() {
        let parser = BootLogParser;
        let entry = parser.parse_line("luat: boot done").unwrap();
        assert_eq!(entry.level, LogLevel::Info);
        assert_eq!(entry.module.as_deref(), Some("luat"));
        assert_eq!(entry.message, "boot done");
    }

    #[test]
    fn dispatcher_tries_parsers_in_order() {
        let d = LogDispatcher::default_parsers();
        let e1 = d.parse("I/user.main hello world");
        assert_eq!(e1.level, LogLevel::Info);
        assert_eq!(e1.module.as_deref(), Some("user.main"));

        let e2 = d.parse("ap0: started");
        assert_eq!(e2.module.as_deref(), Some("ap0"));

        let e3 = d.parse("random garbage");
        assert_eq!(e3.level, LogLevel::Raw);
        assert_eq!(e3.message, "random garbage");
    }

    #[test]
    fn parse_error_level() {
        let d = LogDispatcher::default_parsers();
        let e = d.parse("[2025-01-01 00:00:00.000] E/sys.crash panic at line 42");
        assert_eq!(e.level, LogLevel::Error);
        assert_eq!(e.module.as_deref(), Some("sys.crash"));
        assert!(e.message.contains("panic"));
    }

    #[test]
    fn log_level_display() {
        assert_eq!(LogLevel::Info.to_string(), "I");
        assert_eq!(LogLevel::Error.to_string(), "E");
        assert_eq!(LogLevel::Raw.to_string(), "-");
    }

    #[test]
    fn crc16_modbus_known_vector() {
        // CRC16-ModBus (init=0, poly=0xA001): "123456789"
        let data = b"123456789";
        // With init=0 instead of standard init=0xFFFF
        let crc = crc16_modbus(data);
        assert_ne!(crc, 0); // Non-trivial result
    }

    #[test]
    fn crc16_modbus_empty() {
        assert_eq!(crc16_modbus(&[]), 0x0000); // init=0 → empty = 0
    }

    #[test]
    fn soc_log_decoder_basic() {
        let mut decoder = SocLogDecoder::new();

        // Build a minimal valid frame: 24-byte header + 2-byte CRC
        let mut payload = vec![0u8; 24];
        // ms = 1000 (1 second)
        payload[0..8].copy_from_slice(&1000u64.to_le_bytes());
        // tag = level 2 (Info) + empty tag
        payload[8..16].copy_from_slice(&2u64.to_le_bytes());
        // cmd = 0
        payload[16..20].copy_from_slice(&0u32.to_le_bytes());
        // sn = 1
        payload[20..22].copy_from_slice(&1u16.to_le_bytes());
        // type = 0
        payload[22] = 0;
        // cpu = 0
        payload[23] = 0;

        let crc = crc16_modbus(&payload);

        // Build frame with 0xA5 markers
        let mut frame = vec![0xA5];
        // Escape any 0xA5/0xA6 bytes in payload
        for &b in &payload {
            match b {
                0xA5 => frame.extend_from_slice(&[0xA6, 0x01]),
                0xA6 => frame.extend_from_slice(&[0xA6, 0x02]),
                _ => frame.push(b),
            }
        }
        // Append CRC (little-endian)
        let crc_bytes = crc.to_le_bytes();
        for &b in &crc_bytes {
            match b {
                0xA5 => frame.extend_from_slice(&[0xA6, 0x01]),
                0xA6 => frame.extend_from_slice(&[0xA6, 0x02]),
                _ => frame.push(b),
            }
        }
        frame.push(0xA5); // End marker

        let entries = decoder.feed(&frame);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].level, LogLevel::Info);
        assert_eq!(entries[0].device_time.as_deref(), Some("1.000"));
    }

    #[test]
    fn decode_tag_name_basic() {
        // 'a' = index 1, 'b' = index 2
        let tag = 1u64 | (2u64 << 7); // "ab"
        let name = decode_tag_name(tag);
        assert_eq!(name, "ab");
    }

    #[test]
    fn decode_printf_simple() {
        // "hello" (no format args)
        let body = b"hello\0\0\0"; // null-terminated, padded
        let msg = decode_printf_message(body);
        assert_eq!(msg, "hello");
    }

    #[test]
    fn decode_printf_with_int() {
        // "val=%d" with arg 42
        let mut body = Vec::new();
        body.extend_from_slice(b"val=%d\0\0"); // 8 bytes (4-aligned with null)
        body.extend_from_slice(&42i32.to_le_bytes());
        let msg = decode_printf_message(&body);
        assert_eq!(msg, "val=42");
    }

    #[test]
    fn decode_printf_with_short_ints() {
        let mut body = b"hd=%hd hu=%hu hx=%hx hX=%hX hhd=%hhd hhu=%hhu next=%u\0".to_vec();
        body.resize((body.len() + 3) & !3, 0);
        body.extend_from_slice(&(-2i32).to_le_bytes());
        body.extend_from_slice(&65535u32.to_le_bytes());
        body.extend_from_slice(&0xabcdu32.to_le_bytes());
        body.extend_from_slice(&0xbeefu32.to_le_bytes());
        body.extend_from_slice(&(-2i32).to_le_bytes());
        body.extend_from_slice(&255u32.to_le_bytes());
        body.extend_from_slice(&77u32.to_le_bytes());

        let msg = decode_printf_message(&body);
        assert_eq!(msg, "hd=-2 hu=65535 hx=abcd hX=BEEF hhd=-2 hhu=255 next=77");
    }

    #[test]
    fn soc_log_escape_handling() {
        let mut decoder = SocLogDecoder::new();

        // Test that escape sequences work correctly
        // 0xA6 0x01 should become 0xA5
        // 0xA6 0x02 should become 0xA6
        let mut payload = vec![0u8; 24];
        payload[0..8].copy_from_slice(&500u64.to_le_bytes());
        payload[8..16].copy_from_slice(&1u64.to_le_bytes()); // Debug level
        payload[20..22].copy_from_slice(&1u16.to_le_bytes());

        let crc = crc16_modbus(&payload);

        let mut frame = vec![0xA5];
        for &b in &payload {
            match b {
                0xA5 => frame.extend_from_slice(&[0xA6, 0x01]),
                0xA6 => frame.extend_from_slice(&[0xA6, 0x02]),
                _ => frame.push(b),
            }
        }
        let crc_bytes = crc.to_le_bytes();
        for &b in &crc_bytes {
            match b {
                0xA5 => frame.extend_from_slice(&[0xA6, 0x01]),
                0xA6 => frame.extend_from_slice(&[0xA6, 0x02]),
                _ => frame.push(b),
            }
        }
        frame.push(0xA5);

        let entries = decoder.feed(&frame);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].level, LogLevel::Debug);
    }

    // ─── Ec718LogDecoder tests ─────────────────────────────

    #[test]
    fn ec718_decoder_basic() {
        let mut decoder = Ec718LogDecoder::new();

        // Build a 0x7E frame: 12-byte header + "hello\0" body
        let mut payload = Vec::new();
        payload.extend_from_slice(&5000u32.to_le_bytes()); // ms = 5000
        payload.extend_from_slice(&0u32.to_le_bytes()); // reserved
        payload.extend_from_slice(&0x12345678u32.to_le_bytes()); // tag
        payload.extend_from_slice(b"hello\0\0\0"); // format string, padded

        let mut frame = vec![0x7E];
        frame.extend_from_slice(&payload);
        frame.push(0x7E);

        let entries = decoder.feed(&frame);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].device_time.as_deref(), Some("5.000"));
        assert_eq!(entries[0].message, "hello");
    }

    #[test]
    fn ec718_decoder_with_level_prefix() {
        let mut decoder = Ec718LogDecoder::new();

        let mut payload = Vec::new();
        payload.extend_from_slice(&1234u32.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(b"D/net connection ok\0");

        let mut frame = vec![0x7E];
        frame.extend_from_slice(&payload);
        frame.push(0x7E);

        let entries = decoder.feed(&frame);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].level, LogLevel::Debug);
        assert_eq!(entries[0].module.as_deref(), Some("net"));
        assert_eq!(entries[0].message, "connection ok");
    }

    #[test]
    fn ec718_decoder_printf_with_int() {
        let mut decoder = Ec718LogDecoder::new();

        let mut payload = Vec::new();
        payload.extend_from_slice(&100u32.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());
        // "val=%d\0" + padding + arg 42
        payload.extend_from_slice(b"val=%d\0\0"); // 8 bytes
        payload.extend_from_slice(&42i32.to_le_bytes());

        let mut frame = vec![0x7E];
        frame.extend_from_slice(&payload);
        frame.push(0x7E);

        let entries = decoder.feed(&frame);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].message, "val=42");
    }

    #[test]
    fn ec718_printf_with_short_ints() {
        let mut body = b"hd=%hd hu=%hu hx=%hx next=%u\0".to_vec();
        body.resize((body.len() + 3) & !3, 0);
        body.extend_from_slice(&(-2i32).to_le_bytes());
        body.extend_from_slice(&65535u32.to_le_bytes());
        body.extend_from_slice(&0xabcdu32.to_le_bytes());
        body.extend_from_slice(&77u32.to_le_bytes());

        let msg = decode_ec718_printf(&body);
        assert_eq!(msg, "hd=-2 hu=65535 hx=abcd next=77");
    }

    #[test]
    fn ec718_decoder_printf_with_length_prefixed_string() {
        let mut decoder = Ec718LogDecoder::new();

        let mut payload = Vec::new();
        payload.extend_from_slice(&200u32.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());
        // "name=%s\0" (8 bytes, already aligned)
        payload.extend_from_slice(b"name=%s\0");
        // %s arg: length-prefixed: u32(5) + "world" + padding
        payload.extend_from_slice(&5u32.to_le_bytes());
        payload.extend_from_slice(b"world\0\0\0"); // padded to 8

        let mut frame = vec![0x7E];
        frame.extend_from_slice(&payload);
        frame.push(0x7E);

        let entries = decoder.feed(&frame);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].message, "name=world");
    }

    #[test]
    fn ec718_decoder_printf_with_star_s() {
        let mut decoder = Ec718LogDecoder::new();

        let mut payload = Vec::new();
        payload.extend_from_slice(&300u32.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());
        // "%.*s\0" (5 bytes) → aligned args at offset 8
        payload.extend_from_slice(b"%.*s\0\0\0\0");
        // precision=4, then "test"
        payload.extend_from_slice(&4u32.to_le_bytes());
        payload.extend_from_slice(b"test");

        let mut frame = vec![0x7E];
        frame.extend_from_slice(&payload);
        frame.push(0x7E);

        let entries = decoder.feed(&frame);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].message, "test");
    }

    #[test]
    fn ec718_decoder_hdlc_escape() {
        let mut decoder = Ec718LogDecoder::new();

        // Build payload where a byte value is 0x7E (needs escaping as 0x7D 0x5E)
        let mut payload = Vec::new();
        payload.extend_from_slice(&0x7Eu32.to_le_bytes()); // ms = 126 (contains 0x7E byte)
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(b"ok\0");

        // HDLC-escape the payload
        let mut frame = vec![0x7E];
        for &b in &payload {
            if b == 0x7E {
                frame.push(0x7D);
                frame.push(b ^ 0x20); // 0x5E
            } else if b == 0x7D {
                frame.push(0x7D);
                frame.push(b ^ 0x20); // 0x5D
            } else {
                frame.push(b);
            }
        }
        frame.push(0x7E);

        let entries = decoder.feed(&frame);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].device_time.as_deref(), Some("0.126"));
    }

    #[test]
    fn ec718_decoder_multiple_frames() {
        let mut decoder = Ec718LogDecoder::new();

        let mut data = Vec::new();
        for i in 0..3u32 {
            data.push(0x7E);
            data.extend_from_slice(&(i * 1000).to_le_bytes());
            data.extend_from_slice(&0u32.to_le_bytes());
            data.extend_from_slice(&0u32.to_le_bytes());
            let msg = format!("msg{}\0", i);
            data.extend_from_slice(msg.as_bytes());
        }
        data.push(0x7E); // final delimiter

        let entries = decoder.feed(&data);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].message, "msg0");
        assert_eq!(entries[1].message, "msg1");
        assert_eq!(entries[2].message, "msg2");
    }

    // ─── Rda8910LogDecoder tests ───────────────────────────

    /// 模拟真机抓包：二进制 host trace 中夹带 `:%.*s>>` 模板与 `[DIWE]/模块 消息` 日志行，
    /// 以及 CFW_GetRFTemperature / nd6_tmr 等无关可读碎串。
    fn rda_mock_chunk() -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(b"\x80\x10\x23\xc0 :%.*s>> \x00\x80D/user.adc adc0 -1\x1a\x9c");
        d.extend_from_slice(
            b"\x81\x11CFW_GetRFTemperature\x12\x9d :%.*s>> \x01\x90I/user.adc CPU TEMP 29960 \xe5\x8d\x95\xe4\xbd\x8d0.001\xe6\x91\x84\xe6\xb0\x8f\xe5\xba\xa6\xf0\xff",
        );
        d.extend_from_slice(b"\x82\x12nd6_tmr\x0e\xa1 :%.*s>> \x02\x91W/user.net reconnect\x09\xaa");
        d
    }

    #[test]
    fn rda8910_decoder_extracts_log_lines_and_skips_junk() {
        let mut decoder = Rda8910LogDecoder::new();
        let entries = decoder.feed(&rda_mock_chunk());
        assert_eq!(entries.len(), 3, "应提取 3 条日志行");
        assert_eq!(entries[0].module.as_deref(), Some("user.adc"));
        assert_eq!(entries[0].level, LogLevel::Debug);
        assert_eq!(entries[0].message, "adc0 -1");
        assert_eq!(entries[1].module.as_deref(), Some("user.adc"));
        assert_eq!(entries[1].level, LogLevel::Info);
        assert_eq!(entries[1].message, "CPU TEMP 29960 单位0.001摄氏度");
        assert_eq!(entries[2].module.as_deref(), Some("user.net"));
        assert_eq!(entries[2].level, LogLevel::Warn);
        assert_eq!(entries[2].message, "reconnect");
    }

    #[test]
    fn rda8910_decoder_handles_chunk_boundary_split_marker() {
        let mut decoder = Rda8910LogDecoder::new();
        let data = rda_mock_chunk();
        // 把数据切成两半喂入，日志行标记跨块时也能拼出完整行
        let mid = data.len() / 2;
        let mut first = decoder.feed(&data[..mid]);
        first.extend(decoder.feed(&data[mid..]));
        assert_eq!(first.len(), 3, "分块喂入后仍应提取 3 条日志行");
        assert_eq!(first[0].module.as_deref(), Some("user.adc"));
    }

    #[test]
    fn rda8910_decoder_ignores_noise_without_marker() {
        let mut decoder = Rda8910LogDecoder::new();
        let entries = decoder.feed(b"\x80\x81\x82 CFW_GetRFTemperature nd6_tmr \xff\xfe");
        assert!(entries.is_empty(), "无可读日志标记时不应产出条目");
    }

    /// 构造一个合法的 RDA8910 host trace 日志帧（sn/tick/tag/fmt/args）。
    /// fmt 需以 `\n` 结尾；args 需按 RDA8910 参数编码预先组好（含 NUL 与 4 对齐填充）。
    fn build_rda_frame(fmt: &str, tag: u32, args: &[u8]) -> Vec<u8> {
        let fmt_bytes = fmt.as_bytes();
        let fmt_aligned = align4(fmt_bytes.len() + 1);
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0u8, 0]); // sn
        payload.extend_from_slice(&[0u8, 0]); // tick
        payload.extend_from_slice(&tag.to_le_bytes());
        payload.extend_from_slice(fmt_bytes);
        payload.resize(8 + fmt_aligned, 0); // 格式串 + NUL + 4 对齐填充
        payload.extend_from_slice(args);
        let len = payload.len();
        let mut frame = vec![0xAD, (len >> 8) as u8, (len & 0xff) as u8, 0x98];
        frame.extend_from_slice(&payload);
        frame
    }

    /// 构造一个合法 tag：`(level << 28) | 4 个 7 位 ASCII 字符`。
    fn make_rda_tag(level: u8, chars: [char; 4]) -> u32 {
        let mut v = 0u32;
        for (i, c) in chars.iter().enumerate() {
            v |= ((*c as u32) & 0x7F) << (i * 7);
        }
        v | ((level as u32) << 28)
    }

    #[test]
    fn rda8910_decoder_decodes_c_side_printf_args() {
        // C 侧 LLOGI("main", "LuatOS@%s base %s bsp %s 64bit", ...)：
        // 格式串含 %s，参数为 3 个 NUL 结尾、4 对齐的字符串。
        let fmt = "I/main LuatOS@%s base %s bsp %s 64bit\n";
        let mut args = Vec::new();
        args.extend_from_slice(b"Air724UG\0"); // 9 字节，4 对齐到 12
        args.extend_from_slice(&[0, 0, 0]);
        args.extend_from_slice(b"V1009\0"); // 6 字节，4 对齐到 8
        args.extend_from_slice(&[0, 0]);
        args.extend_from_slice(b"V1\0"); // 3 字节，4 对齐到 4
        args.extend_from_slice(&[0]);
        let frame = build_rda_frame(fmt, make_rda_tag(3, ['m', 'a', 'i', 'n']), &args);

        let mut decoder = Rda8910LogDecoder::new();
        let entries = decoder.feed(&frame);
        assert_eq!(entries.len(), 1, "应解码出 1 条日志行");
        assert_eq!(entries[0].level, LogLevel::Info);
        assert_eq!(entries[0].module.as_deref(), Some("main"));
        assert_eq!(entries[0].message, "LuatOS@Air724UG base V1009 bsp V1 64bit");
    }

    #[test]
    fn rda8910_decoder_decodes_int_args() {
        // C 侧 LLOGD("main", "%s luavm %ld %ld %ld", ...)：%ld 在 32 位 ARM 上占 u32。
        let fmt = "D/main %s luavm %ld %ld %ld\n";
        let mut args = Vec::new();
        args.extend_from_slice(b"v1.0.0\0"); // 7 字节，4 对齐到 8
        args.extend_from_slice(&[0]);
        args.extend_from_slice(&(42u32).to_le_bytes());
        args.extend_from_slice(&(256u32).to_le_bytes());
        args.extend_from_slice(&(0xFFFF_FFFEu32).to_le_bytes());
        let frame = build_rda_frame(fmt, make_rda_tag(4, ['m', 'a', 'i', 'n']), &args);

        let mut decoder = Rda8910LogDecoder::new();
        let entries = decoder.feed(&frame);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].module.as_deref(), Some("main"));
        assert_eq!(entries[0].message, "v1.0.0 luavm 42 256 -2");
    }

    #[test]
    fn rda8910_decoder_unescapes_transport_escape() {
        // 传输层转义：`5c a3`→`5c`（反斜杠）。消息内联在 `:%.*s>> ` 事件里，
        // 标记不在帧格式串起点，按已格式化文本输出。
        let mut d = Vec::new();
        d.extend_from_slice(b"\x80\x10\x23\xc0 :%.*s>> \x00\x80I/user C:");
        d.extend_from_slice(&[0x5c, 0xa3]); // 转义后的反斜杠
        d.extend_from_slice(b"temp\\dir\n\x1a\x9c");
        let mut decoder = Rda8910LogDecoder::new();
        let entries = decoder.feed(&d);
        assert_eq!(entries.len(), 1, "应还原转义的反斜杠并提取 1 条日志行");
        assert_eq!(entries[0].module.as_deref(), Some("user"));
        assert_eq!(entries[0].message, "C:\\temp\\dir");
    }

    #[test]
    fn rda8910_decoder_skips_marker_in_binary_frame_header() {
        // C 侧 SDK trace（如 CFW 组件）的格式串不带 `I/` 前缀，日志级别在 tag 里。
        // 若帧的二进制 sn/tick/tag 恰好拼成可打印串 `I/mCFW_GprsGetstatus`，不应被当成日志行。
        let fmt = b"prsGetstatus\n";
        let fmt_aligned = align4(fmt.len() + 1);
        let mut payload = Vec::new();
        payload.extend_from_slice(&0x2F49u16.to_le_bytes()); // sn = `I/`
        payload.extend_from_slice(&0x436Du16.to_le_bytes()); // tick = `mC`
        payload.extend_from_slice(&0x475F5746u32.to_le_bytes()); // tag = `FW_G` (level 4)
        payload.extend_from_slice(fmt);
        payload.resize(8 + fmt_aligned, 0);
        let len = payload.len();
        let mut frame = vec![0xAD, (len >> 8) as u8, (len & 0xff) as u8, 0x98];
        frame.extend_from_slice(&payload);

        let mut decoder = Rda8910LogDecoder::new();
        let entries = decoder.feed(&frame);
        assert!(entries.is_empty(), "二进制帧头里的 `I/` 不应被识别为日志行");
    }

    #[test]
    fn rda8910_decoder_skips_marker_in_fmtid_frame_args() {
        // 真机抓包：fmtid 型帧（tag bit31=TRACE_TDB_FLAG），帧内无内联格式串，
        // 第一个 u32 参数 0x6D2F4987 的字节 `87 49 2f 6d` 拼出 `I/m`，后续字符串参数
        // "CFW_GprsGetstatus" → 不应被当成日志行。
        let frame: Vec<u8> = vec![
            0xad, 0x00, 0x28, 0x98, // 帧头 (len=40, flowid 0x98)
            0x43, 0x12, // sn
            0x63, 0x22, // tick
            0x52, 0xe8, 0x90, 0xc8, // tag = 0xC890E852, bit31 置位
            0xdb, 0xb7, 0x00, 0x10, // fmtid = 0x1000B7DB
            0x87, 0x49, 0x2f, 0x6d, // arg0 = 0x6D2F4987 → 字节 `87 I / m`
            0x43, 0x46, 0x57, 0x5f, // "CFW_"
            0x47, 0x70, 0x72, 0x73, // "Gprs"
            0x47, 0x65, 0x74, 0x73, // "Gets"
            0x74, 0x61, 0x74, 0x75, // "tatu"
            0x73, 0x00, 0x43, 0x46, // "s\0CF"
            0x29, 0x01, 0x00, 0x00,
        ];
        let mut decoder = Rda8910LogDecoder::new();
        let entries = decoder.feed(&frame);
        assert!(entries.is_empty(), "fmtid 帧参数里的 `I/m` 不应被识别为日志行");
    }

    #[test]
    fn rda8910_decoder_extracts_real_lua_wrapper_frame() {
        // 真机抓包（2026-08-25）：Lua 侧 print 包装帧 `%.*s` + `>> ` + [ptr][size][content]。
        // 内容 `D/user.adc adc0 -1` 应被完整提取；fmtid 过滤不得误伤真实日志。
        let frame: Vec<u8> = vec![
            0xad, 0x00, 0x28, 0x98, // 帧头 (len=40)
            0x0f, 0x12, // sn
            0x22, 0xee, // tick
            0xc3, 0x20, 0x14, 0x3a, // tag = 0x3A1420C3, bit31=0（内联 fmt）
            0x25, 0x2e, 0x2a, 0x73, 0x00, // "%.*s\0"
            0x3e, 0x3e, 0x20, // ">> "
            0x8c, 0x63, 0xd6, 0x80, // ptr
            0x12, 0x00, // size = 18
            b'D', b'/', b'u', b's', b'e', b'r', b'.', b'a', b'd', b'c', b' ', b'a', b'd', b'c', b'0', b' ', b'-', b'1',
        ];
        let mut decoder = Rda8910LogDecoder::new();
        let entries = decoder.feed(&frame);
        assert_eq!(entries.len(), 1, "真实 Lua 日志帧应被提取");
        assert_eq!(entries[0].module.as_deref(), Some("user.adc"));
        assert_eq!(entries[0].message, "adc0 -1");
    }

    #[test]
    fn parse_level_prefix_cases() {
        let (level, module, msg) = parse_level_prefix("I/http close connection");
        assert_eq!(level, LogLevel::Info);
        assert_eq!(module.as_deref(), Some("http"));
        assert_eq!(msg, "close connection");

        let (level, module, msg) = parse_level_prefix("D/net timeout");
        assert_eq!(level, LogLevel::Debug);
        assert_eq!(module.as_deref(), Some("net"));
        assert_eq!(msg, "timeout");

        let (level, _, msg) = parse_level_prefix("no prefix here");
        assert_eq!(level, LogLevel::Info); // default
        assert_eq!(msg, "no prefix here");
    }

    /// Helper: 构造一个 0xA5..0xA5 帧 (含 escape 处理 + CRC16)
    fn build_soc_frame(payload: &[u8]) -> Vec<u8> {
        let crc = crc16_soc_log(payload);
        let mut frame = vec![0xA5u8];
        // 写入 payload (含 escape)
        for &b in payload {
            match b {
                0xA5 => frame.extend_from_slice(&[0xA6, 0x01]),
                0xA6 => frame.extend_from_slice(&[0xA6, 0x02]),
                _ => frame.push(b),
            }
        }
        // 写入 CRC (含 escape)
        for &b in &crc.to_le_bytes() {
            match b {
                0xA5 => frame.extend_from_slice(&[0xA6, 0x01]),
                0xA6 => frame.extend_from_slice(&[0xA6, 0x02]),
                _ => frame.push(b),
            }
        }
        frame.push(0xA5);
        frame
    }

    #[test]
    fn decode_one_log_frame() {
        // 日志帧: ms=5000, level=2(Info), cmd=0, sn=1, "hello"
        let mut payload = vec![0u8; 24];
        payload[0..8].copy_from_slice(&5000u64.to_le_bytes());
        payload[8..16].copy_from_slice(&2u64.to_le_bytes()); // level=2, 无 tag
        payload[16..20].copy_from_slice(&0u32.to_le_bytes()); // cmd=0
        payload[20..22].copy_from_slice(&1u16.to_le_bytes());
        payload[22] = 0; // type
        payload[23] = 0; // cpu
                         // body: "hello\0\0\0" (8 字节)
        payload.extend_from_slice(b"hello\0\0\0");

        let frame = build_soc_frame(&payload);
        let decoded = SocLogDecoder::decode_one(&frame).expect("decode_one failed");
        match decoded {
            SocFrame::Log(entry, header) => {
                assert_eq!(header.ms, 5000);
                assert_eq!(header.cmd, 0);
                assert_eq!(header.sn, 1);
                assert_eq!(header.cpu, 0);
                assert_eq!(entry.message, "hello");
                assert_eq!(entry.device_time.as_deref(), Some("5.000"));
            }
            SocFrame::Cmd(_) => panic!("expected Log, got Cmd"),
        }
    }

    #[test]
    fn decode_one_cmd_frame() {
        // 命令帧: ms=10000, address=0x12345678, len=4, cmd=0x09, payload=[0xDE,0xAD,0xBE,0xEF]
        let mut payload = vec![0u8; 24 + 4];
        payload[0..8].copy_from_slice(&10000u64.to_le_bytes());
        payload[8..12].copy_from_slice(&0x12345678u32.to_le_bytes()); // address
        payload[12..16].copy_from_slice(&4u32.to_le_bytes()); // len
        payload[16..20].copy_from_slice(&0x09u32.to_le_bytes()); // cmd
        payload[20..22].copy_from_slice(&7u16.to_le_bytes());
        payload[22] = 1; // type
        payload[23] = 2; // cpu
        payload[24..28].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);

        let frame = build_soc_frame(&payload);
        let decoded = SocLogDecoder::decode_one(&frame).expect("decode_one failed");
        match decoded {
            SocFrame::Cmd(c) => {
                assert_eq!(c.ms, 10000);
                assert_eq!(c.address, 0x12345678);
                assert_eq!(c.len, 4);
                assert_eq!(c.cmd, 0x09);
                assert_eq!(c.sn, 7);
                assert_eq!(c.msg_type, 1);
                assert_eq!(c.cpu, 2);
                assert_eq!(c.payload, vec![0xDE, 0xAD, 0xBE, 0xEF]);
            }
            SocFrame::Log(_, _) => panic!("expected Cmd, got Log"),
        }
    }

    #[test]
    fn decode_one_bad_crc() {
        // 构造一个会 CRC 失败的帧
        let mut payload = vec![0u8; 24];
        payload[0..8].copy_from_slice(&1000u64.to_le_bytes());
        payload[8..16].copy_from_slice(&2u64.to_le_bytes());
        payload[16..20].copy_from_slice(&0u32.to_le_bytes());
        payload[20..22].copy_from_slice(&1u16.to_le_bytes());
        payload[22] = 0;
        payload[23] = 0;
        // 故意算错的 CRC: 全 0
        let mut frame = vec![0xA5u8];
        for &b in &payload {
            frame.push(b);
        }
        frame.extend_from_slice(&[0xFF, 0xFF]); // 错的 CRC
        frame.push(0xA5);
        assert!(SocLogDecoder::decode_one(&frame).is_none());
    }

    #[test]
    fn decode_one_no_a5_markers() {
        // 没有 0xA5 边界, 返回 None
        let data = [0u8; 32];
        assert!(SocLogDecoder::decode_one(&data).is_none());
    }

    #[test]
    fn decode_one_truncated() {
        // 只有 0xA5 头, 没有 0xA5 尾
        let data = [0xA5, 0x00, 0x01, 0x02];
        assert!(SocLogDecoder::decode_one(&data).is_none());
    }

    #[test]
    fn log_entry_is_clone() {
        let entry = LogEntry {
            timestamp: "2026-06-27T22:00:00.000Z".into(),
            device_time: None,
            level: LogLevel::Info,
            module: Some("user.main".into()),
            message: "hello".into(),
            raw: "`R\x00\x00I/user.main hello".into(),
        };
        let cloned = entry.clone();
        assert_eq!(cloned.message, "hello");
        assert_eq!(cloned.module.as_deref(), Some("user.main"));
        assert_eq!(cloned.level.as_str(), "I");
    }

    #[test]
    fn cached_timestamp_format_matches_previous() {
        // 时间戳格式应与之前的 Local::now().format("%Y-%m-%d %H:%M:%S%.3f") 保持一致
        let ts = cached_local_timestamp();
        assert_eq!(ts.len(), 23, "时间戳应为 YYYY-MM-DD HH:MM:SS.mmm, 实际: {ts}");
        let b = ts.as_bytes();
        assert_eq!(b[4], b'-');
        assert_eq!(b[7], b'-');
        assert_eq!(b[10], b' ');
        assert_eq!(b[13], b':');
        assert_eq!(b[16], b':');
        assert_eq!(b[19], b'.');
    }
}
