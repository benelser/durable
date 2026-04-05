//! LLM abstraction — trait for calling language models.
//!
//! The runtime is LLM-agnostic. Implement [`LlmClient`] for your provider.

use crate::core::error::{DurableError, DurableResult};
use crate::json::{self, FromJson, ToJson, Value};
use crate::tool::ToolCall;

/// A message in the conversation.
#[derive(Clone, Debug)]
pub struct Message {
    pub role: Role,
    pub content: MessageContent,
}

/// Message role.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// Message content — text, tool calls, or tool results.
#[derive(Clone, Debug)]
pub enum MessageContent {
    Text(String),
    ToolCalls(Vec<ToolCall>),
    ToolResult {
        call_id: String,
        output: Value,
        is_error: bool,
    },
}

impl Message {
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: MessageContent::Text(text.into()),
        }
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: MessageContent::Text(text.into()),
        }
    }

    pub fn assistant_text(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: MessageContent::Text(text.into()),
        }
    }

    pub fn assistant_tool_calls(calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: MessageContent::ToolCalls(calls),
        }
    }

    pub fn tool_result(call_id: impl Into<String>, output: Value, is_error: bool) -> Self {
        Self {
            role: Role::Tool,
            content: MessageContent::ToolResult {
                call_id: call_id.into(),
                output,
                is_error,
            },
        }
    }
}

impl ToJson for Role {
    fn to_json(&self) -> Value {
        json::json_str(match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        })
    }
}

impl FromJson for Role {
    fn from_json(val: &Value) -> Result<Self, String> {
        match val.as_str() {
            Some("system") => Ok(Role::System),
            Some("user") => Ok(Role::User),
            Some("assistant") => Ok(Role::Assistant),
            Some("tool") => Ok(Role::Tool),
            _ => Err("invalid role".to_string()),
        }
    }
}

impl ToJson for Message {
    fn to_json(&self) -> Value {
        let mut entries = vec![("role", self.role.to_json())];
        match &self.content {
            MessageContent::Text(text) => {
                entries.push(("content", json::json_str(text)));
            }
            MessageContent::ToolCalls(calls) => {
                entries.push((
                    "tool_calls",
                    json::json_array(calls.iter().map(|c| c.to_json()).collect()),
                ));
            }
            MessageContent::ToolResult {
                call_id,
                output,
                is_error,
            } => {
                entries.push(("tool_call_id", json::json_str(call_id)));
                entries.push(("content", json::json_str(&json::to_string(output))));
                if *is_error {
                    entries.push(("is_error", json::json_bool(true)));
                }
            }
        }
        json::json_object(entries)
    }
}

impl FromJson for Message {
    fn from_json(val: &Value) -> Result<Self, String> {
        let role = Role::from_json(val.get("role").ok_or("missing role")?)?;

        let content = if let Some(tool_calls) = val.get("tool_calls") {
            let calls: Vec<ToolCall> = tool_calls
                .as_array()
                .ok_or("tool_calls must be array")?
                .iter()
                .map(ToolCall::from_json)
                .collect::<Result<_, _>>()?;
            MessageContent::ToolCalls(calls)
        } else if role == Role::Tool {
            MessageContent::ToolResult {
                call_id: val
                    .get("tool_call_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                output: val
                    .get("content")
                    .and_then(|v| v.as_str())
                    .map(|s| json::parse(s).unwrap_or(json::json_str(s)))
                    .unwrap_or(Value::Null),
                is_error: val
                    .get("is_error")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            }
        } else {
            MessageContent::Text(
                val.get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            )
        };

        Ok(Message { role, content })
    }
}

/// Token usage from an LLM call.
#[derive(Clone, Debug, Default)]
pub struct TokenUsage {
    /// Number of tokens in the prompt/input.
    pub input_tokens: u64,
    /// Number of tokens in the completion/output.
    pub output_tokens: u64,
}

impl TokenUsage {
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }

    /// Estimate cost in dollars based on per-token pricing.
    pub fn cost(&self, input_price_per_m: f64, output_price_per_m: f64) -> f64 {
        (self.input_tokens as f64 * input_price_per_m / 1_000_000.0)
            + (self.output_tokens as f64 * output_price_per_m / 1_000_000.0)
    }
}

/// The LLM's response.
#[derive(Clone, Debug)]
pub struct LlmResponse {
    /// The response content — text or tool calls.
    pub content: LlmResponseContent,
    /// Token usage (if reported by the provider).
    pub usage: Option<TokenUsage>,
    /// Which model actually served the request (for routing/fallback).
    pub model: Option<String>,
}

impl LlmResponse {
    /// Create a text response.
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            content: LlmResponseContent::Text(s.into()),
            usage: None,
            model: None,
        }
    }

    /// Create a tool calls response.
    pub fn tool_calls(calls: Vec<ToolCall>) -> Self {
        Self {
            content: LlmResponseContent::ToolCalls(calls),
            usage: None,
            model: None,
        }
    }

    /// Attach token usage.
    pub fn with_usage(mut self, usage: TokenUsage) -> Self {
        self.usage = Some(usage);
        self
    }

    /// Attach model name.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Get the text content, if this is a text response.
    pub fn as_text(&self) -> Option<&str> {
        match &self.content {
            LlmResponseContent::Text(s) => Some(s),
            _ => None,
        }
    }

    /// Get tool calls, if this is a tool call response.
    pub fn as_tool_calls(&self) -> Option<&[ToolCall]> {
        match &self.content {
            LlmResponseContent::ToolCalls(calls) => Some(calls),
            _ => None,
        }
    }
}

