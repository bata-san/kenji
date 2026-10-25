use std::sync::{Arc, RwLock};

use crate::audit::Audit;
use crate::auth::AuthStore;
use crate::config::Config;
use crate::jobs::JobManager;
use crate::metrics::MetricsCache;
use crate::pty::TerminalManager;
use crate::tunnel::TunnelSupervisor;
use crate::update::UpdateManager;

/// Shared application state handed to every handler.
pub struct AppState {
    pub config: RwLock<Config>,
    pub auth: Arc<AuthStore>,
    pub audit: Arc<Audit>,
    pub jobs: Arc<JobManager>,
    pub metrics: Arc<MetricsCache>,
    pub tunnel: Arc<TunnelSupervisor>,
    pub update: Arc<UpdateManager>,
    pub terminals: TerminalManager,
    pub setup_done: bool,
    pub version: &'static str,
}

pub type SharedState = Arc<AppState>;

pub fn role_str(r: &crate::auth::Role) -> &'static str {
    match r {
        crate::auth::Role::Viewer => "viewer",
        crate::auth::Role::Operator => "operator",
        crate::auth::Role::Admin => "admin",
    }
}
