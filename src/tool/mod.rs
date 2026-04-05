//! Tool abstraction for AI agent tool use.
//!
//! Tools are the agent's interface to the outside world. This module provides:
//! - [`ToolDefinition`]: metadata the LLM sees (name, description, parameter schema)
//! - [`ToolHandler`]: trait for executing a tool
//! - [`ToolRegistry`]: maps tool names to definitions + handlers
//! - [`ProcessToolHandler`]: runs tools as external processes (language-agnostic)

use crate::core::error::{DurableError, DurableResult};
use crate::json::{self, Value};
use std::collections::HashMap;
use std::process::{Command, Stdio};

/// Tool definition — the metadata an LLM needs for function calling.
#[derive(Clone, Debug)]
pub struct ToolDefinition {
    /// Unique tool name.
    pub name: String,
    /// Human-readable description of what the tool does.
    pub description: String,
    /// JSON Schema for the tool's parameters.
    pub parameters: Value,
    /// Whether this tool requires human confirmation before execution.
    pub requires_confirmation: bool,
}

impl ToolDefinition {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters: json::json_object(vec![
                ("type", json::json_str("object")),
                ("properties", json::json_object(vec![])),
            ]),
            requires_confirmation: false,
        }
    }

    pub fn with_parameters(mut self, params: Value) -> Self {
        self.parameters = params;
        self
    }

    pub fn with_confirmation(mut self) -> Self {
        self.requires_confirmation = true;
        self
    }

    /// Convert to the JSON format expected by LLM function-calling APIs.
    pub fn to_function_json(&self) -> Value {
        json::json_object(vec![
            ("type", json::json_str("function")),
            (
                "function",
                json::json_object(vec![
                    ("name", json::json_str(&self.name)),
                    ("description", json::json_str(&self.description)),
                    ("parameters", self.parameters.clone()),
                ]),
            ),
        ])
    }
}

impl json::ToJson for ToolDefinition {
    fn to_json(&self) -> Value {
        json::json_object(vec![
            ("name", json::json_str(&self.name)),
            ("description", json::json_str(&self.description)),
            ("parameters", self.parameters.clone()),
            ("requires_confirmation", json::json_bool(self.requires_confirmation)),
        ])
    }
}

/// A tool call request from the LLM.
#[derive(Clone, Debug)]
pub struct ToolCall {
    /// The call ID (for correlating with responses).
    pub id: String,
    /// Tool name.
    pub name: String,
    /// Arguments as JSON.
    pub arguments: Value,
}

impl json::ToJson for ToolCall {
    fn to_json(&self) -> Value {
        json::json_object(vec![
            ("id", json::json_str(&self.id)),
            ("name", json::json_str(&self.name)),
            ("arguments", self.arguments.clone()),
        ])
    }
}

impl json::FromJson for ToolCall {
    fn from_json(val: &Value) -> Result<Self, String> {
        Ok(ToolCall {
            id: val
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            name: val
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or("missing tool name")?
                .to_string(),
            arguments: val
                .get("arguments")
                .cloned()
                .unwrap_or(Value::Object(std::collections::BTreeMap::new())),
        })
    }
}

/// Result of a tool execution.
#[derive(Clone, Debug)]
pub struct ToolResult {
    pub call_id: String,
    pub output: Value,
    pub is_error: bool,
}

impl json::ToJson for ToolResult {
    fn to_json(&self) -> Value {
        json::json_object(vec![
            ("call_id", json::json_str(&self.call_id)),
            ("output", self.output.clone()),
            ("is_error", json::json_bool(self.is_error)),
        ])
    }
}

/// Trait for tool execution handlers.
pub trait ToolHandler: Send + Sync {
    /// Execute the tool with the given arguments. Returns the result as JSON.
    fn execute(&self, arguments: &Value) -> DurableResult<Value>;
}

/// A tool handler implemented as a Rust closure.
pub struct FnToolHandler {
    handler: Box<dyn Fn(&Value) -> DurableResult<Value> + Send + Sync>,
}

impl FnToolHandler {
    pub fn new<F>(f: F) -> Self
    where
        F: Fn(&Value) -> DurableResult<Value> + Send + Sync + 'static,
    {
        Self {
            handler: Box::new(f),
        }
    }
}

impl ToolHandler for FnToolHandler {
    fn execute(&self, arguments: &Value) -> DurableResult<Value> {
        (self.handler)(arguments)
    }
}

/// A tool handler that runs an external process (language-agnostic).
/// Sends JSON arguments on stdin, reads JSON result from stdout.
pub struct ProcessToolHandler {
    /// Path to the executable.
    pub command: String,
    /// Additional arguments to pass to the command.
    pub args: Vec<String>,
    /// Working directory.
    pub cwd: Option<String>,
    /// Environment variables.
    pub env: HashMap<String, String>,
    /// Timeout in seconds.
    pub timeout_secs: u64,
}

impl ProcessToolHandler {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            args: Vec::new(),
            cwd: None,
            env: HashMap::new(),
            timeout_secs: 30,
        }
    }

    pub fn with_args(mut self, args: Vec<String>) -> Self {
        self.args = args;
        self
    }

    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }
}

