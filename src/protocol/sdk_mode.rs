//! SDK mode — the Rust runtime as a managed subprocess.
//!
//! In SDK mode, the binary reads commands from stdin and writes events/callbacks
//! to stdout. Tools and LLM calls are executed by the parent SDK process via
//! protocol callbacks. All durable state (events, memoization, crash recovery)
//! stays in the Rust binary.
//!
//! This enables first-class SDKs in any language: Python spawns this binary,
//! sends `create_agent`/`run_agent` commands, receives `completed`/`suspended`
//! events, and handles `execute_tool`/`chat_request` callbacks.

use crate::agent::llm::{LlmClient, LlmRequest, LlmResponse};
use crate::agent::runtime::{AgentConfig, AgentOutcome, AgentRuntime};
use crate::core::error::{DurableError, DurableResult};
use crate::core::types::ExecutionId;
use crate::core::uuid::Uuid;
use crate::json::{self, FromJson, ToJson, Value};
use crate::protocol::{Envelope, ProtocolMessage};
use crate::storage::{FileEventStore, FileStorage, InMemoryEventStore, InMemoryStorage};
use crate::tool::{ToolCall, ToolDefinition, ToolRegistry};
use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Stdio I/O helpers (thread-safe)
// ---------------------------------------------------------------------------

/// Thread-safe writer to stdout.
pub(crate) struct StdoutWriter {
    inner: Mutex<io::Stdout>,
}

impl StdoutWriter {
    fn new() -> Self {
        Self {
            inner: Mutex::new(io::stdout()),
        }
    }

    fn send_event(&self, msg_type: &str, fields: Vec<(&str, Value)>) {
        let mut entries = vec![("type", json::json_str(msg_type))];
        entries.extend(fields);
        let payload_val = json::json_object(entries);
        // Wrap in envelope
        let env = Envelope {
            version: crate::protocol::PROTOCOL_VERSION.to_string(),
            id: Uuid::new_v4().to_hyphenated(),
            timestamp: crate::core::time::now_millis(),
            payload: ProtocolMessage::TextResponse {
                content: String::new(),
            }, // placeholder
        };
        // Write the raw JSON (bypassing ProtocolMessage enum for new message types)
        let mut obj = payload_val
            .as_object()
            .cloned()
            .unwrap_or_else(BTreeMap::new);
        obj.insert(
            "v".to_string(),
            json::json_str(&env.version),
        );
        obj.insert("id".to_string(), json::json_str(&env.id));
        obj.insert(
            "ts".to_string(),
            json::json_num(env.timestamp as f64),
        );
        let line = format!("{}\n", json::to_string(&Value::Object(obj)));
        let mut out = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let _ = out.write_all(line.as_bytes());
        let _ = out.flush();
    }
}

/// Thread-safe reader for correlated responses from stdin.
///
/// A dedicated reader thread reads all stdin lines and routes them:
/// - Callback responses (tool_result, chat_response) → per-request channels
/// - Commands (run_agent, etc.) → command channel for the main loop
pub(crate) struct StdinReader {
    /// Per-request response channels keyed by correlation ID.
    waiters: Mutex<BTreeMap<String, std::sync::mpsc::Sender<Value>>>,
    /// Channel for commands (messages that aren't callback responses).
    command_tx: std::sync::mpsc::Sender<Value>,
    command_rx: Mutex<std::sync::mpsc::Receiver<Value>>,
}

impl StdinReader {
    fn new() -> (Arc<Self>, std::thread::JoinHandle<()>) {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        let reader = Arc::new(Self {
            waiters: Mutex::new(BTreeMap::new()),
            command_tx: cmd_tx,
            command_rx: Mutex::new(cmd_rx),
        });

        // Spawn dedicated stdin reader thread
        let reader_clone = reader.clone();
        let handle = std::thread::spawn(move || {
            let stdin = io::stdin();
            let mut buf_reader = stdin.lock();
            let mut line = String::new();

            loop {
                line.clear();
                match buf_reader.read_line(&mut line) {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        match json::parse(trimmed) {
                            Ok(val) => reader_clone.dispatch(val),
                            Err(e) => eprintln!("[sdk-mode] bad JSON from stdin: {}", e),
                        }
                    }
                    Err(e) => {
                        eprintln!("[sdk-mode] stdin error: {}", e);
                        break;
                    }
                }
            }
        });

