//! The WebSocket endpoint: one session per connection, live event streaming.

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use futures::{SinkExt, StreamExt};
use serde_json::json;

use rh_agent::{AgentBuilder, AgentDefinition};
use rh_session::{SessionEvent, SessionStore};

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
                    json!({ "type": "error", "error": "no session store registered" }).to_string().into(),
                ))
                .await;
            return;
        }
    };
    let session = store.create_fresh();

    let definition = AgentDefinition {
        name: "rh-web".to_string(),
        model: "mock".to_string(),
        system_prompt: "You are a Rust harness agent. Use tools when useful.".to_string(),
        tool_ids: Vec::new(),
        max_steps: 8,
    };
    let agent = match AgentBuilder::new(state.ctx.clone(), definition).build(session.clone()) {
        Ok(agent) => Arc::new(agent),
        Err(err) => {
            let _ = sender
                .send(Message::Text(
                    json!({ "type": "error", "error": err.to_string() }).to_string().into(),
                ))
                .await;
            return;
        }
    };

    // Greet the client with the session id.
    let _ = sender
        .send(Message::Text(
            json!({ "type": "hello", "session_id": session.id() })
                .to_string()
                .into(),
        ))
        .await;

    let mut live = session.subscribe();
    let mut running = false;

    loop {
        tokio::select! {
            event = live.recv() => {
                match event {
                    Ok(event) => {
                        if matches!(event, SessionEvent::TurnEnd { .. }) {
                            running = false;
                        }
                        let text = serde_json::to_string(&event).unwrap_or_default();
                        if sender.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                    // A slow client that lags beyond the buffer; keep going.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => break,
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
                        let agent = Arc::clone(&agent);
                        tokio::spawn(async move {
                            let _ = agent.run(&input).await;
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
