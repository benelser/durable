//! Builder API for AgentRuntime — the "sqlite3_open()" moment.
//!
//! Uses the **type-state pattern** for compile-time enforcement:
//! `.build()` is only available after `.llm()` has been called.
//! An unconfigured runtime cannot be constructed.
//!
//! ```rust,ignore
//! use delite_core::*;
//!
//! let runtime = AgentRuntime::builder()
//!     .llm(MockLlmClient::new(vec![LlmResponse::Text("Hi!".into())]))
//!     .system_prompt("You are helpful.")
//!     .build();
//!
//! let response = runtime.run("Hello")?;
//! ```

use crate::agent::budget::Budget;
use crate::agent::contract::Contract;
use crate::agent::hooks::LifecycleHooks;
use crate::agent::llm::{LlmClient, LlmResponse, Message};
use crate::agent::response::AgentResponse;
use crate::agent::runtime::{AgentConfig, AgentOutcome, AgentRuntime};
use crate::core::error::{DurableError, DurableResult};
use crate::core::log::Logger;
use crate::agent::hooks::ErrorAction;
use crate::core::retry::RetryPolicy;
use crate::core::types::ExecutionId;
use crate::json::Value;
use crate::storage::{EventStore, ExecutionLog, FileEventStore, FileStorage, InMemoryEventStore, InMemoryStorage};
use crate::tool::{FnToolHandler, ToolDefinition, ToolRegistry};
use std::path::PathBuf;
use std::sync::Arc;

/// Helper for accepting both concrete event store types and `Arc<dyn EventStore>`.
pub struct EventStoreArg(Arc<dyn EventStore>);

impl<T: EventStore + 'static> From<T> for EventStoreArg {
    fn from(s: T) -> Self {
        EventStoreArg(Arc::new(s))
    }
}

impl From<Arc<dyn EventStore>> for EventStoreArg {
    fn from(s: Arc<dyn EventStore>) -> Self {
        EventStoreArg(s)
    }
}

/// Helper for accepting both concrete storage types and `Arc<dyn ExecutionLog>`.
pub struct StorageArg(Arc<dyn ExecutionLog>);

impl<T: ExecutionLog + 'static> From<T> for StorageArg {
    fn from(s: T) -> Self {
        StorageArg(Arc::new(s))
    }
}

impl From<Arc<dyn ExecutionLog>> for StorageArg {
    fn from(s: Arc<dyn ExecutionLog>) -> Self {
        StorageArg(s)
    }
}

// ---------------------------------------------------------------------------
// Type-state markers
// ---------------------------------------------------------------------------

/// Marker: LLM client has not been set yet.
pub struct NoLlm(());
/// Marker: LLM client has been set — `.build()` is available.
pub struct HasLlm(());

// ---------------------------------------------------------------------------
// Shared inner state
// ---------------------------------------------------------------------------

struct BuilderInner {
    llm: Option<Arc<dyn LlmClient>>,
    storage: Option<Arc<dyn ExecutionLog>>,
    event_store: Option<Arc<dyn EventStore>>,
    tools: ToolRegistry,
    system_prompt: Option<String>,
    model: Option<String>,
    max_iterations: Option<u32>,
    llm_retry_policy: Option<RetryPolicy>,
    tool_retry_policy: Option<RetryPolicy>,
    max_conversation_messages: Option<usize>,
    temperature: Option<f64>,
    max_tokens: Option<u64>,
    max_concurrent_tools: Option<usize>,
    hooks: LifecycleHooks,
    contracts: Vec<Contract>,
    budget: Option<Budget>,
    logger: Option<Arc<dyn Logger>>,
    execution_timeout: Option<std::time::Duration>,
    llm_call_timeout: Option<std::time::Duration>,
    persistent_error: Option<String>,
}

// ---------------------------------------------------------------------------
// Builder with type-state
// ---------------------------------------------------------------------------

/// Builder for constructing an `AgentRuntime`.
///
/// `L` is a type-state marker:
/// - `NoLlm` — LLM not yet set; `.build()` is unavailable.
/// - `HasLlm` — LLM set; `.build()` compiles.
pub struct AgentRuntimeBuilder<L = NoLlm> {
    inner: BuilderInner,
    _marker: std::marker::PhantomData<L>,
}

// -- Methods available regardless of LLM state --

