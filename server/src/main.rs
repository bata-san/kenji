mod audit;
mod auth;
mod config;
mod error;
mod files;
mod jobs;
mod mcp;
mod metrics;
mod pty;
mod repair;
mod resources;
mod routes;
#[cfg(windows)]
mod service;
mod state;
mod tunnel;
mod update;

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use clap::{Parser, Subcommand};
use futures_util::FutureExt as _;
use tokio::sync::watch;

use crate::audit::Audit;
use crate::auth::{AuthStore, Role, UserError};
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
#[command(
    name = "kenji",
    version,
    about = "Kenji — PCを汎用リモートコンピューティング基盤に変換する"
)]
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
        /// 既存の管理者パスワードを再設定する（他のユーザーとAPIキーは保持）
        #[arg(long)]
        force: bool,
    },
    /// ユーザー管理（SQLite: users.db）
    User {
        #[arg(long, global = true)]
        data_dir: Option<PathBuf>,
        #[command(subcommand)]
        sub: UserSub,
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
    /// 独立したAI診断ジョブ（内部利用）
    #[command(hide = true)]
    RepairRun {
        #[arg(long)]
        data_dir: PathBuf,
    },
    /// 独立したクラッシュ監視プロセス（内部利用）
    #[command(hide = true)]
    RepairWatch {
        #[arg(long)]
        pid: u32,
        #[arg(long)]
        data_dir: PathBuf,
    },
    /// アップデート適用後の再起動ヘルパー（内部利用・手動実行しない）
    #[command(hide = true)]
    UpdateFinalize {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long)]
        wait_pid: Option<u32>,
        #[arg(long)]
        service: bool,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        relaunch: Vec<String>,
    },
}

#[derive(Subcommand)]
enum UserSub {
    /// ユーザーを追加
    Add {
        #[arg(long)]
        username: String,
        #[arg(long)]
        password: String,
        #[arg(long, default_value = "viewer")]
        role: String,
    },
    /// ユーザー一覧
    List,
    /// パスワードを変更
    Passwd {
        #[arg(long)]
        username: String,
        #[arg(long)]
        password: String,
    },
    /// ユーザーを削除
    Delete {
        #[arg(long)]
        username: String,
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
        Cmd::Serve {
            bind,
            port,
            data_dir,
        } => do_serve(bind, port, data_dir),
        Cmd::Setup {
            data_dir,
            username,
            password,
            force,
        } => do_setup(data_dir, username, password, force),
        Cmd::User { data_dir, sub } => do_user(data_dir, sub),
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
        Cmd::RepairRun { data_dir } => match Config::load(&data_dir).and_then(|cfg| {
            repair::run(
                &data_dir,
                &cfg.repair,
                "Operator requested a manual diagnostic check. No crash details supplied.",
            )
        }) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("AI診断: {e:#}");
                1
            }
        },
        Cmd::RepairWatch { pid, data_dir } => match repair::watch(pid, data_dir) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("修復監視: {e:#}");
                1
            }
        },
        Cmd::UpdateFinalize {
            data_dir,
            wait_pid,
            service,
            relaunch,
        } => update::finalize(data_dir, wait_pid, service, relaunch),
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
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
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
        // アップデート適用後に同じ設定で再起動するための引数定義
        let mut relaunch = vec![
            "serve".to_string(),
            "--data-dir".to_string(),
            data_dir.display().to_string(),
        ];
        if let Some(b) = bind.as_ref() {
            relaunch.push("--bind".into());
            relaunch.push(b.clone());
        }
        if let Some(p) = port.as_ref() {
            relaunch.push("--port".into());
            relaunch.push(p.to_string());
        }
        run_server(
            data_dir,
            bind,
            port,
            sd_tx,
            sd_rx,
            update::RestartMode::foreground(relaunch),
        )
        .await
    })
}

/// 標準出力に加えて logs/server.log にも書き出す（サービスモードでも状況が追える）。
/// 起動時に 10MB 超過していれば server.log.1 へローテーションする。
fn init_tracing(data_dir: &std::path::Path) {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,hyper=off".into());

    let dir = data_dir.join("logs");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("server.log");
    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > 10 * 1024 * 1024 {
        let _ = std::fs::rename(&path, dir.join("server.log.1"));
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path);

    let registry = tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::Layer::default());
    match file {
        Ok(f) => {
            registry
                .with(
                    tracing_subscriber::fmt::Layer::default()
                        .with_ansi(false)
                        .with_writer(SharedFile(std::sync::Mutex::new(f))),
                )
                .init();
        }
        Err(e) => {
            eprintln!("警告: ログファイル {} を開けません: {e}", path.display());
            registry.init();
        }
    }
}

