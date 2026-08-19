//! rh-mcp — a Model Context Protocol (MCP) client with pluggable transports.
//!
//! Bridges MCP servers into the harness's unified [`Tool`] trait. Supported
//! transports: `stdio` (subprocess), `sse` (legacy HTTP+SSE), and `http`
//! (streamable HTTP). Each server's `tools/list` results become [`McpTool`]s,
//! and `tools/call` is forwarded on [`Tool::run`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{oneshot, Mutex};

use rh_tool::{Tool, ToolCallContext, ToolDescription, ToolError, ToolId};

const PROTOCOL_VERSION: &str = "2024-11-05";

/// Configuration for one MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    /// Transport: `stdio` (default) | `sse` | `http`.
    #[serde(default)]
    pub transport: String,
    /// Launch command (stdio transport).
    #[serde(default)]
    pub command: String,
    /// Launch args (stdio transport).
    #[serde(default)]
    pub args: Vec<String>,
    /// Server URL (`sse` / `http` transports).
    #[serde(default)]
    pub url: String,
}

impl McpServerConfig {
    fn transport_kind(&self) -> &str {
        let t = self.transport.trim();
        if t.is_empty() {
            "stdio"
        } else {
            t
        }
    }
}

/// Shared JSON-RPC transport contract.
#[async_trait]
trait Transport: Send + Sync {
    async fn request(&self, id: u64, method: &str, params: Value) -> Result<Value>;
    async fn notify(&self, method: &str, params: Value) -> Result<()>;
}

async fn write_line(stdin: &mut ChildStdin, msg: &Value) -> Result<()> {
    let mut line = serde_json::to_string(msg)?;
    line.push('\n');
    stdin.write_all(line.as_bytes()).await?;
    stdin.flush().await?;
    Ok(())
}

fn rpc_result(parsed: Value) -> Result<Value> {
    if let Some(err) = parsed.get("error") {
        let message = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return Err(anyhow!("MCP 错误：{message}"));
    }
    Ok(parsed.get("result").cloned().unwrap_or(Value::Null))
}

// ---------- stdio transport ----------

struct StdioInner {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    child: Child,
}

impl Drop for StdioInner {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

struct StdioTransport {
    inner: Mutex<StdioInner>,
}

impl StdioTransport {
    async fn spawn(config: &McpServerConfig) -> Result<Self> {
        let mut child = Command::new(&config.command)
            .args(&config.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| {
                anyhow!(
                    "无法启动 MCP 服务器 `{}` ({})：{e}",
                    config.command,
                    config.name
                )
            })?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdout = BufReader::new(child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?);
        Ok(Self {
            inner: Mutex::new(StdioInner {
                stdin,
                stdout,
                child,
            }),
        })
    }
}

#[async_trait]
impl Transport for StdioTransport {
    async fn request(&self, id: u64, method: &str, params: Value) -> Result<Value> {
        let mut inner = self.inner.lock().await;
        write_line(
            &mut inner.stdin,
            &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
        )
        .await?;
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
            if parsed.get("id").and_then(Value::as_u64) == Some(id) {
                return rpc_result(parsed);
            }
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let mut inner = self.inner.lock().await;
        write_line(
            &mut inner.stdin,
            &json!({ "jsonrpc": "2.0", "method": method, "params": params }),
        )
        .await
    }
}

// ---------- streamable HTTP transport ----------

struct HttpTransport {
    url: String,
    client: reqwest::Client,
    session: Mutex<Option<String>>,
}

impl HttpTransport {
    fn new(url: String) -> Self {
        Self {
            url,
            client: reqwest::Client::new(),
            session: Mutex::new(None),
        }
    }

    async fn send(&self, msg: &Value, want_response: bool) -> Result<Option<Value>> {
        let session = self.session.lock().await.clone();
        let mut req = self
            .client
            .post(&self.url)
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .json(msg);
        if let Some(s) = &session {
            req = req.header("Mcp-Session-Id", s);
        }
        let response = req.send().await?.error_for_status()?;
        if let Some(s) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            *self.session.lock().await = Some(s.to_string());
        }
        if !want_response {
            let _ = response.bytes().await;
            return Ok(None);
        }
        let ct = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if ct.contains("text/event-stream") {
            parse_sse_for_id(response.bytes_stream()).await
        } else {
            let value: Value = response.json().await?;
            Ok(Some(value))
        }
    }
}

#[async_trait]
impl Transport for HttpTransport {
    async fn request(&self, id: u64, method: &str, params: Value) -> Result<Value> {
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let value = self
            .send(&msg, true)
            .await?
            .ok_or_else(|| anyhow!("MCP HTTP 服务器未返回响应（方法 {method}）"))?;
        rpc_result(value)
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        self.send(&msg, false).await?;
        Ok(())
    }
}

// ---------- legacy SSE transport ----------

struct SseTransport {
    client: reqwest::Client,
    endpoint: String,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
}

