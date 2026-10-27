use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::Role;
use crate::error::{ApiError, ApiResult};
use crate::files;
use crate::pty::TerminalInfo;
use crate::routes::ws_routes::terminal_settings;
use crate::routes::AuthCtx;
use crate::state::{role_str, SharedState};

#[derive(Debug, Default, Deserialize)]
pub struct CreateTerminalRequest {
    pub title: Option<String>,
    pub cols: Option<u16>,
    pub rows: Option<u16>,
}

/// List the caller's persistent terminal sessions (newest tab numbering).
pub async fn list(State(state): State<SharedState>, ctx: AuthCtx) -> ApiResult<Json<Value>> {
    ctx.require(Role::Operator)?;
    let sessions = state.terminals.list(&ctx.username);
    Ok(Json(json!({ "sessions": sessions })))
}

/// Open a new persistent tab. The shell keeps running after the browser
/// disconnects; it ends only when the tab is closed or the shell exits.
pub async fn create(
    State(state): State<SharedState>,
    ctx: AuthCtx,
    Json(request): Json<CreateTerminalRequest>,
) -> ApiResult<Json<TerminalInfo>> {
    ctx.require(Role::Operator)?;
    let (shell, args, workspace_root, data_dir) = terminal_settings(&state);
    let workspace = files::ensure_user_root(&workspace_root, &ctx.username).map_err(|e| {
        ApiError::Internal(anyhow::anyhow!(
            "ユーザー作業フォルダを準備できません: {e:#}"
        ))
    })?;
    let profile = crate::profiles::CliProfile::ensure(&data_dir, &ctx.username).map_err(|e| {
        ApiError::Internal(anyhow::anyhow!(
            "ユーザーCLIプロファイルを準備できません: {e:#}"
        ))
    })?;

    let cols = request.cols.unwrap_or(120).clamp(20, 500);
    let rows = request.rows.unwrap_or(30).clamp(5, 300);
    let title = request
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(|title| title.chars().take(64).collect::<String>());

    let session = state
        .terminals
        .create(
            &ctx.username,
            &shell,
            &args,
            &workspace,
            &profile,
            cols,
            rows,
            title,
        )
        .map_err(ApiError::from)?;

    state.audit.log(
        &ctx.username,
        role_str(&ctx.role),
        "terminal_open",
        &format!("{} ({})", session.title(), session.id),
    );
    Ok(Json(session.info()))
}

/// Close a tab. This is the only operation that kills the shell; navigation,
/// browser reloads and WebSocket drops leave it running.
pub async fn remove(
    State(state): State<SharedState>,
    ctx: AuthCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    ctx.require(Role::Operator)?;
    if !state.terminals.remove(&ctx.username, id) {
        return Err(ApiError::NotFound);
    }
    state.audit.log(
        &ctx.username,
        role_str(&ctx.role),
        "terminal_close",
        &id.to_string(),
    );
    Ok(Json(json!({ "ok": true })))
}
