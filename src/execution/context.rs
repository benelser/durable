//! Execution context — tracks step numbering, caching, and suspension.

use crate::core::cancel::CancellationToken;
use crate::core::error::{DurableError, DurableResult, SuspendReason};
use crate::core::hash::hash_params;
use crate::core::time::now_millis;
use crate::core::types::*;
use crate::json;
use crate::storage::ExecutionLog;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// The execution context for a single agent run.
/// Tracks step numbering, provides memoized step execution, and manages suspension.
pub struct ExecutionContext {
    pub id: ExecutionId,
    storage: Arc<dyn ExecutionLog>,
    step_counter: AtomicU64,
    suspend_reason: Mutex<Option<SuspendReason>>,
    cancel_token: CancellationToken,
}

impl ExecutionContext {
    /// Create a new execution context.
    pub fn new(id: ExecutionId, storage: Arc<dyn ExecutionLog>) -> Self {
        Self {
            id,
            storage,
            step_counter: AtomicU64::new(0),
            suspend_reason: Mutex::new(None),
            cancel_token: CancellationToken::new(),
        }
    }

    /// Create with an external cancellation token.
    pub fn with_cancel_token(
        id: ExecutionId,
        storage: Arc<dyn ExecutionLog>,
        token: CancellationToken,
    ) -> Self {
        Self {
            id,
            storage,
            step_counter: AtomicU64::new(0),
            suspend_reason: Mutex::new(None),
            cancel_token: token,
        }
    }

    /// Resume an existing execution, continuing from where it left off.
    pub fn resume(
        id: ExecutionId,
        storage: Arc<dyn ExecutionLog>,
        step_count: u64,
    ) -> Self {
        Self {
            id,
            storage,
            step_counter: AtomicU64::new(step_count),
            suspend_reason: Mutex::new(None),
            cancel_token: CancellationToken::new(),
        }
    }

    /// Get the cancellation token (for external cancellation).
    pub fn cancel_token(&self) -> &CancellationToken {
        &self.cancel_token
    }

    /// Get the storage backend.
    pub fn storage(&self) -> &Arc<dyn ExecutionLog> {
        &self.storage
    }

    /// Allocate the next step number.
    pub fn next_step(&self) -> u64 {
        self.step_counter.fetch_add(1, Ordering::SeqCst)
    }

    /// Get the current step count.
    pub fn step_count(&self) -> u64 {
        self.step_counter.load(Ordering::SeqCst)
    }

    /// Reset step counter for replay.
    pub fn reset_counter(&self) {
        self.step_counter.store(0, Ordering::SeqCst);
    }

    /// Execute a memoized step. If a cached result exists with matching param_hash,
    /// return it immediately. Otherwise, execute the closure and cache the result.
    pub fn step<F>(
        &self,
        name: &str,
        params: &json::Value,
        f: F,
    ) -> DurableResult<json::Value>
    where
        F: FnOnce() -> DurableResult<json::Value>,
    {
        // Check for cancellation before executing
        if self.cancel_token.is_cancelled() {
            return Err(DurableError::Cancelled);
        }

        let step_number = self.next_step();
        let param_str = json::to_string(params);
        let param_hash = hash_params(&param_str);

        let key = StepKey {
            execution_id: self.id,
            step_number,
            step_name: name.to_string(),
            param_hash,
        };

        // Check for cached result
        match self.storage.get_step(&key) {
            Ok(Some(record)) if record.status == StepStatus::Completed => {
                // Cache hit — return memoized result
                if let Some(ref result_str) = record.result {
                    let val = json::parse(result_str).map_err(|e| {
                        DurableError::Serialization(format!(
                            "failed to parse cached result for step {}: {}",
                            name, e
                        ))
                    })?;
                    return Ok(val);
                }
                return Ok(json::Value::Null);
            }
            Ok(Some(record)) if record.status == StepStatus::Failed && !record.retryable => {
                // Cached permanent failure — re-raise
                return Err(DurableError::StepFailed {
                    step_name: name.to_string(),
                    message: record.error.unwrap_or_else(|| "unknown error".to_string()),
                    retryable: false,
                    execution_id: Some(self.id.to_string()),
                    step_number: Some(step_number),
                });
            }
            _ => {
                // Check if there's a step at this number with different params
                if let Ok(Some(existing)) =
                    self.storage.get_step_by_number(self.id, step_number)
                {
                    if existing.key.param_hash != param_hash
                        && existing.status == StepStatus::Completed
                    {
                        // Parameters changed — need to re-execute
                        // (the old cached result is stale)
                    }
                }
            }
        }

        // Cache miss — execute the step
        self.storage.log_step_start(key.clone()).map_err(|e| {
            DurableError::Storage(format!("failed to log step start: {}", e))
        })?;

        match f() {
            Ok(result) => {
                let result_str = json::to_string(&result);
                self.storage
                    .log_step_completion(&key, Some(result_str), None, false)
                    .map_err(|e| {
                        DurableError::Storage(format!("failed to log step completion: {}", e))
                    })?;
                Ok(result)
            }
            Err(err) => {
                let retryable = match &err {
                    DurableError::Suspended(_) => {
                        // Suspension is not an error — don't cache it as failure
                        return Err(err);
                    }
                    DurableError::StepFailed { retryable, .. } => *retryable,
                    DurableError::ToolError { retryable, .. } => *retryable,
                    DurableError::LlmError { retryable, .. } => *retryable,
                    _ => false,
                };
                let err_msg = err.to_string();
                self.storage
                    .log_step_completion(&key, None, Some(err_msg), retryable)
                    .map_err(|e| {
                        DurableError::Storage(format!("failed to log step failure: {}", e))
                    })?;
                Err(err)
            }
        }
    }

