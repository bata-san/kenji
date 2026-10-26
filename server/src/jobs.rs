use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use cron::Schedule;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{broadcast, Mutex as AsyncMutex};
use uuid::Uuid;

use crate::audit::Audit;

const RING_CAP_BYTES: usize = 64 * 1024;
const LOG_ROTATE_BYTES: u64 = 5 * 1024 * 1024;
const RESTART_DELAY: Duration = Duration::from_secs(5);
const MAX_RESTART_ATTEMPTS: u32 = 5;
const UPTIME_RESET: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RestartPolicy {
    #[default]
    Never,
    OnFailure,
    Always,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobDef {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub restart_policy: RestartPolicy,
    #[serde(default)]
    pub schedule: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Username that created the job. Empty for jobs created before multi-user
    /// support; those remain manageable by any operator for compatibility.
    #[serde(default)]
    pub owner: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Running,
    Stopped,
    Failed,
    Stopping,
}

#[derive(Serialize)]
pub struct JobView {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: HashMap<String, String>,
    pub restart_policy: RestartPolicy,
    pub schedule: Option<String>,
    pub tags: Vec<String>,
    pub owner: String,
    pub status: JobStatus,
    pub pid: Option<u32>,
    pub exit_code: Option<i32>,
    pub last_error: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub stopped_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub schedule_next_at: Option<DateTime<Utc>>,
}

struct LogFile {
    file: tokio::fs::File,
    size: u64,
}

/// Mutable runtime state for one job (not persisted; status is rebuilt on boot).
pub struct JobRuntime {
    pub status: RwLock<JobStatus>,
    pub pid: RwLock<Option<u32>>,
    pub started_at: RwLock<Option<DateTime<Utc>>>,
    pub stopped_at: RwLock<Option<DateTime<Utc>>>,
    pub exit_code: RwLock<Option<i32>>,
    pub last_error: RwLock<Option<String>>,
    pub manual_stop: AtomicBool,
    pub restart_attempts: RwLock<u32>,
    pub next_run: RwLock<Option<DateTime<Utc>>>,
    supervisor_active: AtomicBool,
    ring: Mutex<VecDeque<u8>>,
    log_tx: broadcast::Sender<String>,
    log_path: PathBuf,
    log_file: AsyncMutex<Option<LogFile>>,
}

impl JobRuntime {
    fn new(log_path: PathBuf) -> Arc<JobRuntime> {
        let (tx, _) = broadcast::channel(256);
        Arc::new(JobRuntime {
            status: RwLock::new(JobStatus::Stopped),
            pid: RwLock::new(None),
            started_at: RwLock::new(None),
            stopped_at: RwLock::new(None),
            exit_code: RwLock::new(None),
            last_error: RwLock::new(None),
            manual_stop: AtomicBool::new(false),
            restart_attempts: RwLock::new(0),
            next_run: RwLock::new(None),
            supervisor_active: AtomicBool::new(false),
            ring: Mutex::new(VecDeque::new()),
            log_tx: tx,
            log_path,
            log_file: AsyncMutex::new(None),
        })
    }

    fn set_running(&self, pid: u32) {
        *self.status.write().unwrap_or_else(|e| e.into_inner()) = JobStatus::Running;
        *self.pid.write().unwrap_or_else(|e| e.into_inner()) = Some(pid);
        *self.started_at.write().unwrap_or_else(|e| e.into_inner()) = Some(Utc::now());
        *self.stopped_at.write().unwrap_or_else(|e| e.into_inner()) = None;
        *self.exit_code.write().unwrap_or_else(|e| e.into_inner()) = None;
    }

    fn mark_exit(&self, code: Option<i32>) {
        *self.pid.write().unwrap_or_else(|e| e.into_inner()) = None;
        *self.stopped_at.write().unwrap_or_else(|e| e.into_inner()) = Some(Utc::now());
        *self.exit_code.write().unwrap_or_else(|e| e.into_inner()) = code;
        let manual = self.manual_stop.load(Ordering::Relaxed);
        *self.status.write().unwrap_or_else(|e| e.into_inner()) = if manual || code == Some(0) {
            JobStatus::Stopped
        } else {
            JobStatus::Failed
        };
        if let Some(c) = code.filter(|c| *c != 0) {
            *self.last_error.write().unwrap_or_else(|e| e.into_inner()) =
                Some(format!("exit code {c}"));
        }
    }

    fn set_failed(&self, msg: String) {
        *self.status.write().unwrap_or_else(|e| e.into_inner()) = JobStatus::Failed;
        *self.last_error.write().unwrap_or_else(|e| e.into_inner()) = Some(msg);
        *self.pid.write().unwrap_or_else(|e| e.into_inner()) = None;
        *self.stopped_at.write().unwrap_or_else(|e| e.into_inner()) = Some(Utc::now());
    }

    pub async fn append_log(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let text = String::from_utf8_lossy(bytes).into_owned();
        {
            let mut ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
            ring.extend(bytes.iter().copied());
            while ring.len() > RING_CAP_BYTES {
                ring.pop_front();
            }
        }
        let _ = self.log_tx.send(text);

        let mut guard = self.log_file.lock().await;
        // lazily open
        if guard.is_none() {
            if let Some(parent) = self.log_path.parent() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
            match tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.log_path)
                .await
            {
                Ok(f) => {
                    let size = f.metadata().await.map(|m| m.len()).unwrap_or(0);
                    *guard = Some(LogFile { file: f, size });
                }
                Err(e) => {
                    tracing::warn!("cannot open log file {}: {e}", self.log_path.display());
                    return;
                }
            }
        }
        if let Some(lf) = guard.as_mut() {
            if lf.size > LOG_ROTATE_BYTES {
                // rotate: drop current handle, rename, reopen
                *guard = None;
                let rotated = self.log_path.with_extension("log.1");
                let _ = tokio::fs::remove_file(&rotated).await;
                if let Err(e) = tokio::fs::rename(&self.log_path, &rotated).await {
                    tracing::warn!("log rotate failed: {e}");
                }
                match tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.log_path)
                    .await
                {
                    Ok(f) => {
                        *guard = Some(LogFile { file: f, size: 0 });
                    }
                    Err(_) => return,
                }
            }
            let Some(lf) = guard.as_mut() else { return };
            if lf.file.write_all(bytes).await.is_ok() {
                lf.size += bytes.len() as u64;
            }
        }
    }

    /// Recent in-memory log tail as lossy UTF-8.
    pub fn tail(&self, max_bytes: usize) -> String {
        let ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        let take = max_bytes.min(ring.len());
        let start = ring.len() - take;
        let data: Vec<u8> = ring.iter().skip(start).copied().collect();
        String::from_utf8_lossy(&data).into_owned()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.log_tx.subscribe()
    }
}

