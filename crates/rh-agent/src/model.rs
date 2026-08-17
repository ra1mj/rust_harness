//! The model seam: Service Definition ([`ModelProvider`]) plus a
//! deterministic mock provider used for keyless runs and tests.
//!
//! The seam is **streaming**: a provider returns a [`ModelStream`] of
//! [`ModelEvent`]s, so the agent loop can log `assistant/chunk` facts as
//! text arrives and the UI can render it live.

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use rh_core::{Context, Disposers, Plugin};
use rh_session::{ContentBlock, Message, Role};
use rh_tool::ToolDescription;

use crate::agent::next_call_id;

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

/// A user-managed model: a named provider configuration that the web UI can
/// add, select, and switch between at runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub id: String,
    pub label: String,
    /// `"mock"` (offline) or `"openai"` (OpenAI-compatible HTTP).
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

fn default_provider() -> String {
    "mock".to_string()
}

impl ModelConfig {
    /// The built-in offline demo model.
    pub fn mock() -> Self {
        Self {
            id: "mock".to_string(),
            label: "Mock（离线演示）".to_string(),
            provider: "mock".to_string(),
            base_url: None,
            api_key: None,
            model: Some("mock".to_string()),
        }
    }
}

/// The model adapter seam (the "Service Definition" role).
///
/// Register an implementation on the [`Context`] (e.g. [`MockModelProvider`]
/// or an HTTP provider) and the agent loop will resolve it; there is no
/// privileged model path to patch.
#[async_trait]
pub trait ModelProvider: Send + Sync {
    /// Stream the model's completion for a request.
    async fn stream(&self, request: ModelRequest) -> anyhow::Result<ModelStream>;
}

/// A deterministic, keyless model provider for tests and demos.
///
/// Behavior:
/// * if the last message is a tool result, synthesize a final assistant
///   answer (streamed as text chunks);
/// * else if the latest user text names one of the tools in the request,
///   emit exactly one tool call to that tool with example arguments;
/// * else echo the user text back as the assistant reply (streamed).
#[derive(Debug, Clone, Default)]
pub struct MockModelProvider {
    pub model: String,
}

#[async_trait]
impl ModelProvider for MockModelProvider {
    async fn stream(&self, request: ModelRequest) -> anyhow::Result<ModelStream> {
        let events = events_for(request);
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

fn events_for(request: ModelRequest) -> Vec<ModelEvent> {
    let is_after_tool = matches!(
        request.messages.last().map(|m| m.role),
        Some(ModelRole::Tool)
    );
    if is_after_tool {
        let mut events =
            chunk_text("done — the tool result above was recorded to the session log.");
        events.push(ModelEvent::Done(FinishReason::Stop));
        return events;
    }

    let user_text = request
        .messages
        .iter()
        .rev()
        .find(|m| m.role == ModelRole::User)
        .and_then(|m| m.content.clone())
        .unwrap_or_default();

    for tool in &request.tools {
        if !tool.name.is_empty() && user_text.contains(&tool.name) {
            return vec![
                ModelEvent::ToolCall(ModelToolCall {
                    id: next_call_id(),
                    name: tool.name.clone(),
                    arguments: example_args_for(&tool.name),
                }),
                ModelEvent::Done(FinishReason::ToolCalls),
            ];
        }
    }

    let mut events = chunk_text(&format!("echo: {user_text}"));
    events.push(ModelEvent::Done(FinishReason::Stop));
    events
}

/// Split text into fixed-size [`ModelEvent::Text`] chunks for streaming.
fn chunk_text(text: &str) -> Vec<ModelEvent> {
    const CHUNK: usize = 6;
    if text.is_empty() {
        return Vec::new();
    }
    let chars: Vec<char> = text.chars().collect();
    chars
        .chunks(CHUNK)
        .map(|chunk| ModelEvent::Text(chunk.iter().collect()))
        .collect()
}

/// Example arguments for the mock provider's synthetic tool calls.
fn example_args_for(name: &str) -> Value {
    match name {
        "bash" => json!({ "command": "echo hello from rh" }),
        "fs_read" => json!({ "path": "README.md" }),
        "fs_write" => json!({ "path": "/tmp/rh-demo.txt", "content": "written by rh" }),
        "todo_write" => json!({ "title": "demonstrate the loop" }),
        _ => json!({}),
    }
}

/// Mounts the mock model provider as a context service.
pub struct MockModelPlugin;

impl Plugin for MockModelPlugin {
    fn name(&self) -> &'static str {
        "model:mock"
    }

    fn mount(&self, ctx: &Context) -> anyhow::Result<Disposers> {
        let provider: Arc<dyn ModelProvider> = Arc::new(MockModelProvider {
            model: "mock".to_string(),
        });
        let disposer = ctx.provide_named("ModelProvider(mock)", provider);
        Ok(vec![disposer])
    }
}
