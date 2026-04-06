//! Error types for the durable runtime.

use crate::core::retry::Retryable;
use std::fmt;

/// Top-level error type for the runtime.
///
/// All variants are `Clone` so errors can be forwarded, logged, and retried
/// without ownership gymnastics.
#[derive(Debug, Clone)]
pub enum DurableError {
    /// Storage backend error. Includes optional path for debugging.
    Storage(String),
    /// Storage error with file path context.
    StorageAt { message: String, path: String },
    /// Serialization error (JSON parse/format).
    Serialization(String),
    /// Step execution failed.
    StepFailed {
        step_name: String,
        message: String,
        retryable: bool,
        execution_id: Option<String>,
        step_number: Option<u64>,
    },
    /// Execution was suspended (not an error — control flow).
    Suspended(SuspendReason),
    /// Tool execution error.
    ToolError {
        tool_name: String,
        message: String,
        retryable: bool,
    },
    /// LLM call error.
    LlmError {
        message: String,
        retryable: bool,
    },
    /// Execution not found.
    NotFound(String),
    /// Invalid state transition.
    InvalidState(String),
    /// I/O error (converted to string for Clone support).
    Io(String),
    /// Protocol error (wire protocol).
    Protocol(String),
    /// Human rejected a confirmation gate.
    Rejected {
        tool_name: String,
        reason: String,
    },
    /// Execution was cancelled via CancellationToken.
    Cancelled,
    /// Replay detected non-determinism (step mismatch between code and history).
    NonDeterminismDetected {
        step_number: u64,
        expected_name: String,
        actual_name: String,
    },
    /// System prompt changed between execution and resume (Invariant I violation).
    PromptDrift {
        stored_hash: u64,
        current_hash: u64,
    },
    /// Fencing violation — a stale worker tried to write.
    StaleGeneration {
        expected: u64,
        actual: u64,
    },
    /// Work queue is full — caller should retry later.
    QueueFull,
}

/// Why an execution was suspended.
#[derive(Debug, Clone)]
pub enum SuspendReason {
    /// Waiting for user/human input.
    WaitingForInput {
        prompt: String,
    },
    /// Waiting for an external signal by name.
    WaitingForSignal {
        signal_name: String,
    },
    /// Waiting for a timer to fire.
    WaitingForTimer {
        fire_at_millis: u64,
        timer_name: String,
    },
    /// Waiting for human confirmation of a tool call.
    WaitingForConfirmation {
        tool_name: String,
        arguments: crate::json::Value,
        confirmation_id: String,
    },
    /// Waiting for a child flow to complete.
    WaitingForChild {
        child_id: crate::core::types::ExecutionId,
    },
    /// An agent contract was violated. Suspended for human review.
    ContractViolation {
        contract_name: String,
        step_name: String,
        reason: String,
    },
    /// Execution budget exhausted.
    BudgetExhausted {
        dimension: String,
        limit: String,
        used: String,
    },
    /// Graceful shutdown requested. The agent should suspend at the next
    /// step boundary so its state can be persisted before the process exits.
    GracefulShutdown,
}

impl fmt::Display for DurableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DurableError::Storage(msg) => write!(f, "storage error: {}", msg),
            DurableError::StorageAt { message, path } => {
                write!(f, "storage error at {}: {}", path, message)
            }
            DurableError::Serialization(msg) => write!(f, "serialization error: {}", msg),
            DurableError::StepFailed {
                step_name, message, execution_id, step_number, ..
            } => {
                let mut ctx = format!("step '{}'", step_name);
                if let Some(n) = step_number {
                    ctx.push_str(&format!(" (#{n})"));
                }
                if let Some(id) = execution_id {
                    ctx.push_str(&format!(" in exec {id}"));
                }
                write!(f, "{} failed: {}", ctx, message)
            }
            DurableError::Suspended(reason) => write!(f, "suspended: {:?}", reason),
            DurableError::ToolError {
                tool_name, message, ..
            } => write!(f, "tool '{}' error: {}", tool_name, message),
            DurableError::LlmError { message, .. } => write!(f, "LLM error: {}", message),
            DurableError::NotFound(msg) => write!(f, "not found: {}", msg),
            DurableError::InvalidState(msg) => write!(f, "invalid state: {}", msg),
            DurableError::Io(msg) => write!(f, "I/O error: {}", msg),
            DurableError::Protocol(msg) => write!(f, "protocol error: {}", msg),
            DurableError::Rejected {
                tool_name, reason, ..
            } => write!(f, "rejected tool '{}': {}", tool_name, reason),
            DurableError::Cancelled => write!(f, "execution cancelled"),
            DurableError::NonDeterminismDetected {
                step_number,
                expected_name,
                actual_name,
            } => write!(
                f,
                "non-determinism detected at step {}: expected '{}', got '{}'",
                step_number, expected_name, actual_name
            ),
            DurableError::PromptDrift { stored_hash, current_hash } => write!(
                f,
                "system prompt changed between execution and resume \
                 (stored {:016x}, current {:016x}) — replay determinism violation (Invariant I)",
                stored_hash, current_hash
            ),
            DurableError::StaleGeneration { expected, actual } => write!(
                f,
                "stale generation: expected {}, got {}",
                expected, actual
            ),
            DurableError::QueueFull => write!(f, "work queue is full — retry later"),
        }
    }
}

impl std::error::Error for DurableError {}

