//! Language-agnostic JSON wire protocol.
//!
//! This module defines the protocol for communicating with agents and tools
//! written in any language. Communication happens over stdin/stdout with
//! newline-delimited JSON messages.
//!
//! ## Protocol Messages
//!
//! ### Runtime → Agent/Tool
//! - `{"type": "execute", "tool_name": "...", "arguments": {...}}`
//! - `{"type": "chat", "messages": [...], "tools": [...]}`
//!
//! ### Agent/Tool → Runtime
//! - `{"type": "result", "output": ...}`
//! - `{"type": "error", "message": "...", "retryable": bool}`
//! - `{"type": "tool_calls", "calls": [...]}`
//! - `{"type": "text", "content": "..."}`

use crate::core::time::now_millis;
use crate::core::uuid::Uuid;
use crate::json::{self, Value};

/// Protocol version.
pub const PROTOCOL_VERSION: &str = "1.0";

/// A protocol message.
#[derive(Clone, Debug)]
pub enum ProtocolMessage {
    // Runtime → Tool
    ExecuteTool {
        tool_name: String,
        arguments: Value,
    },

    // Runtime → LLM adapter
    ChatRequest {
        messages: Value,
        tools: Option<Value>,
        model: Option<String>,
    },

    // Tool → Runtime
    ToolResult {
        output: Value,
    },

    // Tool → Runtime
    ToolError {
        message: String,
        retryable: bool,
    },

    // LLM adapter → Runtime
    TextResponse {
        content: String,
    },

    // LLM adapter → Runtime
    ToolCallsResponse {
        calls: Value,
    },

    // Bidirectional keepalive
    Heartbeat {
        timestamp: u64,
    },

    HeartbeatAck {
        timestamp: u64,
    },
}

/// Protocol envelope — wraps every message with version, ID, and timestamp.
#[derive(Clone, Debug)]
pub struct Envelope {
    pub version: String,
    pub id: String,
    pub timestamp: u64,
    pub payload: ProtocolMessage,
}

impl Envelope {
    /// Create a new envelope for a message.
    pub fn wrap(payload: ProtocolMessage) -> Self {
        Self {
            version: PROTOCOL_VERSION.to_string(),
            id: Uuid::new_v4().to_hyphenated(),
            timestamp: now_millis(),
            payload,
        }
    }

    /// Create an envelope with a specific ID (for response correlation).
    pub fn reply(request_id: &str, payload: ProtocolMessage) -> Self {
        Self {
            version: PROTOCOL_VERSION.to_string(),
            id: request_id.to_string(),
            timestamp: now_millis(),
            payload,
        }
    }

    /// Serialize to JSON.
    pub fn to_json(&self) -> Value {
        let mut payload_obj = match self.payload.to_json().as_object().cloned() {
            Some(map) => map,
            None => std::collections::BTreeMap::new(),
        };
        payload_obj.insert("v".to_string(), json::json_str(&self.version));
        payload_obj.insert("id".to_string(), json::json_str(&self.id));
        payload_obj.insert("ts".to_string(), json::json_num(self.timestamp as f64));
        Value::Object(payload_obj)
    }

    /// Parse from JSON. Missing version assumes 1.0 for backward compat.
    pub fn from_json(val: &Value) -> Result<Self, String> {
        let version = val
            .get("v")
            .and_then(|v| v.as_str())
            .unwrap_or(PROTOCOL_VERSION)
            .to_string();
        let id = val
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let timestamp = val.get("ts").and_then(|v| v.as_u64()).unwrap_or(0);

        let payload = ProtocolMessage::from_json(val)?;
        Ok(Self {
            version,
            id,
            timestamp,
            payload,
        })
    }

    /// Serialize to a newline-delimited JSON string.
    pub fn to_line(&self) -> String {
        format!("{}\n", json::to_string(&self.to_json()))
    }
}

