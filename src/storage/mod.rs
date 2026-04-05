//! Storage abstraction for durable execution state.
//!
//! Provides the [`ExecutionLog`] trait and two implementations:
//! - [`InMemoryStorage`]: fast, ephemeral (for testing)
//! - [`FileStorage`]: durable, crash-safe (for production)

pub mod event;
pub mod file;
pub mod journal;
pub mod memory;
pub mod upcaster;
pub mod wal;
pub mod wal_store;

pub use event::{
    Event, EventStore, EventType, ExecutionState, FileEventStore, InMemoryEventStore, StepSnapshot,
    hash_event, validate_chain, CURRENT_SCHEMA_VERSION,
};
pub use upcaster::UpcasterRegistry;
pub use file::FileStorage;
pub use journal::Journal;
pub use memory::InMemoryStorage;

use crate::core::error::SuspendReason;
use crate::core::types::*;

/// Abstract storage interface for all execution state.
///
/// All methods take `&self` — implementations must handle interior mutability
/// (via Mutex, RwLock, or atomic file operations).
pub trait ExecutionLog: Send + Sync {
    // -- Execution lifecycle --

    /// Create a new execution record.
    fn create_execution(&self, id: ExecutionId) -> Result<(), String>;

    /// Get execution metadata.
    fn get_execution(&self, id: ExecutionId) -> Result<Option<ExecutionMetadata>, String>;

    /// Update execution status.
    fn update_execution_status(
        &self,
        id: ExecutionId,
        status: ExecutionStatus,
    ) -> Result<(), String>;

    /// Set the suspend reason for an execution.
    fn set_suspend_reason(
        &self,
        id: ExecutionId,
        reason: Option<SuspendReason>,
    ) -> Result<(), String>;

    /// List all executions (optionally filtered by status).
    fn list_executions(
        &self,
        status: Option<ExecutionStatus>,
    ) -> Result<Vec<ExecutionMetadata>, String>;

    // -- Step memoization --

    /// Log the start of a step. Returns the StepRecord.
    fn log_step_start(&self, key: StepKey) -> Result<StepRecord, String>;

    /// Log the completion of a step (success or failure).
    fn log_step_completion(
        &self,
        key: &StepKey,
        result: Option<String>,
        error: Option<String>,
        retryable: bool,
    ) -> Result<(), String>;

    /// Get a cached step result by key.
    fn get_step(&self, key: &StepKey) -> Result<Option<StepRecord>, String>;

    /// Get a step by execution_id + step_number (ignoring param_hash).
    /// Used during replay to detect parameter changes.
    fn get_step_by_number(
        &self,
        execution_id: ExecutionId,
        step_number: u64,
    ) -> Result<Option<StepRecord>, String>;

    /// Get all steps for an execution, ordered by step_number.
    fn get_steps(&self, execution_id: ExecutionId) -> Result<Vec<StepRecord>, String>;

    // -- Signals --

    /// Store a signal value for a waiting execution.
    fn store_signal(&self, execution_id: ExecutionId, name: &str, data: &str)
        -> Result<(), String>;

    /// Retrieve and consume a signal (returns None if not yet arrived).
    fn consume_signal(
        &self,
        execution_id: ExecutionId,
        name: &str,
    ) -> Result<Option<String>, String>;

    /// Check if a signal exists without consuming it.
    fn peek_signal(
        &self,
        execution_id: ExecutionId,
        name: &str,
    ) -> Result<Option<String>, String>;

    // -- Timers --

    /// Register a durable timer.
    fn create_timer(
        &self,
        execution_id: ExecutionId,
        name: &str,
        fire_at_millis: u64,
    ) -> Result<(), String>;

    /// Get all expired timers.
    fn get_expired_timers(&self) -> Result<Vec<(ExecutionId, String, u64)>, String>;

    /// Delete a timer after it fires.
    fn delete_timer(&self, execution_id: ExecutionId, name: &str) -> Result<(), String>;

    // -- Execution tags --

    /// Set a tag on an execution.
    fn set_tag(&self, execution_id: ExecutionId, key: &str, value: &str) -> Result<(), String>;

    /// Get a tag value.
    fn get_tag(&self, execution_id: ExecutionId, key: &str) -> Result<Option<String>, String>;

    // -- Retention --

    /// Delete an execution and all its associated data.
    fn delete_execution(&self, id: ExecutionId) -> Result<(), String>;

    /// Delete executions older than `age_millis` milliseconds. Returns count deleted.
    fn cleanup_older_than(&self, age_millis: u64) -> Result<u64, String>;
}
