//! rh-mcp — a minimal Model Context Protocol (MCP) client over stdio.
//!
//! Bridges MCP servers into the harness's unified [`Tool`] trait: each MCP
//! server is spawned as a subprocess, its `tools/list` results become
//! [`McpTool`]s, and `tools/call` is forwarded on [`Tool::run`].

use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use rh_tool::{Tool, ToolCallContext, ToolDescription, ToolError, ToolId};

/// Configuration for one MCP server (stdio transport).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

struct McpInner {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    child: Child,
}

impl Drop for McpInner {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

/// A connected MCP server (cloneable; shares the connection).
#[derive(Clone)]
pub struct McpClient(Arc<tokio::sync::Mutex<McpInner>>);

impl McpClient {
    /// Spawn the server process and perform the MCP handshake.
    pub async fn spawn(config: &McpServerConfig) -> Result<Self> {
        let mut child = Command::new(&config.command)
            .args(&config.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| anyhow!("无法启动 MCP 服务器 `{}` ({})：{e}", config.command, config.name))?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdout = BufReader::new(child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?);

        let client = Self(Arc::new(tokio::sync::Mutex::new(McpInner {
            stdin,
            stdout,
            next_id: 0,
            child,
        })));

        client
            .request("initialize", json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "rh", "version": "0.1.0" }
            }))
            .await?;
        client
            .notify("notifications/initialized", json!({}))
            .await?;
        Ok(client)
    }

    /// List the server's tools.
    pub async fn list_tools(&self) -> Result<Vec<Value>> {
        let response = self.request("tools/list", json!({})).await?;
        Ok(response
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// Call a tool, returning the `result` JSON (which carries `content`).
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value> {
        let response = self
            .request("tools/call", json!({ "name": name, "arguments": arguments }))
            .await?;
        Ok(response)
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let mut inner = self.0.lock().await;
        let mut line = serde_json::to_string(&msg)?;
        line.push('\n');
        inner.stdin.write_all(line.as_bytes()).await?;
        inner.stdin.flush().await?;
        Ok(())
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let mut inner = self.0.lock().await;
        let id = inner.next_id;
        inner.next_id += 1;
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let mut line = serde_json::to_string(&msg)?;
        line.push('\n');
        inner.stdin.write_all(line.as_bytes()).await?;
        inner.stdin.flush().await?;

        loop {
            let mut buf = String::new();
            let n = inner.stdout.read_line(&mut buf).await?;
            if n == 0 {
                return Err(anyhow!("MCP 服务器连接关闭（方法 {method}）"));
            }
            if buf.trim().is_empty() {
                continue;
            }
            let parsed: Value = serde_json::from_str(&buf)?;
            // Responses carry an id; notifications do not.
            if parsed.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(err) = parsed.get("error") {
                    let message = err.get("message").and_then(Value::as_str).unwrap_or("unknown");
                    return Err(anyhow!("MCP 错误：{message}"));
                }
                return Ok(parsed.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    }
}

/// A tool backed by an MCP server's `tools/call`.
pub struct McpTool {
    name: String,
    description: String,
    schema: Value,
    client: McpClient,
}

#[async_trait]
impl Tool for McpTool {
    fn id(&self) -> ToolId {
        self.name.clone()
    }

    fn description(&self) -> ToolDescription {
        ToolDescription::new(self.name.clone(), self.description.clone(), self.schema.clone())
    }

    async fn run(&self, _ctx: &ToolCallContext, args: Value) -> Result<Value, ToolError> {
        let result = self
            .client
            .call_tool(&self.name, args)
            .await
            .map_err(|e| ToolError::execution(e.to_string()))?;
        // Extract text from the MCP result's `content` blocks.
        let text = result
            .get("content")
            .and_then(Value::as_array)
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|b| b.get("text").and_then(Value::as_str).map(str::to_string))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_else(|| result.to_string());
        Ok(json!({
            "is_error": result.get("isError").and_then(Value::as_bool).unwrap_or(false),
            "content": text
        }))
    }
}

/// Connect to a server and return the client plus one [`Tool`] per MCP tool.
pub async fn connect(config: &McpServerConfig) -> Result<(McpClient, Vec<Arc<dyn Tool>>)> {
    let client = McpClient::spawn(config).await?;
    let tools = client.list_tools().await?;
    let mut out: Vec<Arc<dyn Tool>> = Vec::new();
    for tool in tools {
        let name = match tool.get("name").and_then(Value::as_str) {
            Some(n) => n.to_string(),
            None => continue,
        };
        let description = tool
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let schema = tool
            .get("inputSchema")
            .cloned()
            .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
        out.push(Arc::new(McpTool {
            name,
            description,
            schema,
            client: client.clone(),
        }));
    }
    Ok((client, out))
}
