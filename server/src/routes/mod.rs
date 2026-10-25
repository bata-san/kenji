pub mod admin_routes;
pub mod auth_routes;
pub mod files_routes;
pub mod jobs_routes;
pub mod system_routes;
pub mod update_routes;
pub mod ws_routes;

use std::collections::HashMap;

use axum::body::Body;
use axum::extract::FromRequestParts;
use axum::http::header::{AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::Router;
use rust_embed::RustEmbed;
use tower_http::catch_panic::CatchPanicLayer;

use crate::auth::Role;
use crate::error::{ApiError, ApiResult};
use crate::state::SharedState;

/// Authenticated user attached to every protected handler.
#[derive(Debug, Clone)]
pub struct AuthCtx {
    pub username: String,
    pub role: Role,
}

impl AuthCtx {
    pub fn require(&self, min: Role) -> ApiResult<()> {
        self.role.require(min)
    }
}

fn authenticate_token(state: &SharedState, token: &str) -> ApiResult<AuthCtx> {
    let secret = state.auth.jwt_secret();
    state
        .auth
        .authenticate(token, &secret)
        .map(|ctx| AuthCtx {
            username: ctx.username,
            role: ctx.role,
        })
        .ok_or(ApiError::Unauthorized)
}

impl FromRequestParts<SharedState> for AuthCtx {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &SharedState,
    ) -> Result<Self, Self::Rejection> {
        let token = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or(ApiError::Unauthorized)?;
        authenticate_token(state, token)
    }
}

/// Auth helper for WebSocket upgrades (token via query param) and downloads.
pub fn ws_auth(state: &SharedState, params: &HashMap<String, String>) -> ApiResult<AuthCtx> {
    let token = params.get("token").ok_or(ApiError::Unauthorized)?;
    authenticate_token(state, token)
}

pub fn bearer_auth(state: &SharedState, headers: &HeaderMap) -> ApiResult<AuthCtx> {
    let token = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(ApiError::Unauthorized)?;
    authenticate_token(state, token)
}

#[derive(RustEmbed)]
#[folder = "../web/dist"]
struct WebAssets;

/// Serve the embedded SPA; fall back to index.html for client-side routes.
pub async fn static_handler(uri: Uri) -> Response {
    let path = uri.path();
    if path.starts_with("/api/") || path == "/api" {
        return ApiError::NotFound.into_response();
    }
    let trimmed = path.trim_start_matches('/');
    let asset_path = if trimmed.is_empty() {
        "index.html"
    } else {
        trimmed
    };
    match WebAssets::get(asset_path) {
        Some(asset) => {
            let mime = mime_guess::from_path(asset_path).first_or_octet_stream();
            let mut builder = Response::builder()
                .header(CONTENT_TYPE, mime.as_ref())
                .header(CACHE_CONTROL, "no-cache");
            builder = if asset_path == "index.html" {
                builder.header(CACHE_CONTROL, "no-store")
            } else {
                builder.header(CACHE_CONTROL, "public, max-age=86400")
            };
            builder.body(Body::from(asset.data)).unwrap_or_else(|_| {
                ApiError::Internal(anyhow::anyhow!("body build")).into_response()
            })
        }
        None => {
            // Missing asset with a file extension → 404; otherwise SPA route → index.
            if asset_path.contains('.') {
                (StatusCode::NOT_FOUND, "not found").into_response()
            } else {
                match WebAssets::get("index.html") {
                    Some(asset) => Response::builder()
                        .header(CONTENT_TYPE, "text/html; charset=utf-8")
                        .header(CACHE_CONTROL, "no-store")
                        .body(Body::from(asset.data))
                        .unwrap(),
                    None => (StatusCode::NOT_FOUND, "web UI not built").into_response(),
                }
            }
        }
    }
}

async fn api_404() -> ApiError {
    ApiError::NotFound
}

pub fn build_router(state: SharedState) -> Router {
    let api = Router::new()
        .route("/api/health", get(system_routes::health))
        .route("/api/auth/login", post(auth_routes::login))
        .route("/api/auth/me", get(auth_routes::me))
        .route(
            "/api/auth/keys",
            get(auth_routes::list_keys).post(auth_routes::create_key),
        )
        .route("/api/auth/keys/{id}", delete(auth_routes::delete_key))
        .route("/api/system/overview", get(system_routes::overview))
        .route(
            "/api/jobs",
            get(jobs_routes::list).post(jobs_routes::create),
        )
        .route(
            "/api/jobs/{id}",
            get(jobs_routes::get_job).delete(jobs_routes::delete_job),
        )
        .route("/api/jobs/{id}/start", post(jobs_routes::start_job))
        .route("/api/jobs/{id}/stop", post(jobs_routes::stop_job))
        .route("/api/jobs/{id}/logs", get(jobs_routes::get_logs))
        .route("/api/jobs/{id}/logs/ws", get(jobs_routes::logs_ws))
        .route("/api/terminal/ws", get(ws_routes::terminal_ws))
        .route("/api/files/list", get(files_routes::list))
        .route("/api/files/mkdir", post(files_routes::mkdir))
        .route("/api/files/move", post(files_routes::r#move))
        .route("/api/files/delete", delete(files_routes::delete))
        .route("/api/files/download", get(files_routes::download))
        .route("/api/files/upload", post(files_routes::upload))
        .route(
            "/api/settings",
            get(admin_routes::get_settings).put(admin_routes::put_settings),
        )
        .route("/api/audit", get(admin_routes::audit_list))
        .route("/api/repair/diagnose", post(admin_routes::diagnose))
        .route("/api/update/status", get(update_routes::status))
        .route("/api/update/check", post(update_routes::check))
        .route("/api/update/install", post(update_routes::install))
        .fallback(api_404)
        .with_state(state);

    Router::new()
        .merge(api)
        .fallback(static_handler)
        // ハンドラ内のパニックを握り潰さず 500 JSON に変換する（サーバー全体は継続稼働）
        .layer(CatchPanicLayer::custom(|panic| {
            tracing::error!("request panic: {}", panic_message(panic));
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({
                    "error": { "code": "internal", "message": "内部エラーが発生しました" }
                })),
            )
                .into_response()
        }))
        .layer(axum::extract::DefaultBodyLimit::max(512 * 1024 * 1024))
}

/// `std::panic::panic_any` のペイロードは型不明なので、主要な2型からメッセージを取り出す
fn panic_message(panic: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "不明なパニック（型情報なし）".to_string()
    }
}