struct SharedFile(std::sync::Mutex<std::fs::File>);

impl std::io::Write for &SharedFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).flush()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SharedFile {
    type Writer = &'a SharedFile;
    fn make_writer(&'a self) -> Self::Writer {
        self
    }
}

/// Shared server entry point for console and Windows-service modes.
pub async fn run_server(
    data_dir: PathBuf,
    bind: Option<String>,
    port: Option<u16>,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
    restart: update::RestartMode,
) -> i32 {
    init_tracing(&data_dir);

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
            eprintln!(
                "認証情報の読み込みに失敗しました: {e:#}（`kenji setup` を実行してください）"
            );
            return 1;
        }
    };
    // 旧バージョンからの移行で users.db が作られた場合も含めて ACL を厳格化する。
    harden_acl(&data_dir);
    // Every existing account owns a folder named after its username. Missing
    // folders are recreated on startup so a manual cleanup cannot break logins.
    match auth.list_users() {
        Ok(users) => {
            for user in users {
                if let Err(e) = files::ensure_user_root(&cfg.workspace_root, &user.username) {
                    tracing::error!(
                        "ユーザー `{}` の作業フォルダを準備できません: {e:#}",
                        user.username
                    );
                }
            }
        }
        Err(e) => {
            eprintln!("ユーザー一覧の読み込みに失敗しました: {e:#}");
            return 1;
        }
    }
    if !auth.list_keys().is_empty() {
        if let Err(e) = files::ensure_user_root(&cfg.workspace_root, "key:shared") {
            tracing::error!("APIキー共有作業フォルダを準備できません: {e:#}");
        }
    }
    let resources = match resources::ResourceGuard::new() {
        Ok(guard) => Arc::new(guard),
        Err(e) => {
            tracing::error!("リソース保護初期化失敗: {e:#}");
            return 1;
        }
    };
    if let Err(e) = resources.set_enabled(cfg.desktop_priority) {
        tracing::error!("リソース保護設定失敗: {e:#}");
        return 1;
    }
    if let Err(e) = repair::start_observer(&data_dir) {
        tracing::error!("クラッシュ監視起動失敗: {e:#}");
        if cfg.repair.enabled {
            return 1;
        }
    }
    let audit = Arc::new(Audit::new(&data_dir.join("audit")));
    let jobs = JobManager::new(
        &data_dir,
        cfg.workspace_root.clone(),
        audit.clone(),
        resources,
    );
    if let Err(e) = jobs.load() {
        // リモート運用で起動不能になるのを避けるため、破損ファイルを退避して空で起動する
        eprintln!("警告: ジョブ定義の読み込みに失敗: {e:#}（jobs.json を退避して空で起動します）");
        tracing::error!("jobs.json 読み込み失敗: {e:#}");
        let corrupt = data_dir.join("jobs.json");
        if corrupt.exists() {
            let bak = data_dir.join(format!(
                "jobs.json.corrupt-{}",
                chrono::Utc::now().format("%Y%m%d-%H%M%S")
            ));
            match std::fs::rename(&corrupt, &bak) {
                Ok(()) => eprintln!("  破損ファイルを退避: {}", bak.display()),
                Err(e2) => {
                    eprintln!("エラー: 退避に失敗したため起動を中断します: {e2}");
                    return 1;
                }
            }
        }
        if let Err(e) = jobs.load() {
            eprintln!("エラー: ジョブ定義の再読み込みに失敗しました: {e:#}");
            return 1;
        }
    }
    let metrics = MetricsCache::new();
    metrics::start_poller(metrics.clone(), jobs.clone());
    jobs.start_scheduler();

    let tunnel = TunnelSupervisor::new(data_dir.clone());
    if cfg.tunnel.enabled {
        tunnel.start(cfg.port);
    }

    let update = update::UpdateManager::new(
        data_dir.clone(),
        env!("CARGO_PKG_VERSION").to_string(),
        shutdown_tx,
        restart,
    );
    let state: SharedState = Arc::new(AppState {
        config: RwLock::new(cfg),
        auth,
        audit,
        jobs,
        metrics,
        tunnel,
        terminals: TerminalManager::new(),
        update,
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

    // Reap shells that exited on their own so their tab slots are released.
    {
        let terminals = state.terminals.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                terminals.prune_exited();
            }
        });
    }

    // 定期アップデート確認（auto_check 有効時）。auto_install が有効なら検証・適用まで行う。
    {
        let st = state.clone();
        tokio::spawn(async move {
            // 起動直後はサーバーが安定してから確認する
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            loop {
                let (auto_check, auto_install, hours, repo) = {
                    let c = st.config.read().unwrap_or_else(|e| e.into_inner());
                    (
                        c.update.auto_check,
                        c.update.auto_install,
                        c.update.check_interval_hours,
                        c.update.repo.clone(),
                    )
                };
                if auto_check {
                    // パニックで定期確認タスク全体が停止しないよう隔離する
                    let r = std::panic::AssertUnwindSafe(async {
                        if let Ok(Some(rel)) = st.update.check(&repo).await {
                            let newer = matches!(
                                update::compare_versions(&rel.version, st.version),
                                std::cmp::Ordering::Greater
                            );
                            if auto_install && newer {
                                let _ = st.update.install().await;
                            }
                        }
                    })
                    .catch_unwind()
                    .await;
                    if r.is_err() {
                        tracing::error!(
                            "自動アップデート確認でパニックが発生しました（次回サイクルで再試行）"
                        );
                    }
                }
                let secs = (hours.max(1) as u64) * 3600;
                tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
            }
        });
    }

    let (bind_s, port_n) = {
        let c = state.config.read().unwrap_or_else(|e| e.into_inner());
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
    println!(
        "Kenji v{} 起動 — http://{bind_addr}",
        env!("CARGO_PKG_VERSION")
    );
    println!("  データ:   {}", data_dir.display());
    println!(
        "  作業領域: {}",
        state
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .workspace_root
            .display()
    );

    let app = routes::build_router(state.clone());
    let mut rx = shutdown_rx;
    let serve = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
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
    let failed = if let Err(e) = serve.await {
        eprintln!("サーバーエラー: {e}");
        true
    } else {
        false
    };
    // Graceful cleanup: managed children and the tunnel die with the server.
    state.jobs.stop_all();
    state.terminals.shutdown_all();
    state.tunnel.stop();
    // Give spawned taskkill commands a moment to complete before exit.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    println!("停止しました。");
    if failed {
        1
    } else {
        0
    }
}

