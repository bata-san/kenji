//! Auto-update from GitHub Releases.
//!
//! Flow: check `GET {api}/repos/{repo}/releases/latest` → compare semver →
//! download `kenji-windows-amd64.exe` → verify SHA256 → hot-swap the running
//! binary (rename current → `*.old`, copy new in place) → hand over to a
//! detached `kenji update-finalize` helper that waits for this process to
//! stop and then restarts the service (SCM) or re-execs the new binary.
//!
//! Security: SHA256 checksum asset is REQUIRED; without a matching checksum
//! the release is never offered. Install requires the admin role (routes)
//! and is written to the audit log.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::watch;

use crate::config::UpdateConfig;

const MAX_EXE_BYTES: u64 = 128 * 1024 * 1024;
const EXE_ASSET_NAMES: [&str; 2] = ["kenji-windows-amd64.exe", "kenji.exe"];
const FINALIZE_TIMEOUT_SECS: u64 = 120;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateStatus {
    Idle,
    Checking,
    Downloading,
    Verifying,
    Installing,
    Failed,
}

#[derive(Debug, Clone)]
pub struct Release {
    pub tag: String,
    pub version: String,
    pub notes_url: String,
    exe_url: String,
    sha_url: String,
}

/// How to come back up after applying an update.
#[derive(Debug, Clone)]
pub struct RestartMode {
    /// true: running as a Windows service — the helper restarts it via SCM.
    pub service: bool,
    /// foreground mode: original argv[1..] the helper re-execs after we exit.
    pub relaunch_args: Vec<String>,
    /// foreground mode: this process's pid, waited on by the helper.
    pub wait_pid: u32,
}

impl RestartMode {
    #[allow(dead_code)] // Windowsサービス以外のターゲットでは未使用
    pub fn service() -> RestartMode {
        RestartMode {
            service: true,
            relaunch_args: vec![],
            wait_pid: 0,
        }
    }

    pub fn foreground(relaunch_args: Vec<String>) -> RestartMode {
        RestartMode {
            service: false,
            relaunch_args,
            wait_pid: std::process::id(),
        }
    }
}

struct Inner {
    status: UpdateStatus,
    latest: Option<Release>,
    last_check_at: Option<DateTime<Utc>>,
    error: Option<String>,
}

pub struct UpdateManager {
    inner: Mutex<Inner>,
    data_dir: PathBuf,
    current_version: String,
    shutdown_tx: watch::Sender<bool>,
    restart: RestartMode,
    api_base: String,
}

impl UpdateManager {
    pub fn new(
        data_dir: PathBuf,
        current_version: String,
        shutdown_tx: watch::Sender<bool>,
        restart: RestartMode,
    ) -> Arc<UpdateManager> {
        let mgr = Arc::new(UpdateManager {
            inner: Mutex::new(Inner {
                status: UpdateStatus::Idle,
                latest: None,
                last_check_at: None,
                error: None,
            }),
            data_dir,
            current_version,
            shutdown_tx,
            restart,
            api_base: std::env::var("KENJI_UPDATE_API")
                .unwrap_or_else(|_| "https://api.github.com".into()),
        });
        mgr.cleanup_stale();
        mgr
    }

