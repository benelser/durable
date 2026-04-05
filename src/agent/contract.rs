//! Agent contracts — enforceable invariants at the step boundary.
//!
//! Contracts are checked AFTER the LLM decides to call a tool but BEFORE
//! the tool executes. A violation triggers suspension (not crash), allowing
//! human review. Contract checks are recorded as events for auditability.

use crate::json::Value;
use std::sync::Arc;

/// A named contract that validates tool calls before execution.
///
/// The check function receives `(step_name, step_args)` and returns
/// `Ok(())` if the invariant holds, or `Err(reason)` if violated.
#[derive(Clone)]
pub struct Contract {
    /// Human-readable name for this contract (e.g., "max-charge").
    pub name: String,
    /// The validation function.
    pub check: Arc<dyn Fn(&str, &Value) -> Result<(), String> + Send + Sync>,
}
