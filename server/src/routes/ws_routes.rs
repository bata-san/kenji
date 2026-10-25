use std::collections::HashMap;
use std::io::{Read, Write};

use axum::extract::{Query, State, WebSocketUpgrade};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use portable_pty::PtySize;
use tokio::sync::mpsc;

use crate::auth::Role;
use crate::error::{ApiError, ApiResult};
use crate::pty::PtySession;
use crate::routes::ws_auth;
use crate::state::{role_str, SharedState};

/// Interactive PowerShell over ConPTY <-> WebSocket (binary frames = PTY I/O).
pub async fn terminal_ws(
    State(state): State<SharedState>,
    ws: WebSocketUpgrade,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    let ctx = ws_auth(&state, &params)?;
    ctx.require(Role::Operator)?;

    let guard = state
        .terminals
        .try_acquire()
        .map_err(|e| ApiError::Conflict(e.to_string()))?;

    let (shell, args, workspace) = {
        let cfg = state.config.read().unwrap_or_else(|e| e.into_inner());
        (
            cfg.terminal.shell.clone(),
            cfg.terminal.args.clone(),
            cfg.workspace_root.clone(),
        )
    };
    let session = PtySession::open(&shell, &args, &workspace, 80, 24).map_err(ApiError::from)?;
    state
        .audit
        .log(&ctx.username, role_str(&ctx.role), "terminal_open", &shell);

    Ok(ws.on_upgrade(move |socket| async move {
        let _guard = guard; // releases the session slot on drop

        let PtySession {
            reader,
            writer,
            master,
            mut killer,
        } = session;
        let (mut ws_tx, mut ws_rx) = socket.split();

        // PTY reader → client (blocking reads on a dedicated thread).
        let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(64);
        tokio::task::spawn_blocking(move || {
            let mut reader = reader;
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if out_tx.blocking_send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        // Forward PTY output; on EOF notify the client.
        let forward = tokio::spawn(async move {
            while let Some(bytes) = out_rx.recv().await {
                if ws_tx
                    .send(axum::extract::ws::Message::Binary(bytes.into()))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            // channel closed ⇒ PTY exited
            let _ = ws_tx
                .send(axum::extract::ws::Message::Text(
                    serde_json::json!({"type": "exit"}).to_string().into(),
                ))
                .await;
        });

        // Client → PTY.
        loop {
            match ws_rx.next().await {
                Some(Ok(axum::extract::ws::Message::Binary(b))) => {
                    let mut w = writer.lock().await;
                    let _ = w.write_all(&b);
                    let _ = w.flush();
                }
                Some(Ok(axum::extract::ws::Message::Text(t))) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
                        if v["type"] == "resize" {
                            let cols = v["cols"].as_u64().unwrap_or(80) as u16;
                            let rows = v["rows"].as_u64().unwrap_or(24) as u16;
                            let m = master.lock().await;
                            let _ = m.resize(PtySize {
                                rows,
                                cols,
                                pixel_width: 0,
                                pixel_height: 0,
                            });
                        }
                    }
                }
                None | Some(Ok(axum::extract::ws::Message::Close(_))) | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            }
        }

        // Cleanup: make sure the shell dies with the socket.
        let _ = killer.kill();
        forward.abort();
    }))
}
