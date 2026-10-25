use axum::extract::State;
use axum::Json;

use crate::auth::Role;
use crate::error::ApiResult;
use crate::routes::AuthCtx;
use crate::state::SharedState;

pub async fn health(State(state): State<SharedState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "ok": true,
        "version": state.version,
        "setup_done": state.setup_done,
    }))
}

pub async fn overview(
    State(state): State<SharedState>,
    ctx: AuthCtx,
) -> ApiResult<Json<serde_json::Value>> {
    ctx.require(Role::Viewer)?;
    let snap = state.metrics.snapshot();
    let (running, total, failed) = state.jobs.summary();
    let mut value = serde_json::to_value(&snap).unwrap_or_default();
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "jobs_summary".into(),
            serde_json::json!({"running": running, "total": total, "failed": failed}),
        );
        obj.insert(
            "tunnel".into(),
            serde_json::to_value(state.tunnel.info()).unwrap_or_default(),
        );
    }
    Ok(Json(value))
}
