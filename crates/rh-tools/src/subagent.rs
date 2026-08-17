//! Codex-style subagent tasks: `task`, `task_output`, `task_wait`, `task_kill`.
//!
//! Mirrors Grok Build's vendored Codex task tool: a `task` spawns a
//! subagent — a fresh session running the same agent loop with its own model
//! and tool state — either in the background (return a handle immediately) or
//! foreground (block until done). `task_output` polls, `task_wait` waits for a
//! set, `task_kill` cancels.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::oneshot;

use rh_agent::{AgentBuilder, AgentDefinition};
use rh_core::Context;
use rh_session::{ContentBlock, Session, SessionEvent, SessionStore};
use rh_tool::{Tool, ToolCallContext, ToolDescription, ToolError, ToolId};

struct SubagentHandle {
    output: Arc<Mutex<Option<Result<String, String>>>>,
    cancel: Option<oneshot::Sender<()>>,
}

/// Tracks in-flight subagents (background and foreground).
pub struct SubagentManager {
    tasks: Mutex<HashMap<String, SubagentHandle>>,
    next_id: AtomicU64,
}

impl SubagentManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            tasks: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
        })
    }

    /// Spawn a subagent on its own session; returns the task id.
    fn spawn(&self, ctx: Context, prompt: String, session: Arc<Session>) -> String {
        let task_id = format!("task-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let handle = SubagentHandle {
            output: Arc::new(Mutex::new(None)),
            cancel: Some(cancel_tx),
        };
        self.tasks
            .lock()
            .expect("subagent map poisoned")
            .insert(task_id.clone(), handle);
        let output = self
            .tasks
            .lock()
            .expect("subagent map poisoned")
            .get(&task_id)
            .expect("just inserted")
            .output
            .clone();

        tokio::spawn(async move {
            let result = tokio::select! {
                r = run_subagent(&ctx, session, &prompt) => r.map_err(|e| e.to_string()),
                _ = cancel_rx => Err("cancelled".to_string()),
            };
            *output.lock().expect("output poisoned") = Some(result);
        });

        task_id
    }

    fn output(&self, task_id: &str) -> Option<Option<Result<String, String>>> {
        self.tasks
            .lock()
            .expect("subagent map poisoned")
            .get(task_id)
            .map(|h| h.output.lock().expect("output poisoned").clone())
    }

    fn cancel(&self, task_id: &str) -> bool {
        if let Some(handle) = self.tasks.lock().expect("subagent map poisoned").remove(task_id) {
            if let Some(tx) = handle.cancel {
                let _ = tx.send(());
            }
            true
        } else {
            false
        }
    }
}

/// Run a subagent: a fresh agent loop over a fresh session, reporting the
/// final assistant text.
async fn run_subagent(ctx: &Context, session: Arc<Session>, prompt: &str) -> anyhow::Result<String> {
    let definition = AgentDefinition {
        name: "subagent".to_string(),
        model: "subagent".to_string(),
        system_prompt: "You are a focused subagent. Complete the assigned task using the available tools, then report the result concisely.".to_string(),
        tool_ids: Vec::new(),
        max_steps: 8,
    };
    let agent = AgentBuilder::new(ctx.clone(), definition).build(session)?;
    let report = agent.run(prompt).await?;
    for event in report.events.iter().rev() {
        if let SessionEvent::AssistantMessage { content, .. } = event {
            let text = content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            if !text.is_empty() {
                return Ok(text);
            }
        }
    }
    Ok("(no output)".to_string())
}

/// `task` — spawn a subagent.
pub struct TaskTool;

#[async_trait]
impl Tool for TaskTool {
    fn id(&self) -> ToolId {
        "task".to_string()
    }

    fn description(&self) -> ToolDescription {
        ToolDescription::new(
            "task",
            "Spawn a subagent to complete a task autonomously on its own session. Returns a task_id; use task_output/task_wait to collect results.",
            json!({
                "type": "object",
                "properties": {
                    "prompt": { "type": "string", "description": "the task for the subagent" },
                    "description": { "type": "string", "description": "short label" },
                    "run_in_background": { "type": "boolean", "description": "return immediately instead of blocking (default false)" }
                },
                "required": ["prompt"]
            }),
        )
    }

    async fn run(&self, ctx: &ToolCallContext, args: Value) -> Result<Value, ToolError> {
        let prompt = args
            .get("prompt")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::execution("缺少 prompt"))?;
        let description = args.get("description").and_then(Value::as_str).unwrap_or("");
        let run_in_background = args.get("run_in_background").and_then(Value::as_bool).unwrap_or(false);

        let manager = ctx.service::<SubagentManager>("SubagentManager")?;
        let store = ctx.service::<SessionStore>("SessionStore")?;
        let label: String = if description.is_empty() {
            prompt.chars().take(30).collect()
        } else {
            description.chars().take(30).collect()
        };
        let sub_session = store.create(format!("子任务: {label}"));

        let task_id = manager.spawn(ctx.context.clone(), prompt.to_string(), sub_session);

        if run_in_background {
            Ok(json!({ "task_id": task_id, "status": "running" }))
        } else {
            let output = wait_foreground(&manager, &task_id, 300_000).await;
            match output {
                Ok(text) => Ok(json!({ "task_id": task_id, "status": "completed", "output": text })),
                Err(err) => Err(ToolError::execution(format!("子任务失败：{err}"))),
            }
        }
    }
}

/// `task_output` — read a subagent's current output.
pub struct TaskOutputTool;

#[async_trait]
impl Tool for TaskOutputTool {
    fn id(&self) -> ToolId {
        "task_output".to_string()
    }

