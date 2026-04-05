//! The durable agent runtime — the core agent loop with crash recovery.
//!
//! This is the main entry point. It orchestrates:
//! 1. Receiving user input
//! 2. Calling the LLM (memoized)
//! 3. Executing tool calls (memoized, parallel when independent)
//! 4. Human-in-the-loop confirmation gates
//! 5. Suspension and resumption across restarts

use crate::agent::conversation::Conversation;
use crate::agent::llm::{self, *};
use crate::core::error::*;
use crate::core::log::{LogLevel, Logger, NullLogger};
use crate::core::retry::RetryPolicy;
use crate::core::types::*;
use crate::execution::context::ExecutionContext;
use crate::execution::replay::ReplayContext;
use crate::json::{self, ToJson, Value};
use crate::storage::{EventStore, ExecutionLog, InMemoryEventStore};
use crate::tool::{ToolCall, ToolRegistry, ToolResult};
use std::sync::Arc;

/// Configuration for an agent.
#[derive(Clone, Debug)]
pub struct AgentConfig {
    /// System prompt for the LLM.
    pub system_prompt: String,
    /// Model identifier (passed to LLM client).
    pub model: Option<String>,
    /// Maximum number of agent loop iterations (safety limit).
    pub max_iterations: u32,
    /// Retry policy for LLM calls.
    pub llm_retry_policy: RetryPolicy,
    /// Retry policy for tool calls.
    pub tool_retry_policy: RetryPolicy,
    /// Maximum conversation length before truncation.
    pub max_conversation_messages: Option<usize>,
    /// LLM temperature.
    pub temperature: Option<f64>,
    /// Max tokens per LLM response.
    pub max_tokens: Option<u64>,
    /// Max concurrent tool executions (thread pool size).
    pub max_concurrent_tools: usize,
    /// Snapshot interval: take a snapshot every N steps for fast resume.
    /// Set to 0 to disable. Default: 50.
    pub snapshot_interval: u64,
    /// Maximum wall-clock time for the entire execution (None = unlimited).
    /// The agent suspends when this timeout is reached.
    pub execution_timeout: Option<std::time::Duration>,
    /// Timeout for individual LLM calls (default: 120s).
    /// If the LLM doesn't respond within this time, the call fails with a retryable error.
    pub llm_call_timeout: std::time::Duration,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            system_prompt: "You are a helpful assistant.".to_string(),
            model: None,
            max_iterations: 50,
            llm_retry_policy: RetryPolicy::LLM,
            tool_retry_policy: RetryPolicy::STANDARD,
            max_conversation_messages: None,
            temperature: None,
            max_tokens: None,
            max_concurrent_tools: 8,
            snapshot_interval: 50,
            execution_timeout: None,
            llm_call_timeout: std::time::Duration::from_secs(120),
        }
    }
}

/// The outcome of running the agent loop.
#[derive(Debug)]
pub enum AgentOutcome {
    /// The agent produced a final text response.
    Complete { response: String },
    /// The agent is suspended waiting for something.
    Suspended { reason: SuspendReason },
    /// The agent hit the iteration limit.
    MaxIterations { last_response: String },
    /// The agent encountered a fatal error.
    Error { error: DurableError },
}

/// The durable agent runtime.
pub struct AgentRuntime {
    config: AgentConfig,
    storage: Arc<dyn ExecutionLog>,
    event_store: Arc<dyn EventStore>,
    llm: Arc<dyn LlmClient>,
    tools: Arc<ToolRegistry>,
    #[allow(dead_code)]
    pool: Arc<crate::core::pool::ThreadPool>,
    hooks: crate::agent::hooks::LifecycleHooks,
    contracts: Arc<Vec<crate::agent::contract::Contract>>,
    budget: Option<crate::agent::budget::Budget>,
    logger: Arc<dyn Logger>,
}

impl AgentRuntime {
    /// Create a new agent runtime.
    pub fn new(
        config: AgentConfig,
        storage: Arc<dyn ExecutionLog>,
        llm: Arc<dyn LlmClient>,
        tools: Arc<ToolRegistry>,
    ) -> Self {
        let pool_size = config.max_concurrent_tools;
        Self {
            config,
            pool: Arc::new(crate::core::pool::ThreadPool::new(pool_size)),
            event_store: Arc::new(InMemoryEventStore::new()),
            storage,
            llm,
            tools,
            hooks: crate::agent::hooks::LifecycleHooks::default(),
            contracts: Arc::new(Vec::new()),
            budget: None,
            logger: Arc::new(NullLogger),
        }
    }

