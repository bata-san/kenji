use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use tokio::sync::broadcast;
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

/// Bound on concurrent PTY sessions to protect the host.
pub const MAX_TERMINAL_SESSIONS: usize = 16;
/// Bound per user so one account cannot occupy every terminal slot.
pub const MAX_TERMINAL_SESSIONS_PER_USER: usize = 6;

/// How much recent output is kept for reconnecting clients.
const OUTPUT_BUFFER_LIMIT: usize = 2 * 1024 * 1024;
/// Number of PTY chunks buffered for a slow WebSocket subscriber.
const OUTPUT_BROADCAST_CAPACITY: usize = 1024;
const READ_CHUNK: usize = 8192;

#[derive(Debug, thiserror::Error)]
pub enum TerminalError {
    #[error("{0}")]
    Limit(String),
    #[error(transparent)]
    Open(#[from] anyhow::Error),
}

#[derive(Default)]
struct TerminalState {
    total: usize,
    per_user: HashMap<String, usize>,
    sessions: HashMap<Uuid, Arc<TerminalSession>>,
}

#[derive(Clone)]
pub struct TerminalManager {
    inner: Arc<Mutex<TerminalState>>,
}

impl TerminalManager {
    pub fn new() -> TerminalManager {
        TerminalManager {
            inner: Arc::new(Mutex::new(TerminalState::default())),
        }
    }

    /// Reserve a slot atomically and pick the lowest free tab number for this
    /// user. The returned guard releases the slot on drop.
    fn reserve_slot(&self, user: &str) -> Result<(SessionGuard, usize), TerminalError> {
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if state.total >= MAX_TERMINAL_SESSIONS {
            return Err(TerminalError::Limit(format!(
                "同時ターミナルセッション数の上限（{MAX_TERMINAL_SESSIONS}）に達しています"
            )));
        }
        let user_count = state.per_user.get(user).copied().unwrap_or(0);
        if user_count >= MAX_TERMINAL_SESSIONS_PER_USER {
            return Err(TerminalError::Limit(format!(
                "1ユーザーあたりのターミナル上限（{MAX_TERMINAL_SESSIONS_PER_USER}）に達しています"
            )));
        }

        let mut used = [false; MAX_TERMINAL_SESSIONS_PER_USER + 1];
        for session in state.sessions.values().filter(|s| s.owner == user) {
            if session.number <= MAX_TERMINAL_SESSIONS_PER_USER {
                used[session.number] = true;
            }
        }
        let number = (1..=MAX_TERMINAL_SESSIONS_PER_USER)
            .find(|n| !used[*n])
            .unwrap_or(1);

        state.total += 1;
        *state.per_user.entry(user.to_owned()).or_insert(0) += 1;
        Ok((
            SessionGuard {
                inner: self.inner.clone(),
                user: user.to_owned(),
            },
            number,
        ))
    }

    /// Open a new persistent shell session. The PTY keeps running even when
    /// every WebSocket client disconnects.
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        &self,
        user: &str,
        shell: &str,
        args: &[String],
        cwd: &Path,
        cols: u16,
        rows: u16,
        title: Option<String>,
    ) -> Result<Arc<TerminalSession>, TerminalError> {
        self.prune_exited();

        let (guard, number) = self.reserve_slot(user)?;
        let id = Uuid::new_v4();
        let title = title
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.chars().take(64).collect::<String>())
            .unwrap_or_else(|| format!("ターミナル {number}"));

