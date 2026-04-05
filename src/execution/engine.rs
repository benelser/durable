//! Step executor — wraps step execution with retry logic.

use crate::core::error::{DurableError, DurableResult};
use crate::core::retry::RetryPolicy;
use crate::execution::context::ExecutionContext;
use crate::json;
use std::thread;

/// Executes a step with retry logic.
pub struct StepExecutor;

impl StepExecutor {
    /// Execute a step with the given retry policy.
    /// The closure is retried on transient failures according to the policy.
    pub fn execute_with_retry<F>(
        ctx: &ExecutionContext,
        name: &str,
        params: &json::Value,
        policy: &RetryPolicy,
        f: F,
    ) -> DurableResult<json::Value>
    where
        F: Fn() -> DurableResult<json::Value>,
    {
        let mut attempt = 0;
        loop {
            match ctx.step(name, params, &f) {
                Ok(val) => return Ok(val),
                Err(DurableError::Suspended(reason)) => {
                    return Err(DurableError::Suspended(reason));
                }
                Err(err) => {
                    attempt += 1;
                    let retryable = match &err {
                        DurableError::StepFailed { retryable, .. } => *retryable,
                        DurableError::ToolError { retryable, .. } => *retryable,
                        DurableError::LlmError { retryable, .. } => *retryable,
                        DurableError::Io(io_err) => {
                            use crate::core::retry::Retryable;
                            io_err.is_retryable()
                        }
                        _ => false,
                    };

                    if !retryable || !policy.should_retry(attempt) {
                        return Err(err);
                    }

                    let delay = policy.delay_for_attempt(attempt);
                    if !delay.is_zero() {
                        thread::sleep(delay);
                    }
                }
            }
        }
    }
}
