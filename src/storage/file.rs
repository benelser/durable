//! File-based durable storage backend.
//!
//! Uses a directory structure with atomic writes (write-to-temp-then-rename)
//! for crash safety. Each step result lives in its own file.
//!
//! Layout:
//!   {base_dir}/
//!     executions/
//!       {execution_id}/
//!         meta.json         — execution metadata
//!         steps/
//!           {step_num}_{hash}.json  — individual step records
//!         signals/
//!           {name}.json     — signal data
//!         timers/
//!           {name}.json     — timer fire time

use crate::core::error::SuspendReason;
use crate::core::time::now_millis;
use crate::core::types::*;
use crate::json::{self, FromJson, ToJson, Value};
use crate::storage::ExecutionLog;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Durable file-based storage with crash-safe atomic writes.
pub struct FileStorage {
    base_dir: PathBuf,
}

impl FileStorage {
    /// Create a new FileStorage rooted at the given directory.
    pub fn new(base_dir: impl Into<PathBuf>) -> Result<Self, String> {
        let base_dir = base_dir.into();
        fs::create_dir_all(base_dir.join("executions"))
            .map_err(|e| format!("failed to create storage dir: {}", e))?;
        Ok(Self { base_dir })
    }

    /// Remove orphaned .tmp files older than `max_age`.
    /// These are leftovers from interrupted atomic writes.
    pub fn cleanup_tmp_files(&self, max_age: std::time::Duration) -> Result<u64, String> {
        let mut count = 0u64;
        cleanup_tmp_recursive(&self.base_dir, max_age, &mut count)?;
        Ok(count)
    }

    fn exec_dir(&self, id: ExecutionId) -> PathBuf {
        self.base_dir.join("executions").join(id.to_string())
    }

    fn steps_dir(&self, id: ExecutionId) -> PathBuf {
        self.exec_dir(id).join("steps")
    }

    fn signals_dir(&self, id: ExecutionId) -> PathBuf {
        self.exec_dir(id).join("signals")
    }

    fn timers_dir(&self, id: ExecutionId) -> PathBuf {
        self.exec_dir(id).join("timers")
    }

    fn meta_path(&self, id: ExecutionId) -> PathBuf {
        self.exec_dir(id).join("meta.json")
    }

    fn step_path(&self, key: &StepKey) -> PathBuf {
        self.steps_dir(key.execution_id)
            .join(format!("{}_{:016x}.json", key.step_number, key.param_hash))
    }

    /// Atomic write: write to a PID-unique temp file then rename.
    ///
    /// Uses `{path}.{pid}.{counter}.tmp` to avoid TOCTOU races when
    /// multiple processes target the same file.
    fn atomic_write(&self, path: &Path, content: &str) -> Result<(), String> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create dir {:?}: {}", parent, e))?;
        }

        // PID-unique temp file name prevents races between concurrent processes
        let pid = std::process::id();
        let seq = COUNTER.fetch_add(1, Ordering::SeqCst);
        let tmp_name = format!(
            "{}.{}.{}.tmp",
            path.file_name().unwrap_or_default().to_string_lossy(),
            pid,
            seq,
        );
        let tmp_path = path.with_file_name(tmp_name);

        let mut file = fs::File::create(&tmp_path)
            .map_err(|e| format!("failed to create temp file {:?}: {}", tmp_path, e))?;
        file.write_all(content.as_bytes())
            .map_err(|e| format!("failed to write temp file: {}", e))?;
        file.sync_all()
            .map_err(|e| format!("failed to sync: {}", e))?;
        fs::rename(&tmp_path, path)
            .map_err(|e| format!("failed to rename {:?} -> {:?}: {}", tmp_path, path, e))?;

        // Fsync parent directory to ensure the rename is durable (Unix only).
        // Windows does not support directory fsync — FlushFileBuffers on the
        // file itself (sync_all above) is sufficient.
        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            if let Ok(dir) = fs::File::open(parent) {
                let _ = dir.sync_all();
            }
        }
        Ok(())
    }

    fn read_json(&self, path: &Path) -> Result<Option<Value>, String> {
        match fs::read_to_string(path) {
            Ok(content) => {
                let val = json::parse(&content).map_err(|e| e.to_string())?;
                Ok(Some(val))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("failed to read {:?}: {}", path, e)),
        }
    }

    fn write_meta(&self, meta: &ExecutionMetadata) -> Result<(), String> {
        let json_str = json::to_string_pretty(&meta.to_json());
        self.atomic_write(&self.meta_path(meta.id), &json_str)
    }

    fn read_meta(&self, id: ExecutionId) -> Result<Option<ExecutionMetadata>, String> {
        let val = self.read_json(&self.meta_path(id))?;
        match val {
            None => Ok(None),
            Some(v) => {
                let meta = parse_execution_metadata(&v)?;
                Ok(Some(meta))
            }
        }
    }

    fn write_step_record(&self, record: &StepRecord) -> Result<(), String> {
        let json_str = json::to_string_pretty(&record.to_json());
        self.atomic_write(&self.step_path(&record.key), &json_str)
    }

    fn read_step_record(&self, path: &Path) -> Result<Option<StepRecord>, String> {
        let val = self.read_json(path)?;
        match val {
            None => Ok(None),
            Some(v) => {
                let record = parse_step_record(&v)?;
                Ok(Some(record))
            }
        }
    }
}

