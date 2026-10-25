use axum::extract::{ConnectInfo, Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};
use tokio::sync::Semaphore;

fn login_slots() -> &'static Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SLOTS.get_or_init(|| Arc::new(Semaphore::new(2)))
}

use crate::auth::{issue_jwt, Role};
use crate::error::{ApiError, ApiResult};
use crate::routes::AuthCtx;
use crate::state::{role_str, SharedState};

#[derive(Deserialize)]
pub struct LoginBody {
    pub username: String,
    pub password: String,
}

pub async fn login(
    State(state): State<SharedState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<LoginBody>,
) -> ApiResult<Json<serde_json::Value>> {
    if body.username.len() > 256 || body.password.len() > 1024 {
        return Err(ApiError::BadRequest("login input too long".into()));
    }
    // Reject overload instead of queueing unbounded expensive password checks.
    let permit = login_slots()
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::Conflict("login busy; retry later".into()))?;
    // Lockout is tracked per source IP, not arbitrary usernames, to prevent a
    // memory-exhaustion attack using unlimited username values.
    let who = addr.ip().to_string();
    let secret = state.auth.jwt_secret();
    let auth = state.auth.clone();
    let username = body.username.clone();
    let result = tokio::task::spawn_blocking(move || {
        // The blocking task owns the permit even if the request is cancelled.
        let _permit = permit;
        auth.login(&username, &body.password, &who)
    })
    .await
    .map_err(ApiError::from)?
    .map_err(ApiError::from)?;

    match result {
        Some(ctx) => {
            state
                .audit
                .log(&ctx.username, role_str(&ctx.role), "login", "");
            let (token, exp) =
                issue_jwt(&ctx.username, ctx.role, &secret).map_err(ApiError::from)?;
            Ok(Json(serde_json::json!({
                "token": token,
                "username": ctx.username,
                "role": ctx.role,
                "expires_at": exp,
            })))
        }
        None => {
            state.audit.log(
                &body.username,
                "unknown",
                "login_failed",
                &addr.ip().to_string(),
            );
            Err(ApiError::Unauthorized)
        }
    }
}

pub async fn me(
    State(_state): State<SharedState>,
    ctx: AuthCtx,
) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(serde_json::json!({
        "username": ctx.username,
        "role": ctx.role,
    })))
}

pub async fn list_keys(
    State(state): State<SharedState>,
    ctx: AuthCtx,
) -> ApiResult<Json<serde_json::Value>> {
    ctx.require(Role::Admin)?;
    let keys: Vec<serde_json::Value> = state
        .auth
        .list_keys()
        .into_iter()
        .map(|k| {
            serde_json::json!({
                "id": k.id,
                "name": k.name,
                "role": k.role,
                "created_at": k.created_at,
                "last_used_at": k.last_used_at,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "keys": keys })))
}

#[derive(Deserialize)]
pub struct CreateKeyBody {
    pub name: String,
    pub role: String,
}

pub async fn create_key(
    State(state): State<SharedState>,
    ctx: AuthCtx,
    Json(body): Json<CreateKeyBody>,
) -> ApiResult<Json<serde_json::Value>> {
    ctx.require(Role::Admin)?;
    let name = body.name.trim();
    if name.is_empty() || name.len() > 64 {
        return Err(ApiError::BadRequest(
            "名前は1〜64文字で指定してください".into(),
        ));
    }
    let role = Role::parse(&body.role).ok_or(ApiError::BadRequest(
        "ロールは viewer / operator / admin で指定してください".into(),
    ))?;
    let (rec, key) = state.auth.create_key(name, role).map_err(ApiError::from)?;
    state
        .audit
        .log(&ctx.username, role_str(&ctx.role), "key_create", name);
    Ok(Json(serde_json::json!({
        "id": rec.id,
        "name": rec.name,
        "role": rec.role,
        "key": key,
    })))
}

pub async fn delete_key(
    State(state): State<SharedState>,
    ctx: AuthCtx,
    Path(id): Path<String>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    ctx.require(Role::Admin)?;
    let removed = state.auth.delete_key(&id).map_err(ApiError::from)?;
    if !removed {
        return Err(ApiError::NotFound);
    }
    state
        .audit
        .log(&ctx.username, role_str(&ctx.role), "key_delete", &id);
    Ok((StatusCode::OK, Json(serde_json::json!({"ok": true}))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn login_work_is_bounded_and_cancellation_keeps_permit() {
        let slots = Arc::new(Semaphore::new(1));
        let permit = slots.clone().try_acquire_owned().unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let task = tokio::task::spawn_blocking(move || {
            let permit = permit;
            started_tx.send(()).unwrap();
            finish_rx.recv().unwrap();
            drop(permit);
            done_tx.send(()).unwrap();
        });
        started_rx.await.unwrap();
        task.abort();
        assert!(slots.clone().try_acquire_owned().is_err());
        finish_tx.send(()).unwrap();
        done_rx.await.unwrap();
        assert!(slots.try_acquire_owned().is_ok());
    }
}
