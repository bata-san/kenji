use std::path::PathBuf;

use axum::body::Body;
use axum::extract::{Multipart, Query, State};
use axum::http::{header, HeaderMap};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use tokio::io::AsyncWriteExt;

use crate::auth::Role;
use crate::error::{ApiError, ApiResult};
use crate::files;
use crate::routes::{bearer_auth, AuthCtx};
use crate::state::{role_str, SharedState};

fn workspace_root(state: &SharedState) -> PathBuf {
    state.config.read().unwrap().workspace_root.clone()
}

fn normalize(rel: &str) -> String {
    let t = rel.trim().trim_start_matches('/');
    let t = t.replace('\\', "/");
    if t.is_empty() {
        "/".to_string()
    } else {
        format!("/{t}")
    }
}

#[derive(Deserialize)]
pub struct PathQuery {
    pub path: Option<String>,
}

fn resolve_checked(state: &SharedState, rel: &str) -> ApiResult<(PathBuf, String)> {
    let root = workspace_root(state);
    let p = files::resolve_under_root(&root, rel).map_err(|e| ApiError::BadRequest(e.to_string()))?;
    Ok((p, normalize(rel)))
}

// ---- list ----

pub async fn list(
    State(state): State<SharedState>,
    ctx: AuthCtx,
    Query(q): Query<PathQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    ctx.require(Role::Viewer)?;
    let (dir, norm) = resolve_checked(&state, q.path.as_deref().unwrap_or("/"))?;
    let dir = files::ensure_within(&workspace_root(&state), &dir)
        .map_err(|_| ApiError::NotFound)?;

    let mut entries: Vec<(String, String, Option<u64>, Option<DateTime<Utc>>)> = Vec::new();
    let mut rd = tokio::fs::read_dir(&dir).await.map_err(|_| ApiError::NotFound)?;
    while let Some(e) = rd.next_entry().await.map_err(ApiError::from)? {
        let name = e.file_name().to_string_lossy().to_string();
        let Ok(meta) = e.metadata().await else { continue };
        let (kind, size) = if meta.is_dir() {
            ("dir".to_string(), None)
        } else {
            ("file".to_string(), Some(meta.len()))
        };
        let modified = meta.modified().ok().map(|t| DateTime::<Utc>::from(t));
        entries.push((name, kind, size, modified));
    }
    // directories first, then case-insensitive name order
    entries.sort_by(|a, b| match (a.1 == "dir", b.1 == "dir") {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.0.to_lowercase().cmp(&b.0.to_lowercase()),
    });

    let items: Vec<serde_json::Value> = entries
        .into_iter()
        .map(|(name, kind, size, modified)| {
            serde_json::json!({"name": name, "kind": kind, "size": size, "modified": modified})
        })
        .collect();
    Ok(Json(serde_json::json!({ "path": norm, "entries": items })))
}

// ---- mkdir ----

#[derive(Deserialize)]
pub struct PathBody {
    pub path: String,
}

