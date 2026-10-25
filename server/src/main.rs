mod audit;
mod auth;
mod config;
mod error;
#[cfg(windows)]
mod service;
mod files;
mod jobs;
mod metrics;
mod mcp;
mod pty;
mod routes;
mod state;
mod tunnel;

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use clap::{Parser, Subcommand};
use tokio::sync::watch;

use crate::audit::Audit;
use crate::auth::AuthStore;
use crate::config::Config;
use crate::jobs::JobManager;
use crate::metrics::MetricsCache;
use crate::pty::TerminalManager;
use crate::state::{AppState, SharedState};
use crate::tunnel::TunnelSupervisor;

pub fn default_data_dir() -> PathBuf {
    std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("kenji")
}

#[derive(Parser)]
#[command(name = "kenji", version, about = "Kenji — PCを汎用リモートコンピューティング基盤に変換する")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// フォアグラウンドで実行（開発・動作確認用）
    Serve {
        #[arg(long)]
        bind: Option<String>,
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        data_dir: Option<PathBuf>,
    },
    /// 初回セットアップ: 管理者アカウントと各種設定を作成
    Setup {
        #[arg(long)]
        data_dir: Option<PathBuf>,
        #[arg(long)]
        username: Option<String>,
        #[arg(long)]
        password: Option<String>,
        /// 既存の auth.json を管理者パスワードごと再作成する
        #[arg(long)]
        force: bool,
    },
    /// Windowsサービスの管理（要管理者権限）
    Service {
        #[command(subcommand)]
        sub: ServiceSub,
    },
    /// MCPサーバーとしてstdioで動作（AIエージェント接続用）
    Mcp {
        /// Kenji APIのベースURL（例: http://127.0.0.1:8080）
        #[arg(long)]
        url: String,
        /// APIキーまたはJWT
        #[arg(long)]
        token: String,
    },
}

#[derive(Subcommand)]
enum ServiceSub {
    /// サービスとして登録（自動開始）
    Install {
        #[arg(long)]
        account: Option<String>,
        #[arg(long)]
        password: Option<String>,
    },
    /// 登録を削除
    Uninstall,
    /// SCM から呼ばれる本体（手動実行しない）
    Run {
        #[arg(long)]
        data_dir: Option<PathBuf>,
    },
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.cmd {
        Cmd::Serve { bind, port, data_dir } => do_serve(bind, port, data_dir),
        Cmd::Setup { data_dir, username, password, force } => do_setup(data_dir, username, password, force),
        #[cfg(windows)]
        Cmd::Service { sub } => match sub {
            ServiceSub::Install { account, password } => {
                let acct = match (account, password) {
                    (Some(a), Some(p)) => Some((a, p)),
                    (None, None) => None,
                    (Some(_), None) | (None, Some(_)) => {
                        eprintln!("--account と --password はセットで指定してください");
                        std::process::exit(1);
                    }
                };
                match service::install(acct) {
                    Ok(()) => 0,
                    Err(e) => {
                        eprintln!("エラー: {e:#}");
                        1
                    }
                }
            }
            ServiceSub::Uninstall => match service::uninstall() {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("エラー: {e:#}");
                    1
                }
            },
            ServiceSub::Run { .. } => match service::run() {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("エラー: {e:#}");
                    1
                }
            },
        },
        #[cfg(not(windows))]
        Cmd::Service { .. } => {
            eprintln!("service サブコマンドは Windows 専用です");
            1
        }
        Cmd::Mcp { url, token } => mcp::run(url, token),
    };
    std::process::exit(code);
}

fn do_serve(bind: Option<String>, port: Option<u16>, data_dir: Option<PathBuf>) -> i32 {
    let data_dir = data_dir.unwrap_or_else(default_data_dir);
    let auth_path = data_dir.join("auth.json");
    if !auth_path.exists() {
        eprintln!("初回セットアップが未実施です。先に `kenji setup` を実行してください。");
        return 1;
    }
    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("ランタイム初期化エラー: {e}");
            return 1;
        }
    };
    rt.block_on(async move {
        let (sd_tx, sd_rx) = watch::channel(false);
        let tx = sd_tx.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                println!("停止シグナルを受信しました…");
                let _ = tx.send(true);
            }
        });
        run_server(data_dir, bind, port, sd_rx).await
    })
}

