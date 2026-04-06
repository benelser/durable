//! Replay-aware execution context backed by an immutable event log.
//!
//! On resume, the context loads the event history and replays through it.
//! Each `step()` call either:
//! - **Replays** (cursor < history length): validates step name matches, returns cached result
//! - **Executes live** (cursor >= history length): runs the closure, appends events
//!
//! If a step name mismatch is detected during replay, the context raises
//! `NonDeterminismDetected` — never silently re-executes.

use crate::core::cancel::CancellationToken;
use crate::core::error::{DurableError, DurableResult, SuspendReason};
use crate::core::hash::{fnv1a_hash, hash_params};
use crate::core::types::*;
use crate::json;
use crate::storage::event::{Event, EventStore, EventType, ExecutionState, StepSnapshot};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// A registered compensation handler for saga rollback.
struct Compensation {
    #[allow(dead_code)]
    step_number: u64,
    step_name: String,
    handler: Box<dyn FnOnce() -> DurableResult<json::Value> + Send>,
}

/// An execution context backed by an immutable event log with replay validation.
pub struct ReplayContext {
    pub id: ExecutionId,
    event_store: Arc<dyn EventStore>,
    /// The event history loaded on construction (for replay).
    #[allow(dead_code)]
    history: Vec<Event>,
    /// Step counter for event ordering (still monotonic for event numbering).
    step_counter: AtomicU64,
    /// Per-name occurrence counters for semantic step identity.
    occurrence_counters: Mutex<std::collections::BTreeMap<String, u64>>,
    /// Current generation (fencing token).
    generation: u64,
    /// Code version for this execution.
    version: Option<String>,
    /// Completed step cache indexed by step_number (legacy).
    step_cache: Mutex<std::collections::BTreeMap<u64, StepSnapshot>>,
    /// Semantic step cache: step_name -> ordered occurrences.
    step_cache_by_name: Mutex<std::collections::BTreeMap<String, Vec<StepSnapshot>>>,
    /// Pending signals from history.
    pending_signals: Mutex<std::collections::BTreeMap<String, String>>,
    /// Suspension state.
    suspend_reason: Mutex<Option<SuspendReason>>,
    /// Registered compensation handlers (for saga rollback).
    compensations: Mutex<Vec<Compensation>>,
    /// Child flows started from this execution. Maps child_id -> result (if completed).
    child_flows: Mutex<std::collections::BTreeMap<String, Option<String>>>,
    cancel_token: CancellationToken,
    /// Budget consumption state (event-sourced).
    budget_state: Mutex<crate::agent::budget::BudgetState>,
    /// Whether the last step() call was a replay (cached result).
    last_step_was_replay: std::sync::atomic::AtomicBool,
}

