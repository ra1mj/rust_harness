//! rh-web — the browser front end for the harness.
//!
//! Serves a single-page app and a WebSocket that streams a session's live
//! [`SessionEvent`]s. Each WebSocket connection owns its own session; typing
//! a task drives one turn and streams the transcript back in real time.
//! Models can be added and selected at runtime via `/api/models`.

mod model_catalog;
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

use rh_agent::ModelConfig;
use rh_core::Context;
use rh_tool::{ToolCallContext, ToolRegistry};

use model_catalog::ModelCatalog;

const INDEX: &str = include_str!("index.html");

/// Shared state for all routes.
#[derive(Clone)]
struct AppState {
    ctx: Context,
    plugins: Vec<String>,
    catalog: Arc<ModelCatalog>,
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
        catalog: Arc::new(ModelCatalog::load(models_file)),
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/api/tools", get(tools))
        .route("/api/config", get(config))
        .route("/api/models", get(list_models).post(upsert_model))
        .route("/api/models/active", post(set_active_model))
        .route("/api/models/{id}", delete(delete_model))
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

async fn list_models(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "models": state.catalog.list(),
        "active": state.catalog.active_id(),
    }))
}

async fn upsert_model(
    State(state): State<AppState>,
    Json(config): Json<ModelConfig>,
) -> Result<Json<Value>, (StatusCode, String)> {
    state
        .catalog
        .upsert(config)
        .map(|model| {
            Json(json!({
                "models": state.catalog.list(),
                "active": state.catalog.active_id(),
                "added": model.id,
            }))
        })
        .map_err(|err| (StatusCode::BAD_REQUEST, err.to_string()))
}

async fn set_active_model(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let id = body
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "缺少 id".to_string()))?;
    state
        .catalog
        .set_active(id)
        .map(|_| Json(json!({ "active": id })))
        .map_err(|err| (StatusCode::BAD_REQUEST, err.to_string()))
}

async fn delete_model(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    state
        .catalog
        .remove(&id)
        .map(|_| Json(json!({ "models": state.catalog.list(), "active": state.catalog.active_id() })))
        .map_err(|err| (StatusCode::NOT_FOUND, err.to_string()))
}