pub struct JobManager {
    defs: RwLock<Vec<JobDef>>,
    runtimes: Mutex<HashMap<Uuid, Arc<JobRuntime>>>,
    defs_path: PathBuf,
    logs_dir: PathBuf,
    pub workspace_root: PathBuf,
    audit: Arc<Audit>,
    pub resources: Arc<crate::resources::ResourceGuard>,
}

impl JobManager {
    pub fn new(
        data_dir: &std::path::Path,
        workspace_root: PathBuf,
        audit: Arc<Audit>,
        resources: Arc<crate::resources::ResourceGuard>,
    ) -> Arc<JobManager> {
        Arc::new(JobManager {
            defs: RwLock::new(Vec::new()),
            runtimes: Mutex::new(HashMap::new()),
            defs_path: data_dir.join("jobs.json"),
            logs_dir: data_dir.join("logs"),
            workspace_root,
            audit,
            resources,
        })
    }

    pub fn load(self: &Arc<Self>) -> Result<()> {
        std::fs::create_dir_all(&self.logs_dir)?;
        let mut defs: Vec<JobDef> = Vec::new();
        if self.defs_path.exists() {
            let raw = std::fs::read_to_string(&self.defs_path)?;
            defs = serde_json::from_str(&raw).context("parsing jobs.json")?;
        }
        {
            let mut runtimes = self.runtimes.lock().unwrap_or_else(|e| e.into_inner());
            for def in &defs {
                runtimes.insert(def.id, JobRuntime::new(self.log_path(def.id)));
            }
        }
        *self.defs.write().unwrap_or_else(|e| e.into_inner()) = defs;
        // Restart "always" jobs (resident services) and arm schedules.
        let defs = self.defs.read().unwrap_or_else(|e| e.into_inner()).clone();
        for def in defs {
            let Some(rt) = self.runtime(&def.id) else {
                tracing::error!("ジョブ {} のランタイムが見つかりません（スキップ）", def.id);
                continue;
            };
            if let Some(schedule) = &def.schedule {
                let next = next_fire(schedule, Utc::now());
                *rt.next_run.write().unwrap_or_else(|e| e.into_inner()) = next;
            }
            if def.restart_policy == RestartPolicy::Always {
                self.audit.log(
                    "system",
                    "system",
                    "job_start",
                    &format!("{} (boot auto-start)", def.name),
                );
                self.spawn_supervisor(def.id);
            }
        }
        Ok(())
    }

