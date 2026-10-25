use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const ROTATE_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub ts: DateTime<Utc>,
    pub user: String,
    pub role: String,
    pub action: String,
    pub detail: String,
}

pub struct Audit {
    dir: PathBuf,
    lock: Mutex<()>,
}

impl Audit {
    pub fn new(dir: &Path) -> Audit {
        let _ = std::fs::create_dir_all(dir);
        Audit {
            dir: dir.to_path_buf(),
            lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join("audit.jsonl")
    }

    /// Append an entry. Fire-and-forget friendly (cheap append + rotation).
    pub fn log(&self, user: &str, role: &str, action: &str, detail: &str) {
        let entry = AuditEntry {
            ts: Utc::now(),
            user: user.to_string(),
            role: role.to_string(),
            action: action.to_string(),
            detail: detail.chars().take(500).collect(),
        };
        if let Err(e) = self.append(&entry) {
            tracing::error!("audit write failed: {e:#}");
        }
    }

    fn append(&self, entry: &AuditEntry) -> Result<()> {
        let _g = self.lock.lock().unwrap();
        let path = self.path();
        if let Ok(meta) = std::fs::metadata(&path) {
            if meta.len() > ROTATE_BYTES {
                let rotated = self.dir.join("audit.jsonl.1");
                let _ = std::fs::remove_file(&rotated);
                let _ = std::fs::rename(&path, &rotated);
            }
        }
        let mut line = serde_json::to_string(entry)?;
        line.push('\n');
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
        f.write_all(line.as_bytes())?;
        Ok(())
    }

    /// Newest-first list of up to `limit` entries.
    pub fn tail(&self, limit: usize) -> Vec<AuditEntry> {
        let _g = self.lock.lock().unwrap();
        let mut entries: Vec<AuditEntry> = Vec::new();
        for path in [self.dir.join("audit.jsonl.1"), self.path()] {
            if !path.exists() {
                continue;
            }
            if let Ok(raw) = std::fs::read_to_string(&path) {
                for line in raw.lines() {
                    if let Ok(e) = serde_json::from_str::<AuditEntry>(line) {
                        entries.push(e);
                    }
                }
            }
        }
        entries.sort_by(|a, b| b.ts.cmp(&a.ts));
        entries.truncate(limit);
        entries
    }
}
