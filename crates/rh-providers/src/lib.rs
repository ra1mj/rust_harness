//! rh-providers — the LLM layer: an OpenAI-compatible adapter plus a model
//! hub for managing multiple providers and their discovered models.
//!
//! Mirrors DeepSeek Harness's LLM seam: an **adapter** is bound to a provider
//! *route* (endpoint + credential), while the **model** is selected per
//! request (`ModelRequest.model`). The [`ModelHub`] registers providers,
//! discovers their models (`GET /models`), and tracks the active
//! provider/model pair.

use std::collections::{HashMap, VecDeque};
use std::env;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use rh_agent::{
    FinishReason, ModelEvent, ModelProvider, ModelRequest, ModelRole, ModelStream, ModelToolCall,
};

/// A provider route: an OpenAI-compatible endpoint plus its credential.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub api_key: String,
}

/// One model a provider exposes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Model id passed to the API.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Owning provider route id.
    #[serde(default)]
    pub provider: String,
}

/// An OpenAI-compatible HTTP adapter, bound to one provider route.
pub struct OpenAiCompatibleProvider {
    base_url: String,
    api_key: String,
    client: reqwest::Client,
}

impl OpenAiCompatibleProvider {
    pub fn new(base_url: String, api_key: String) -> Self {
        Self {
            base_url,
            api_key,
            client: reqwest::Client::new(),
        }
    }

    /// Build from `RH_API_KEY` / `RH_BASE_URL` environment vars.
    pub fn from_env() -> Result<Self> {
        let api_key = env::var("RH_API_KEY").map_err(|_| anyhow!("RH_API_KEY not set"))?;
        let base_url =
            env::var("RH_BASE_URL").unwrap_or_else(|_| "https://api.deepseek.com".to_string());
        Ok(Self::new(base_url, api_key))
    }

    /// Discover the provider's models via `GET {base_url}/models`.
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let url = format!("{}/models", self.base_url.trim_end_matches('/'));
        let response: Value = self
            .client
            .get(&url)
            .bearer_auth(&self.api_key)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let data = response["data"].as_array().cloned().unwrap_or_default();
        Ok(data
            .iter()
            .filter_map(|m| {
                let id = m["id"].as_str()?.to_string();
                Some(ModelInfo {
                    id: id.clone(),
                    name: id,
                    provider: String::new(),
                })
            })
            .collect())
    }
}

#[async_trait]
impl ModelProvider for OpenAiCompatibleProvider {
    async fn stream(&self, request: ModelRequest) -> Result<ModelStream> {
        let body = self.chat_body(&request, true);
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?
            .error_for_status()?;

        // True SSE streaming: parse `data:` lines from the response body as
        // they arrive, emitting one `ModelEvent` per delta — no fixed-size
        // chunking and no artificial typing pace.
        Ok(Box::pin(sse_events(response.bytes_stream())))
    }
}

impl OpenAiCompatibleProvider {
    /// Build the OpenAI `chat/completions` request body (shared by streaming
    /// and any future non-streaming path).
    fn chat_body(&self, request: &ModelRequest, stream: bool) -> Value {
        let messages: Vec<Value> = request
            .messages
            .iter()
            .map(|m| {
                let mut obj = json!({ "role": role_str(m.role) });
                if let Some(content) = &m.content {
                    obj["content"] = json!(content);
                }
                if !m.tool_calls.is_empty() {
                    let calls: Vec<Value> = m
                        .tool_calls
                        .iter()
                        .map(|tc| {
                            json!({
                                "id": tc.id,
                                "type": "function",
                                "function": {
                                    "name": tc.name,
                                    "arguments": tc.arguments.to_string()
                                }
                            })
                        })
                        .collect();
                    obj["tool_calls"] = json!(calls);
                }
                if let Some(tci) = &m.tool_call_id {
                    obj["tool_call_id"] = json!(tci);
                }
                obj
            })
            .collect();

        let tools: Vec<Value> = request
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters
                    }
                })
            })
            .collect();

        // The model id comes from the request, not the adapter (dsh parity:
        // provider route = adapter, model = per-request selection).
        let mut body = json!({
            "model": request.model,
            "messages": messages,
            "tools": tools,
        });
        if stream {
            body["stream"] = json!(true);
        }
        body
    }
}

/// One in-flight tool call being assembled across SSE deltas.
struct ToolCallAcc {
    id: String,
    name: String,
    arguments: String,
}

/// Incremental SSE parser state.
struct SseState {
    src: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    buf: Vec<u8>,
    pending: VecDeque<ModelEvent>,
    tool_calls: Vec<ToolCallAcc>,
    finish_reason: FinishReason,
    finished: bool,
}

