use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use tokio::sync::Mutex;

/// Bound on concurrent PTY sessions to protect the host.
pub const MAX_TERMINAL_SESSIONS: usize = 4;

pub struct TerminalManager {
    active: Arc<AtomicUsize>,
}

impl TerminalManager {
    pub fn new() -> TerminalManager {
        TerminalManager {
            active: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn try_acquire(&self) -> Result<SessionGuard> {
        let now = self.active.fetch_add(1, Ordering::SeqCst);
        if now >= MAX_TERMINAL_SESSIONS {
            self.active.fetch_sub(1, Ordering::SeqCst);
            return Err(anyhow::anyhow!(
                "同時ターミナルセッション数の上限に達しています"
            ));
        }
        Ok(SessionGuard {
            active: self.active.clone(),
        })
    }
}

pub struct SessionGuard {
    active: Arc<AtomicUsize>,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct PtySession {
    pub reader: Box<dyn Read + Send>,
    pub writer: Mutex<Box<dyn Write + Send>>,
    pub master: Mutex<Box<dyn MasterPty + Send>>,
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
            writer: Mutex::new(writer),
            master: Mutex::new(pair.master),
            killer,
        })
    }
}
