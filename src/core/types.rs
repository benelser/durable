//! Core types for durable execution.

use crate::core::uuid::Uuid;
use crate::json::{self, FromJson, ToJson, Value};

/// Unique identifier for an execution (an agent run).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ExecutionId(pub Uuid);

impl ExecutionId {
    pub fn new() -> Self {
        ExecutionId(Uuid::new_v4())
    }

    pub fn from_uuid(id: Uuid) -> Self {
        ExecutionId(id)
    }
}

impl Default for ExecutionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ExecutionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl ToJson for ExecutionId {
    fn to_json(&self) -> Value {
        self.0.to_json()
    }
}

impl FromJson for ExecutionId {
    fn from_json(val: &Value) -> Result<Self, String> {
        Uuid::from_json(val).map(ExecutionId)
    }
}

/// Composite key for step memoization: (execution_id, step_number, param_hash).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StepKey {
    pub execution_id: ExecutionId,
    pub step_number: u64,
    pub step_name: String,
    pub param_hash: u64,
}

impl std::fmt::Display for StepKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}:{}:{:016x}",
            self.execution_id, self.step_number, self.step_name, self.param_hash
        )
    }
}

impl ToJson for StepKey {
    fn to_json(&self) -> Value {
        json::json_object(vec![
            ("execution_id", self.execution_id.to_json()),
            ("step_number", json::json_num(self.step_number as f64)),
            ("step_name", json::json_str(&self.step_name)),
            ("param_hash", json::json_string(format!("{:016x}", self.param_hash))),
        ])
    }
}

/// Status of a step execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StepStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Suspended,
}

impl ToJson for StepStatus {
    fn to_json(&self) -> Value {
        json::json_str(match self {
            StepStatus::Pending => "pending",
            StepStatus::Running => "running",
            StepStatus::Completed => "completed",
            StepStatus::Failed => "failed",
            StepStatus::Suspended => "suspended",
        })
    }
}

impl FromJson for StepStatus {
    fn from_json(val: &Value) -> Result<Self, String> {
        match val.as_str() {
            Some("pending") => Ok(StepStatus::Pending),
            Some("running") => Ok(StepStatus::Running),
            Some("completed") => Ok(StepStatus::Completed),
            Some("failed") => Ok(StepStatus::Failed),
            Some("suspended") => Ok(StepStatus::Suspended),
            _ => Err("invalid step status".to_string()),
        }
    }
}

/// A persisted record of a step's execution.
#[derive(Clone, Debug)]
pub struct StepRecord {
    pub key: StepKey,
    pub status: StepStatus,
    /// The step's return value (JSON-serialized).
    pub result: Option<String>,
    /// Error message if the step failed.
    pub error: Option<String>,
    /// Whether the error is retryable.
    pub retryable: bool,
    /// Number of attempts made.
    pub attempts: u32,
    /// Timestamp when the step started (millis since epoch).
    pub started_at: u64,
    /// Timestamp when the step completed (millis since epoch).
    pub completed_at: Option<u64>,
}

impl ToJson for StepRecord {
    fn to_json(&self) -> Value {
        json::json_object(vec![
            ("key", self.key.to_json()),
            ("status", self.status.to_json()),
            ("result", self.result.as_ref().map_or(Value::Null, |r| json::json_str(r))),
            ("error", self.error.as_ref().map_or(Value::Null, |e| json::json_str(e))),
            ("retryable", json::json_bool(self.retryable)),
            ("attempts", json::json_num(self.attempts as f64)),
            ("started_at", json::json_num(self.started_at as f64)),
            (
                "completed_at",
                self.completed_at
                    .map_or(Value::Null, |t| json::json_num(t as f64)),
            ),
        ])
    }
}

/// Overall status of an execution.
///
/// Valid transitions (enforced by `transition_to`):
/// ```text
/// Running -> Suspended | Completed | Failed | Compensating
/// Suspended -> Running | Failed
/// Compensating -> Compensated | CompensationFailed
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecutionStatus {
    Running,
    Completed,
    Failed,
    Suspended,
    /// Running compensation handlers after a failure.
    Compensating,
    /// All compensation handlers completed successfully.
    Compensated,
    /// One or more compensation handlers failed.
    CompensationFailed,
}

