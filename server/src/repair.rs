//! Independent crash observer and bounded pi diagnostic worker. No deployment authority.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RepairConfig {
    pub enabled: bool,
    pub provider: String,
    pub model: String,
    pub base_url: String,
    pub api_key_env: String,
    pub harness_path: String,
}
impl Default for RepairConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: "kenji-custom".into(),
            model: String::new(),
            base_url: "https://api.openai.com/v1".into(),
            api_key_env: "KENJI_REPAIR_API_KEY".into(),
            harness_path: String::new(),
        }
    }
}
impl RepairConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.provider == "kenji-custom",
            "修復プロバイダ識別子が不正です"
        );
        anyhow::ensure!(
            self.api_key_env
                .as_bytes()
                .first()
                .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
                && self.api_key_env.len() <= 128
                && !self.api_key_env.is_empty()
                && self
                    .api_key_env
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_'),
            "APIキー環境変数名が不正です"
        );
        let url = axum::http::Uri::try_from(&self.base_url).context("プロバイダURLが不正です")?;
        anyhow::ensure!(
            url.scheme_str() == Some("https")
                && url.host().is_some()
                && !self.base_url.contains('@')
                && !self.base_url.contains('?')
                && !self.base_url.contains('#'),
            "プロバイダは認証情報を含まないHTTPS URLを指定してください"
        );
        anyhow::ensure!(
            self.model.len() <= 200 && self.harness_path.len() <= 2048,
            "修復設定が長すぎます"
        );
        if self.enabled {
            anyhow::ensure!(!self.model.trim().is_empty(), "モデルを指定してください");
            anyhow::ensure!(
                Path::new(&self.harness_path).is_absolute()
                    && Path::new(&self.harness_path).is_file(),
                "インストール済みworker.mjsの絶対パスを指定してください"
            );
        }
        Ok(())
    }
}

pub fn status(data: &Path) -> serde_json::Value {
    std::fs::read(data.join("repair/status.json"))
        .ok()
        .and_then(|raw| serde_json::from_slice(&raw).ok())
        .unwrap_or_else(|| serde_json::json!({"state":"idle"}))
}
fn record(data: &Path, value: serde_json::Value) -> Result<()> {
    let dir = data.join("repair");
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join("status.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&value)?)?;
    std::fs::rename(tmp, dir.join("status.json"))?;
    Ok(())
}

/// Observer opens the process handle before reporting readiness; it survives the
/// observed server's address-space failure. It does not share the workload job.
#[cfg(windows)]
pub fn start_observer(data: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    let data = std::fs::canonicalize(data)?;
    let ready = data.join(format!("repair-observer-{}.ready", std::process::id()));
    let _ = std::fs::remove_file(&ready);
    std::process::Command::new(std::env::current_exe()?)
        .args([
            "repair-watch",
            "--pid",
            &std::process::id().to_string(),
            "--data-dir",
        ])
        .arg(&data)
        .creation_flags(0x0800_0000)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let until = Instant::now() + Duration::from_secs(3);
    while !ready.exists() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(25));
    }
    anyhow::ensure!(
        ready.exists(),
        "クラッシュ監視プロセスの起動を確認できません"
    );
    std::fs::remove_file(ready)?;
    Ok(())
}
#[cfg(not(windows))]
pub fn start_observer(_: &Path) -> Result<()> {
    Ok(())
}

pub fn watch(pid: u32, data: PathBuf) -> Result<()> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::*;
        let process = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                pid,
            )
        };
        anyhow::ensure!(!process.is_null(), "監視対象を開けません");
        let ready = data.join(format!("repair-observer-{pid}.ready"));
        if let Err(e) = std::fs::write(ready, b"ready") {
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(process);
            }
            return Err(e.into());
        }
        let wait = unsafe { WaitForSingleObject(process, u32::MAX) };
        let mut code = 0u32;
        let ok = unsafe { GetExitCodeProcess(process, &mut code) };
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(process);
        }
        anyhow::ensure!(wait == 0 && ok != 0, "監視対象の終了確認に失敗");
        if code == 0 {
            return Ok(());
        }
        let cfg = crate::config::Config::load(&data)?;
        if cfg.repair.enabled {
            run(
                &data,
                &cfg.repair,
                &format!("server process exited abnormally: 0x{code:08x}"),
            )?;
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (pid, data);
        anyhow::bail!("クラッシュ監視はWindows専用です");
    }
    Ok(())
}

