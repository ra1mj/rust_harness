//! OpenAI-compatible HTTP model provider (DeepSeek by default).
//!
//! Enabled by the `http` cargo feature. Reads:
//! * `RH_API_KEY` (required)
//! * `RH_BASE_URL` (default `https://api.deepseek.com`)
//! * `RH_MODEL` (default `deepseek-chat`)
//!
//! The request is non-streaming; the full completion is then re-emitted
//! through the streaming [`ModelStream`] interface (text chunked for live
//! rendering). True SSE token streaming is a planned follow-up.

use std::env;

use async_trait::async_trait;
use serde_json::{json, Value};

use rh_agent::{
    FinishReason, ModelEvent, ModelProvider, ModelRequest, ModelRole, ModelStream, ModelToolCall,
};

/// A model provider speaking the OpenAI `/chat/completions` shape.
pub struct OpenAiCompatibleProvider {
    base_url: String,
    api_key: String,
    model: String,
    client: reqwest::Client,
}

impl OpenAiCompatibleProvider {
    pub fn from_env() -> anyhow::Result<Self> {
        let api_key = env::var("RH_API_KEY").map_err(|_| anyhow::anyhow!("RH_API_KEY not set"))?;
        let base_url =
            env::var("RH_BASE_URL").unwrap_or_else(|_| "https://api.deepseek.com".to_string());
        let model = env::var("RH_MODEL").unwrap_or_else(|_| "deepseek-chat".to_string());
        Ok(Self {
            base_url,
            api_key,
            model,
            client: reqwest::Client::new(),
        })
    }
}

#[async_trait]
impl ModelProvider for OpenAiCompatibleProvider {
    async fn stream(&self, request: ModelRequest) -> anyhow::Result<ModelStream> {
        let (content, tool_calls, finish_reason) = self.complete_once(&request).await?;

        let mut events: Vec<ModelEvent> = chunk_text(&content);
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
    ) -> anyhow::Result<(String, Vec<ModelToolCall>, FinishReason)> {
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

        let body = json!({
            "model": self.model,
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
            .ok_or_else(|| anyhow::anyhow!("no choices in model response"))?;
        let message = &choice["message"];

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

        Ok((content, tool_calls, finish_reason))
    }
}

/// Split text into fixed-size [`ModelEvent::Text`] chunks for live rendering.
fn chunk_text(text: &str) -> Vec<ModelEvent> {
    const CHUNK: usize = 6;
    if text.is_empty() {
        return Vec::new();
    }
    let chars: Vec<char> = text.chars().collect();
    chars
        .chunks(CHUNK)
        .map(|chunk| ModelEvent::Text(chunk.iter().collect()))
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
