//! Agent response type — the result of running an agent.
//!
//! `AgentResponse` replaces pattern-matching on `AgentOutcome` with a
//! simpler Result-based API:
//!
//! ```rust,ignore
//! let response = runtime.run("prompt")?;
//! println!("{response}");
//! ```

use crate::core::error::SuspendReason;
use crate::core::types::{ExecutionId, ExecutionStatus};
use std::fmt;

/// The result of running or resuming an agent execution.
///
/// Suspension is a valid response (not an error). Use `is_suspended()` to check,
/// then `execution_id()` to get the ID for later resumption.
#[derive(Debug, Clone)]
pub struct AgentResponse {
    /// The text output, if the agent completed with a response.
    text: Option<String>,
    /// The execution ID (for resumption, inspection, etc.).
    execution_id: ExecutionId,
    /// Current execution status.
    status: ExecutionStatus,
    /// Suspension reason, if the agent is suspended.
    suspend_reason: Option<SuspendReason>,
}

impl AgentResponse {
    /// Create a completed response.
    pub(crate) fn completed(execution_id: ExecutionId, text: String) -> Self {
        Self {
            text: Some(text),
            execution_id,
            status: ExecutionStatus::Completed,
            suspend_reason: None,
        }
    }

    /// Create a suspended response.
    pub(crate) fn suspended(execution_id: ExecutionId, reason: SuspendReason) -> Self {
        Self {
            text: None,
            execution_id,
            status: ExecutionStatus::Suspended,
            suspend_reason: Some(reason),
        }
    }

    /// Create a max-iterations response.
    pub(crate) fn max_iterations(execution_id: ExecutionId, last_response: String) -> Self {
        Self {
            text: if last_response.is_empty() { None } else { Some(last_response) },
            execution_id,
            status: ExecutionStatus::Running,
            suspend_reason: None,
        }
    }

    /// The text output from the agent, if it completed.
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }

    /// The execution ID. Use this to resume, inspect, or signal the execution.
    pub fn execution_id(&self) -> ExecutionId {
        self.execution_id
    }

    /// The current execution status.
    pub fn status(&self) -> &ExecutionStatus {
        &self.status
    }

    /// Whether the agent is suspended (waiting for input, signal, timer, or confirmation).
    pub fn is_suspended(&self) -> bool {
        self.status == ExecutionStatus::Suspended
    }

    /// Whether the agent completed with a response.
    pub fn is_completed(&self) -> bool {
        self.status == ExecutionStatus::Completed
    }

    /// The suspension reason, if suspended.
    pub fn suspend_reason(&self) -> Option<&SuspendReason> {
        self.suspend_reason.as_ref()
    }
}

impl fmt::Display for AgentResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.text {
            Some(text) => write!(f, "{}", text),
            None if self.is_suspended() => {
                if let Some(reason) = &self.suspend_reason {
                    write!(f, "[suspended: {:?}]", reason)
                } else {
                    write!(f, "[suspended]")
                }
            }
            None => Ok(()),
        }
    }
}