pub fn run(data: &Path, cfg: &RepairConfig, incident: &str) -> Result<()> {
    cfg.validate()?;
    anyhow::ensure!(cfg.enabled, "AI修復は無効です");
    let dir = data.join("repair");
    std::fs::create_dir_all(&dir)?;
    // Cross-process single flight. A stale lock is deliberately fail-closed and
    // needs operator review rather than replaying a possibly billed request.
    let lock = dir.join("active.lock");
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)
        .context("修復ジョブが実行中か、前回の結果確認が必要です")?;
    struct Unlock {
        path: PathBuf,
        file: Option<std::fs::File>,
    }
    impl Drop for Unlock {
        fn drop(&mut self) {
            drop(self.file.take());
            let _ = std::fs::remove_file(&self.path);
        }
    }
    let _unlock = Unlock {
        path: lock,
        file: Some(file),
    };
    // Bound billable crash loops across server restarts. Failed attempts count
    // too: uncertain provider outcomes must not be automatically replayed.
    let attempt = dir.join("last-attempt");
    if let Ok(meta) = std::fs::metadata(&attempt) {
        if meta.modified()?.elapsed().unwrap_or_default() < Duration::from_secs(300) {
            anyhow::bail!("診断の再実行は5分以上あけてください");
        }
    }
    std::fs::write(attempt, b"attempted")?;
    let id = uuid::Uuid::new_v4().to_string();
    let output = dir.join(format!("{id}.txt"));
    // Send only a fixed incident description. Raw logs may contain credentials or
    // personal file paths and are not automatically uploaded to the provider.
    let request = serde_json::json!({"provider":cfg.provider,"model":cfg.model,"base_url":cfg.base_url,"api_key_env":cfg.api_key_env,"incident":incident});
    let result = (|| -> Result<()> {
        anyhow::ensure!(
            std::env::var(&cfg.api_key_env).is_ok_and(|v| !v.is_empty()),
            "APIキー環境変数が未設定です"
        );
        record(
            data,
            serde_json::json!({"state":"running","id":id,"automatic_deployment":false}),
        )?;
        let log = std::fs::File::create(&output)?;
        let mut cmd = std::process::Command::new("node");
        cmd.arg(&cfg.harness_path)
            .stdin(std::process::Stdio::piped())
            .stdout(log)
            .stderr(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0004); // no window, initially suspended
        }
        let guard = crate::resources::ResourceGuard::new()?;
        guard.set_enabled(true)?;
        let mut child = cmd.spawn().context("Node.js修復ハーネスを起動できません")?;
        if let Err(e) = guard
            .attach(child.id())
            .and_then(|_| crate::resources::resume_suspended(child.id()))
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
        use std::io::Write;
        if let Err(e) = child
            .stdin
            .take()
            .context("worker stdin")?
            .write_all(serde_json::to_string(&request)?.as_bytes())
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e.into());
        }
        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            if let Some(exit) = child.try_wait()? {
                anyhow::ensure!(
                    exit.success(),
                    "修復ハーネスが失敗しました（認証・モデル・結果を確認してください）"
                );
                break;
            }
            if Instant::now() >= deadline || std::fs::metadata(&output)?.len() > 1024 * 1024 {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!("修復ジョブが時間または出力上限を超えました");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    })();
    record(
        data,
        serde_json::json!({"state":if result.is_ok() {"completed"} else {"failed"},"id":id,"output_file":output,"error":result.as_ref().err().map(|e|format!("{e:#}")),"automatic_deployment":false}),
    )?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provider_config_rejects_unsafe_values() {
        let mut config = RepairConfig::default();
        config.validate().unwrap();
        config.base_url = "http://example.com/v1".into();
        assert!(config.validate().is_err());
        config.base_url = "https://key@example.com/v1".into();
        assert!(config.validate().is_err());
        config.base_url = "https://example.com/v1".into();
        config.api_key_env = "!print-secret".into();
        assert!(config.validate().is_err());
    }
}