    fn set(&self, f: impl FnOnce(&mut Inner)) {
        let mut i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut i);
    }

    /// Remove leftovers from a previous update (staged download / old binary).
    pub fn cleanup_stale(&self) {
        let _ = std::fs::remove_file(self.staging_path());
        if let Ok(exe) = std::env::current_exe() {
            let old = old_binary_path(&exe);
            if old != exe {
                let _ = std::fs::remove_file(&old);
            }
        }
    }

    fn staging_path(&self) -> PathBuf {
        self.data_dir.join("updates").join("kenji.exe.new")
    }

    /// JSON snapshot for the API/UI. `available` is true only when the latest
    /// release is strictly newer than the running version.
    pub fn snapshot(&self, cfg: &UpdateConfig) -> Value {
        let i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let available = i
            .latest
            .as_ref()
            .map(|r| {
                matches!(
                    compare_versions(&r.version, &self.current_version),
                    std::cmp::Ordering::Greater
                )
            })
            .unwrap_or(false);
        json!({
            "current_version": self.current_version,
            "available": available,
            "latest": i.latest.as_ref().map(|r| json!({
                "tag": r.tag,
                "version": r.version,
                "notes_url": r.notes_url,
            })),
            "status": i.status,
            "last_check_at": i.last_check_at,
            "error": i.error,
            "settings": {
                "auto_check": cfg.auto_check,
                "auto_install": cfg.auto_install,
                "check_interval_hours": cfg.check_interval_hours,
                "repo": cfg.repo,
            },
        })
    }

    /// Check GitHub for the latest release. Does not download anything.
    pub async fn check(self: &Arc<Self>, repo: &str) -> Result<Option<Release>, String> {
        self.set(|i| {
            i.status = UpdateStatus::Checking;
            i.error = None;
        });
        let url = format!("{}/repos/{}/releases/latest", self.api_base, repo);
        let version = self.current_version.clone();
        let res = tokio::task::spawn_blocking(move || fetch_latest(&url, &version))
            .await
            .map_err(|e| format!("更新確認タスクが失敗: {e}"))?;
        match res {
            Ok(latest) => {
                self.set(|i| {
                    i.status = UpdateStatus::Idle;
                    i.latest = latest.clone();
                    i.last_check_at = Some(Utc::now());
                });
                Ok(latest)
            }
            Err(e) => {
                self.set(|i| {
                    i.status = UpdateStatus::Idle;
                    i.error = Some(e.clone());
                });
                Err(e)
            }
        }
    }

    /// Download the latest release, verify its SHA256, hot-swap the binary and
    /// hand over to the finalize helper, then trigger a graceful shutdown.
    pub async fn install(self: &Arc<Self>) -> Result<(), String> {
        // ステータスを Idle→Downloading へ原子的に遷移させて二重実行を防ぐ
        let release = {
            let mut i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            match i.status {
                UpdateStatus::Idle | UpdateStatus::Failed => {}
                _ => return Err("アップデート処理がすでに進行中です".into()),
            }
            let Some(rel) = i.latest.clone() else {
                return Err(
                    "利用可能なアップデート情報がありません。先に確認を実行してください。".into(),
                );
            };
            i.status = UpdateStatus::Downloading;
            rel
        };
        if !matches!(
            compare_versions(&release.version, &self.current_version),
            std::cmp::Ordering::Greater
        ) {
            self.set(|i| i.status = UpdateStatus::Idle);
            return Err("新バージョンはありません".into());
        }

        let staging = self.staging_path();
        if let Some(dir) = staging.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("updates ディレクトリ作成に失敗: {e}"))?;
        }
        let r = {
            let release = release.clone();
            let staging = staging.clone();
            tokio::task::spawn_blocking(move || download_and_verify(&release, &staging))
                .await
                .map_err(|e| format!("ダウンロードタスクが失敗: {e}"))?
        };
        if let Err(e) = r {
            let _ = std::fs::remove_file(&staging);
            self.set(|i| {
                i.status = UpdateStatus::Failed;
                i.error = Some(e.clone());
            });
            return Err(e);
        }
        self.set(|i| i.status = UpdateStatus::Verifying);
        // download_and_verify already verified; the split exists for UI clarity.

        self.set(|i| i.status = UpdateStatus::Installing);
        let exe =
            std::env::current_exe().map_err(|e| format!("実行ファイルのパス取得に失敗: {e}"))?;
        let restart = self.restart.clone();
        let data_dir = self.data_dir.clone();
        let r = tokio::task::spawn_blocking(move || {
            apply_and_hand_over(&exe, &staging, &restart, &data_dir)
        })
        .await
        .map_err(|e| format!("適用タスクが失敗: {e}"))?;
        if let Err(e) = r {
            self.set(|i| {
                i.status = UpdateStatus::Failed;
                i.error = Some(e.clone());
            });
            return Err(e);
        }
        // Restart: graceful shutdown of this (old) instance; the finalize
        // helper waits for us to disappear and brings the new one up.
        let _ = self.shutdown_tx.send(true);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Pure helpers (unit-testable)
