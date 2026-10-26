use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;

use crate::auth::Role;
use crate::error::{ApiError, ApiResult};
use crate::routes::AuthCtx;
use crate::state::{role_str, SharedState};

#[derive(Deserialize)]
pub struct CreateBody {
    pub username: String,
    pub password: String,
    pub role: String,
}

#[derive(Deserialize)]
pub struct PasswordBody {
    pub password: String,
}

#[derive(Deserialize)]
pub struct RoleBody {
    pub role: String,
}

fn parse_role(raw: &str) -> ApiResult<Role> {
    Role::parse(raw).ok_or_else(|| {
        ApiError::BadRequest("role は viewer / operator / admin のいずれかです".into())
    })
}

pub async fn list(
    State(state): State<SharedState>,
    ctx: AuthCtx,
) -> ApiResult<Json<serde_json::Value>> {
    ctx.require(Role::Admin)?;
    let users = state.auth.list_users().map_err(ApiError::from)?;
    Ok(Json(serde_json::json!({ "users": users })))
}

pub async fn create(
    State(state): State<SharedState>,
    ctx: AuthCtx,
    Json(body): Json<CreateBody>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    ctx.require(Role::Admin)?;
    let role = parse_role(&body.role)?;
    let user = state
        .auth
        .create_user(&body.username, &body.password, role)?;
    state.audit.log(
        &ctx.username,
        role_str(&ctx.role),
        "user_create",
        &format!("{} ({})", user.username, user.role.as_str()),
    );
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "user": user })),
    ))
}

pub async fn delete_user(
    State(state): State<SharedState>,
    ctx: AuthCtx,
    Path(username): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    ctx.require(Role::Admin)?;
    if username.eq_ignore_ascii_case(&ctx.username) {
        return Err(ApiError::Conflict("自分自身は削除できません".into()));
    }
    if !state.auth.delete_user(&username)? {
        return Err(ApiError::NotFound);
    }
    state
        .audit
        .log(&ctx.username, role_str(&ctx.role), "user_delete", &username);
    Ok(Json(serde_json::json!({ "deleted": true })))
}

pub async fn set_password(
    State(state): State<SharedState>,
    ctx: AuthCtx,
    Path(username): Path<String>,
    Json(body): Json<PasswordBody>,
) -> ApiResult<Json<serde_json::Value>> {
    ctx.require(Role::Admin)?;
    if !state.auth.set_password(&username, &body.password)? {
        return Err(ApiError::NotFound);
    }
    state.audit.log(
        &ctx.username,
        role_str(&ctx.role),
        "user_password",
        &username,
    );
    Ok(Json(serde_json::json!({ "updated": true })))
}

pub async fn set_role(
    State(state): State<SharedState>,
    ctx: AuthCtx,
    Path(username): Path<String>,
    Json(body): Json<RoleBody>,
) -> ApiResult<Json<serde_json::Value>> {
    ctx.require(Role::Admin)?;
    let role = parse_role(&body.role)?;
    if !state.auth.set_role(&username, role)? {
        return Err(ApiError::NotFound);
    }
    state.audit.log(
        &ctx.username,
        role_str(&ctx.role),
        "user_role",
        &format!("{username} -> {}", role.as_str()),
    );
    Ok(Json(serde_json::json!({ "updated": true })))
}
