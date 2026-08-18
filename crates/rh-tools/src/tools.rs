//! Built-in tools (the Consumer role) and the tool registry plugin.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use rh_core::{Context, Disposers, Plugin};
use rh_session::SessionStore;
use rh_tool::{Tool, ToolCallContext, ToolDescription, ToolError, ToolId, ToolRegistry};

use crate::fs::FileSystem;
use crate::search::{GlobTool, GrepTool};
use crate::shell::Shell;
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

/// Appends a todo to the current session's task list.
pub struct TodoWriteTool;

#[async_trait]
impl Tool for TodoWriteTool {
    fn id(&self) -> ToolId {
        "todo_write".to_string()
    }

    fn description(&self) -> ToolDescription {
        ToolDescription::new(
            "todo_write",
            "Append a task to the current session's todo list and return the list.",
            json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "the todo title" }
                },
                "required": ["title"]
            }),
        )
    }

    async fn run(&self, ctx: &ToolCallContext, args: Value) -> Result<Value, ToolError> {
        let title = args
            .get("title")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::execution("missing `title` argument"))?;
        let store = ctx.service::<SessionStore>("SessionStore")?;
        let session = store
            .get(&ctx.session_id)
            .ok_or_else(|| ToolError::execution("session not found"))?;
        let item = session.add_task(title.to_string());
        store
            .save(&session)
            .map_err(|e| ToolError::execution(e.to_string()))?;
        Ok(json!({ "id": item.id, "todos": session.tasks() }))
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
        ];
        for tool in tools {
            disposers.push(registry.register(tool));
        }

        Ok(disposers)
    }
}