impl Retryable for DurableError {
    /// Classify whether this error is retryable.
    ///
    /// **Safe default**: unknown or unclassified errors are retryable.
    /// The dangerous case — incorrectly treating a permanent error as retryable —
    /// wastes retries but does not corrupt state. The opposite — treating a
    /// transient error as permanent — causes premature failure.
    fn is_retryable(&self) -> bool {
        match self {
            // Known permanent errors — never retry
            DurableError::Serialization(_) => false,
            DurableError::NotFound(_) => false,
            DurableError::InvalidState(_) => false,
            DurableError::Protocol(_) => false,
            DurableError::Rejected { .. } => false,
            DurableError::Cancelled => false,
            DurableError::NonDeterminismDetected { .. } => false,
            DurableError::PromptDrift { .. } => false,
            DurableError::StaleGeneration { .. } => false,
            DurableError::Suspended(_) => false,

            // Explicitly classified by the error producer
            DurableError::StepFailed { retryable, .. } => *retryable,
            DurableError::ToolError { retryable, .. } => *retryable,
            DurableError::LlmError { retryable, .. } => *retryable,
            DurableError::Io(_) => true, // I/O errors are generally transient

            // Safe default: assume retryable for anything unclassified
            DurableError::Storage(_) => true,
            DurableError::StorageAt { .. } => true,
            DurableError::QueueFull => true,
        }
    }
}

impl From<std::io::Error> for DurableError {
    fn from(err: std::io::Error) -> Self {
        DurableError::Io(err.to_string())
    }
}

impl From<crate::json::ParseError> for DurableError {
    fn from(err: crate::json::ParseError) -> Self {
        DurableError::Serialization(err.to_string())
    }
}

impl crate::json::ToJson for SuspendReason {
    fn to_json(&self) -> crate::json::Value {
        use crate::json::*;
        match self {
            SuspendReason::WaitingForInput { prompt } => json_object(vec![
                ("type", json_str("waiting_for_input")),
                ("prompt", json_str(prompt)),
            ]),
            SuspendReason::WaitingForSignal { signal_name } => json_object(vec![
                ("type", json_str("waiting_for_signal")),
                ("signal_name", json_str(signal_name)),
            ]),
            SuspendReason::WaitingForTimer {
                fire_at_millis,
                timer_name,
            } => json_object(vec![
                ("type", json_str("waiting_for_timer")),
                ("fire_at_millis", json_num(*fire_at_millis as f64)),
                ("timer_name", json_str(timer_name)),
            ]),
            SuspendReason::WaitingForConfirmation {
                tool_name,
                arguments,
                confirmation_id,
            } => json_object(vec![
                ("type", json_str("waiting_for_confirmation")),
                ("tool_name", json_str(tool_name)),
                ("arguments", arguments.clone()),
                ("confirmation_id", json_str(confirmation_id)),
            ]),
            SuspendReason::WaitingForChild { child_id } => json_object(vec![
                ("type", json_str("waiting_for_child")),
                ("child_id", child_id.to_json()),
            ]),
            SuspendReason::ContractViolation {
                contract_name,
                step_name,
                reason,
            } => json_object(vec![
                ("type", json_str("contract_violation")),
                ("contract_name", json_str(contract_name)),
                ("step_name", json_str(step_name)),
                ("reason", json_str(reason)),
            ]),
            SuspendReason::BudgetExhausted {
                dimension,
                limit,
                used,
            } => json_object(vec![
                ("type", json_str("budget_exhausted")),
                ("dimension", json_str(dimension)),
                ("limit", json_str(limit)),
                ("used", json_str(used)),
            ]),
            SuspendReason::GracefulShutdown => json_object(vec![
                ("type", json_str("graceful_shutdown")),
            ]),
        }
    }
}

impl crate::json::FromJson for SuspendReason {
    fn from_json(val: &crate::json::Value) -> Result<Self, String> {
        let typ = val
            .get("type")
            .and_then(|v| v.as_str())
            .ok_or("missing type field")?;
        match typ {
            "waiting_for_input" => Ok(SuspendReason::WaitingForInput {
                prompt: val
                    .get("prompt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            }),
            "waiting_for_signal" => Ok(SuspendReason::WaitingForSignal {
                signal_name: val
                    .get("signal_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            }),
            "waiting_for_timer" => Ok(SuspendReason::WaitingForTimer {
                fire_at_millis: val
                    .get("fire_at_millis")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
                timer_name: val
                    .get("timer_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            }),
            "waiting_for_confirmation" => Ok(SuspendReason::WaitingForConfirmation {
                tool_name: val
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                arguments: val
                    .get("arguments")
                    .cloned()
                    .unwrap_or(crate::json::Value::Null),
                confirmation_id: val
                    .get("confirmation_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            }),
            "waiting_for_child" => {
                let child_id = crate::core::types::ExecutionId::from_json(
                    val.get("child_id").unwrap_or(&crate::json::Value::Null),
                )?;
                Ok(SuspendReason::WaitingForChild { child_id })
            }
            "contract_violation" => Ok(SuspendReason::ContractViolation {
                contract_name: val.get("contract_name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                step_name: val.get("step_name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                reason: val.get("reason").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            }),
            "budget_exhausted" => Ok(SuspendReason::BudgetExhausted {
                dimension: val.get("dimension").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                limit: val.get("limit").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                used: val.get("used").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            }),
            "graceful_shutdown" => Ok(SuspendReason::GracefulShutdown),
            other => Err(format!("unknown suspend reason type: {}", other)),
        }
    }
}

pub type DurableResult<T> = Result<T, DurableError>;