    fn log_path(&self, id: Uuid) -> PathBuf {
        self.logs_dir.join(format!("{id}.log"))
    }

    fn runtime(&self, id: &Uuid) -> Option<Arc<JobRuntime>> {
        self.runtimes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }

    fn persist(&self) -> Result<()> {
        // Serialize concurrent snapshots and disk writes under one lock. A shared
        // fixed temp path must never be written concurrently by API requests.
        let defs = self.defs.write().unwrap_or_else(|e| e.into_inner());
        let raw = serde_json::to_string_pretty(&*defs)?;
        let tmp = self.defs_path.with_extension("json.tmp");
        use std::io::Write;
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(raw.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, &self.defs_path)?;
        Ok(())
    }

    // ---- API operations ----

    pub fn create(
        self: &Arc<Self>,
        user: &str,
        role: &str,
        req: CreateJobRequest,
    ) -> Result<JobView> {
        let name = req.name.trim();
        if name.is_empty() || name.len() > 64 {
            return Err(anyhow!("名前は1〜64文字で入力してください"));
        }
        if req.command.trim().is_empty() {
            return Err(anyhow!("コマンドを指定してください"));
        }
        if let Some(schedule) = &req.schedule {
            parse_cron(schedule).map_err(|e| anyhow!("スケジュール形式が不正です: {e}"))?;
        }
        // Reject a relative cwd that escapes the owner's folder at creation
        // time. Absolute paths keep their existing semantics.
        if let Some(cwd) = &req.cwd {
            if !std::path::Path::new(cwd).is_absolute() {
                let base = crate::files::ensure_user_root(&self.workspace_root, user)?;
                let p = crate::files::resolve_under_root(&base, cwd)
                    .map_err(|e| anyhow!("作業ディレクトリが不正です: {e}"))?;
                crate::files::ensure_within(&base, &p)
                    .map_err(|e| anyhow!("作業ディレクトリが作業フォルダ外です: {e}"))?;
            }
        }
        let def = JobDef {
            id: Uuid::new_v4(),
            name: name.to_string(),
            description: req.description.unwrap_or_default(),
            command: req.command.trim().to_string(),
            args: req.args.unwrap_or_default(),
            cwd: req.cwd,
            env: req.env.unwrap_or_default(),
            restart_policy: req.restart_policy.unwrap_or(RestartPolicy::Never),
            schedule: req.schedule,
            tags: req.tags.unwrap_or_default(),
            owner: user.to_string(),
            created_at: Utc::now(),
        };
        self.defs
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .push(def.clone());
        self.persist()?;
        {
            let mut runtimes = self.runtimes.lock().unwrap_or_else(|e| e.into_inner());
            runtimes.insert(def.id, JobRuntime::new(self.log_path(def.id)));
        }
        let rt = self.runtime(&def.id);
        if let (Some(schedule), Some(rt)) = (&def.schedule, rt.as_ref()) {
            *rt.next_run.write().unwrap_or_else(|e| e.into_inner()) =
                next_fire(schedule, Utc::now());
        }
        self.audit.log(
            user,
            role,
            "job_create",
            &format!("{} ({})", def.name, def.command),
        );
        self.spawn_supervisor(def.id);
        self.view(&def.id)
            .ok_or_else(|| anyhow!("作成直後のジョブが参照できません（内部エラー）"))
    }