impl ExecutionLog for FileStorage {
    fn create_execution(&self, id: ExecutionId) -> Result<(), String> {
        let now = now_millis();
        let meta = ExecutionMetadata {
            id,
            status: ExecutionStatus::Running,
            created_at: now,
            updated_at: now,
            step_count: 0,
            suspend_reason: None,
            tags: BTreeMap::new(),
        };
        fs::create_dir_all(self.steps_dir(id))
            .map_err(|e| format!("failed to create steps dir: {}", e))?;
        fs::create_dir_all(self.signals_dir(id))
            .map_err(|e| format!("failed to create signals dir: {}", e))?;
        fs::create_dir_all(self.timers_dir(id))
            .map_err(|e| format!("failed to create timers dir: {}", e))?;
        self.write_meta(&meta)
    }

    fn get_execution(&self, id: ExecutionId) -> Result<Option<ExecutionMetadata>, String> {
        self.read_meta(id)
    }

    fn update_execution_status(
        &self,
        id: ExecutionId,
        status: ExecutionStatus,
    ) -> Result<(), String> {
        let mut meta = self
            .read_meta(id)?
            .ok_or_else(|| format!("execution {} not found", id))?;
        meta.status = status;
        meta.updated_at = now_millis();
        self.write_meta(&meta)
    }

    fn set_suspend_reason(
        &self,
        id: ExecutionId,
        reason: Option<SuspendReason>,
    ) -> Result<(), String> {
        let mut meta = self
            .read_meta(id)?
            .ok_or_else(|| format!("execution {} not found", id))?;
        meta.suspend_reason = reason;
        meta.updated_at = now_millis();
        self.write_meta(&meta)
    }