impl SseState {
    /// Drain complete `\n`-terminated lines from the buffer and process them.
    fn process_lines(&mut self) {
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line_bytes: Vec<u8> = self.buf.drain(..=pos).collect();
            let mut line = String::from_utf8_lossy(&line_bytes).into_owned();
            while line.ends_with('\n') || line.ends_with('\r') {
                line.pop();
            }
            self.handle_line(&line);
        }
    }

    fn handle_line(&mut self, line: &str) {
        if line.is_empty() {
            return;
        }
        let Some(data) = line.strip_prefix("data:") else {
            return; // ignore `event:`/`id:`/keep-alive comments
        };
        let data = data.trim_start();
        if data == "[DONE]" {
            self.finish();
            return;
        }
        if let Ok(value) = serde_json::from_str::<Value>(data) {
            self.handle_delta(&value);
        }
    }

    fn handle_delta(&mut self, value: &Value) {
        let Some(choice) = value["choices"].as_array().and_then(|c| c.first()) else {
            return;
        };
        if let Some(reason) = choice["finish_reason"].as_str() {
            self.finish_reason = match reason {
                "tool_calls" => FinishReason::ToolCalls,
                "length" => FinishReason::Length,
                _ => FinishReason::Stop,
            };
        }
        let delta = &choice["delta"];
        if let Some(text) = delta["reasoning_content"].as_str() {
            if !text.is_empty() {
                self.pending
                    .push_back(ModelEvent::Reasoning(text.to_string()));
            }
        }
        if let Some(text) = delta["content"].as_str() {
            if !text.is_empty() {
                self.pending.push_back(ModelEvent::Text(text.to_string()));
            }
        }
        if let Some(calls) = delta["tool_calls"].as_array() {
            for call in calls {
                let index = call["index"].as_u64().unwrap_or(0) as usize;
                while self.tool_calls.len() <= index {
                    self.tool_calls.push(ToolCallAcc {
                        id: String::new(),
                        name: String::new(),
                        arguments: String::new(),
                    });
                }
                let acc = &mut self.tool_calls[index];
                if let Some(id) = call["id"].as_str() {
                    acc.id = id.to_string();
                }
                let function = &call["function"];
                if let Some(name) = function["name"].as_str() {
                    if !name.is_empty() {
                        acc.name = name.to_string();
                    }
                }
                if let Some(args) = function["arguments"].as_str() {
                    acc.arguments.push_str(args);
                }
            }
        }
    }

    /// Flush accumulated tool calls and the terminal event.
    fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        for acc in &self.tool_calls {
            if !acc.name.is_empty() {
                let arguments = serde_json::from_str(&acc.arguments).unwrap_or(Value::Null);
                self.pending.push_back(ModelEvent::ToolCall(ModelToolCall {
                    id: acc.id.clone(),
                    name: acc.name.clone(),
                    arguments,
                }));
            }
        }
        self.pending.push_back(ModelEvent::Done(self.finish_reason));
    }
}

/// Turn a byte stream from `chat/completions?stream=true` into `ModelEvent`s.
fn sse_events(
    src: impl Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
) -> impl Stream<Item = ModelEvent> {
    let state = SseState {
        src: Box::pin(src),
        buf: Vec::new(),
        pending: VecDeque::new(),
        tool_calls: Vec::new(),
        finish_reason: FinishReason::Stop,
        finished: false,
    };
    futures::stream::unfold(state, |mut st| async move {
        loop {
            if let Some(event) = st.pending.pop_front() {
                return Some((event, st));
            }
            if st.finished {
                return None;
            }
            match st.src.next().await {
                Some(Ok(chunk)) => {
                    st.buf.extend_from_slice(&chunk);
                    st.process_lines();
                }
                Some(Err(_)) | None => st.finish(),
            }
        }
    })
}

/// On-disk hub state.
#[derive(Serialize, Deserialize, Default)]
struct HubFile {
    #[serde(default)]
    providers: Vec<ProviderConfig>,
    #[serde(default)]
    models: HashMap<String, Vec<ModelInfo>>,
    #[serde(default)]
    active_provider: Option<String>,
    #[serde(default)]
    active_model: Option<String>,
}

/// The model hub: register providers, discover their models, and track the
/// active provider/model pair. Persisted to a JSON file.
pub struct ModelHub {
    inner: RwLock<HubFile>,
    path: PathBuf,
    next_id: AtomicU64,
}

impl ModelHub {
    /// Load (or seed) the hub from `path`.
    pub fn load(path: PathBuf) -> Self {
        let file = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<HubFile>(&s).ok())
            .unwrap_or_default();

        let hub = Self {
            inner: RwLock::new(file),
            path,
            next_id: AtomicU64::new(0),
        };

