//! Session event log, projection, tasks, and store (with persistence + export).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use anyhow::{anyhow, Result};
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

/// A user- or agent-created task in the session's todo list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskItem {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub done: bool,
}

/// Lightweight metadata for listing sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub title: String,
    pub created_at: u64,
    pub event_count: usize,
    pub task_count: usize,
}

/// The on-disk shape of a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: String,
    pub title: String,
    pub created_at: u64,
    pub events: Vec<SessionEvent>,
    pub tasks: Vec<TaskItem>,
    #[serde(default)]
    pub workspace: Option<PathBuf>,
}

/// Default isolated workspace root for a session (per-session folder, so the
/// agent never operates on the harness's own directory by default).
fn default_workspace(id: &str) -> PathBuf {
    let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."));
    home.join(".rh").join("workspaces").join(id)
}

/// Human-readable workspace context for prompt injection: the root path, its
/// top-level contents, and whether it is a git repo.
pub fn workspace_context(root: &std::path::Path) -> String {
    let mut out = format!("工作区目录：{}\n", root.display());
    match std::fs::read_dir(root) {
        Ok(entries) => {
            let mut items: Vec<String> = entries
                .flatten()
                .map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    let mark = if e.path().is_dir() { "📁" } else { "📄" };
                    format!("{mark} {name}")
                })
                .collect();
            items.sort();
            let shown = items.into_iter().take(50).collect::<Vec<_>>().join("\n");
            out.push_str(&format!("目录内容：\n{}\n", if shown.is_empty() { "(空)" } else { &shown }));
        }
        Err(_) => out.push_str("目录内容：(不可读)\n"),
    }
    out.push_str(&format!(
        "Git 仓库：{}\n",
        if root.join(".git").exists() { "是" } else { "否" }
    ));
    out
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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
    title: Arc<RwLock<String>>,
    tasks: Arc<RwLock<Vec<TaskItem>>>,
    created_at: u64,
    workspace: Arc<RwLock<PathBuf>>,
}

impl Session {
    /// Create a new, empty session with the default title.
    pub fn new(id: impl Into<String>, sink: Option<Context>) -> Arc<Self> {
        Self::with_title(id, "新会话".to_string(), sink)
    }

    /// Create a new, empty session with the given title.
    pub fn with_title(id: impl Into<String>, title: String, sink: Option<Context>) -> Arc<Self> {
        let id = id.into();
        let workspace = default_workspace(&id);
        let (live, _) = tokio::sync::broadcast::channel(256);
        Arc::new(Self {
            id,
            events: Arc::new(RwLock::new(Vec::new())),
            sink,
            live,
            title: Arc::new(RwLock::new(title)),
            tasks: Arc::new(RwLock::new(Vec::new())),
            created_at: now_millis(),
            workspace: Arc::new(RwLock::new(workspace)),
        })
    }

