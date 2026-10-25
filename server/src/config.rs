use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalConfig {
    pub shell: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelConfig {
    pub enabled: bool,
    /// トンネル実装の選択。URL は起動のたびに動的に取得され、
    /// 固定ドメインを前提にしない（ランダムURL運用を本番でも通るようにする）。
    #[serde(default)]
    pub provider: TunnelProvider,
}

/// トンネル実装の選択。`auto` は cloudflared → ngrok の順で利用可能なものを使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TunnelProvider {
    #[default]
    Auto,
    Cloudflared,
    Ngrok,
}

impl std::fmt::Display for TunnelProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            TunnelProvider::Auto => "auto",
            TunnelProvider::Cloudflared => "cloudflared",
            TunnelProvider::Ngrok => "ngrok",
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateConfig {
    /// 起動時・定期の自動確認（GitHub Releases）
    pub auto_check: bool,
    /// 新バージョン検出時に確認・SHA256検証・適用まで自動で行う
    pub auto_install: bool,
    pub check_interval_hours: u32,
    /// "owner/repo" 形式の配布元
    pub repo: String,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        UpdateConfig {
            auto_check: true,
            auto_install: false,
            check_interval_hours: 24,
            repo: "bata-san/kenji".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub bind: String,
    pub port: u16,
    #[serde(skip_serializing, default)] // implicit: where config.json lives
    pub data_dir: PathBuf,
    pub workspace_root: PathBuf,
    pub terminal: TerminalConfig,
    pub tunnel: TunnelConfig,
    #[serde(default)]
    pub update: UpdateConfig,
    #[serde(default)]
    pub desktop_priority: bool,
    #[serde(default)]
    pub repair: crate::repair::RepairConfig,
}

impl Config {
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("config.json")
    }

    /// Load config.json from the data dir, creating a default one if missing.
    pub fn load(data_dir: &Path) -> Result<Config> {
        let p = Self::path(data_dir);
        if p.exists() {
            let raw =
                std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
            let mut cfg: Config =
                serde_json::from_str(&raw).with_context(|| format!("parsing {}", p.display()))?;
            cfg.data_dir = data_dir.to_path_buf();
            Ok(cfg)
        } else {
            let cfg = Config::default_for(data_dir);
            cfg.save()?;
            Ok(cfg)
        }
    }
    pub fn save(&self) -> Result<()> {
        let p = Config::path(&self.data_dir);
        let raw = serde_json::to_string_pretty(self)?;
        let tmp = p.with_extension("json.tmp");
        use std::io::Write;
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(raw.as_bytes())?;
        file.sync_all()?;
        drop(file);
        // Replace only after the complete file has reached disk.
        std::fs::rename(&tmp, &p)?;
        Ok(())
    }

    fn default_for(data_dir: &Path) -> Config {
        Config {
            bind: "127.0.0.1".into(),
            port: 8787,
            data_dir: data_dir.to_path_buf(),
            workspace_root: data_dir.join("workspace"),
            terminal: TerminalConfig {
                shell: "powershell.exe".into(),
                args: vec!["-NoLogo".into(), "-NoProfile".into()],
            },
            tunnel: TunnelConfig {
                enabled: false,
                provider: TunnelProvider::Auto,
            },
            update: UpdateConfig::default(),
            desktop_priority: false,
            repair: crate::repair::RepairConfig::default(),
        }
    }
}

pub fn generate_secret() -> String {
    use rand::RngCore;
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    hex::encode(b)
}
