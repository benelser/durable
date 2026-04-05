//! Write-ahead journal for crash-safe storage operations.
//!
//! Operations are logged to a journal file before being applied to main storage.
//! On startup, `replay()` recovers any operations that were logged but not checkpointed.
//! Format: newline-delimited JSON, one operation per line.

use crate::json::{self, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// A journal operation.
#[derive(Clone, Debug)]
pub enum JournalOp {
    WriteStep {
        execution_id: String,
        step_file: String,
        data: String,
    },
    WriteMetadata {
        execution_id: String,
        data: String,
    },
    WriteSignal {
        execution_id: String,
        name: String,
        data: String,
    },
    DeleteSignal {
        execution_id: String,
        name: String,
    },
    WriteTimer {
        execution_id: String,
        name: String,
        data: String,
    },
    DeleteTimer {
        execution_id: String,
        name: String,
    },
}

impl JournalOp {
    fn to_json(&self) -> Value {
        match self {
            JournalOp::WriteStep {
                execution_id,
                step_file,
                data,
            } => json::json_object(vec![
                ("op", json::json_str("write_step")),
                ("execution_id", json::json_str(execution_id)),
                ("step_file", json::json_str(step_file)),
                ("data", json::json_str(data)),
            ]),
            JournalOp::WriteMetadata {
                execution_id,
                data,
            } => json::json_object(vec![
                ("op", json::json_str("write_metadata")),
                ("execution_id", json::json_str(execution_id)),
                ("data", json::json_str(data)),
            ]),
            JournalOp::WriteSignal {
                execution_id,
                name,
                data,
            } => json::json_object(vec![
                ("op", json::json_str("write_signal")),
                ("execution_id", json::json_str(execution_id)),
                ("name", json::json_str(name)),
                ("data", json::json_str(data)),
            ]),
            JournalOp::DeleteSignal {
                execution_id,
                name,
            } => json::json_object(vec![
                ("op", json::json_str("delete_signal")),
                ("execution_id", json::json_str(execution_id)),
                ("name", json::json_str(name)),
            ]),
            JournalOp::WriteTimer {
                execution_id,
                name,
                data,
            } => json::json_object(vec![
                ("op", json::json_str("write_timer")),
                ("execution_id", json::json_str(execution_id)),
                ("name", json::json_str(name)),
                ("data", json::json_str(data)),
            ]),
            JournalOp::DeleteTimer {
                execution_id,
                name,
            } => json::json_object(vec![
                ("op", json::json_str("delete_timer")),
                ("execution_id", json::json_str(execution_id)),
                ("name", json::json_str(name)),
            ]),
        }
    }

    fn from_json(val: &Value) -> Result<Self, String> {
        let op = val
            .get("op")
            .and_then(|v| v.as_str())
            .ok_or("missing op field")?;
        let exec_id = val
            .get("execution_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        match op {
            "write_step" => Ok(JournalOp::WriteStep {
                execution_id: exec_id,
                step_file: val
                    .get("step_file")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                data: val
                    .get("data")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            }),
            "write_metadata" => Ok(JournalOp::WriteMetadata {
                execution_id: exec_id,
                data: val
                    .get("data")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            }),
            "write_signal" => Ok(JournalOp::WriteSignal {
                execution_id: exec_id,
                name: val
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                data: val
                    .get("data")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            }),
            "delete_signal" => Ok(JournalOp::DeleteSignal {
                execution_id: exec_id,
                name: val
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            }),
            "write_timer" => Ok(JournalOp::WriteTimer {
                execution_id: exec_id,
                name: val
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                data: val
                    .get("data")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            }),
            "delete_timer" => Ok(JournalOp::DeleteTimer {
                execution_id: exec_id,
                name: val
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            }),
            other => Err(format!("unknown journal op: {}", other)),
        }
    }
}

/// A write-ahead journal.
pub struct Journal {
    path: PathBuf,
    file: Mutex<fs::File>,
}

impl Journal {
    /// Open or create a journal file.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create journal dir: {}", e))?;
        }
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("failed to open journal: {}", e))?;
        Ok(Self {
            path,
            file: Mutex::new(file),
        })
    }

    /// Append an operation to the journal. Fsyncs for durability.
    pub fn append(&self, op: &JournalOp) -> Result<(), String> {
        let line = json::to_string(&op.to_json());
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        writeln!(file, "{}", line).map_err(|e| format!("journal write failed: {}", e))?;
        file.sync_all()
            .map_err(|e| format!("journal sync failed: {}", e))?;
        Ok(())
    }

    /// Read all operations from the journal (for recovery).
    pub fn replay(path: &Path) -> Result<Vec<JournalOp>, String> {
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("failed to read journal: {}", e)),
        };

        let mut ops = Vec::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match json::parse(line) {
                Ok(val) => match JournalOp::from_json(&val) {
                    Ok(op) => ops.push(op),
                    Err(e) => {
                        eprintln!("warning: skipping malformed journal entry: {}", e);
                    }
                },
                Err(e) => {
                    eprintln!("warning: skipping unparseable journal line: {}", e);
                }
            }
        }
        Ok(ops)
    }

    /// Clear the journal (called after checkpoint — all ops have been applied).
    pub fn checkpoint(&self) -> Result<(), String> {
        let file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        file.set_len(0)
            .map_err(|e| format!("journal truncate failed: {}", e))?;
        Ok(())
    }

    /// Get the journal file path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}
