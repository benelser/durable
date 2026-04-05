//! WalEventStore — EventStore backed by the DurableLog WAL engine.
//!
//! Each execution gets its own `.wal` file. Events are stored as JSON
//! entries in the binary WAL format (CRC-protected, crash-safe).
//!
//! Drop-in replacement for `FileEventStore`:
//! ```rust,ignore
//! // Before:
//! let store = FileEventStore::new("./data")?;
//! // After:
//! let store = WalEventStore::new("./data")?;
//! ```

use crate::core::time::now_millis;
use crate::core::types::ExecutionId;
use crate::json::{self, FromJson, ToJson, Value};
use crate::storage::event::*;
use crate::storage::wal::DurableLog;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

/// EventStore backed by per-execution WAL files.
pub struct WalEventStore {
    base_dir: PathBuf,
    /// Open WAL handles, cached for performance.
    logs: Mutex<BTreeMap<String, DurableLog>>,
}

impl WalEventStore {
    pub fn new(base_dir: impl Into<PathBuf>) -> Result<Self, String> {
        let base_dir = base_dir.into();
        fs::create_dir_all(base_dir.join("wal"))
            .map_err(|e| format!("wal_store: mkdir failed: {}", e))?;
        Ok(Self {
            base_dir,
            logs: Mutex::new(BTreeMap::new()),
        })
    }

    fn wal_path(&self, execution_id: ExecutionId) -> PathBuf {
        self.base_dir.join("wal").join(format!("{}.wal", execution_id))
    }

    fn get_or_open(&self, execution_id: ExecutionId) -> Result<(), String> {
        let key = execution_id.to_string();
        let mut logs = self.logs.lock().unwrap_or_else(|e| e.into_inner());
        if !logs.contains_key(&key) {
            let log = DurableLog::open(self.wal_path(execution_id))?;
            logs.insert(key, log);
        }
        Ok(())
    }

    fn with_log<F, R>(&self, execution_id: ExecutionId, f: F) -> Result<R, String>
    where
        F: FnOnce(&DurableLog) -> Result<R, String>,
    {
        self.get_or_open(execution_id)?;
        let key = execution_id.to_string();
        let logs = self.logs.lock().unwrap_or_else(|e| e.into_inner());
        let log = logs.get(&key).ok_or_else(|| "wal_store: log not found".to_string())?;
        f(log)
    }
}

impl EventStore for WalEventStore {
    fn append(&self, execution_id: ExecutionId, event_type: EventType) -> Result<Event, String> {
        self.get_or_open(execution_id)?;

        let key = execution_id.to_string();
        let logs = self.logs.lock().unwrap_or_else(|e| e.into_inner());
        let log = logs.get(&key).ok_or("wal_store: log not found")?;

        // Build the event
        let event_count = log.len();
        let event = Event {
            event_id: event_count + 1,
            execution_id,
            timestamp: now_millis(),
            event_type,
            idempotency_key: None,
            prev_hash: 0, // WAL uses CRC, not hash chain
            schema_version: CURRENT_SCHEMA_VERSION,
            lamport_ts: 0,
        };

        // Serialize and append
        let json_str = json::to_string(&event.to_json());
        log.append(json_str.as_bytes())?;
        log.commit()?;

        Ok(event)
    }

    fn events(&self, execution_id: ExecutionId) -> Result<Vec<Event>, String> {
        let path = self.wal_path(execution_id);
        if !path.exists() {
            return Ok(Vec::new());
        }

        self.get_or_open(execution_id)?;
        let key = execution_id.to_string();
        let logs = self.logs.lock().unwrap_or_else(|e| e.into_inner());
        let log = logs.get(&key).ok_or("wal_store: log not found")?;

        let entries = log.read_all()?;
        let mut events = Vec::new();
        for entry in entries {
            match json::parse(entry.as_str()) {
                Ok(val) => match Event::from_json(&val) {
                    Ok(event) => events.push(event),
                    Err(_) => continue, // Skip unparseable events
                },
                Err(_) => continue,
            }
        }
        Ok(events)
    }

    fn events_since(
        &self,
        execution_id: ExecutionId,
        after_event_id: u64,
    ) -> Result<Vec<Event>, String> {
        let all = self.events(execution_id)?;
        Ok(all.into_iter().filter(|e| e.event_id > after_event_id).collect())
    }

    fn latest_event_id(&self, execution_id: ExecutionId) -> Result<u64, String> {
        self.with_log(execution_id, |log| Ok(log.len()))
    }

    fn list_execution_ids(&self) -> Result<Vec<ExecutionId>, String> {
        let wal_dir = self.base_dir.join("wal");
        let entries = match fs::read_dir(&wal_dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.to_string()),
        };
        let mut ids = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(uuid_str) = name.strip_suffix(".wal") {
                if let Ok(uuid) = crate::core::uuid::Uuid::parse(uuid_str) {
                    ids.push(ExecutionId::from_uuid(uuid));
                }
            }
        }
        Ok(ids)
    }

    fn compact(&self, execution_id: ExecutionId) -> Result<u64, String> {
        let events = self.events(execution_id)?;
        if events.len() < 20 {
            return Ok(0);
        }

        let state = ExecutionState::from_events(execution_id, &events);
        let state_json = json::to_string(&state.to_json());
        let events_removed = events.len() as u64;

        // Build snapshot event
        let snapshot = Event {
            event_id: events.last().map(|e| e.event_id).unwrap_or(0) + 1,
            execution_id,
            timestamp: now_millis(),
            event_type: EventType::Snapshot {
                state_json: state_json.clone(),
                up_to_event_id: events.last().map(|e| e.event_id).unwrap_or(0),
            },
            idempotency_key: None,
            prev_hash: 0,
            schema_version: CURRENT_SCHEMA_VERSION,
            lamport_ts: 0,
        };
        let snapshot_bytes = json::to_string(&snapshot.to_json());

        self.with_log(execution_id, |log| {
            log.compact_with_snapshot(snapshot_bytes.as_bytes())
        })?;

        Ok(events_removed)
    }
}
