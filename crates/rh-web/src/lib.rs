//! rh-web — the browser front end for the harness.
//!
//! Serves a single-page app and a WebSocket that streams a session's live
//! [`SessionEvent`]s. Each WebSocket connection owns its own session; typing
//! a task drives one turn and streams the transcript back in real time.
//! Providers and models are managed at runtime via the `/api/providers` /
//! `/api/active` endpoints (backed by [`ModelHub`]).

mod ws;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Html;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use rh_core::Context;
use rh_providers::{ModelHub, ProviderConfig};
use rh_tool::{ToolCallContext, ToolRegistry};

const INDEX: &str = include_str!("index.html");

/// Shared state for all routes.
#[derive(Clone)]
struct AppState {
    ctx: Context,
    plugins: Vec<String>,
    hub: Arc<ModelHub>,
}

/// Serve the web UI until the process is stopped.
pub async fn serve(
    ctx: Context,
    plugins: Vec<String>,
    addr: SocketAddr,
    models_file: PathBuf,
) -> anyhow::Result<()> {
    let state = AppState {
        ctx,
        plugins,
        hub: Arc::new(ModelHub::load(models_file)),
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/api/tools", get(tools))
        .route("/api/config", get(config))
        .route("/api/providers", get(list_providers).post(add_provider))
        .route("/api/providers/{id}/discover", post(discover_models))
        .route("/api/providers/{id}", delete(remove_provider))
        .route("/api/active", post(set_active))
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

/// The full hub state: providers, their models, and the active pair.
fn hub_state(hub: &ModelHub) -> Value {
    let providers = hub.providers();
    let mut models = serde_json::Map::new();
    for provider in &providers {
        models.insert(
            provider.id.clone(),
            serde_json::to_value(hub.models(&provider.id)).unwrap_or(Value::Array(vec![])),
        );
    }
    let (active_provider, active_model) = hub.active_state();
    json!({
        "providers": providers,
        "models": models,
        "active": { "provider": active_provider, "model": active_model },
    })
}

async fn list_providers(State(state): State<AppState>) -> Json<Value> {
    Json(hub_state(&state.hub))
}

async fn add_provider(
    State(state): State<AppState>,
    Json(config): Json<ProviderConfig>,
) -> Result<Json<Value>, (StatusCode, String)> {
    state
        .hub
        .add_provider(config)
        .map(|_| Json(hub_state(&state.hub)))
        .map_err(|err| (StatusCode::BAD_REQUEST, err.to_string()))
}

async fn discover_models(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    state
        .hub
        .discover_models(&id)
        .await
        .map(|_| Json(hub_state(&state.hub)))
        .map_err(|err| (StatusCode::BAD_REQUEST, err.to_string()))
}

async fn remove_provider(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    state
        .hub
        .remove_provider(&id)
        .map(|_| Json(hub_state(&state.hub)))
        .map_err(|err| (StatusCode::NOT_FOUND, err.to_string()))
}

async fn set_active(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let provider = body
        .get("provider")
        .and_then(Value::as_str)
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "缺少 provider".to_string()))?;
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "缺少 model".to_string()))?;
    state
        .hub
        .set_active(provider, model)
        .map(|_| Json(hub_state(&state.hub)))
        .map_err(|err| (StatusCode::BAD_REQUEST, err.to_string()))
}
