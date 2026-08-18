//! Built-in tools (the Consumer role) and the tool registry plugin.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use rh_core::{Context, Disposers, Plugin};
use rh_session::{SessionStore, TaskStatus};
use rh_tool::{Tool, ToolCallContext, ToolDescription, ToolError, ToolId, ToolRegistry};

use crate::fs::FileSystem;
use crate::search::{GlobTool, GrepTool};
use crate::shell::Shell;
use crate::skills::{SkillListTool, SkillStore, SkillTool};
use crate::subagent::{SubagentManager, TaskKillTool, TaskOutputTool, TaskTool, TaskWaitTool};
use crate::web::{WebFetchTool, WebSearchTool};
use crate::workflow::WorkflowStepTool;

/// Resolve a path against the tool's working directory (the workspace root),
/// so relative paths never escape to the process cwd.
fn resolve_path(ctx: &ToolCallContext, p: &str) -> std::path::PathBuf {
    let path = std::path::Path::new(p);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        ctx.cwd.join(path)
    }
}

/// Whether the session is in plan mode (write operations must be refused).
fn is_plan_mode(ctx: &ToolCallContext) -> bool {
    ctx.context
        .service::<SessionStore>()
        .and_then(|store| store.get(&ctx.session_id))
        .map(|session| session.work_mode() == "plan")
        .unwrap_or(false)
}

/// Runs a shell command via the [`Shell`] service.
pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn id(&self) -> ToolId {
        "bash".to_string()
    }

    fn description(&self) -> ToolDescription {
        ToolDescription::new(
            "bash",
            "Run a shell command and return stdout/stderr/exit code.",
            json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "the shell command to run" }
                },
                "required": ["command"]
            }),
        )
    }

    async fn run(&self, ctx: &ToolCallContext, args: Value) -> Result<Value, ToolError> {
        if is_plan_mode(ctx) {
            return Err(ToolError::execution("plan 模式下禁止执行命令"));
        }
        let command = args
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::execution("missing `command` argument"))?;
        let shell = ctx.service::<dyn Shell>("Shell")?;
        let output = shell.run(command, &ctx.cwd).await?;
        serde_json::to_value(output).map_err(ToolError::from)
    }
}

/// Reads a file via the [`FileSystem`] service.
pub struct FsReadTool;

#[async_trait]
impl Tool for FsReadTool {
    fn id(&self) -> ToolId {
        "fs_read".to_string()
    }

    fn description(&self) -> ToolDescription {
        ToolDescription::new(
            "fs_read",
            "Read a UTF-8 text file and return its content.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "path to the file" }
                },
                "required": ["path"]
            }),
        )
    }

    fn capabilities(&self) -> rh_tool::ToolCapabilities {
        rh_tool::ToolCapabilities {
            concurrency: rh_tool::ToolConcurrency::Concurrent,
            read_only: true,
        }
    }

    async fn run(&self, ctx: &ToolCallContext, args: Value) -> Result<Value, ToolError> {
        let path_str = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::execution("missing `path` argument"))?;
        let path = resolve_path(ctx, path_str);
        let fs = ctx.service::<dyn FileSystem>("FileSystem")?;
        let content = fs.read(&path).await?;
        Ok(json!({ "path": path.display().to_string(), "content": content }))
    }
}

/// Writes a file via the [`FileSystem`] service.
pub struct FsWriteTool;

#[async_trait]
impl Tool for FsWriteTool {
    fn id(&self) -> ToolId {
        "fs_write".to_string()
    }

    fn description(&self) -> ToolDescription {
        ToolDescription::new(
            "fs_write",
            "Write UTF-8 text content to a file.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "path to the file" },
                    "content": { "type": "string", "description": "content to write" }
                },
                "required": ["path", "content"]
            }),
        )
    }

    async fn run(&self, ctx: &ToolCallContext, args: Value) -> Result<Value, ToolError> {
        if is_plan_mode(ctx) {
            return Err(ToolError::execution("plan 模式下禁止写文件"));
        }
        let path_str = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::execution("missing `path` argument"))?;
        let content = args
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::execution("missing `content` argument"))?;
        let path = resolve_path(ctx, path_str);
        let fs = ctx.service::<dyn FileSystem>("FileSystem")?;
        fs.write(&path, content).await?;
        Ok(json!({ "path": path.display().to_string(), "written": content.len() }))
    }
}

/// Sets the current session's todo list (standard `todo_write` format).
pub struct TodoWriteTool;

#[async_trait]
impl Tool for TodoWriteTool {
    fn id(&self) -> ToolId {
        "todo_write".to_string()
    }

    fn description(&self) -> ToolDescription {
        ToolDescription::new(
            "todo_write",
            "Set the session's todo list. Pass the full list of todos with their status (pending / in_progress / completed / cancelled).",
            json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": { "type": "string" },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed", "cancelled"]
                                }
                            },
                            "required": ["content", "status"]
                        }
                    }
                },
                "required": ["todos"]
            }),
        )
    }

    async fn run(&self, ctx: &ToolCallContext, args: Value) -> Result<Value, ToolError> {
        let store = ctx.service::<SessionStore>("SessionStore")?;
        let session = store
            .get(&ctx.session_id)
            .ok_or_else(|| ToolError::execution("session not found"))?;

        if let Some(todos) = args.get("todos").and_then(Value::as_array) {
            let items: Vec<(String, TaskStatus)> = todos
                .iter()
                .filter_map(|t| {
                    let content = t.get("content").and_then(Value::as_str)?.to_string();
                    let status = parse_status(t.get("status").and_then(Value::as_str).unwrap_or("pending"));
                    Some((content, status))
                })
                .collect();
            session.replace_tasks(items);
        } else if let Some(title) = args.get("title").and_then(Value::as_str) {
            session.add_task(title.to_string());
        } else {
            return Err(ToolError::execution("缺少 todos 或 title"));
        }

        store
            .save(&session)
            .map_err(|e| ToolError::execution(e.to_string()))?;
        Ok(json!({ "todos": session.tasks() }))
    }
}

fn parse_status(s: &str) -> TaskStatus {
    match s {
        "in_progress" => TaskStatus::InProgress,
        "completed" => TaskStatus::Completed,
        "cancelled" => TaskStatus::Cancelled,
        _ => TaskStatus::Pending,
    }
}

/// Mounts the tool registry and the built-in tools.
pub struct ToolsPlugin;

impl Plugin for ToolsPlugin {
    fn name(&self) -> &'static str {
        "tools"
    }

    fn mount(&self, ctx: &Context) -> anyhow::Result<Disposers> {
        let mut disposers: Disposers = Vec::new();

        let registry = Arc::new(ToolRegistry::new());
        disposers.push(ctx.provide_named("ToolRegistry", registry.clone()));

        disposers.push(ctx.provide_named("SubagentManager", SubagentManager::new()));
        disposers.push(ctx.provide_named("SkillStore", SkillStore::new(None)));

        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(BashTool),
            Arc::new(FsReadTool),
            Arc::new(FsWriteTool),
            Arc::new(TodoWriteTool),
            Arc::new(WebFetchTool),
            Arc::new(WebSearchTool),
            Arc::new(GrepTool),
            Arc::new(GlobTool),
            Arc::new(TaskTool),
            Arc::new(TaskOutputTool),
            Arc::new(TaskWaitTool),
            Arc::new(TaskKillTool),
            Arc::new(WorkflowStepTool),
            Arc::new(SkillTool),
            Arc::new(SkillListTool),
        ];
        for tool in tools {
            disposers.push(registry.register(tool));
        }

        Ok(disposers)
    }
}