        let session =
            TerminalSession::spawn(id, user, title, number, cwd, shell, args, cols, rows, guard)
                .map_err(TerminalError::Open)?;

        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        state.sessions.insert(id, session.clone());
        Ok(session)
    }

    /// All live sessions owned by `user`, oldest first.
    pub fn list(&self, user: &str) -> Vec<TerminalInfo> {
        self.prune_exited();
        let state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut sessions: Vec<TerminalInfo> = state
            .sessions
            .values()
            .filter(|s| s.owner == user)
            .map(|s| s.info())
            .collect();
        sessions.sort_by_key(|s| s.created_at);
        sessions
    }

    /// Look up a session without requiring it to still be alive. This lets an
    /// attached client receive the final `exit` notification after the shell
    /// exits.
    pub fn get(&self, user: &str, id: Uuid) -> Option<Arc<TerminalSession>> {
        let state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        state.sessions.get(&id).filter(|s| s.owner == user).cloned()
    }

    /// Terminate and remove a session. Returns false when it does not exist or
    /// belongs to another user.
    pub fn remove(&self, user: &str, id: Uuid) -> bool {
        let removed = {
            let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            match state.sessions.get(&id) {
                Some(session) if session.owner == user => state.sessions.remove(&id),
                _ => None,
            }
        };
        match removed {
            Some(session) => {
                session.shutdown();
                true
            }
            None => false,
        }
    }

    /// Terminate every shell. Used during graceful server shutdown so
    /// PowerShell processes do not outlive the service.
    pub fn shutdown_all(&self) {
        let sessions = {
            let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            state
                .sessions
                .drain()
                .map(|(_, session)| session)
                .collect::<Vec<_>>()
        };
        for session in &sessions {
            session.shutdown();
        }
    }

    /// Remove sessions whose shell has exited and release their slots.
    pub fn prune_exited(&self) -> usize {
        let removed = {
            let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let ids: Vec<Uuid> = state
                .sessions
                .iter()
                .filter(|(_, session)| session.is_exited())
                .map(|(id, _)| *id)
                .collect();
            ids.into_iter()
                .filter_map(|id| state.sessions.remove(&id))
                .collect::<Vec<_>>()
        };
        for session in &removed {
            session.shutdown();
        }
        removed.len()
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

#[derive(Clone, Debug)]
pub enum TerminalEvent {
    Data(Arc<[u8]>),
    Exit,
}

#[derive(Debug, Clone, Serialize)]
pub struct TerminalInfo {
    pub id: Uuid,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub cwd: String,
    pub attached: bool,
    pub exited: bool,
}

pub struct AttachData {
    pub snapshot: Vec<u8>,
    /// First byte offset represented by `snapshot`.
    pub offset: u64,
    /// True when the client must clear its terminal before consuming the
    /// snapshot (first attach, ring-buffer overrun, or server-side reset).
    pub reset: bool,
    pub exited: bool,
    pub receiver: broadcast::Receiver<TerminalEvent>,
}

/// Keeps the "attached client" count accurate for every disconnect path.
pub struct AttachGuard {
    session: Arc<TerminalSession>,
}

impl Drop for AttachGuard {
    fn drop(&mut self) {
        self.session.attached.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct TerminalSession {
    pub id: Uuid,
    pub owner: String,
    pub number: usize,
    pub created_at: DateTime<Utc>,
    cwd: PathBuf,
    title: Mutex<String>,
    output: Arc<Mutex<OutputScrollback>>,
    events: broadcast::Sender<TerminalEvent>,
    writer: AsyncMutex<Box<dyn Write + Send>>,
    master: AsyncMutex<Box<dyn MasterPty + Send>>,
    killer: Mutex<Option<Box<dyn ChildKiller + Send + Sync>>>,
    exited: Arc<AtomicBool>,
    attached: AtomicUsize,
    closed: AtomicBool,
    guard: Mutex<Option<SessionGuard>>,
}

impl TerminalSession {
    #[allow(clippy::too_many_arguments)]
    fn spawn(
        id: Uuid,
        owner: &str,
        title: String,
        number: usize,
        cwd: &Path,
        shell: &str,
        args: &[String],
        cols: u16,
        rows: u16,
        guard: SessionGuard,
    ) -> Result<Arc<TerminalSession>> {
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
        // Closing the slave side lets the reader observe EOF when the shell
        // exits instead of keeping the PTY open forever.
        drop(pair.slave);

        let reader = pair.master.try_clone_reader().context("PTY reader")?;
        let writer = pair.master.take_writer().context("PTY writer")?;
        let mut killer = child.clone_killer();
        drop(child);

        let output = Arc::new(Mutex::new(OutputScrollback::new(OUTPUT_BUFFER_LIMIT)));
        let (events, _) = broadcast::channel(OUTPUT_BROADCAST_CAPACITY);
        let exited = Arc::new(AtomicBool::new(false));

        let reader_output = output.clone();
        let reader_events = events.clone();
        let reader_exited = exited.clone();
        let thread = std::thread::Builder::new()
            .name(format!("kenji-pty-{id}"))
            .spawn(move || {
                let mut reader = reader;
                let mut buf = [0u8; READ_CHUNK];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let data: Arc<[u8]> = Arc::from(&buf[..n]);
                            let mut output =
                                reader_output.lock().unwrap_or_else(|e| e.into_inner());
                            output.push(data.as_ref());
                            // Send while the lock is held so a client that
                            // subscribes after taking its snapshot cannot
                            // receive this chunk twice (or miss it).
                            let _ = reader_events.send(TerminalEvent::Data(data));
                        }
                    }
                }

                let _output = reader_output.lock().unwrap_or_else(|e| e.into_inner());
                reader_exited.store(true, Ordering::SeqCst);
                let _ = reader_events.send(TerminalEvent::Exit);
            });

        if let Err(error) = thread {
            let _ = killer.kill();
            return Err(anyhow::Error::new(error).context("PTY reader thread"));
        }

        Ok(Arc::new(TerminalSession {
            id,
            owner: owner.to_owned(),
            number,
            created_at: Utc::now(),
            cwd: cwd.to_path_buf(),
            title: Mutex::new(title),
            output,
            events,
            writer: AsyncMutex::new(writer),
            master: AsyncMutex::new(pair.master),
            killer: Mutex::new(Some(killer)),
            exited,
            attached: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
            guard: Mutex::new(Some(guard)),
        }))
    }

    pub fn info(&self) -> TerminalInfo {
        TerminalInfo {
            id: self.id,
            title: self.title(),
            created_at: self.created_at,
            cwd: self.cwd.to_string_lossy().into_owned(),
            attached: self.attached.load(Ordering::SeqCst) > 0,
            exited: self.is_exited(),
        }
    }

    pub fn title(&self) -> String {
        self.title.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn is_exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }

    /// Subscribe and take a replay snapshot atomically with respect to PTY
    /// output. `since` is the last byte offset the client already has.
    pub fn attach(self: &Arc<Self>, since: Option<u64>) -> (AttachData, AttachGuard) {
        let output = self.output.lock().unwrap_or_else(|e| e.into_inner());
        // Subscribe before releasing the output lock: the reader thread appends
        // to the scrollback and publishes while holding the same lock, so
        // there is no gap or duplicate between replay and live output.
        let receiver = self.events.subscribe();
        let (snapshot, offset, reset) = output.snapshot(since);
        let exited = self.is_exited();
        drop(output);

        self.attached.fetch_add(1, Ordering::SeqCst);
        let guard = AttachGuard {
            session: self.clone(),
        };
        (
            AttachData {
                snapshot,
                offset,
                reset,
                exited,
                receiver,
            },
            guard,
        )
    }

    pub async fn write(&self, bytes: &[u8]) {
        let mut writer = self.writer.lock().await;
        let _ = writer.write_all(bytes);
        let _ = writer.flush();
    }

    pub async fn resize(&self, cols: u16, rows: u16) {
        let master = self.master.lock().await;
        let _ = master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    /// Kill the shell and release the concurrency slot. Safe to call more than
    /// once (the manager and Drop both do).
    pub fn shutdown(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Ok(mut killer) = self.killer.lock() {
            if let Some(mut killer) = killer.take() {
                let _ = killer.kill();
            }
        }
        if let Ok(mut guard) = self.guard.lock() {
            drop(guard.take());
        }
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Byte ring buffer with absolute offsets so reconnecting clients can resume
/// exactly where they left off.
struct OutputScrollback {
    data: VecDeque<u8>,
    start: u64,
    end: u64,
    limit: usize,
}

impl OutputScrollback {
    fn new(limit: usize) -> OutputScrollback {
        OutputScrollback {
            data: VecDeque::new(),
            start: 0,
            end: 0,
            limit,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.data.extend(bytes.iter().copied());
        self.end = self.end.saturating_add(bytes.len() as u64);
        while self.data.len() > self.limit {
            self.data.pop_front();
            self.start = self.start.saturating_add(1);
        }
        // Do not start a replay in the middle of a UTF-8 code point.
        while let Some(byte) = self.data.front().copied() {
            if byte & 0xC0 == 0x80 {
                self.data.pop_front();
                self.start = self.start.saturating_add(1);
            } else {
                break;
            }
        }
    }

    fn snapshot(&self, since: Option<u64>) -> (Vec<u8>, u64, bool) {
        match since {
            Some(offset) if offset >= self.start && offset <= self.end => {
                let skip = (offset - self.start) as usize;
                (
                    self.data.iter().skip(skip).copied().collect(),
                    offset,
                    false,
                )
            }
            _ => (self.data.iter().copied().collect(), self.start, true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn take_slot(mgr: &TerminalManager, user: &str) -> SessionGuard {
        mgr.reserve_slot(user).expect("slot").0
    }

    #[test]
    fn per_user_limit_and_release() {
        let mgr = TerminalManager::new();
        let mut guards = Vec::new();
        for _ in 0..MAX_TERMINAL_SESSIONS_PER_USER {
            guards.push(take_slot(&mgr, "alice"));
        }
        assert!(mgr.reserve_slot("alice").is_err());
        // Another user can still connect.
        let bob = take_slot(&mgr, "bob");
        drop(bob);
        // Releasing one slot lets the same user reconnect.
        guards.pop();
        assert!(mgr.reserve_slot("alice").is_ok());
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
                guards.push(take_slot(&mgr, &format!("user{i}")));
            }
        }
        assert_eq!(guards.len(), MAX_TERMINAL_SESSIONS);
        assert!(mgr.reserve_slot("extra").is_err());
    }

    #[test]
    fn scrollback_replays_from_offset() {
        let mut buffer = OutputScrollback::new(8);
        buffer.push(b"abc");
        let (snapshot, offset, reset) = buffer.snapshot(Some(1));
        assert_eq!(snapshot, b"bc");
        assert_eq!(offset, 1);
        assert!(!reset);

        buffer.push(b"defghij");
        // The oldest bytes were trimmed, so the client must reset.
        let (snapshot, offset, reset) = buffer.snapshot(Some(1));
        assert!(reset);
        assert_eq!(offset, buffer.start);
        assert_eq!(snapshot.len(), buffer.data.len());
    }

    #[test]
    fn scrollback_never_starts_mid_utf8() {
        let mut buffer = OutputScrollback::new(2);
        buffer.push("あ".as_bytes()); // 3 bytes
                                      // Only the last two bytes remain, which are UTF-8 continuation bytes.
        assert!(buffer.data.is_empty());
        assert_eq!(buffer.end, 3);
    }
}
