use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;

use crate::auth::Role;
use crate::error::{ApiError, ApiResult};
use crate::routes::AuthCtx;
use crate::state::{role_str, SharedState};

async fn settings_value(state: &SharedState) -> serde_json::Value {
    let cfg = state.config.read().unwrap();
    serde_json::json!({
        "data_dir": cfg.data_dir,
        "workspace_root": cfg.workspace_root,
        "bind": cfg.bind,
        "port": cfg.port,
        "version": state.version,
        "tunnel": state.tunnel.info(),
    })
}

pub async fn get_settings(
    State(state): State<SharedState>,
    ctx: AuthCtx,
) -> ApiResult<Json<serde_json::Value>> {
    ctx.require(Role::Admin)?;
    Ok(Json(settings_value(&state).await))
}

#[derive(Deserialize)]
pub struct UpdateSettingsBody {
    pub tunnel: Option<TunnelUpdate>,
}

#[derive(Deserialize)]
pub struct TunnelUpdate {
    pub enabled: bool,
}

pub async fn put_settings(
    State(state): State<SharedState>,
    ctx: AuthCtx,
    Json(body): Json<UpdateSettingsBody>,
) -> ApiResult<Json<serde_json::Value>> {
    ctx.require(Role::Admin)?;
    if let Some(t) = body.tunnel {
        let port = {
            let mut cfg = state.config.write().unwrap();
            cfg.tunnel.enabled = t.enabled;
            cfg.save().map_err(ApiError::from)?;
            cfg.port
        };
        if t.enabled {
            state.tunnel.start(port);
        } else {
            state.tunnel.stop();
        }
        state.audit.log(
            &ctx.username,
            role_str(&ctx.role),
            "settings_update",
            &format!("tunnel.enabled={}", t.enabled),
        );
    }
    Ok(Json(settings_value(&state).await))
}

#[derive(Deserialize)]
pub struct AuditQuery {
    pub limit: Option<usize>,
}

pub async fn audit_list(
    State(state): State<SharedState>,
    ctx: AuthCtx,
    Query(q): Query<AuditQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    ctx.require(Role::Admin)?;
    let limit = q.limit.unwrap_or(200).min(2000);
    let entries = state.audit.tail(limit);
    Ok(Json(serde_json::json!({ "entries": entries })))
}
