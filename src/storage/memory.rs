//! In-memory storage backend for testing and development.

use crate::core::error::SuspendReason;
use crate::core::time::now_millis;
use crate::core::types::*;
use crate::storage::ExecutionLog;
use std::collections::BTreeMap;
use std::sync::Mutex;

/// Fast, ephemeral in-memory storage. All state is lost on process exit.
pub struct InMemoryStorage {
    state: Mutex<State>,
}

struct State {
    executions: BTreeMap<String, ExecutionMetadata>,
    steps: BTreeMap<String, StepRecord>,
    /// Map from (execution_id, step_number) -> step_key_string
    step_index: BTreeMap<(String, u64), String>,
    signals: BTreeMap<(String, String), String>,
    timers: BTreeMap<(String, String), u64>,
    tags: BTreeMap<(String, String), String>,
}

impl InMemoryStorage {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
                executions: BTreeMap::new(),
                steps: BTreeMap::new(),
                step_index: BTreeMap::new(),
                signals: BTreeMap::new(),
                timers: BTreeMap::new(),
                tags: BTreeMap::new(),
            }),
        }
    }
}

impl Default for InMemoryStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl ExecutionLog for InMemoryStorage {
    fn create_execution(&self, id: ExecutionId) -> Result<(), String> {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let now = now_millis();
        s.executions.insert(
            id.to_string(),
            ExecutionMetadata {
                id,
                status: ExecutionStatus::Running,
                created_at: now,
                updated_at: now,
                step_count: 0,
                suspend_reason: None,
                tags: BTreeMap::new(),
            },
        );
        Ok(())
    }

    fn get_execution(&self, id: ExecutionId) -> Result<Option<ExecutionMetadata>, String> {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        Ok(s.executions.get(&id.to_string()).cloned())
    }

    fn update_execution_status(
        &self,
        id: ExecutionId,
        status: ExecutionStatus,
    ) -> Result<(), String> {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(meta) = s.executions.get_mut(&id.to_string()) {
            meta.status = status;
            meta.updated_at = now_millis();
            Ok(())
        } else {
            Err(format!("execution {} not found", id))
        }
    }

    fn set_suspend_reason(
        &self,
        id: ExecutionId,
        reason: Option<SuspendReason>,
    ) -> Result<(), String> {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(meta) = s.executions.get_mut(&id.to_string()) {
            meta.suspend_reason = reason;
            meta.updated_at = now_millis();
            Ok(())
        } else {
            Err(format!("execution {} not found", id))
        }
    }

    fn list_executions(
        &self,
        status: Option<ExecutionStatus>,
    ) -> Result<Vec<ExecutionMetadata>, String> {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let all = s.executions.values();
        match status {
            Some(ref st) => Ok(all.filter(|m| m.status == *st).cloned().collect()),
            None => Ok(all.cloned().collect()),
        }
    }

