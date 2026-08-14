//! Session event log, projection, and store.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use rh_core::{Context, Disposers, Plugin};

use crate::ids::{next_id, IdKind};
use crate::{MessageId, SessionId, StepId, TurnId};

/// A single content block inside a message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Plain text.
    Text { text: String },
    /// A model-requested tool invocation.
    ToolCall {
        id: String,
        name: String,
        arguments: Value,
    },
    /// The result of a tool invocation, returned to the model.
    ToolResult {
        id: String,
        output: Value,
        is_error: bool,
    },
}

/// The role a message plays in the conversation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A message as projected to the model (or echoed back to a UI).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }
}

/// A durable fact in a session.
///
/// The append-only log is the source of truth for model context
/// ([`Session::derive_messages`]); lifecycle events (`TurnStart`, …) are
/// durable but are not model-visible content.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    UserMessage {
        message_id: MessageId,
        content: Vec<ContentBlock>,
    },
    AssistantMessage {
        message_id: MessageId,
        content: Vec<ContentBlock>,
    },
    AssistantChunk {
        message_id: MessageId,
        text: String,
    },
    ToolCall {
        tool_call_id: String,
        tool_name: String,
        arguments: Value,
    },
    ToolResult {
        tool_call_id: String,
        output: Value,
        is_error: bool,
    },
    TurnStart {
        turn_id: TurnId,
    },
    StepStart {
        step_id: StepId,
    },
    StepEnd {
        step_id: StepId,
    },
    TurnEnd {
        turn_id: TurnId,
    },
}

/// An append-only session.
///
/// Cheap to clone; clones share the same log. Appending a durable event
/// also broadcasts it on the optional [`Context`] sink, so observers read
/// the same facts the model does.
#[derive(Clone)]
pub struct Session {
    id: SessionId,
    events: Arc<RwLock<Vec<SessionEvent>>>,
    sink: Option<Context>,
    live: tokio::sync::broadcast::Sender<SessionEvent>,
}

