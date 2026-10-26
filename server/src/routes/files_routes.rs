use std::path::{Path, PathBuf};

const MAX_UPLOAD_FILE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_UPLOAD_REQUEST_BYTES: u64 = 512 * 1024 * 1024;
// Drop cleanup also runs when the request future is cancelled.
struct UploadTemp(PathBuf);

impl Drop for UploadTemp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

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

/// Each authenticated principal gets its own folder under the workspace root.
/// API keys share `_shared`; the folder is created lazily if it is missing.
fn user_workspace_root(state: &SharedState, ctx: &AuthCtx) -> ApiResult<PathBuf> {
    let workspace = state
        .config
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .workspace_root
        .clone();
    files::ensure_user_root(&workspace, &ctx.username).map_err(|e| {
        ApiError::Internal(anyhow::anyhow!(
            "ユーザー `{}` の作業フォルダを準備できません: {e:#}",
            ctx.username
        ))
    })
}

fn data_dir(state: &SharedState) -> PathBuf {
    state
        .config
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .data_dir
        .clone()
}

fn parse_scope(ctx: &AuthCtx, raw: Option<&str>) -> ApiResult<files::FsScope> {
    let scope = files::FsScope::parse(raw).map_err(|e| ApiError::BadRequest(e.to_string()))?;
    if scope == files::FsScope::System {
        // Whole-PC access is the highest privilege in the file API. Operators
        // and viewers stay inside their dedicated workspace.
        ctx.require(Role::Admin)?;
    }
    Ok(scope)
}

fn normalize(rel: &str) -> String {
    let t = rel.trim_start_matches('/');
    let t = t.replace('\\', "/");
    if t.is_empty() {
        "/".to_string()
    } else {
        format!("/{t}")
    }
}

fn is_virtual_root(path: &str) -> bool {
    let trimmed = path.trim();
    trimmed.is_empty() || trimmed == "/" || trimmed == "\\"
}

#[derive(Deserialize)]
pub struct PathQuery {
    pub path: Option<String>,
    pub scope: Option<String>,
}

fn resolve_in_root(root: &Path, rel: &str) -> ApiResult<(PathBuf, String)> {
    let p =
        files::resolve_under_root(root, rel).map_err(|e| ApiError::BadRequest(e.to_string()))?;
    Ok((p, normalize(rel)))
}

/// Resolve a request path in either the per-user workspace or, for admins,
/// the whole local filesystem. The first tuple element is the workspace root
/// when the scope is `workspace` (used for defense-in-depth checks).
fn resolve_checked(
    state: &SharedState,
    ctx: &AuthCtx,
    rel: &str,
    scope: files::FsScope,
) -> ApiResult<(Option<PathBuf>, PathBuf, String)> {
    match scope {
        files::FsScope::Workspace => {
            let root = user_workspace_root(state, ctx)?;
            let (p, norm) = resolve_in_root(&root, rel)?;
            let p = files::ensure_within(&root, &p).map_err(|_| ApiError::NotFound)?;
            Ok((Some(root), p, norm))
        }
        files::FsScope::System => {
            let (p, norm) =
                files::resolve_system_path(rel).map_err(|e| ApiError::BadRequest(e.to_string()))?;
            Ok((None, p, norm))
        }
    }
}

fn audit_detail(scope: files::FsScope, detail: &str) -> String {
    format!("[{}] {detail}", scope.as_str())
}

// ---- list ----

type FsEntry = (String, String, Option<u64>, Option<DateTime<Utc>>);

