//! Retry policies for step execution.

use std::time::Duration;

/// Configurable retry policy for steps.
#[derive(Clone, Debug)]
pub struct RetryPolicy {
    /// Maximum number of attempts (including the first try).
    pub max_attempts: u32,
    /// Delay before the first retry.
    pub initial_delay: Duration,
    /// Maximum delay between retries (caps exponential backoff).
    pub max_delay: Duration,
    /// Multiplier applied to delay after each retry.
    pub backoff_multiplier: f64,
}

impl RetryPolicy {
    /// No retries — fail immediately.
    pub const NONE: RetryPolicy = RetryPolicy {
        max_attempts: 1,
        initial_delay: Duration::from_secs(0),
        max_delay: Duration::from_secs(0),
        backoff_multiplier: 1.0,
    };

    /// Standard retry: 3 attempts with 1-2s exponential backoff.
    pub const STANDARD: RetryPolicy = RetryPolicy {
        max_attempts: 3,
        initial_delay: Duration::from_secs(1),
        max_delay: Duration::from_secs(10),
        backoff_multiplier: 2.0,
    };

    /// Aggressive retry: 10 attempts with 100ms-10s backoff.
    pub const AGGRESSIVE: RetryPolicy = RetryPolicy {
        max_attempts: 10,
        initial_delay: Duration::from_millis(100),
        max_delay: Duration::from_secs(10),
        backoff_multiplier: 2.0,
    };

    /// LLM-specific retry: handles rate limits with longer backoff.
    pub const LLM: RetryPolicy = RetryPolicy {
        max_attempts: 5,
        initial_delay: Duration::from_secs(2),
        max_delay: Duration::from_secs(60),
        backoff_multiplier: 3.0,
    };

    /// Calculate the delay for a given attempt number (0-indexed).
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        if attempt == 0 {
            return Duration::ZERO;
        }
        let mut delay = self.initial_delay.as_millis() as f64;
        for _ in 1..attempt {
            delay *= self.backoff_multiplier;
        }
        let capped = delay.min(self.max_delay.as_millis() as f64) as u64;
        Duration::from_millis(capped)
    }

    /// Whether another attempt should be made.
    pub fn should_retry(&self, attempt: u32) -> bool {
        attempt < self.max_attempts
    }
}

/// Trait for errors that know whether they're transient (retryable) or permanent.
pub trait Retryable {
    fn is_retryable(&self) -> bool;
}

impl Retryable for String {
    fn is_retryable(&self) -> bool {
        // Strings are retryable by default (safe default)
        true
    }
}

impl Retryable for std::io::Error {
    fn is_retryable(&self) -> bool {
        use std::io::ErrorKind::*;
        matches!(
            self.kind(),
            ConnectionRefused
                | ConnectionReset
                | ConnectionAborted
                | TimedOut
                | Interrupted
                | WouldBlock
        )
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::STANDARD
    }
}

/// Per-step override for retry behavior.
///
/// This is the third axis of the three-axis error classification:
/// 1. **Policy** — how many times to retry, with what delay (`RetryPolicy`)
/// 2. **Classification** — which errors are retryable (`Retryable` trait)
/// 3. **Override** — per-step escape hatch that overrides the default
#[derive(Clone, Debug)]
pub enum StepRetryOverride {
    /// Use the default retry policy for this step category (LLM, tool, etc.).
    UseDefault,
    /// Force a specific retry policy for this step, overriding the default.
    ForceRetry(RetryPolicy),
    /// Never retry this step, regardless of error classification.
    NeverRetry,
}

impl crate::json::ToJson for RetryPolicy {
    fn to_json(&self) -> crate::json::Value {
        crate::json::json_object(vec![
            ("max_attempts", crate::json::json_num(self.max_attempts as f64)),
            (
                "initial_delay_ms",
                crate::json::json_num(self.initial_delay.as_millis() as f64),
            ),
            (
                "max_delay_ms",
                crate::json::json_num(self.max_delay.as_millis() as f64),
            ),
            (
                "backoff_multiplier",
                crate::json::json_num(self.backoff_multiplier),
            ),
        ])
    }
}

impl crate::json::FromJson for RetryPolicy {
    fn from_json(val: &crate::json::Value) -> Result<Self, String> {
        let obj = val.as_object().ok_or("expected object for RetryPolicy")?;
        Ok(RetryPolicy {
            max_attempts: obj
                .get("max_attempts")
                .and_then(|v| v.as_u64())
                .unwrap_or(3) as u32,
            initial_delay: Duration::from_millis(
                obj.get("initial_delay_ms")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1000),
            ),
            max_delay: Duration::from_millis(
                obj.get("max_delay_ms")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(10000),
            ),
            backoff_multiplier: obj
                .get("backoff_multiplier")
                .and_then(|v| v.as_f64())
                .unwrap_or(2.0),
        })
    }
}