    /// Create a new agent runtime with an explicit event store for replay.
    pub fn with_event_store(
        config: AgentConfig,
        storage: Arc<dyn ExecutionLog>,
        event_store: Arc<dyn EventStore>,
        llm: Arc<dyn LlmClient>,
        tools: Arc<ToolRegistry>,
    ) -> Self {
        let pool_size = config.max_concurrent_tools;
        Self {
            config,
            pool: Arc::new(crate::core::pool::ThreadPool::new(pool_size)),
            event_store,
            storage,
            llm,
            tools,
            hooks: crate::agent::hooks::LifecycleHooks::default(),
            contracts: Arc::new(Vec::new()),
            budget: None,
            logger: Arc::new(NullLogger),
        }
    }

    /// Set lifecycle hooks (called from builder).
    pub fn set_hooks(&mut self, hooks: crate::agent::hooks::LifecycleHooks) {
        self.hooks = hooks;
    }

    /// Set agent contracts (called from builder).
    pub fn set_contracts(&mut self, contracts: Vec<crate::agent::contract::Contract>) {
        self.contracts = Arc::new(contracts);
    }

    /// Set execution budget (called from builder).
    pub fn set_budget(&mut self, budget: Option<crate::agent::budget::Budget>) {
        self.budget = budget;
    }

    /// Set the logger (called from builder).
    pub fn set_logger(&mut self, logger: Arc<dyn Logger>) {
        self.logger = logger;
    }

}

/// Model pricing lookup (per million tokens).
/// Returns (input_price_per_m, output_price_per_m).
fn model_pricing(model: &str) -> (f64, f64) {
    let m = model.to_lowercase();

    // Anthropic Claude
    if m.contains("opus") { return (15.0, 75.0); }
    if m.contains("sonnet") { return (3.0, 15.0); }
    if m.contains("haiku") { return (0.25, 1.25); }

    // OpenAI GPT-4o
    if m.contains("gpt-4o-mini") { return (0.15, 0.60); }
    if m.contains("gpt-4o") { return (2.50, 10.0); }
    if m.contains("gpt-4-turbo") { return (10.0, 30.0); }
    if m.contains("gpt-4") { return (30.0, 60.0); }
    if m.contains("gpt-3.5") { return (0.50, 1.50); }
    if m.contains("o1") { return (15.0, 60.0); }
    if m.contains("o3") { return (10.0, 40.0); }

    // Google Gemini
    if m.contains("gemini-2") { return (0.075, 0.30); }
    if m.contains("gemini-1.5-pro") { return (1.25, 5.0); }
    if m.contains("gemini-1.5-flash") { return (0.075, 0.30); }

    // Default: mid-range pricing
    (3.0, 15.0)
}

impl AgentRuntime {
    /// Get the event store (for advanced use, inspection, etc.).
    pub fn event_store(&self) -> &Arc<dyn EventStore> {
        &self.event_store
    }

    /// Get an inspector for querying execution state.
    pub fn inspector(&self) -> crate::observe::ExecutionInspector {
        crate::observe::ExecutionInspector::new(self.storage.clone())
    }

    /// Start a new agent execution with the given user input.
    pub fn start(&self, user_input: &str) -> AgentOutcome {
        let exec_id = ExecutionId::new();
        self.start_with_id(exec_id, user_input)
    }

    /// Start a new execution with a specific ID (for resumption scenarios).
    pub fn start_with_id(&self, exec_id: ExecutionId, user_input: &str) -> AgentOutcome {
        if let Err(e) = self.storage.create_execution(exec_id) {
            return AgentOutcome::Error {
                error: DurableError::Storage(e),
            };
        }

        // Create the event-sourced replay context for durable step execution
        let replay_ctx = match ReplayContext::new(exec_id, self.event_store.clone(), Some(&self.config.system_prompt)) {
            Ok(ctx) => ctx,
            Err(e) => return AgentOutcome::Error { error: e },
        };

        let ctx = ExecutionContext::new(exec_id, self.storage.clone());
        let mut conversation = Conversation::with_system_prompt(&self.config.system_prompt);
        conversation.push(Message::user(user_input));

        self.run_loop(&ctx, &replay_ctx, &mut conversation)
    }