// ---------------------------------------------------------------------------

/// "0.2.0" / "1.2.3-beta.n" style → first three numeric segments.
pub fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut nums = [0u64; 3];
    let mut count = 0;
    for seg in v.split('.') {
        let numeric = seg
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap_or("");
        if numeric.is_empty() {
            return None;
        }
        let n = numeric.parse::<u64>().ok()?;
        if count < 3 {
            nums[count] = n;
        }
        count += 1;
    }
    if count == 0 {
        return None;
    }
    Some((nums[0], nums[1], nums[2]))
}

pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let pa = parse_version(a).unwrap_or((0, 0, 0));
    let pb = parse_version(b).unwrap_or((0, 0, 0));
    pa.cmp(&pb)
}

fn asset_by_name<'a>(assets: &'a Value, name: &str) -> Option<&'a Value> {
    assets
        .as_array()?
        .iter()
        .find(|a| a["name"].as_str() == Some(name))
}

/// Extract a Release from a GitHub `releases/latest` JSON body.
/// Returns None when the payload is not a release we can safely install
/// (missing tag, non-numeric tag, or missing exe/checksum assets).
pub fn parse_release(v: &Value) -> Option<Release> {
    let tag = v["tag_name"].as_str()?;
    let version = tag.trim_start_matches('v').to_string();
    parse_version(&version)?;
    let assets = &v["assets"];
    let exe_name = EXE_ASSET_NAMES
        .iter()
        .find(|n| asset_by_name(assets, n).is_some())?;
    let exe_url = asset_by_name(assets, exe_name)?["browser_download_url"]
        .as_str()?
        .to_string();
    let sha_name = format!("{exe_name}.sha256");
    let sha_url = asset_by_name(assets, &sha_name)?["browser_download_url"]
        .as_str()?
        .to_string();
    let notes_url = v["html_url"].as_str().unwrap_or_default().to_string();
    Some(Release {
        tag: tag.to_string(),
        version,
        notes_url,
        exe_url,
        sha_url,
    })
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut f = std::fs::File::open(path).map_err(|e| format!("ファイル読み込み失敗: {e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("ファイル読み込み失敗: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Fetch the latest release from `url`. Ok(None) when 404 (no releases yet).
fn fetch_latest(url: &str, version: &str) -> Result<Option<Release>, String> {
    let resp = ureq::get(url)
        .set("User-Agent", &format!("kenji-updater/{version}"))
        .set("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(20))
        .call();
    match resp {
        Ok(r) => {
            let v: Value = r
                .into_json()
                .map_err(|e| format!("GitHub 応答の解析に失敗: {e}"))?;
            let release = parse_release(&v).ok_or_else(|| {
                "リリースにWindows用バイナリとチェックサムがありません".to_string()
            })?;
            Ok(Some(release))
        }
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(ureq::Error::Status(code, r)) => {
            let msg = r.into_string().unwrap_or_default();
            Err(format!("GitHub API エラー {code}: {msg}"))
        }
        Err(e) => Err(format!("GitHub への接続に失敗: {e}")),
    }
}

/// Download the exe asset, fetch the .sha256 asset, verify. Leaves the staged
/// file in place on success; removes staged/temp files on any failure.
fn download_and_verify(release: &Release, dest: &Path) -> Result<(), String> {
    let tmp = dest.with_extension("part");
    let result = download_and_verify_impl(release, dest, &tmp);
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(dest);
    }
    result
}

fn download_and_verify_impl(release: &Release, dest: &Path, tmp: &Path) -> Result<(), String> {
    let resp = ureq::get(&release.exe_url)
        .set("User-Agent", "kenji-updater")
        .timeout(Duration::from_secs(300))
        .call()
        .map_err(|e| format!("ダウンロードに失敗: {e}"))?;
    {
        let mut out =
            std::fs::File::create(tmp).map_err(|e| format!("書き込み先の作成に失敗: {e}"))?;
        let mut reader = resp.into_reader().take(MAX_EXE_BYTES + 1);
        std::io::copy(&mut reader, &mut out).map_err(|e| format!("ダウンロード中のエラー: {e}"))?;
    }
    let size = std::fs::metadata(tmp).map(|m| m.len()).unwrap_or(0);
    if size > MAX_EXE_BYTES {
        return Err("バイナリが大きすぎます (上限128MB)".into());
    }
    let expected = match ureq::get(&release.sha_url)
        .set("User-Agent", "kenji-updater")
        .timeout(Duration::from_secs(20))
        .call()
    {
        Ok(r) => r
            .into_string()
            .map_err(|e| format!("チェックサムの読み取りに失敗: {e}"))?,
        Err(e) => return Err(format!("チェックサムの取得に失敗: {e}")),
    };
    let expected = expected
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let actual = sha256_file(tmp)?;
    if expected.is_empty() || expected != actual {
        return Err(format!(
            "SHA256チェックサムが一致しません（期待値 {expected}、実際 {actual}）"
        ));
    }
    // 以前のステージングが残っていても構わず置き換える
    let _ = std::fs::remove_file(dest);
    std::fs::rename(tmp, dest).map_err(|e| format!("ステージングに失敗: {e}"))?;
    Ok(())
}

fn old_binary_path(exe: &Path) -> PathBuf {
    PathBuf::from(format!("{}.old", exe.display()))
}

/// Hot-swap: the running exe can be renamed on Windows; put the new one in place.
fn swap_files(exe: &Path, staged: &Path) -> Result<(), String> {
    let old = old_binary_path(exe);
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old).map_err(|e| format!("実行バイナリの退避に失敗: {e}"))?;
    match std::fs::copy(staged, exe) {
        Ok(_) => {
            // ディスクへフラッシュ（電源断・クラッシュ時に exe が中途半端にならないように）
            if let Ok(f) = std::fs::File::open(exe) {
                let _ = f.sync_all();
            }
            Ok(())
        }
        Err(e) => {
            // Try to restore the original so the next boot still works.
            let _ = std::fs::rename(&old, exe);
            Err(format!("新バイナリの設置に失敗: {e}"))
        }
    }
}

fn spawn_detached(exe: &Path, args: &[String]) -> Result<(), String> {
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args);
    #[cfg(windows)]
    {
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| format!("ヘルパー起動に失敗: {e}"))
}

/// Swap the binary, then spawn the detached finalize helper. Does NOT shut
/// anything down — the caller triggers the graceful shutdown afterwards.
fn apply_and_hand_over(
    exe: &Path,
    staged: &Path,
    restart: &RestartMode,
    data_dir: &Path,
) -> Result<(), String> {
    swap_files(exe, staged)?;
    let mut args: Vec<String> = vec![
        "update-finalize".into(),
        "--data-dir".into(),
        data_dir.display().to_string(),
    ];
    if restart.service {
        args.push("--service".into());
    } else {
        args.push("--wait-pid".into());
        args.push(restart.wait_pid.to_string());
        args.extend(restart.relaunch_args.iter().cloned());
    }
    spawn_detached(exe, &args)
        .map_err(|e| format!("再起動ヘルパーの起動に失敗（現行プロセスは継続し、次回起動から新バイナリになります）: {e}"))
}

// ---------------------------------------------------------------------------
// update-finalize helper (runs detached after the swap)
// ---------------------------------------------------------------------------

fn finalize_log(data_dir: &Path, msg: &str) {
    let dir = data_dir.join("updates");
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("finalize.log"))
    {
        let _ = f.write_all(format!("[{}] {msg}\n", Utc::now().to_rfc3339()).as_bytes());
    }
}

