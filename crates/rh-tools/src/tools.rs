//! Built-in tools (the Consumer role) and the tool registry plugin.

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde_json::{json, Value};

use rh_core::{Context, Disposers, Plugin};
use rh_tool::{
    Tool, ToolCallContext, ToolDescription, ToolError, ToolId, ToolRegistry,
};

use crate::fs::FileSystem;
use crate::shell::Shell;

/// An in-memory todo list, shared behind a service so tools and future
/// plugins see the same state.
pub struct TodoList {
    items: RwLock<Vec<String>>,
}

impl TodoList {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            items: RwLock::new(Vec::new()),
        })
    }

    pub fn add(&self, title: String) -> usize {
        let mut items = self.items.write().expect("todo list poisoned");
        items.push(title);
        items.len() - 1
    }

    pub fn list(&self) -> Vec<String> {
        self.items.read().expect("todo list poisoned").clone()
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
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::execution("missing `path` argument"))?;
        let fs = ctx.service::<dyn FileSystem>("FileSystem")?;
        let content = fs.read(std::path::Path::new(path)).await?;
        Ok(json!({ "path": path, "content": content }))
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
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::execution("missing `path` argument"))?;
        let content = args
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::execution("missing `content` argument"))?;
        let fs = ctx.service::<dyn FileSystem>("FileSystem")?;
        fs.write(std::path::Path::new(path), content).await?;
        Ok(json!({ "path": path, "written": content.len() }))
    }
}

/// Appends a todo to the shared [`TodoList`].
pub struct TodoWriteTool;

#[async_trait]
impl Tool for TodoWriteTool {
    fn id(&self) -> ToolId {
        "todo_write".to_string()
    }

    fn description(&self) -> ToolDescription {
        ToolDescription::new(
            "todo_write",
            "Append a todo item to the shared todo list and return the list.",
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
        let todos = ctx.service::<TodoList>("TodoList")?;
        let index = todos.add(title.to_string());
        Ok(json!({ "index": index, "todos": todos.list() }))
    }
}

/// Mounts the tool registry, the todo list service, and the built-in tools.
pub struct ToolsPlugin;

impl Plugin for ToolsPlugin {
    fn name(&self) -> &'static str {
        "tools"
    }

    fn mount(&self, ctx: &Context) -> anyhow::Result<Disposers> {
        let mut disposers: Disposers = Vec::new();

        let registry = Arc::new(ToolRegistry::new());
        disposers.push(ctx.provide_named("ToolRegistry", registry.clone()));

        disposers.push(ctx.provide_named("TodoList", TodoList::new()));

        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(BashTool),
            Arc::new(FsReadTool),
            Arc::new(FsWriteTool),
            Arc::new(TodoWriteTool),
        ];
        for tool in tools {
            disposers.push(registry.register(tool));
        }

        Ok(disposers)
    }
}