        (reader, handle)
    }

    /// Route an incoming message to the right destination.
    fn dispatch(&self, val: Value) {
        let msg_type = val
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Callback responses go to their waiting thread
        let callback_id = val
            .get("callback_id")
            .or_else(|| val.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let is_callback_response = matches!(
            msg_type,
            "tool_result" | "chat_response" | "contract_result" | "hook_result"
        );

        if is_callback_response && !callback_id.is_empty() {
            let mut waiters = self.waiters.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(tx) = waiters.remove(&callback_id) {
                let _ = tx.send(val);
                return;
            }
        }

        // Everything else is a command
        let _ = self.command_tx.send(val);
    }

    /// Wait for a response with the given correlation ID.
    /// Non-blocking for other threads — only this thread blocks on its channel.
    fn wait_for_response(&self, correlation_id: &str) -> Result<Value, String> {
        let (tx, rx) = std::sync::mpsc::channel();

        {
            let mut waiters = self.waiters.lock().unwrap_or_else(|e| e.into_inner());
            waiters.insert(correlation_id.to_string(), tx);
        }

        // Wait with timeout (5 minutes for LLM calls)
        rx.recv_timeout(std::time::Duration::from_secs(300))
            .map_err(|e| format!("callback timeout for {}: {}", correlation_id, e))
    }

    /// Receive the next command from the command channel.
    fn recv_command(&self) -> Option<Value> {
        let rx = self.command_rx.lock().unwrap_or_else(|e| e.into_inner());
        rx.recv().ok()
    }
}

// ---------------------------------------------------------------------------
// SdkLlmClient — implements LlmClient by calling back to the SDK
// ---------------------------------------------------------------------------

/// LLM client that sends chat requests to the parent SDK via protocol.
pub(crate) struct SdkLlmClient {
    writer: Arc<StdoutWriter>,
    reader: Arc<StdinReader>,
}

impl SdkLlmClient {
    pub(crate) fn new(writer: Arc<StdoutWriter>, reader: Arc<StdinReader>) -> Self {
        Self { writer, reader }
    }
}

impl LlmClient for SdkLlmClient {
    fn chat(&self, request: &LlmRequest) -> DurableResult<LlmResponse> {
        let correlation_id = Uuid::new_v4().to_hyphenated();

        // Send chat_request callback to SDK
        self.writer.send_event(
            "chat_request",
            vec![
                ("callback_id", json::json_str(&correlation_id)),
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
            ],
        );

        // Wait for chat_response from SDK
        let response = self
            .reader
            .wait_for_response(&correlation_id)
            .map_err(|e| DurableError::LlmError {
                message: format!("SDK chat callback failed: {}", e),
                retryable: true,
            })?;

        // Parse response
        let resp_type = response
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("chat_response");

        if resp_type == "error" {
            let msg = response
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown LLM error");
            return Err(DurableError::LlmError {
                message: msg.to_string(),
                retryable: response
                    .get("retryable")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true),
            });
        }

        // Check for tool_calls
        if let Some(calls) = response.get("tool_calls") {
            if let Some(arr) = calls.as_array() {
                if !arr.is_empty() {
                    let tool_calls: Vec<ToolCall> = arr
                        .iter()
                        .filter_map(|c| ToolCall::from_json(c).ok())
                        .collect();
                    return Ok(LlmResponse::tool_calls(tool_calls));
                }
            }
        }

        // Text response
        let content = response
            .get("content")
            .or_else(|| response.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        Ok(LlmResponse::text(content))
    }
}

// ---------------------------------------------------------------------------
// SdkToolHandler — implements ToolHandler by calling back to the SDK
// ---------------------------------------------------------------------------

/// A tool registry that dispatches to the SDK via protocol callbacks.
pub(crate) struct SdkToolRegistry {
    definitions: Vec<ToolDefinition>,
    writer: Arc<StdoutWriter>,
    reader: Arc<StdinReader>,
}

impl SdkToolRegistry {
    pub(crate) fn new(writer: Arc<StdoutWriter>, reader: Arc<StdinReader>) -> Self {
        Self {
            definitions: Vec::new(),
            writer,
            reader,
        }
    }

    pub(crate) fn register_definition(&mut self, def: ToolDefinition) {
        self.definitions.push(def);
    }