fn do_setup(
    data_dir: Option<PathBuf>,
    username: Option<String>,
    password: Option<String>,
    force: bool,
) -> i32 {
    let data_dir = data_dir.unwrap_or_else(default_data_dir);
    if let Err(e) = std::fs::create_dir_all(&data_dir) {
        eprintln!("データディレクトリ作成に失敗: {e:#}");
        return 1;
    }
    let auth_path = data_dir.join("auth.json");
    if auth_path.exists() && !force {
        eprintln!(
            "既にセットアップ済みです（{}）。再設定するには --force を指定してください。",
            auth_path.display()
        );
        return 1;
    }
    let username =
        username.unwrap_or_else(|| prompt_line("管理者ユーザー名 [admin]: ").trim().to_string());
    let username = if username.is_empty() {
        "admin".to_string()
    } else {
        username
    };
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
    if let Err(e) = files::ensure_user_root(&cfg.workspace_root, &username) {
        eprintln!("管理者の作業フォルダ作成に失敗: {e:#}");
        return 1;
    }
    harden_acl(&data_dir);

    println!("セットアップ完了。");
    println!("  データ:     {}", data_dir.display());
    println!("  作業領域:   {}", cfg.workspace_root.display());
    println!();
    println!("次のステップ:");
    println!(
        "  * すぐに使う      : kenji serve   → http://127.0.0.1:{}",
        cfg.port
    );
    println!("  * 常駐サービス化  : 管理者権限で kenji service install → sc start kenji");
    println!("  * 外部アクセス    : cloudflared を導入し、設定でトンネルを有効化");
    if force {
        println!(
            "  注意: --force により管理者パスワードを再設定しました（APIキーは保持されます）。"
        );
    }
    0
}

