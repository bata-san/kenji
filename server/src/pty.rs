use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use tokio::sync::Mutex as AsyncMutex;

/// Bound on concurrent PTY sessions to protect the host.
pub const MAX_TERMINAL_SESSIONS: usize = 8;
/// Bound per user so one account cannot occupy every terminal slot.
pub const MAX_TERMINAL_SESSIONS_PER_USER: usize = 4;

#[derive(Default)]
struct TerminalState {
    total: usize,
    per_user: HashMap<String, usize>,
}

pub struct TerminalManager {
    inner: Arc<Mutex<TerminalState>>,
}

impl TerminalManager {
    pub fn new() -> TerminalManager {
        TerminalManager {
            inner: Arc::new(Mutex::new(TerminalState::default())),
        }
    }

    /// Acquire a terminal slot for `user`. The returned guard releases the
    /// slot on drop (including when the WebSocket disconnects abruptly).
    pub fn try_acquire(&self, user: &str) -> Result<SessionGuard> {
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if state.total >= MAX_TERMINAL_SESSIONS {
            return Err(anyhow::anyhow!(
                "同時ターミナルセッション数の上限（{MAX_TERMINAL_SESSIONS}）に達しています"
            ));
        }
        let user_count = state.per_user.get(user).copied().unwrap_or(0);
        if user_count >= MAX_TERMINAL_SESSIONS_PER_USER {
            return Err(anyhow::anyhow!(
                "1ユーザーあたりのターミナル上限（{MAX_TERMINAL_SESSIONS_PER_USER}）に達しています"
            ));
        }
        state.total += 1;
        *state.per_user.entry(user.to_owned()).or_insert(0) += 1;
        Ok(SessionGuard {
            inner: self.inner.clone(),
            user: user.to_owned(),
        })
    }
}

pub struct SessionGuard {
    inner: Arc<Mutex<TerminalState>>,
    user: String,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        state.total = state.total.saturating_sub(1);
        if let Some(count) = state.per_user.get_mut(&self.user) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                state.per_user.remove(&self.user);
            }
        }
    }
}

pub struct PtySession {
    pub reader: Box<dyn Read + Send>,
    pub writer: AsyncMutex<Box<dyn Write + Send>>,
    pub master: AsyncMutex<Box<dyn MasterPty + Send>>,
    pub killer: Box<dyn ChildKiller + Send + Sync>,
}

impl PtySession {
    /// Open a PTY running the configured shell.
    pub fn open(
        shell: &str,
        args: &[String],
        cwd: &std::path::Path,
        cols: u16,
        rows: u16,
    ) -> Result<PtySession> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("PTY を開けませんでした")?;
        let mut cmd = CommandBuilder::new(shell);
        cmd.args(args);
        cmd.cwd(cwd);
        let child = pair
            .slave
            .spawn_command(cmd)
            .context("シェルを起動できませんでした")?;
        let reader = pair.master.try_clone_reader().context("PTY reader")?;
        let writer = pair.master.take_writer().context("PTY writer")?;
        let killer = child.clone_killer();
        Ok(PtySession {
            reader,
            writer: AsyncMutex::new(writer),
            master: AsyncMutex::new(pair.master),
            killer,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_user_limit_and_release() {
        let mgr = TerminalManager::new();
        let mut guards = Vec::new();
        for _ in 0..MAX_TERMINAL_SESSIONS_PER_USER {
            guards.push(mgr.try_acquire("alice").unwrap());
        }
        assert!(mgr.try_acquire("alice").is_err());
        // Another user can still connect.
        let bob = mgr.try_acquire("bob").unwrap();
        drop(bob);
        // Releasing one slot lets the same user reconnect.
        guards.pop();
        assert!(mgr.try_acquire("alice").is_ok());
    }

    #[test]
    fn global_limit_is_enforced() {
        let mgr = TerminalManager::new();
        let per_user = MAX_TERMINAL_SESSIONS_PER_USER;
        let users = MAX_TERMINAL_SESSIONS.div_ceil(per_user);
        let mut guards = Vec::new();
        for i in 0..users {
            for _ in 0..per_user {
                if guards.len() == MAX_TERMINAL_SESSIONS {
                    break;
                }
                guards.push(mgr.try_acquire(&format!("user{i}")).unwrap());
            }
        }
        assert_eq!(guards.len(), MAX_TERMINAL_SESSIONS);
        assert!(mgr.try_acquire("extra").is_err());
    }
}