    fn description(&self) -> ToolDescription {
        ToolDescription::new(
            "task_output",
            "Read a subagent's output by task_id (returns 'running' if still in flight).",
            json!({
                "type": "object",
                "properties": { "task_id": { "type": "string" } },
                "required": ["task_id"]
            }),
        )
    }

    async fn run(&self, ctx: &ToolCallContext, args: Value) -> Result<Value, ToolError> {
        let task_id = args.get("task_id").and_then(Value::as_str).ok_or_else(|| ToolError::execution("缺少 task_id"))?;
        let manager = ctx.service::<SubagentManager>("SubagentManager")?;
        match manager.output(task_id) {
            Some(Some(Ok(output))) => Ok(json!({ "task_id": task_id, "status": "completed", "output": output })),
            Some(Some(Err(err))) => Ok(json!({ "task_id": task_id, "status": "failed", "error": err })),
            Some(None) => Ok(json!({ "task_id": task_id, "status": "running" })),
            None => Err(ToolError::execution(format!("任务 {task_id} 不存在"))),
        }
    }
}

/// `task_wait` — wait for a set of subagents to finish.
pub struct TaskWaitTool;

#[async_trait]
impl Tool for TaskWaitTool {
    fn id(&self) -> ToolId {
        "task_wait".to_string()
    }

    fn description(&self) -> ToolDescription {
        ToolDescription::new(
            "task_wait",
            "Wait for one or more subagents to finish and return their outputs.",
            json!({
                "type": "object",
                "properties": {
                    "task_ids": { "type": "array", "items": { "type": "string" } },
                    "timeout_ms": { "type": "integer" }
                },
                "required": ["task_ids"]
            }),
        )
    }

    async fn run(&self, ctx: &ToolCallContext, args: Value) -> Result<Value, ToolError> {
        let task_ids: Vec<String> = args
            .get("task_ids")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .ok_or_else(|| ToolError::execution("缺少 task_ids"))?;
        let timeout_ms = args.get("timeout_ms").and_then(Value::as_u64).unwrap_or(300_000);
        let manager = ctx.service::<SubagentManager>("SubagentManager")?;

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
        loop {
            let mut all_done = true;
            let results: Vec<Value> = task_ids.iter().map(|id| match manager.output(id) {
                Some(Some(Ok(o))) => json!({ "task_id": id, "status": "completed", "output": o }),
                Some(Some(Err(e))) => json!({ "task_id": id, "status": "failed", "error": e }),
                _ => { all_done = false; json!({ "task_id": id, "status": "running" }) }
            }).collect();
            if all_done || tokio::time::Instant::now() >= deadline {
                return Ok(json!({ "tasks": results }));
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
}

/// `task_kill` — cancel a subagent.
pub struct TaskKillTool;

#[async_trait]
impl Tool for TaskKillTool {
    fn id(&self) -> ToolId {
        "task_kill".to_string()
    }

    fn description(&self) -> ToolDescription {
        ToolDescription::new(
            "task_kill",
            "Cancel a running subagent by task_id.",
            json!({
                "type": "object",
                "properties": { "task_id": { "type": "string" } },
                "required": ["task_id"]
            }),
        )
    }

    async fn run(&self, ctx: &ToolCallContext, args: Value) -> Result<Value, ToolError> {
        let task_id = args.get("task_id").and_then(Value::as_str).ok_or_else(|| ToolError::execution("缺少 task_id"))?;
        let manager = ctx.service::<SubagentManager>("SubagentManager")?;
        let cancelled = manager.cancel(task_id);
        Ok(json!({ "task_id": task_id, "cancelled": cancelled }))
    }
}

/// Block until a single subagent finishes, returning its output or error.
async fn wait_foreground(manager: &SubagentManager, task_id: &str, timeout_ms: u64) -> Result<String, String> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        match manager.output(task_id) {
            Some(Some(Ok(o))) => return Ok(o),
            Some(Some(Err(e))) => return Err(e),
            _ => {
                if tokio::time::Instant::now() >= deadline {
                    return Err("timeout".to_string());
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rh_core::Context;
    use rh_session::SessionStore;
    use rh_tool::ToolRegistry;

    struct EchoProvider;

    #[async_trait]
    impl rh_agent::ModelProvider for EchoProvider {
        async fn stream(
            &self,
            request: rh_agent::ModelRequest,
        ) -> anyhow::Result<rh_agent::ModelStream> {
            let user = request
                .messages
                .iter()
                .rev()
                .find(|m| m.role == rh_agent::ModelRole::User)
                .and_then(|m| m.content.clone())
                .unwrap_or_default();
            let events = vec![
                rh_agent::ModelEvent::Text(format!("echo: {user}")),
                rh_agent::ModelEvent::Done(rh_agent::FinishReason::Stop),
            ];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    #[tokio::test]
    async fn task_tool_spawns_and_collects_subagent() {
        let ctx = Context::new();
        let store = Arc::new(SessionStore::new(None));
        let _s = ctx.provide(store);
        let _m = ctx.provide(SubagentManager::new());
        let registry = Arc::new(ToolRegistry::new());
        let _r = ctx.provide(registry);
        let provider: Arc<dyn rh_agent::ModelProvider> = Arc::new(EchoProvider);
        let _p = ctx.provide(provider);

        let call_ctx = ToolCallContext::new(ctx, "parent", "call-1");
        let result = TaskTool
            .run(&call_ctx, json!({ "prompt": "just say hello", "description": "hi" }))
            .await
            .unwrap();

        assert_eq!(result["status"], "completed");
        assert!(result["output"].as_str().unwrap().contains("hello"));
    }
}