fn pid_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let out = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .output();
        match out {
            Ok(o) if o.status.success() => {
                let text = String::from_utf8_lossy(&o.stdout);
                text.contains(&format!("\"{pid}\""))
            }
            _ => true, // unknown → keep waiting
        }
    }
    #[cfg(not(windows))]
    {
        // POSIX: signal 0 probes existence without sending anything.
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(true)
    }
}

#[cfg(windows)]
fn service_stopped() -> bool {
    use windows_service::service::ServiceAccess;
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    let manager = match ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
    {
        Ok(m) => m,
        Err(_) => return false,
    };
    let service =
        match manager.open_service(crate::service::SERVICE_NAME, ServiceAccess::QUERY_STATUS) {
            Ok(s) => s,
            Err(_) => return false,
        };
    matches!(
        service.query_status().map(|s| s.current_state),
        Ok(windows_service::service::ServiceState::Stopped)
    )
}

#[cfg(windows)]
fn start_service() -> Result<(), String> {
    use windows_service::service::ServiceAccess;
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|e| format!("SCMへの接続に失敗: {e}"))?;
    let service = manager
        .open_service(crate::service::SERVICE_NAME, ServiceAccess::START)
        .map_err(|e| {
            format!(
                "サービス'{}'を開けません: {e}",
                crate::service::SERVICE_NAME
            )
        })?;
    service
        .start::<&str>(&[])
        .map_err(|e| format!("サービスの開始に失敗: {e}"))
}