    /// Resume a suspended execution after providing a signal.
    pub fn resume(&self, exec_id: ExecutionId) -> AgentOutcome {
        // Load execution metadata
        match self.storage.get_execution(exec_id) {
            Ok(Some(_)) => {}
            Ok(None) => {
                return AgentOutcome::Error {
                    error: DurableError::NotFound(format!("execution {}", exec_id)),
                }
            }
            Err(e) => {
                return AgentOutcome::Error {
                    error: DurableError::Storage(e),
                }
            }
        };

        // Resume the event-sourced replay context (loads history, increments generation)
        // Passes system_prompt for drift detection (Invariant I).
        let replay_ctx = match ReplayContext::resume(exec_id, self.event_store.clone(), Some(&self.config.system_prompt)) {
            Ok(ctx) => ctx,
            Err(e) => return AgentOutcome::Error { error: e },
        };

        // Create CRUD context and clear suspension
        let ctx = ExecutionContext::new(exec_id, self.storage.clone());
        if let Err(e) = ctx.clear_suspension() {
            return AgentOutcome::Error { error: e };
        }

        // Reconstruct conversation from event store (single source of truth)
        let mut conversation = Conversation::with_system_prompt(&self.config.system_prompt);
        match self.reconstruct_conversation(&replay_ctx, &mut conversation) {
            Ok(()) => {}
            Err(e) => return AgentOutcome::Error { error: e },
        }

        self.run_loop(&ctx, &replay_ctx, &mut conversation)
    }

    /// Send a signal to a suspended execution (user input, confirmation, etc.).
    pub fn signal(
        &self,
        exec_id: ExecutionId,
        signal_name: &str,
        data: Value,
    ) -> DurableResult<()> {
        let data_str = json::to_string(&data);
        self.storage
            .store_signal(exec_id, signal_name, &data_str)
            .map_err(DurableError::Storage)
    }

    /// Send user input to a suspended execution and resume it.
    pub fn send_input(&self, exec_id: ExecutionId, input: &str) -> AgentOutcome {
        // Find the expected input signal name from the suspend reason
        let meta = match self.storage.get_execution(exec_id) {
            Ok(Some(meta)) => meta,
            Ok(None) => {
                return AgentOutcome::Error {
                    error: DurableError::NotFound(format!("execution {}", exec_id)),
                }
            }
            Err(e) => {
                return AgentOutcome::Error {
                    error: DurableError::Storage(e),
                }
            }
        };

        // Store the input as a signal
        let signal_name = match &meta.suspend_reason {
            Some(SuspendReason::WaitingForInput { .. }) => {
                format!("__user_input")
            }
            Some(SuspendReason::WaitingForSignal { signal_name }) => signal_name.clone(),
            _ => "__user_input".to_string(),
        };

        let data = json::json_str(input);
        if let Err(e) = self
            .storage
            .store_signal(exec_id, &signal_name, &json::to_string(&data))
        {
            return AgentOutcome::Error {
                error: DurableError::Storage(e),
            };
        }

        self.resume(exec_id)
    }

    /// Approve a confirmation gate.
    pub fn approve_confirmation(&self, exec_id: ExecutionId, confirmation_id: &str) -> DurableResult<()> {
        self.signal(exec_id, confirmation_id, json::json_bool(true))
    }

    /// Reject a confirmation gate.
    pub fn reject_confirmation(
        &self,
        exec_id: ExecutionId,
        confirmation_id: &str,
        reason: &str,
    ) -> DurableResult<()> {
        self.signal(
            exec_id,
            confirmation_id,
            json::json_object(vec![
                ("approved", json::json_bool(false)),
                ("reason", json::json_str(reason)),
            ]),
        )
    }

