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
        std::fs::write(&tmp, raw.as_bytes())?;
        // atomic-ish replace
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
            tunnel: TunnelConfig { enabled: false },
            update: UpdateConfig::default(),
        }
    }
}

pub fn generate_secret() -> String {
    use rand::RngCore;
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    hex::encode(b)
}