    fn log_step_start(&self, key: StepKey) -> Result<StepRecord, String> {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
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
        let key_str = key.to_string();
        s.step_index.insert(
            (key.execution_id.to_string(), key.step_number),
            key_str.clone(),
        );
        s.steps.insert(key_str, record.clone());
        // Update step count
        if let Some(meta) = s.executions.get_mut(&key.execution_id.to_string()) {
            meta.step_count = meta.step_count.max(key.step_number + 1);
            meta.updated_at = now;
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
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let key_str = key.to_string();
        if let Some(record) = s.steps.get_mut(&key_str) {
            record.status = if error.is_some() {
                StepStatus::Failed
            } else {
                StepStatus::Completed
            };
            record.result = result;
            record.error = error;
            record.retryable = retryable;
            record.completed_at = Some(now_millis());
            Ok(())
        } else {
            Err(format!("step {} not found", key_str))
        }
    }

    fn get_step(&self, key: &StepKey) -> Result<Option<StepRecord>, String> {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        Ok(s.steps.get(&key.to_string()).cloned())
    }

    fn get_step_by_number(
        &self,
        execution_id: ExecutionId,
        step_number: u64,
    ) -> Result<Option<StepRecord>, String> {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let idx_key = (execution_id.to_string(), step_number);
        match s.step_index.get(&idx_key) {
            Some(key_str) => Ok(s.steps.get(key_str).cloned()),
            None => Ok(None),
        }
    }

    fn get_steps(&self, execution_id: ExecutionId) -> Result<Vec<StepRecord>, String> {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let prefix = execution_id.to_string();
        let mut steps: Vec<StepRecord> = s
            .steps
            .values()
            .filter(|r| r.key.execution_id.to_string() == prefix)
            .cloned()
            .collect();
        steps.sort_by_key(|r| r.key.step_number);
        Ok(steps)
    }

    fn store_signal(
        &self,
        execution_id: ExecutionId,
        name: &str,
        data: &str,
    ) -> Result<(), String> {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.signals.insert(
            (execution_id.to_string(), name.to_string()),
            data.to_string(),
        );
        Ok(())
    }

    fn consume_signal(
        &self,
        execution_id: ExecutionId,
        name: &str,
    ) -> Result<Option<String>, String> {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        Ok(s.signals
            .remove(&(execution_id.to_string(), name.to_string())))
    }

    fn peek_signal(
        &self,
        execution_id: ExecutionId,
        name: &str,
    ) -> Result<Option<String>, String> {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        Ok(s.signals
            .get(&(execution_id.to_string(), name.to_string()))
            .cloned())
    }

    fn create_timer(
        &self,
        execution_id: ExecutionId,
        name: &str,
        fire_at_millis: u64,
    ) -> Result<(), String> {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.timers.insert(
            (execution_id.to_string(), name.to_string()),
            fire_at_millis,
        );
        Ok(())
    }

    fn get_expired_timers(&self) -> Result<Vec<(ExecutionId, String, u64)>, String> {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let now = now_millis();
        let mut expired = Vec::new();
        for ((exec_id_str, name), fire_at) in &s.timers {
            if *fire_at <= now {
                let exec_id = ExecutionId(crate::core::uuid::Uuid::parse(exec_id_str)?);
                expired.push((exec_id, name.clone(), *fire_at));
            }
        }
        Ok(expired)
    }

    fn delete_timer(&self, execution_id: ExecutionId, name: &str) -> Result<(), String> {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.timers
            .remove(&(execution_id.to_string(), name.to_string()));
        Ok(())
    }

    fn set_tag(&self, execution_id: ExecutionId, key: &str, value: &str) -> Result<(), String> {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.tags.insert(
            (execution_id.to_string(), key.to_string()),
            value.to_string(),
        );
        if let Some(meta) = s.executions.get_mut(&execution_id.to_string()) {
            meta.tags.insert(key.to_string(), value.to_string());
        }
        Ok(())
    }

    fn get_tag(&self, execution_id: ExecutionId, key: &str) -> Result<Option<String>, String> {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        Ok(s.tags
            .get(&(execution_id.to_string(), key.to_string()))
            .cloned())
    }

    fn delete_execution(&self, id: ExecutionId) -> Result<(), String> {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let id_str = id.to_string();
        s.executions.remove(&id_str);
        // Remove associated steps, signals, timers, tags
        s.steps.retain(|_, r| r.key.execution_id.to_string() != id_str);
        s.step_index.retain(|(eid, _), _| *eid != id_str);
        s.signals.retain(|(eid, _), _| *eid != id_str);
        s.timers.retain(|(eid, _), _| *eid != id_str);
        s.tags.retain(|(eid, _), _| *eid != id_str);
        Ok(())
    }

    fn cleanup_older_than(&self, age_millis: u64) -> Result<u64, String> {
        let cutoff = crate::core::time::now_millis().saturating_sub(age_millis);
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let old_ids: Vec<String> = s
            .executions
            .iter()
            .filter(|(_, m)| m.created_at < cutoff)
            .map(|(id, _)| id.clone())
            .collect();
        let count = old_ids.len() as u64;
        for id_str in &old_ids {
            s.executions.remove(id_str);
            s.steps.retain(|_, r| r.key.execution_id.to_string() != *id_str);
            s.step_index.retain(|(eid, _), _| eid != id_str);
            s.signals.retain(|(eid, _), _| eid != id_str);
            s.timers.retain(|(eid, _), _| eid != id_str);
            s.tags.retain(|(eid, _), _| eid != id_str);
        }
        Ok(count)
    }
}