    /// Rebuild a session from a persisted record.
    pub fn from_record(record: SessionRecord, sink: Option<Context>) -> Arc<Self> {
        let workspace = record
            .workspace
            .unwrap_or_else(|| default_workspace(&record.id));
        let (live, _) = tokio::sync::broadcast::channel(256);
        Arc::new(Self {
            id: record.id,
            events: Arc::new(RwLock::new(record.events)),
            sink,
            live,
            title: Arc::new(RwLock::new(record.title)),
            tasks: Arc::new(RwLock::new(record.tasks)),
            created_at: record.created_at,
            workspace: Arc::new(RwLock::new(workspace)),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn title(&self) -> String {
        self.title.read().expect("title poisoned").clone()
    }

    pub fn set_title(&self, title: impl Into<String>) {
        *self.title.write().expect("title poisoned") = title.into();
    }

    pub fn created_at(&self) -> u64 {
        self.created_at
    }

    pub fn tasks(&self) -> Vec<TaskItem> {
        self.tasks.read().expect("tasks poisoned").clone()
    }

    /// Add a task, returning the new item.
    pub fn add_task(&self, title: impl Into<String>) -> TaskItem {
        let item = TaskItem {
            id: next_id(IdKind::Task),
            title: title.into(),
            done: false,
        };
        self.tasks.write().expect("tasks poisoned").push(item.clone());
        item
    }

    /// Mark a task done/undone.
    pub fn set_task_done(&self, id: &str, done: bool) -> bool {
        let mut tasks = self.tasks.write().expect("tasks poisoned");
        if let Some(item) = tasks.iter_mut().find(|t| t.id == id) {
            item.done = done;
            true
        } else {
            false
        }
    }

    /// The session's workspace root (created on demand).
    pub fn workspace(&self) -> PathBuf {
        let root = self.workspace.read().expect("workspace poisoned").clone();
        let _ = std::fs::create_dir_all(&root);
        root
    }

    /// Point this session at a different workspace folder.
    pub fn set_workspace(&self, root: impl Into<PathBuf>) -> PathBuf {
        let root = root.into();
        let _ = std::fs::create_dir_all(&root);
        *self.workspace.write().expect("workspace poisoned") = root.clone();
        root
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

    pub fn meta(&self) -> SessionMeta {
        SessionMeta {
            id: self.id.clone(),
            title: self.title(),
            created_at: self.created_at,
            event_count: self.len(),
            task_count: self.tasks.read().expect("tasks poisoned").len(),
        }
    }

    pub fn to_record(&self) -> SessionRecord {
        SessionRecord {
            id: self.id.clone(),
            title: self.title(),
            created_at: self.created_at,
            events: self.events(),
            tasks: self.tasks(),
            workspace: Some(self.workspace()),
        }
    }

    /// Serialize the full session (log + tasks + metadata) as pretty JSON.
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(&self.to_record())?)
    }

    /// Render the transcript as human-readable Markdown.
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("# {}\n\n", self.title()));
        for event in self.events() {
            match event {
                SessionEvent::UserMessage { content, .. } => {
                    let text = text_of(&content);
                    if !text.is_empty() {
                        out.push_str(&format!("**你**：{}\n\n", text));
                    }
                }
                SessionEvent::AssistantMessage { content, .. } => {
                    let text = text_of(&content);
                    if !text.is_empty() {
                        out.push_str(&format!("**助手**：{}\n\n", text));
                    }
                }
                SessionEvent::ToolCall {
                    tool_name,
                    arguments,
                    ..
                } => {
                    out.push_str(&format!("**工具调用** `{tool_name}`：`{arguments}`\n"));
                }
                SessionEvent::ToolResult {
                    output, is_error, ..
                } => {
                    let mark = if is_error { "✗" } else { "✓" };
                    out.push_str(&format!("  {mark} 结果：`{output}`\n\n"));
                }
                _ => {}
            }
        }
        if !self.tasks().is_empty() {
            out.push_str("\n## 任务\n\n");
            for task in self.tasks() {
                let mark = if task.done { "[x]" } else { "[ ]" };
                out.push_str(&format!("- {mark} {}\n", task.title));
            }
        }
        out
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
        let new_id = new_id.into();
        let (live, _) = tokio::sync::broadcast::channel(256);
        Arc::new(Self {
            workspace: Arc::new(RwLock::new(default_workspace(&new_id))),
            id: new_id,
            events: Arc::new(RwLock::new(self.events())),
            sink: self.sink.clone(),
            live,
            title: Arc::new(RwLock::new(self.title())),
            tasks: Arc::new(RwLock::new(self.tasks())),
            created_at: now_millis(),
        })
    }
}

