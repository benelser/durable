//! Lifecycle hooks — interceptors at LLM and tool boundaries.
//!
//! Hooks are lightweight, ephemeral interceptors that run on every execution
//! (not memoized). They can transform inputs/outputs, reject operations, or
//! override error handling. On replay, hooks run again but the underlying
//! durable step returns cached results regardless.

use crate::agent::llm::{LlmResponse, Message};
use crate::core::error::{DurableError, DurableResult, SuspendReason};
use crate::json::Value;
use std::sync::Arc;

/// What to do when an error occurs in the agent loop.
pub enum ErrorAction {
    /// Retry the failed operation.
    Retry,
    /// Fail the execution with this error.
    Fail(DurableError),
    /// Suspend the execution for external intervention.
    Suspend(SuspendReason),
}

/// Collection of lifecycle hooks. All fields are optional.
///
/// Hooks use `Arc<dyn Fn(...)>` because they must be `Clone + Send + Sync`
/// for parallel tool execution (captured by multiple threads in `thread::scope`).
#[derive(Clone)]
pub struct LifecycleHooks {
    /// Called before each tool execution. Can modify arguments or reject.
    /// Receives: (tool_name, original_args) → modified_args or error.
    pub before_tool: Option<Arc<dyn Fn(&str, &Value) -> DurableResult<Value> + Send + Sync>>,

    /// Called after each tool execution. Can transform the result.
    /// Receives: (tool_name, args, result) → modified_result or error.
    pub after_tool:
        Option<Arc<dyn Fn(&str, &Value, &Value) -> DurableResult<Value> + Send + Sync>>,

    /// Called before each LLM call. Can modify the message list.
    /// Receives: messages → modified_messages or error.
    pub before_llm: Option<Arc<dyn Fn(&[Message]) -> DurableResult<Vec<Message>> + Send + Sync>>,

    /// Called after each LLM call. Can transform or reject the response.
    /// Receives: response → modified_response or error.
    pub after_llm: Option<Arc<dyn Fn(&LlmResponse) -> DurableResult<LlmResponse> + Send + Sync>>,

    /// Called on error in the agent loop. Can override error handling.
    /// Receives: error → action (retry, fail, or suspend).
    pub on_error: Option<Arc<dyn Fn(&DurableError) -> ErrorAction + Send + Sync>>,
}

impl Default for LifecycleHooks {
    fn default() -> Self {
        Self {
            before_tool: None,
            after_tool: None,
            before_llm: None,
            after_llm: None,
            on_error: None,
        }
    }
}
