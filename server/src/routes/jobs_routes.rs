use std::collections::HashMap;
use std::time::Duration;

use axum::extract::{Path, Query, State, WebSocketUpgrade};
use axum::response::Response;
use axum::Json;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use uuid::Uuid;

use crate::auth::Role;
use crate::error::{ApiError, ApiResult};
use crate::jobs::{CreateJobRequest, JobStatus};
use crate::routes::{ws_auth, AuthCtx};
use crate::state::{role_str, SharedState};

/// Reject operations on jobs owned by another user before touching runtime
/// state, so the API returns 403 (not a generic conflict).
fn ensure_job_access(state: &SharedState, ctx: &AuthCtx, id: &Uuid) -> ApiResult<()> {
    let view = state.jobs.get(id).ok_or(ApiError::NotFound)?;
    if ctx.role == Role::Admin || view.owner.is_empty() || view.owner == ctx.username {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

pub async fn list(
    State(state): State<SharedState>,
    ctx: crate::routes::AuthCtx,
) -> ApiResult<Json<serde_json::Value>> {
    ctx.require(Role::Viewer)?;
    let jobs = state.jobs.list();
    Ok(Json(json!({ "jobs": jobs })))
}

pub async fn create(
    State(state): State<SharedState>,
    ctx: crate::routes::AuthCtx,
    Json(body): Json<CreateJobRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    ctx.require(Role::Operator)?;
    let view = state
        .jobs
        .create(&ctx.username, role_str(&ctx.role), body)
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    Ok(Json(serde_json::to_value(view).unwrap_or_default()))
}

pub async fn get_job(
    State(state): State<SharedState>,
    ctx: crate::routes::AuthCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    ctx.require(Role::Viewer)?;
    let view = state.jobs.get(&id).ok_or(ApiError::NotFound)?;
    Ok(Json(json!({ "job": view })))
}

pub async fn start_job(
    State(state): State<SharedState>,
    ctx: crate::routes::AuthCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    ctx.require(Role::Operator)?;
    ensure_job_access(&state, &ctx, &id)?;
    let view = state
        .jobs
        .start(&ctx.username, role_str(&ctx.role), &id)
        .await
        .map_err(ApiError::Conflict)?;
    Ok(Json(json!({ "job": view })))
}

pub async fn stop_job(
    State(state): State<SharedState>,
    ctx: crate::routes::AuthCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    ctx.require(Role::Operator)?;
    ensure_job_access(&state, &ctx, &id)?;
    let view = state
        .jobs
        .stop(&ctx.username, role_str(&ctx.role), &id)
        .map_err(ApiError::Conflict)?;
    Ok(Json(json!({ "job": view })))
}

pub async fn delete_job(
    State(state): State<SharedState>,
    ctx: crate::routes::AuthCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    ctx.require(Role::Operator)?;
    ensure_job_access(&state, &ctx, &id)?;
    state
        .jobs
        .delete(&ctx.username, role_str(&ctx.role), &id)
        .map_err(ApiError::Conflict)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(serde::Deserialize)]
pub struct LogsQuery {
    pub max_bytes: Option<usize>,
}

pub async fn get_logs(
    State(state): State<SharedState>,
    ctx: crate::routes::AuthCtx,
    Path(id): Path<Uuid>,
    Query(q): Query<LogsQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    ctx.require(Role::Viewer)?;
    let rt = state
        .jobs
        .runtime_if_exists(&id)
        .ok_or(ApiError::NotFound)?;
    let max = q.max_bytes.unwrap_or(64 * 1024).min(1024 * 1024);
    let content = rt.tail(max);
    Ok(Json(json!({ "content": content })))
}

/// Live log stream: initial backlog then tail via broadcast, plus status updates.
pub async fn logs_ws(
    State(state): State<SharedState>,
    Path(id): Path<Uuid>,
    ws: WebSocketUpgrade,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    let ctx = ws_auth(&state, &params)?;
    ctx.require(Role::Viewer)?;
    let Some(rt) = state.jobs.runtime_if_exists(&id) else {
        return Err(ApiError::NotFound);
    };
    let jobs = state.jobs.clone();

    Ok(ws.on_upgrade(move |socket| async move {
        let (mut tx, mut rx) = socket.split();
        // Backlog
        let backlog = rt.tail(64 * 1024);
        for chunk in split_chunks(&backlog, 8 * 1024) {
            let frame = json!({"type": "log", "data": chunk});
            if tx.send(axum::extract::ws::Message::Text(frame.to_string().into())).await.is_err() {
                return;
            }
        }
        let mut sub = rt.subscribe();
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        let mut last_status: Option<JobStatus> = None;
        loop {
            tokio::select! {
                chunk = sub.recv() => {
                    match chunk {
                        Ok(data) => {
                            let frame = json!({"type": "log", "data": data});
                            if tx.send(axum::extract::ws::Message::Text(frame.to_string().into())).await.is_err() {
                                break;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = ticker.tick() => {
                    let st = *rt.status.read().unwrap_or_else(|e| e.into_inner());
                    if last_status != Some(st) {
                        last_status = Some(st);
                        let job = jobs.get(&id);
                        let frame = json!({"type": "status", "job": job});
                        if tx.send(axum::extract::ws::Message::Text(frame.to_string().into())).await.is_err() {
                            break;
                        }
                    }
                }
                msg = rx.next() => {
                    match msg {
                        Some(Ok(axum::extract::ws::Message::Text(t))) => {
                            if t.contains("ping") {
                                let _ = tx.send(axum::extract::ws::Message::Text(json!({"type":"pong"}).to_string().into())).await;
                            }
                        }
                        None | Some(Ok(axum::extract::ws::Message::Close(_))) | Some(Err(_)) => break,
                        Some(Ok(_)) => {}
                    }
                }
            }
        }
    }))
}

fn split_chunks(s: &str, size: usize) -> Vec<&str> {
    if s.is_empty() {
        return vec![];
    }
    let mut out = Vec::new();
    let mut start = 0;
    while start < s.len() {
        let mut end = (start + size).min(s.len());
        while end < s.len() && !s.is_char_boundary(end) {
            end += 1;
        }
        out.push(&s[start..end]);
        start = end;
    }
    out
}
