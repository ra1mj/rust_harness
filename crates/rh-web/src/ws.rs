//! The WebSocket endpoint: one session per connection, live event streaming.
//!
//! Each turn rebuilds the model provider from the catalog's *current*
//! active model, so switching models in the settings takes effect on the
//! next message without reconnecting.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use futures::{SinkExt, StreamExt};
use serde_json::json;
use tokio::sync::mpsc;

use rh_agent::{AgentBuilder, AgentDefinition};
use rh_session::{workspace_context, Session, SessionStore};

use crate::AppState;

pub async fn upgrade(
    ws: WebSocketUpgrade,
    Query(params): Query<HashMap<String, String>>,
    State(state): State<AppState>,
) -> Response {
    let session_id = params.get("session").cloned();
    ws.on_upgrade(move |socket| handle(socket, state, session_id))
}

async fn handle(socket: WebSocket, state: AppState, session_id: Option<String>) {
    let (mut sender, mut receiver) = socket.split();

    let store = match state.ctx.service::<SessionStore>() {
        Some(store) => store,
        None => {
            let _ = sender
                .send(Message::Text(
                    json!({ "type": "error", "error": "未注册会话存储" })
                        .to_string()
                        .into(),
                ))
                .await;
            return;
        }
    };
    // Attach to the requested session, or start a fresh one.
    let session = match session_id {
        Some(id) => store.get(&id).unwrap_or_else(|| store.create("新会话")),
        None => store.create("新会话"),
    };

    let _ = sender
        .send(Message::Text(
            json!({ "type": "hello", "session_id": session.id() })
                .to_string()
                .into(),
        ))
        .await;

    let mut live = session.subscribe();
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<Result<(), String>>();
    let mut running = false;

    loop {
        tokio::select! {
            event = live.recv() => {
                match event {
                    Ok(event) => {
                        let text = serde_json::to_string(&event).unwrap_or_default();
                        if sender.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => break,
                }
            }
            done = done_rx.recv() => {
                match done {
                    Some(Ok(())) => running = false,
                    Some(Err(err)) => {
                        running = false;
                        let frame = json!({ "type": "error", "error": err }).to_string();
                        if sender.send(Message::Text(frame.into())).await.is_err() {
                            break;
                        }
                    }
                    None => {}
                }
            }
            message = receiver.next() => {
                match message {
                    Some(Ok(Message::Text(bytes))) => {
                        let text: &str = &bytes;
                        let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
                            continue;
                        };
                        if value.get("type").and_then(|t| t.as_str()) != Some("input") {
                            continue;
                        }
                        let input = value.get("text").and_then(|t| t.as_str()).unwrap_or("").trim().to_string();
                        if input.is_empty() || running {
                            continue;
                        }
                        running = true;
                        let session = Arc::clone(&session);
                        let state = state.clone();
                        let done_tx = done_tx.clone();
                        tokio::spawn(async move {
                            // Auto-title an untitled session from its first message.
                            if session.title() == "新会话" {
                                let title: String = input.chars().take(30).collect();
                                session.set_title(if title.trim().is_empty() { "新会话".to_string() } else { title });
                            }
                            let result = run_turn(&state, session.clone(), &input).await.map_err(|e| e.to_string());
                            // Persist the transcript after the turn settles.
                            if let Some(store) = state.ctx.service::<SessionStore>() {
                                let _ = store.save(&session);
                            }
                            let _ = done_tx.send(result);
                        });
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
        }
    }
}

/// Run one turn with the hub's currently-active provider + model.
async fn run_turn(state: &AppState, session: Arc<Session>, input: &str) -> anyhow::Result<()> {
    let (provider_config, model_info) = state
        .hub
        .active()
        .ok_or_else(|| anyhow::anyhow!("没有可用的模型，请先在设置里添加并选择"))?;
    let provider = state.hub.build_provider_for(&provider_config.id)?;
    // Register the active provider on the context so the agent AND any
    // subagents it spawns resolve the same model (dsh: model adapter is a
    // plugin). Held for this turn only.
    let _registration = state.ctx.provide_named("ModelProvider", provider);

    // Inject workspace context (root, contents, git) into the system prompt.
    let workspace_summary = workspace_context(&session.workspace());
    let system_prompt = format!(
        "You are a Rust harness agent. Use tools when useful.\n\n{workspace_summary}"
    );

    let definition = AgentDefinition {
        name: "rh-web".to_string(),
        model: model_info.id.clone(),
        system_prompt,
        tool_ids: Vec::new(),
        max_steps: 8,
    };
    let agent = AgentBuilder::new(state.ctx.clone(), definition).build(session)?;
    agent.run(input).await?;
    Ok(())
}