    pub fn list(&self) -> Vec<JobView> {
        let defs = self.defs.read().unwrap_or_else(|e| e.into_inner()).clone();
        defs.iter().filter_map(|d| self.view(&d.id)).collect()
    }

    pub fn get(&self, id: &Uuid) -> Option<JobView> {
        self.view(id)
    }

    pub async fn start(
        self: &Arc<Self>,
        user: &str,
        role: &str,
        id: &Uuid,
    ) -> Result<JobView, String> {
        let def = self
            .defs
            .read()
            .unwrap()
            .iter()
            .find(|d| d.id == *id)
            .cloned()
            .ok_or_else(|| "job not found".to_string())?;
        ensure_can_manage(user, role, &def)?;
        let rt = self
            .runtime(id)
            .ok_or_else(|| "job not found".to_string())?;
        if *rt.status.read().unwrap_or_else(|e| e.into_inner()) == JobStatus::Running {
            return Err("already running".to_string());
        }
        rt.manual_stop.store(false, Ordering::Relaxed);
        *rt.restart_attempts
            .write()
            .unwrap_or_else(|e| e.into_inner()) = 0;
        self.audit.log(user, role, "job_start", &def.name);
        // A previous supervisor may still be winding down (restart delay); retry briefly.
        let mut spawned = false;
        for _ in 0..30 {
            if self.spawn_supervisor_with_flag(*id) {
                spawned = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if !spawned {
            return Err("前回の停止処理が完了していません。数秒後に再試行してください".to_string());
        }
        self.view(id).ok_or_else(|| "job not found".to_string())
    }

    pub fn stop(&self, user: &str, role: &str, id: &Uuid) -> Result<JobView, String> {
        let def = self
            .defs
            .read()
            .unwrap()
            .iter()
            .find(|d| d.id == *id)
            .cloned()
            .ok_or_else(|| "job not found".to_string())?;
        ensure_can_manage(user, role, &def)?;
        let rt = self
            .runtime(id)
            .ok_or_else(|| "job not found".to_string())?;
        rt.manual_stop.store(true, Ordering::Relaxed);
        if *rt.status.read().unwrap_or_else(|e| e.into_inner()) == JobStatus::Running {
            *rt.status.write().unwrap_or_else(|e| e.into_inner()) = JobStatus::Stopping;
            if let Some(pid) = *rt.pid.read().unwrap_or_else(|e| e.into_inner()) {
                kill_tree(pid);
            }
        }
        self.audit.log(user, role, "job_stop", &def.name);
        self.view(id).ok_or_else(|| "job not found".to_string())
    }

    pub fn delete(self: &Arc<Self>, user: &str, role: &str, id: &Uuid) -> Result<(), String> {
        let def = self
            .defs
            .read()
            .unwrap()
            .iter()
            .find(|d| d.id == *id)
            .cloned()
            .ok_or_else(|| "job not found".to_string())?;
        ensure_can_manage(user, role, &def)?;
        let rt = self
            .runtime(id)
            .ok_or_else(|| "job not found".to_string())?;
        rt.manual_stop.store(true, Ordering::Relaxed);
        if let Some(pid) = *rt.pid.read().unwrap_or_else(|e| e.into_inner()) {
            kill_tree(pid);
        }
        self.defs
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|d| d.id != *id);
        if let Err(e) = self.persist() {
            tracing::error!("persist after delete failed: {e:#}");
        }
        self.runtimes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        self.audit.log(user, role, "job_delete", &def.name);
        Ok(())
    }

    /// Kill all running children (graceful server shutdown).
    pub fn stop_all(&self) {
        let runtimes = self
            .runtimes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        for (id, rt) in runtimes {
            rt.manual_stop.store(true, Ordering::Relaxed);
            if let Some(pid) = *rt.pid.read().unwrap_or_else(|e| e.into_inner()) {
                tracing::info!("stopping job {id} (pid {pid}) on shutdown");
                kill_tree(pid);
            }
        }
    }

    pub fn summary(&self) -> (usize, usize, usize) {
        let defs = self.defs.read().unwrap_or_else(|e| e.into_inner());
        let total = defs.len();
        let mut running = 0;
        let mut failed = 0;
        let runtimes = self.runtimes.lock().unwrap_or_else(|e| e.into_inner());
        for d in defs.iter() {
            if let Some(rt) = runtimes.get(&d.id) {
                match *rt.status.read().unwrap_or_else(|e| e.into_inner()) {
                    JobStatus::Running | JobStatus::Stopping => running += 1,
                    JobStatus::Failed => failed += 1,
                    JobStatus::Stopped => {}
                }
            }
        }
        (running, total, failed)
    }

    pub fn view(&self, id: &Uuid) -> Option<JobView> {
        let def = self
            .defs
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|d| d.id == *id)
            .cloned()?;
        let rt = self
            .runtimes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()?;
        let v = JobView {
            id: def.id,
            name: def.name,
            description: def.description,
            command: def.command,
            args: def.args,
            cwd: def.cwd,
            env: def.env,
            restart_policy: def.restart_policy,
            schedule: def.schedule,
            tags: def.tags,
            owner: def.owner,
            status: *rt.status.read().unwrap_or_else(|e| e.into_inner()),
            pid: *rt.pid.read().unwrap_or_else(|e| e.into_inner()),
            exit_code: *rt.exit_code.read().unwrap_or_else(|e| e.into_inner()),
            last_error: rt
                .last_error
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            started_at: *rt.started_at.read().unwrap_or_else(|e| e.into_inner()),
            stopped_at: *rt.stopped_at.read().unwrap_or_else(|e| e.into_inner()),
            created_at: def.created_at,
            schedule_next_at: *rt.next_run.read().unwrap_or_else(|e| e.into_inner()),
        };
        Some(v)
    }