        // Seed a provider from the environment when present.
        if env::var("RH_API_KEY").is_ok() {
            let id = "deepseek";
            let exists = hub.inner.read().expect("hub poisoned").providers.iter().any(|p| p.id == id);
            if !exists {
                let config = ProviderConfig {
                    id: id.to_string(),
                    name: "DeepSeek".to_string(),
                    base_url: env::var("RH_BASE_URL")
                        .unwrap_or_else(|_| "https://api.deepseek.com".to_string()),
                    api_key: env::var("RH_API_KEY").unwrap_or_default(),
                };
                let _ = hub.add_provider(config);
            }
        }

        let _ = hub.save();
        hub
    }

    fn save(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(&*self.inner.read().expect("hub poisoned"))?;
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        std::fs::write(&self.path, json)?;
        Ok(())
    }

    pub fn providers(&self) -> Vec<ProviderConfig> {
        self.inner.read().expect("hub poisoned").providers.clone()
    }

    pub fn provider(&self, id: &str) -> Option<ProviderConfig> {
        self.inner
            .read()
            .expect("hub poisoned")
            .providers
            .iter()
            .find(|p| p.id == id)
            .cloned()
    }

    pub fn models(&self, provider: &str) -> Vec<ModelInfo> {
        self.inner
            .read()
            .expect("hub poisoned")
            .models
            .get(provider)
            .cloned()
            .unwrap_or_default()
    }

    pub fn active_state(&self) -> (Option<String>, Option<String>) {
        let inner = self.inner.read().expect("hub poisoned");
        (inner.active_provider.clone(), inner.active_model.clone())
    }

    /// The active provider route + model.
    pub fn active(&self) -> Option<(ProviderConfig, ModelInfo)> {
        let inner = self.inner.read().expect("hub poisoned");
        let provider_id = inner.active_provider.as_ref()?;
        let model_id = inner.active_model.as_ref()?;
        let provider = inner.providers.iter().find(|p| &p.id == provider_id)?;
        let model = inner
            .models
            .get(provider_id)?
            .iter()
            .find(|m| &m.id == model_id)?;
        Some((provider.clone(), model.clone()))
    }

    /// Insert or replace a provider; generates an id when empty.
    pub fn add_provider(&self, mut config: ProviderConfig) -> Result<ProviderConfig> {
        if config.id.trim().is_empty() {
            config.id = format!("p-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        }
        if config.name.trim().is_empty() {
            config.name = config.id.clone();
        }
        {
            let mut inner = self.inner.write().expect("hub poisoned");
            match inner.providers.iter_mut().find(|p| p.id == config.id) {
                Some(existing) => *existing = config.clone(),
                None => inner.providers.push(config.clone()),
            }
        }
        self.save()?;
        Ok(config)
    }

    pub fn remove_provider(&self, id: &str) -> Result<()> {
        {
            let mut inner = self.inner.write().expect("hub poisoned");
            inner.providers.retain(|p| p.id != id);
            inner.models.remove(id);
            if inner.active_provider.as_deref() == Some(id) {
                inner.active_provider = None;
                inner.active_model = None;
            }
        }
        self.save()
    }

    pub fn set_active(&self, provider: &str, model: &str) -> Result<()> {
        {
            let mut inner = self.inner.write().expect("hub poisoned");
            if !inner.providers.iter().any(|p| p.id == provider) {
                return Err(anyhow!("provider {provider} not found"));
            }
            if !inner
                .models
                .get(provider)
                .map(|ms| ms.iter().any(|m| m.id == model))
                .unwrap_or(false)
            {
                return Err(anyhow!("model {model} not found under provider {provider}"));
            }
            inner.active_provider = Some(provider.to_string());
            inner.active_model = Some(model.to_string());
        }
        self.save()
    }

    /// Interrogate a provider's `GET /models` and store the result.
    pub async fn discover_models(&self, provider_id: &str) -> Result<Vec<ModelInfo>> {
        let provider = self
            .provider(provider_id)
            .ok_or_else(|| anyhow!("provider {provider_id} not found"))?;
        let adapter = OpenAiCompatibleProvider::new(provider.base_url, provider.api_key);
        let mut models = adapter.list_models().await?;
        for model in &mut models {
            model.provider = provider_id.to_string();
        }
        {
            let mut inner = self.inner.write().expect("hub poisoned");
            inner.models.insert(provider_id.to_string(), models.clone());
        }
        self.save()?;
        Ok(models)
    }

    /// Build the adapter for a provider route.
    pub fn build_provider_for(&self, provider_id: &str) -> Result<Arc<dyn ModelProvider>> {
        let provider = self
            .provider(provider_id)
            .ok_or_else(|| anyhow!("provider {provider_id} not found"))?;
        Ok(Arc::new(OpenAiCompatibleProvider::new(
            provider.base_url,
            provider.api_key,
        )))
    }
}

fn role_str(role: ModelRole) -> &'static str {
    match role {
        ModelRole::System => "system",
        ModelRole::User => "user",
        ModelRole::Assistant => "assistant",
        ModelRole::Tool => "tool",
    }
}
