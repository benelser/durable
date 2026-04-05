//! Structured logging for the durable runtime.
//!
//! JSON-lines format to stderr. Each log entry includes correlation fields
//! (execution_id, step_name) for distributed tracing.

use crate::json::{self, Value};
use crate::core::time::now_iso8601;

/// Log severity level.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Trace = 0,
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
}

impl LogLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Trace => "TRACE",
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
        }
    }
}

/// Trait for log output destinations.
pub trait Logger: Send + Sync {
    fn log(&self, level: LogLevel, fields: &[(&str, &str)], message: &str);
    fn min_level(&self) -> LogLevel;

    fn is_enabled(&self, level: LogLevel) -> bool {
        level >= self.min_level()
    }
}

/// JSON-lines logger that writes to stderr.
pub struct StderrJsonLogger {
    min_level: LogLevel,
}

impl StderrJsonLogger {
    pub fn new(min_level: LogLevel) -> Self {
        Self { min_level }
    }
}

impl Logger for StderrJsonLogger {
    fn log(&self, level: LogLevel, fields: &[(&str, &str)], message: &str) {
        if !self.is_enabled(level) {
            return;
        }
        let mut entries: Vec<(&str, Value)> = vec![
            ("ts", json::json_str(&now_iso8601())),
            ("level", json::json_str(level.as_str())),
            ("msg", json::json_str(message)),
        ];
        for (k, v) in fields {
            entries.push((k, json::json_str(v)));
        }
        let line = json::to_string(&json::json_object(entries));
        eprintln!("{}", line);
    }

    fn min_level(&self) -> LogLevel {
        self.min_level
    }
}

/// Logger that discards all output (for tests).
pub struct NullLogger;

impl Logger for NullLogger {
    fn log(&self, _level: LogLevel, _fields: &[(&str, &str)], _message: &str) {}
    fn min_level(&self) -> LogLevel {
        LogLevel::Error
    }
}

impl Default for StderrJsonLogger {
    fn default() -> Self {
        Self::new(LogLevel::Info)
    }
}
