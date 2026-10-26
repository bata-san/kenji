use anyhow::{Context, Result};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use chrono::{DateTime, Duration, Utc};
use rand::distributions::Alphanumeric;
use rand::Rng;
use rusqlite::{params, Connection, OptionalExtension};
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
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Operator => "operator",
            Role::Admin => "admin",
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

/// auth.json に残すのは JWT シークレットと API キーのみ。
/// 管理者の旧フィールドは既存ファイルからの移行のために読み取る。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthData {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub admin_username: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub admin_password_hash: String,
    pub jwt_secret: String,
    #[serde(default)]
    pub keys: Vec<ApiKeyRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserRecord {
    pub id: i64,
    pub username: String,
    pub role: Role,
    pub created_at: String,
    pub updated_at: String,
    pub disabled: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum UserError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Conflict(String),
    #[error("ユーザーが見つかりません")]
    NotFound,
    #[error("最後の管理者は削除または降格できません")]
    LastAdmin,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
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
    db: Mutex<Connection>,
    data: Mutex<AuthData>,
    dummy_hash: String,
    dirty: AtomicBool,
    lockouts: Mutex<Lockout>,
}

impl AuthStore {
    pub fn load(path: &Path) -> Result<AuthStore> {
        let raw =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut data: AuthData = serde_json::from_str(&raw).context("parsing auth.json")?;
        let db_path = path.with_file_name("users.db");
        let db = open_db(&db_path)?;
        migrate_legacy_admin(&db, &data)?;

        // Once the users table is authoritative, do not keep a second copy of
        // the administrator password hash in auth.json.
        if !data.admin_username.is_empty() || !data.admin_password_hash.is_empty() {
            data.admin_username.clear();
            data.admin_password_hash.clear();
            if let Err(e) = write_auth_data(path, &data) {
                tracing::warn!(
                    "legacy admin credentials could not be removed from auth.json: {e:#}"
                );
            }
        }

        let dummy_hash = hash_password("kenji-dummy-password-not-used")?;
        Ok(AuthStore {
            path: path.to_path_buf(),
            db: Mutex::new(db),
            data: Mutex::new(data),
            dummy_hash,
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
        validate_username(username).map_err(|e| anyhow::anyhow!(e))?;
        validate_password(password).map_err(|e| anyhow::anyhow!(e))?;
        let hash = hash_password(password)?;

        // Keep API keys when setup --force is used to reset the admin password.
        let existing_keys = if path.exists() {
            match std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))
                .and_then(|raw| serde_json::from_str::<AuthData>(&raw).context("parsing auth.json"))
            {
                Ok(old) => old.keys,
                Err(e) => {
                    eprintln!("警告: 既存 auth.json を引き継げませんでした（APIキーは保持されません）: {e:#}");
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        let data = AuthData {
            admin_username: String::new(),
            admin_password_hash: String::new(),
            jwt_secret: jwt_secret.to_string(),
            keys: existing_keys,
        };
        write_auth_data(path, &data)?;

        let db_path = path.with_file_name("users.db");
        let db = open_db(&db_path)?;
        let now = Utc::now().to_rfc3339();
        db.execute(
            "INSERT INTO users (username, password_hash, role, created_at, updated_at, disabled)
             VALUES (?1, ?2, 'admin', ?3, ?3, 0)
             ON CONFLICT(username) DO UPDATE SET
               password_hash = excluded.password_hash,
               role = 'admin',
               disabled = 0,
               updated_at = excluded.updated_at",
            params![username, hash, now],
        )?;

        let dummy_hash = hash_password("kenji-dummy-password-not-used")?;
        Ok(AuthStore {
            path: path.to_path_buf(),
            db: Mutex::new(db),
            data: Mutex::new(data),
            dummy_hash,
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

    /// Verify credentials against the users table. Returns auth context on success.
    /// `who` is a bounded identifier for lockout tracking (the client IP).
    /// Lockouts are scoped to username + client so one user behind a shared NAT
    /// cannot lock out everybody else.
    pub fn login(&self, username: &str, password: &str, who: &str) -> Result<Option<AuthContext>> {
        if username.len() > 256 || password.len() > 1024 || who.len() > 256 {
            return Ok(None);
        }
        let lock_key = format!("{username}\u{0}{who}");
        {
            let mut l = self.lockouts.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            l.prune(now);
            if let Some(entry) = l.fails.get_mut(&lock_key) {
                if entry.until.is_some_and(|until| now < until) {
                    entry.last_seen = now;
                    return Ok(None);
                }
            }
        }

        // Snapshot the hash and role only; expensive Argon2 must not hold the DB lock.
        let row: Option<(String, String, String, i64)> = {
            let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
            db.query_row(
                "SELECT username, password_hash, role, disabled FROM users WHERE username = ?1",
                params![username],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?
        };

        let hash = row
            .as_ref()
            .map(|r| r.1.clone())
            .unwrap_or_else(|| self.dummy_hash.clone());
        let pass_ok = PasswordHash::new(&hash).ok().is_some_and(|h| {
            Argon2::default()
                .verify_password(password.as_bytes(), &h)
                .is_ok()
        });

        let user = match row {
            Some((name, _, role, disabled)) => {
                if disabled != 0 || !pass_ok {
                    None
                } else {
                    match Role::parse(&role) {
                        Some(role) => Some(AuthContext {
                            username: name,
                            role,
                        }),
                        None => {
                            tracing::error!("users.db contains invalid role for {name}: {role}");
                            None
                        }
                    }
                }
            }
            None => None,
        };

        if let Some(ctx) = user {
            self.lockouts
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .fails
                .remove(&lock_key);
            Ok(Some(ctx))
        } else {
            let mut l = self.lockouts.lock().unwrap_or_else(|e| e.into_inner());
            l.record_failure(&lock_key, Instant::now());
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

        let claims = verify_jwt(token, jwt_secret).ok()?;
        // Re-check the user on every request so deletions, disables, and role
        // changes take effect immediately instead of waiting for JWT expiry.
        let row: Option<(String, i64)> = {
            let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
            match db
                .query_row(
                    "SELECT role, disabled FROM users WHERE username = ?1",
                    params![&claims.username],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
            {
                Ok(row) => row,
                Err(e) => {
                    tracing::error!("auth db lookup failed: {e}");
                    return None;
                }
            }
        };
        let (role, disabled) = row?;
        if disabled != 0 {
            return None;
        }
        Some(AuthContext {
            username: claims.username,
            role: Role::parse(&role)?,
        })
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

    pub fn create_user(
        &self,
        username: &str,
        password: &str,
        role: Role,
    ) -> Result<UserRecord, UserError> {
        validate_username(username).map_err(UserError::Invalid)?;
        validate_password(password).map_err(UserError::Invalid)?;
        let hash = hash_password(password).map_err(UserError::Other)?;
        let now = Utc::now().to_rfc3339();
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let res = db.execute(
            "INSERT INTO users (username, password_hash, role, created_at, updated_at, disabled)
             VALUES (?1, ?2, ?3, ?4, ?4, 0)",
            params![username, hash, role.as_str(), now],
        );
        match res {
            Ok(_) => Ok(UserRecord {
                id: db.last_insert_rowid(),
                username: username.to_string(),
                role,
                created_at: now.clone(),
                updated_at: now,
                disabled: false,
            }),
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(UserError::Conflict(format!(
                    "ユーザー `{username}` は既に存在します"
                )))
            }
            Err(e) => Err(UserError::Other(e.into())),
        }
    }

    pub fn list_users(&self) -> Result<Vec<UserRecord>> {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = db.prepare(
            "SELECT id, username, role, created_at, updated_at, disabled
             FROM users ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })?;
        let mut users = Vec::new();
        for row in rows {
            let (id, username, role, created_at, updated_at, disabled) = row?;
            let role = Role::parse(&role)
                .ok_or_else(|| anyhow::anyhow!("users.db contains invalid role: {role}"))?;
            users.push(UserRecord {
                id,
                username,
                role,
                created_at,
                updated_at,
                disabled: disabled != 0,
            });
        }
        Ok(users)
    }

    pub fn delete_user(&self, username: &str) -> Result<bool, UserError> {
        let mut db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let tx = db.transaction().map_err(|e| UserError::Other(e.into()))?;
        let row: Option<(String, i64)> = tx
            .query_row(
                "SELECT role, disabled FROM users WHERE username = ?1",
                params![username],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| UserError::Other(e.into()))?;
        let Some((role, disabled)) = row else {
            return Ok(false);
        };
        if role == "admin" && disabled == 0 {
            let count: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM users WHERE role = 'admin' AND disabled = 0",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| UserError::Other(e.into()))?;
            if count <= 1 {
                return Err(UserError::LastAdmin);
            }
        }
        tx.execute("DELETE FROM users WHERE username = ?1", params![username])
            .map_err(|e| UserError::Other(e.into()))?;
        tx.commit().map_err(|e| UserError::Other(e.into()))?;
        Ok(true)
    }

    pub fn set_password(&self, username: &str, password: &str) -> Result<bool, UserError> {
        validate_password(password).map_err(UserError::Invalid)?;
        let hash = hash_password(password).map_err(UserError::Other)?;
        let now = Utc::now().to_rfc3339();
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let changed = db
            .execute(
                "UPDATE users SET password_hash = ?1, updated_at = ?2 WHERE username = ?3",
                params![hash, now, username],
            )
            .map_err(|e| UserError::Other(e.into()))?;
        Ok(changed > 0)
    }

    pub fn set_role(&self, username: &str, role: Role) -> Result<bool, UserError> {
        let mut db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let tx = db.transaction().map_err(|e| UserError::Other(e.into()))?;
        let row: Option<(String, i64)> = tx
            .query_row(
                "SELECT role, disabled FROM users WHERE username = ?1",
                params![username],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| UserError::Other(e.into()))?;
        let Some((current_role, disabled)) = row else {
            return Ok(false);
        };
        if current_role == "admin" && disabled == 0 && role != Role::Admin {
            let count: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM users WHERE role = 'admin' AND disabled = 0",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| UserError::Other(e.into()))?;
            if count <= 1 {
                return Err(UserError::LastAdmin);
            }
        }
        tx.execute(
            "UPDATE users SET role = ?1, updated_at = ?2 WHERE username = ?3",
            params![role.as_str(), Utc::now().to_rfc3339(), username],
        )
        .map_err(|e| UserError::Other(e.into()))?;
        tx.commit().map_err(|e| UserError::Other(e.into()))?;
        Ok(true)
    }

    pub fn flush(&self) -> Result<()> {
        let data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        write_auth_data(&self.path, &data)?;
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

fn open_db(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)
        .with_context(|| format!("opening user database {}", path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    // WAL lets the CLI and the running server read/write users.db without
    // blocking each other; busy_timeout absorbs short write contention.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE IF NOT EXISTS users (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             username TEXT NOT NULL UNIQUE COLLATE NOCASE,
             password_hash TEXT NOT NULL,
             role TEXT NOT NULL CHECK (role IN ('viewer', 'operator', 'admin')),
             created_at TEXT NOT NULL,
             updated_at TEXT NOT NULL,
             disabled INTEGER NOT NULL DEFAULT 0
         );
         CREATE INDEX IF NOT EXISTS idx_users_role ON users(role);",
    )?;
    Ok(conn)
}

fn migrate_legacy_admin(db: &Connection, data: &AuthData) -> Result<bool> {
    let count: i64 = db.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?;
    if count > 0 || data.admin_username.trim().is_empty() || data.admin_password_hash.is_empty() {
        return Ok(false);
    }
    let now = Utc::now().to_rfc3339();
    db.execute(
        "INSERT OR IGNORE INTO users (username, password_hash, role, created_at, updated_at, disabled)
         VALUES (?1, ?2, 'admin', ?3, ?3, 0)",
        params![
            data.admin_username.trim(),
            data.admin_password_hash,
            now
        ],
    )?;
    Ok(true)
}

fn write_auth_data(path: &Path, data: &AuthData) -> Result<()> {
    let raw = serde_json::to_string_pretty(data)?;
    let tmp = path.with_extension("json.tmp");
    use std::io::Write;
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(raw.as_bytes())?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn validate_username(username: &str) -> Result<(), String> {
    if username.is_empty() || username.len() > 64 {
        return Err("ユーザー名は1〜64文字で指定してください".into());
    }
    if username.trim() != username {
        return Err("ユーザー名の前後に空白は使用できません".into());
    }
    if !username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '@'))
    {
        return Err("ユーザー名は英数字と . _ - @ のみ使用できます".into());
    }
    crate::files::validate_user_folder_name(username).map_err(|_| {
        "ユーザー名はフォルダ名として使用できません（Windows予約名・末尾のドット/空白・_sharedは不可）"
            .to_string()
    })?;
    Ok(())
}

fn validate_password(password: &str) -> Result<(), String> {
    if password.len() < 6 {
        return Err("パスワードは6文字以上にしてください".into());
    }
    if password.len() > 1024 {
        return Err("パスワードが長すぎます".into());
    }
    Ok(())
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
        let db_path = path.with_file_name(format!("users-{}.db", uuid::Uuid::new_v4()));
        let db = open_db(&db_path).unwrap();
        let now = Utc::now().to_rfc3339();
        db.execute(
            "INSERT INTO users (username, password_hash, role, created_at, updated_at, disabled)
             VALUES (?1, ?2, 'admin', ?3, ?3, 0)",
            params!["admin", hash_password("test-password").unwrap(), now],
        )
        .unwrap();
        AuthStore {
            path: path.to_owned(),
            db: Mutex::new(db),
            data: Mutex::new(AuthData {
                admin_username: String::new(),
                admin_password_hash: String::new(),
                jwt_secret: "test-secret".into(),
                keys: Vec::new(),
            }),
            dummy_hash: hash_password("dummy").unwrap(),
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

    #[test]
    fn db_users_login_and_role() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let store = AuthStore::create(&path, "admin", "admin-pass", "test-secret").unwrap();
        let admin = store.login("admin", "admin-pass", "ip1").unwrap().unwrap();
        assert_eq!(admin.role, Role::Admin);

        let test_user = store.create_user("42101", "hattori", Role::Admin).unwrap();
        assert_eq!(test_user.username, "42101");
        assert_eq!(test_user.role, Role::Admin);

        let ctx = store.login("42101", "hattori", "ip2").unwrap().unwrap();
        assert_eq!(ctx.role, Role::Admin);
        assert!(store.login("42101", "wrong", "ip3").unwrap().is_none());

        let users = store.list_users().unwrap();
        assert_eq!(users.len(), 2);
        assert!(users
            .iter()
            .any(|u| u.username == "42101" && u.role == Role::Admin));

        assert!(store.delete_user("42101").unwrap());
        assert!(store.login("42101", "hattori", "ip4").unwrap().is_none());
    }

    #[test]
    fn usernames_must_be_valid_folder_names() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let store = AuthStore::create(&path, "admin", "admin-pass", "test-secret").unwrap();
        for name in [
            "CON", "nul", "COM1", "LPT9.txt", "report.", "report ", "_shared", "_SHARED", "a/b",
            "key:foo", "..", ".",
        ] {
            assert!(
                matches!(
                    store.create_user(name, "password", Role::Viewer),
                    Err(UserError::Invalid(_))
                ),
                "{name:?} should be rejected"
            );
        }
        assert!(store
            .create_user("user.name-1@example", "password", Role::Viewer)
            .is_ok());
    }

    #[test]
    fn legacy_admin_is_migrated_to_db() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let legacy = AuthData {
            admin_username: "admin".into(),
            admin_password_hash: hash_password("legacy-pass").unwrap(),
            jwt_secret: "test-secret".into(),
            keys: Vec::new(),
        };
        write_auth_data(&path, &legacy).unwrap();

        let store = AuthStore::load(&path).unwrap();
        assert!(store.login("admin", "legacy-pass", "ip").unwrap().is_some());
        assert!(path.with_file_name("users.db").exists());

        let raw = std::fs::read_to_string(&path).unwrap();
        let data: AuthData = serde_json::from_str(&raw).unwrap();
        assert!(data.admin_username.is_empty());
        assert!(data.admin_password_hash.is_empty());
    }

    #[test]
    fn last_admin_cannot_be_deleted_or_demoted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let store = AuthStore::create(&path, "admin", "admin-pass", "test-secret").unwrap();
        assert!(matches!(
            store.delete_user("admin"),
            Err(UserError::LastAdmin)
        ));
        assert!(matches!(
            store.set_role("admin", Role::Viewer),
            Err(UserError::LastAdmin)
        ));

        store
            .create_user("second", "second-pass", Role::Admin)
            .unwrap();
        assert!(store.set_role("admin", Role::Viewer).unwrap());
        assert!(store.delete_user("admin").unwrap());
        assert!(matches!(
            store.delete_user("second"),
            Err(UserError::LastAdmin)
        ));
    }

    #[test]
    fn jwt_is_revoked_after_user_delete() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let store = AuthStore::create(&path, "admin", "admin-pass", "test-secret").unwrap();
        store.create_user("42101", "hattori", Role::Admin).unwrap();
        let (token, _) = issue_jwt("42101", Role::Admin, "test-secret").unwrap();
        assert!(store.authenticate(&token, "test-secret").is_some());
        store.delete_user("42101").unwrap();
        assert!(store.authenticate(&token, "test-secret").is_none());
    }

    #[test]
    fn lockout_is_scoped_per_user_not_per_ip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let store = AuthStore::create(&path, "admin", "admin-pass", "test-secret").unwrap();
        store
            .create_user("alice", "alice-pass", Role::Operator)
            .unwrap();
        store
            .create_user("bob", "bob-pass", Role::Operator)
            .unwrap();

        for _ in 0..MAX_FAILS {
            assert!(store
                .login("alice", "wrong", "shared-ip")
                .unwrap()
                .is_none());
        }
        // Alice is temporarily locked out from this IP...
        assert!(store
            .login("alice", "alice-pass", "shared-ip")
            .unwrap()
            .is_none());
        // ...but Bob, behind the same NAT/IP, is unaffected.
        assert!(store
            .login("bob", "bob-pass", "shared-ip")
            .unwrap()
            .is_some());
    }

    #[test]
    fn database_uses_wal_for_concurrent_access() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let _store = AuthStore::create(&path, "admin", "admin-pass", "test-secret").unwrap();
        let db = Connection::open(path.with_file_name("users.db")).unwrap();
        let mode: String = db
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
    }
}
