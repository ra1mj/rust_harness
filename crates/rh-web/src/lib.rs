//! rh-web — the browser front end for the harness.
//!
//! Serves a single-page app, a WebSocket that streams a session's live
//! [`SessionEvent`]s, and REST endpoints for workspace management: sessions
//! (create/switch/rename/delete/export), tasks, and model providers.

mod mcp;
mod ws;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::http::StatusCode;
use axum::response::{Html, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use rh_core::Context;
use rh_mcp::McpServerConfig;
use rh_providers::{ModelHub, ProviderConfig};
use rh_session::{workspace_context, SessionStore, TaskItem};
use rh_tool::{ToolCallContext, ToolRegistry};
use rh_tools::SkillStore;

use mcp::McpHub;

const INDEX: &str = include_str!("index.html");

/// Shared state for all routes.
#[derive(Clone)]
struct AppState {
    ctx: Context,
    plugins: Vec<String>,
    hub: Arc<ModelHub>,
    mcp: Arc<McpHub>,
}

/// Serve the web UI until the process is stopped.
pub async fn serve(
    ctx: Context,
    plugins: Vec<String>,
    addr: SocketAddr,
    models_file: PathBuf,
    data_dir: PathBuf,
    mcp_file: PathBuf,
    skills_dir: PathBuf,
) -> anyhow::Result<()> {
    // Replace the in-memory session store with a persistent one (one JSON
    // file per session under `data_dir`). Held for the server lifetime.
    let store = Arc::new(SessionStore::persistent(data_dir, Some(ctx.clone())));
    let _store_registration = ctx.provide_named("SessionStore", store);

    // Replace the built-in-only skill store with one that also loads user
    // skills from `skills_dir`.
    let _skills_registration = ctx.provide_named("SkillStore", SkillStore::new(Some(skills_dir)));

    let mcp = McpHub::load(mcp_file);
    mcp.connect_all(&ctx).await;

    let state = AppState {
        ctx,
        plugins,
        hub: Arc::new(ModelHub::load(models_file)),
        mcp,
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/api/tools", get(tools))
        .route("/api/skills", get(skills))
        .route("/api/config", get(config))
        .route("/api/providers", get(list_providers).post(add_provider))
        .route("/api/providers/{id}/discover", post(discover_models))
        .route("/api/providers/{id}", delete(remove_provider))
        .route("/api/active", post(set_active))
        .route("/api/mcp", get(list_mcp).post(add_mcp))
        .route("/api/mcp/{name}", delete(remove_mcp))
        .route("/api/sessions", get(list_sessions).post(create_session))
        .route(
            "/api/sessions/{id}",
            get(get_session).patch(rename_session).delete(delete_session),
        )
        .route("/api/sessions/{id}/export", get(export_session))
        .route("/api/sessions/{id}/tasks", get(list_tasks).post(add_task))
        .route("/api/sessions/{id}/tasks/{task_id}", patch(set_task_done))
        .route("/api/sessions/{id}/workspace", get(get_workspace).post(set_workspace))
        .route("/api/sessions/{id}/mode", get(get_mode).post(set_mode))
        .route("/ws", get(ws::upgrade))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("rh web listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

fn store(state: &AppState) -> Arc<SessionStore> {
    state
        .ctx
        .service::<SessionStore>()
        .expect("no session store registered")
}

async fn index() -> Html<&'static str> {
    Html(INDEX)
}

async fn skills(State(state): State<AppState>) -> Json<Value> {
    let store = state
        .ctx
        .service::<SkillStore>()
        .expect("no skill store registered");
    let skills: Vec<Value> = store
        .list()
        .into_iter()
        .map(|(name, description)| json!({ "name": name, "description": description }))
        .collect();
    Json(json!({ "skills": skills }))
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

// ---------- model providers ----------

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

// ---------- MCP servers ----------

async fn list_mcp(State(state): State<AppState>) -> Json<Value> {
    Json(json!({ "servers": state.mcp.list() }))
}

async fn add_mcp(
    State(state): State<AppState>,
    Json(config): Json<McpServerConfig>,
) -> Result<Json<Value>, (StatusCode, String)> {
    state
        .mcp
        .add(&state.ctx, config)
        .await
        .map(|_| Json(json!({ "servers": state.mcp.list() })))
        .map_err(|err| (StatusCode::BAD_REQUEST, err.to_string()))
}

async fn remove_mcp(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    state
        .mcp
        .remove(&name)
        .await
        .map(|_| Json(json!({ "servers": state.mcp.list() })))
        .map_err(|err| (StatusCode::NOT_FOUND, err.to_string()))
}

// ---------- sessions ----------

async fn list_sessions(State(state): State<AppState>) -> Json<Value> {
    Json(json!({ "sessions": store(&state).list() }))
}

async fn create_session(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let title = body
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("新会话");
    let session = store(&state).create(title);
    Json(json!({ "sessions": store(&state).list(), "created": session.id() }))
}

async fn get_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let session = store(&state)
        .get(&id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, "session not found".to_string()))?;
    Ok(Json(json!(session.to_record())))
}

async fn rename_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let title = body
        .get("title")
        .and_then(Value::as_str)
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "缺少 title".to_string()))?;
    store(&state)
        .rename(&id, title)
        .map(|_| Json(json!({ "sessions": store(&state).list() })))
        .map_err(|err| (StatusCode::NOT_FOUND, err.to_string()))
}

