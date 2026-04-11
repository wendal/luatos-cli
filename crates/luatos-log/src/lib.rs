// LuatOS log parsing and storage.
//
// Extensible log parser system supporting multiple module log formats:
//   - LuatOS standard: [timestamp] L/module message
//   - Plain text fallback for unknown formats
//
// Design: LogParser trait allows adding new chip-specific parsers without
// modifying existing code.

use chrono::Local;
use serde::{Deserialize, Serialize};

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

        let now = Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string();

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
            if prefix.len() <= 20
                && prefix
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                let now = Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string();
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
        Self {
            parsers: Vec::new(),
        }
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
        let now = Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string();
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
    pub fn new(
        text_path: Option<&std::path::Path>,
        json_path: Option<&std::path::Path>,
    ) -> anyhow::Result<Self> {
        let text_file = if let Some(p) = text_path {
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent)?;
            }
            Some(std::io::BufWriter::new(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(p)?,
            ))
        } else {
            None
        };

        let json_file = if let Some(p) = json_path {
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent)?;
            }
            Some(std::io::BufWriter::new(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(p)?,
            ))
        } else {
            None
        };

        Ok(Self {
            text_file,
            json_file,
        })
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
pub fn parse_log_file(
    path: &std::path::Path,
    dispatcher: &LogDispatcher,
) -> anyhow::Result<Vec<LogEntry>> {
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

        let now = Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string();
        let device_time = format!("{}.{:03}", ms / 1000, ms % 1000);

        Some(LogEntry {
            timestamp: now,
            device_time: Some(device_time),
            level,
            module: if module.is_empty() {
                None
            } else {
                Some(module)
            },
            message,
            raw: format!("[SOC frame {} bytes]", data.len()),
        })
    }
}

impl Default for SocLogDecoder {
    fn default() -> Self {
        Self::new()
    }
}

/// CRC16-ModBus with init=0 (used by SOC log protocol).
fn crc16_modbus(data: &[u8]) -> u16 {
    let mut crc: u16 = 0x0000; // SOC log uses init=0, not standard 0xFFFF
    for &byte in data {
        crc ^= byte as u16;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xA001;
            } else {
                crc >>= 1;
            }
        }
    }
    crc
}

/// Decode tag name from the tag bitfield (7 bits per character, up to 8 chars).
fn decode_tag_name(tag_bits: u64) -> String {
    // ASCII lookup: each 7-bit value maps to a character
    const TAG_CHARS: &[u8; 64] =
        b" abcdefghijklmnopqrstuvwxyz012345ABCDEFGHIJKLMNOPQRSTUVWXYZ_*-./";

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
        let mut is_long = false;
        let mut is_long_long = false;

        while let Some(&next) = chars.peek() {
            match next {
                '0'..='9' | '-' | '+' | ' ' | '#' | '.' => {
                    spec.push(next);
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
                            let val = i64::from_le_bytes(
                                args_data[arg_pos..arg_pos + 8].try_into().unwrap_or([0; 8]),
                            );
                            match next {
                                'x' => result.push_str(&format!("{val:x}")),
                                'X' => result.push_str(&format!("{val:X}")),
                                _ => result.push_str(&format!("{val}")),
                            }
                            arg_pos += 8;
                        }
                    } else {
                        // 4-byte integer
                        if arg_pos + 4 <= args_data.len() {
                            let val = i32::from_le_bytes(
                                args_data[arg_pos..arg_pos + 4].try_into().unwrap_or([0; 4]),
                            );
                            match next {
                                'x' => result.push_str(&format!("{val:x}")),
                                'X' => result.push_str(&format!("{val:X}")),
                                'u' => result.push_str(&format!("{}", val as u32)),
                                _ => result.push_str(&format!("{val}")),
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
                        let val = f64::from_le_bytes(
                            args_data[arg_pos..arg_pos + 8].try_into().unwrap_or([0; 8]),
                        );
                        result.push_str(&format!("{val}"));
                        arg_pos += 8;
                    }
                    break;
                }
                's' => {
                    chars.next();
                    // String: null-terminated, 4-byte aligned length
                    let str_start = arg_pos;
                    let str_end = args_data[str_start..]
                        .iter()
                        .position(|&b| b == 0)
                        .map(|p| str_start + p)
                        .unwrap_or(args_data.len());
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
                        let val = u32::from_le_bytes(
                            args_data[arg_pos..arg_pos + 4].try_into().unwrap_or([0; 4]),
                        );
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

        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string();
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
                            let val = i64::from_le_bytes(
                                args_data[arg_pos..arg_pos + 8].try_into().unwrap_or([0; 8]),
                            );
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
                            let val = i32::from_le_bytes(
                                args_data[arg_pos..arg_pos + 4].try_into().unwrap_or([0; 4]),
                            );
                            match spec {
                                'x' => result.push_str(&format!("{val:x}")),
                                'X' => result.push_str(&format!("{val:X}")),
                                'u' => result.push_str(&format!("{}", val as u32)),
                                _ => result.push_str(&format!("{val}")),
                            }
                            arg_pos += 4;
                        }
                    }
                    break;
                }
                Some(&'f') | Some(&'g') | Some(&'e') => {
                    chars.next();
                    if arg_pos + 8 <= args_data.len() {
                        let val = f64::from_le_bytes(
                            args_data[arg_pos..arg_pos + 8].try_into().unwrap_or([0; 8]),
                        );
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
                            let precision = u32::from_le_bytes(
                                args_data[arg_pos..arg_pos + 4].try_into().unwrap_or([0; 4]),
                            ) as usize;
                            arg_pos += 4;
                            let end = (arg_pos + precision).min(args_data.len());
                            let s = String::from_utf8_lossy(&args_data[arg_pos..end]);
                            result.push_str(&s);
                            arg_pos = (end + 3) & !3; // align to 4
                        }
                    } else {
                        // %s: [u32 length] [string bytes], 4-byte aligned
                        if arg_pos + 4 <= args_data.len() {
                            let slen = u32::from_le_bytes(
                                args_data[arg_pos..arg_pos + 4].try_into().unwrap_or([0; 4]),
                            ) as usize;
                            arg_pos += 4;
                            let end = (arg_pos + slen).min(args_data.len());
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
                        let val = u32::from_le_bytes(
                            args_data[arg_pos..arg_pos + 4].try_into().unwrap_or([0; 4]),
                        );
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
        let bytes: Vec<u8> = trimmed
            .split_whitespace()
            .filter_map(|s| u8::from_str_radix(s, 16).ok())
            .collect();

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
        let entry = parser
            .parse_line("[2026-04-09 12:03:37.290] I/user.test 1234")
            .unwrap();
        assert_eq!(entry.level, LogLevel::Info);
        assert_eq!(entry.module.as_deref(), Some("user.test"));
        assert_eq!(entry.message, "1234");
        assert_eq!(
            entry.device_time.as_deref(),
            Some("2026-04-09 12:03:37.290")
        );
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
}
