use serde::Serialize;
use serde_json::{json, Value};

use crate::OutputFormat;

#[derive(Clone, Copy)]
pub enum MessageLevel {
    Info,
    Warn,
    Error,
}

impl MessageLevel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

pub fn emit_message(format: &OutputFormat, command: &str, level: MessageLevel, message: impl AsRef<str>) -> anyhow::Result<()> {
    let message = message.as_ref();
    match format {
        OutputFormat::Text | OutputFormat::Json => {
            eprintln!("{message}");
            Ok(())
        }
        OutputFormat::Jsonl => emit_line(&message_event(command, level, message)),
    }
}

pub fn emit_flash_progress(format: &OutputFormat, command: &str, progress: &luatos_flash::FlashProgress) -> anyhow::Result<()> {
    match format {
        OutputFormat::Text => {
            if progress.percent >= 0.0 {
                eprintln!("[{:>6.1}%] {} — {}", progress.percent, progress.stage, progress.message);
            } else {
                eprintln!("          {}", progress.message);
            }
            Ok(())
        }
        OutputFormat::Json => emit_line(progress),
        OutputFormat::Jsonl => emit_line(&progress_event(command, progress)),
    }
}

pub fn emit_log_entry(format: &OutputFormat, command: &str, entry: &luatos_log::LogEntry) -> anyhow::Result<()> {
    match format {
        OutputFormat::Text => {
            println!("{}", format_log_entry(entry));
            Ok(())
        }
        OutputFormat::Json => emit_line(entry),
        OutputFormat::Jsonl => emit_line(&log_entry_event(command, entry)),
    }
}

pub fn format_log_entry(entry: &luatos_log::LogEntry) -> String {
    let module = entry.module.as_deref().unwrap_or("-");
    let time = entry.device_time.as_deref().unwrap_or(&entry.timestamp);
    format!("[{}] {}/{} {}", time, entry.level, module, entry.message)
}

pub fn emit_result<T: Serialize>(format: &OutputFormat, command: &str, status: &str, data: T) -> anyhow::Result<()> {
    let data = serde_json::to_value(data)?;
    match format {
        OutputFormat::Text => Ok(()),
        OutputFormat::Json => emit_pretty(&result_envelope(command, status, data)),
        OutputFormat::Jsonl => emit_line(&result_event(command, status, data)),
    }
}

pub fn emit_error(format: &OutputFormat, command: Option<&str>, message: &str) -> anyhow::Result<()> {
    match format {
        OutputFormat::Text => {
            eprintln!("Error: {message}");
            Ok(())
        }
        OutputFormat::Json => emit_pretty(&error_envelope(message, command)),
        OutputFormat::Jsonl => emit_line(&error_event(message, command)),
    }
}

pub fn emit_jsonl_event(format: &OutputFormat, event: Value) -> anyhow::Result<()> {
    match format {
        OutputFormat::Jsonl => emit_line(&event),
        OutputFormat::Text | OutputFormat::Json => Ok(()),
    }
}

fn emit_pretty(value: &Value) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn emit_line<T: Serialize>(value: &T) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

fn result_event(command: &str, status: &str, data: Value) -> Value {
    json!({
        "type": "result",
        "command": command,
        "status": status,
        "data": data,
    })
}

fn error_event(message: &str, command: Option<&str>) -> Value {
    json!({
        "type": "error",
        "command": command,
        "message": message,
    })
}

fn message_event(command: &str, level: MessageLevel, message: &str) -> Value {
    json!({
        "type": "message",
        "command": command,
        "level": level.as_str(),
        "message": message,
    })
}

fn progress_event(command: &str, progress: &luatos_flash::FlashProgress) -> Value {
    json!({
        "type": "progress",
        "command": command,
        "stage": progress.stage,
        "percent": progress.percent,
        "message": progress.message,
        "done": progress.done,
        "error": progress.error,
    })
}

fn log_entry_event(command: &str, entry: &luatos_log::LogEntry) -> Value {
    json!({
        "type": "log_entry",
        "command": command,
        "entry": entry,
    })
}

fn result_envelope(command: &str, status: &str, data: Value) -> Value {
    json!({
        "status": status,
        "command": command,
        "data": data,
    })
}

fn error_envelope(message: &str, command: Option<&str>) -> Value {
    json!({
        "status": "error",
        "command": command,
        "error": message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_envelope_has_stable_shape() {
        let value = result_envelope("flash.run", "ok", json!({ "chip": "air8101" }));

        assert_eq!(value["status"], "ok");
        assert_eq!(value["command"], "flash.run");
        assert_eq!(value["data"]["chip"], "air8101");
    }

    #[test]
    fn progress_event_has_type_and_command() {
        let progress = luatos_flash::FlashProgress::info("Writing", 12.5, "sending blocks");
        let value = progress_event("flash.run", &progress);

        assert_eq!(value["type"], "progress");
        assert_eq!(value["command"], "flash.run");
        assert_eq!(value["stage"], "Writing");
    }
}