    pub fn runtime_if_exists(&self, id: &Uuid) -> Option<Arc<JobRuntime>> {
        self.runtimes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }

    // ---- Scheduler ----

    pub fn start_scheduler(self: &Arc<Self>) {
        let mgr = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(30));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                // パニックでスケジューラタスク全体が停止しないよう、1ティック分を隔離する
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let now = Utc::now();
                    let defs = mgr.defs.read().unwrap_or_else(|e| e.into_inner()).clone();
                    for def in defs {
                        let Some(schedule) = def.schedule.clone() else {
                            continue;
                        };
                        let Some(rt) = mgr.runtime_if_exists(&def.id) else {
                            continue;
                        };
                        let next = *rt.next_run.read().unwrap_or_else(|e| e.into_inner());
                        let Some(next) = next else { continue };
                        if next <= now {
                            // Advance first so a slow start cannot double-fire.
                            *rt.next_run.write().unwrap_or_else(|e| e.into_inner()) =
                                next_fire(&schedule, now);
                            // Jobs the user stopped manually stay stopped until explicitly started.
                            if *rt.status.read().unwrap_or_else(|e| e.into_inner())
                                != JobStatus::Running
                                && !rt.manual_stop.load(Ordering::Relaxed)
                            {
                                mgr.audit.log(
                                    "scheduler",
                                    "system",
                                    "job_start",
                                    &format!("{} (cron {})", def.name, schedule),
                                );
                                mgr.spawn_supervisor(def.id);
                            }
                        }
                    }
                }));
                if r.is_err() {
                    tracing::error!(
                        "スケジューラ処理でパニックが発生しました（次回ティックで再試行）"
                    );
                }
            }
        });
    }

    // ---- Supervisor ----

    fn spawn_supervisor(self: &Arc<Self>, id: Uuid) -> bool {
        self.spawn_supervisor_with_flag(id)
    }

    /// Returns false if another supervisor is still active for this job.
    fn spawn_supervisor_with_flag(self: &Arc<Self>, id: Uuid) -> bool {
        let Some(rt) = self.runtime_if_exists(&id) else {
            return false;
        };
        if rt.supervisor_active.swap(true, Ordering::SeqCst) {
            return false;
        }
        let mgr = self.clone();
        let rt = rt.clone();
        tokio::spawn(async move {
            loop {
                let Some(def) = mgr
                    .defs
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .iter()
                    .find(|d| d.id == id)
                    .cloned()
                else {
                    break;
                };

                match mgr.launch(&def, &rt).await {
                    Ok(mut child) => {
                        let pid = child.id().unwrap_or_default();
                        rt.set_running(pid);
                        rt.append_log(
                            format!(
                                "[kenji] started: {} {} (pid {pid})\n",
                                def.command,
                                def.args.join(" ")
                            )
                            .as_bytes(),
                        )
                        .await;
                        let exit = child.wait().await;
                        let code = match exit {
                            Ok(status) => status.code(),
                            Err(_) => None,
                        };
                        // Log pipes close when the process exits; give pumps a moment to drain.
                        tokio::time::sleep(Duration::from_millis(150)).await;
                        rt.mark_exit(code);
                        rt.append_log(format!("[kenji] exited with code {code:?}\n").as_bytes())
                            .await;
                        // Long-lived successful runs reset the backoff counter.
                        let started = *rt.started_at.read().unwrap_or_else(|e| e.into_inner());
                        if let Some(st) = started {
                            let up = Utc::now().signed_duration_since(st);
                            if up.to_std().map(|d| d > UPTIME_RESET).unwrap_or(false) {
                                *rt.restart_attempts
                                    .write()
                                    .unwrap_or_else(|e| e.into_inner()) = 0;
                            }
                        }
                    }
                    Err(e) => {
                        rt.set_failed(format!("起動失敗: {e:#}"));
                        rt.append_log(format!("[kenji] launch failed: {e:#}\n").as_bytes())
                            .await;
                    }
                }

                if rt.manual_stop.load(Ordering::Relaxed) {
                    break;
                }
                let should_restart = match def.restart_policy {
                    RestartPolicy::Never => false,
                    RestartPolicy::OnFailure => {
                        *rt.exit_code.read().unwrap_or_else(|e| e.into_inner()) != Some(0)
                    }
                    RestartPolicy::Always => true,
                };
                if !should_restart {
                    break;
                }
                let attempts = {
                    let mut a = rt
                        .restart_attempts
                        .write()
                        .unwrap_or_else(|e| e.into_inner());
                    *a += 1;
                    *a
                };
                if attempts >= MAX_RESTART_ATTEMPTS {
                    *rt.last_error.write().unwrap_or_else(|e| e.into_inner()) =
                        Some("再起動試行回数の上限に達しました".into());
                    *rt.status.write().unwrap_or_else(|e| e.into_inner()) = JobStatus::Failed;
                    break;
                }
                rt.append_log(
                    format!(
                        "[kenji] restarting in {}s (attempt {attempts})\n",
                        RESTART_DELAY.as_secs()
                    )
                    .as_bytes(),
                )
                .await;
                tokio::time::sleep(RESTART_DELAY).await;
            }
            rt.supervisor_active.store(false, Ordering::SeqCst);
        });
        true
    }

    async fn launch(&self, def: &JobDef, rt: &Arc<JobRuntime>) -> Result<tokio::process::Child> {
        let mut cmd = tokio::process::Command::new(&def.command);
        cmd.args(&def.args);
        // New jobs run inside their owner's dedicated folder. Legacy jobs
        // (owner empty) keep the old shared workspace as their base.
        let base = if def.owner.is_empty() {
            self.workspace_root.clone()
        } else {
            crate::files::ensure_user_root(&self.workspace_root, &def.owner).with_context(|| {
                format!(
                    "ジョブ所有者 `{}` の作業フォルダを準備できません",
                    def.owner
                )
            })?
        };
        let cwd = match &def.cwd {
            Some(c) if std::path::Path::new(c).is_absolute() => PathBuf::from(c),
            Some(c) => {
                let p = crate::files::resolve_under_root(&base, c)
                    .with_context(|| format!("ジョブの作業ディレクトリが不正です: {c}"))?;
                crate::files::ensure_within(&base, &p)
                    .with_context(|| format!("ジョブの作業ディレクトリが作業フォルダ外です: {c}"))?
            }
            None => base,
        };
        cmd.current_dir(&cwd);
        cmd.envs(&def.env);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        #[cfg(windows)]
        {
            cmd.creation_flags(0x0800_0004); // CREATE_NO_WINDOW | CREATE_SUSPENDED: attach before user code runs
        }
        let child = cmd
            .spawn()
            .with_context(|| format!("コマンドの起動に失敗: {}", def.command))?;
        let mut child = child;
        if let Some(pid) = child.id() {
            if let Err(e) = self
                .resources
                .attach(pid)
                .and_then(|_| crate::resources::resume_suspended(pid))
            {
                kill_tree(pid);
                let _ = child.kill().await;
                return Err(e);
            }
        }
        if let Some(out) = child.stdout.take() {
            spawn_log_pump(rt.clone(), out);
        }
        if let Some(err) = child.stderr.take() {
            spawn_log_pump(rt.clone(), err);
        }
        Ok(child)
    }
}