impl<L> AgentRuntimeBuilder<L> {
    /// Set the LLM client (required). Transitions the builder to the
    /// `HasLlm` state, unlocking `.build()`.
    pub fn llm(self, llm: impl LlmClient + 'static) -> AgentRuntimeBuilder<HasLlm> {
        AgentRuntimeBuilder {
            inner: BuilderInner {
                llm: Some(Arc::new(llm)),
                ..self.inner
            },
            _marker: std::marker::PhantomData,
        }
    }

    /// Set up durable file-backed storage at the given directory.
    ///
    /// This is the one-call equivalent of `.storage(FileStorage::new(path)?)` +
    /// `.event_store(FileEventStore::new(path)?)`. Both the execution log and
    /// event store are backed by the same directory.
    ///
    /// Storage directories are created immediately. If creation fails,
    /// `build()` will return the error.
    ///
    /// ```rust,ignore
    /// let runtime = AgentRuntime::builder()
    ///     .llm(my_llm)
    ///     .persistent("./my-agent-data")
    ///     .build()?;
    /// ```
    pub fn persistent(mut self, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        match FileStorage::new(&path).and_then(|s| {
            FileEventStore::new(&path).map(|e| (s, e))
        }) {
            Ok((storage, event_store)) => {
                self.inner.storage = Some(Arc::new(storage));
                self.inner.event_store = Some(Arc::new(event_store));
            }
            Err(e) => {
                self.inner.persistent_error = Some(e);
            }
        }
        self
    }

    /// Set the storage backend. Defaults to in-memory if not specified.
    pub fn storage(mut self, storage: impl Into<StorageArg>) -> Self {
        self.inner.storage = Some(storage.into().0);
        self
    }

    /// Set the event store for durable replay. Defaults to in-memory if not specified.
    /// Accepts concrete types or `Arc<dyn EventStore>`.
    pub fn event_store(mut self, store: impl Into<EventStoreArg>) -> Self {
        self.inner.event_store = Some(store.into().0);
        self
    }