impl Session {
    /// Create a new, empty session.
    pub fn new(id: impl Into<String>, sink: Option<Context>) -> Arc<Self> {
        let (live, _) = tokio::sync::broadcast::channel(256);
        Arc::new(Self {
            id: id.into(),
            events: Arc::new(RwLock::new(Vec::new())),
            sink,
            live,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// Subscribe to this session's live event stream (e.g. for a WebSocket).
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<SessionEvent> {
        self.live.subscribe()
    }

    /// Append a durable event, broadcasting it to live subscribers and any
    /// context observers.
    pub fn append(&self, event: SessionEvent) {
        let _ = self.live.send(event.clone());
        if let Some(sink) = &self.sink {
            sink.emit(&event);
        }
        self.events.write().expect("session log poisoned").push(event);
    }

    /// A snapshot of the log, oldest first.
    pub fn events(&self) -> Vec<SessionEvent> {
        self.events.read().expect("session log poisoned").clone()
    }

    /// The number of events in the log.
    pub fn len(&self) -> usize {
        self.events.read().expect("session log poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Project model history from the log.
    ///
    /// This is the *only* path from the log to the model: the agent loop
    /// builds its request exclusively from this projection, enforcing the
    /// "model-visible means logged" invariant.
    pub fn derive_messages(&self, system_prompt: Option<&str>) -> Vec<Message> {
        let mut out: Vec<Message> = Vec::new();
        if let Some(sp) = system_prompt {
            out.push(Message::system(sp));
        }
        for event in self.events() {
            match event {
                SessionEvent::UserMessage { content, .. } => {
                    out.push(Message {
                        role: Role::User,
                        content,
                    });
                }
                SessionEvent::AssistantMessage { content, .. } => {
                    out.push(Message {
                        role: Role::Assistant,
                        content,
                    });
                }
                // Chunks are replay/streaming fidelity only; model history
                // comes from the assembled `AssistantMessage`.
                SessionEvent::AssistantChunk { .. } => {}
                SessionEvent::ToolCall {
                    tool_call_id,
                    tool_name,
                    arguments,
                } => {
                    push_tool_call(&mut out, ContentBlock::ToolCall {
                        id: tool_call_id,
                        name: tool_name,
                        arguments,
                    });
                }
                SessionEvent::ToolResult {
                    tool_call_id,
                    output,
                    is_error,
                } => {
                    out.push(Message {
                        role: Role::Tool,
                        content: vec![ContentBlock::ToolResult {
                            id: tool_call_id,
                            output,
                            is_error,
                        }],
                    });
                }
                // Lifecycle events are durable but not model-visible content.
                SessionEvent::TurnStart { .. }
                | SessionEvent::StepStart { .. }
                | SessionEvent::StepEnd { .. }
                | SessionEvent::TurnEnd { .. } => {}
            }
        }
        out
    }

    /// Fork this session: a new id over a copy of the current log.
    pub fn fork(&self, new_id: impl Into<String>) -> Arc<Self> {
        let (live, _) = tokio::sync::broadcast::channel(256);
        Arc::new(Self {
            id: new_id.into(),
            events: Arc::new(RwLock::new(self.events())),
            sink: self.sink.clone(),
            live,
        })
    }
}

fn push_tool_call(out: &mut Vec<Message>, block: ContentBlock) {
    if let Some(last) = out.last_mut().filter(|m| m.role == Role::Assistant) {
        last.content.push(block);
        return;
    }
    out.push(Message {
        role: Role::Assistant,
        content: vec![block],
    });
}

/// A registry of live sessions.
pub struct SessionStore {
    sessions: RwLock<HashMap<SessionId, Arc<Session>>>,
    sink: Option<Context>,
}

impl SessionStore {
    pub fn new(sink: Option<Context>) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            sink,
        }
    }

    /// Create and store a session with a fresh id.
    pub fn create_fresh(&self) -> Arc<Session> {
        self.create(next_id(IdKind::Session))
    }

    /// Create and store a session with the given id.
    pub fn create(&self, id: impl Into<String>) -> Arc<Session> {
        let id = id.into();
        let session = Session::new(id.clone(), self.sink.clone());
        self.sessions
            .write()
            .expect("session store poisoned")
            .insert(id, Arc::clone(&session));
        session
    }

    /// Look up a session by id.
    pub fn get(&self, id: &str) -> Option<Arc<Session>> {
        self.sessions
            .read()
            .expect("session store poisoned")
            .get(id)
            .cloned()
    }
}

/// Mounts a [`SessionStore`] as a context service.
pub struct SessionPlugin;

impl Plugin for SessionPlugin {
    fn name(&self) -> &'static str {
        "session"
    }

    fn mount(&self, ctx: &Context) -> anyhow::Result<Disposers> {
        let store = Arc::new(SessionStore::new(Some(ctx.clone())));
        let disposer = ctx.provide_named("SessionStore", store);
        Ok(vec![disposer])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn derive_messages_projects_model_history_from_the_log() {
        let session = Session::new("s", None);
        session.append(SessionEvent::UserMessage {
            message_id: "m1".into(),
            content: vec![ContentBlock::Text {
                text: "hello".into(),
            }],
        });
        session.append(SessionEvent::AssistantMessage {
            message_id: "m2".into(),
            content: vec![
                ContentBlock::Text { text: "hi".into() },
                ContentBlock::ToolCall {
                    id: "c1".into(),
                    name: "bash".into(),
                    arguments: json!({}),
                },
            ],
        });
        session.append(SessionEvent::ToolResult {
            tool_call_id: "c1".into(),
            output: json!("ok"),
            is_error: false,
        });

        let messages = session.derive_messages(Some("sys"));

        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, Role::System);
        assert_eq!(messages[1].role, Role::User);
        assert_eq!(messages[2].role, Role::Assistant);
        assert!(matches!(messages[2].content[1], ContentBlock::ToolCall { .. }));
        assert_eq!(messages[3].role, Role::Tool);
    }

    #[test]
    fn fork_copies_the_log() {
        let session = Session::new("a", None);
        session.append(SessionEvent::UserMessage {
            message_id: "m1".into(),
            content: vec![ContentBlock::Text {
                text: "x".into(),
            }],
        });
        let fork = session.fork("b");
        assert_eq!(fork.id(), "b");
        assert_eq!(fork.len(), 1);
    }
}