/// Shared server entry point for console and Windows-service modes.
pub async fn run_server(
    data_dir: PathBuf,
    bind: Option<String>,
    port: Option<u16>,
    shutdown_rx: watch::Receiver<bool>,
) -> i32 {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,hyper=off".into()),
        )
        .init();

    let cfg = match Config::load(&data_dir) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("設定の読み込みに失敗しました: {e:#}");
            return 1;
        }
    };
    if let Err(e) = std::fs::create_dir_all(&cfg.workspace_root) {
        eprintln!("ワークスペース作成に失敗: {e:#}");
        return 1;
    }
    let auth = match AuthStore::load(&data_dir.join("auth.json")) {
        Ok(a) => Arc::new(a),
        Err(e) => {
            eprintln!("認証情報の読み込みに失敗しました: {e:#}（`kenji setup` を実行してください）");
            return 1;
        }
    };
    let audit = Arc::new(Audit::new(&data_dir.join("audit")));
    let jobs = JobManager::new(&data_dir, cfg.workspace_root.clone(), audit.clone());
    if let Err(e) = jobs.load() {
        eprintln!("ジョブ定義の読み込みに失敗: {e:#}");
        return 1;
    }
    let metrics = MetricsCache::new();
    metrics::start_poller(metrics.clone(), jobs.clone());
    jobs.start_scheduler();

    let tunnel = TunnelSupervisor::new(data_dir.clone());
    if cfg.tunnel.enabled {
        tunnel.start(cfg.port);
    }

    let state: SharedState = Arc::new(AppState {
        config: RwLock::new(cfg),
        auth,
        audit,
        jobs,
        metrics,
        tunnel,
        terminals: TerminalManager::new(),
        setup_done: true,
        version: env!("CARGO_PKG_VERSION"),
    });

    // Periodically persist API-key metadata (last_used_at).
    {
        let auth = state.auth.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                auth.flush_if_dirty();
            }
        });
    }

    let (bind_s, port_n) = {
        let c = state.config.read().unwrap();
        (c.bind.clone(), c.port)
    };
    let bind_addr = format!("{}:{}", bind.unwrap_or(bind_s), port.unwrap_or(port_n));
    let listener = match tokio::net::TcpListener::bind(&bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("ポート {bind_addr} のバインドに失敗: {e}");
            return 1;
        }
    };
    println!("Kenji v{} 起動 — http://{bind_addr}", env!("CARGO_PKG_VERSION"));
    println!("  データ:   {}", data_dir.display());
    println!("  作業領域: {}", state.config.read().unwrap().workspace_root.display());

    let app = routes::build_router(state.clone());
    let mut rx = shutdown_rx;
    let serve = axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>())
        .with_graceful_shutdown(async move {
            loop {
                if rx.changed().await.is_err() {
                    break;
                }
                if *rx.borrow() {
                    break;
                }
            }
        });
    if let Err(e) = serve.await {
        eprintln!("サーバーエラー: {e}");
    }
    // Graceful cleanup: managed children and the tunnel die with the server.
    state.jobs.stop_all();
    state.tunnel.stop();
    // Give spawned taskkill commands a moment to complete before exit.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    println!("停止しました。");
    0
}

fn do_setup(data_dir: Option<PathBuf>, username: Option<String>, password: Option<String>, force: bool) -> i32 {
    let data_dir = data_dir.unwrap_or_else(default_data_dir);
    if let Err(e) = std::fs::create_dir_all(&data_dir) {
        eprintln!("データディレクトリ作成に失敗: {e:#}");
        return 1;
    }
    let auth_path = data_dir.join("auth.json");
    if auth_path.exists() && !force {
        eprintln!("既にセットアップ済みです（{}）。再設定するには --force を指定してください。", auth_path.display());
        return 1;
    }
    let username = username.unwrap_or_else(|| prompt_line("管理者ユーザー名 [admin]: ").trim().to_string());
    let username = if username.is_empty() { "admin".to_string() } else { username };
    let password = match password {
        Some(p) => {
            if p.len() < 6 {
                eprintln!("パスワードは6文字以上にしてください。");
                return 1;
            }
            p
        }
        None => loop {
            let p1 = match rpassword::prompt_password("管理者パスワード: ") {
                Ok(p) => p,
                Err(_) => {
                    eprintln!("パスワードの読み込みに失敗しました（非対話環境では --password を指定してください）");
                    return 1;
                }
            };
            if p1.len() < 6 {
                eprintln!("パスワードは6文字以上にしてください。");
                continue;
            }
            let p2 = rpassword::prompt_password("もう一度入力してください: ").unwrap_or_default();
            if p1 != p2 {
                eprintln!("一致しません。やり直してください。");
                continue;
            }
            break p1;
        },
    };

    let cfg = match Config::load(&data_dir) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("設定作成に失敗: {e:#}");
            return 1;
        }
    };
    if let Err(e) = std::fs::create_dir_all(&cfg.workspace_root) {
        eprintln!("ワークスペース作成に失敗: {e:#}");
        return 1;
    }
    let secret = config::generate_secret();
    if let Err(e) = AuthStore::create(&auth_path, &username, &password, &secret) {
        eprintln!("認証情報の作成に失敗: {e:#}");
        return 1;
    }
    harden_acl(&data_dir);

    println!("セットアップ完了。");
    println!("  データ:     {}", data_dir.display());
    println!("  作業領域:   {}", cfg.workspace_root.display());
    println!();
    println!("次のステップ:");
    println!("  * すぐに使う      : kenji serve   → http://127.0.0.1:{}", cfg.port);
    println!("  * 常駐サービス化  : 管理者権限で kenji service install → sc start kenji");
    println!("  * 外部アクセス    : cloudflared を導入し、設定でトンネルを有効化");
    if force {
        println!("  注意: --force により管理者パスワードを再設定しました（APIキーは保持されます）。");
    }
    0
}

fn prompt_line(msg: &str) -> String {
    print!("{msg}");
    let _ = std::io::stdout().flush();
    let mut s = String::new();
    let _ = std::io::stdin().read_line(&mut s);
    s
}

/// Restrict secrets (auth.json) to SYSTEM / Administrators / the installing user.
fn harden_acl(data_dir: &std::path::Path) {
    #[cfg(windows)]
    {
        let user = std::process::Command::new("whoami")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        let Some(user) = user else {
            eprintln!("警告: ユーザー名を取得できず、ACLの厳格化をスキップしました。");
            return;
        };
        for name in ["auth.json", "config.json"] {
            let p = data_dir.join(name);
            if !p.exists() {
                continue;
            }
            let res = std::process::Command::new("icacls")
                .arg(&p)
                .arg("/inheritance:r")
                .arg("/grant:r")
                .arg(format!("{user}:(F)"))
                .arg("/grant:r")
                .arg("SYSTEM:(F)")
                .arg("/grant:r")
                .arg("Administrators:(F)")
                .output();
            match res {
                Ok(o) if o.status.success() => {}
                _ => eprintln!("警告: {} のACL設定に失敗しました。", p.display()),
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = data_dir;
    }
}
