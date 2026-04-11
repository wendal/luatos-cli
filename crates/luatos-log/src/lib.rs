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
}