    pub(crate) fn into_registry(self) -> ToolRegistry {
        let writer = self.writer.clone();
        let reader = self.reader.clone();
        let mut registry = ToolRegistry::new();

        for def in self.definitions {
            let w = writer.clone();
            let r = reader.clone();
            let tool_name = def.name.clone();

            registry.register(
                def,
                crate::tool::FnToolHandler::new(move |args: &Value| {
                    let correlation_id = Uuid::new_v4().to_hyphenated();

                    // Send execute_tool callback to SDK
                    w.send_event(
                        "execute_tool",
                        vec![
                            ("callback_id", json::json_str(&correlation_id)),
                            ("tool_name", json::json_str(&tool_name)),
                            ("arguments", args.clone()),
                        ],
                    );

                    // Wait for tool_result from SDK
                    let response = r.wait_for_response(&correlation_id).map_err(|e| {
                        DurableError::ToolError {
                            tool_name: tool_name.clone(),
                            message: format!("SDK tool callback failed: {}", e),
                            retryable: true,
                        }
                    })?;

                    let resp_type = response
                        .get("type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("tool_result");

                    if resp_type == "error" {
                        let msg = response
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("tool execution failed");
                        return Err(DurableError::ToolError {
                            tool_name: tool_name.clone(),
                            message: msg.to_string(),
                            retryable: response
                                .get("retryable")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(true),
                        });
                    }

                    Ok(response
                        .get("output")
                        .cloned()
                        .unwrap_or(Value::Null))
                }),
            );
        }

        registry
    }
}

// ---------------------------------------------------------------------------
// SDK Mode event loop
// ---------------------------------------------------------------------------

/// Run the runtime in SDK mode, reading commands from stdin.
pub fn run_sdk_mode() {
    run_sdk_mode_with_auth(None);
}

/// Run SDK mode with optional authentication.
pub fn run_sdk_mode_with_auth(auth_token: Option<&str>) {
    let writer = Arc::new(StdoutWriter::new());
    let (reader, _reader_handle) = StdinReader::new();

    let mut authenticated = auth_token.is_none();

    // Shared runtime — wrapped in Arc<Mutex> for concurrent command threads
    let runtime: Arc<Mutex<Option<Arc<AgentRuntime>>>> = Arc::new(Mutex::new(None));
    let mut _data_dir: Option<String> = None;

    while let Some(val) = reader.recv_command() {
        let msg_type = val
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // Auth check
        if !authenticated {
            if msg_type == "auth" {
                let token = val.get("token").and_then(|v| v.as_str()).unwrap_or("");
                if Some(token) == auth_token {
                    authenticated = true;
                    writer.send_event("auth_ok", vec![]);
                } else {
                    writer.send_event("error", vec![
                        ("message", json::json_str("invalid auth token")),
                        ("retryable", json::json_bool(false)),
                    ]);
                    break;
                }
            } else {
                writer.send_event("error", vec![
                    ("message", json::json_str("authentication required")),
                    ("retryable", json::json_bool(false)),
                ]);
            }
            continue;
        }

        match msg_type.as_str() {
            "create_agent" => {
                let dir = val
                    .get("data_dir")
                    .and_then(|v| v.as_str())
                    .unwrap_or("./data")
                    .to_string();
                _data_dir = Some(dir.clone());

                // Parse config
                let config_val = val.get("config").cloned().unwrap_or(Value::Null);
                let mut config = AgentConfig::default();
                if let Some(sp) = config_val.get("system_prompt").and_then(|v| v.as_str()) {
                    config.system_prompt = sp.to_string();
                }
                if let Some(m) = config_val.get("model").and_then(|v| v.as_str()) {
                    config.model = Some(m.to_string());
                }
                if let Some(n) = config_val.get("max_iterations").and_then(|v| v.as_u64()) {
                    config.max_iterations = n as u32;
                }

                // Parse tool definitions
                let mut sdk_tools = SdkToolRegistry::new(writer.clone(), reader.clone());
                if let Some(tools_arr) = val.get("tools").and_then(|v| v.as_array()) {
                    for t in tools_arr {
                        let name = t.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let desc = t
                            .get("description")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let params = t.get("parameters").cloned().unwrap_or(Value::Null);
                        let mut def = ToolDefinition::new(&name, &desc);
                        if params != Value::Null {
                            def = def.with_parameters(params);
                        }
                        sdk_tools.register_definition(def);
                    }
                }

                // Create storage
                let (storage, event_store): (
                    Arc<dyn crate::storage::ExecutionLog>,
                    Arc<dyn crate::storage::EventStore>,
                ) = match (
                    FileStorage::new(&dir),
                    FileEventStore::new(&dir),
                ) {
                    (Ok(s), Ok(e)) => (Arc::new(s), Arc::new(e)),
                    _ => (
                        Arc::new(InMemoryStorage::new()),
                        Arc::new(InMemoryEventStore::new()),
                    ),
                };

                // Create LLM client that callbacks to SDK
                let llm = Arc::new(SdkLlmClient::new(writer.clone(), reader.clone()));
                let tools = Arc::new(sdk_tools.into_registry());

                let rt = AgentRuntime::with_event_store(
                    config, storage, event_store, llm, tools,
                );
                *runtime.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(rt));

                writer.send_event(
                    "agent_created",
                    vec![("data_dir", json::json_str(&dir))],
                );
            }

            "run_agent" => {
                let rt = match runtime.lock().unwrap_or_else(|e| e.into_inner()).clone() {
                    Some(r) => r,
                    None => {
                        writer.send_event(
                            "error",
                            vec![
                                ("message", json::json_str("no agent created")),
                                ("retryable", json::json_bool(false)),
                            ],
                        );
                        continue;
                    }
                };

                let input = val
                    .get("input")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                let exec_id = val
                    .get("execution_id")
                    .and_then(|v| v.as_str())
                    .and_then(|s| {
                        crate::core::uuid::Uuid::parse(s)
                            .ok()
                            .map(ExecutionId::from_uuid)
                    });

                let outcome = match exec_id {
                    Some(id) => rt.start_with_id(id, input),
                    None => rt.start(input),
                };

                match outcome {
                    AgentOutcome::Complete { response } => {
                        writer.send_event(
                            "completed",
                            vec![("response", json::json_str(&response))],
                        );
                    }
                    AgentOutcome::Suspended { reason } => {
                        let reason_json = reason.to_json();
                        writer.send_event(
                            "suspended",
                            vec![("reason", reason_json)],
                        );
                    }
                    AgentOutcome::MaxIterations { last_response } => {
                        writer.send_event(
                            "completed",
                            vec![
                                ("response", json::json_str(&last_response)),
                                ("max_iterations", json::json_bool(true)),
                            ],
                        );
                    }
                    AgentOutcome::Error { error } => {
                        writer.send_event(
                            "error",
                            vec![
                                ("message", json::json_str(&error.to_string())),
                                ("retryable", json::json_bool(false)),
                            ],
                        );
                    }
                }
            }

            "resume_agent" => {
                let rt = match runtime.lock().unwrap_or_else(|e| e.into_inner()).clone() {
                    Some(r) => r,
                    None => {
                        writer.send_event(
                            "error",
                            vec![("message", json::json_str("no agent created"))],
                        );
                        continue;
                    }
                };

                let exec_id_str = val
                    .get("execution_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let exec_id = match crate::core::uuid::Uuid::parse(exec_id_str) {
                    Ok(uuid) => ExecutionId::from_uuid(uuid),
                    Err(e) => {
                        writer.send_event(
                            "error",
                            vec![("message", json::json_str(&format!("bad execution_id: {}", e)))],
                        );
                        continue;
                    }
                };

                let outcome = rt.resume(exec_id);
                match outcome {
                    AgentOutcome::Complete { response } => {
                        writer.send_event(
                            "completed",
                            vec![("response", json::json_str(&response))],
                        );
                    }
                    AgentOutcome::Suspended { reason } => {
                        writer.send_event(
                            "suspended",
                            vec![("reason", reason.to_json())],
                        );
                    }
                    AgentOutcome::Error { error } => {
                        writer.send_event(
                            "error",
                            vec![("message", json::json_str(&error.to_string()))],
                        );
                    }
                    AgentOutcome::MaxIterations { last_response } => {
                        writer.send_event(
                            "completed",
                            vec![("response", json::json_str(&last_response))],
                        );
                    }
                }
            }

            "signal" => {
                let rt = match runtime.lock().unwrap_or_else(|e| e.into_inner()).clone() {
                    Some(r) => r,
                    None => continue,
                };
                let exec_id_str = val.get("execution_id").and_then(|v| v.as_str()).unwrap_or("");
                if let Ok(uuid) = crate::core::uuid::Uuid::parse(exec_id_str) {
                    let exec_id = ExecutionId::from_uuid(uuid);
                    let signal_name = val.get("signal_name").and_then(|v| v.as_str()).unwrap_or("");
                    let data = val.get("data").cloned().unwrap_or(Value::Null);
                    let _ = rt.signal(exec_id, signal_name, data);
                }
            }

            "shutdown" => {
                writer.send_event("shutdown_ack", vec![]);
                break;
            }

            // Callback responses are handled by the reader thread —
            // they never reach the command channel.

            other => {
                eprintln!("[sdk-mode] unknown command: {}", other);
            }
        }
    }
}
