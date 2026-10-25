//! アップデートAPI: ステータス確認・最新版チェック・インストール適用。
//! すべて管理者権限必須。install は監査ログに記録される。

use axum::extract::State;
use axum::Json;
use serde_json::json;

use crate::auth::Role;
use crate::error::{ApiError, ApiResult};
use crate::routes::AuthCtx;
use crate::state::{role_str, SharedState};

/// 現在バージョン・利用可能な更新・アップデート設定のスナップショット。
pub async fn status(
    State(state): State<SharedState>,
    ctx: AuthCtx,
) -> ApiResult<Json<serde_json::Value>> {
    ctx.require(Role::Admin)?;
    let cfg = state.config.read().unwrap();
    Ok(Json(state.update.snapshot(&cfg.update)))
}

/// GitHub Releases の最新版を確認する。エラーはスナップショットの error フィールドで返す。
pub async fn check(
    State(state): State<SharedState>,
    ctx: AuthCtx,
) -> ApiResult<Json<serde_json::Value>> {
    ctx.require(Role::Admin)?;
    let repo = state.config.read().unwrap().update.repo.clone();
    let result = state.update.check(&repo).await;
    let detail = match &result {
        Ok(Some(r)) => {
            let newer = matches!(
                crate::update::compare_versions(&r.version, state.version),
                std::cmp::Ordering::Greater
            );
            format!(
                "repo={} 最新=v{} {}",
                repo,
                r.version,
                if newer { "（更新あり）" } else { "（最新状態）" }
            )
        }
        Ok(None) => format!("repo={} リリースが存在しません", repo),
        Err(e) => format!("repo={} 失敗: {e}", repo),
    };
    state.audit.log(&ctx.username, role_str(&ctx.role), "update_check", &detail);
    let cfg = state.config.read().unwrap();
    Ok(Json(state.update.snapshot(&cfg.update)))
}

/// 最新版をダウンロード・SHA256検証・適用し、完了後にサーバーを再起動する。
pub async fn install(
    State(state): State<SharedState>,
    ctx: AuthCtx,
) -> ApiResult<Json<serde_json::Value>> {
    ctx.require(Role::Admin)?;
    state.audit.log(
        &ctx.username,
        role_str(&ctx.role),
        "update_install",
        "最新版のダウンロード・SHA256検証・適用を実行（完了後は自動再起動）",
    );
    state.update.install().await.map_err(ApiError::BadRequest)?;
    Ok(Json(json!({ "restarting": true })))
}