pub async fn mkdir(
    State(state): State<SharedState>,
    ctx: AuthCtx,
    Json(body): Json<PathBody>,
) -> ApiResult<Json<serde_json::Value>> {
    ctx.require(Role::Operator)?;
    let (dir, norm) = resolve_checked(&state, &body.path)?;
    tokio::fs::create_dir_all(&dir).await.map_err(ApiError::from)?;
    state.audit.log(&ctx.username, role_str(&ctx.role), "file_mkdir", &norm);
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---- move/rename ----

#[derive(Deserialize)]
pub struct MoveBody {
    pub from: String,
    pub to: String,
}

pub async fn r#move(
    State(state): State<SharedState>,
    ctx: AuthCtx,
    Json(body): Json<MoveBody>,
) -> ApiResult<Json<serde_json::Value>> {
    ctx.require(Role::Operator)?;
    let (from, from_norm) = resolve_checked(&state, &body.from)?;
    let (to, to_norm) = resolve_checked(&state, &body.to)?;
    files::safe_file_name(&normalize(&body.to).trim_start_matches('/').rsplit('/').next().unwrap_or(""))?;
    tokio::fs::rename(&from, &to).await.map_err(ApiError::from)?;
    state
        .audit
        .log(&ctx.username, role_str(&ctx.role), "file_move", &format!("{from_norm} → {to_norm}"));
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---- delete ----

pub async fn delete(
    State(state): State<SharedState>,
    ctx: AuthCtx,
    Query(q): Query<PathQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    ctx.require(Role::Operator)?;
    let (p, norm) = resolve_checked(&state, q.path.as_deref().unwrap_or("/"))?;
    if p == workspace_root(&state) {
        return Err(ApiError::BadRequest("ワークスペースルートは削除できません".into()));
    }
    let meta = tokio::fs::metadata(&p).await.map_err(|_| ApiError::NotFound)?;
    if meta.is_dir() {
        tokio::task::spawn_blocking(move || std::fs::remove_dir_all(p))
            .await
            .map_err(ApiError::from)?
            .map_err(ApiError::from)?;
    } else {
        tokio::fs::remove_file(&p).await.map_err(ApiError::from)?;
    }
    state.audit.log(&ctx.username, role_str(&ctx.role), "file_delete", &norm);
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---- download ----

#[derive(Deserialize)]
pub struct DownloadQuery {
    pub path: String,
    pub token: Option<String>,
}

fn content_disposition(name: &str) -> String {
    let fallback: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let encoded = percent_encoding::utf8_percent_encode(name, percent_encoding::NON_ALPHANUMERIC).to_string();
    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

/// Accepts `Authorization: Bearer` (API/CLI) or `?token=` (browser links).
pub async fn download(
    State(state): State<SharedState>,
    Query(q): Query<DownloadQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = match &q.token {
        Some(t) => {
            let secret = state.auth.jwt_secret();
            state
                .auth
                .authenticate(t, &secret)
                .map(|c| AuthCtx { username: c.username, role: c.role })
        }
        None => bearer_auth(&state, &headers).ok(),
    };
    let ctx = ctx.ok_or(ApiError::Unauthorized)?;
    ctx.require(Role::Viewer)?;

    let (p, norm) = resolve_checked(&state, &q.path)?;
    let root = workspace_root(&state);
    let p = files::ensure_within(&root, &p).map_err(|e| ApiError::BadRequest(e.to_string()))?;
    let meta = tokio::fs::metadata(&p).await.map_err(|_| ApiError::NotFound)?;
    if meta.is_dir() {
        return Err(ApiError::BadRequest("ディレクトリのダウンロードは未対応です（サブフォルダは個別にダウンロードしてください）".into()));
    }
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "download.bin".into());
    let file = tokio::fs::File::open(&p).await.map_err(ApiError::from)?;
    let stream = tokio_util::io::ReaderStream::new(file);
    let body = Body::from_stream(stream);
    let resp = Response::builder()
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, meta.len())
        .header(
            header::CONTENT_DISPOSITION,
            content_disposition(&name),
        )
        .body(body)
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("{e}")))?;
    state
        .audit
        .log(&ctx.username, role_str(&ctx.role), "file_download", &norm);
    Ok(resp)
}

// ---- upload ----

pub async fn upload(
    State(state): State<SharedState>,
    ctx: AuthCtx,
    Query(q): Query<PathQuery>,
    mut multipart: Multipart,
) -> ApiResult<Json<serde_json::Value>> {
    ctx.require(Role::Operator)?;
    let (dir, norm) = resolve_checked(&state, q.path.as_deref().unwrap_or("/"))?;
    let root = workspace_root(&state);
    files::ensure_within(&root, &dir).map_err(|_| ApiError::NotFound)?;
    let _ = norm;

    let mut saved: Vec<serde_json::Value> = Vec::new();
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::BadRequest(format!("multipart error: {e}")))?
    {
        if field.name() != Some("file") {
            continue;
        }
        let orig = field.file_name().unwrap_or("upload.bin").to_string();
        let safe = files::safe_file_name(&orig).map_err(|e| ApiError::BadRequest(e.to_string()))?;
        let dest = dir.join(&safe);
        let mut f = tokio::fs::File::create(&dest).await.map_err(ApiError::from)?;
        let mut size: u64 = 0;
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|e| ApiError::BadRequest(format!("upload error: {e}")))?
        {
            f.write_all(&chunk).await.map_err(ApiError::from)?;
            size += chunk.len() as u64;
        }
        f.flush().await.map_err(ApiError::from)?;
        saved.push(serde_json::json!({ "name": safe, "size": size }));
    }
    state.audit.log(
        &ctx.username,
        role_str(&ctx.role),
        "file_upload",
        &format!("{} ({} files)", normalize(&q.path.unwrap_or_default()), saved.len()),
    );
    Ok(Json(serde_json::json!({ "ok": true, "saved": saved })))
}