    /// The core agent loop.
    fn run_loop(
        &self,
        ctx: &ExecutionContext,
        replay_ctx: &ReplayContext,
        conversation: &mut Conversation,
    ) -> AgentOutcome {
        let last_response = String::new();
        let loop_start = std::time::Instant::now();
        let exec_id_str = ctx.id.to_string();

        self.logger.log(
            LogLevel::Info,
            &[("execution_id", &exec_id_str)],
            "agent loop started",
        );

        for _iteration in 0..self.config.max_iterations {
            // Check execution timeout
            if let Some(timeout) = self.config.execution_timeout {
                if loop_start.elapsed() >= timeout {
                    self.logger.log(
                        LogLevel::Warn,
                        &[("execution_id", &exec_id_str)],
                        "execution timeout reached",
                    );
                    let reason = SuspendReason::BudgetExhausted {
                        dimension: "wall_time".into(),
                        limit: format!("{}ms", timeout.as_millis()),
                        used: format!("{}ms", loop_start.elapsed().as_millis()),
                    };
                    return AgentOutcome::Suspended { reason };
                }
            }
            // Truncate conversation if needed
            if let Some(max) = self.config.max_conversation_messages {
                conversation.truncate(max);
            }

            // before_llm hook: can transform messages before the LLM sees them
            let effective_messages = if let Some(ref hook) = self.hooks.before_llm {
                match hook(conversation.messages()) {
                    Ok(msgs) => msgs,
                    Err(e) => return self.handle_error(ctx, replay_ctx, e),
                }
            } else {
                conversation.messages().to_vec()
            };

            // Step: call the LLM (using replay context for durable step execution)
            let llm_params = conversation.to_json();
            let tools_json = self.tools.to_function_json();

            let llm_result = self.durable_step(
                ctx,
                replay_ctx,
                "llm_call",
                &llm_params,
                || {
                    let request = LlmRequest {
                        messages: effective_messages.clone(),
                        tools: if self.tools.definitions().is_empty() {
                            None
                        } else {
                            Some(tools_json.clone())
                        },
                        model: self.config.model.clone(),
                        temperature: self.config.temperature,
                        max_tokens: self.config.max_tokens,
                        response_format: None,
                    };
                    // Use streaming API — providers that support it will emit
                    // tokens in real-time. The complete response is still memoized.
                    let response = self.llm.chat_stream(&request, &|_chunk| {
                        // Streaming chunks are ephemeral — not persisted.
                        // In SDK mode, the protocol layer forwards these as text_delta events.
                    })?;
                    // Serialize the response for caching
                    let mut entries = match &response.content {
                        LlmResponseContent::Text(text) => vec![
                            ("type", json::json_str("text")),
                            ("content", json::json_str(text)),
                        ],
                        LlmResponseContent::ToolCalls(calls) => vec![
                            ("type", json::json_str("tool_calls")),
                            (
                                "tool_calls",
                                json::json_array(calls.iter().map(|c| c.to_json()).collect()),
                            ),
                        ],
                    };
                    // Include token usage if reported
                    if let Some(ref usage) = response.usage {
                        entries.push(("input_tokens", json::json_num(usage.input_tokens as f64)));
                        entries.push(("output_tokens", json::json_num(usage.output_tokens as f64)));
                    }
                    Ok(json::json_object(entries))
                },
            );

            let llm_response_val = match llm_result {
                Ok(val) => val,
                Err(DurableError::Suspended(reason)) => {
                    return AgentOutcome::Suspended { reason };
                }
                Err(err) => {
                    return self.handle_error(ctx, replay_ctx, err);
                }
            };

            // Reconstruct LlmResponse for the after_llm hook
            let response_type = llm_response_val
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("text");

            // after_llm hook: can transform or reject the LLM response
            if let Some(ref hook) = self.hooks.after_llm {
                let parsed_response = match response_type {
                    "text" => {
                        let text = llm_response_val.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        LlmResponse::text(text)
                    }
                    "tool_calls" => {
                        let calls: Vec<crate::tool::ToolCall> = llm_response_val
                            .get("tool_calls").and_then(|v| v.as_array()).unwrap_or(&[])
                            .iter().filter_map(|c| crate::json::FromJson::from_json(c).ok()).collect();
                        LlmResponse::tool_calls(calls)
                    }
                    _ => LlmResponse::text(""),
                };
                if let Err(e) = hook(&parsed_response) {
                    return self.handle_error(ctx, replay_ctx, e);
                }
            }

            match response_type {
                "text" => {
                    let text = llm_response_val
                        .get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    conversation.push(Message::assistant_text(&text));

                    // Store the conversation state
                    if let Err(e) = self.storage.set_tag(
                        ctx.id,
                        "conversation",
                        &json::to_string(&conversation.to_json()),
                    ) {
                        return AgentOutcome::Error {
                            error: DurableError::Storage(e),
                        };
                    }

                    // Update execution as completed (validated transition)
                    if let Err(e) = self.transition_status(
                        ctx.id,
                        ExecutionStatus::Running,
                        ExecutionStatus::Completed,
                    ) {
                        return AgentOutcome::Error { error: e };
                    }

                    return AgentOutcome::Complete { response: text };
                }
                "tool_calls" => {
                    let calls: Vec<ToolCall> = llm_response_val
                        .get("tool_calls")
                        .and_then(|v| v.as_array())
                        .unwrap_or(&[])
                        .iter()
                        .filter_map(|c| crate::json::FromJson::from_json(c).ok())
                        .collect();

                    if calls.is_empty() {
                        if let Err(e) = self.transition_status(
                            ctx.id,
                            ExecutionStatus::Running,
                            ExecutionStatus::Completed,
                        ) {
                            return AgentOutcome::Error { error: e };
                        }
                        return AgentOutcome::Complete {
                            response: String::new(),
                        };
                    }

                    // Add assistant message with tool calls
                    conversation.push(Message::assistant_tool_calls(calls.clone()));

                    // Execute tool calls
                    let tool_results = match self.execute_tool_calls(ctx, replay_ctx, &calls) {
                        Ok(results) => results,
                        Err(DurableError::Suspended(reason)) => {
                            return AgentOutcome::Suspended { reason };
                        }
                        Err(err) => {
                            return self.handle_error(ctx, replay_ctx, err);
                        }
                    };

                    // Add tool results to conversation
                    for result in &tool_results {
                        conversation.push(Message::tool_result(
                            &result.call_id,
                            result.output.clone(),
                            result.is_error,
                        ));
                    }

                    // Loop back to call LLM again with tool results
                }
                other => {
                    return AgentOutcome::Error {
                        error: DurableError::InvalidState(format!(
                            "unknown LLM response type: {}",
                            other
                        )),
                    };
                }
            }
        }

        AgentOutcome::MaxIterations {
            last_response,
        }
    }