impl ExecutionStatus {
    /// Validate a state transition. Returns `Ok(())` if the transition is valid,
    /// or `Err(DurableError::InvalidState)` if it is not.
    pub fn transition_to(&self, target: &ExecutionStatus) -> Result<(), crate::core::error::DurableError> {
        let valid = matches!(
            (self, target),
            // Running can go to suspended, completed, failed, or start compensating
            (ExecutionStatus::Running, ExecutionStatus::Suspended)
                | (ExecutionStatus::Running, ExecutionStatus::Completed)
                | (ExecutionStatus::Running, ExecutionStatus::Failed)
                | (ExecutionStatus::Running, ExecutionStatus::Compensating)
                // Suspended can resume (Running) or fail
                | (ExecutionStatus::Suspended, ExecutionStatus::Running)
                | (ExecutionStatus::Suspended, ExecutionStatus::Failed)
                // Compensating finishes as Compensated or CompensationFailed
                | (ExecutionStatus::Compensating, ExecutionStatus::Compensated)
                | (ExecutionStatus::Compensating, ExecutionStatus::CompensationFailed)
        );
        if valid {
            Ok(())
        } else {
            Err(crate::core::error::DurableError::InvalidState(format!(
                "invalid transition: {:?} -> {:?}",
                self, target
            )))
        }
    }
}

impl ToJson for ExecutionStatus {
    fn to_json(&self) -> Value {
        json::json_str(match self {
            ExecutionStatus::Running => "running",
            ExecutionStatus::Completed => "completed",
            ExecutionStatus::Failed => "failed",
            ExecutionStatus::Suspended => "suspended",
            ExecutionStatus::Compensating => "compensating",
            ExecutionStatus::Compensated => "compensated",
            ExecutionStatus::CompensationFailed => "compensation_failed",
        })
    }
}

impl FromJson for ExecutionStatus {
    fn from_json(val: &Value) -> Result<Self, String> {
        match val.as_str() {
            Some("running") => Ok(ExecutionStatus::Running),
            Some("completed") => Ok(ExecutionStatus::Completed),
            Some("failed") => Ok(ExecutionStatus::Failed),
            Some("suspended") => Ok(ExecutionStatus::Suspended),
            Some("compensating") => Ok(ExecutionStatus::Compensating),
            Some("compensated") => Ok(ExecutionStatus::Compensated),
            Some("compensation_failed") => Ok(ExecutionStatus::CompensationFailed),
            _ => Err("invalid execution status".to_string()),
        }
    }
}

/// Metadata for an execution.
#[derive(Clone, Debug)]
pub struct ExecutionMetadata {
    pub id: ExecutionId,
    pub status: ExecutionStatus,
    pub created_at: u64,
    pub updated_at: u64,
    pub step_count: u64,
    pub suspend_reason: Option<crate::core::error::SuspendReason>,
    /// Arbitrary user-provided metadata.
    pub tags: std::collections::BTreeMap<String, String>,
}

impl ToJson for ExecutionMetadata {
    fn to_json(&self) -> Value {
        let mut entries = vec![
            ("id", self.id.to_json()),
            ("status", self.status.to_json()),
            ("created_at", json::json_num(self.created_at as f64)),
            ("updated_at", json::json_num(self.updated_at as f64)),
            ("step_count", json::json_num(self.step_count as f64)),
        ];
        if let Some(ref reason) = self.suspend_reason {
            entries.push(("suspend_reason", reason.to_json()));
        }
        if !self.tags.is_empty() {
            let tag_obj = Value::Object(
                self.tags
                    .iter()
                    .map(|(k, v)| (k.clone(), json::json_str(v)))
                    .collect(),
            );
            entries.push(("tags", tag_obj));
        }
        json::json_object(entries)
    }
}
