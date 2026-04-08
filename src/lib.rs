//! # Durable Runtime
//!
//! A Rust runtime for durable AI agent execution with crash recovery,
//! step memoization, and language-agnostic tool support.
//!
//! Zero dependencies — pure stdlib Rust.
//!
//! ## Architecture
//!
//! The runtime separates **deterministic orchestration** (what to do, in what order)
//! from **effectful execution** (LLM calls, tool invocations, I/O). Each effectful
//! operation is a **step** whose result is cached. On replay after a crash, the
//! orchestrator re-runs but cached steps return immediately.
//!
//! ## Key Types
//!
//! - [`AgentRuntime`] — the main entry point for running durable agents
//! - [`AgentConfig`] — configuration for an agent (system prompt, model, limits)
//! - [`ToolRegistry`] — maps tool names to definitions and handlers
//! - [`ExecutionContext`] — tracks step numbering and caching during execution
//! - [`ExecutionLog`] — trait for pluggable storage backends
//! - [`InMemoryStorage`] — ephemeral storage for testing
//! - [`FileStorage`] — durable file-based storage for production
//! - [`ExecutionInspector`] — read-only observability queries
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use delite_core::*;
//!
//! fn main() -> Result<(), DurableError> {
//!     // Ephemeral (in-memory) — great for testing
//!     let agent = delite_core::agent_in_memory(
//!         MockLlmClient::new(vec![LlmResponse::text("Hello!")])
//!     );
//!     let response = agent.run("Hi")?;
//!     println!("{response}");
//!
//!     // Durable (file-backed) — survives crashes
//!     // let agent = delite_core::agent("./my-agent", my_llm)?;
//!
//!     Ok(())
//! }
//! ```

pub mod json;
pub mod core;
pub mod storage;
pub mod execution;
pub mod tool;
pub mod agent;
pub mod protocol;
pub mod observe;
pub mod worker;

// =============================================================================
// Level 1 API — what developers need on day 1
// =============================================================================

// The runtime and its builder
pub use agent::runtime::{AgentConfig, AgentRuntime};
pub use agent::response::AgentResponse;

// Errors
pub use core::error::{DurableError, DurableResult, SuspendReason};

// Storage (pick one)
pub use storage::{FileStorage, InMemoryStorage};

// LLM (implement the trait or use provided clients)
pub use agent::llm::{
    LlmClient, LlmRequest, LlmResponse, LlmResponseContent, Message,
    MockLlmClient, ResponseFormat, Role, StreamChunk, TokenUsage,
};

// Tools
pub use tool::ToolDefinition;

// JSON (the json! macro is exported via #[macro_export], Value for tool handlers)
pub use json::Value;

// Execution identity (for resume, inspect, signal)
pub use core::types::ExecutionId;

// =============================================================================
// Level 2 API — power users access via module paths
// e.g., delite_core::tool::ToolRegistry, delite_core::observe::ExecutionInspector
//
// Re-export the most commonly needed Level 2 types for smoother migration.
// =============================================================================

pub use agent::runtime::AgentOutcome;
pub use tool::{ToolCall, ToolRegistry, ToolResult, FnToolHandler, ProcessToolHandler, ToolHandler};
pub use observe::ExecutionInspector;
pub use core::types::{ExecutionMetadata, ExecutionStatus, StepKey, StepRecord, StepStatus};
pub use core::retry::{RetryPolicy, StepRetryOverride};
pub use storage::ExecutionLog;

// Level 2 — internals for power users, tests, and advanced scenarios
pub use agent::llm::ProcessLlmClient;
pub use agent::conversation::Conversation;
pub use execution::context::ExecutionContext;
pub use execution::replay::ReplayContext;
pub use execution::dag::{DagExecutor, execute_parallel};
pub use storage::{
    Event, EventStore, EventType, ExecutionState, FileEventStore,
    InMemoryEventStore, StepSnapshot,
};
pub use agent::budget::Budget;
pub use agent::contract::Contract;
pub use agent::coordinator::AgentCoordinator;
pub use agent::hooks::{ErrorAction, LifecycleHooks};
pub use storage::wal::DurableLog;
pub use storage::wal_store::WalEventStore;
pub use core::hash::fnv1a_hash as core_hash_fnv1a;
pub use core::cancel::CancellationToken;
pub use core::pool::ThreadPool;
pub use core::uuid::Uuid;
pub use core::log::{LogLevel, Logger, NullLogger, StderrJsonLogger};
pub use protocol::{Envelope, ProtocolMessage, PROTOCOL_VERSION};
pub use worker::{Worker, WorkerConfig, WorkerHandle};
pub use json::{parse as parse_json, to_string as json_to_string, to_string_pretty as json_to_string_pretty};

// =============================================================================
// Level 0 API — the "sqlite3_open" moment
// =============================================================================

/// Create a durable agent backed by file storage at the given path.
///
/// This is the simplest way to create a crash-recoverable agent.
/// Both the execution log and event store are backed by the same directory.
///
/// ```rust,ignore
/// let agent = delite_core::agent("./my-agent", my_llm)?;
/// let response = agent.run("Hello")?;
/// println!("{response}");
/// ```
pub fn agent(
    path: impl Into<std::path::PathBuf>,
    llm: impl agent::llm::LlmClient + 'static,
) -> DurableResult<AgentRuntime> {
    let path = path.into();
    let storage = FileStorage::new(&path)
        .map_err(|e| DurableError::Storage(e))?;
    let event_store = storage::FileEventStore::new(&path)
        .map_err(|e| DurableError::Storage(e))?;
    let config = agent::runtime::AgentConfig::default();
    Ok(AgentRuntime::with_event_store(
        config,
        std::sync::Arc::new(storage),
        std::sync::Arc::new(event_store),
        std::sync::Arc::new(llm),
        std::sync::Arc::new(tool::ToolRegistry::new()),
    ))
}

/// Create an ephemeral agent (in-memory storage, lost on exit).
///
/// Perfect for testing, prototyping, or stateless use cases.
///
/// ```rust,ignore
/// let agent = delite_core::agent_in_memory(my_llm);
/// let response = agent.run("Hello")?;
/// println!("{response}");
/// ```
pub fn agent_in_memory(
    llm: impl agent::llm::LlmClient + 'static,
) -> AgentRuntime {
    AgentRuntime::builder().llm(llm).build()
}