    /// Execute a batch of tool calls (parallel when possible).
    ///
    /// Each tool call is individually memoized via `durable_step`, so on crash
    /// recovery, completed tools are skipped and only incomplete tools re-execute.
    fn execute_tool_calls(
        &self,
        ctx: &ExecutionContext,
        replay_ctx: &ReplayContext,
        calls: &[ToolCall],
    ) -> DurableResult<Vec<ToolResult>> {
        if calls.len() == 1 {
            return self
                .execute_single_tool(ctx, replay_ctx, &calls[0])
                .map(|r| vec![r]);
        }

        // Multiple tool calls — check for confirmation requirements first
        for call in calls {
            if self.tools.requires_confirmation(&call.name) {
                ctx.request_confirmation(&call.name, &call.arguments)?;
            }
        }

        // Execute in parallel using scoped threads so each tool call routes
        // through durable_step (memoized individually in the event store).
        let results: std::sync::Mutex<Vec<(usize, DurableResult<ToolResult>)>> =
            std::sync::Mutex::new(Vec::new());

        std::thread::scope(|s| {
            for (i, call) in calls.iter().enumerate() {
                let results = &results;
                s.spawn(move || {
                    let step_name = format!("tool_{}", call.name);
                    let tools = self.tools.clone();
                    let call_clone = call.clone();

                    let step_result = self.durable_step(
                        ctx,
                        replay_ctx,
                        &step_name,
                        &call.arguments,
                        || {
                            match tools.execute(&call_clone.name, &call_clone.arguments) {
                                Ok(output) => Ok(json::json_object(vec![
                                    ("call_id", json::json_str(&call_clone.id)),
                                    ("output", output),
                                    ("is_error", json::json_bool(false)),
                                ])),
                                Err(err) => Ok(json::json_object(vec![
                                    ("call_id", json::json_str(&call_clone.id)),
                                    ("output", json::json_str(&err.to_string())),
                                    ("is_error", json::json_bool(true)),
                                ])),
                            }
                        },
                    );

                    let tool_result = step_result.map(|val| {
                        ToolResult {
                            call_id: val
                                .get("call_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            output: val.get("output").cloned().unwrap_or(Value::Null),
                            is_error: val
                                .get("is_error")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false),
                        }
                    });

                    results.lock().unwrap().push((i, tool_result));
                });
            }
        });

        // Collect results in original call order
        let mut indexed_results = results.into_inner().unwrap();
        indexed_results.sort_by_key(|(i, _)| *i);

        let mut out = Vec::new();
        for (_, result) in indexed_results {
            out.push(result?);
        }
        Ok(out)
    }