/// Entry point of the hidden `kenji update-finalize` subcommand.
pub fn finalize(
    data_dir: PathBuf,
    wait_pid: Option<u32>,
    service: bool,
    relaunch: Vec<String>,
) -> i32 {
    finalize_log(
        &data_dir,
        &format!("finalize開始 (wait_pid={wait_pid:?}, service={service}, relaunch={relaunch:?})"),
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(FINALIZE_TIMEOUT_SECS);
    loop {
        if service {
            #[cfg(windows)]
            {
                if service_stopped() {
                    break;
                }
            }
            #[cfg(not(windows))]
            {
                break;
            }
        } else if let Some(pid) = wait_pid {
            if !pid_alive(pid) {
                break;
            }
        } else {
            break;
        }
        if std::time::Instant::now() >= deadline {
            finalize_log(
                &data_dir,
                "タイムアウト: 前プロセスの停止を確認できませんでした。念のため再起動を試みます",
            );
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    std::thread::sleep(Duration::from_millis(500)); // settle sockets / SCM transitions

    if service {
        #[cfg(windows)]
        {
            match start_service() {
                Ok(()) => {
                    finalize_log(&data_dir, "サービスを再開しました");
                    0
                }
                Err(e) => {
                    finalize_log(&data_dir, &format!("サービス再開に失敗: {e}"));
                    1
                }
            }
        }
        #[cfg(not(windows))]
        {
            finalize_log(&data_dir, "service モードは Windows 専用です");
            1
        }
    } else if !relaunch.is_empty() {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("kenji"));
        match spawn_detached(&exe, &relaunch) {
            Ok(()) => {
                finalize_log(&data_dir, "新バイナリを再起動しました");
                0
            }
            Err(e) => {
                finalize_log(&data_dir, &format!("再起動に失敗: {e}"));
                1
            }
        }
    } else {
        finalize_log(&data_dir, "再起動动作なし（完了）");
        0
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::net::TcpListener;

    #[test]
    fn version_parsing_and_compare() {
        assert_eq!(parse_version("0.2.0"), Some((0, 2, 0)));
        assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("10.20.30.40"), Some((10, 20, 30))); // 4th segment ignored
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("dev"), None);
        assert_eq!(
            compare_versions("0.2.0", "0.1.9"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            compare_versions("0.2.0", "0.2.0"),
            std::cmp::Ordering::Equal
        );
        assert_eq!(
            compare_versions("0.2.0", "0.10.0"),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn parse_release_requires_assets_and_numeric_tag() {
        let body = json!({
            "tag_name": "v0.2.1",
            "html_url": "https://github.com/o/r/releases/tag/v0.2.1",
            "assets": [
                { "name": "kenji-windows-amd64.exe", "browser_download_url": "https://x/kenji.exe", "size": 7 },
                { "name": "kenji-windows-amd64.exe.sha256", "browser_download_url": "https://x/kenji.exe.sha256" }
            ]
        });
        let r = parse_release(&body).expect("release");
        assert_eq!(r.version, "0.2.1");
        assert_eq!(r.exe_url, "https://x/kenji.exe");

        // Checksum missing → not installable.
        let no_sha = json!({
            "tag_name": "v0.2.1",
            "assets": [{ "name": "kenji-windows-amd64.exe", "browser_download_url": "https://x/kenji.exe" }]
        });
        assert!(parse_release(&no_sha).is_none());

        // Non-numeric tag → ignored.
        let bad_tag = json!({
            "tag_name": "nightly",
            "assets": [
                { "name": "kenji.exe", "browser_download_url": "u1" },
                { "name": "kenji.exe.sha256", "browser_download_url": "u2" }
            ]
        });
        assert!(parse_release(&bad_tag).is_none());

        // 404-style body → None.
        assert!(parse_release(&json!({"message": "Not Found"})).is_none());
    }

    #[test]
    fn swap_files_replaces_and_keeps_old() {
        let dir = std::env::temp_dir().join(format!("kenji-swap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("kenji.exe");
        let staged = dir.join("kenji.exe.new");
        std::fs::write(&exe, b"OLD").unwrap();
        std::fs::write(&staged, b"NEWNEW").unwrap();

        swap_files(&exe, &staged).expect("swap");
        assert_eq!(std::fs::read(&exe).unwrap(), b"NEWNEW");
        assert_eq!(std::fs::read(old_binary_path(&exe)).unwrap(), b"OLD");

        // Second swap cleans up the previous .old instead of failing.
        std::fs::write(&staged, b"THIRD").unwrap();
        swap_files(&exe, &staged).expect("swap2");
        assert_eq!(std::fs::read(&exe).unwrap(), b"THIRD");
        assert_eq!(std::fs::read(old_binary_path(&exe)).unwrap(), b"NEWNEW");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Minimal HTTP server: serves a fixed map of path → (status, body).
    fn serve(
        paths: Vec<(String, &'static [u8])>,
    ) -> (std::net::SocketAddr, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                let (status, body) = paths
                    .iter()
                    .find(|(p, _)| *p == path)
                    .map(|(_, b)| (200, b.to_vec()))
                    .unwrap_or((404, b"not found".to_vec()));
                let head = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
                let _ = stream.flush();
            }
        });
        (addr, handle)
    }

    #[test]
    fn download_and_verify_end_to_end_and_bad_checksum() {
        let exe_bytes: &[u8] = b"fake-binary-content-0123456789";
        let mut sha_input = Sha256::new();
        sha_input.update(exe_bytes);
        let sha = hex::encode(sha_input.finalize());
        let sha_body = format!("{sha}  kenji-windows-amd64.exe\n").into_bytes();

        // --- good checksum ---
        let (addr, _t) = serve(vec![
            ("/dl/kenji.exe".to_string(), exe_bytes),
            (
                "/dl/kenji.exe.sha256".to_string(),
                Box::leak(sha_body.clone().into_boxed_slice()),
            ),
        ]);
        let base = format!("http://{addr}/dl");
        let release = Release {
            tag: "v9.9.9".into(),
            version: "9.9.9".into(),
            notes_url: String::new(),
            exe_url: format!("{base}/kenji.exe"),
            sha_url: format!("{base}/kenji.exe.sha256"),
        };
        let dest = std::env::temp_dir().join(format!("kenji-dl-{}", std::process::id()));
        download_and_verify(&release, &dest).expect("verified download");
        assert_eq!(std::fs::read(&dest).unwrap(), exe_bytes);
        let _ = std::fs::remove_file(&dest);

        // --- checksum mismatch → error + no staged file ---
        let (addr2, _t2) = serve(vec![
            ("/dl/kenji.exe".to_string(), exe_bytes),
            (
                "/dl/kenji.exe.sha256".to_string(),
                b"0000000000000000000000000000000000000000000000000000000000000000  kenji.exe\n",
            ),
        ]);
        let base2 = format!("http://{addr2}/dl");
        let release2 = Release {
            tag: "v9.9.9".into(),
            version: "9.9.9".into(),
            notes_url: String::new(),
            exe_url: format!("{base2}/kenji.exe"),
            sha_url: format!("{base2}/kenji.exe.sha256"),
        };
        let err = download_and_verify(&release2, &dest).unwrap_err();
        assert!(err.contains("SHA256"));
        assert!(!dest.exists());
    }
}
