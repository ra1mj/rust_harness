//! The WebSocket endpoint: one session per connection, live event streaming.
//!
//! Each turn rebuilds the model provider from the catalog's *current*
//! active model, so switching models in the settings takes effect on the
//! next message without reconnecting.

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use futures::{SinkExt, StreamExt};
use serde_json::json;
use tokio::sync::mpsc;

use rh_agent::{AgentBuilder, AgentDefinition};
use rh_session::{Session, SessionStore};

use crate::model_catalog::build_provider;
use crate::AppState;

pub async fn upgrade(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| handle(socket, state))
}

async fn handle(socket: WebSocket, state: AppState) {
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
    let session = store.create_fresh();

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
                            let result = run_turn(&state, session, &input).await.map_err(|e| e.to_string());
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

/// Run one turn with the catalog's currently-active model.
async fn run_turn(state: &AppState, session: Arc<Session>, input: &str) -> anyhow::Result<()> {
    let config = state
        .catalog
        .active()
        .ok_or_else(|| anyhow::anyhow!("没有可用的模型，请先在设置里添加"))?;
    let provider = build_provider(&config)?;
    let definition = AgentDefinition {
        name: "rh-web".to_string(),
        model: config.model.clone().unwrap_or_else(|| "mock".to_string()),
        system_prompt: "You are a Rust harness agent. Use tools when useful.".to_string(),
        tool_ids: Vec::new(),
        max_steps: 8,
    };
    let agent = AgentBuilder::new(state.ctx.clone(), definition)
        .with_model(provider)
        .build(session)?;
    agent.run(input).await?;
    Ok(())
}