pub async fn list(
    State(state): State<SharedState>,
    ctx: AuthCtx,
    Query(q): Query<PathQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    ctx.require(Role::Viewer)?;
    let scope = parse_scope(&ctx, q.scope.as_deref())?;
    let requested = q.path.as_deref().unwrap_or("/");

    // In system scope the virtual root is a drive list, not a directory.
    if scope == files::FsScope::System && is_virtual_root(requested) {
        let entries: Vec<serde_json::Value> = files::list_drives()
            .into_iter()
            .map(|name| {
                serde_json::json!({
                    "name": name,
                    "kind": "drive",
                    "size": null,
                    "modified": null
                })
            })
            .collect();
        return Ok(Json(serde_json::json!({
            "path": "/",
            "scope": scope.as_str(),
            "entries": entries
        })));
    }

    let (root, dir, norm) = resolve_checked(&state, &ctx, requested, scope)?;
    if let Some(root) = &root {
        files::ensure_within(root, &dir).map_err(|_| ApiError::NotFound)?;
    }

    let mut entries: Vec<FsEntry> = Vec::new();
    let mut rd = tokio::fs::read_dir(&dir)
        .await
        .map_err(|_| ApiError::NotFound)?;
    while let Some(e) = rd.next_entry().await.map_err(ApiError::from)? {
        let name = e.file_name().to_string_lossy().to_string();
        let Ok(meta) = e.metadata().await else {
            continue;
        };
        let (kind, size) = if meta.is_dir() {
            ("dir".to_string(), None)
        } else {
            ("file".to_string(), Some(meta.len()))
        };
        let modified = meta.modified().ok().map(DateTime::<Utc>::from);
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
    Ok(Json(serde_json::json!({
        "path": norm,
        "scope": scope.as_str(),
        "entries": items
    })))
}

// ---- mkdir ----

#[derive(Deserialize)]
pub struct PathBody {
    pub path: String,
    #[serde(default)]
    pub scope: Option<String>,
}

pub async fn mkdir(
    State(state): State<SharedState>,
    ctx: AuthCtx,
    Json(body): Json<PathBody>,
) -> ApiResult<Json<serde_json::Value>> {
    ctx.require(Role::Operator)?;
    let scope = parse_scope(&ctx, body.scope.as_deref())?;
    let (root, dir, norm) = resolve_checked(&state, &ctx, &body.path, scope)?;
    if let Some(root) = &root {
        files::ensure_within(root, &dir).map_err(|e| ApiError::BadRequest(e.to_string()))?;
    } else {
        files::ensure_system_write_allowed(&dir, &data_dir(&state))
            .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    }
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(ApiError::from)?;
    state.audit.log(
        &ctx.username,
        role_str(&ctx.role),
        "file_mkdir",
        &audit_detail(scope, &norm),
    );
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---- move/rename ----

#[derive(Deserialize)]
pub struct MoveBody {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub scope: Option<String>,
}

pub async fn r#move(
    State(state): State<SharedState>,
    ctx: AuthCtx,
    Json(body): Json<MoveBody>,
) -> ApiResult<Json<serde_json::Value>> {
    ctx.require(Role::Operator)?;
    let scope = parse_scope(&ctx, body.scope.as_deref())?;
    let (root, from, from_norm) = resolve_checked(&state, &ctx, &body.from, scope)?;
    let (_, to, to_norm) = resolve_checked(&state, &ctx, &body.to, scope)?;
    if let Some(root) = &root {
        files::ensure_not_root(root, &from).map_err(|e| ApiError::BadRequest(e.to_string()))?;
        files::ensure_not_root(root, &to).map_err(|e| ApiError::BadRequest(e.to_string()))?;
    } else {
        let data = data_dir(&state);
        files::ensure_system_destructive_allowed(&from, &data)
            .map_err(|e| ApiError::BadRequest(e.to_string()))?;
        files::ensure_system_destructive_allowed(&to, &data)
            .map_err(|e| ApiError::BadRequest(e.to_string()))?;
        files::ensure_system_write_allowed(&to, &data)
            .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    }
    tokio::fs::rename(&from, &to)
        .await
        .map_err(ApiError::from)?;
    state.audit.log(
        &ctx.username,
        role_str(&ctx.role),
        "file_move",
        &audit_detail(scope, &format!("{from_norm} → {to_norm}")),
    );
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---- delete ----

pub async fn delete(
    State(state): State<SharedState>,
    ctx: AuthCtx,
    Query(q): Query<PathQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    ctx.require(Role::Operator)?;
    let scope = parse_scope(&ctx, q.scope.as_deref())?;
    let (root, p, norm) = resolve_checked(&state, &ctx, q.path.as_deref().unwrap_or("/"), scope)?;
    if let Some(root) = &root {
        files::ensure_not_root(root, &p).map_err(|e| ApiError::BadRequest(e.to_string()))?;
    } else {
        files::ensure_system_destructive_allowed(&p, &data_dir(&state))
            .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    }
    let meta = tokio::fs::metadata(&p)
        .await
        .map_err(|_| ApiError::NotFound)?;
    if meta.is_dir() {
        tokio::task::spawn_blocking(move || std::fs::remove_dir_all(p))
            .await
            .map_err(ApiError::from)?
            .map_err(ApiError::from)?;
    } else {
        tokio::fs::remove_file(&p).await.map_err(ApiError::from)?;
    }
    state.audit.log(
        &ctx.username,
        role_str(&ctx.role),
        "file_delete",
        &audit_detail(scope, &norm),
    );
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---- download ----

#[derive(Deserialize)]
pub struct DownloadQuery {
    pub path: String,
    pub token: Option<String>,
    pub scope: Option<String>,
}

fn content_disposition(name: &str) -> String {
    let fallback: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let encoded =
        percent_encoding::utf8_percent_encode(name, percent_encoding::NON_ALPHANUMERIC).to_string();
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
            state.auth.authenticate(t, &secret).map(|c| AuthCtx {
                username: c.username,
                role: c.role,
            })
        }
        None => bearer_auth(&state, &headers).ok(),
    };
    let ctx = ctx.ok_or(ApiError::Unauthorized)?;
    ctx.require(Role::Viewer)?;
    let scope = parse_scope(&ctx, q.scope.as_deref())?;

    let (_root, p, norm) = resolve_checked(&state, &ctx, &q.path, scope)?;
    let meta = tokio::fs::metadata(&p)
        .await
        .map_err(|_| ApiError::NotFound)?;
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
        .header(header::CONTENT_DISPOSITION, content_disposition(&name))
        .body(body)
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("{e}")))?;
    state.audit.log(
        &ctx.username,
        role_str(&ctx.role),
        "file_download",
        &audit_detail(scope, &norm),
    );
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
    let scope = parse_scope(&ctx, q.scope.as_deref())?;
    let (root, dir, norm) = resolve_checked(&state, &ctx, q.path.as_deref().unwrap_or("/"), scope)?;
    let dir = match &root {
        Some(root) => {
            files::ensure_within(root, &dir).map_err(|e| ApiError::BadRequest(e.to_string()))?
        }
        None => {
            files::ensure_system_write_allowed(&dir, &data_dir(&state))
                .map_err(|e| ApiError::BadRequest(e.to_string()))?;
            dir
        }
    };

    let mut saved: Vec<serde_json::Value> = Vec::new();
    let mut total_size = 0u64;
    let mut file_count = 0usize;
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::BadRequest(format!("multipart error: {e}")))?
    {
        if field.name() != Some("file") {
            continue;
        }
        file_count += 1;
        if file_count > 32 {
            return Err(ApiError::BadRequest(
                "一度にアップロードできるファイルは32個までです".into(),
            ));
        }
        let orig = field.file_name().unwrap_or("upload.bin").to_string();
        let safe = files::safe_file_name(&orig).map_err(|e| ApiError::BadRequest(e.to_string()))?;
        let dest = dir.join(&safe);
        if root.is_none() {
            files::ensure_system_write_allowed(&dest, &data_dir(&state))
                .map_err(|e| ApiError::BadRequest(e.to_string()))?;
        }
        // Never follow a pre-existing symlink or overwrite a directory.
        match tokio::fs::symlink_metadata(&dest).await {
            Ok(meta) if meta.file_type().is_symlink() || meta.is_dir() => {
                return Err(ApiError::BadRequest(
                    "アップロード先が通常ファイルではありません".into(),
                ));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(ApiError::from(e)),
        }
        let temp = dir.join(format!(".kenji-upload-{}.tmp", uuid::Uuid::new_v4()));
        // Create synchronously so cancellation cannot occur between creation and
        // installing the cleanup guard. The actual upload writes stay async.
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(ApiError::from)?;
        let _cleanup = UploadTemp(temp.clone());
        let mut f = tokio::fs::File::from_std(file);
        let upload_result: ApiResult<u64> = async {
            let mut size = 0u64;
            while let Some(chunk) = field
                .chunk()
                .await
                .map_err(|e| ApiError::BadRequest(format!("upload error: {e}")))?
            {
                size = size
                    .checked_add(chunk.len() as u64)
                    .ok_or_else(|| ApiError::BadRequest("アップロードサイズ超過".into()))?;
                if size > MAX_UPLOAD_FILE_BYTES
                    || total_size.saturating_add(size) > MAX_UPLOAD_REQUEST_BYTES
                {
                    return Err(ApiError::BadRequest(
                        "アップロードサイズ上限を超えました".into(),
                    ));
                }
                f.write_all(&chunk).await.map_err(ApiError::from)?;
            }
            f.flush().await.map_err(ApiError::from)?;
            f.sync_all().await.map_err(ApiError::from)?;
            Ok(size)
        }
        .await;
        drop(f);
        let size = upload_result?;
        // Recheck parents and destination, but path-based checks cannot eliminate
        // concurrent parent/junction swaps (see files::ensure_within).
        if let Some(root) = &root {
            files::ensure_within(root, &dir).map_err(|e| ApiError::BadRequest(e.to_string()))?;
        } else {
            files::ensure_system_write_allowed(&dest, &data_dir(&state))
                .map_err(|e| ApiError::BadRequest(e.to_string()))?;
        }
        match tokio::fs::symlink_metadata(&dest).await {
            Ok(meta) if meta.file_type().is_symlink() || meta.is_dir() => {
                return Err(ApiError::BadRequest(
                    "アップロード先が通常ファイルではありません".into(),
                ));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(ApiError::from(e)),
        }
        // Same-directory rename atomically replaces files on Windows too
        // (MoveFileExW with REPLACE_EXISTING); never move the old file aside.
        std::fs::rename(&temp, &dest).map_err(ApiError::from)?;
        total_size = total_size.saturating_add(size);
        saved.push(serde_json::json!({ "name": safe, "size": size }));
    }
    state.audit.log(
        &ctx.username,
        role_str(&ctx.role),
        "file_upload",
        &audit_detail(scope, &format!("{norm} ({} files)", saved.len())),
    );
    Ok(Json(serde_json::json!({ "ok": true, "saved": saved })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_temp_cleanup_and_atomic_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join("upload.tmp");
        let dest = dir.path().join("existing.txt");
        std::fs::write(&dest, "old").unwrap();
        {
            std::fs::write(&temp, "cancelled").unwrap();
            let _cleanup = UploadTemp(temp.clone());
        }
        assert!(!temp.exists());
        {
            std::fs::write(&temp, "new").unwrap();
            let _cleanup = UploadTemp(temp.clone());
            std::fs::rename(&temp, &dest).unwrap();
        }
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "new");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