fn spawn_log_pump(
    rt: Arc<JobRuntime>,
    mut stream: impl tokio::io::AsyncRead + Unpin + Send + 'static,
) {
    tokio::spawn(async move {
        let mut buf = [0u8; 8192];
        loop {
            match stream.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => rt.append_log(&buf[..n]).await,
            }
        }
    });
}

/// Kill a process and all of its descendants (Windows: `taskkill /T /F`).
/// 標準プロセスとして起動しランタイムの生存に依存しない（graceful shutdown 中に
/// tokioタスクが打ち切られても kill 自体は実行される）。
pub fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use std::process::Stdio;
        let spawned = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .creation_flags(0x0800_0000)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Err(e) = spawned {
            tracing::warn!("taskkill pid {pid}: {e}");
        }
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
    }
}

fn parse_cron(s: &str) -> Result<Schedule> {
    // Accept standard 5-field cron (min hour dom mon dow); expand with a seconds field.
    let fields = s.split_whitespace().count();
    let expanded = if fields == 5 {
        format!("0 {s}")
    } else {
        s.to_string()
    };
    Schedule::from_str(&expanded).map_err(|e| anyhow!("{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cron_5field_parses_and_fires() {
        assert!(parse_cron("*/5 * * * *").is_ok());
        assert!(parse_cron("0 3 * * 1-5").is_ok());
        assert!(parse_cron("bad").is_err());
        let next = next_fire("*/5 * * * *", Utc::now()).expect("fires");
        assert!(next > Utc::now());
        // next fire should be within 5 minutes for a */5 schedule
        assert!(Utc::now().signed_duration_since(next).num_minutes() <= 5);
    }

    #[test]
    fn job_ownership_rules() {
        assert!(can_manage("alice", "operator", "alice"));
        assert!(can_manage("bob", "operator", ""));
        assert!(!can_manage("bob", "operator", "alice"));
        assert!(can_manage("admin", "admin", "alice"));
    }
}

fn next_fire(schedule: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    parse_cron(schedule).ok()?.after(&now).next()
}

/// Admins may manage every job. Operators may manage their own jobs and
/// legacy/shared jobs (empty owner) but not jobs owned by another user.
fn can_manage(user: &str, role: &str, owner: &str) -> bool {
    role == "admin" || owner.is_empty() || owner == user
}

fn ensure_can_manage(user: &str, role: &str, def: &JobDef) -> Result<(), String> {
    if can_manage(user, role, &def.owner) {
        Ok(())
    } else {
        Err("このジョブを操作する権限がありません（作成者または管理者のみ）".to_string())
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateJobRequest {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Option<Vec<String>>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
    #[serde(default)]
    pub restart_policy: Option<RestartPolicy>,
    #[serde(default)]
    pub schedule: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub description: Option<String>,
}