/// The content of an LLM response.
#[derive(Clone, Debug)]
pub enum LlmResponseContent {
    /// Pure text response (agent is done or providing info).
    Text(String),
    /// The LLM wants to call one or more tools.
    ToolCalls(Vec<ToolCall>),
}

/// Configuration for an LLM call.
#[derive(Clone, Debug)]
pub struct LlmRequest {
    pub messages: Vec<Message>,
    pub tools: Option<Value>,
    pub model: Option<String>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u64>,
    /// Structured output schema. When set, the LLM is constrained to return
    /// valid JSON matching this schema.
    pub response_format: Option<ResponseFormat>,
}

/// Structured output format for LLM responses.
#[derive(Clone, Debug)]
pub enum ResponseFormat {
    /// Free-form text (default).
    Text,
    /// JSON output (any valid JSON).
    Json,
    /// JSON output constrained to a specific schema.
    JsonSchema {
        name: String,
        schema: Value,
    },
}

/// A chunk of a streaming LLM response.
#[derive(Clone, Debug)]
pub enum StreamChunk {
    /// A text delta (partial token).
    TextDelta(String),
    /// The complete response (final chunk).
    Done(LlmResponse),
}

/// Trait for LLM providers. Implement this for OpenAI, Anthropic, local models, etc.
///
/// Implement `chat()` for the basic blocking API. Override `chat_stream()` for
/// token-by-token streaming — the default implementation collects the full
/// response via `chat()` and emits it as a single chunk.
pub trait LlmClient: Send + Sync {
    /// Send a chat completion request and return the complete response.
    fn chat(&self, request: &LlmRequest) -> DurableResult<LlmResponse>;

    /// Stream a chat completion response token-by-token.
    ///
    /// The callback receives each chunk as it arrives. The final chunk is
    /// `StreamChunk::Done(full_response)`. Default implementation calls
    /// `chat()` and emits the result as a single `Done` chunk.
    fn chat_stream(
        &self,
        request: &LlmRequest,
        on_chunk: &dyn Fn(StreamChunk),
    ) -> DurableResult<LlmResponse> {
        let response = self.chat(request)?;
        // Emit text as a delta before the Done signal
        if let Some(text) = response.as_text() {
            on_chunk(StreamChunk::TextDelta(text.to_string()));
        }
        on_chunk(StreamChunk::Done(response.clone()));
        Ok(response)
    }
}

/// A mock LLM client for testing. Returns scripted responses.
pub struct MockLlmClient {
    responses: std::sync::Mutex<Vec<LlmResponse>>,
}

impl MockLlmClient {
    pub fn new(responses: Vec<LlmResponse>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses),
        }
    }
}

impl LlmClient for MockLlmClient {
    fn chat(&self, _request: &LlmRequest) -> DurableResult<LlmResponse> {
        let mut responses = self.responses.lock().unwrap();
        if responses.is_empty() {
            Ok(LlmResponse::text(
                "No more scripted responses".to_string(),
            ))
        } else {
            Ok(responses.remove(0))
        }
    }
}

/// An LLM client that calls an external process (language-agnostic).
/// The process receives the full request as JSON on stdin and returns a response on stdout.
/// An LLM client that calls an external process (language-agnostic).
/// The process receives the full request as JSON on stdin and returns a response on stdout.
pub struct ProcessLlmClient {
    pub command: String,
    pub args: Vec<String>,
    /// Timeout in seconds (default 120).
    pub timeout_secs: u64,
}

impl ProcessLlmClient {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            args: Vec::new(),
            timeout_secs: 120,
        }
    }

    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }
}

impl LlmClient for ProcessLlmClient {
    fn chat(&self, request: &LlmRequest) -> DurableResult<LlmResponse> {
        use crate::core::process::run_with_kill_timeout;
        use std::process::{Command, Stdio};
        use std::time::Duration;

        let request_json = json::json_object(vec![
            (
                "messages",
                json::json_array(request.messages.iter().map(|m| m.to_json()).collect()),
            ),
            (
                "tools",
                request.tools.clone().unwrap_or(Value::Null),
            ),
            (
                "model",
                request
                    .model
                    .as_ref()
                    .map(|m| json::json_str(m))
                    .unwrap_or(Value::Null),
            ),
        ]);

        let child = Command::new(&self.command)
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| DurableError::LlmError {
                message: format!("failed to spawn LLM process: {}", e),
                retryable: match e.kind() {
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied => false,
                    _ => true,
                },
            })?;

        let input_bytes = json::to_string(&request_json);
        let timeout = Duration::from_secs(self.timeout_secs);

        let output = run_with_kill_timeout(child, Some(input_bytes.as_bytes()), timeout)
            .map_err(|e| DurableError::LlmError {
                message: e.to_string(),
                retryable: true,
            })?;

        if !output.status.success() {
            return Err(DurableError::LlmError {
                message: format!(
                    "LLM process exited with {}: {}",
                    output.status,
                    &output.stderr[..output.stderr.len().min(1000)]
                ),
                retryable: true,
            });
        }

        let val = json::parse(&output.stdout).map_err(|e| DurableError::LlmError {
            message: format!("invalid JSON from LLM process: {}", e),
            retryable: false,
        })?;

        // Parse response: look for tool_calls or text content
        if let Some(tool_calls) = val.get("tool_calls") {
            let calls: Vec<ToolCall> = tool_calls
                .as_array()
                .unwrap_or(&[])
                .iter()
                .filter_map(|c| ToolCall::from_json(c).ok())
                .collect();
            if !calls.is_empty() {
                return Ok(LlmResponse::tool_calls(calls));
            }
        }

        let text = val
            .get("content")
            .or_else(|| val.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        Ok(LlmResponse::text(text))
    }
}