    /// Set the system prompt.
    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.inner.system_prompt = Some(prompt.into());
        self
    }

    /// Set the model identifier.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.inner.model = Some(model.into());
        self
    }

    /// Register a tool with a closure handler (inline).
    pub fn tool<F>(
        mut self,
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
        handler: F,
    ) -> Self
    where
        F: Fn(&Value) -> DurableResult<Value> + Send + Sync + 'static,
    {
        let def = ToolDefinition::new(name, description).with_parameters(parameters);
        self.inner.tools.register(def, FnToolHandler::new(handler));
        self
    }

    /// Register a tool that requires human confirmation before execution.
    pub fn tool_with_confirmation<F>(
        mut self,
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
        handler: F,
    ) -> Self
    where
        F: Fn(&Value) -> DurableResult<Value> + Send + Sync + 'static,
    {
        let def = ToolDefinition::new(name, description)
            .with_parameters(parameters)
            .with_confirmation();
        self.inner.tools.register(def, FnToolHandler::new(handler));
        self
    }

    /// Add a pre-built `ToolRegistry` (merges with any inline tools).
    pub fn tools(mut self, registry: ToolRegistry) -> Self {
        self.inner.tools = registry;
        self
    }

    /// Set the maximum number of agent loop iterations.
    pub fn max_iterations(mut self, n: u32) -> Self {
        self.inner.max_iterations = Some(n);
        self
    }

    /// Set the LLM retry policy.
    pub fn llm_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.inner.llm_retry_policy = Some(policy);
        self
    }

    /// Set the tool retry policy.
    pub fn tool_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.inner.tool_retry_policy = Some(policy);
        self
    }

    /// Set the maximum conversation length before truncation.
    pub fn max_conversation_messages(mut self, n: usize) -> Self {
        self.inner.max_conversation_messages = Some(n);
        self
    }

    /// Set the LLM temperature.
    pub fn temperature(mut self, t: f64) -> Self {
        self.inner.temperature = Some(t);
        self
    }

    /// Set the maximum tokens per LLM response.
    pub fn max_tokens(mut self, n: u64) -> Self {
        self.inner.max_tokens = Some(n);
        self
    }

    /// Set the maximum concurrent tool executions.
    pub fn max_concurrent_tools(mut self, n: usize) -> Self {
        self.inner.max_concurrent_tools = Some(n);
        self
    }

    // -- Lifecycle hooks --

    /// Register a hook that runs before each tool execution.
    pub fn before_tool<F>(mut self, f: F) -> Self
    where
        F: Fn(&str, &Value) -> DurableResult<Value> + Send + Sync + 'static,
    {
        self.inner.hooks.before_tool = Some(Arc::new(f));
        self
    }

    /// Register a hook that runs after each tool execution.
    pub fn after_tool<F>(mut self, f: F) -> Self
    where
        F: Fn(&str, &Value, &Value) -> DurableResult<Value> + Send + Sync + 'static,
    {
        self.inner.hooks.after_tool = Some(Arc::new(f));
        self
    }

    /// Register a hook that runs before each LLM call.
    pub fn before_llm<F>(mut self, f: F) -> Self
    where
        F: Fn(&[Message]) -> DurableResult<Vec<Message>> + Send + Sync + 'static,
    {
        self.inner.hooks.before_llm = Some(Arc::new(f));
        self
    }

    /// Register a hook that runs after each LLM call.
    pub fn after_llm<F>(mut self, f: F) -> Self
    where
        F: Fn(&LlmResponse) -> DurableResult<LlmResponse> + Send + Sync + 'static,
    {
        self.inner.hooks.after_llm = Some(Arc::new(f));
        self
    }

    /// Register an error handling hook.
    pub fn on_error<F>(mut self, f: F) -> Self
    where
        F: Fn(&DurableError) -> ErrorAction + Send + Sync + 'static,
    {
        self.inner.hooks.on_error = Some(Arc::new(f));
        self
    }

    // -- Agent contracts --

    /// Register an enforceable invariant checked before each tool execution.
    ///
    /// If the contract returns `Err(reason)`, the execution suspends for
    /// human review — the tool does not execute.
    pub fn contract<F>(mut self, name: impl Into<String>, check: F) -> Self
    where
        F: Fn(&str, &Value) -> Result<(), String> + Send + Sync + 'static,
    {
        self.inner.contracts.push(Contract {
            name: name.into(),
            check: Arc::new(check),
        });
        self
    }

    // -- Execution budget --

    /// Set an execution budget. The agent suspends when any dimension is exhausted.
    pub fn budget(mut self, b: Budget) -> Self {
        self.inner.budget = Some(b);
        self
    }

    /// Set a structured logger for runtime tracing.
    pub fn logger(mut self, logger: impl Logger + 'static) -> Self {
        self.inner.logger = Some(Arc::new(logger));
        self
    }

    /// Set a maximum wall-clock time for the entire execution.
    /// The agent suspends when this timeout is reached.
    pub fn execution_timeout(mut self, d: std::time::Duration) -> Self {
        self.inner.execution_timeout = Some(d);
        self
    }

    /// Set the timeout for individual LLM calls (default: 120s).
    pub fn llm_call_timeout(mut self, d: std::time::Duration) -> Self {
        self.inner.llm_call_timeout = Some(d);
        self
    }
}

// -- `.build()` only available when LLM has been set --

impl AgentRuntimeBuilder<HasLlm> {
    /// Build the `AgentRuntime`. This is only callable after `.llm()` has been set.
    ///
    /// Panics if `.persistent()` was called with an invalid path. Use
    /// `try_build()` for a non-panicking alternative.
    pub fn build(self) -> AgentRuntime {
        match self.try_build() {
            Ok(runtime) => runtime,
            Err(e) => panic!("AgentRuntime build failed: {}", e),
        }
    }