async fn delete_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    store(&state)
        .remove(&id)
        .map(|_| Json(json!({ "sessions": store(&state).list() })))
        .map_err(|err| (StatusCode::NOT_FOUND, err.to_string()))
}

async fn export_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, (StatusCode, String)> {
    let session = store(&state)
        .get(&id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, "session not found".to_string()))?;
    let format = params.get("format").map(String::as_str).unwrap_or("markdown");
    match format {
        "json" => Ok(download(
            &format!("{}.json", session.id()),
            session.to_json().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
            "application/json",
        )),
        _ => Ok(download(
            &format!("{}.md", session.id()),
            session.to_markdown(),
            "text/markdown; charset=utf-8",
        )),
    }
}

fn download(filename: &str, content: String, content_type: &str) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .body(Body::from(content))
        .unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "body error").into_response())
}

use axum::response::IntoResponse;

// ---------- tasks ----------

async fn list_tasks(State(state): State<AppState>, Path(id): Path<String>) -> Json<Value> {
    let tasks: Vec<TaskItem> = store(&state)
        .get(&id)
        .map(|s| s.tasks())
        .unwrap_or_default();
    Json(json!({ "tasks": tasks }))
}

async fn add_task(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let title = body
        .get("title")
        .and_then(Value::as_str)
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "缺少 title".to_string()))?;
    let session = store(&state)
        .get(&id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, "session not found".to_string()))?;
    let item = session.add_task(title);
    store(&state)
        .save(&session)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "tasks": session.tasks(), "added": item.id })))
}

async fn get_workspace(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let session = store(&state)
        .get(&id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, "session not found".to_string()))?;
    let root = session.workspace();
    Ok(Json(json!({
        "root": root.display().to_string(),
        "context": workspace_context(&root),
    })))
}

async fn get_mode(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let session = store(&state)
        .get(&id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, "session not found".to_string()))?;
    Ok(Json(json!({
        "mode": session.work_mode(),
        "phase": session.workflow_phase(),
    })))
}

async fn set_mode(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let mode = body
        .get("mode")
        .and_then(Value::as_str)
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "缺少 mode".to_string()))?;
    let session = store(&state)
        .get(&id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, "session not found".to_string()))?;
    session.set_work_mode(mode);
    store(&state)
        .save(&session)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({
        "mode": session.work_mode(),
        "phase": session.workflow_phase(),
    })))
}

async fn set_workspace(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let root = body
        .get("root")
        .and_then(Value::as_str)
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "缺少 root".to_string()))?;
    let session = store(&state)
        .get(&id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, "session not found".to_string()))?;
    let root = session.set_workspace(PathBuf::from(root));
    store(&state)
        .save(&session)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({
        "root": root.display().to_string(),
        "context": workspace_context(&root),
    })))
}

async fn set_task_done(
    State(state): State<AppState>,
    Path((id, task_id)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let done = body.get("done").and_then(Value::as_bool).unwrap_or(false);
    let session = store(&state)
        .get(&id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, "session not found".to_string()))?;
    session.set_task_done(&task_id, done);
    store(&state)
        .save(&session)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "tasks": session.tasks() })))
}