fn text_of(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
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

/// A registry of sessions, optionally persisted to a directory.
pub struct SessionStore {
    sessions: RwLock<HashMap<SessionId, Arc<Session>>>,
    sink: Option<Context>,
    dir: Option<PathBuf>,
}

impl SessionStore {
    /// An in-memory-only store (no persistence).
    pub fn new(sink: Option<Context>) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            sink,
            dir: None,
        }
    }

    /// A store persisted to `dir` (one JSON file per session).
    pub fn persistent(dir: impl Into<PathBuf>, sink: Option<Context>) -> Self {
        let store = Self {
            sessions: RwLock::new(HashMap::new()),
            sink,
            dir: Some(dir.into()),
        };
        store.load_all();
        store
    }

    fn load_all(&self) {
        let Some(dir) = &self.dir else { return };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(record) = serde_json::from_str::<SessionRecord>(&text) {
                    let session = Session::from_record(record, self.sink.clone());
                    self.sessions
                        .write()
                        .expect("session store poisoned")
                        .insert(session.id().to_string(), session);
                }
            }
        }
    }

    /// Create and store a session with a fresh id and the default title.
    pub fn create_fresh(&self) -> Arc<Session> {
        self.create("新会话")
    }

    /// Create and store a session with a fresh id and the given title.
    pub fn create(&self, title: impl Into<String>) -> Arc<Session> {
        let id = next_id(IdKind::Session);
        let session = Session::with_title(id.clone(), title.into(), self.sink.clone());
        self.sessions
            .write()
            .expect("session store poisoned")
            .insert(id, Arc::clone(&session));
        let _ = self.save(&session);
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

    /// All sessions, oldest first.
    pub fn list(&self) -> Vec<SessionMeta> {
        let mut metas: Vec<SessionMeta> = self
            .sessions
            .read()
            .expect("session store poisoned")
            .values()
            .map(|s| s.meta())
            .collect();
        metas.sort_by_key(|m| m.created_at);
        metas
    }

    /// Rename a session.
    pub fn rename(&self, id: &str, title: impl Into<String>) -> Result<()> {
        let session = self
            .get(id)
            .ok_or_else(|| anyhow!("session {id} not found"))?;
        session.set_title(title);
        self.save(&session)
    }

    /// Delete a session (from memory and disk).
    pub fn remove(&self, id: &str) -> Result<()> {
        let removed = self.sessions.write().expect("session store poisoned").remove(id);
        if let Some(session) = removed {
            if let Some(dir) = &self.dir {
                let _ = std::fs::remove_file(dir.join(format!("{}.json", session.id())));
            }
        }
        Ok(())
    }

    /// Persist a session (no-op for the in-memory store).
    pub fn save(&self, session: &Session) -> Result<()> {
        let Some(dir) = &self.dir else { return Ok(()) };
        std::fs::create_dir_all(dir)?;
        std::fs::write(dir.join(format!("{}.json", session.id())), session.to_json()?)?;
        Ok(())
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

    #[test]
    fn tasks_and_export() {
        let session = Session::new("t", None);
        session.add_task("写文档");
        let item = session.add_task("写测试");
        session.set_task_done(&item.id, true);
        assert_eq!(session.tasks().len(), 2);

        session.append(SessionEvent::UserMessage {
            message_id: "m1".into(),
            content: vec![ContentBlock::Text {
                text: "你好".into(),
            }],
        });

        let md = session.to_markdown();
        assert!(md.contains("**你**：你好"));
        assert!(md.contains("- [x] 写测试"));

        let json = session.to_json().unwrap();
        assert!(json.contains("\"tasks\""));
    }

    #[test]
    fn persistent_store_roundtrip() {
        let dir = std::env::temp_dir().join(format!("rh-session-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        {
            let store = SessionStore::persistent(&dir, None);
            let session = store.create("我的会话");
            session.add_task("任务一");
            session.append(SessionEvent::UserMessage {
                message_id: "m1".into(),
                content: vec![ContentBlock::Text {
                    text: "hi".into(),
                }],
            });
            store.save(&session).unwrap();
        }
        {
            let store = SessionStore::persistent(&dir, None);
            let metas = store.list();
            assert_eq!(metas.len(), 1);
            assert_eq!(metas[0].title, "我的会话");
            let session = store.get(&metas[0].id).unwrap();
            assert_eq!(session.len(), 1);
            assert_eq!(session.tasks().len(), 1);
            store.remove(&metas[0].id).unwrap();
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