    /// Execute a single tool call with memoization and optional confirmation.
    fn execute_single_tool(
        &self,
        ctx: &ExecutionContext,
        replay_ctx: &ReplayContext,
        call: &ToolCall,
    ) -> DurableResult<ToolResult> {
        // Check if confirmation is required
        if self.tools.requires_confirmation(&call.name) {
            // Use replay context's step count for stable confirmation IDs
            let confirmation_id = format!(
                "confirm_{}_{}_{}",
                call.name, ctx.id, replay_ctx.step_count()
            );
            match self.storage.peek_signal(ctx.id, &confirmation_id) {
                Ok(Some(data)) => {
                    let _ = self.storage.consume_signal(ctx.id, &confirmation_id);
                    if let Ok(val) = json::parse(&data) {
                        if val.as_bool() != Some(true) {
                            return Err(DurableError::Rejected {
                                tool_name: call.name.clone(),
                                reason: val.get("reason").and_then(|v| v.as_str())
                                    .unwrap_or("rejected by human").to_string(),
                            });
                        }
                    }
                }
                Ok(None) => {
                    let reason = SuspendReason::WaitingForConfirmation {
                        tool_name: call.name.clone(),
                        arguments: call.arguments.clone(),
                        confirmation_id,
                    };
                    let _ = self.storage.set_suspend_reason(ctx.id, Some(reason.clone()));
                    let _ = self.transition_status(
                        ctx.id,
                        ExecutionStatus::Running,
                        ExecutionStatus::Suspended,
                    );
                    return Err(DurableError::Suspended(reason));
                }
                Err(e) => return Err(DurableError::Storage(e)),
            }
        }

        // Semantic step name: "tool_{name}" — occurrence counter handles uniqueness
        let step_name = format!("tool_{}", call.name);

        // Contract check: after confirmation, before execution
        self.check_contracts(replay_ctx, &step_name, &call.arguments)?;

        // before_tool hook: can modify arguments
        let effective_args = if let Some(ref hook) = self.hooks.before_tool {
            hook(&call.name, &call.arguments)?
        } else {
            call.arguments.clone()
        };

        let tools = self.tools.clone();
        let call_clone = call.clone();

        let result = self.durable_step(ctx, replay_ctx, &step_name, &effective_args, || {
            match tools.execute(&call_clone.name, &call_clone.arguments) {
                Ok(output) => Ok(json::json_object(vec![
                    ("call_id", json::json_str(&call_clone.id)),
                    ("output", output),
                    ("is_error", json::json_bool(false)),
                ])),
                Err(err) => Ok(json::json_object(vec![
                    ("call_id", json::json_str(&call_clone.id)),
                    ("output", json::json_str(&err.to_string())),
                    ("is_error", json::json_bool(true)),
                ])),
            }
        })?;

        // after_tool hook: can transform result
        let result = if let Some(ref hook) = self.hooks.after_tool {
            hook(&call.name, &call.arguments, &result)?
        } else {
            result
        };

        Ok(ToolResult {
            call_id: result
                .get("call_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            output: result.get("output").cloned().unwrap_or(Value::Null),
            is_error: result
                .get("is_error")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        })
    }

    /// Reconstruct conversation from the event store (single source of truth).
    /// Reads completed step results from the replay context's state,
    /// not from the CRUD storage layer.
    fn reconstruct_conversation(
        &self,
        replay_ctx: &ReplayContext,
        conversation: &mut Conversation,
    ) -> DurableResult<()> {
        let state = replay_ctx.current_state()?;

        // Iterate step results in step_number order (preserves execution order)
        for (_step_num, snapshot) in &state.step_results {
            if !snapshot.completed {
                continue;
            }
            if let Some(ref result_str) = snapshot.result {
                if let Ok(val) = json::parse(result_str) {
                    // LLM call results
                    if snapshot.step_name == "llm_call" || snapshot.step_name.starts_with("llm_call_") {
                        match val.get("type").and_then(|v| v.as_str()) {
                            Some("text") => {
                                let text = val
                                    .get("content")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                conversation.push(Message::assistant_text(&text));
                            }
                            Some("tool_calls") => {
                                if let Some(calls_val) = val.get("tool_calls") {
                                    let calls: Vec<ToolCall> = calls_val
                                        .as_array()
                                        .unwrap_or(&[])
                                        .iter()
                                        .filter_map(|c| {
                                            crate::json::FromJson::from_json(c).ok()
                                        })
                                        .collect();
                                    conversation
                                        .push(Message::assistant_tool_calls(calls));
                                }
                            }
                            _ => {}
                        }
                    }
                    // Tool results
                    else if snapshot.step_name.starts_with("tool_") {
                        let call_id = val
                            .get("call_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let output = val.get("output").cloned().unwrap_or(Value::Null);
                        let is_error = val
                            .get("is_error")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        conversation.push(Message::tool_result(&call_id, output, is_error));
                    }
                }
            }
        }

        Ok(())
    }

    /// Execute a durable step via the replay context, also syncing to CRUD storage.
    fn durable_step<F>(
        &self,
        ctx: &ExecutionContext,
        replay_ctx: &ReplayContext,
        name: &str,
        params: &json::Value,
        f: F,
    ) -> DurableResult<json::Value>
    where
        F: FnOnce() -> DurableResult<json::Value>,
    {
        let exec_id_str = ctx.id.to_string();
        let step_num = replay_ctx.step_count().to_string();

        // Budget check before execution
        if let Some(ref budget) = self.budget {
            replay_ctx.check_budget(budget)?;
        }

        self.logger.log(
            LogLevel::Debug,
            &[("execution_id", &exec_id_str), ("step", name), ("step_num", &step_num)],
            "step started",
        );

        // Use lenient mode: conversation reconstruction on resume may produce
        // different param hashes. Semantic identity (name + occurrence) is sufficient.
        let step_start = std::time::Instant::now();
        let result = replay_ctx.step_lenient(name, params, f);

        let elapsed_ms = step_start.elapsed().as_millis().to_string();
        let replayed = replay_ctx.was_last_step_replay();

        match &result {
            Ok(_) => {
                self.logger.log(
                    LogLevel::Info,
                    &[
                        ("execution_id", &exec_id_str),
                        ("step", name),
                        ("step_num", &step_num),
                        ("duration_ms", &elapsed_ms),
                        ("replayed", if replayed { "true" } else { "false" }),
                    ],
                    "step completed",
                );
            }
            Err(DurableError::Suspended(reason)) => {
                let reason_str = format!("{:?}", reason);
                self.logger.log(
                    LogLevel::Info,
                    &[
                        ("execution_id", &exec_id_str),
                        ("step", name),
                        ("reason", &reason_str),
                    ],
                    "step suspended",
                );
            }
            Err(err) => {
                let err_str = err.to_string();
                self.logger.log(
                    LogLevel::Error,
                    &[
                        ("execution_id", &exec_id_str),
                        ("step", name),
                        ("step_num", &step_num),
                        ("error", &err_str),
                    ],
                    "step failed",
                );
            }
        }

        // Record budget usage for live (non-replay) steps
        if let Ok(ref val) = result {
            if !replayed {
                let is_llm = name == "llm_call" || name.starts_with("llm_call_");
                let is_tool = name.starts_with("tool_");
                let llm_calls = if is_llm { 1u64 } else { 0 };
                let tool_calls = if is_tool { 1u64 } else { 0 };

                // Extract token usage from the cached LLM result for cost tracking
                let dollars = if is_llm {
                    let input_tokens = val.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    let output_tokens = val.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    let usage = llm::TokenUsage { input_tokens, output_tokens };
                    let model_name = self.config.model.as_deref().unwrap_or("");
                    let (input_price, output_price) = model_pricing(model_name);
                    usage.cost(input_price, output_price)
                } else {
                    0.0
                };

                let _ = replay_ctx.record_budget_usage(dollars, llm_calls, tool_calls);
            }
        }

        // Periodic snapshot for fast resume of long-running agents
        if self.config.snapshot_interval > 0 {
            let _ = replay_ctx.maybe_snapshot(self.config.snapshot_interval);
        }

        // Sync to CRUD storage for observability (inspector reads from here)
        let step_number = replay_ctx.step_count().saturating_sub(1);
        let param_str = json::to_string(params);
        let param_hash = crate::core::hash::hash_params(&param_str);
        let key = StepKey {
            execution_id: ctx.id,
            step_number,
            step_name: name.to_string(),
            param_hash,
        };
        let _ = self.storage.log_step_start(key.clone());
        match &result {
            Ok(val) => {
                let _ = self.storage.log_step_completion(
                    &key,
                    Some(json::to_string(val)),
                    None,
                    false,
                );
            }
            Err(DurableError::Suspended(_)) => {}
            Err(err) => {
                let _ = self.storage.log_step_completion(
                    &key,
                    None,
                    Some(err.to_string()),
                    false,
                );
            }
        }

        result
    }

    /// Check all registered contracts against a tool call.
    /// Returns `Err(Suspended(ContractViolation))` on first violation.
    fn check_contracts(
        &self,
        replay_ctx: &ReplayContext,
        step_name: &str,
        args: &json::Value,
    ) -> DurableResult<()> {
        for contract in self.contracts.iter() {
            let result = (contract.check)(step_name, args);
            let passed = result.is_ok();
            let reason = result.err();

            // Record contract check as an auditable event
            let _ = replay_ctx.event_store().append(
                replay_ctx.id,
                crate::storage::event::EventType::ContractChecked {
                    step_name: step_name.to_string(),
                    contract_name: contract.name.clone(),
                    passed,
                    reason: reason.clone(),
                },
            );

            if let Some(reason) = reason {
                let suspend = SuspendReason::ContractViolation {
                    contract_name: contract.name.clone(),
                    step_name: step_name.to_string(),
                    reason,
                };
                return Err(DurableError::Suspended(suspend));
            }
        }
        Ok(())
    }

    /// Handle a fatal error in the agent loop. If compensations are registered,
    /// run them before returning the error.
    fn handle_error(
        &self,
        ctx: &ExecutionContext,
        replay_ctx: &ReplayContext,
        error: DurableError,
    ) -> AgentOutcome {
        if matches!(&error, DurableError::Suspended(..)) {
            if let DurableError::Suspended(reason) = error {
                return AgentOutcome::Suspended { reason };
            }
        }

        // on_error hook: can override error handling
        if let Some(ref hook) = self.hooks.on_error {
            use crate::agent::hooks::ErrorAction;
            match hook(&error) {
                ErrorAction::Retry => {
                    // Caller should retry — but we're in handle_error, not the retry loop.
                    // Return the error and let the caller decide.
                }
                ErrorAction::Fail(override_err) => {
                    return AgentOutcome::Error { error: override_err };
                }
                ErrorAction::Suspend(reason) => {
                    return AgentOutcome::Suspended { reason };
                }
            }
        }

        // If compensations are registered, run them
        if replay_ctx.has_compensations() {
            let _ = self.transition_status(
                ctx.id,
                ExecutionStatus::Running,
                ExecutionStatus::Compensating,
            );

            let comp_results = replay_ctx.compensate();

            let all_ok = comp_results
                .as_ref()
                .map(|results| results.iter().all(|(_, r)| r.is_ok()))
                .unwrap_or(false);

            if all_ok {
                let _ = self.transition_status(
                    ctx.id,
                    ExecutionStatus::Compensating,
                    ExecutionStatus::Compensated,
                );
            } else {
                let _ = self.transition_status(
                    ctx.id,
                    ExecutionStatus::Compensating,
                    ExecutionStatus::CompensationFailed,
                );
            }
        } else {
            let _ = self.transition_status(
                ctx.id,
                ExecutionStatus::Running,
                ExecutionStatus::Failed,
            );
        }

        AgentOutcome::Error { error }
    }

    /// Transition execution status with validation. Checks the state machine before updating.
    fn transition_status(
        &self,
        exec_id: ExecutionId,
        from: ExecutionStatus,
        to: ExecutionStatus,
    ) -> DurableResult<()> {
        from.transition_to(&to)?;
        self.storage
            .update_execution_status(exec_id, to)
            .map_err(DurableError::Storage)
    }

    /// Get the execution ID from a running agent (for external use).
    pub fn get_execution_status(
        &self,
        exec_id: ExecutionId,
    ) -> DurableResult<Option<ExecutionMetadata>> {
        self.storage.get_execution(exec_id).map_err(DurableError::Storage)
    }
}
