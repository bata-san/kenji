use anyhow::{Context, Result};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use chrono::{DateTime, Duration, Utc};
use rand::distributions::Alphanumeric;
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Viewer = 0,
    Operator = 1,
    Admin = 2,
}

impl Role {
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "viewer" => Some(Role::Viewer),
            "operator" => Some(Role::Operator),
            "admin" => Some(Role::Admin),
            _ => None,
        }
    }
    pub fn require(&self, min: Role) -> Result<(), super::error::ApiError> {
        if *self >= min {
            Ok(())
        } else {
            Err(super::error::ApiError::Forbidden)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyRecord {
    pub id: String,
    pub name: String,
    pub role: Role,
    pub key_hash: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthData {
    pub admin_username: String,
    pub admin_password_hash: String,
    pub jwt_secret: String,
    pub keys: Vec<ApiKeyRecord>,
}

#[derive(Debug, Clone)]
pub struct AuthContext {
    pub username: String,
    pub role: Role,
}

#[derive(Default)]
struct Lockout {
    fails: HashMap<String, LockoutEntry>,
}

struct LockoutEntry {
    fails: u32,
    until: Option<Instant>,
    last_seen: Instant,
}

impl Lockout {
    fn prune(&mut self, now: Instant) {
        self.fails.retain(|_, entry| {
            now.duration_since(entry.last_seen).as_secs() < LOCKOUT_ENTRY_TTL_SECS
        });
    }

    fn record_failure(&mut self, who: &str, now: Instant) {
        self.prune(now);
        if self.fails.len() >= MAX_LOCKOUT_ENTRIES && !self.fails.contains_key(who) {
            if let Some(oldest) = self
                .fails
                .iter()
                .min_by_key(|(_, entry)| entry.last_seen)
                .map(|(key, _)| key.clone())
            {
                self.fails.remove(&oldest);
            }
        }
        let entry = self.fails.entry(who.to_owned()).or_insert(LockoutEntry {
            fails: 0,
            until: None,
            last_seen: now,
        });
        entry.last_seen = now;
        entry.fails += 1;
        if entry.fails >= MAX_FAILS {
            entry.until = Some(now + std::time::Duration::from_secs(LOCKOUT_SECS));
            entry.fails = 0;
        }
    }
}

const MAX_FAILS: u32 = 5;
const LOCKOUT_SECS: u64 = 60;
const LOCKOUT_ENTRY_TTL_SECS: u64 = 10 * 60;
const MAX_LOCKOUT_ENTRIES: usize = 10_000;

pub struct AuthStore {
    path: PathBuf,
    data: Mutex<AuthData>,
    dirty: AtomicBool,
    lockouts: Mutex<Lockout>,
}

impl AuthStore {
    pub fn load(path: &Path) -> Result<AuthStore> {
        let raw =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let data: AuthData = serde_json::from_str(&raw).context("parsing auth.json")?;
        Ok(AuthStore {
            path: path.to_path_buf(),
            data: Mutex::new(data),
            dirty: AtomicBool::new(false),
            lockouts: Mutex::new(Lockout::default()),
        })
    }

    pub fn create(
        path: &Path,
        username: &str,
        password: &str,
        jwt_secret: &str,
    ) -> Result<AuthStore> {
        let hash = hash_password(password)?;
        let data = AuthData {
            admin_username: username.to_string(),
            admin_password_hash: hash,
            jwt_secret: jwt_secret.to_string(),
            keys: Vec::new(),
        };
        let raw = serde_json::to_string_pretty(&data)?;
        std::fs::write(path, raw)?;
        Ok(AuthStore {
            path: path.to_path_buf(),
            data: Mutex::new(data),
            dirty: AtomicBool::new(false),
            lockouts: Mutex::new(Lockout::default()),
        })
    }

    /// JWT signing secret (lives in auth.json; generated at setup).
    pub fn jwt_secret(&self) -> String {
        self.data
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .jwt_secret
            .clone()
    }

    /// Verify admin credentials. Returns auth context on success.
    /// `who` is a bounded identifier for lockout tracking (the client IP).
    pub fn login(&self, username: &str, password: &str, who: &str) -> Result<Option<AuthContext>> {
        if username.len() > 256 || password.len() > 1024 || who.len() > 256 {
            return Ok(None);
        }
        {
            let mut l = self.lockouts.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            l.prune(now);
            if let Some(entry) = l.fails.get_mut(who) {
                if entry.until.is_some_and(|until| now < until) {
                    entry.last_seen = now;
                    return Ok(None);
                }
            }
        }

        // Snapshot credentials only; expensive Argon2 must not hold the data lock.
        let (admin_username, password_hash) = {
            let data = self.data.lock().unwrap_or_else(|e| e.into_inner());
            (
                data.admin_username.clone(),
                data.admin_password_hash.clone(),
            )
        };
        let user_ok = admin_username == username;
        let pass_ok = PasswordHash::new(&password_hash).ok().is_some_and(|h| {
            Argon2::default()
                .verify_password(password.as_bytes(), &h)
                .is_ok()
        });

        if user_ok && pass_ok {
            self.lockouts
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .fails
                .remove(who);
            Ok(Some(AuthContext {
                username: username.to_string(),
                role: Role::Admin,
            }))
        } else {
            let mut l = self.lockouts.lock().unwrap_or_else(|e| e.into_inner());
            l.record_failure(who, Instant::now());
            Ok(None)
        }
    }

    /// Authenticate a bearer token: JWT or API key (`kjk_...`).
    pub fn authenticate(&self, token: &str, jwt_secret: &str) -> Option<AuthContext> {
        if token.starts_with("kjk_") {
            let hash = hex::encode(Sha256::digest(token.as_bytes()));
            let mut data = self.data.lock().unwrap_or_else(|e| e.into_inner());
            for k in data.keys.iter_mut() {
                if constant_time_eq(k.key_hash.as_bytes(), hash.as_bytes()) {
                    k.last_used_at = Some(Utc::now());
                    self.dirty.store(true, Ordering::Relaxed);
                    return Some(AuthContext {
                        username: format!("key:{}", k.name),
                        role: k.role,
                    });
                }
            }
            return None;
        }
        verify_jwt(token, jwt_secret).ok()
    }

    pub fn create_key(&self, name: &str, role: Role) -> Result<(ApiKeyRecord, String)> {
        let secret: String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(40)
            .map(char::from)
            .collect();
        let full = format!("kjk_{secret}");
        let rec = ApiKeyRecord {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            role,
            key_hash: hex::encode(Sha256::digest(full.as_bytes())),
            created_at: Utc::now(),
            last_used_at: None,
        };
        {
            let mut data = self.data.lock().unwrap_or_else(|e| e.into_inner());
            data.keys.push(rec.clone());
            self.dirty.store(true, Ordering::Relaxed);
        }
        self.flush()?;
        Ok((rec, full))
    }

    pub fn delete_key(&self, id: &str) -> Result<bool> {
        let removed = {
            let mut data = self.data.lock().unwrap_or_else(|e| e.into_inner());
            let before = data.keys.len();
            data.keys.retain(|k| k.id != id);
            let removed = data.keys.len() != before;
            if removed {
                self.dirty.store(true, Ordering::Relaxed);
            }
            removed
        };
        if removed {
            self.flush()?;
        }
        Ok(removed)
    }

    pub fn list_keys(&self) -> Vec<ApiKeyRecord> {
        self.data
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys
            .clone()
    }

    pub fn flush(&self) -> Result<()> {
        let data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        let raw = serde_json::to_string_pretty(&*data)?;
        let tmp = self.path.with_extension("json.tmp");
        let mut file = std::fs::File::create(&tmp)?;
        use std::io::Write;
        file.write_all(raw.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, &self.path)?;
        self.dirty.store(false, Ordering::Relaxed);
        Ok(())
    }

    pub fn flush_if_dirty(&self) {
        if self.dirty.load(Ordering::Relaxed) {
            if let Err(e) = self.flush() {
                tracing::error!("auth store flush failed: {e:#}");
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct JwtClaims {
    pub sub: String,
    pub role: Role,
    pub iat: i64,
    pub exp: i64,
}

pub fn issue_jwt(username: &str, role: Role, secret: &str) -> Result<(String, DateTime<Utc>)> {
    let now = Utc::now();
    let exp = now + Duration::hours(12);
    let claims = JwtClaims {
        sub: username.to_string(),
        role,
        iat: now.timestamp(),
        exp: exp.timestamp(),
    };
    let token = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
    )?;
    Ok((token, exp))
}

pub fn verify_jwt(token: &str, secret: &str) -> Result<AuthContext> {
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
    validation.validate_exp = true;
    let data = jsonwebtoken::decode::<JwtClaims>(
        token,
        &jsonwebtoken::DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )?;
    Ok(AuthContext {
        username: data.claims.sub,
        role: data.claims.role,
    })
}

pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut rand::rngs::OsRng);
    Ok(Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("hash failed: {e}"))?
        .to_string())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    fn test_store(path: &Path) -> AuthStore {
        AuthStore {
            path: path.to_owned(),
            data: Mutex::new(AuthData {
                admin_username: "admin".into(),
                admin_password_hash: String::new(),
                jwt_secret: "test-secret".into(),
                keys: Vec::new(),
            }),
            dirty: AtomicBool::new(false),
            lockouts: Mutex::new(Lockout::default()),
        }
    }

    #[test]
    fn lockout_entries_expire_and_stay_bounded() {
        let mut lockout = Lockout::default();
        let now = Instant::now();
        lockout.record_failure("stale", now);
        lockout.prune(now + std::time::Duration::from_secs(LOCKOUT_ENTRY_TTL_SECS));
        assert!(lockout.fails.is_empty());
        for _ in 0..MAX_FAILS {
            lockout.record_failure("locked", now);
        }
        assert!(lockout.fails["locked"].until.unwrap() > now);
        lockout.prune(now + std::time::Duration::from_secs(LOCKOUT_ENTRY_TTL_SECS));
        assert!(lockout.fails.is_empty());
        for i in 0..MAX_LOCKOUT_ENTRIES {
            lockout.fails.insert(
                i.to_string(),
                LockoutEntry {
                    fails: 1,
                    until: None,
                    last_seen: now,
                },
            );
        }
        lockout.fails.get_mut("0").unwrap().last_seen = now - std::time::Duration::from_secs(1);
        lockout.record_failure("new", now);
        assert_eq!(lockout.fails.len(), MAX_LOCKOUT_ENTRIES);
        assert!(!lockout.fails.contains_key("0"));
    }

    #[test]
    fn flush_failure_preserves_dirty_for_retry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let store = test_store(&path);
        std::fs::create_dir(path.with_extension("json.tmp")).unwrap();
        assert!(store.create_key("retry", Role::Viewer).is_err());
        assert!(store.dirty.load(Ordering::Relaxed));
        std::fs::remove_dir(path.with_extension("json.tmp")).unwrap();
        store.flush_if_dirty();
        assert!(!store.dirty.load(Ordering::Relaxed));
        assert_eq!(AuthStore::load(&path).unwrap().list_keys().len(), 1);
    }

    #[test]
    fn concurrent_key_writes_and_flushes_preserve_updates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let store = Arc::new(test_store(&path));
        let barrier = Arc::new(Barrier::new(5));
        std::thread::scope(|scope| {
            for i in 0..4 {
                let store = store.clone();
                let barrier = barrier.clone();
                scope.spawn(move || {
                    barrier.wait();
                    for n in 0..8 {
                        let (_, token) =
                            store.create_key(&format!("{i}-{n}"), Role::Viewer).unwrap();
                        assert!(store.authenticate(&token, "test-secret").is_some());
                        store.flush_if_dirty();
                    }
                });
            }
            barrier.wait();
            for _ in 0..16 {
                store.flush_if_dirty();
            }
        });
        store.flush_if_dirty();
        let keys = AuthStore::load(&path).unwrap().list_keys();
        assert_eq!(keys.len(), 32);
        assert!(keys.iter().all(|key| key.last_used_at.is_some()));
        assert!(!store.dirty.load(Ordering::Relaxed));
    }
}
