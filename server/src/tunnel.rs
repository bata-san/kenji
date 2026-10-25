use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tokio::io::AsyncReadExt;
use tokio::process::Child;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TunnelState {
    Disabled,
    Starting,
    Running,
    Stopped,
    Unavailable,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct TunnelInfo {
    pub available: bool,
    pub enabled: bool,
    pub url: Option<String>,
    pub status: TunnelState,
    pub error: Option<String>,
}

struct Inner {
    enabled: bool,
    state: TunnelState,
    url: Option<String>,
    error: Option<String>,
    child: Option<Child>,
    available: bool,
}

pub struct TunnelSupervisor {
    inner: Mutex<Inner>,
    data_dir: PathBuf,
}

impl TunnelSupervisor {
    pub fn new(data_dir: PathBuf) -> Arc<TunnelSupervisor> {
        Arc::new(TunnelSupervisor {
            inner: Mutex::new(Inner {
                enabled: false,
                state: TunnelState::Disabled,
                url: None,
                error: None,
                child: None,
                available: false,
            }),
            data_dir,
        })
    }

    /// Detect cloudflared (bundled copy or on PATH) without blocking long.
    pub fn detect_available(&self) -> bool {
        let exe = self.find_exe();
        let ok = match exe {
            Some(exe) => std::process::Command::new(&exe)
                .arg("--version")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false),
            None => false,
        };
        self.inner.lock().unwrap().available = ok;
        ok
    }

    fn find_exe(&self) -> Option<PathBuf> {
        let bundled = self.data_dir.join("bin").join("cloudflared.exe");
        if bundled.exists() {
            return Some(bundled);
        }
        // rely on PATH resolution
        Some(PathBuf::from("cloudflared"))
    }

    pub fn info(&self) -> TunnelInfo {
        let i = self.inner.lock().unwrap();
        TunnelInfo {
            available: i.available,
            enabled: i.enabled,
            url: i.url.clone(),
            status: i.state,
            error: i.error.clone(),
        }
    }

    /// Start (or restart) the managed quick tunnel. No-op if unavailable.
    pub fn start(self: &Arc<Self>, port: u16) {
        self.detect_available();
        {
            let mut i = self.inner.lock().unwrap();
            if !i.available {
                i.enabled = false;
                i.state = TunnelState::Unavailable;
                i.error = Some("cloudflared が見つかりません。PATH または data/bin/cloudflared.exe に設置してください".into());
                return;
            }
            i.enabled = true;
            i.state = TunnelState::Starting;
            i.url = None;
            i.error = None;
        }
        let sup = self.clone();
        tokio::spawn(async move {
            sup.supervise(port).await;
        });
    }

    /// Stop the tunnel and mark disabled.
    pub fn stop(&self) {
        let mut i = self.inner.lock().unwrap();
        i.enabled = false;
        i.url = None;
        i.state = TunnelState::Disabled;
        i.error = None;
        if let Some(mut child) = i.child.take() {
            let _ = child.start_kill();
        }
    }

    async fn supervise(self: Arc<Self>, port: u16) {
        loop {
            let (enabled, exe) = {
                let i = self.inner.lock().unwrap();
                (i.enabled, self.find_exe())
            };
            if !enabled {
                break;
            }
            let Some(exe) = exe else { break };

            let url_target = format!("http://127.0.0.1:{port}");
            let child = tokio::process::Command::new(&exe)
                .args(["tunnel", "--no-autoupdate", "--url", &url_target])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn();

            let mut child = match child {
                Ok(c) => c,
                Err(e) => {
                    let mut i = self.inner.lock().unwrap();
                    i.state = TunnelState::Error;
                    i.error = Some(format!("cloudflared の起動に失敗: {e}"));
                    break;
                }
            };
            {
                let mut i = self.inner.lock().unwrap();
                i.state = TunnelState::Starting;
                i.url = None;
                i.error = None;
            }
            let stdout = child.stdout.take();
            let stderr = child.stderr.take();
            {
                let mut i = self.inner.lock().unwrap();
                i.child = Some(child);
            }
            let sup = self.clone();
            if let Some(s) = stdout {
                tokio::spawn(scan_output(sup.clone(), s));
            }
            let sup = self.clone();
            if let Some(s) = stderr {
                tokio::spawn(scan_output(sup, s));
            }

            // Wait for the child to exit (poll; stop() may take and kill it meanwhile).
            let exit: Option<std::process::ExitStatus> = loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let mut i = self.inner.lock().unwrap();
                match i.child.as_mut().map(|c| c.try_wait()) {
                    Some(Ok(Some(status))) => break Some(status),
                    Some(Err(_)) | None => break None,
                    Some(Ok(None)) => {}
                }
            };
            {
                let mut i = self.inner.lock().unwrap();
                i.child = None;
                if !i.enabled {
                    break;
                }
                i.state = TunnelState::Stopped;
                i.error = Some(format!("cloudflared が終了しました {exit:?}。{RESTART_DELAY_SECS} 秒後に再起動します"));
            }
            tracing::warn!("tunnel exited {exit:?}; restarting in {RESTART_DELAY_SECS}s");
            tokio::time::sleep(Duration::from_secs(RESTART_DELAY_SECS)).await;
        }
        // final state sync if not explicitly stopped
        let mut i = self.inner.lock().unwrap();
        if !i.enabled {
            i.state = TunnelState::Disabled;
        }
    }
}

const RESTART_DELAY_SECS: u64 = 10;

async fn scan_output(sup: Arc<TunnelSupervisor>, mut stream: impl tokio::io::AsyncRead + Unpin) {
    let mut buf = Vec::<u8>::new();
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf).into_owned();
                if let Some(url) = extract_tunnel_url(&text) {
                    let mut i = sup.inner.lock().unwrap();
                    i.url = Some(url);
                    i.state = TunnelState::Running;
                    i.error = None;
                    tracing::info!("tunnel running: {}", i.url.as_deref().unwrap_or_default());
                    return;
                }
                // keep only the tail to bound memory
                if buf.len() > 256 * 1024 {
                    buf.drain(0..buf.len() - 4096);
                }
            }
        }
    }
}

fn extract_tunnel_url(text: &str) -> Option<String> {
    for tok in text.split(|c: char| c.is_whitespace() || c == '|' || c == '"') {
        if let Some(host) = tok.strip_prefix("https://") {
            if let Some(host) = host.strip_suffix(".trycloudflare.com") {
                if !host.is_empty()
                    && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                {
                    return Some(format!("https://{host}.trycloudflare.com"));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_quick_tunnel_url() {
        let sample = "2026-10-25T10:00:00Z INF |  https://abc-def-xyz.trycloudflare.com |";
        assert_eq!(
            extract_tunnel_url(sample).as_deref(),
            Some("https://abc-def-xyz.trycloudflare.com")
        );
        assert_eq!(extract_tunnel_url("no url here"), None);
        assert_eq!(
            extract_tunnel_url("bad https://x$.trycloudflare.com nope").is_none(),
            true
        );
    }
}
