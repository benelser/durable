//! Immutable, append-only event log — the foundation of durable execution.
//!
//! All state changes are recorded as events. Current state is derived
//! by folding over the event sequence. Events are never mutated or deleted.

use crate::core::time::now_millis;
use crate::core::types::ExecutionId;
use crate::json::{self, FromJson, ToJson, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

// ---------------------------------------------------------------------------
// Event types
// ---------------------------------------------------------------------------

/// A single immutable event in the execution history.
#[derive(Clone, Debug)]
pub struct Event {
    /// Monotonically increasing ID within the execution.
    pub event_id: u64,
    /// The execution this event belongs to.
    pub execution_id: ExecutionId,
    /// When the event was created (millis since epoch).
    pub timestamp: u64,
    /// The event payload.
    pub event_type: EventType,
    /// Idempotency key for dedup. If set, a retry with the same key
    /// returns the existing event instead of appending a duplicate.
    pub idempotency_key: Option<String>,
    /// Hash of the previous event's serialized form (FNV-1a).
    /// First event in the chain has `prev_hash: 0`.
    /// Used to detect tampering or corruption in the event log.
    pub prev_hash: u64,
    /// Schema version of the event payload. Defaults to 1.
    /// When the event schema evolves, this is incremented and upcasters
    /// transform old events to the current format during deserialization.
    pub schema_version: u32,
    /// Lamport timestamp for cross-execution total ordering within the process.
    /// Monotonically increasing across all executions.
    pub lamport_ts: u64,
}

/// Current schema version for newly created events.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Apply upcasters to an event's JSON before deserialization.
/// If the event's schema_version is below CURRENT_SCHEMA_VERSION and an
/// upcaster registry is provided, transforms the JSON to current format.
pub fn upcast_event_json(
    val: &mut Value,
    registry: &crate::storage::upcaster::UpcasterRegistry,
) {
    let version = val.get("schema_version").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
    if version >= CURRENT_SCHEMA_VERSION || registry.is_empty() {
        return;
    }

    // Get event type name for upcasting
    if let Some(event_type_val) = val.get("event_type") {
        if let Some(type_name) = event_type_val.get("type").and_then(|v| v.as_str()) {
            let event_type_json = event_type_val.clone();
            let (upcasted, new_version) = registry.upcast(
                type_name,
                event_type_json,
                version,
                CURRENT_SCHEMA_VERSION,
            );
            if new_version > version {
                if let Some(map) = val.as_object_mut() {
                    map.insert("event_type".to_string(), upcasted);
                    map.insert("schema_version".to_string(), json::json_num(new_version as f64));
                }
            }
        }
    }
}

/// Process-global Lamport clock for cross-execution ordering.
static LAMPORT_CLOCK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Get the next Lamport timestamp.
fn next_lamport() -> u64 {
    LAMPORT_CLOCK.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1
}

/// All possible event types in the execution lifecycle.
#[derive(Clone, Debug)]
pub enum EventType {
    /// Execution was created.
    ExecutionCreated {
        version: Option<String>,
        /// FNV-1a hash of the system prompt active at creation time.
        /// Used to detect prompt drift on replay.
        prompt_hash: Option<u64>,
        /// The full system prompt text, stored once for auditability.
        prompt_text: Option<String>,
    },
    /// A step began executing.
    StepStarted {
        step_number: u64,
        step_name: String,
        param_hash: u64,
        params: String, // full serialized params for idempotency verification
    },
    /// A step completed successfully.
    StepCompleted {
        step_number: u64,
        step_name: String,
        result: String,
    },
    /// A step failed.
    StepFailed {
        step_number: u64,
        step_name: String,
        error: String,
        retryable: bool,
    },
    /// Execution suspended.
    Suspended {
        reason: String, // JSON-serialized SuspendReason
    },
    /// Execution resumed (with new generation for fencing).
    Resumed {
        generation: u64,
    },
    /// An external signal was received.
    SignalReceived {
        name: String,
        data: String,
    },
    /// A signal was consumed by the execution.
    SignalConsumed {
        name: String,
    },
    /// A timer was created.
    TimerCreated {
        name: String,
        fire_at_millis: u64,
    },
    /// A timer fired.
    TimerFired {
        name: String,
    },
    /// Execution completed successfully.
    ExecutionCompleted {
        result: String,
    },
    /// Execution failed permanently.
    ExecutionFailed {
        error: String,
    },
    /// A tag was set on the execution.
    TagSet {
        key: String,
        value: String,
    },
    /// Compensation started for a step.
    CompensationStarted {
        step_name: String,
    },
    /// Compensation completed for a step.
    CompensationCompleted {
        step_name: String,
        result: String,
    },
    /// Compensation failed for a step.
    CompensationFailed {
        step_name: String,
        error: String,
    },
    /// A snapshot of execution state for fast resume.
    /// On resume, load the latest snapshot and replay only events after it.
    Snapshot {
        /// Serialized ExecutionState JSON.
        state_json: String,
        /// The event_id this snapshot covers through (inclusive).
        up_to_event_id: u64,
    },
    /// A child flow was started from this parent execution.
    ChildFlowStarted {
        child_id: crate::core::types::ExecutionId,
        input: String,
    },
    /// A child flow completed (result delivered to parent).
    ChildFlowCompleted {
        child_id: crate::core::types::ExecutionId,
        result: String,
    },
    /// A lease was acquired for mutual exclusion (Invariant V).
    LeaseAcquired {
        generation: u64,
        holder: String,
        ttl_millis: u64,
        acquired_at: u64,
    },
    /// A lease was released.
    LeaseReleased {
        generation: u64,
    },
    /// A contract was checked before tool execution.
    ContractChecked {
        step_name: String,
        contract_name: String,
        passed: bool,
        reason: Option<String>,
    },
    /// Budget usage was updated.
    BudgetUpdated {
        dollars_used: f64,
        llm_calls_used: u64,
        tool_calls_used: u64,
    },
    /// Budget was exhausted.
    BudgetExhausted {
        dimension: String,
    },
}

impl ToJson for Event {
    fn to_json(&self) -> Value {
        let mut entries = vec![
            ("event_id", json::json_num(self.event_id as f64)),
            ("execution_id", self.execution_id.to_json()),
            ("timestamp", json::json_num(self.timestamp as f64)),
            ("event_type", self.event_type.to_json()),
        ];
        if let Some(ref key) = self.idempotency_key {
            entries.push(("idempotency_key", json::json_str(key)));
        }
        if self.prev_hash != 0 {
            entries.push(("prev_hash", json::json_string(format!("{:016x}", self.prev_hash))));
        }
        if self.schema_version != 1 {
            entries.push(("schema_version", json::json_num(self.schema_version as f64)));
        }
        if self.lamport_ts != 0 {
            entries.push(("lamport_ts", json::json_num(self.lamport_ts as f64)));
        }
        json::json_object(entries)
    }
}

impl FromJson for Event {
    fn from_json(val: &Value) -> Result<Self, String> {
        Ok(Event {
            event_id: val.get("event_id").and_then(|v| v.as_u64()).unwrap_or(0),
            execution_id: ExecutionId::from_json(
                val.get("execution_id").unwrap_or(&Value::Null),
            )?,
            timestamp: val.get("timestamp").and_then(|v| v.as_u64()).unwrap_or(0),
            event_type: EventType::from_json(
                val.get("event_type").unwrap_or(&Value::Null),
            )?,
            idempotency_key: val.get("idempotency_key").and_then(|v| v.as_str()).map(String::from),
            prev_hash: val.get("prev_hash").and_then(|v| v.as_str())
                .and_then(|s| u64::from_str_radix(s, 16).ok()).unwrap_or(0),
            schema_version: val.get("schema_version").and_then(|v| v.as_u64())
                .map(|v| v as u32).unwrap_or(1),
            lamport_ts: val.get("lamport_ts").and_then(|v| v.as_u64()).unwrap_or(0),
        })
    }
}

impl ToJson for EventType {
    fn to_json(&self) -> Value {
        match self {
            EventType::ExecutionCreated { version, prompt_hash, prompt_text } => json::json_object(vec![
                ("type", json::json_str("execution_created")),
                ("version", version.as_ref().map_or(Value::Null, |v| json::json_str(v))),
                ("prompt_hash", prompt_hash.map_or(Value::Null, |h| json::json_string(format!("{:016x}", h)))),
                ("prompt_text", prompt_text.as_ref().map_or(Value::Null, |t| json::json_str(t))),
            ]),
            EventType::StepStarted { step_number, step_name, param_hash, params } => {
                json::json_object(vec![
                    ("type", json::json_str("step_started")),
                    ("step_number", json::json_num(*step_number as f64)),
                    ("step_name", json::json_str(step_name)),
                    ("param_hash", json::json_string(format!("{:016x}", param_hash))),
                    ("params", json::json_str(params)),
                ])
            }
            EventType::StepCompleted { step_number, step_name, result } => {
                json::json_object(vec![
                    ("type", json::json_str("step_completed")),
                    ("step_number", json::json_num(*step_number as f64)),
                    ("step_name", json::json_str(step_name)),
                    ("result", json::json_str(result)),
                ])
            }
            EventType::StepFailed { step_number, step_name, error, retryable } => {
                json::json_object(vec![
                    ("type", json::json_str("step_failed")),
                    ("step_number", json::json_num(*step_number as f64)),
                    ("step_name", json::json_str(step_name)),
                    ("error", json::json_str(error)),
                    ("retryable", json::json_bool(*retryable)),
                ])
            }
            EventType::Suspended { reason } => json::json_object(vec![
                ("type", json::json_str("suspended")),
                ("reason", json::json_str(reason)),
            ]),
            EventType::Resumed { generation } => json::json_object(vec![
                ("type", json::json_str("resumed")),
                ("generation", json::json_num(*generation as f64)),
            ]),
            EventType::SignalReceived { name, data } => json::json_object(vec![
                ("type", json::json_str("signal_received")),
                ("name", json::json_str(name)),
                ("data", json::json_str(data)),
            ]),
            EventType::SignalConsumed { name } => json::json_object(vec![
                ("type", json::json_str("signal_consumed")),
                ("name", json::json_str(name)),
            ]),
            EventType::TimerCreated { name, fire_at_millis } => json::json_object(vec![
                ("type", json::json_str("timer_created")),
                ("name", json::json_str(name)),
                ("fire_at_millis", json::json_num(*fire_at_millis as f64)),
            ]),
            EventType::TimerFired { name } => json::json_object(vec![
                ("type", json::json_str("timer_fired")),
                ("name", json::json_str(name)),
            ]),
            EventType::ExecutionCompleted { result } => json::json_object(vec![
                ("type", json::json_str("execution_completed")),
                ("result", json::json_str(result)),
            ]),
            EventType::ExecutionFailed { error } => json::json_object(vec![
                ("type", json::json_str("execution_failed")),
                ("error", json::json_str(error)),
            ]),
            EventType::TagSet { key, value } => json::json_object(vec![
                ("type", json::json_str("tag_set")),
                ("key", json::json_str(key)),
                ("value", json::json_str(value)),
            ]),
            EventType::CompensationStarted { step_name } => json::json_object(vec![
                ("type", json::json_str("compensation_started")),
                ("step_name", json::json_str(step_name)),
            ]),
            EventType::CompensationCompleted { step_name, result } => json::json_object(vec![
                ("type", json::json_str("compensation_completed")),
                ("step_name", json::json_str(step_name)),
                ("result", json::json_str(result)),
            ]),
            EventType::CompensationFailed { step_name, error } => json::json_object(vec![
                ("type", json::json_str("compensation_failed")),
                ("step_name", json::json_str(step_name)),
                ("error", json::json_str(error)),
            ]),
            EventType::Snapshot { state_json, up_to_event_id } => json::json_object(vec![
                ("type", json::json_str("snapshot")),
                ("state_json", json::json_str(state_json)),
                ("up_to_event_id", json::json_num(*up_to_event_id as f64)),
            ]),
            EventType::ChildFlowStarted { child_id, input } => json::json_object(vec![
                ("type", json::json_str("child_flow_started")),
                ("child_id", child_id.to_json()),
                ("input", json::json_str(input)),
            ]),
            EventType::ChildFlowCompleted { child_id, result } => json::json_object(vec![
                ("type", json::json_str("child_flow_completed")),
                ("child_id", child_id.to_json()),
                ("result", json::json_str(result)),
            ]),
            EventType::LeaseAcquired { generation, holder, ttl_millis, acquired_at } => json::json_object(vec![
                ("type", json::json_str("lease_acquired")),
                ("generation", json::json_num(*generation as f64)),
                ("holder", json::json_str(holder)),
                ("ttl_millis", json::json_num(*ttl_millis as f64)),
                ("acquired_at", json::json_num(*acquired_at as f64)),
            ]),
            EventType::LeaseReleased { generation } => json::json_object(vec![
                ("type", json::json_str("lease_released")),
                ("generation", json::json_num(*generation as f64)),
            ]),
            EventType::ContractChecked { step_name, contract_name, passed, reason } => json::json_object(vec![
                ("type", json::json_str("contract_checked")),
                ("step_name", json::json_str(step_name)),
                ("contract_name", json::json_str(contract_name)),
                ("passed", json::json_bool(*passed)),
                ("reason", reason.as_ref().map_or(Value::Null, |r| json::json_str(r))),
            ]),
            EventType::BudgetUpdated { dollars_used, llm_calls_used, tool_calls_used } => json::json_object(vec![
                ("type", json::json_str("budget_updated")),
                ("dollars_used", json::json_num(*dollars_used)),
                ("llm_calls_used", json::json_num(*llm_calls_used as f64)),
                ("tool_calls_used", json::json_num(*tool_calls_used as f64)),
            ]),
            EventType::BudgetExhausted { dimension } => json::json_object(vec![
                ("type", json::json_str("budget_exhausted")),
                ("dimension", json::json_str(dimension)),
            ]),
        }
    }
}

impl FromJson for EventType {
    fn from_json(val: &Value) -> Result<Self, String> {
        let t = val.get("type").and_then(|v| v.as_str()).ok_or("missing event type")?;
        match t {
            "execution_created" => Ok(EventType::ExecutionCreated {
                version: val.get("version").and_then(|v| v.as_str()).map(String::from),
                prompt_hash: val.get("prompt_hash").and_then(|v| v.as_str())
                    .and_then(|s| u64::from_str_radix(s, 16).ok()),
                prompt_text: val.get("prompt_text").and_then(|v| v.as_str()).map(String::from),
            }),
            "step_started" => Ok(EventType::StepStarted {
                step_number: val.get("step_number").and_then(|v| v.as_u64()).unwrap_or(0),
                step_name: val.get("step_name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                param_hash: val.get("param_hash").and_then(|v| v.as_str())
                    .and_then(|s| u64::from_str_radix(s, 16).ok()).unwrap_or(0),
                params: val.get("params").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            }),
            "step_completed" => Ok(EventType::StepCompleted {
                step_number: val.get("step_number").and_then(|v| v.as_u64()).unwrap_or(0),
                step_name: val.get("step_name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                result: val.get("result").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            }),
            "step_failed" => Ok(EventType::StepFailed {
                step_number: val.get("step_number").and_then(|v| v.as_u64()).unwrap_or(0),
                step_name: val.get("step_name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                error: val.get("error").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                retryable: val.get("retryable").and_then(|v| v.as_bool()).unwrap_or(false),
            }),
            "suspended" => Ok(EventType::Suspended {
                reason: val.get("reason").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            }),
            "resumed" => Ok(EventType::Resumed {
                generation: val.get("generation").and_then(|v| v.as_u64()).unwrap_or(0),
            }),
            "signal_received" => Ok(EventType::SignalReceived {
                name: val.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                data: val.get("data").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            }),
            "signal_consumed" => Ok(EventType::SignalConsumed {
                name: val.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            }),
            "timer_created" => Ok(EventType::TimerCreated {
                name: val.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                fire_at_millis: val.get("fire_at_millis").and_then(|v| v.as_u64()).unwrap_or(0),
            }),
            "timer_fired" => Ok(EventType::TimerFired {
                name: val.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            }),
            "execution_completed" => Ok(EventType::ExecutionCompleted {
                result: val.get("result").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            }),
            "execution_failed" => Ok(EventType::ExecutionFailed {
                error: val.get("error").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            }),
            "tag_set" => Ok(EventType::TagSet {
                key: val.get("key").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                value: val.get("value").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            }),
            "compensation_started" => Ok(EventType::CompensationStarted {
                step_name: val.get("step_name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            }),
            "compensation_completed" => Ok(EventType::CompensationCompleted {
                step_name: val.get("step_name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                result: val.get("result").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            }),
            "compensation_failed" => Ok(EventType::CompensationFailed {
                step_name: val.get("step_name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                error: val.get("error").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            }),
            "snapshot" => Ok(EventType::Snapshot {
                state_json: val.get("state_json").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                up_to_event_id: val.get("up_to_event_id").and_then(|v| v.as_u64()).unwrap_or(0),
            }),
            "child_flow_started" => {
                let child_id = crate::core::types::ExecutionId::from_json(
                    val.get("child_id").unwrap_or(&Value::Null),
                )?;
                Ok(EventType::ChildFlowStarted {
                    child_id,
                    input: val.get("input").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                })
            }
            "child_flow_completed" => {
                let child_id = crate::core::types::ExecutionId::from_json(
                    val.get("child_id").unwrap_or(&Value::Null),
                )?;
                Ok(EventType::ChildFlowCompleted {
                    child_id,
                    result: val.get("result").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                })
            }
            "lease_acquired" => Ok(EventType::LeaseAcquired {
                generation: val.get("generation").and_then(|v| v.as_u64()).unwrap_or(0),
                holder: val.get("holder").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                ttl_millis: val.get("ttl_millis").and_then(|v| v.as_u64()).unwrap_or(0),
                acquired_at: val.get("acquired_at").and_then(|v| v.as_u64()).unwrap_or(0),
            }),
            "lease_released" => Ok(EventType::LeaseReleased {
                generation: val.get("generation").and_then(|v| v.as_u64()).unwrap_or(0),
            }),
            "contract_checked" => Ok(EventType::ContractChecked {
                step_name: val.get("step_name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                contract_name: val.get("contract_name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                passed: val.get("passed").and_then(|v| v.as_bool()).unwrap_or(false),
                reason: val.get("reason").and_then(|v| v.as_str()).map(String::from),
            }),
            "budget_updated" => Ok(EventType::BudgetUpdated {
                dollars_used: val.get("dollars_used").and_then(|v| v.as_f64()).unwrap_or(0.0),
                llm_calls_used: val.get("llm_calls_used").and_then(|v| v.as_u64()).unwrap_or(0),
                tool_calls_used: val.get("tool_calls_used").and_then(|v| v.as_u64()).unwrap_or(0),
            }),
            "budget_exhausted" => Ok(EventType::BudgetExhausted {
                dimension: val.get("dimension").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            }),
            other => Err(format!("unknown event type: {}", other)),
        }
    }
}

// ---------------------------------------------------------------------------
// EventStore trait
// ---------------------------------------------------------------------------

/// Append-only event store. The source of truth for all execution state.
pub trait EventStore: Send + Sync {
    /// Append an event. Returns the assigned event_id.
    fn append(&self, execution_id: ExecutionId, event_type: EventType) -> Result<Event, String>;

    /// Get all events for an execution, ordered by event_id.
    fn events(&self, execution_id: ExecutionId) -> Result<Vec<Event>, String>;

    /// Get events starting from a specific event_id (exclusive).
    fn events_since(
        &self,
        execution_id: ExecutionId,
        after_event_id: u64,
    ) -> Result<Vec<Event>, String>;

    /// Get the latest event_id for an execution (0 if none).
    fn latest_event_id(&self, execution_id: ExecutionId) -> Result<u64, String>;

    /// List all execution IDs that have events.
    fn list_execution_ids(&self) -> Result<Vec<ExecutionId>, String>;

    /// Append an event only if the expected generation matches.
    /// Returns `Err("stale_generation:expected:actual")` if fencing fails.
    fn append_fenced(
        &self,
        execution_id: ExecutionId,
        event_type: EventType,
        expected_generation: u64,
    ) -> Result<Event, String> {
        // Default: derive generation from events and check
        let events = self.events(execution_id)?;
        let state = ExecutionState::from_events(execution_id, &events);
        if state.generation != expected_generation {
            return Err(format!(
                "stale_generation:{}:{}",
                expected_generation, state.generation
            ));
        }
        self.append(execution_id, event_type)
    }

    /// Append an event with an idempotency key.
    /// If an event with the same key already exists for this execution,
    /// returns the existing event instead of appending a duplicate.
    fn append_idempotent(
        &self,
        execution_id: ExecutionId,
        event_type: EventType,
        idempotency_key: String,
    ) -> Result<Event, String> {
        // Default: scan existing events for key match, then append.
        // Implementations should override for better performance.
        let existing = self.events(execution_id)?;
        for event in &existing {
            if event.idempotency_key.as_deref() == Some(&idempotency_key) {
                return Ok(event.clone());
            }
        }
        self.append(execution_id, event_type)
    }

    /// Find the latest snapshot event for an execution.
    /// Returns `(snapshot_event, up_to_event_id)` if one exists.
    fn latest_snapshot(
        &self,
        execution_id: ExecutionId,
    ) -> Result<Option<(String, u64)>, String> {
        let events = self.events(execution_id)?;
        for event in events.iter().rev() {
            if let EventType::Snapshot { state_json, up_to_event_id } = &event.event_type {
                return Ok(Some((state_json.clone(), *up_to_event_id)));
            }
        }
        Ok(None)
    }

    /// Atomically append multiple events as a batch.
    /// Either all succeed or none do.
    fn append_batch(
        &self,
        execution_id: ExecutionId,
        event_types: Vec<EventType>,
    ) -> Result<Vec<Event>, String> {
        // Default: sequential append (implementations can override for atomicity)
        let mut events = Vec::new();
        for et in event_types {
            events.push(self.append(execution_id, et)?);
        }
        Ok(events)
    }

    /// Compact the event log for an execution.
    ///
    /// Takes a snapshot of the current state, then removes all events before
    /// the snapshot. The event file is replaced with: snapshot + events after
    /// the snapshot point. This bounds memory and file size for long-running
    /// executions.
    ///
    /// Returns the number of events removed, or 0 if compaction was not needed.
    ///
    /// Compaction is only safe for completed or suspended executions, or when
    /// called between steps (not during step execution).
    fn compact(&self, execution_id: ExecutionId) -> Result<u64, String> {
        let events = self.events(execution_id)?;
        if events.len() < 20 {
            return Ok(0); // Not worth compacting
        }

        // Build current state from all events
        let state = ExecutionState::from_events(execution_id, &events);
        let state_json = crate::json::to_string(&state.to_json());

        // Find the latest event ID
        let latest_id = events.last().map(|e| e.event_id).unwrap_or(0);
        let events_removed = events.len() as u64;

        // Create a snapshot event
        let snapshot_event = EventType::Snapshot {
            state_json,
            up_to_event_id: latest_id,
        };

        // Append the snapshot, then the log is: old events + snapshot.
        // Future resumes will use the snapshot (fast path) and skip
        // all events before it.
        self.append(execution_id, snapshot_event)?;

        Ok(events_removed)
    }

    /// Verify the integrity of the event log for an execution (Invariant II).
    ///
    /// Checks:
    /// 1. Hash chain is intact (each event's prev_hash matches the hash of the previous event)
    /// 2. Event IDs are monotonically increasing
    /// 3. No missing or corrupt events
    ///
    /// Returns `Ok(())` if the log is intact, or `Err` with details.
    fn verify_integrity(&self, execution_id: ExecutionId) -> Result<(), String> {
        let events = self.events(execution_id)?;
        if events.is_empty() {
            return Ok(());
        }

        // Check event_id monotonicity
        for i in 1..events.len() {
            if events[i].event_id <= events[i - 1].event_id {
                return Err(format!(
                    "event_id not monotonic: event {} has id {}, previous has id {}",
                    i, events[i].event_id, events[i - 1].event_id
                ));
            }
        }

        // Check hash chain
        validate_chain(&events)
    }

    /// Acquire a lease for an execution (Invariant V — mutual exclusion with TTL).
    ///
    /// If no active lease exists (or the existing lease has expired), a new lease
    /// is acquired and the generation is incremented. If an active lease exists,
    /// returns an error.
    ///
    /// Returns the new generation number.
    fn acquire_lease(
        &self,
        execution_id: ExecutionId,
        holder: &str,
        ttl_millis: u64,
    ) -> Result<u64, String> {
        let events = self.events(execution_id)?;
        let state = ExecutionState::from_events(execution_id, &events);

        // Check for active lease
        if let Some(ref lease_holder) = state.lease_holder {
            if !state.is_lease_expired() {
                return Err(format!(
                    "execution {} has active lease held by '{}' (expires in {}ms)",
                    execution_id,
                    lease_holder,
                    state.lease_acquired_at + state.lease_ttl_millis
                        - crate::core::time::now_millis()
                ));
            }
            // Lease expired — ok to acquire
        }

        let new_generation = state.generation + 1;
        let now = crate::core::time::now_millis();
        self.append(execution_id, EventType::LeaseAcquired {
            generation: new_generation,
            holder: holder.to_string(),
            ttl_millis,
            acquired_at: now,
        })?;

        Ok(new_generation)
    }

    /// Release a lease for an execution.
    fn release_lease(
        &self,
        execution_id: ExecutionId,
        generation: u64,
    ) -> Result<(), String> {
        self.append(execution_id, EventType::LeaseReleased { generation })?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// In-memory EventStore
// ---------------------------------------------------------------------------

pub struct InMemoryEventStore {
    state: Mutex<BTreeMap<String, Vec<Event>>>,
}

impl InMemoryEventStore {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(BTreeMap::new()),
        }
    }
}

impl Default for InMemoryEventStore {
    fn default() -> Self {
        Self::new()
    }
}

impl EventStore for InMemoryEventStore {
    fn append(&self, execution_id: ExecutionId, event_type: EventType) -> Result<Event, String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let events = state.entry(execution_id.to_string()).or_default();
        let prev_hash = events.last().map(|e| hash_event(e)).unwrap_or(0);
        let event_id = events.len() as u64 + 1;
        let event = Event {
            event_id,
            execution_id,
            timestamp: now_millis(),
            event_type,
            idempotency_key: None,
            prev_hash,
            schema_version: CURRENT_SCHEMA_VERSION,
            lamport_ts: next_lamport(),
        };
        events.push(event.clone());
        Ok(event)
    }

    fn append_idempotent(
        &self,
        execution_id: ExecutionId,
        event_type: EventType,
        idempotency_key: String,
    ) -> Result<Event, String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let events = state.entry(execution_id.to_string()).or_default();

        // Check for existing event with same key (under same lock — atomic)
        for event in events.iter() {
            if event.idempotency_key.as_deref() == Some(&idempotency_key) {
                return Ok(event.clone());
            }
        }

        let prev_hash = events.last().map(|e| hash_event(e)).unwrap_or(0);
        let event_id = events.len() as u64 + 1;
        let event = Event {
            event_id,
            execution_id,
            timestamp: now_millis(),
            event_type,
            idempotency_key: Some(idempotency_key),
            prev_hash,
            schema_version: CURRENT_SCHEMA_VERSION,
            lamport_ts: next_lamport(),
        };
        events.push(event.clone());
        Ok(event)
    }

    fn events(&self, execution_id: ExecutionId) -> Result<Vec<Event>, String> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        Ok(state
            .get(&execution_id.to_string())
            .cloned()
            .unwrap_or_default())
    }

    fn events_since(
        &self,
        execution_id: ExecutionId,
        after_event_id: u64,
    ) -> Result<Vec<Event>, String> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        Ok(state
            .get(&execution_id.to_string())
            .map(|events| {
                events
                    .iter()
                    .filter(|e| e.event_id > after_event_id)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    fn latest_event_id(&self, execution_id: ExecutionId) -> Result<u64, String> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        Ok(state
            .get(&execution_id.to_string())
            .and_then(|events| events.last())
            .map(|e| e.event_id)
            .unwrap_or(0))
    }

    fn list_execution_ids(&self) -> Result<Vec<ExecutionId>, String> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .keys()
            .map(|k| {
                crate::core::uuid::Uuid::parse(k)
                    .map(ExecutionId::from_uuid)
            })
            .collect()
    }

    fn append_fenced(
        &self,
        execution_id: ExecutionId,
        event_type: EventType,
        expected_generation: u64,
    ) -> Result<Event, String> {
        // Atomic: check generation + append under same lock
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let events = state.entry(execution_id.to_string()).or_default();

        // Derive current generation from events
        let current_gen = {
            let exec_state = ExecutionState::from_events(execution_id, events);
            exec_state.generation
        };
        if current_gen != expected_generation {
            return Err(format!(
                "stale_generation:{}:{}",
                expected_generation, current_gen
            ));
        }

        let prev_hash = events.last().map(|e| hash_event(e)).unwrap_or(0);
        let event_id = events.len() as u64 + 1;
        let event = Event {
            event_id,
            execution_id,
            timestamp: now_millis(),
            event_type,
            idempotency_key: None,
            prev_hash,
            schema_version: CURRENT_SCHEMA_VERSION,
            lamport_ts: next_lamport(),
        };
        events.push(event.clone());
        Ok(event)
    }

    fn append_batch(
        &self,
        execution_id: ExecutionId,
        event_types: Vec<EventType>,
    ) -> Result<Vec<Event>, String> {
        // Atomic: all appends under same lock
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let events = state.entry(execution_id.to_string()).or_default();
        let mut results = Vec::new();
        for et in event_types {
            let prev_hash = events.last().map(|e| hash_event(e)).unwrap_or(0);
            let event_id = events.len() as u64 + 1;
            let event = Event {
                event_id,
                execution_id,
                timestamp: now_millis(),
                event_type: et,
                idempotency_key: None,
                prev_hash,
                schema_version: CURRENT_SCHEMA_VERSION,
                lamport_ts: next_lamport(),
            };
            events.push(event.clone());
            results.push(event);
        }
        Ok(results)
    }
}

// ---------------------------------------------------------------------------
// File-backed EventStore (append-only NDJSON file per execution)
// ---------------------------------------------------------------------------

pub struct FileEventStore {
    base_dir: PathBuf,
    upcaster: Option<crate::storage::upcaster::UpcasterRegistry>,
}

impl FileEventStore {
    pub fn new(base_dir: impl Into<PathBuf>) -> Result<Self, String> {
        let base_dir = base_dir.into();
        fs::create_dir_all(base_dir.join("events"))
            .map_err(|e| format!("failed to create events dir: {}", e))?;
        Ok(Self { base_dir, upcaster: None })
    }

    /// Create with an upcaster registry for schema evolution.
    pub fn with_upcaster(
        base_dir: impl Into<PathBuf>,
        upcaster: crate::storage::upcaster::UpcasterRegistry,
    ) -> Result<Self, String> {
        let base_dir = base_dir.into();
        fs::create_dir_all(base_dir.join("events"))
            .map_err(|e| format!("failed to create events dir: {}", e))?;
        Ok(Self { base_dir, upcaster: Some(upcaster) })
    }

    fn event_file(&self, execution_id: ExecutionId) -> PathBuf {
        self.base_dir
            .join("events")
            .join(format!("{}.ndjson", execution_id))
    }
}

impl FileEventStore {
    /// Read the last valid line of the event file and compute its hash for chaining.
    /// Tolerates a trailing corrupt line (crash recovery).
    fn last_event_hash(&self, execution_id: ExecutionId) -> Result<(u64, u64), String> {
        let path = self.event_file(execution_id);
        match fs::read_to_string(&path) {
            Ok(content) => {
                let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
                // Walk backwards to find the last valid line (skip trailing corrupt from crash)
                for i in (0..lines.len()).rev() {
                    if let Ok(val) = json::parse(lines[i]) {
                        if let Ok(event) = Event::from_json(&val) {
                            return Ok((i as u64 + 1, hash_event(&event)));
                        }
                    }
                    // Only tolerate corrupt last line — earlier corrupt lines are a real problem
                    if i < lines.len() - 1 {
                        return Err(format!("corrupt event at line {} (not trailing)", i + 1));
                    }
                }
                Ok((0, 0))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((0, 0)),
            Err(e) => Err(format!("failed to read event file: {}", e)),
        }
    }
}

impl EventStore for FileEventStore {
    fn append(&self, execution_id: ExecutionId, event_type: EventType) -> Result<Event, String> {
        let path = self.event_file(execution_id);
        let (current_count, prev_hash) = self.last_event_hash(execution_id)?;

        let event = Event {
            event_id: current_count + 1,
            execution_id,
            timestamp: now_millis(),
            event_type,
            idempotency_key: None,
            prev_hash,
            schema_version: CURRENT_SCHEMA_VERSION,
            lamport_ts: next_lamport(),
        };

        let line = json::to_string(&event.to_json());

        // Append atomically: single write_all with newline included, then fsync.
        // Combining data + newline in one write prevents partial lines from
        // concurrent appends or interruption between the two writes.
        let mut buf = line.into_bytes();
        buf.push(b'\n');

        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("failed to open event file: {}", e))?;
        file.write_all(&buf)
            .map_err(|e| format!("failed to write event: {}", e))?;
        file.sync_all()
            .map_err(|e| format!("failed to sync event file: {}", e))?;

        Ok(event)
    }

    fn events(&self, execution_id: ExecutionId) -> Result<Vec<Event>, String> {
        let path = self.event_file(execution_id);
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("failed to read events: {}", e)),
        };

        let mut events = Vec::new();
        let lines: Vec<&str> = content.lines().collect();
        for (_i, line) in lines.iter().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // Crash recovery: skip corrupt/truncated lines rather than failing.
            // A partial write (e.g., process killed mid-fsync) leaves a truncated
            // JSON line. Subsequent events (like lease_released) may follow it.
            // Skipping corrupt lines preserves all valid state.
            let val = match json::parse(line) {
                Ok(mut v) => {
                    if let Some(ref registry) = self.upcaster {
                        upcast_event_json(&mut v, registry);
                    }
                    v
                }
                Err(_) => continue, // skip corrupt line
            };
            match Event::from_json(&val) {
                Ok(event) => events.push(event),
                Err(_) => continue, // skip unparseable event
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
        // Fast: just count lines instead of parsing all events
        let (count, _hash) = self.last_event_hash(execution_id)?;
        Ok(count)
    }

    fn append_fenced(
        &self,
        execution_id: ExecutionId,
        event_type: EventType,
        expected_generation: u64,
    ) -> Result<Event, String> {
        // Fast path: scan file for last "resumed" event to get generation,
        // instead of parsing all events and building full state.
        let path = self.event_file(execution_id);
        let current_gen = match fs::read_to_string(&path) {
            Ok(content) => {
                let mut gen = 1u64;
                for line in content.lines() {
                    let line = line.trim();
                    if line.is_empty() { continue; }
                    // Quick string check before parsing
                    if line.contains("\"resumed\"") {
                        if let Ok(val) = json::parse(line) {
                            if let Some(et) = val.get("event_type") {
                                if et.get("type").and_then(|v| v.as_str()) == Some("resumed") {
                                    if let Some(g) = et.get("generation").and_then(|v| v.as_u64()) {
                                        gen = g;
                                    }
                                }
                            }
                        }
                    }
                }
                gen
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 1,
            Err(e) => return Err(format!("failed to read event file: {}", e)),
        };

        if current_gen != expected_generation {
            return Err(format!(
                "stale_generation:{}:{}",
                expected_generation, current_gen
            ));
        }

        self.append(execution_id, event_type)
    }

    fn list_execution_ids(&self) -> Result<Vec<ExecutionId>, String> {
        let events_dir = self.base_dir.join("events");
        let entries = match fs::read_dir(&events_dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.to_string()),
        };
        let mut ids = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(uuid_str) = name.strip_suffix(".ndjson") {
                if let Ok(uuid) = crate::core::uuid::Uuid::parse(uuid_str) {
                    ids.push(ExecutionId::from_uuid(uuid));
                }
            }
        }
        Ok(ids)
    }

    /// Compact the event log by replacing old events with a snapshot.
    ///
    /// After compaction, the file contains only: the snapshot event + any
    /// events that were appended after the snapshot point. This bounds
    /// file size for long-running executions.
    fn compact(&self, execution_id: ExecutionId) -> Result<u64, String> {
        let events = self.events(execution_id)?;
        if events.len() < 20 {
            return Ok(0);
        }

        let state = ExecutionState::from_events(execution_id, &events);
        let state_json = crate::json::to_string(&state.to_json());
        let latest_id = events.last().map(|e| e.event_id).unwrap_or(0);
        let events_before = events.len() as u64;

        // Create a new file with just the snapshot
        let snapshot_event = Event {
            event_id: latest_id + 1,
            execution_id,
            timestamp: crate::core::time::now_millis(),
            event_type: EventType::Snapshot {
                state_json,
                up_to_event_id: latest_id,
            },
            idempotency_key: None,
            prev_hash: events.last().map(|e| hash_event(e)).unwrap_or(0),
            schema_version: CURRENT_SCHEMA_VERSION,
            lamport_ts: next_lamport(),
        };

        let path = self.event_file(execution_id);
        let snapshot_line = crate::json::to_string(&snapshot_event.to_json());

        // Atomic replace: write snapshot to temp file, rename over original
        let tmp_path = path.with_extension("compact.tmp");
        let mut file = fs::File::create(&tmp_path)
            .map_err(|e| format!("compact: failed to create temp: {}", e))?;
        file.write_all(snapshot_line.as_bytes())
            .map_err(|e| format!("compact: failed to write: {}", e))?;
        file.write_all(b"\n")
            .map_err(|e| format!("compact: failed to write newline: {}", e))?;
        file.sync_all()
            .map_err(|e| format!("compact: failed to sync: {}", e))?;
        drop(file);

        fs::rename(&tmp_path, &path)
            .map_err(|e| format!("compact: failed to rename: {}", e))?;

        // Fsync parent
        if let Some(parent) = path.parent() {
            if let Ok(dir) = fs::File::open(parent) {
                let _ = dir.sync_all();
            }
        }

        Ok(events_before)
    }
}

// ---------------------------------------------------------------------------
// Hash chain validation — detect tampering or corruption
// ---------------------------------------------------------------------------

/// Compute the FNV-1a hash of an event's serialized form (for hash chain).
pub fn hash_event(event: &Event) -> u64 {
    let serialized = json::to_string(&event.to_json());
    crate::core::hash::fnv1a_hash(serialized.as_bytes())
}

/// Validate the hash chain of an event sequence.
/// Returns `Ok(())` if the chain is intact, or `Err` with details if broken.
///
/// The chain rule: `event[i].prev_hash == hash(event[i-1])`.
/// First event must have `prev_hash == 0`.
pub fn validate_chain(events: &[Event]) -> Result<(), String> {
    if events.is_empty() {
        return Ok(());
    }

    // First event: prev_hash must be 0
    if events[0].prev_hash != 0 {
        return Err(format!(
            "event {} has prev_hash {:016x} but should be 0 (first event)",
            events[0].event_id, events[0].prev_hash
        ));
    }

    // Subsequent events: prev_hash must match hash of previous event
    for i in 1..events.len() {
        let expected = hash_event(&events[i - 1]);
        if events[i].prev_hash != expected && events[i].prev_hash != 0 {
            // prev_hash == 0 means the event was written before hash chain was enabled
            return Err(format!(
                "hash chain broken at event {}: prev_hash={:016x}, expected={:016x}",
                events[i].event_id, events[i].prev_hash, expected
            ));
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// State projection — derive current state from events
// ---------------------------------------------------------------------------

/// Current state of an execution, derived from its event history.
#[derive(Clone, Debug)]
pub struct ExecutionState {
    pub execution_id: ExecutionId,
    pub status: crate::core::types::ExecutionStatus,
    pub generation: u64,
    pub version: Option<String>,
    /// FNV-1a hash of the system prompt recorded at execution creation.
    pub prompt_hash: Option<u64>,
    /// The full system prompt text recorded at execution creation.
    pub prompt_text: Option<String>,
    pub step_count: u64,
    pub created_at: u64,
    pub updated_at: u64,
    pub suspend_reason: Option<String>,
    pub tags: BTreeMap<String, String>,
    /// Signals that have been received but not yet consumed.
    pub pending_signals: BTreeMap<String, String>,
    /// Active timers.
    pub active_timers: BTreeMap<String, u64>,
    /// Completed step results indexed by step_number (legacy, for backward compat).
    pub step_results: BTreeMap<u64, StepSnapshot>,
    /// Semantic step index: step_name -> list of occurrences (in order).
    /// This is the primary lookup for replay. Inserting a step between existing
    /// steps no longer breaks replay of subsequent steps.
    pub step_results_by_name: BTreeMap<String, Vec<StepSnapshot>>,
    /// Child flows started from this execution. Maps child_id -> result (if completed).
    pub child_flows: BTreeMap<String, Option<String>>,
    /// Budget consumption state.
    pub budget_state: crate::agent::budget::BudgetState,
    /// Current lease holder (if any).
    pub lease_holder: Option<String>,
    /// Lease TTL in milliseconds.
    pub lease_ttl_millis: u64,
    /// When the lease was acquired (millis since epoch).
    pub lease_acquired_at: u64,
    /// Generation associated with the current lease.
    pub lease_generation: u64,
}

/// Snapshot of a completed step's state.
#[derive(Clone, Debug)]
pub struct StepSnapshot {
    pub step_number: u64,
    pub step_name: String,
    /// Which occurrence of this step name (0-based).
    pub occurrence: u64,
    pub param_hash: u64,
    pub params: String,
    pub result: Option<String>,
    pub error: Option<String>,
    pub retryable: bool,
    pub completed: bool,
}

impl ToJson for StepSnapshot {
    fn to_json(&self) -> Value {
        json::json_object(vec![
            ("step_number", json::json_num(self.step_number as f64)),
            ("step_name", json::json_str(&self.step_name)),
            ("occurrence", json::json_num(self.occurrence as f64)),
            ("param_hash", json::json_string(format!("{:016x}", self.param_hash))),
            ("params", json::json_str(&self.params)),
            ("result", self.result.as_ref().map_or(Value::Null, |r| json::json_str(r))),
            ("error", self.error.as_ref().map_or(Value::Null, |e| json::json_str(e))),
            ("retryable", json::json_bool(self.retryable)),
            ("completed", json::json_bool(self.completed)),
        ])
    }
}

impl FromJson for StepSnapshot {
    fn from_json(val: &Value) -> Result<Self, String> {
        Ok(StepSnapshot {
            step_number: val.get("step_number").and_then(|v| v.as_u64()).unwrap_or(0),
            step_name: val.get("step_name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            occurrence: val.get("occurrence").and_then(|v| v.as_u64()).unwrap_or(0),
            param_hash: val.get("param_hash").and_then(|v| v.as_str())
                .and_then(|s| u64::from_str_radix(s, 16).ok()).unwrap_or(0),
            params: val.get("params").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            result: val.get("result").and_then(|v| v.as_str()).map(String::from),
            error: val.get("error").and_then(|v| v.as_str()).map(String::from),
            retryable: val.get("retryable").and_then(|v| v.as_bool()).unwrap_or(false),
            completed: val.get("completed").and_then(|v| v.as_bool()).unwrap_or(false),
        })
    }
}

impl ToJson for ExecutionState {
    fn to_json(&self) -> Value {
        // Serialize step_results as array of snapshots
        let steps: Vec<Value> = self.step_results.values().map(|s| s.to_json()).collect();

        // Serialize tags
        let tags = Value::Object(
            self.tags.iter().map(|(k, v)| (k.clone(), json::json_str(v))).collect(),
        );

        // Serialize pending signals
        let signals = Value::Object(
            self.pending_signals.iter().map(|(k, v)| (k.clone(), json::json_str(v))).collect(),
        );

        // Serialize active timers
        let timers = Value::Object(
            self.active_timers.iter().map(|(k, v)| (k.clone(), json::json_num(*v as f64))).collect(),
        );

        // Serialize child flows
        let children = Value::Object(
            self.child_flows.iter().map(|(k, v)| {
                (k.clone(), v.as_ref().map_or(Value::Null, |r| json::json_str(r)))
            }).collect(),
        );

        json::json_object(vec![
            ("execution_id", self.execution_id.to_json()),
            ("status", self.status.to_json()),
            ("generation", json::json_num(self.generation as f64)),
            ("version", self.version.as_ref().map_or(Value::Null, |v| json::json_str(v))),
            ("prompt_hash", self.prompt_hash.map_or(Value::Null, |h| json::json_string(format!("{:016x}", h)))),
            ("prompt_text", self.prompt_text.as_ref().map_or(Value::Null, |t| json::json_str(t))),
            ("step_count", json::json_num(self.step_count as f64)),
            ("created_at", json::json_num(self.created_at as f64)),
            ("updated_at", json::json_num(self.updated_at as f64)),
            ("suspend_reason", self.suspend_reason.as_ref().map_or(Value::Null, |r| json::json_str(r))),
            ("tags", tags),
            ("pending_signals", signals),
            ("active_timers", timers),
            ("steps", json::json_array(steps)),
            ("child_flows", children),
            ("budget_state", self.budget_state.to_json()),
            ("lease_holder", self.lease_holder.as_ref().map_or(Value::Null, |h| json::json_str(h))),
            ("lease_ttl_millis", json::json_num(self.lease_ttl_millis as f64)),
            ("lease_acquired_at", json::json_num(self.lease_acquired_at as f64)),
            ("lease_generation", json::json_num(self.lease_generation as f64)),
        ])
    }
}

impl FromJson for ExecutionState {
    fn from_json(val: &Value) -> Result<Self, String> {
        use crate::core::types::{ExecutionId, ExecutionStatus};

        let execution_id = ExecutionId::from_json(
            val.get("execution_id").unwrap_or(&Value::Null),
        )?;
        let status = ExecutionStatus::from_json(
            val.get("status").unwrap_or(&Value::Null),
        )?;

        // Deserialize steps
        let mut step_results = BTreeMap::new();
        let mut step_results_by_name: BTreeMap<String, Vec<StepSnapshot>> = BTreeMap::new();
        if let Some(steps_arr) = val.get("steps").and_then(|v| v.as_array()) {
            for step_val in steps_arr {
                let snap = StepSnapshot::from_json(step_val)?;
                step_results_by_name.entry(snap.step_name.clone()).or_default().push(snap.clone());
                step_results.insert(snap.step_number, snap);
            }
        }

        // Deserialize tags
        let mut tags = BTreeMap::new();
        if let Some(obj) = val.get("tags").and_then(|v| v.as_object()) {
            for (k, v) in obj {
                if let Some(s) = v.as_str() {
                    tags.insert(k.clone(), s.to_string());
                }
            }
        }

        // Deserialize pending signals
        let mut pending_signals = BTreeMap::new();
        if let Some(obj) = val.get("pending_signals").and_then(|v| v.as_object()) {
            for (k, v) in obj {
                if let Some(s) = v.as_str() {
                    pending_signals.insert(k.clone(), s.to_string());
                }
            }
        }

        // Deserialize active timers
        let mut active_timers = BTreeMap::new();
        if let Some(obj) = val.get("active_timers").and_then(|v| v.as_object()) {
            for (k, v) in obj {
                if let Some(n) = v.as_u64() {
                    active_timers.insert(k.clone(), n);
                }
            }
        }

        // Deserialize child flows
        let mut child_flows = BTreeMap::new();
        if let Some(obj) = val.get("child_flows").and_then(|v| v.as_object()) {
            for (k, v) in obj {
                child_flows.insert(k.clone(), v.as_str().map(String::from));
            }
        }

        Ok(ExecutionState {
            execution_id,
            status,
            generation: val.get("generation").and_then(|v| v.as_u64()).unwrap_or(1),
            version: val.get("version").and_then(|v| v.as_str()).map(String::from),
            prompt_hash: val.get("prompt_hash").and_then(|v| v.as_str())
                .and_then(|s| u64::from_str_radix(s, 16).ok()),
            prompt_text: val.get("prompt_text").and_then(|v| v.as_str()).map(String::from),
            step_count: val.get("step_count").and_then(|v| v.as_u64()).unwrap_or(0),
            created_at: val.get("created_at").and_then(|v| v.as_u64()).unwrap_or(0),
            updated_at: val.get("updated_at").and_then(|v| v.as_u64()).unwrap_or(0),
            suspend_reason: val.get("suspend_reason").and_then(|v| v.as_str()).map(String::from),
            tags,
            pending_signals,
            active_timers,
            step_results,
            step_results_by_name,
            child_flows,
            budget_state: val.get("budget_state")
                .map(|v| crate::agent::budget::BudgetState::from_json(v).unwrap_or_default())
                .unwrap_or_default(),
            lease_holder: val.get("lease_holder").and_then(|v| v.as_str()).map(String::from),
            lease_ttl_millis: val.get("lease_ttl_millis").and_then(|v| v.as_u64()).unwrap_or(0),
            lease_acquired_at: val.get("lease_acquired_at").and_then(|v| v.as_u64()).unwrap_or(0),
            lease_generation: val.get("lease_generation").and_then(|v| v.as_u64()).unwrap_or(0),
        })
    }
}

impl ExecutionState {
    /// Reconstruct state from a snapshot + subsequent events.
    /// Much faster than replaying all events for long-running executions.
    pub fn from_snapshot_and_events(
        snapshot_json: &str,
        subsequent_events: &[Event],
    ) -> Result<Self, String> {
        let val = json::parse(snapshot_json).map_err(|e| format!("bad snapshot: {}", e))?;
        let mut state = Self::from_json(&val)?;
        for event in subsequent_events {
            state.apply(event);
        }
        Ok(state)
    }

    /// Reconstruct execution state from an event sequence.
    pub fn from_events(execution_id: ExecutionId, events: &[Event]) -> Self {
        let mut state = Self {
            execution_id,
            status: crate::core::types::ExecutionStatus::Running,
            generation: 1,
            version: None,
            prompt_hash: None,
            prompt_text: None,
            step_count: 0,
            created_at: 0,
            updated_at: 0,
            suspend_reason: None,
            tags: BTreeMap::new(),
            pending_signals: BTreeMap::new(),
            active_timers: BTreeMap::new(),
            step_results: BTreeMap::new(),
            step_results_by_name: BTreeMap::new(),
            child_flows: BTreeMap::new(),
            budget_state: crate::agent::budget::BudgetState::default(),
            lease_holder: None,
            lease_ttl_millis: 0,
            lease_acquired_at: 0,
            lease_generation: 0,
        };

        for event in events {
            state.apply(event);
        }

        state
    }

    /// Whether the current lease has expired.
    pub fn is_lease_expired(&self) -> bool {
        if self.lease_holder.is_none() {
            return true;
        }
        let now = crate::core::time::now_millis();
        now >= self.lease_acquired_at + self.lease_ttl_millis
    }

    /// Transition to a new status, validating the transition.
    /// Returns true if valid, false if invalid (but applies anyway — events are truth).
    fn try_transition(&mut self, target: crate::core::types::ExecutionStatus) -> bool {
        let valid = self.status.transition_to(&target).is_ok();
        self.status = target;
        valid
    }

    /// Apply a single event to the state (fold step).
    pub fn apply(&mut self, event: &Event) {
        self.updated_at = event.timestamp;

        match &event.event_type {
            EventType::ExecutionCreated { version, prompt_hash, prompt_text } => {
                self.created_at = event.timestamp;
                self.version = version.clone();
                self.prompt_hash = *prompt_hash;
                self.prompt_text = prompt_text.clone();
                self.status = crate::core::types::ExecutionStatus::Running;
            }
            EventType::StepStarted { step_number, step_name, param_hash, params } => {
                self.step_count = self.step_count.max(*step_number + 1);
                let occurrences = self.step_results_by_name.entry(step_name.clone()).or_default();
                let occurrence = occurrences.len() as u64;
                let snapshot = StepSnapshot {
                    step_number: *step_number,
                    step_name: step_name.clone(),
                    occurrence,
                    param_hash: *param_hash,
                    params: params.clone(),
                    result: None,
                    error: None,
                    retryable: false,
                    completed: false,
                };
                occurrences.push(snapshot.clone());
                self.step_results.insert(*step_number, snapshot);
            }
            EventType::StepCompleted { step_number, step_name, result } => {
                if let Some(snap) = self.step_results.get_mut(step_number) {
                    snap.result = Some(result.clone());
                    snap.completed = true;
                    // Also update the by-name index
                    let occurrence = snap.occurrence;
                    if let Some(by_name) = self.step_results_by_name.get_mut(step_name) {
                        if let Some(s) = by_name.get_mut(occurrence as usize) {
                            s.result = Some(result.clone());
                            s.completed = true;
                        }
                    }
                }
            }
            EventType::StepFailed { step_number, step_name, error, retryable } => {
                if let Some(snap) = self.step_results.get_mut(step_number) {
                    snap.error = Some(error.clone());
                    snap.retryable = *retryable;
                    snap.completed = true;
                    // Also update the by-name index
                    let occurrence = snap.occurrence;
                    if let Some(by_name) = self.step_results_by_name.get_mut(step_name) {
                        if let Some(s) = by_name.get_mut(occurrence as usize) {
                            s.error = Some(error.clone());
                            s.retryable = *retryable;
                            s.completed = true;
                        }
                    }
                }
            }
            EventType::Suspended { reason } => {
                self.try_transition(crate::core::types::ExecutionStatus::Suspended);
                self.suspend_reason = Some(reason.clone());
            }
            EventType::Resumed { generation } => {
                self.try_transition(crate::core::types::ExecutionStatus::Running);
                self.generation = *generation;
                self.suspend_reason = None;
            }
            EventType::SignalReceived { name, data } => {
                self.pending_signals.insert(name.clone(), data.clone());
            }
            EventType::SignalConsumed { name } => {
                self.pending_signals.remove(name);
            }
            EventType::TimerCreated { name, fire_at_millis } => {
                self.active_timers.insert(name.clone(), *fire_at_millis);
            }
            EventType::TimerFired { name } => {
                self.active_timers.remove(name);
            }
            EventType::ExecutionCompleted { .. } => {
                self.try_transition(crate::core::types::ExecutionStatus::Completed);
            }
            EventType::ExecutionFailed { .. } => {
                self.try_transition(crate::core::types::ExecutionStatus::Failed);
            }
            EventType::TagSet { key, value } => {
                self.tags.insert(key.clone(), value.clone());
            }
            EventType::CompensationStarted { .. } => {
                self.try_transition(crate::core::types::ExecutionStatus::Compensating);
            }
            EventType::CompensationCompleted { .. } => {
                // Stay in Compensating until all compensations finish
            }
            EventType::CompensationFailed { .. } => {
                self.try_transition(crate::core::types::ExecutionStatus::CompensationFailed);
            }
            EventType::Snapshot { .. } => {
                // Snapshots are read-only checkpoints — no state change
            }
            EventType::ChildFlowStarted { child_id, .. } => {
                self.child_flows.insert(child_id.to_string(), None);
            }
            EventType::ChildFlowCompleted { child_id, result } => {
                self.child_flows.insert(child_id.to_string(), Some(result.clone()));
            }
            EventType::LeaseAcquired { generation, holder, ttl_millis, acquired_at } => {
                self.generation = *generation;
                self.lease_holder = Some(holder.clone());
                self.lease_ttl_millis = *ttl_millis;
                self.lease_acquired_at = *acquired_at;
                self.lease_generation = *generation;
            }
            EventType::LeaseReleased { generation } => {
                if self.lease_generation == *generation {
                    self.lease_holder = None;
                    self.lease_ttl_millis = 0;
                    self.lease_acquired_at = 0;
                }
            }
            EventType::ContractChecked { .. } => {
                // Auditing event — no state change
            }
            EventType::BudgetUpdated { dollars_used, llm_calls_used, tool_calls_used } => {
                self.budget_state.dollars_used = *dollars_used;
                self.budget_state.llm_calls_used = *llm_calls_used;
                self.budget_state.tool_calls_used = *tool_calls_used;
            }
            EventType::BudgetExhausted { .. } => {
                // Recorded for auditability — suspension is handled separately
            }
        }
    }
}