    fn list_executions(
        &self,
        status: Option<ExecutionStatus>,
    ) -> Result<Vec<ExecutionMetadata>, String> {
        let exec_dir = self.base_dir.join("executions");
        let entries = match fs::read_dir(&exec_dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("failed to read executions dir: {}", e)),
        };
        let mut result = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name().to_string_lossy().to_string();
            if let Ok(uuid) = crate::core::uuid::Uuid::parse(&name) {
                let exec_id = ExecutionId(uuid);
                if let Some(meta) = self.read_meta(exec_id)? {
                    match &status {
                        Some(st) if meta.status != *st => continue,
                        _ => result.push(meta),
                    }
                }
            }
        }
        result.sort_by_key(|m| m.created_at);
        Ok(result)
    }

    fn log_step_start(&self, key: StepKey) -> Result<StepRecord, String> {
        let now = now_millis();
        let record = StepRecord {
            key: key.clone(),
            status: StepStatus::Running,
            result: None,
            error: None,
            retryable: false,
            attempts: 1,
            started_at: now,
            completed_at: None,
        };
        self.write_step_record(&record)?;
        // Update step count in metadata
        if let Some(mut meta) = self.read_meta(key.execution_id)? {
            meta.step_count = meta.step_count.max(key.step_number + 1);
            meta.updated_at = now;
            self.write_meta(&meta)?;
        }
        Ok(record)
    }

    fn log_step_completion(
        &self,
        key: &StepKey,
        result: Option<String>,
        error: Option<String>,
        retryable: bool,
    ) -> Result<(), String> {
        let path = self.step_path(key);
        let mut record = self
            .read_step_record(&path)?
            .ok_or_else(|| format!("step {} not found", key))?;
        record.status = if error.is_some() {
            StepStatus::Failed
        } else {
            StepStatus::Completed
        };
        record.result = result;
        record.error = error;
        record.retryable = retryable;
        record.completed_at = Some(now_millis());
        self.write_step_record(&record)
    }

    fn get_step(&self, key: &StepKey) -> Result<Option<StepRecord>, String> {
        self.read_step_record(&self.step_path(key))
    }

    fn get_step_by_number(
        &self,
        execution_id: ExecutionId,
        step_number: u64,
    ) -> Result<Option<StepRecord>, String> {
        let steps_dir = self.steps_dir(execution_id);
        let prefix = format!("{}_", step_number);
        let entries = match fs::read_dir(&steps_dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(&prefix) && name.ends_with(".json") {
                return self.read_step_record(&entry.path());
            }
        }
        Ok(None)
    }

    fn get_steps(&self, execution_id: ExecutionId) -> Result<Vec<StepRecord>, String> {
        let steps_dir = self.steps_dir(execution_id);
        let entries = match fs::read_dir(&steps_dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.to_string()),
        };
        let mut records = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".json") && !name.ends_with(".tmp") {
                if let Some(record) = self.read_step_record(&entry.path())? {
                    records.push(record);
                }
            }
        }
        records.sort_by_key(|r| r.key.step_number);
        Ok(records)
    }

    fn store_signal(
        &self,
        execution_id: ExecutionId,
        name: &str,
        data: &str,
    ) -> Result<(), String> {
        let path = self.signals_dir(execution_id).join(format!("{}.json", name));
        self.atomic_write(&path, data)
    }

    fn consume_signal(
        &self,
        execution_id: ExecutionId,
        name: &str,
    ) -> Result<Option<String>, String> {
        let path = self.signals_dir(execution_id).join(format!("{}.json", name));
        // Atomic consumption via rename: prevents TOCTOU race where two
        // threads both read the file before either deletes it.
        let consumed_path = path.with_extension("consumed");
        match fs::rename(&path, &consumed_path) {
            Ok(()) => {
                let data = fs::read_to_string(&consumed_path)
                    .map_err(|e| format!("failed to read consumed signal: {}", e))?;
                let _ = fs::remove_file(&consumed_path);
                Ok(Some(data))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn peek_signal(
        &self,
        execution_id: ExecutionId,
        name: &str,
    ) -> Result<Option<String>, String> {
        let path = self.signals_dir(execution_id).join(format!("{}.json", name));
        match fs::read_to_string(&path) {
            Ok(data) => Ok(Some(data)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn create_timer(
        &self,
        execution_id: ExecutionId,
        name: &str,
        fire_at_millis: u64,
    ) -> Result<(), String> {
        let path = self.timers_dir(execution_id).join(format!("{}.json", name));
        let data = json::to_string(&json::json_object(vec![
            ("execution_id", execution_id.to_json()),
            ("name", json::json_str(name)),
            ("fire_at_millis", json::json_num(fire_at_millis as f64)),
        ]));
        self.atomic_write(&path, &data)
    }

    fn get_expired_timers(&self) -> Result<Vec<(ExecutionId, String, u64)>, String> {
        let now = now_millis();
        let exec_dir = self.base_dir.join("executions");
        let entries = match fs::read_dir(&exec_dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.to_string()),
        };
        let mut expired = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name().to_string_lossy().to_string();
            if let Ok(uuid) = crate::core::uuid::Uuid::parse(&name) {
                let exec_id = ExecutionId(uuid);
                let timers_dir = self.timers_dir(exec_id);
                if let Ok(timer_entries) = fs::read_dir(&timers_dir) {
                    for te in timer_entries {
                        let te = te.map_err(|e| e.to_string())?;
                        if let Some(val) = self.read_json(&te.path())? {
                            let fire_at = val
                                .get("fire_at_millis")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(u64::MAX);
                            if fire_at <= now {
                                let timer_name = val
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                expired.push((exec_id, timer_name, fire_at));
                            }
                        }
                    }
                }
            }
        }
        Ok(expired)
    }

    fn delete_timer(&self, execution_id: ExecutionId, name: &str) -> Result<(), String> {
        let path = self.timers_dir(execution_id).join(format!("{}.json", name));
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn set_tag(&self, execution_id: ExecutionId, key: &str, value: &str) -> Result<(), String> {
        if let Some(mut meta) = self.read_meta(execution_id)? {
            meta.tags.insert(key.to_string(), value.to_string());
            meta.updated_at = now_millis();
            self.write_meta(&meta)?;
        }
        Ok(())
    }

    fn get_tag(&self, execution_id: ExecutionId, key: &str) -> Result<Option<String>, String> {
        if let Some(meta) = self.read_meta(execution_id)? {
            Ok(meta.tags.get(key).cloned())
        } else {
            Ok(None)
        }
    }

    fn delete_execution(&self, id: ExecutionId) -> Result<(), String> {
        let dir = self.exec_dir(id);
        match fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("failed to delete execution {}: {}", id, e)),
        }
    }

    fn cleanup_older_than(&self, age_millis: u64) -> Result<u64, String> {
        let cutoff = crate::core::time::now_millis().saturating_sub(age_millis);
        let execs = self.list_executions(None)?;
        let mut count = 0u64;
        for exec in execs {
            if exec.created_at < cutoff {
                self.delete_execution(exec.id)?;
                count += 1;
            }
        }
        Ok(count)
    }
}

// -- JSON parsing helpers --

fn parse_execution_metadata(v: &Value) -> Result<ExecutionMetadata, String> {
    Ok(ExecutionMetadata {
        id: ExecutionId::from_json(v.get("id").unwrap_or(&Value::Null))?,
        status: ExecutionStatus::from_json(v.get("status").unwrap_or(&Value::Null))?,
        created_at: v.get("created_at").and_then(|v| v.as_u64()).unwrap_or(0),
        updated_at: v.get("updated_at").and_then(|v| v.as_u64()).unwrap_or(0),
        step_count: v.get("step_count").and_then(|v| v.as_u64()).unwrap_or(0),
        suspend_reason: v
            .get("suspend_reason")
            .filter(|v| !v.is_null())
            .map(SuspendReason::from_json)
            .transpose()?,
        tags: v
            .get("tags")
            .and_then(|v| v.as_object())
            .map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn parse_step_record(v: &Value) -> Result<StepRecord, String> {
    let key_val = v.get("key").ok_or("missing key in step record")?;
    let key = StepKey {
        execution_id: ExecutionId::from_json(
            key_val.get("execution_id").unwrap_or(&Value::Null),
        )?,
        step_number: key_val
            .get("step_number")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        step_name: key_val
            .get("step_name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        param_hash: key_val
            .get("param_hash")
            .and_then(|v| v.as_str())
            .and_then(|s| u64::from_str_radix(s, 16).ok())
            .unwrap_or(0),
    };
    Ok(StepRecord {
        key,
        status: StepStatus::from_json(v.get("status").unwrap_or(&Value::Null))?,
        result: v.get("result").and_then(|v| v.as_str()).map(|s| s.to_string()),
        error: v.get("error").and_then(|v| v.as_str()).map(|s| s.to_string()),
        retryable: v.get("retryable").and_then(|v| v.as_bool()).unwrap_or(false),
        attempts: v.get("attempts").and_then(|v| v.as_u64()).unwrap_or(1) as u32,
        started_at: v.get("started_at").and_then(|v| v.as_u64()).unwrap_or(0),
        completed_at: v.get("completed_at").and_then(|v| v.as_u64()),
    })
}

fn cleanup_tmp_recursive(
    dir: &Path,
    max_age: std::time::Duration,
    count: &mut u64,
) -> Result<(), String> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.to_string()),
    };
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.is_dir() {
            cleanup_tmp_recursive(&path, max_age, count)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("tmp") {
            if let Ok(meta) = fs::metadata(&path) {
                if let Ok(modified) = meta.modified() {
                    if let Ok(age) = modified.elapsed() {
                        if age > max_age {
                            let _ = fs::remove_file(&path);
                            *count += 1;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
