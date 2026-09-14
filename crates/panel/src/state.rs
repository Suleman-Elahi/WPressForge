use crate::agent::AgentClient;
use crate::config::Config;
use crate::csrf::CsrfKey;
use crate::db::Db;
use crate::secrets::SecretBox;
use std::sync::Arc;
use tokio::sync::Notify;

/// Shared, cheap-to-clone application state.
#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub config: Arc<Config>,
    pub agent: AgentClient,
    pub csrf: Arc<CsrfKey>,
    pub secrets: Arc<SecretBox>,
    /// Woken whenever a job is enqueued so workers react without polling delay.
    pub job_signal: Arc<Notify>,
    pub started_at: std::time::Instant,
}

impl AppState {
    pub fn new(db: Db, config: Config, agent: AgentClient, csrf: CsrfKey, secrets: SecretBox) -> Self {
        Self {
            db,
            config: Arc::new(config),
            agent,
            csrf: Arc::new(csrf),
            secrets: Arc::new(secrets),
            job_signal: Arc::new(Notify::new()),
            started_at: std::time::Instant::now(),
        }
    }

    pub fn notify_jobs(&self) {
        self.job_signal.notify_waiters();
    }
}
