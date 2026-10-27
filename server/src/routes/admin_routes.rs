use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;

use crate::auth::Role;
use crate::error::{ApiError, ApiResult};
use crate::routes::AuthCtx;
use crate::state::{role_str, SharedState};

/// Read the native ngrok Windows service state without spawning a shell or
/// exposing agent configuration / credentials through the API.
#[cfg(windows)]
fn ngrok_service_status() -> serde_json::Value {
    use windows_service::service::{ServiceAccess, ServiceState};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    let manager = match ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
    {
        Ok(manager) => manager,
        Err(_) => {
            return serde_json::json!({"installed":false,"running":false,"status":"unavailable"})
        }
    };
    let service = match manager.open_service("ngrok", ServiceAccess::QUERY_STATUS) {
        Ok(service) => service,
        Err(_) => {
            return serde_json::json!({"installed":false,"running":false,"status":"not_installed"})
        }
    };
    match service.query_status() {
        Ok(status) => {
            let state = match status.current_state {
                ServiceState::Running => "running",
                ServiceState::Stopped => "stopped",
                ServiceState::StartPending => "starting",
                ServiceState::StopPending => "stopping",
                _ => "pending",
            };
            serde_json::json!({
                "installed":true,
                "running":status.current_state == ServiceState::Running,
                "status":state,
            })
        }
        Err(_) => serde_json::json!({"installed":true,"running":false,"status":"unavailable"}),
    }
}

#[cfg(not(windows))]
fn ngrok_service_status() -> serde_json::Value {
    serde_json::json!({"installed":false,"running":false,"status":"unsupported"})
}

async fn settings_value(state: &SharedState) -> serde_json::Value {
    let cfg = state.config.read().unwrap_or_else(|e| e.into_inner());
    serde_json::json!({
        "data_dir": cfg.data_dir,
        "workspace_root": cfg.workspace_root,
        "bind": cfg.bind,
        "port": cfg.port,
        "version": state.version,
        "tunnel": state.tunnel.info(),
        "ngrok": ngrok_service_status(),
        "update": cfg.update,
        "resources": state.jobs.resources.status(),
        "repair": cfg.repair,
        "repair_status": crate::repair::status(&cfg.data_dir),
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
    pub desktop_priority: Option<bool>,
    pub repair: Option<crate::repair::RepairConfig>,
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
    let (port, tunnel_enabled) = {
        let mut cfg = state.config.write().unwrap_or_else(|e| e.into_inner());
        let mut next = cfg.clone();
        if let Some(repair) = body.repair {
            repair
                .validate()
                .map_err(|e| ApiError::BadRequest(e.to_string()))?;
            next.repair = repair;
        }
        if let Some(enabled) = body.desktop_priority {
            next.desktop_priority = enabled;
        }
        if let Some(tunnel) = &body.tunnel {
            next.tunnel.enabled = tunnel.enabled;
        }
        if let Some(update) = body.update {
            if let Some(v) = update.auto_check {
                next.update.auto_check = v;
            }
            if let Some(v) = update.auto_install {
                next.update.auto_install = v;
            }
            if let Some(v) = update.check_interval_hours {
                if !(1..=168).contains(&v) {
                    return Err(ApiError::BadRequest(
                        "確認間隔は1〜168時間で指定してください".into(),
                    ));
                }
                next.update.check_interval_hours = v;
            }
            if let Some(repo) = update.repo {
                let parts: Vec<_> = repo.split('/').collect();
                if parts.len() != 2
                    || parts.iter().any(|p| {
                        p.is_empty()
                            || *p == "."
                            || *p == ".."
                            || !p.bytes().all(|c| {
                                c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.')
                            })
                    })
                {
                    return Err(ApiError::BadRequest(
                        "repo は owner/repo 形式で指定してください".into(),
                    ));
                }
                next.update.repo = repo;
            }
        }
        // Validate the whole request before touching OS state or persisted config.
        let old = cfg.desktop_priority;
        if body.desktop_priority.is_some() {
            state
                .jobs
                .resources
                .set_enabled(next.desktop_priority)
                .map_err(ApiError::from)?;
        }
        if let Err(e) = next.save() {
            if body.desktop_priority.is_some() {
                let _ = state.jobs.resources.set_enabled(old);
            }
            return Err(ApiError::from(e));
        }
        let result = (next.port, next.tunnel.enabled);
        *cfg = next;
        result
    };
    if body.tunnel.is_some() {
        if tunnel_enabled {
            state.tunnel.start(port);
        } else {
            state.tunnel.stop();
        }
    }
    state.audit.log(
        &ctx.username,
        role_str(&ctx.role),
        "settings_update",
        "validated configuration saved",
    );
    Ok(Json(settings_value(&state).await))
}

pub async fn diagnose(
    State(state): State<SharedState>,
    ctx: AuthCtx,
) -> ApiResult<(axum::http::StatusCode, Json<serde_json::Value>)> {
    ctx.require(Role::Admin)?;
    let cfg = state
        .config
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    cfg.repair
        .validate()
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    if !cfg.repair.enabled {
        return Err(ApiError::Conflict("AI診断は無効です".into()));
    }
    if cfg.data_dir.join("repair/active.lock").exists() {
        return Err(ApiError::Conflict(
            "診断ジョブが実行中、または前回の確認が必要です".into(),
        ));
    }
    if std::env::var(&cfg.repair.api_key_env).map_or(true, |key| key.is_empty()) {
        return Err(ApiError::BadRequest("APIキー環境変数が未設定です".into()));
    }
    let mut cmd = std::process::Command::new(std::env::current_exe().map_err(ApiError::from)?);
    cmd.arg("repair-run")
        .arg("--data-dir")
        .arg(&cfg.data_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd.spawn().map_err(ApiError::from)?;
    state.audit.log(
        &ctx.username,
        role_str(&ctx.role),
        "repair_diagnose",
        "manual request",
    );
    Ok((
        axum::http::StatusCode::ACCEPTED,
        Json(serde_json::json!({"queued":true})),
    ))
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
