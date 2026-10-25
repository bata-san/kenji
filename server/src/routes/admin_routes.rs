use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;

use crate::auth::Role;
use crate::error::{ApiError, ApiResult};
use crate::routes::AuthCtx;
use crate::state::{role_str, SharedState};

async fn settings_value(state: &SharedState) -> serde_json::Value {
    let cfg = state.config.read().unwrap_or_else(|e| e.into_inner());
    serde_json::json!({
        "data_dir": cfg.data_dir,
        "workspace_root": cfg.workspace_root,
        "bind": cfg.bind,
        "port": cfg.port,
        "version": state.version,
        "tunnel": state.tunnel.info(),
        "update": cfg.update,
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
    pub update: Option<UpdatePrefUpdate>,
}

#[derive(Deserialize)]
pub struct TunnelUpdate {
    pub enabled: bool,
}

#[derive(Deserialize)]
pub struct UpdatePrefUpdate {
    pub auto_check: Option<bool>,
    pub auto_install: Option<bool>,
    pub check_interval_hours: Option<u32>,
    pub repo: Option<String>,
}

pub async fn put_settings(
    State(state): State<SharedState>,
    ctx: AuthCtx,
    Json(body): Json<UpdateSettingsBody>,
) -> ApiResult<Json<serde_json::Value>> {
    ctx.require(Role::Admin)?;
    if let Some(t) = body.tunnel {
        let port = {
            let mut cfg = state.config.write().unwrap_or_else(|e| e.into_inner());
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
    if let Some(u) = body.update {
        let detail = {
            let mut cfg = state.config.write().unwrap_or_else(|e| e.into_inner());
            if let Some(v) = u.auto_check {
                cfg.update.auto_check = v;
            }
            if let Some(v) = u.auto_install {
                cfg.update.auto_install = v;
            }
            if let Some(v) = u.check_interval_hours {
                cfg.update.check_interval_hours = v;
            }
            if let Some(v) = u.repo {
                if !v.contains('/') {
                    return Err(ApiError::BadRequest(
                        "repo は owner/repo 形式で指定してください".into(),
                    ));
                }
                cfg.update.repo = v;
            }
            cfg.save().map_err(ApiError::from)?;
            format!(
                "update.auto_check={} auto_install={} interval={}h repo={}",
                cfg.update.auto_check,
                cfg.update.auto_install,
                cfg.update.check_interval_hours,
                cfg.update.repo
            )
        };
        state.audit.log(
            &ctx.username,
            role_str(&ctx.role),
            "settings_update",
            &detail,
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
