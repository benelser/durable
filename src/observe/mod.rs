//! Observability — query execution state without mutation.

use crate::core::error::{DurableResult, DurableError};
use crate::core::types::*;
use crate::json::{self, ToJson, Value};
use crate::storage::ExecutionLog;
use std::sync::Arc;

/// Read-only view into execution state for monitoring and debugging.
pub struct ExecutionInspector {
    storage: Arc<dyn ExecutionLog>,
}

impl ExecutionInspector {
    pub fn new(storage: Arc<dyn ExecutionLog>) -> Self {
        Self { storage }
    }

    /// Get execution metadata.
    pub fn get_execution(&self, id: ExecutionId) -> DurableResult<Option<ExecutionMetadata>> {
        self.storage.get_execution(id).map_err(DurableError::Storage)
    }

    /// List all executions, optionally filtered by status.
    pub fn list_executions(
        &self,
        status: Option<ExecutionStatus>,
    ) -> DurableResult<Vec<ExecutionMetadata>> {
        self.storage.list_executions(status).map_err(DurableError::Storage)
    }

    /// Get all steps for an execution.
    pub fn get_steps(&self, exec_id: ExecutionId) -> DurableResult<Vec<StepRecord>> {
        self.storage.get_steps(exec_id).map_err(DurableError::Storage)
    }

    /// Get a specific step by number.
    pub fn get_step(
        &self,
        exec_id: ExecutionId,
        step_number: u64,
    ) -> DurableResult<Option<StepRecord>> {
        self.storage
            .get_step_by_number(exec_id, step_number)
            .map_err(DurableError::Storage)
    }

    /// Get the current execution status as a JSON summary.
    pub fn execution_summary(&self, id: ExecutionId) -> DurableResult<Value> {
        let meta = self
            .storage
            .get_execution(id)
            .map_err(DurableError::Storage)?
            .ok_or_else(|| DurableError::NotFound(format!("execution {}", id)))?;

        let steps = self.storage.get_steps(id).map_err(DurableError::Storage)?;

        let completed = steps.iter().filter(|s| s.status == StepStatus::Completed).count();
        let failed = steps.iter().filter(|s| s.status == StepStatus::Failed).count();
        let running = steps.iter().filter(|s| s.status == StepStatus::Running).count();

        let current_step = steps.last().map(|s| {
            json::json_object(vec![
                ("number", json::json_num(s.key.step_number as f64)),
                ("name", json::json_str(&s.key.step_name)),
                ("status", s.status.to_json()),
            ])
        });

        Ok(json::json_object(vec![
            ("execution_id", id.to_json()),
            ("status", meta.status.to_json()),
            ("total_steps", json::json_num(steps.len() as f64)),
            ("completed_steps", json::json_num(completed as f64)),
            ("failed_steps", json::json_num(failed as f64)),
            ("running_steps", json::json_num(running as f64)),
            (
                "current_step",
                current_step.unwrap_or(Value::Null),
            ),
            (
                "suspend_reason",
                meta.suspend_reason
                    .as_ref()
                    .map(|r| r.to_json())
                    .unwrap_or(Value::Null),
            ),
            ("created_at", json::json_num(meta.created_at as f64)),
            ("updated_at", json::json_num(meta.updated_at as f64)),
        ]))
    }

    /// Get the full step history as JSON.
    pub fn step_history(&self, exec_id: ExecutionId) -> DurableResult<Value> {
        let steps = self.storage.get_steps(exec_id).map_err(DurableError::Storage)?;
        Ok(json::json_array(steps.iter().map(|s| s.to_json()).collect()))
    }

    /// Get conversation history from execution tags.
    pub fn conversation_history(&self, exec_id: ExecutionId) -> DurableResult<Option<Value>> {
        match self
            .storage
            .get_tag(exec_id, "conversation")
            .map_err(DurableError::Storage)?
        {
            Some(json_str) => {
                let val = json::parse(&json_str)?;
                Ok(Some(val))
            }
            None => Ok(None),
        }
    }
}
