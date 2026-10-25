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
    fails: HashMap<String, (u32, Option<Instant>)>,
}

const MAX_FAILS: u32 = 5;
const LOCKOUT_SECS: u64 = 60;

pub struct AuthStore {
    path: PathBuf,
    data: Mutex<AuthData>,
    dirty: AtomicBool,
    lockouts: Mutex<Lockout>,
}

impl AuthStore {
    pub fn load(path: &Path) -> Result<AuthStore> {
        let raw = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let data: AuthData = serde_json::from_str(&raw).context("parsing auth.json")?;
        Ok(AuthStore {
            path: path.to_path_buf(),
            data: Mutex::new(data),
            dirty: AtomicBool::new(false),
            lockouts: Mutex::new(Lockout::default()),
        })
    }

    pub fn create(path: &Path, username: &str, password: &str, jwt_secret: &str) -> Result<AuthStore> {
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
        self.data.lock().unwrap().jwt_secret.clone()
    }

    /// Verify admin credentials. Returns auth context on success.
    /// `who` is an identifier for lockout tracking (username + client ip).
    pub fn login(&self, username: &str, password: &str, who: &str) -> Result<Option<AuthContext>> {
        {
            let l = self.lockouts.lock().unwrap();
            if let Some((_, Some(until))) = l.fails.get(who) {
                if Instant::now() < *until {
                    return Ok(None); // still locked out
                }
            }
        }

        let (user_ok, pass_ok) = {
            let data = self.data.lock().unwrap();
            let user_ok = data.admin_username == username;
            let parsed = PasswordHash::new(&data.admin_password_hash).ok();
            let pass_ok = parsed
                .map(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
                .unwrap_or(false);
            (user_ok, pass_ok)
        };

        if user_ok && pass_ok {
            self.lockouts.lock().unwrap().fails.remove(who);
            Ok(Some(AuthContext { username: username.to_string(), role: Role::Admin }))
        } else {
            let mut l = self.lockouts.lock().unwrap();
            let entry = l.fails.entry(who.to_string()).or_insert((0, None));
            entry.0 += 1;
            if entry.0 >= MAX_FAILS {
                entry.1 = Some(Instant::now() + std::time::Duration::from_secs(LOCKOUT_SECS));
                entry.0 = 0;
            }
            Ok(None)
        }
    }

    /// Authenticate a bearer token: JWT or API key (`kjk_...`).
    pub fn authenticate(&self, token: &str, jwt_secret: &str) -> Option<AuthContext> {
        if token.starts_with("kjk_") {
            let hash = hex::encode(Sha256::digest(token.as_bytes()));
            let mut data = self.data.lock().unwrap();
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
        let secret: String = rand::thread_rng().sample_iter(&Alphanumeric).take(40).map(char::from).collect();
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
            let mut data = self.data.lock().unwrap();
            data.keys.push(rec.clone());
            self.dirty.store(true, Ordering::Relaxed);
        }
        self.flush()?;
        Ok((rec, full))
    }

    pub fn delete_key(&self, id: &str) -> Result<bool> {
        let removed = {
            let mut data = self.data.lock().unwrap();
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
        self.data.lock().unwrap().keys.clone()
    }

    pub fn flush(&self) -> Result<()> {
        let raw = {
            let data = self.data.lock().unwrap();
            serde_json::to_string_pretty(&*data)?
        };
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, raw.as_bytes())?;
        std::fs::rename(&tmp, &self.path)?;
        self.dirty.store(false, Ordering::Relaxed);
        Ok(())
    }

    pub fn flush_if_dirty(&self) {
        if self.dirty.swap(false, Ordering::Relaxed) {
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