fn do_user(data_dir: Option<PathBuf>, sub: UserSub) -> i32 {
    let data_dir = data_dir.unwrap_or_else(default_data_dir);
    let auth_path = data_dir.join("auth.json");
    if !auth_path.exists() {
        eprintln!(
            "認証情報がありません（{}）。先に `kenji setup` を実行してください。",
            auth_path.display()
        );
        return 1;
    }
    let store = match AuthStore::load(&auth_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("認証情報の読み込みに失敗しました: {e:#}");
            return 1;
        }
    };

    let result: Result<(), UserError> = match sub {
        UserSub::Add {
            username,
            password,
            role,
        } => {
            let Some(role) = Role::parse(&role) else {
                eprintln!("ロールは viewer / operator / admin のいずれかで指定してください。");
                return 1;
            };
            let cfg = match Config::load(&data_dir) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("設定の読み込みに失敗しました: {e:#}");
                    return 1;
                }
            };
            match store.create_user(&username, &password, role) {
                Ok(u) => {
                    if let Err(e) = files::ensure_user_root(&cfg.workspace_root, &u.username) {
                        let _ = store.delete_user(&u.username);
                        eprintln!(
                            "ユーザー `{}` の作業フォルダ作成に失敗しました: {e:#}",
                            u.username
                        );
                        return 1;
                    }
                    println!(
                        "ユーザー `{}` を追加しました（ロール: {}）。",
                        u.username,
                        u.role.as_str()
                    );
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }
        UserSub::List => store
            .list_users()
            .map(|users| {
                if users.is_empty() {
                    println!("ユーザーがいません。");
                    return;
                }
                println!(
                    "{:<24} {:<10} {:<20} {}",
                    "USERNAME", "ROLE", "CREATED", "STATUS"
                );
                for u in users {
                    let created = chrono::DateTime::parse_from_rfc3339(&u.created_at)
                        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_else(|_| u.created_at.clone());
                    println!(
                        "{:<24} {:<10} {:<20} {}",
                        u.username,
                        u.role.as_str(),
                        created,
                        if u.disabled { "disabled" } else { "active" }
                    );
                }
            })
            .map_err(UserError::Other),
        UserSub::Passwd { username, password } => store
            .set_password(&username, &password)
            .and_then(|changed| {
                if changed {
                    println!("ユーザー `{username}` のパスワードを変更しました。");
                    Ok(())
                } else {
                    Err(UserError::NotFound)
                }
            }),
        UserSub::Delete { username } => store.delete_user(&username).and_then(|deleted| {
            if deleted {
                println!("ユーザー `{username}` を削除しました。");
                Ok(())
            } else {
                Err(UserError::NotFound)
            }
        }),
    };

    // users.db はここで新規作成される場合があるため、秘密情報の ACL を厳格化する。
    harden_acl(&data_dir);

    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("エラー: {e}");
            1
        }
    }
}

fn prompt_line(msg: &str) -> String {
    print!("{msg}");
    let _ = std::io::stdout().flush();
    let mut s = String::new();
    let _ = std::io::stdin().read_line(&mut s);
    s
}

/// Restrict secrets (auth.json, users.db) to SYSTEM / Administrators / the installing user.
fn harden_acl(data_dir: &std::path::Path) {
    #[cfg(windows)]
    {
        // `whoami` はコンソールのコードページによって文字化けするため、
        // プロセスの環境変数からアカウント名を組み立てる。
        let user = std::env::var("USERNAME")
            .ok()
            .filter(|u| !u.trim().is_empty());
        let Some(user) = user else {
            eprintln!("警告: ユーザー名を取得できず、ACLの厳格化をスキップしました。");
            return;
        };
        let account = match std::env::var("USERDOMAIN") {
            Ok(domain) if !domain.trim().is_empty() => format!("{domain}\\{user}"),
            _ => user,
        };
        for name in [
            "auth.json",
            "config.json",
            "users.db",
            "users.db-journal",
            "users.db-wal",
            "users.db-shm",
        ] {
            let p = data_dir.join(name);
            if !p.exists() {
                continue;
            }
            let res = std::process::Command::new("icacls")
                .arg(&p)
                .arg("/inheritance:r")
                .arg("/grant:r")
                .arg(format!("{account}:(F)"))
                .arg("SYSTEM:(F)")
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