impl ReplayContext {
    /// Create a new context for a brand-new execution.
    pub fn new(id: ExecutionId, event_store: Arc<dyn EventStore>, system_prompt: Option<&str>, cancel_token: Option<CancellationToken>) -> DurableResult<Self> {
        let prompt_hash = system_prompt.map(|p| fnv1a_hash(p.as_bytes()));
        event_store
            .append(id, EventType::ExecutionCreated {
                version: None,
                prompt_hash,
                prompt_text: system_prompt.map(String::from),
            })
            .map_err(|e| DurableError::Storage(e))?;

        Ok(Self {
            id,
            event_store,
            history: Vec::new(),
            step_counter: AtomicU64::new(0),
            occurrence_counters: Mutex::new(std::collections::BTreeMap::new()),
            generation: 1,
            version: None,
            step_cache: Mutex::new(std::collections::BTreeMap::new()),
            step_cache_by_name: Mutex::new(std::collections::BTreeMap::new()),
            pending_signals: Mutex::new(std::collections::BTreeMap::new()),
            suspend_reason: Mutex::new(None),
            compensations: Mutex::new(Vec::new()),
            child_flows: Mutex::new(std::collections::BTreeMap::new()),
            cancel_token: cancel_token.unwrap_or_else(CancellationToken::new),
            budget_state: Mutex::new(crate::agent::budget::BudgetState::default()),
            last_step_was_replay: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Create a context with a version tag.
    pub fn new_versioned(
        id: ExecutionId,
        event_store: Arc<dyn EventStore>,
        version: &str,
        system_prompt: Option<&str>,
        cancel_token: Option<CancellationToken>,
    ) -> DurableResult<Self> {
        let prompt_hash = system_prompt.map(|p| fnv1a_hash(p.as_bytes()));
        event_store
            .append(
                id,
                EventType::ExecutionCreated {
                    version: Some(version.to_string()),
                    prompt_hash,
                    prompt_text: system_prompt.map(String::from),
                },
            )
            .map_err(|e| DurableError::Storage(e))?;

        Ok(Self {
            id,
            event_store,
            history: Vec::new(),
            step_counter: AtomicU64::new(0),
            occurrence_counters: Mutex::new(std::collections::BTreeMap::new()),
            generation: 1,
            version: Some(version.to_string()),
            step_cache: Mutex::new(std::collections::BTreeMap::new()),
            step_cache_by_name: Mutex::new(std::collections::BTreeMap::new()),
            pending_signals: Mutex::new(std::collections::BTreeMap::new()),
            suspend_reason: Mutex::new(None),
            compensations: Mutex::new(Vec::new()),
            child_flows: Mutex::new(std::collections::BTreeMap::new()),
            cancel_token: cancel_token.unwrap_or_else(CancellationToken::new),
            budget_state: Mutex::new(crate::agent::budget::BudgetState::default()),
            last_step_was_replay: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Resume an existing execution from the event log.
    /// If a snapshot exists, resumes from it (fast path).
    /// Otherwise, replays all events from the beginning.
    ///
    /// Acquires a lease with TTL for mutual exclusion. If another worker
    /// holds an active lease, this call fails with `InvalidState`.
    pub fn resume(id: ExecutionId, event_store: Arc<dyn EventStore>, system_prompt: Option<&str>, cancel_token: Option<CancellationToken>) -> DurableResult<Self> {
        // Try snapshot-based fast resume first
        let snapshot = event_store
            .latest_snapshot(id)
            .map_err(|e| DurableError::Storage(e))?;

        let state = if let Some((snapshot_json, up_to_event_id)) = snapshot {
            // Fast path: load from snapshot + replay only subsequent events
            let subsequent = event_store
                .events_since(id, up_to_event_id)
                .map_err(|e| DurableError::Storage(e))?;
            ExecutionState::from_snapshot_and_events(&snapshot_json, &subsequent)
                .map_err(|e| DurableError::Serialization(e))?
        } else {
            // Slow path: replay all events
            let history = event_store
                .events(id)
                .map_err(|e| DurableError::Storage(e))?;
            if history.is_empty() {
                return Err(DurableError::NotFound(format!("execution {}", id)));
            }
            ExecutionState::from_events(id, &history)
        };

        // Invariant I: detect prompt drift between original execution and resume.
        // If the original execution recorded a prompt hash, the current prompt must match.
        if let (Some(stored_hash), Some(current_prompt)) = (state.prompt_hash, system_prompt) {
            let current_hash = fnv1a_hash(current_prompt.as_bytes());
            if stored_hash != current_hash {
                return Err(DurableError::PromptDrift {
                    stored_hash,
                    current_hash,
                });
            }
        }

        // Acquire lease for mutual exclusion (Invariant V).
        // Default TTL: 5 minutes. If an active lease exists and hasn't expired,
        // another worker owns this execution.
        let holder = format!("worker-{}", std::process::id());
        let new_generation = match event_store.acquire_lease(id, &holder, 300_000) {
            Ok(gen) => gen,
            Err(e) => return Err(DurableError::InvalidState(e)),
        };

        event_store
            .append(
                id,
                EventType::Resumed {
                    generation: new_generation,
                },
            )
            .map_err(|e| DurableError::Storage(e))?;

        let history = event_store
            .events(id)
            .map_err(|e| DurableError::Storage(e))?;

        Ok(Self {
            id,
            event_store,
            history,
            step_counter: AtomicU64::new(0),
            occurrence_counters: Mutex::new(std::collections::BTreeMap::new()),
            generation: new_generation,
            version: state.version.clone(),
            step_cache: Mutex::new(state.step_results),
            step_cache_by_name: Mutex::new(state.step_results_by_name),
            pending_signals: Mutex::new(state.pending_signals),
            suspend_reason: Mutex::new(None),
            compensations: Mutex::new(Vec::new()),
            child_flows: Mutex::new(state.child_flows),
            cancel_token: cancel_token.unwrap_or_else(CancellationToken::new),
            budget_state: Mutex::new(state.budget_state),
            last_step_was_replay: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Resume with version checking. Fails if the execution's version doesn't match.
    pub fn resume_versioned(
        id: ExecutionId,
        event_store: Arc<dyn EventStore>,
        expected_version: &str,
        system_prompt: Option<&str>,
        cancel_token: Option<CancellationToken>,
    ) -> DurableResult<Self> {
        let history = event_store
            .events(id)
            .map_err(|e| DurableError::Storage(e))?;

        if history.is_empty() {
            return Err(DurableError::NotFound(format!("execution {}", id)));
        }

        let state = ExecutionState::from_events(id, &history);

        // Version enforcement
        if let Some(ref stored_version) = state.version {
            if stored_version != expected_version {
                return Err(DurableError::InvalidState(format!(
                    "version mismatch: execution has '{}', code is '{}'",
                    stored_version, expected_version
                )));
            }
        }

        // Prompt drift detection (Invariant I)
        if let (Some(stored_hash), Some(current_prompt)) = (state.prompt_hash, system_prompt) {
            let current_hash = fnv1a_hash(current_prompt.as_bytes());
            if stored_hash != current_hash {
                return Err(DurableError::PromptDrift {
                    stored_hash,
                    current_hash,
                });
            }
        }

        let new_generation = state.generation + 1;

        event_store
            .append(
                id,
                EventType::Resumed {
                    generation: new_generation,
                },
            )
            .map_err(|e| DurableError::Storage(e))?;

        Ok(Self {
            id,
            event_store,
            history,
            step_counter: AtomicU64::new(0),
            occurrence_counters: Mutex::new(std::collections::BTreeMap::new()),
            generation: new_generation,
            version: state.version.clone(),
            step_cache: Mutex::new(state.step_results),
            step_cache_by_name: Mutex::new(state.step_results_by_name),
            pending_signals: Mutex::new(state.pending_signals),
            suspend_reason: Mutex::new(None),
            compensations: Mutex::new(Vec::new()),
            child_flows: Mutex::new(state.child_flows),
            cancel_token: cancel_token.unwrap_or_else(CancellationToken::new),
            budget_state: Mutex::new(state.budget_state),
            last_step_was_replay: std::sync::atomic::AtomicBool::new(false),
        })
    }

    pub fn event_store(&self) -> &Arc<dyn EventStore> {
        &self.event_store
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    pub fn cancel_token(&self) -> &CancellationToken {
        &self.cancel_token
    }

    pub fn step_count(&self) -> u64 {
        self.step_counter.load(Ordering::SeqCst)
    }

    // -----------------------------------------------------------------------
    // Patching API — safe code evolution between versions
    // -----------------------------------------------------------------------

    /// Version-aware branching for safe code evolution.
    ///
    /// Returns the version number to use. During replay, returns the version
    /// that was originally chosen. During live execution, returns `max_supported`.
    ///
    /// Example:
    /// ```ignore
    /// let v = ctx.get_version("add-validation-step", 1, 2);
    /// if v >= 2 {
    ///     ctx.step("validate", ...)?;
    /// }
    /// ctx.step("process", ...)?;
    /// ```
    pub fn get_version(
        &self,
        change_id: &str,
        _min_supported: u32,
        max_supported: u32,
    ) -> DurableResult<u32> {
        // Semantic lookup: version decisions are steps named "__version_{change_id}"
        let version_step = format!("__version_{}", change_id);

        // Allocate occurrence counter for this version step
        let occurrence = {
            let mut counters = self.occurrence_counters.lock().unwrap_or_else(|e| e.into_inner());
            let counter = counters.entry(version_step.clone()).or_insert(0);
            let occ = *counter;
            *counter += 1;
            occ
        };

        // O(1) semantic lookup
        let cached = {
            let cache = self.step_cache_by_name.lock().unwrap_or_else(|e| e.into_inner());
            cache.get(&version_step).and_then(|v| v.get(occurrence as usize)).cloned()
        };

        if let Some(snapshot) = cached {
            if let Some(ref result) = snapshot.result {
                let val = json::parse(result).map_err(|e| {
                    DurableError::Serialization(format!(
                        "failed to parse version result for '{}': {}", change_id, e
                    ))
                })?;
                if let Some(v) = val.as_u64() {
                    return Ok(v as u32);
                }
            }
        }

        // Live execution: use max_supported and record it
        let step_number = self.step_counter.fetch_add(1, Ordering::SeqCst);
        self.event_store.append(
            self.id,
            EventType::StepStarted {
                step_number,
                step_name: version_step.clone(),
                param_hash: 0,
                params: format!("{{\"change_id\":\"{}\"}}", change_id),
            },
        ).map_err(|e| DurableError::Storage(e))?;

        self.event_store.append(
            self.id,
            EventType::StepCompleted {
                step_number,
                step_name: version_step,
                result: format!("{}", max_supported),
            },
        ).map_err(|e| DurableError::Storage(e))?;

        Ok(max_supported)
    }

    /// Check if a version decision has already been made for a change_id.
    /// Does NOT consume an occurrence — safe to call speculatively.
    pub fn has_version(&self, change_id: &str) -> bool {
        let version_step = format!("__version_{}", change_id);
        let cache = self.step_cache_by_name.lock().unwrap_or_else(|e| e.into_inner());
        cache.get(&version_step).map_or(false, |v| !v.is_empty())
    }

    // -----------------------------------------------------------------------
    // Step execution with replay validation
    // -----------------------------------------------------------------------

    /// Execute a memoized step with replay validation.
    ///
    /// Identity is **semantic**: `(step_name, occurrence_count)`, not positional.
    /// Inserting a new step between existing steps does not break replay.
    ///
    /// During **replay**: looks up by `(name, occurrence)`, returns cached result.
    /// During **live execution**: runs the closure, appends events.
    /// On **mismatch**: returns `NonDeterminismDetected`.
    pub fn step<F>(
        &self,
        name: &str,
        params: &json::Value,
        f: F,
    ) -> DurableResult<json::Value>
    where
        F: FnOnce() -> DurableResult<json::Value>,
    {
        if self.cancel_token.is_cancelled() {
            return Err(DurableError::Cancelled);
        }

        // Assume replay until we reach live execution
        self.last_step_was_replay.store(true, Ordering::SeqCst);

        let step_number = self.step_counter.fetch_add(1, Ordering::SeqCst);
        let param_str = json::to_string(params);
        let param_hash = hash_params(&param_str);

        // Allocate the next occurrence for this step name
        let occurrence = {
            let mut counters = self.occurrence_counters.lock().unwrap_or_else(|e| e.into_inner());
            let counter = counters.entry(name.to_string()).or_insert(0);
            let occ = *counter;
            *counter += 1;
            occ
        };

        // Check if this step exists in the semantic replay cache
        let cached = {
            let cache = self.step_cache_by_name.lock().unwrap_or_else(|e| e.into_inner());
            cache.get(name).and_then(|v| v.get(occurrence as usize)).cloned()
        };

        if let Some(ref snapshot) = cached {
            // REPLAY MODE — validate parameters if they differ
            if snapshot.params != param_str && !snapshot.params.is_empty() {
                if snapshot.param_hash != param_hash {
                    return Err(DurableError::NonDeterminismDetected {
                        step_number: snapshot.step_number,
                        expected_name: format!("{}[{}](hash:{:016x})", name, occurrence, snapshot.param_hash),
                        actual_name: format!("{}[{}](hash:{:016x})", name, occurrence, param_hash),
                    });
                }
            }

            if snapshot.completed {
                if let Some(ref err) = snapshot.error {
                    if !snapshot.retryable {
                        return Err(DurableError::StepFailed {
                            step_name: name.to_string(),
                            message: err.clone(),
                            retryable: false,
                            execution_id: Some(self.id.to_string()),
                            step_number: Some(step_number),
                        });
                    }
                } else if let Some(ref result) = snapshot.result {
                    let val = json::parse(result).map_err(|e| {
                        DurableError::Serialization(format!(
                            "failed to parse cached result for step {}: {}",
                            name, e
                        ))
                    })?;
                    return Ok(val);
                } else {
                    return Ok(json::Value::Null);
                }
            }
        }

        // Also check legacy positional cache (backward compat with old event logs)
        if cached.is_none() {
            let legacy_cached = {
                let cache = self.step_cache.lock().unwrap_or_else(|e| e.into_inner());
                cache.get(&step_number).cloned()
            };

            if let Some(snapshot) = legacy_cached {
                if snapshot.step_name == name && snapshot.completed {
                    if let Some(ref err) = snapshot.error {
                        if !snapshot.retryable {
                            return Err(DurableError::StepFailed {
                                step_name: name.to_string(),
                                message: err.clone(),
                                retryable: false,
                                execution_id: Some(self.id.to_string()),
                                step_number: Some(step_number),
                            });
                        }
                    } else if let Some(ref result) = snapshot.result {
                        let val = json::parse(result).map_err(|e| {
                            DurableError::Serialization(format!(
                                "failed to parse cached result for step {}: {}",
                                name, e
                            ))
                        })?;
                        return Ok(val);
                    } else {
                        return Ok(json::Value::Null);
                    }
                } else if snapshot.step_name != name {
                    return Err(DurableError::NonDeterminismDetected {
                        step_number,
                        expected_name: snapshot.step_name.clone(),
                        actual_name: name.to_string(),
                    });
                }
            }
        }

        // LIVE MODE — not a replay
        self.last_step_was_replay.store(false, Ordering::SeqCst);

        // Execute with fenced, idempotent append
        let idem_key = format!("{}:{}:{}:started", self.id, name, occurrence);
        self.event_store
            .append_idempotent(
                self.id,
                EventType::StepStarted {
                    step_number,
                    step_name: name.to_string(),
                    param_hash,
                    params: param_str,
                },
                idem_key,
            )
            .map_err(|e| DurableError::Storage(e))?;

        match f() {
            Ok(result) => {
                let result_str = json::to_string(&result);
                let idem_key = format!("{}:{}:{}:completed", self.id, name, occurrence);
                self.event_store
                    .append_idempotent(
                        self.id,
                        EventType::StepCompleted {
                            step_number,
                            step_name: name.to_string(),
                            result: result_str,
                        },
                        idem_key,
                    )
                    .map_err(|e| DurableError::Storage(e))?;
                Ok(result)
            }
            Err(err) => {
                if matches!(&err, DurableError::Suspended(_)) {
                    return Err(err);
                }
                let retryable = match &err {
                    DurableError::StepFailed { retryable, .. } => *retryable,
                    DurableError::ToolError { retryable, .. } => *retryable,
                    DurableError::LlmError { retryable, .. } => *retryable,
                    _ => false,
                };
                self.event_store
                    .append(
                        self.id,
                        EventType::StepFailed {
                            step_number,
                            step_name: name.to_string(),
                            error: err.to_string(),
                            retryable,
                        },
                    )
                    .map_err(|e| DurableError::Storage(e))?;
                Err(err)
            }
        }
    }

    /// Like `step()`, but skips parameter hash validation during replay.
    /// Use this when parameters may legitimately differ on resume
    /// (e.g., reconstructed conversation history).
    pub fn step_lenient<F>(
        &self,
        name: &str,
        params: &json::Value,
        f: F,
    ) -> DurableResult<json::Value>
    where
        F: FnOnce() -> DurableResult<json::Value>,
    {
        if self.cancel_token.is_cancelled() {
            return Err(DurableError::Cancelled);
        }

        self.last_step_was_replay.store(true, Ordering::SeqCst);

        let step_number = self.step_counter.fetch_add(1, Ordering::SeqCst);
        let param_str = json::to_string(params);
        let param_hash = hash_params(&param_str);

        let occurrence = {
            let mut counters = self.occurrence_counters.lock().unwrap_or_else(|e| e.into_inner());
            let counter = counters.entry(name.to_string()).or_insert(0);
            let occ = *counter;
            *counter += 1;
            occ
        };

        // Check semantic cache (lenient: no param validation)
        let cached = {
            let cache = self.step_cache_by_name.lock().unwrap_or_else(|e| e.into_inner());
            cache.get(name).and_then(|v| v.get(occurrence as usize)).cloned()
        };

        if let Some(ref snapshot) = cached {
            if snapshot.completed {
                if let Some(ref err) = snapshot.error {
                    if !snapshot.retryable {
                        return Err(DurableError::StepFailed {
                            step_name: name.to_string(),
                            message: err.clone(),
                            retryable: false,
                            execution_id: Some(self.id.to_string()),
                            step_number: Some(step_number),
                        });
                    }
                } else if let Some(ref result) = snapshot.result {
                    let val = json::parse(result).map_err(|e| {
                        DurableError::Serialization(format!(
                            "failed to parse cached result for step {}: {}",
                            name, e
                        ))
                    })?;
                    return Ok(val);
                } else {
                    return Ok(json::Value::Null);
                }
            }
        }

        // Check legacy cache
        if cached.is_none() {
            let legacy_cached = {
                let cache = self.step_cache.lock().unwrap_or_else(|e| e.into_inner());
                cache.get(&step_number).cloned()
            };

            if let Some(snapshot) = legacy_cached {
                if snapshot.step_name == name && snapshot.completed {
                    if let Some(ref result) = snapshot.result {
                        let val = json::parse(result).map_err(|e| {
                            DurableError::Serialization(format!(
                                "failed to parse cached result for step {}: {}",
                                name, e
                            ))
                        })?;
                        return Ok(val);
                    } else {
                        return Ok(json::Value::Null);
                    }
                }
            }
        }

        // LIVE MODE — not a replay
        self.last_step_was_replay.store(false, Ordering::SeqCst);
        self.event_store
            .append_idempotent(
                self.id,
                EventType::StepStarted {
                    step_number,
                    step_name: name.to_string(),
                    param_hash,
                    params: param_str,
                },
                format!("{}:{}:{}:started", self.id, name, occurrence),
            )
            .map_err(|e| DurableError::Storage(e))?;

        match f() {
            Ok(result) => {
                let result_str = json::to_string(&result);
                self.event_store
                    .append_idempotent(
                        self.id,
                        EventType::StepCompleted {
                            step_number,
                            step_name: name.to_string(),
                            result: result_str,
                        },
                        format!("{}:{}:{}:completed", self.id, name, occurrence),
                    )
                    .map_err(|e| DurableError::Storage(e))?;
                Ok(result)
            }
            Err(err) => {
                if matches!(&err, DurableError::Suspended(_)) {
                    return Err(err);
                }
                let retryable = match &err {
                    DurableError::StepFailed { retryable, .. } => *retryable,
                    DurableError::ToolError { retryable, .. } => *retryable,
                    DurableError::LlmError { retryable, .. } => *retryable,
                    _ => false,
                };
                self.event_store
                    .append(
                        self.id,
                        EventType::StepFailed {
                            step_number,
                            step_name: name.to_string(),
                            error: err.to_string(),
                            retryable,
                        },
                    )
                    .map_err(|e| DurableError::Storage(e))?;
                Err(err)
            }
        }
    }

    // -----------------------------------------------------------------------
    // Saga compensation — step with undo
    // -----------------------------------------------------------------------

    /// Execute a step with a compensation handler for saga rollback.
    ///
    /// If this step succeeds but a later step fails, `compensate()` will
    /// run the undo handler (in reverse order of registration).
    pub fn step_with_compensation<F, C>(
        &self,
        name: &str,
        params: &json::Value,
        do_fn: F,
        undo_fn: C,
    ) -> DurableResult<json::Value>
    where
        F: FnOnce() -> DurableResult<json::Value>,
        C: FnOnce() -> DurableResult<json::Value> + Send + 'static,
    {
        let step_number = self.step_count(); // peek before step increments
        let result = self.step(name, params, do_fn)?;

        // Register compensation only if step succeeded
        let mut comps = self.compensations.lock().unwrap_or_else(|e| e.into_inner());
        comps.push(Compensation {
            step_number,
            step_name: name.to_string(),
            handler: Box::new(undo_fn),
        });

        Ok(result)
    }

    /// Run all registered compensations in reverse order.
    /// Each compensation is itself a durable step (memoized) and emits
    /// `CompensationStarted`/`CompensationCompleted`/`CompensationFailed` events.
    pub fn compensate(&self) -> DurableResult<Vec<(String, DurableResult<json::Value>)>> {
        let comps: Vec<Compensation> = {
            let mut c = self.compensations.lock().unwrap_or_else(|e| e.into_inner());
            c.drain(..).rev().collect()
        };

        let mut results = Vec::new();
        for comp in comps {
            // Emit CompensationStarted
            let _ = self.event_store.append(
                self.id,
                EventType::CompensationStarted {
                    step_name: comp.step_name.clone(),
                },
            );

            let comp_name = format!("__compensate_{}", comp.step_name);
            let comp_result = self.step(&comp_name, &json::json_null(), comp.handler);

            // Emit CompensationCompleted or CompensationFailed
            match &comp_result {
                Ok(val) => {
                    let _ = self.event_store.append(
                        self.id,
                        EventType::CompensationCompleted {
                            step_name: comp.step_name.clone(),
                            result: json::to_string(val),
                        },
                    );
                }
                Err(err) => {
                    let _ = self.event_store.append(
                        self.id,
                        EventType::CompensationFailed {
                            step_name: comp.step_name.clone(),
                            error: err.to_string(),
                        },
                    );
                }
            }

            results.push((comp.step_name, comp_result));
        }
        results.reverse(); // return in original order
        Ok(results)
    }

    // -----------------------------------------------------------------------
    // Signals
    // -----------------------------------------------------------------------

    /// Suspend waiting for an external signal.
    pub fn await_signal(&self, signal_name: &str) -> DurableResult<json::Value> {
        let data = {
            let signals = self.pending_signals.lock().unwrap_or_else(|e| e.into_inner());
            signals.get(signal_name).cloned()
        };

        if let Some(data) = data {
            self.event_store
                .append(
                    self.id,
                    EventType::SignalConsumed {
                        name: signal_name.to_string(),
                    },
                )
                .map_err(|e| DurableError::Storage(e))?;
            {
                let mut signals = self.pending_signals.lock().unwrap_or_else(|e| e.into_inner());
                signals.remove(signal_name);
            }
            let val = json::parse(&data)?;
            return Ok(val);
        }

        let reason = SuspendReason::WaitingForSignal {
            signal_name: signal_name.to_string(),
        };
        let reason_json = json::to_string(&reason.to_json());
        self.event_store
            .append(
                self.id,
                EventType::Suspended {
                    reason: reason_json,
                },
            )
            .map_err(|e| DurableError::Storage(e))?;
        *self.suspend_reason.lock().unwrap_or_else(|e| e.into_inner()) = Some(reason.clone());
        Err(DurableError::Suspended(reason))
    }

    /// Send a signal to this execution (external caller).
    pub fn send_signal(&self, name: &str, data: &str) -> DurableResult<()> {
        self.event_store
            .append(
                self.id,
                EventType::SignalReceived {
                    name: name.to_string(),
                    data: data.to_string(),
                },
            )
            .map_err(|e| DurableError::Storage(e))?;
        {
            let mut signals = self.pending_signals.lock().unwrap_or_else(|e| e.into_inner());
            signals.insert(name.to_string(), data.to_string());
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Child flows — hierarchical execution
    // -----------------------------------------------------------------------

    /// Record that a child flow was started from this parent execution.
    /// The child_id is the ExecutionId of the child. The caller is responsible
    /// for actually creating and running the child flow.
    ///
    /// This is a memoized step: on replay, the child_id is returned from cache
    /// without re-creating the child.
    pub fn start_child_flow(
        &self,
        child_id: ExecutionId,
        input: &str,
    ) -> DurableResult<ExecutionId> {
        // Check if this child was already started (replay)
        {
            let children = self.child_flows.lock().unwrap_or_else(|e| e.into_inner());
            if children.contains_key(&child_id.to_string()) {
                return Ok(child_id);
            }
        }

        // Live mode: record ChildFlowStarted event
        self.event_store
            .append(
                self.id,
                EventType::ChildFlowStarted {
                    child_id,
                    input: input.to_string(),
                },
            )
            .map_err(|e| DurableError::Storage(e))?;

        // Track locally
        {
            let mut children = self.child_flows.lock().unwrap_or_else(|e| e.into_inner());
            children.insert(child_id.to_string(), None);
        }

        Ok(child_id)
    }

    /// Wait for a child flow to complete. If the child has already completed
    /// (result in the event log), returns immediately. Otherwise, suspends
    /// the parent with `WaitingForChild`.
    ///
    /// The child's result is delivered by calling `complete_child_flow` on
    /// the parent's replay context (typically by the runtime after the child
    /// flow finishes).
    pub fn await_child_flow(&self, child_id: ExecutionId) -> DurableResult<json::Value> {
        // Check if child already completed (from history or live)
        {
            let children = self.child_flows.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(Some(result_str)) = children.get(&child_id.to_string()) {
                let val = json::parse(result_str)?;
                return Ok(val);
            }
        }

        // Child not yet complete — suspend
        let reason = SuspendReason::WaitingForChild { child_id };
        let reason_json = json::to_string(&reason.to_json());
        self.event_store
            .append(
                self.id,
                EventType::Suspended {
                    reason: reason_json,
                },
            )
            .map_err(|e| DurableError::Storage(e))?;
        *self.suspend_reason.lock().unwrap_or_else(|e| e.into_inner()) = Some(reason.clone());
        Err(DurableError::Suspended(reason))
    }

    /// Deliver a child flow's result to this parent execution.
    /// Called by the runtime when a child flow completes.
    pub fn complete_child_flow(
        &self,
        child_id: ExecutionId,
        result: &str,
    ) -> DurableResult<()> {
        self.event_store
            .append(
                self.id,
                EventType::ChildFlowCompleted {
                    child_id,
                    result: result.to_string(),
                },
            )
            .map_err(|e| DurableError::Storage(e))?;

        {
            let mut children = self.child_flows.lock().unwrap_or_else(|e| e.into_inner());
            children.insert(child_id.to_string(), Some(result.to_string()));
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Lifecycle
    // -----------------------------------------------------------------------

    /// Mark execution as completed (atomic batch: tag + status).
    pub fn complete(&self, result: &str) -> DurableResult<()> {
        let r = self.event_store
            .append_batch(
                self.id,
                vec![
                    EventType::TagSet {
                        key: "final_result".to_string(),
                        value: result.to_string(),
                    },
                    EventType::ExecutionCompleted {
                        result: result.to_string(),
                    },
                ],
            )
            .map_err(|e| DurableError::Storage(e))
            .map(|_| ());
        self.release_lease();
        r
    }

    /// Mark execution as failed.
    pub fn fail(&self, error: &str) -> DurableResult<()> {
        let r = self.event_store
            .append(
                self.id,
                EventType::ExecutionFailed {
                    error: error.to_string(),
                },
            )
            .map_err(|e| DurableError::Storage(e))
            .map(|_| ());
        self.release_lease();
        r
    }

    /// Set a tag.
    pub fn set_tag(&self, key: &str, value: &str) -> DurableResult<()> {
        self.event_store
            .append(
                self.id,
                EventType::TagSet {
                    key: key.to_string(),
                    value: value.to_string(),
                },
            )
            .map_err(|e| DurableError::Storage(e))
            .map(|_| ())
    }

    /// Get the current execution state by replaying all events.
    pub fn current_state(&self) -> DurableResult<ExecutionState> {
        let events = self
            .event_store
            .events(self.id)
            .map_err(|e| DurableError::Storage(e))?;
        Ok(ExecutionState::from_events(self.id, &events))
    }

    /// Create a snapshot of the current execution state if the step count
    /// has reached the given interval. Call this periodically (e.g., after each
    /// step) to keep resume latency bounded for long-running agents.
    ///
    /// Returns `true` if a snapshot was taken, `false` if not yet due.
    pub fn maybe_snapshot(&self, interval: u64) -> DurableResult<bool> {
        let step_count = self.step_counter.load(Ordering::SeqCst);
        if interval == 0 || step_count == 0 || step_count % interval != 0 {
            return Ok(false);
        }

        let state = self.current_state()?;
        let state_json = json::to_string(&state.to_json());
        let latest_id = self.event_store
            .latest_event_id(self.id)
            .map_err(|e| DurableError::Storage(e))?;

        self.event_store
            .append(
                self.id,
                EventType::Snapshot {
                    state_json,
                    up_to_event_id: latest_id,
                },
            )
            .map_err(|e| DurableError::Storage(e))?;

        Ok(true)
    }

    /// Whether any compensation handlers have been registered.
    pub fn has_compensations(&self) -> bool {
        let comps = self.compensations.lock().unwrap_or_else(|e| e.into_inner());
        !comps.is_empty()
    }

    /// Get suspend reason.
    pub fn suspend_reason(&self) -> Option<SuspendReason> {
        self.suspend_reason.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    // -----------------------------------------------------------------------
    // Invariant I — Deterministic primitives
    //
    // These wrap non-deterministic operations as memoized steps so that
    // flow logic never reads wall-clock time or random values directly.
    // The type system enforces the boundary: flow logic interacts only
    // with ReplayContext, which only exposes these deterministic wrappers.
    // -----------------------------------------------------------------------

    /// Deterministic wall-clock time (millis since epoch).
    ///
    /// On first execution, captures `SystemTime::now()` and records it as a
    /// memoized step. On replay, returns the recorded timestamp — never
    /// re-reads the clock.
    pub fn now(&self) -> DurableResult<u64> {
        let result = self.step("__deterministic_now", &json::json_null(), || {
            let millis = crate::core::time::now_millis();
            Ok(json::json_num(millis as f64))
        })?;
        Ok(result.as_u64().unwrap_or(0))
    }

    /// Deterministic random u64.
    ///
    /// On first execution, generates a random value from system entropy and
    /// records it as a memoized step. On replay, returns the recorded value.
    pub fn random_u64(&self) -> DurableResult<u64> {
        let result = self.step("__deterministic_random", &json::json_null(), || {
            // Simple entropy from system sources — no external crate needed.
            // Mix multiple sources for reasonable randomness.
            let t1 = crate::core::time::now_millis();
            let addr = &t1 as *const u64 as u64;
            let t2 = crate::core::time::now_millis();
            let seed = t1
                .wrapping_mul(6364136223846793005)
                .wrapping_add(addr)
                .wrapping_mul(1442695040888963407)
                .wrapping_add(t2);
            Ok(json::json_num(seed as f64))
        })?;
        Ok(result.as_u64().unwrap_or(0))
    }

    // -----------------------------------------------------------------------
    // Invariant IV — Transparent suspension primitives
    //
    // These present suspension as ordinary function calls. The workflow
    // author sees a method that takes arguments and returns a value —
    // no knowledge of the underlying suspend/resume mechanics.
    // -----------------------------------------------------------------------

    /// Sleep for the given duration (durable timer).
    ///
    /// On first execution: creates a timer, suspends, and the runtime
    /// resumes after the duration elapses. On replay: returns immediately
    /// (the sleep already happened). To the caller, this looks like a
    /// blocking `thread::sleep()` that survives crashes.
    pub fn sleep(&self, name: &str, duration: std::time::Duration) -> DurableResult<()> {
        let fire_at = crate::core::time::now_millis() + duration.as_millis() as u64;
        let timer_step = format!("__timer_{}", name);

        // Check if timer already completed in history
        let cached = {
            let cache = self.step_cache_by_name.lock().unwrap_or_else(|e| e.into_inner());
            cache.get(&timer_step).and_then(|v| v.first()).cloned()
        };

        if let Some(snapshot) = cached {
            if snapshot.completed && snapshot.error.is_none() {
                // Timer already fired — advance occurrence counter and return
                let mut counters = self.occurrence_counters.lock().unwrap_or_else(|e| e.into_inner());
                let counter = counters.entry(timer_step).or_insert(0);
                *counter += 1;
                return Ok(());
            }
        }

        // Record timer creation
        self.event_store
            .append(self.id, EventType::TimerCreated {
                name: name.to_string(),
                fire_at_millis: fire_at,
            })
            .map_err(|e| DurableError::Storage(e))?;

        // Suspend until the timer fires
        let reason = SuspendReason::WaitingForTimer {
            fire_at_millis: fire_at,
            timer_name: name.to_string(),
        };
        let reason_json = json::to_string(&reason.to_json());
        self.event_store
            .append(self.id, EventType::Suspended { reason: reason_json })
            .map_err(|e| DurableError::Storage(e))?;
        *self.suspend_reason.lock().unwrap_or_else(|e| e.into_inner()) = Some(reason.clone());
        Err(DurableError::Suspended(reason))
    }

    /// Wait for a child flow to complete (transparent).
    ///
    /// Combines starting a child flow and awaiting its result into a single
    /// call that looks like a normal function invocation. On replay, returns
    /// the cached result immediately.
    pub fn wait_for_child(
        &self,
        child_id: ExecutionId,
        input: &str,
    ) -> DurableResult<json::Value> {
        self.start_child_flow(child_id, input)?;
        self.await_child_flow(child_id)
    }

    // -----------------------------------------------------------------------
    // Invariant VI — Per-step retry override (three-axis classification)
    //
    // Axis 1: RetryPolicy (how many times, what delay)
    // Axis 2: Retryable trait (which errors are transient vs permanent)
    // Axis 3: StepRetryOverride (per-step escape hatch)
    // -----------------------------------------------------------------------

    /// Execute a step with explicit retry policy and per-step override.
    ///
    /// This is the full three-axis error classification API:
    /// - `policy`: default retry policy (how many times, backoff)
    /// - `step_override`: per-step escape hatch (`NeverRetry`, `ForceRetry`, `UseDefault`)
    /// - Error classification is automatic via the `Retryable` trait
    pub fn step_with_retry<F>(
        &self,
        name: &str,
        params: &json::Value,
        policy: &crate::core::retry::RetryPolicy,
        step_override: &crate::core::retry::StepRetryOverride,
        f: F,
    ) -> DurableResult<json::Value>
    where
        F: Fn() -> DurableResult<json::Value>,
    {
        use crate::core::retry::{Retryable, StepRetryOverride};

        // Resolve effective policy from the three axes
        let effective_policy = match step_override {
            StepRetryOverride::NeverRetry => &crate::core::retry::RetryPolicy::NONE,
            StepRetryOverride::ForceRetry(ref p) => p,
            StepRetryOverride::UseDefault => policy,
        };

        let mut attempt = 0u32;
        loop {
            match self.step(name, params, &f) {
                Ok(val) => return Ok(val),
                Err(DurableError::Suspended(reason)) => {
                    return Err(DurableError::Suspended(reason));
                }
                Err(err) => {
                    attempt += 1;

                    // Axis 3: NeverRetry overrides everything
                    if matches!(step_override, StepRetryOverride::NeverRetry) {
                        return Err(err);
                    }

                    // Axis 2: error classification (pure function of error value)
                    let retryable = err.is_retryable();

                    // Axis 3: ForceRetry overrides classification
                    let should_retry = match step_override {
                        StepRetryOverride::ForceRetry(_) => true,
                        _ => retryable,
                    };

                    // Axis 1: policy (max attempts, backoff)
                    if !should_retry || !effective_policy.should_retry(attempt) {
                        return Err(err);
                    }

                    let delay = effective_policy.delay_for_attempt(attempt);
                    if !delay.is_zero() {
                        std::thread::sleep(delay);
                    }
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Budget tracking
    // -----------------------------------------------------------------------

    /// Check whether the budget is exhausted. Returns an error (suspension)
    /// if any budget dimension is exceeded.
    pub fn check_budget(&self, budget: &crate::agent::budget::Budget) -> DurableResult<()> {
        let state = self.budget_state.lock().unwrap_or_else(|e| e.into_inner());

        if let Some(max) = budget.max_dollars {
            if state.dollars_used >= max {
                return Err(DurableError::Suspended(SuspendReason::BudgetExhausted {
                    dimension: "dollars".into(),
                    limit: format!("{:.2}", max),
                    used: format!("{:.2}", state.dollars_used),
                }));
            }
        }
        if let Some(max) = budget.max_llm_calls {
            if state.llm_calls_used >= max {
                return Err(DurableError::Suspended(SuspendReason::BudgetExhausted {
                    dimension: "llm_calls".into(),
                    limit: max.to_string(),
                    used: state.llm_calls_used.to_string(),
                }));
            }
        }
        if let Some(max) = budget.max_tool_calls {
            if state.tool_calls_used >= max {
                return Err(DurableError::Suspended(SuspendReason::BudgetExhausted {
                    dimension: "tool_calls".into(),
                    limit: max.to_string(),
                    used: state.tool_calls_used.to_string(),
                }));
            }
        }
        if let Some(max_millis) = budget.max_wall_time_millis {
            let elapsed = crate::core::time::now_millis().saturating_sub(state.start_time_millis);
            if state.start_time_millis > 0 && elapsed >= max_millis {
                return Err(DurableError::Suspended(SuspendReason::BudgetExhausted {
                    dimension: "wall_time".into(),
                    limit: format!("{}ms", max_millis),
                    used: format!("{}ms", elapsed),
                }));
            }
        }

        Ok(())
    }

    /// Record budget usage after a live (non-replay) step.
    pub fn record_budget_usage(
        &self,
        dollars: f64,
        llm_calls: u64,
        tool_calls: u64,
    ) -> DurableResult<()> {
        let mut state = self.budget_state.lock().unwrap_or_else(|e| e.into_inner());
        state.dollars_used += dollars;
        state.llm_calls_used += llm_calls;
        state.tool_calls_used += tool_calls;
        if state.start_time_millis == 0 {
            state.start_time_millis = crate::core::time::now_millis();
        }

        self.event_store
            .append(
                self.id,
                EventType::BudgetUpdated {
                    dollars_used: state.dollars_used,
                    llm_calls_used: state.llm_calls_used,
                    tool_calls_used: state.tool_calls_used,
                },
            )
            .map_err(|e| DurableError::Storage(e))?;
        Ok(())
    }

    /// Whether the last step() call was a replay (returned cached result).
    pub fn was_last_step_replay(&self) -> bool {
        self.last_step_was_replay.load(Ordering::SeqCst)
    }

    // -----------------------------------------------------------------------
    // Child flow inspection
    // -----------------------------------------------------------------------

    /// Check if a child flow has a result (for coordinator pattern).
    /// Returns `None` if the child was never started, `Some(None)` if started
    /// but not completed, `Some(Some(result))` if completed.
    pub fn child_flow_result(&self, child_id: &ExecutionId) -> Option<Option<String>> {
        let children = self.child_flows.lock().unwrap_or_else(|e| e.into_inner());
        children.get(&child_id.to_string()).cloned()
    }

    // -----------------------------------------------------------------------
    // Invariant V — Lease lifecycle
    // -----------------------------------------------------------------------

    /// Release the execution lease (called on complete/fail).
    fn release_lease(&self) {
        let _ = self.event_store.release_lease(self.id, self.generation);
    }
}

impl Drop for ReplayContext {
    fn drop(&mut self) {
        // Release the lease on drop to prevent zombie leases
        // when a context is discarded without calling complete()/fail().
        self.release_lease();
    }
}

use crate::json::ToJson;
