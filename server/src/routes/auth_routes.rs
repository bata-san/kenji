use axum::extract::{ConnectInfo, Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use std::net::SocketAddr;

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
    let who = format!("{}|{}", body.username, addr.ip());
    let secret = state.auth.jwt_secret();
    let result = state
        .auth
        .login(&body.username, &body.password, &who)
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
