//! Conversation history management.
//!
//! Conversation history is the deterministic accumulation of step results.
//! It is reconstructed from the execution log on replay, not stored separately.

use crate::agent::llm::Message;
use crate::json::{self, FromJson, ToJson, Value};

/// Manages the conversation history for an agent execution.
#[derive(Clone, Debug)]
pub struct Conversation {
    messages: Vec<Message>,
}

impl Conversation {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }

    /// Create a conversation with a system prompt.
    pub fn with_system_prompt(prompt: impl Into<String>) -> Self {
        Self {
            messages: vec![Message::system(prompt)],
        }
    }

    /// Add a message to the conversation.
    pub fn push(&mut self, message: Message) {
        self.messages.push(message);
    }

    /// Get all messages.
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Get the number of messages.
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Serialize the conversation to JSON for storage/transmission.
    pub fn to_json(&self) -> Value {
        json::json_array(self.messages.iter().map(|m| m.to_json()).collect())
    }

    /// Reconstruct a conversation from JSON.
    pub fn from_json(val: &Value) -> Result<Self, String> {
        let arr = val.as_array().ok_or("expected array")?;
        let messages: Vec<Message> = arr
            .iter()
            .map(Message::from_json)
            .collect::<Result<_, _>>()?;
        Ok(Self { messages })
    }

    /// Get the last user message (if any).
    pub fn last_user_message(&self) -> Option<&str> {
        self.messages.iter().rev().find_map(|m| {
            if m.role == crate::agent::llm::Role::User {
                if let crate::agent::llm::MessageContent::Text(ref text) = m.content {
                    return Some(text.as_str());
                }
            }
            None
        })
    }

    /// Truncate to last N messages (keeping system prompt).
    pub fn truncate(&mut self, max_messages: usize) {
        if self.messages.len() <= max_messages {
            return;
        }
        let has_system = self
            .messages
            .first()
            .map(|m| m.role == crate::agent::llm::Role::System)
            .unwrap_or(false);

        if has_system && max_messages > 1 {
            let system = self.messages[0].clone();
            let keep_start = self.messages.len() - (max_messages - 1);
            self.messages = std::iter::once(system)
                .chain(self.messages[keep_start..].iter().cloned())
                .collect();
        } else {
            let keep_start = self.messages.len() - max_messages;
            self.messages = self.messages[keep_start..].to_vec();
        }
    }
}

impl Default for Conversation {
    fn default() -> Self {
        Self::new()
    }
}
