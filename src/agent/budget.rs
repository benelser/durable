//! Execution budget — cost-aware execution with suspend-on-exhaustion.
//!
//! Every step has a cost (LLM tokens, API calls, wall time). The runtime
//! tracks cumulative spend against a declared budget. When the budget runs
//! out, the agent suspends — not crashes — suspends. The user can approve
//! more budget and resume.
//!
//! Budget state is event-sourced and survives crashes.

use crate::json::{self, FromJson, ToJson, Value};

/// Budget limits for an agent execution.
#[derive(Clone, Debug)]
pub struct Budget {
    /// Maximum dollar cost (None = unlimited).
    pub max_dollars: Option<f64>,
    /// Maximum LLM calls (None = unlimited).
    pub max_llm_calls: Option<u64>,
    /// Maximum tool calls (None = unlimited).
    pub max_tool_calls: Option<u64>,
    /// Maximum wall time in milliseconds (None = unlimited).
    pub max_wall_time_millis: Option<u64>,
}

impl Budget {
    pub fn new() -> Self {
        Self {
            max_dollars: None,
            max_llm_calls: None,
            max_tool_calls: None,
            max_wall_time_millis: None,
        }
    }

    pub fn max_dollars(mut self, v: f64) -> Self {
        self.max_dollars = Some(v);
        self
    }

    pub fn max_llm_calls(mut self, v: u64) -> Self {
        self.max_llm_calls = Some(v);
        self
    }

    pub fn max_tool_calls(mut self, v: u64) -> Self {
        self.max_tool_calls = Some(v);
        self
    }

    pub fn max_wall_time(mut self, d: std::time::Duration) -> Self {
        self.max_wall_time_millis = Some(d.as_millis() as u64);
        self
    }
}

impl Default for Budget {
    fn default() -> Self {
        Self::new()
    }
}

/// Current budget consumption state. Event-sourced — reconstructed from
/// `BudgetUpdated` events on replay.
#[derive(Clone, Debug, Default)]
pub struct BudgetState {
    pub dollars_used: f64,
    pub llm_calls_used: u64,
    pub tool_calls_used: u64,
    pub start_time_millis: u64,
}

impl ToJson for BudgetState {
    fn to_json(&self) -> Value {
        json::json_object(vec![
            ("dollars_used", json::json_num(self.dollars_used)),
            ("llm_calls_used", json::json_num(self.llm_calls_used as f64)),
            ("tool_calls_used", json::json_num(self.tool_calls_used as f64)),
            (
                "start_time_millis",
                json::json_num(self.start_time_millis as f64),
            ),
        ])
    }
}

impl FromJson for BudgetState {
    fn from_json(val: &Value) -> Result<Self, String> {
        Ok(Self {
            dollars_used: val
                .get("dollars_used")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0),
            llm_calls_used: val
                .get("llm_calls_used")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            tool_calls_used: val
                .get("tool_calls_used")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            start_time_millis: val
                .get("start_time_millis")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        })
    }
}