    /// Suspend the execution waiting for an external signal.
    pub fn await_signal(&self, signal_name: &str) -> DurableResult<json::Value> {
        // Check if the signal has already arrived
        match self.storage.peek_signal(self.id, signal_name) {
            Ok(Some(data)) => {
                // Signal already here — consume and return
                self.storage
                    .consume_signal(self.id, signal_name)
                    .map_err(|e| DurableError::Storage(e))?;
                let val = json::parse(&data)?;
                return Ok(val);
            }
            Ok(None) => {
                // Signal not yet arrived — suspend
                let reason = SuspendReason::WaitingForSignal {
                    signal_name: signal_name.to_string(),
                };
                *self.suspend_reason.lock().unwrap_or_else(|e| e.into_inner()) = Some(reason.clone());
                self.storage
                    .set_suspend_reason(self.id, Some(reason.clone()))
                    .map_err(|e| DurableError::Storage(e))?;
                self.storage
                    .update_execution_status(self.id, ExecutionStatus::Suspended)
                    .map_err(|e| DurableError::Storage(e))?;
                return Err(DurableError::Suspended(reason));
            }
            Err(e) => return Err(DurableError::Storage(e)),
        }
    }

    /// Suspend waiting for user input.
    pub fn await_input(&self, _prompt: &str) -> DurableResult<json::Value> {
        // Check for input signal
        self.await_signal(&format!("__input_{}", self.step_count()))
    }

    /// Schedule a durable timer and suspend.
    pub fn schedule_timer(
        &self,
        name: &str,
        duration: std::time::Duration,
    ) -> DurableResult<()> {
        let fire_at = now_millis() + duration.as_millis() as u64;
        self.storage
            .create_timer(self.id, name, fire_at)
            .map_err(|e| DurableError::Storage(e))?;
        let reason = SuspendReason::WaitingForTimer {
            fire_at_millis: fire_at,
            timer_name: name.to_string(),
        };
        *self.suspend_reason.lock().unwrap_or_else(|e| e.into_inner()) = Some(reason.clone());
        self.storage
            .set_suspend_reason(self.id, Some(reason.clone()))
            .map_err(|e| DurableError::Storage(e))?;
        self.storage
            .update_execution_status(self.id, ExecutionStatus::Suspended)
            .map_err(|e| DurableError::Storage(e))?;
        Err(DurableError::Suspended(reason))
    }

    /// Request human confirmation for a tool call. Suspends until approved.
    pub fn request_confirmation(
        &self,
        tool_name: &str,
        arguments: &json::Value,
    ) -> DurableResult<bool> {
        let confirmation_id = format!(
            "confirm_{}_{}_{}",
            tool_name,
            self.id,
            self.step_count()
        );

        // Check if confirmation has arrived
        match self.storage.peek_signal(self.id, &confirmation_id) {
            Ok(Some(data)) => {
                self.storage
                    .consume_signal(self.id, &confirmation_id)
                    .map_err(|e| DurableError::Storage(e))?;
                let val = json::parse(&data)?;
                match val.as_bool() {
                    Some(true) => Ok(true),
                    _ => Err(DurableError::Rejected {
                        tool_name: tool_name.to_string(),
                        reason: val
                            .get("reason")
                            .and_then(|v| v.as_str())
                            .unwrap_or("rejected by human")
                            .to_string(),
                    }),
                }
            }
            Ok(None) => {
                let reason = SuspendReason::WaitingForConfirmation {
                    tool_name: tool_name.to_string(),
                    arguments: arguments.clone(),
                    confirmation_id,
                };
                *self.suspend_reason.lock().unwrap_or_else(|e| e.into_inner()) = Some(reason.clone());
                self.storage
                    .set_suspend_reason(self.id, Some(reason.clone()))
                    .map_err(|e| DurableError::Storage(e))?;
                self.storage
                    .update_execution_status(self.id, ExecutionStatus::Suspended)
                    .map_err(|e| DurableError::Storage(e))?;
                Err(DurableError::Suspended(reason))
            }
            Err(e) => Err(DurableError::Storage(e)),
        }
    }

    /// Get the current suspend reason (if any).
    pub fn suspend_reason(&self) -> Option<SuspendReason> {
        self.suspend_reason.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Clear suspension state (called on resume).
    pub fn clear_suspension(&self) -> DurableResult<()> {
        *self.suspend_reason.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.storage
            .set_suspend_reason(self.id, None)
            .map_err(|e| DurableError::Storage(e))?;
        self.storage
            .update_execution_status(self.id, ExecutionStatus::Running)
            .map_err(|e| DurableError::Storage(e))
    }
}
