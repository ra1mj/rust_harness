//! The model seam: the streaming [`ModelProvider`] Service Definition and the
//! wire vocabulary it carries.
//!
//! Mirrors DeepSeek Harness's LLM seam: the provider *route* (endpoint +
//! credential) lives in the adapter (`rh-providers`), while the **model id**
//! is selected per request via [`ModelRequest::model`].

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use rh_session::{ContentBlock, Message, Role};
use rh_tool::ToolDescription;

/// The role a model message plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelRole {
    System,
    User,
    Assistant,
    Tool,
}

impl From<Role> for ModelRole {
    fn from(role: Role) -> Self {
        match role {
            Role::System => ModelRole::System,
            Role::User => ModelRole::User,
            Role::Assistant => ModelRole::Assistant,
            Role::Tool => ModelRole::Tool,
        }
    }
}

/// A single model-requested tool invocation (flat, provider-agnostic form).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// A message on the wire to the model provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelMessage {
    pub role: ModelRole,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tool_calls: Vec<ModelToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ModelMessage {
    /// Project session [`Message`]s (from the log) onto the model wire form.
    ///
    /// This is the only conversion from the session log to the model; it is
    /// a pure function of the projection, preserving the invariant that
    /// everything model-visible is logged.
    pub fn from_session(messages: &[Message]) -> Vec<ModelMessage> {
        messages
            .iter()
            .map(|message| {
                let mut text_parts: Vec<String> = Vec::new();
                let mut tool_calls: Vec<ModelToolCall> = Vec::new();
                let mut tool_call_id: Option<String> = None;
                for block in &message.content {
                    match block {
                        ContentBlock::Text { text } => text_parts.push(text.clone()),
                        ContentBlock::ToolCall {
                            id,
                            name,
                            arguments,
                        } => tool_calls.push(ModelToolCall {
                            id: id.clone(),
                            name: name.clone(),
                            arguments: arguments.clone(),
                        }),
                        ContentBlock::ToolResult { id, .. } => tool_call_id = Some(id.clone()),
                    }
                }
                ModelMessage {
                    role: ModelRole::from(message.role),
                    content: if text_parts.is_empty() {
                        None
                    } else {
                        Some(text_parts.join("\n"))
                    },
                    tool_calls,
                    tool_call_id,
                }
            })
            .collect()
    }
}

/// Why a completion finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
}

/// A request to a model provider.
#[derive(Debug, Clone)]
pub struct ModelRequest {
    /// Model id selected for this request.
    pub model: String,
    pub messages: Vec<ModelMessage>,
    pub tools: Vec<ToolDescription>,
}

/// One event in a model completion stream.
#[derive(Debug, Clone)]
pub enum ModelEvent {
    /// A chunk of assistant text.
    Text(String),
    /// A tool call the model wants executed.
    ToolCall(ModelToolCall),
    /// Terminal: the reason the completion ended. Always last.
    Done(FinishReason),
}

/// An opaque, pinned stream of [`ModelEvent`]s.
pub type ModelStream = Pin<Box<dyn Stream<Item = ModelEvent> + Send>>;

/// The model adapter seam (the "Service Definition" role).
///
/// An adapter is bound to one provider route; the model id is carried per
/// request. Register adapters on the [`Context`](rh_core::Context) or inject
/// them with [`AgentBuilder::with_model`](crate::AgentBuilder::with_model).
#[async_trait]
pub trait ModelProvider: Send + Sync {
    /// Stream the model's completion for a request.
    async fn stream(&self, request: ModelRequest) -> anyhow::Result<ModelStream>;
}
