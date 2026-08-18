//! rh-providers — the LLM layer: an OpenAI-compatible adapter plus a model
//! hub for managing multiple providers and their discovered models.
//!
//! Mirrors DeepSeek Harness's LLM seam: an **adapter** is bound to a provider
//! *route* (endpoint + credential), while the **model** is selected per
//! request (`ModelRequest.model`). The [`ModelHub`] registers providers,
//! discovers their models (`GET /models`), and tracks the active
//! provider/model pair.

use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
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
        let (reasoning, content, tool_calls, finish_reason) = self.complete_once(&request).await?;

        let mut events: Vec<ModelEvent> = chunks(&reasoning)
            .into_iter()
            .map(ModelEvent::Reasoning)
            .collect();
        events.extend(chunks(&content).into_iter().map(ModelEvent::Text));
        for call in tool_calls {
            events.push(ModelEvent::ToolCall(call));
        }
        events.push(ModelEvent::Done(finish_reason));

        Ok(Box::pin(futures::stream::iter(events)))
    }
}

impl OpenAiCompatibleProvider {
    async fn complete_once(
        &self,
        request: &ModelRequest,
    ) -> Result<(String, String, Vec<ModelToolCall>, FinishReason)> {
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
        let body = json!({
            "model": request.model,
            "messages": messages,
            "tools": tools,
        });

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let response: Value = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let choice = response["choices"]
            .as_array()
            .and_then(|choices| choices.first())
            .cloned()
            .ok_or_else(|| anyhow!("no choices in model response"))?;
        let message = &choice["message"];

        let reasoning = message["reasoning_content"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let content = message["content"].as_str().unwrap_or_default().to_string();
        let tool_calls: Vec<ModelToolCall> = message["tool_calls"]
            .as_array()
            .map(|calls| {
                calls
                    .iter()
                    .filter_map(|tc| {
                        let function = &tc["function"];
                        Some(ModelToolCall {
                            id: tc["id"].as_str().unwrap_or_default().to_string(),
                            name: function["name"].as_str()?.to_string(),
                            arguments: serde_json::from_str(
                                function["arguments"].as_str().unwrap_or("{}"),
                            )
                            .unwrap_or(Value::Null),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let finish_reason = match choice["finish_reason"].as_str() {
            Some("tool_calls") => FinishReason::ToolCalls,
            Some("length") => FinishReason::Length,
            _ => FinishReason::Stop,
        };

        Ok((reasoning, content, tool_calls, finish_reason))
    }
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

/// Split text into fixed-size string chunks for live rendering.
fn chunks(text: &str) -> Vec<String> {
    const CHUNK: usize = 6;
    if text.is_empty() {
        return Vec::new();
    }
    let chars: Vec<char> = text.chars().collect();
    chars
        .chunks(CHUNK)
        .map(|chunk| chunk.iter().collect())
        .collect()
}

fn role_str(role: ModelRole) -> &'static str {
    match role {
        ModelRole::System => "system",
        ModelRole::User => "user",
        ModelRole::Assistant => "assistant",
        ModelRole::Tool => "tool",
    }
}
