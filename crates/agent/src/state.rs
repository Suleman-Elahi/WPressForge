use crate::config::Config;
use crate::store::Store;
use std::sync::Arc;
use tokio::sync::Semaphore;

#[derive(Clone)]
pub struct AgentState {
    pub config: Arc<Config>,
    pub store: Store,
    /// Privileged operations are serialised per node: two concurrent Nginx
    /// reloads or useradd calls would race.
    pub lock: Arc<Semaphore>,
}

impl AgentState {
    pub fn new(config: Config, store: Store) -> Self {
        Self {
            config: Arc::new(config),
            store,
            lock: Arc::new(Semaphore::new(1)),
        }
    }
}
