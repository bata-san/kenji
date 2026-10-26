use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::broadcast::error::RecvError;
use tokio::time::MissedTickBehavior;
use uuid::Uuid;

use crate::auth::Role;
use crate::error::{ApiError, ApiResult};
use crate::files;
use crate::pty::{TerminalEvent, TerminalSession};
use crate::routes::ws_auth;
use crate::state::{role_str, SharedState};

/// Interactive PowerShell over ConPTY <-> WebSocket (binary frames = PTY I/O).
///
/// The PTY belongs to the server-side session, not to the socket. Closing the
/// browser, changing pages, or a temporary network outage only detaches the
/// client; the shell keeps running and is replayed on the next attach.
pub async fn terminal_ws(
    State(state): State<SharedState>,
    ws: WebSocketUpgrade,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    let ctx = ws_auth(&state, &params)?;
    ctx.require(Role::Operator)?;

    let requested = match params.get("session").or_else(|| params.get("id")) {
        Some(raw) => Some(
            Uuid::parse_str(raw)
                .map_err(|_| ApiError::BadRequest("invalid terminal session id".into()))?,
        ),
        None => None,
    };
    let since = params.get("since").and_then(|v| v.parse::<u64>().ok());
    let cols = params
        .get("cols")
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(120)
        .clamp(20, 500);
    let rows = params
        .get("rows")
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(30)
        .clamp(5, 300);

    let session = match requested {
        Some(id) => state
            .terminals
            .get(&ctx.username, id)
            .ok_or(ApiError::NotFound)?,
        None => {
            let (shell, args, workspace_root) = terminal_settings(&state);
            let workspace = files::ensure_user_root(&workspace_root, &ctx.username)
                .context("ユーザー作業フォルダを準備できません")
                .map_err(ApiError::from)?;
            state
                .terminals
                .create(&ctx.username, &shell, &args, &workspace, cols, rows, None)
                .map_err(ApiError::from)?
        }
    };

    let ephemeral = requested.is_none();
    if ephemeral {
        state.audit.log(
            &ctx.username,
            role_str(&ctx.role),
            "terminal_open",
            &format!("{} ({})", session.title(), session.id),
        );
    }

    let terminals = state.terminals.clone();
    let username = ctx.username.clone();
    let session_id = session.id;

    Ok(ws.on_upgrade(move |socket| async move {
        run_socket(socket, session, since).await;
        // Legacy clients that connect without a session id keep the old
        // one-shot behaviour instead of leaking a persistent shell.
        if ephemeral {
            terminals.remove(&username, session_id);
        }
    }))
}

pub(crate) fn terminal_settings(state: &SharedState) -> (String, Vec<String>, std::path::PathBuf) {
    let cfg = state.config.read().unwrap_or_else(|e| e.into_inner());
    (
        cfg.terminal.shell.clone(),
        cfg.terminal.args.clone(),
        cfg.workspace_root.clone(),
    )
}

async fn run_socket(socket: WebSocket, session: Arc<TerminalSession>, since: Option<u64>) {
    let (mut tx, mut rx) = socket.split();
    let (data, _attach_guard) = session.attach(since);
    let info = session.info();

    let ready = serde_json::json!({
        "type": "ready",
        "session": info,
        "offset": data.offset,
        "reset": data.reset,
        "exited": data.exited,
    });
    if tx
        .send(Message::Text(ready.to_string().into()))
        .await
        .is_err()
    {
        return;
    }

    // Replay the scrollback in bounded chunks. The ready frame told the
    // client which absolute offset the following bytes start at.
    for chunk in data.snapshot.chunks(32 * 1024) {
        if tx
            .send(Message::Binary(chunk.to_vec().into()))
            .await
            .is_err()
        {
            return;
        }
    }

    if data.exited {
        let _ = tx
            .send(Message::Text(
                serde_json::json!({"type": "exit"}).to_string().into(),
            ))
            .await;
        return;
    }

    let mut events = data.receiver;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(20));
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
    heartbeat.tick().await; // consume the immediate first tick

    loop {
        tokio::select! {
            event = events.recv() => {
                match event {
                    Ok(TerminalEvent::Data(bytes)) => {
                        if tx.send(Message::Binary(bytes.to_vec().into())).await.is_err() {
                            break;
                        }
                    }
                    Ok(TerminalEvent::Exit) => {
                        let _ = tx
                            .send(Message::Text(
                                serde_json::json!({"type": "exit"}).to_string().into(),
                            ))
                            .await;
                        break;
                    }
                    Err(RecvError::Lagged(_)) => {
                        // The client is too slow to consume the live stream.
                        // Reconnecting makes it resume from a clean offset
                        // using the scrollback buffer.
                        let _ = tx
                            .send(Message::Text(
                                serde_json::json!({"type": "reconnect"}).to_string().into(),
                            ))
                            .await;
                        break;
                    }
                    Err(RecvError::Closed) => break,
                }
            }
            msg = rx.next() => {
                match msg {
                    Some(Ok(Message::Binary(bytes))) => {
                        session.write(&bytes).await;
                    }
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                            match value.get("type").and_then(|v| v.as_str()) {
                                Some("resize") => {
                                    let cols = value
                                        .get("cols")
                                        .and_then(|v| v.as_u64())
                                        .unwrap_or(80)
                                        .clamp(20, 500) as u16;
                                    let rows = value
                                        .get("rows")
                                        .and_then(|v| v.as_u64())
                                        .unwrap_or(24)
                                        .clamp(5, 300) as u16;
                                    session.resize(cols, rows).await;
                                }
                                Some("ping") => {
                                    if tx
                                        .send(Message::Text(
                                            serde_json::json!({"type": "pong"}).to_string().into(),
                                        ))
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    None | Some(Ok(Message::Close(_))) | Some(Err(_)) => break,
                    Some(Ok(_)) => {}
                }
            }
            _ = heartbeat.tick() => {
                // Application-level keepalive for tunnels/proxies (ngrok,
                // Cloudflare, etc.) that close idle WebSockets. Browsers
                // answer Ping frames automatically.
                if tx.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
        }
    }
}