impl SseTransport {
    async fn connect(url: &str) -> Result<Self> {
        let client = reqwest::Client::new();
        let response = client
            .get(url)
            .header("Accept", "text/event-stream")
            .send()
            .await?
            .error_for_status()?;
        let mut stream = response.bytes_stream();
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // Read SSE events until the `endpoint` event announces the POST url.
        let mut buf: Vec<u8> = Vec::new();
        let mut event = String::new();
        let mut endpoint: Option<String> = None;
        'outer: loop {
            match stream.next().await {
                Some(Ok(chunk)) => buf.extend_from_slice(&chunk),
                _ => break,
            }
            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let line_bytes: Vec<u8> = buf.drain(..=pos).collect();
                let line = String::from_utf8_lossy(&line_bytes);
                let line = line.trim_end_matches(['\n', '\r']);
                if let Some(ev) = line.strip_prefix("event:") {
                    event = ev.trim().to_string();
                } else if let Some(data) = line.strip_prefix("data:") {
                    if event == "endpoint" {
                        endpoint = Some(data.trim_start().to_string());
                        break 'outer;
                    }
                } else if line.is_empty() {
                    event.clear();
                }
            }
        }
        let endpoint = endpoint.ok_or_else(|| anyhow!("MCP SSE 服务器未返回 endpoint 事件"))?;
        let endpoint = resolve_url(url, &endpoint);

        // Background reader: route responses back to pending requests by id.
        let pending2 = Arc::clone(&pending);
        tokio::spawn(async move {
            let mut buf: Vec<u8> = Vec::new();
            while let Some(chunk) = stream.next().await {
                let Ok(chunk) = chunk else { break };
                buf.extend_from_slice(&chunk);
                while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                    let line_bytes: Vec<u8> = buf.drain(..=pos).collect();
                    let line = String::from_utf8_lossy(&line_bytes);
                    let line = line.trim_end_matches(['\n', '\r']);
                    if let Some(data) = line.strip_prefix("data:") {
                        let data = data.trim_start();
                        if let Ok(v) = serde_json::from_str::<Value>(data) {
                            if let Some(id) = v.get("id").and_then(Value::as_u64) {
                                if let Some(tx) = pending2.lock().await.remove(&id) {
                                    let _ = tx.send(v);
                                }
                            }
                        }
                    }
                }
            }
        });

        Ok(Self {
            client,
            endpoint,
            pending,
        })
    }
}

#[async_trait]
impl Transport for SseTransport {
    async fn request(&self, id: u64, method: &str, params: Value) -> Result<Value> {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.client
            .post(&self.endpoint)
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .json(&msg)
            .send()
            .await?
            .error_for_status()?;
        let value = match tokio::time::timeout(std::time::Duration::from_secs(300), rx).await {
            Ok(Ok(v)) => v,
            _ => {
                self.pending.lock().await.remove(&id);
                return Err(anyhow!("等待 MCP SSE 响应超时（方法 {method}）"));
            }
        };
        rpc_result(value)
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        self.client
            .post(&self.endpoint)
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .json(&msg)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }
}

/// Parse an SSE byte stream, returning the first JSON-RPC message that carries
/// an `id` (the response to our request).
async fn parse_sse_for_id<S, B, E>(mut src: S) -> Result<Option<Value>>
where
    S: futures::Stream<Item = Result<B, E>> + Unpin,
    B: AsRef<[u8]>,
    E: std::fmt::Display,
{
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = src.next().await {
        let chunk = chunk.map_err(|e| anyhow!("读取 SSE 流失败：{e}"))?;
        buf.extend_from_slice(chunk.as_ref());
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line_bytes: Vec<u8> = buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line_bytes);
            let line = line.trim_end_matches(['\n', '\r']);
            if let Some(data) = line.strip_prefix("data:") {
                let data = data.trim_start();
                if data == "[DONE]" {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<Value>(data) {
                    if v.get("id").is_some() {
                        return Ok(Some(v));
                    }
                }
            }
        }
    }
    Ok(None)
}

/// Resolve a (possibly relative) endpoint URL against the SSE base URL.
fn resolve_url(base: &str, endpoint: &str) -> String {
    if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        return endpoint.to_string();
    }
    let Some(scheme_end) = base.find("://") else {
        return format!(
            "{}/{}",
            base.trim_end_matches('/'),
            endpoint.trim_start_matches('/')
        );
    };
    let origin_end = base[scheme_end + 3..]
        .find('/')
        .map(|p| scheme_end + 3 + p)
        .unwrap_or(base.len());
    if endpoint.starts_with('/') {
        format!("{}{}", &base[..origin_end], endpoint)
    } else {
        format!("{}/{}", base.trim_end_matches('/'), endpoint)
    }
}

// ---------- client ----------

struct ClientInner {
    transport: Box<dyn Transport>,
    next_id: AtomicU64,
}

/// A connected MCP server (cloneable; shares the connection).
#[derive(Clone)]
pub struct McpClient(Arc<ClientInner>);

impl McpClient {
    /// Connect using the configured transport and perform the MCP handshake.
    pub async fn connect(config: &McpServerConfig) -> Result<Self> {
        let transport: Box<dyn Transport> = match config.transport_kind() {
            "sse" => Box::new(SseTransport::connect(&config.url).await?),
            "http" => Box::new(HttpTransport::new(config.url.clone())),
            _ => Box::new(StdioTransport::spawn(config).await?),
        };
        let client = Self(Arc::new(ClientInner {
            transport,
            next_id: AtomicU64::new(0),
        }));
        client
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "rh", "version": "0.1.0" }
                }),
            )
            .await?;
        client.notify("notifications/initialized", json!({})).await?;
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
        self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
        .await
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.0.next_id.fetch_add(1, Ordering::Relaxed);
        self.0.transport.request(id, method, params).await
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.0.transport.notify(method, params).await
    }
}

// ---------- tool bridge ----------

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
    let client = McpClient::connect(config).await?;
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