    /// Build the `AgentRuntime`, returning an error if configuration is invalid.
    pub fn try_build(self) -> DurableResult<AgentRuntime> {
        if let Some(e) = self.inner.persistent_error {
            return Err(DurableError::Storage(e));
        }

        // SAFETY: llm is always Some when L = HasLlm (set by .llm() which is
        // the only way to reach this type state).
        let llm = self.inner.llm.unwrap();

        let storage: Arc<dyn ExecutionLog> = self
            .inner
            .storage
            .unwrap_or_else(|| Arc::new(InMemoryStorage::new()));

        let defaults = AgentConfig::default();
        let config = AgentConfig {
            system_prompt: self.inner.system_prompt.unwrap_or(defaults.system_prompt),
            model: self.inner.model.or(defaults.model),
            max_iterations: self.inner.max_iterations.unwrap_or(defaults.max_iterations),
            llm_retry_policy: self.inner.llm_retry_policy.unwrap_or(defaults.llm_retry_policy),
            tool_retry_policy: self.inner.tool_retry_policy.unwrap_or(defaults.tool_retry_policy),
            max_conversation_messages: self.inner.max_conversation_messages.or(defaults.max_conversation_messages),
            temperature: self.inner.temperature.or(defaults.temperature),
            max_tokens: self.inner.max_tokens.or(defaults.max_tokens),
            max_concurrent_tools: self.inner.max_concurrent_tools.unwrap_or(defaults.max_concurrent_tools),
            snapshot_interval: defaults.snapshot_interval,
            execution_timeout: self.inner.execution_timeout.or(defaults.execution_timeout),
            llm_call_timeout: self.inner.llm_call_timeout.unwrap_or(defaults.llm_call_timeout),
        };

        let event_store: Arc<dyn EventStore> = self
            .inner
            .event_store
            .unwrap_or_else(|| Arc::new(InMemoryEventStore::new()));

        let mut runtime = AgentRuntime::with_event_store(
            config,
            storage,
            event_store,
            llm,
            Arc::new(self.inner.tools),
        );
        runtime.set_hooks(self.inner.hooks);
        runtime.set_contracts(self.inner.contracts);
        runtime.set_budget(self.inner.budget);
        if let Some(logger) = self.inner.logger {
            runtime.set_logger(logger);
        }
        Ok(runtime)
    }
}

// -- Ergonomic methods on AgentRuntime --

impl AgentRuntime {
    /// Create a builder for constructing an `AgentRuntime`.
    pub fn builder() -> AgentRuntimeBuilder<NoLlm> {
        AgentRuntimeBuilder {
            inner: BuilderInner {
                llm: None,
                storage: None,
                event_store: None,
                tools: ToolRegistry::new(),
                system_prompt: None,
                model: None,
                max_iterations: None,
                llm_retry_policy: None,
                tool_retry_policy: None,
                max_conversation_messages: None,
                temperature: None,
                max_tokens: None,
                max_concurrent_tools: None,
                hooks: LifecycleHooks::default(),
                contracts: Vec::new(),
                budget: None,
                logger: None,
                execution_timeout: None,
                llm_call_timeout: None,
                persistent_error: None,
            },
            _marker: std::marker::PhantomData,
        }
    }

    /// Run the agent with user input. Returns `Result<AgentResponse, DurableError>`.
    ///
    /// This is the ergonomic alternative to `start()` which returns `AgentOutcome`.
    pub fn run(&self, user_input: &str) -> DurableResult<AgentResponse> {
        let exec_id = ExecutionId::new();
        self.run_with_id(exec_id, user_input)
    }

    /// Run the agent with a specific execution ID.
    pub fn run_with_id(&self, exec_id: ExecutionId, user_input: &str) -> DurableResult<AgentResponse> {
        match self.start_with_id(exec_id, user_input) {
            AgentOutcome::Complete { response } => Ok(AgentResponse::completed(exec_id, response)),
            AgentOutcome::Suspended { reason } => Ok(AgentResponse::suspended(exec_id, reason)),
            AgentOutcome::MaxIterations { last_response } => {
                Ok(AgentResponse::max_iterations(exec_id, last_response))
            }
            AgentOutcome::Error { error } => Err(error),
        }
    }

    /// Resume a suspended execution. Returns `Result<AgentResponse, DurableError>`.
    pub fn resume_run(&self, exec_id: ExecutionId) -> DurableResult<AgentResponse> {
        match self.resume(exec_id) {
            AgentOutcome::Complete { response } => Ok(AgentResponse::completed(exec_id, response)),
            AgentOutcome::Suspended { reason } => Ok(AgentResponse::suspended(exec_id, reason)),
            AgentOutcome::MaxIterations { last_response } => {
                Ok(AgentResponse::max_iterations(exec_id, last_response))
            }
            AgentOutcome::Error { error } => Err(error),
        }
    }

    /// Send user input to a suspended execution and resume it.
    pub fn send_input_run(&self, exec_id: ExecutionId, input: &str) -> DurableResult<AgentResponse> {
        match self.send_input(exec_id, input) {
            AgentOutcome::Complete { response } => Ok(AgentResponse::completed(exec_id, response)),
            AgentOutcome::Suspended { reason } => Ok(AgentResponse::suspended(exec_id, reason)),
            AgentOutcome::MaxIterations { last_response } => {
                Ok(AgentResponse::max_iterations(exec_id, last_response))
            }
            AgentOutcome::Error { error } => Err(error),
        }
    }
}
