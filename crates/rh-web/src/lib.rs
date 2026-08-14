//! rh-web — the browser front end for the harness.
//!
//! Serves a single-page app and a WebSocket that streams a session's live
//! [`SessionEvent`]s. Each WebSocket connection owns its own session; typing
//! a task drives one turn and streams the transcript back in real time.

mod ws;

use std::net::SocketAddr;

use axum::extract::State;
use axum::response::Html;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};

use rh_core::Context;
use rh_tool::{ToolCallContext, ToolRegistry};

const INDEX: &str = include_str!("index.html");

/// Shared state for all routes.
#[derive(Clone)]
struct AppState {
    ctx: Context,
    plugins: Vec<String>,
}

/// Serve the web UI until the process is stopped.
pub async fn serve(
    ctx: Context,
    plugins: Vec<String>,
    addr: SocketAddr,
) -> anyhow::Result<()> {
    let state = AppState { ctx, plugins };

    let app = Router::new()
        .route("/", get(index))
        .route("/api/tools", get(tools))
        .route("/api/config", get(config))
        .route("/ws", get(ws::upgrade))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("rh web listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn index() -> Html<&'static str> {
    Html(INDEX)
}

async fn tools(State(state): State<AppState>) -> Json<Value> {
    let registry = state
        .ctx
        .service::<ToolRegistry>()
        .expect("no tool registry registered");
    let listing_ctx = ToolCallContext::new(state.ctx.clone(), "web", "web");
    let tools: Vec<Value> = registry
        .list(&listing_ctx)
        .into_iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters,
            })
        })
        .collect();
    Json(json!({ "tools": tools }))
}

async fn config(State(state): State<AppState>) -> Json<Value> {
    let registry = state
        .ctx
        .service::<ToolRegistry>()
        .expect("no tool registry registered");
    let listing_ctx = ToolCallContext::new(state.ctx.clone(), "web", "web");
    let tools: Vec<String> = registry
        .list(&listing_ctx)
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    Json(json!({
        "plugins": state.plugins,
        "services": state.ctx.service_names(),
        "tools": tools,
    }))
}