impl ToolHandler for ProcessToolHandler {
    fn execute(&self, arguments: &Value) -> DurableResult<Value> {
        use crate::core::process::run_with_kill_timeout;
        use std::time::Duration;

        let mut cmd = Command::new(&self.command);
        cmd.args(&self.args);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        if let Some(ref cwd) = self.cwd {
            cmd.current_dir(cwd);
        }
        for (k, v) in &self.env {
            cmd.env(k, v);
        }

        let child = cmd.spawn().map_err(|e| DurableError::ToolError {
            tool_name: self.command.clone(),
            message: format!("failed to spawn process: {}", e),
            retryable: false,
        })?;

        let input = json::to_string(arguments);
        let timeout = Duration::from_secs(self.timeout_secs);

        let output = run_with_kill_timeout(child, Some(input.as_bytes()), timeout)
            .map_err(|e| DurableError::ToolError {
                tool_name: self.command.clone(),
                message: e.to_string(),
                retryable: true,
            })?;

        if !output.status.success() {
            return Err(DurableError::ToolError {
                tool_name: self.command.clone(),
                message: format!(
                    "process exited with {}: {}",
                    output.status,
                    &output.stderr[..output.stderr.len().min(1000)]
                ),
                retryable: false,
            });
        }

        let result = json::parse(&output.stdout).map_err(|e| DurableError::ToolError {
            tool_name: self.command.clone(),
            message: format!("invalid JSON output: {}", e),
            retryable: false,
        })?;

        Ok(result)
    }
}

/// Registry of tools — maps names to definitions and handlers.
pub struct ToolRegistry {
    definitions: Vec<ToolDefinition>,
    handlers: HashMap<String, Box<dyn ToolHandler>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            definitions: Vec::new(),
            handlers: HashMap::new(),
        }
    }

    /// Register a tool with its definition and handler.
    pub fn register(&mut self, def: ToolDefinition, handler: impl ToolHandler + 'static) {
        self.handlers
            .insert(def.name.clone(), Box::new(handler));
        self.definitions.push(def);
    }

    /// Register a tool with a closure handler.
    pub fn register_fn<F>(
        &mut self,
        def: ToolDefinition,
        handler: F,
    ) where
        F: Fn(&Value) -> DurableResult<Value> + Send + Sync + 'static,
    {
        self.register(def, FnToolHandler::new(handler));
    }

    /// Get all tool definitions (for passing to the LLM).
    pub fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }

    /// Get a tool definition by name.
    pub fn get_definition(&self, name: &str) -> Option<&ToolDefinition> {
        self.definitions.iter().find(|d| d.name == name)
    }

    /// Execute a tool by name.
    ///
    /// Validates arguments against the tool's JSON Schema before execution.
    /// If the LLM sent invalid arguments (wrong type, missing required field),
    /// returns a retryable ToolError so the LLM can try again.
    pub fn execute(&self, name: &str, arguments: &Value) -> DurableResult<Value> {
        let handler = self.handlers.get(name).ok_or_else(|| DurableError::ToolError {
            tool_name: name.to_string(),
            message: "tool not found".to_string(),
            retryable: false,
        })?;

        // Validate arguments against schema before execution
        if let Some(def) = self.get_definition(name) {
            if let Err(errors) = validate_args(arguments, &def.parameters) {
                return Err(DurableError::ToolError {
                    tool_name: name.to_string(),
                    message: format!("invalid arguments: {}", errors.join("; ")),
                    retryable: true, // Let the LLM retry with corrected arguments
                });
            }
        }

        handler.execute(arguments)
    }

    /// Check if a tool requires confirmation.
    pub fn requires_confirmation(&self, name: &str) -> bool {
        self.definitions
            .iter()
            .find(|d| d.name == name)
            .map(|d| d.requires_confirmation)
            .unwrap_or(false)
    }

    /// Get tool definitions as JSON (for LLM function-calling).
    pub fn to_function_json(&self) -> Value {
        json::json_array(
            self.definitions
                .iter()
                .map(|d| d.to_function_json())
                .collect(),
        )
    }
}

/// Validate arguments against a JSON Schema.
///
/// Checks required fields and basic type matching. Returns a list of
/// validation errors, or Ok(()) if valid.
fn validate_args(args: &Value, schema: &Value) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();

    // Schema must be an object type
    let schema_type = schema.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if schema_type != "object" {
        return Ok(()); // Can't validate non-object schemas
    }

    // Args must be an object
    let obj = match args.as_object() {
        Some(o) => o,
        None => {
            errors.push("arguments must be a JSON object".to_string());
            return Err(errors);
        }
    };

    // Check required fields
    if let Some(required) = schema.get("required").and_then(|v| v.as_array()) {
        for req in required {
            if let Some(field_name) = req.as_str() {
                if !obj.contains_key(field_name) {
                    errors.push(format!("missing required field '{}'", field_name));
                }
            }
        }
    }

    // Type-check provided fields against schema properties
    if let Some(properties) = schema.get("properties").and_then(|v| v.as_object()) {
        for (key, prop_schema) in properties {
            if let Some(value) = obj.get(key) {
                if let Some(expected_type) = prop_schema.get("type").and_then(|v| v.as_str()) {
                    let actual_type = match value {
                        Value::String(_) => "string",
                        Value::Number(_) => {
                            // Numbers can satisfy both "number" and "integer"
                            if expected_type == "integer" {
                                if value.as_f64().map(|n| n.fract() == 0.0).unwrap_or(false) {
                                    "integer"
                                } else {
                                    "number"
                                }
                            } else {
                                "number"
                            }
                        }
                        Value::Bool(_) => "boolean",
                        Value::Array(_) => "array",
                        Value::Object(_) => "object",
                        Value::Null => "null",
                    };

                    let type_matches = actual_type == expected_type
                        || (expected_type == "number" && actual_type == "integer");

                    if !type_matches {
                        errors.push(format!(
                            "field '{}': expected type '{}', got '{}'",
                            key, expected_type, actual_type
                        ));
                    }
                }
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