impl ProtocolMessage {
    /// Serialize to JSON.
    pub fn to_json(&self) -> Value {
        match self {
            ProtocolMessage::ExecuteTool {
                tool_name,
                arguments,
            } => json::json_object(vec![
                ("type", json::json_str("execute")),
                ("tool_name", json::json_str(tool_name)),
                ("arguments", arguments.clone()),
            ]),
            ProtocolMessage::ChatRequest {
                messages,
                tools,
                model,
            } => {
                let mut entries = vec![
                    ("type", json::json_str("chat")),
                    ("messages", messages.clone()),
                ];
                if let Some(tools) = tools {
                    entries.push(("tools", tools.clone()));
                }
                if let Some(model) = model {
                    entries.push(("model", json::json_str(model)));
                }
                json::json_object(entries)
            }
            ProtocolMessage::ToolResult { output } => json::json_object(vec![
                ("type", json::json_str("result")),
                ("output", output.clone()),
            ]),
            ProtocolMessage::ToolError {
                message,
                retryable,
            } => json::json_object(vec![
                ("type", json::json_str("error")),
                ("message", json::json_str(message)),
                ("retryable", json::json_bool(*retryable)),
            ]),
            ProtocolMessage::TextResponse { content } => json::json_object(vec![
                ("type", json::json_str("text")),
                ("content", json::json_str(content)),
            ]),
            ProtocolMessage::ToolCallsResponse { calls } => json::json_object(vec![
                ("type", json::json_str("tool_calls")),
                ("calls", calls.clone()),
            ]),
            ProtocolMessage::Heartbeat { timestamp } => json::json_object(vec![
                ("type", json::json_str("heartbeat")),
                ("timestamp", json::json_num(*timestamp as f64)),
            ]),
            ProtocolMessage::HeartbeatAck { timestamp } => json::json_object(vec![
                ("type", json::json_str("heartbeat_ack")),
                ("timestamp", json::json_num(*timestamp as f64)),
            ]),
        }
    }

    /// Parse from JSON.
    pub fn from_json(val: &Value) -> Result<Self, String> {
        let msg_type = val
            .get("type")
            .and_then(|v| v.as_str())
            .ok_or("missing 'type' field")?;

        match msg_type {
            "execute" => Ok(ProtocolMessage::ExecuteTool {
                tool_name: val
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .ok_or("missing tool_name")?
                    .to_string(),
                arguments: val.get("arguments").cloned().unwrap_or(Value::Null),
            }),
            "chat" => Ok(ProtocolMessage::ChatRequest {
                messages: val.get("messages").cloned().unwrap_or(Value::Null),
                tools: val.get("tools").cloned(),
                model: val.get("model").and_then(|v| v.as_str()).map(String::from),
            }),
            "result" => Ok(ProtocolMessage::ToolResult {
                output: val.get("output").cloned().unwrap_or(Value::Null),
            }),
            "error" => Ok(ProtocolMessage::ToolError {
                message: val
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error")
                    .to_string(),
                retryable: val
                    .get("retryable")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            }),
            "text" => Ok(ProtocolMessage::TextResponse {
                content: val
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            }),
            "tool_calls" => Ok(ProtocolMessage::ToolCallsResponse {
                calls: val.get("calls").cloned().unwrap_or(Value::Null),
            }),
            "heartbeat" => Ok(ProtocolMessage::Heartbeat {
                timestamp: val.get("timestamp").and_then(|v| v.as_u64()).unwrap_or(0),
            }),
            "heartbeat_ack" => Ok(ProtocolMessage::HeartbeatAck {
                timestamp: val.get("timestamp").and_then(|v| v.as_u64()).unwrap_or(0),
            }),
            other => Err(format!("unknown message type: {}", other)),
        }
    }

    /// Serialize to a newline-delimited JSON string.
    pub fn to_line(&self) -> String {
        format!("{}\n", json::to_string(&self.to_json()))
    }

    /// Parse from a line of JSON.
    pub fn from_line(line: &str) -> Result<Self, String> {
        let val = json::parse(line.trim()).map_err(|e| e.to_string())?;
        Self::from_json(&val)
    }
}

/// Read protocol messages from a reader (e.g., stdin).
pub fn read_messages(reader: &mut dyn std::io::BufRead) -> impl Iterator<Item = Result<ProtocolMessage, String>> + '_ {
    reader.lines().map(|line| {
        let line = line.map_err(|e| e.to_string())?;
        ProtocolMessage::from_line(&line)
    })
}

/// Write a protocol message to a writer (e.g., stdout).
pub fn write_message(
    writer: &mut dyn std::io::Write,
    msg: &ProtocolMessage,
) -> Result<(), String> {
    writer
        .write_all(msg.to_line().as_bytes())
        .map_err(|e| e.to_string())?;
    writer.flush().map_err(|e| e.to_string())?;
    Ok(())
}

pub mod sdk_mode;

use std::io::BufRead;
