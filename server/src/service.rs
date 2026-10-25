//! Windows Service integration (SCM): install / uninstall / run.

use std::ffi::OsString;
use std::io::Write;
use std::sync::mpsc;

use anyhow::{Context, Result};
use windows_service::service::{
    ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
    ServiceInfo, ServiceState, ServiceStatus, ServiceStartType, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

pub const SERVICE_NAME: &str = "kenji";
const SERVICE_DISPLAY: &str = "Kenji Computing Platform";
const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

fn log_to_file(msg: &str) {
    let dir = crate::default_data_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("logs").join("service.log"))
    {
        let _ = f.write_all(format!("[{}] {msg}\n", chrono::Utc::now().to_rfc3339()).as_bytes());
    }
}

define_windows_service!(ffi_service_main, windows_service_main);

fn windows_service_main(_arguments: Vec<OsString>) {
    if let Err(e) = run_service_impl() {
        let msg = format!("service error: {e:#}");
        eprintln!("{msg}");
        log_to_file(&msg);
        // SCM will mark the service failed via exit code.
        std::process::exit(1);
    }
}

/// Entry: run the HTTP server as a Windows service (called via SCM).
pub fn run() -> Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main).context("サービスディスパッチャの開始に失敗（サービスコンテキスト外での実行?)")
}

fn run_service_impl() -> Result<()> {
    let (stop_tx, stop_rx) = mpsc::channel::<ServiceControl>();
    let event_handler = service_control_handler::register(SERVICE_NAME, move |event| {
        if event == ServiceControl::Stop {
            let _ = stop_tx.send(ServiceControl::Stop);
        }
        ServiceControlHandlerResult::NoError
    })
    .context("SCM ハンドラ登録に失敗")?;

    event_handler
        .set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: ServiceState::StartPending,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: std::time::Duration::from_secs(20),
            process_id: None,
        })
        .context("ステータス更新に失敗")?;

    let data_dir = crate::default_data_dir();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("tokio ランタイム構築に失敗")?;

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let server = {
        let data_dir = data_dir.clone();
        let shutdown_tx = shutdown_tx.clone();
        rt.spawn(async move {
            crate::run_server(
                data_dir,
                None,
                None,
                shutdown_tx,
                shutdown_rx,
                crate::update::RestartMode::service(),
            )
            .await
        })
    };

    event_handler
        .set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: ServiceState::Running,
            controls_accepted: ServiceControlAccept::STOP,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: std::time::Duration::default(),
            process_id: None,
        })
        .context("ステータス更新に失敗")?;

    // Block the service thread until SCM requests stop.
    loop {
        match stop_rx.recv() {
            Ok(ServiceControl::Stop) => break,
            Err(_) => break, // handler dropped
            Ok(_) => {}
        }
    }
    event_handler
        .set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: ServiceState::StopPending,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: std::time::Duration::from_secs(20),
            process_id: None,
        })
        .ok();
    let _ = shutdown_tx.send(true);

    // Give the server up to 15s to shut down gracefully.
    let _ = rt.block_on(async {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(15), server).await;
    });

    event_handler
        .set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: ServiceState::Stopped,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: std::time::Duration::default(),
            process_id: None,
        })
        .ok();
    Ok(())
}

/// Register kenji as an auto-start Windows service. Must run elevated.
/// Default runs as LocalSystem; pass `account`/`password` to use a
/// (recommended) standard user account with "Log on as a service" right.
pub fn install(account: Option<(String, String)>) -> Result<()> {
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )
    .context("サービスマネージャへの接続に失敗（管理者権限が必要です）")?;
    let exe = std::env::current_exe().context("実行ファイルのパス取得に失敗")?;
    let info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(SERVICE_DISPLAY),
        service_type: SERVICE_TYPE,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe,
        launch_arguments: vec!["service".into(), "run".into()],
        dependencies: vec![],
        account_name: account.as_ref().map(|a| OsString::from(&a.0)),
        account_password: account.as_ref().map(|a| OsString::from(&a.1)),
    };
    manager
        .create_service(&info, ServiceAccess::QUERY_STATUS)
        .context("サービスの登録に失敗")?;
    if account.is_none() {
        println!("注意: LocalSystem (SYSTEM) として登録されました。");
        println!("      遠隔操作の権限を絞るため、標準ユーザーアカウントでの実行を推奨します:");
        println!("      kenji service install --account DOMAIN\\user --password <pw>");
    }
    println!("サービス 'kenji' を登録しました。次のコマンドで開始します:");
    println!("  sc start kenji");
    Ok(())
}

/// Remove the service registration. Must run elevated.
pub fn uninstall() -> Result<()> {
    let manager =
        ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::all()).context("サービスマネージャへの接続に失敗（管理者権限が必要です）")?;
    let service = manager
        .open_service(SERVICE_NAME, ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS)
        .context("サービス 'kenji' が見つかりません")?;
    service.delete().context("サービスの削除に失敗")?;
    println!("サービス 'kenji' を削除しました（実行中の場合は先に `sc stop kenji` してください）");
    Ok(())
}
