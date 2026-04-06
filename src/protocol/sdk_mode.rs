//! SDK mode — the Rust runtime as a managed subprocess.
//!
//! In SDK mode, the binary reads commands from stdin and writes events/callbacks
//! to stdout. Tools and LLM calls are executed by the parent SDK process via
//! protocol callbacks. All durable state (events, memoization, crash recovery)
//! stays in the Rust binary.
//!
//! Multiplexed: one process handles N agents. Each `create_agent` registers an
//! agent in the registry. `run_agent` spawns a thread — the command loop never
//! blocks. Suspended agents cost zero threads; a background event loop watches
//! for signals and auto-resumes.

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
        let env = Envelope {
            version: crate::protocol::PROTOCOL_VERSION.to_string(),
            id: Uuid::new_v4().to_hyphenated(),
            timestamp: crate::core::time::now_millis(),
            payload: ProtocolMessage::TextResponse {
                content: String::new(),
            },
        };
        let mut obj = payload_val
            .as_object()
            .cloned()
            .unwrap_or_else(BTreeMap::new);
        obj.insert("v".to_string(), json::json_str(&env.version));
        obj.insert("id".to_string(), json::json_str(&env.id));
        obj.insert("ts".to_string(), json::json_num(env.timestamp as f64));
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
    waiters: Mutex<BTreeMap<String, std::sync::mpsc::Sender<Value>>>,
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

        let reader_clone = reader.clone();
        let handle = std::thread::spawn(move || {
            let stdin = io::stdin();
            let mut buf_reader = stdin.lock();
            let mut line = String::new();

            loop {
                line.clear();
                match buf_reader.read_line(&mut line) {
                    Ok(0) => break,
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

    fn dispatch(&self, val: Value) {
        let msg_type = val
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("");

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

        let _ = self.command_tx.send(val);
    }

    fn wait_for_response(&self, correlation_id: &str) -> Result<Value, String> {
        let (tx, rx) = std::sync::mpsc::channel();

        {
            let mut waiters = self.waiters.lock().unwrap_or_else(|e| e.into_inner());
            waiters.insert(correlation_id.to_string(), tx);
        }

        rx.recv_timeout(std::time::Duration::from_secs(300))
            .map_err(|e| format!("callback timeout for {}: {}", correlation_id, e))
    }

    fn recv_command(&self) -> Option<Value> {
        let rx = self.command_rx.lock().unwrap_or_else(|e| e.into_inner());
        rx.recv().ok()
    }
}

// ---------------------------------------------------------------------------
// SdkLlmClient — implements LlmClient by calling back to the SDK
// ---------------------------------------------------------------------------

pub(crate) struct SdkLlmClient {
    writer: Arc<StdoutWriter>,
    reader: Arc<StdinReader>,
    agent_id: String,
}

impl SdkLlmClient {
    pub(crate) fn new(writer: Arc<StdoutWriter>, reader: Arc<StdinReader>, agent_id: &str) -> Self {
        Self { writer, reader, agent_id: agent_id.to_string() }
    }
}

impl LlmClient for SdkLlmClient {
    fn chat(&self, request: &LlmRequest) -> DurableResult<LlmResponse> {
        let correlation_id = Uuid::new_v4().to_hyphenated();

        self.writer.send_event(
            "chat_request",
            vec![
                ("callback_id", json::json_str(&correlation_id)),
                ("agent_id", json::json_str(&self.agent_id)),
                ("messages", json::json_array(request.messages.iter().map(|m| m.to_json()).collect())),
                ("tools", request.tools.clone().unwrap_or(Value::Null)),
                ("model", request.model.as_ref().map(|m| json::json_str(m)).unwrap_or(Value::Null)),
            ],
        );

        let response = self
            .reader
            .wait_for_response(&correlation_id)
            .map_err(|e| DurableError::LlmError {
                message: format!("SDK chat callback failed: {}", e),
                retryable: true,
            })?;

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
// SdkToolRegistry — implements ToolHandler by calling back to the SDK
// ---------------------------------------------------------------------------

pub(crate) struct SdkToolRegistry {
    definitions: Vec<ToolDefinition>,
    writer: Arc<StdoutWriter>,
    reader: Arc<StdinReader>,
    agent_id: String,
}

impl SdkToolRegistry {
    pub(crate) fn new(writer: Arc<StdoutWriter>, reader: Arc<StdinReader>, agent_id: &str) -> Self {
        Self {
            definitions: Vec::new(),
            writer,
            reader,
            agent_id: agent_id.to_string(),
        }
    }

    pub(crate) fn register_definition(&mut self, def: ToolDefinition) {
        self.definitions.push(def);
    }

    pub(crate) fn into_registry(self) -> ToolRegistry {
        let writer = self.writer.clone();
        let reader = self.reader.clone();
        let agent_id = self.agent_id.clone();
        let mut registry = ToolRegistry::new();

        for def in self.definitions {
            let w = writer.clone();
            let r = reader.clone();
            let tool_name = def.name.clone();
            let aid = agent_id.clone();

            registry.register(
                def,
                crate::tool::FnToolHandler::new(move |args: &Value| {
                    let correlation_id = Uuid::new_v4().to_hyphenated();

                    w.send_event(
                        "execute_tool",
                        vec![
                            ("callback_id", json::json_str(&correlation_id)),
                            ("agent_id", json::json_str(&aid)),
                            ("tool_name", json::json_str(&tool_name)),
                            ("arguments", args.clone()),
                        ],
                    );

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
// Outcome emission helper
// ---------------------------------------------------------------------------

fn emit_outcome(
    writer: &StdoutWriter,
    agent_id: &str,
    execution_id: &str,
    outcome: AgentOutcome,
) {
    match outcome {
        AgentOutcome::Complete { response } => {
            writer.send_event(
                "completed",
                vec![
                    ("agent_id", json::json_str(agent_id)),
                    ("execution_id", json::json_str(execution_id)),
                    ("response", json::json_str(&response)),
                ],
            );
        }
        AgentOutcome::Suspended { reason } => {
            writer.send_event(
                "suspended",
                vec![
                    ("agent_id", json::json_str(agent_id)),
                    ("execution_id", json::json_str(execution_id)),
                    ("reason", reason.to_json()),
                ],
            );
        }
        AgentOutcome::MaxIterations { last_response } => {
            writer.send_event(
                "completed",
                vec![
                    ("agent_id", json::json_str(agent_id)),
                    ("execution_id", json::json_str(execution_id)),
                    ("response", json::json_str(&last_response)),
                    ("max_iterations", json::json_bool(true)),
                ],
            );
        }
        AgentOutcome::Error { error } => {
            let retryable = crate::core::retry::Retryable::is_retryable(&error);
            writer.send_event(
                "error",
                vec![
                    ("agent_id", json::json_str(agent_id)),
                    ("execution_id", json::json_str(execution_id)),
                    ("message", json::json_str(&error.to_string())),
                    ("retryable", json::json_bool(retryable)),
                ],
            );
        }
    }
}

// ---------------------------------------------------------------------------
// SDK Mode event loop — multiplexed, non-blocking
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

    // Agent registry: agent_id -> AgentRuntime
    let registry: Arc<Mutex<BTreeMap<String, Arc<AgentRuntime>>>> =
        Arc::new(Mutex::new(BTreeMap::new()));

    // Execution-to-agent mapping for routing signals/resumes
    let exec_agent_map: Arc<Mutex<BTreeMap<String, String>>> =
        Arc::new(Mutex::new(BTreeMap::new()));

    // Running executions: exec_id -> JoinHandle (for drain on shutdown)
    let running: Arc<Mutex<BTreeMap<String, std::thread::JoinHandle<()>>>> =
        Arc::new(Mutex::new(BTreeMap::new()));

    // Shared cancellation token: cancel() triggers graceful shutdown of all agents
    let cancel_token = crate::core::cancel::CancellationToken::new();

    // Shutdown flag for the event loop thread
    let shutdown_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Event loop thread: watches for signals/timers and auto-resumes suspended agents.
    // Scans every 200ms. Exits when shutdown_flag is set.
    {
        let registry = registry.clone();
        let exec_agent_map = exec_agent_map.clone();
        let running = running.clone();
        let writer = writer.clone();
        let shutdown_flag = shutdown_flag.clone();

        std::thread::Builder::new()
            .name("durable-event-loop".to_string())
            .spawn(move || {
                while !shutdown_flag.load(std::sync::atomic::Ordering::SeqCst) {
                    std::thread::sleep(std::time::Duration::from_millis(200));

                    // Snapshot the registry to avoid holding the lock during I/O
                    let agents: Vec<(String, Arc<AgentRuntime>)> = {
                        let reg = registry.lock().unwrap_or_else(|e| e.into_inner());
                        reg.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
                    };

                    for (agent_id, rt) in &agents {
                        // List suspended executions for this agent
                        let suspended = match rt.storage().list_executions(
                            Some(crate::core::types::ExecutionStatus::Suspended),
                        ) {
                            Ok(execs) => execs,
                            Err(_) => continue,
                        };

                        for meta in &suspended {
                            let exec_id_str = meta.id.to_string();

                            // Skip if already running
                            if running.lock().unwrap_or_else(|e| e.into_inner()).contains_key(&exec_id_str) {
                                continue;
                            }

                            // Check if the suspend condition is satisfied
                            let should_resume = match &meta.suspend_reason {
                                Some(crate::core::error::SuspendReason::WaitingForConfirmation {
                                    confirmation_id, ..
                                }) => {
                                    rt.storage()
                                        .peek_signal(meta.id, confirmation_id)
                                        .ok()
                                        .flatten()
                                        .is_some()
                                }
                                Some(crate::core::error::SuspendReason::WaitingForSignal {
                                    signal_name,
                                }) => {
                                    rt.storage()
                                        .peek_signal(meta.id, signal_name)
                                        .ok()
                                        .flatten()
                                        .is_some()
                                }
                                Some(crate::core::error::SuspendReason::WaitingForTimer {
                                    fire_at_millis, ..
                                }) => {
                                    crate::core::time::now_millis() >= *fire_at_millis
                                }
                                _ => false,
                            };

                            if should_resume {
                                // Check if already running
                                {
                                    let r = running.lock().unwrap_or_else(|e| e.into_inner());
                                    if r.contains_key(&exec_id_str) {
                                        continue;
                                    }
                                }

                                // Track exec -> agent mapping
                                exec_agent_map.lock().unwrap_or_else(|e| e.into_inner())
                                    .insert(exec_id_str.clone(), agent_id.clone());

                                let rt = rt.clone();
                                let w = writer.clone();
                                let aid = agent_id.clone();
                                let rid = exec_id_str.clone();
                                let exec_id = meta.id;
                                let running_ref = running.clone();

                                if let Ok(handle) = std::thread::Builder::new()
                                    .name(format!("auto-resume-{}", &rid[..8]))
                                    .spawn(move || {
                                        let outcome = rt.resume(exec_id);
                                        emit_outcome(&w, &aid, &rid, outcome);
                                        running_ref.lock().unwrap_or_else(|e| e.into_inner())
                                            .remove(&rid);
                                    })
                                {
                                    running.lock().unwrap_or_else(|e| e.into_inner())
                                        .insert(exec_id_str, handle);
                                }
                            }
                        }
                    }
                }
            })
            .expect("failed to spawn event loop thread");
    }

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
                let agent_id = val
                    .get("agent_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("default")
                    .to_string();

                let dir = val
                    .get("data_dir")
                    .and_then(|v| v.as_str())
                    .unwrap_or("./data")
                    .to_string();

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
                let mut sdk_tools = SdkToolRegistry::new(writer.clone(), reader.clone(), &agent_id);
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
                        if t.get("requires_confirmation").and_then(|v| v.as_bool()).unwrap_or(false) {
                            def = def.with_confirmation();
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

                // Create LLM client with agent_id for callback routing
                let llm = Arc::new(SdkLlmClient::new(writer.clone(), reader.clone(), &agent_id));
                let tools = Arc::new(sdk_tools.into_registry());

                let mut rt = AgentRuntime::with_event_store(
                    config, storage, event_store, llm, tools,
                );

                // Wire up contracts with agent_id in callbacks
                if let Some(contracts_arr) = val.get("contracts").and_then(|v| v.as_array()) {
                    let mut contracts = Vec::new();
                    for c in contracts_arr {
                        if let Some(name) = c.as_str() {
                            let w = writer.clone();
                            let r = reader.clone();
                            let contract_name = name.to_string();
                            let aid = agent_id.clone();
                            contracts.push(crate::agent::contract::Contract {
                                name: contract_name.clone(),
                                check: Arc::new(move |step_name: &str, args: &Value| {
                                    let corr_id = Uuid::new_v4().to_hyphenated();
                                    w.send_event(
                                        "check_contract",
                                        vec![
                                            ("callback_id", json::json_str(&corr_id)),
                                            ("agent_id", json::json_str(&aid)),
                                            ("contract_name", json::json_str(&contract_name)),
                                            ("step_name", json::json_str(step_name)),
                                            ("arguments", args.clone()),
                                        ],
                                    );
                                    let response = r.wait_for_response(&corr_id)
                                        .map_err(|e| format!("contract callback failed: {}", e))?;
                                    let passed = response
                                        .get("passed")
                                        .and_then(|v| v.as_bool())
                                        .unwrap_or(true);
                                    if passed {
                                        Ok(())
                                    } else {
                                        let reason = response
                                            .get("reason")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("contract violated")
                                            .to_string();
                                        Err(reason)
                                    }
                                }),
                            });
                        }
                    }
                    if !contracts.is_empty() {
                        rt.set_contracts(contracts);
                    }
                }

                // Share the process-wide cancellation token for graceful shutdown
                rt.set_cancel_token(cancel_token.clone());
                rt.set_agent_id(agent_id.clone());

                registry.lock().unwrap_or_else(|e| e.into_inner())
                    .insert(agent_id.clone(), Arc::new(rt));

                writer.send_event(
                    "agent_created",
                    vec![
                        ("agent_id", json::json_str(&agent_id)),
                        ("data_dir", json::json_str(&dir)),
                    ],
                );
            }

            "run_agent" => {
                // Resolve agent: explicit agent_id, or infer from single-agent registry
                let agent_id = val.get("agent_id").and_then(|v| v.as_str())
                    .map(String::from)
                    .unwrap_or_else(|| {
                        let reg = registry.lock().unwrap_or_else(|e| e.into_inner());
                        if reg.len() == 1 {
                            reg.keys().next().unwrap().clone()
                        } else {
                            "default".to_string()
                        }
                    });

                let rt = match registry.lock().unwrap_or_else(|e| e.into_inner()).get(&agent_id).cloned() {
                    Some(r) => r,
                    None => {
                        writer.send_event(
                            "error",
                            vec![
                                ("agent_id", json::json_str(&agent_id)),
                                ("message", json::json_str(&format!("no agent with id '{}'", agent_id))),
                                ("retryable", json::json_bool(false)),
                            ],
                        );
                        continue;
                    }
                };

                let input = val
                    .get("input")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                let exec_id = val
                    .get("execution_id")
                    .and_then(|v| v.as_str())
                    .and_then(|s| {
                        crate::core::uuid::Uuid::parse(s)
                            .ok()
                            .map(ExecutionId::from_uuid)
                    });

                let run_id = exec_id.unwrap_or_else(ExecutionId::new);
                let run_id_str = run_id.to_string();

                // Check if execution already exists — if so, resume for memoized replay
                let already_exists = exec_id.is_some()
                    && rt.event_store().events(run_id).map(|e| !e.is_empty()).unwrap_or(false);

                // Track execution -> agent mapping
                exec_agent_map.lock().unwrap_or_else(|e| e.into_inner())
                    .insert(run_id_str.clone(), agent_id.clone());

                // Check if already running
                {
                    let r = running.lock().unwrap_or_else(|e| e.into_inner());
                    if r.contains_key(&run_id_str) {
                        writer.send_event(
                            "error",
                            vec![
                                ("agent_id", json::json_str(&agent_id)),
                                ("execution_id", json::json_str(&run_id_str)),
                                ("message", json::json_str("execution already running")),
                                ("retryable", json::json_bool(false)),
                            ],
                        );
                        continue;
                    }
                }

                // Spawn execution thread — command loop is free immediately
                let w = writer.clone();
                let aid = agent_id.clone();
                let rid = run_id_str.clone();
                let running_ref = running.clone();

                match std::thread::Builder::new()
                    .name(format!("agent-{}-{}", agent_id, &run_id_str[..8]))
                    .spawn(move || {
                        let outcome = if already_exists {
                            rt.resume(run_id)
                        } else {
                            rt.start_with_id(run_id, &input)
                        };
                        emit_outcome(&w, &aid, &rid, outcome);
                        running_ref.lock().unwrap_or_else(|e| e.into_inner()).remove(&rid);
                    })
                {
                    Ok(handle) => {
                        running.lock().unwrap_or_else(|e| e.into_inner())
                            .insert(run_id_str, handle);
                    }
                    Err(e) => {
                        eprintln!("[sdk-mode] failed to spawn agent thread: {}", e);
                        writer.send_event(
                            "error",
                            vec![
                                ("agent_id", json::json_str(&agent_id)),
                                ("execution_id", json::json_str(&run_id_str)),
                                ("message", json::json_str(&format!("thread spawn failed: {}", e))),
                                ("retryable", json::json_bool(true)),
                            ],
                        );
                    }
                }
            }

            "resume_agent" => {
                let exec_id_str = val
                    .get("execution_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                let exec_id = match crate::core::uuid::Uuid::parse(&exec_id_str) {
                    Ok(uuid) => ExecutionId::from_uuid(uuid),
                    Err(e) => {
                        writer.send_event(
                            "error",
                            vec![("message", json::json_str(&format!("bad execution_id: {}", e)))],
                        );
                        continue;
                    }
                };

                // Resolve agent: explicit agent_id, or look up from exec_agent_map, or infer
                let agent_id = val.get("agent_id").and_then(|v| v.as_str())
                    .map(String::from)
                    .or_else(|| {
                        exec_agent_map.lock().unwrap_or_else(|e| e.into_inner())
                            .get(&exec_id_str).cloned()
                    })
                    .unwrap_or_else(|| {
                        let reg = registry.lock().unwrap_or_else(|e| e.into_inner());
                        if reg.len() == 1 {
                            reg.keys().next().unwrap().clone()
                        } else {
                            "default".to_string()
                        }
                    });

                let rt = match registry.lock().unwrap_or_else(|e| e.into_inner()).get(&agent_id).cloned() {
                    Some(r) => r,
                    None => {
                        writer.send_event(
                            "error",
                            vec![
                                ("agent_id", json::json_str(&agent_id)),
                                ("message", json::json_str(&format!("no agent with id '{}'", agent_id))),
                            ],
                        );
                        continue;
                    }
                };

                // Check if already running
                {
                    let r = running.lock().unwrap_or_else(|e| e.into_inner());
                    if r.contains_key(&exec_id_str) {
                        writer.send_event(
                            "error",
                            vec![
                                ("agent_id", json::json_str(&agent_id)),
                                ("execution_id", json::json_str(&exec_id_str)),
                                ("message", json::json_str("execution already running")),
                                ("retryable", json::json_bool(false)),
                            ],
                        );
                        continue;
                    }
                }

                let w = writer.clone();
                let aid = agent_id.clone();
                let rid = exec_id_str.clone();
                let running_ref = running.clone();

                match std::thread::Builder::new()
                    .name(format!("resume-{}", &exec_id_str[..8]))
                    .spawn(move || {
                        let outcome = rt.resume(exec_id);
                        emit_outcome(&w, &aid, &rid, outcome);
                        running_ref.lock().unwrap_or_else(|e| e.into_inner()).remove(&rid);
                    })
                {
                    Ok(handle) => {
                        running.lock().unwrap_or_else(|e| e.into_inner())
                            .insert(exec_id_str, handle);
                    }
                    Err(e) => {
                        eprintln!("[sdk-mode] failed to spawn resume thread: {}", e);
                    }
                }
            }

            "signal" => {
                let exec_id_str = val.get("execution_id").and_then(|v| v.as_str()).unwrap_or("");

                // Resolve agent from exec_agent_map or infer
                let agent_id = val.get("agent_id").and_then(|v| v.as_str())
                    .map(String::from)
                    .or_else(|| {
                        exec_agent_map.lock().unwrap_or_else(|e| e.into_inner())
                            .get(exec_id_str).cloned()
                    })
                    .unwrap_or_else(|| {
                        let reg = registry.lock().unwrap_or_else(|e| e.into_inner());
                        if reg.len() == 1 {
                            reg.keys().next().unwrap().clone()
                        } else {
                            "default".to_string()
                        }
                    });

                let rt = match registry.lock().unwrap_or_else(|e| e.into_inner()).get(&agent_id).cloned() {
                    Some(r) => r,
                    None => continue,
                };

                if let Ok(uuid) = crate::core::uuid::Uuid::parse(exec_id_str) {
                    let exec_id = ExecutionId::from_uuid(uuid);
                    let signal_name = val.get("signal_name").and_then(|v| v.as_str()).unwrap_or("");
                    let data = val.get("data").cloned().unwrap_or(Value::Null);
                    let _ = rt.signal(exec_id, signal_name, data);
                }
            }

            "shutdown" => {
                // Graceful shutdown: cancel all agents, drain threads, ack
                // 1. Stop the event loop
                shutdown_flag.store(true, std::sync::atomic::Ordering::SeqCst);

                // 2. Cancel all active agents (they'll suspend at next step boundary)
                cancel_token.cancel();

                // 3. Drain active threads (30s timeout)
                let handles: BTreeMap<String, std::thread::JoinHandle<()>> = {
                    let mut r = running.lock().unwrap_or_else(|e| e.into_inner());
                    std::mem::take(&mut *r)
                };

                let drain_deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                for (exec_id, handle) in handles.into_iter() {
                    let remaining = drain_deadline.saturating_duration_since(std::time::Instant::now());
                    if remaining.is_zero() {
                        eprintln!("[sdk-mode] drain timeout: execution {} still running", exec_id);
                        break;
                    }
                    // Can't join with timeout in std, so we just join and hope
                    // the CancellationToken causes a timely exit
                    let _ = handle.join();
                }

                writer.send_event("shutdown_ack", vec![]);
                break;
            }

            other => {
                eprintln!("[sdk-mode] unknown command: {}", other);
            }
        }
    }
}
