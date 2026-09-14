use crate::config::Config;
use crate::ops::webserver::WebServer;
use crate::store::Store;
use std::sync::Arc;
use tokio::sync::Semaphore;

#[derive(Clone)]
pub struct AgentState {
    pub config: Arc<Config>,
    pub store: Store,
    /// The host's web server, with its detected capabilities.
    pub web: WebServer,
    /// Privileged operations are serialised per node: two concurrent Nginx
    /// reloads or useradd calls would race.
    pub lock: Arc<Semaphore>,
}

impl AgentState {
    /// Probes the host web server, so this is async and runs once at startup.
    pub async fn new(config: Config, store: Store) -> Self {
        let config = Arc::new(config);
        let web = WebServer::detect(config.clone()).await;

        Self {
            config,
            store,
            web,
            lock: Arc::new(Semaphore::new(1)),
        }
    }
}
