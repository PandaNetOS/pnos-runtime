//! 应用状态与配置

use std::sync::Arc;

use reqwest::Client;

use crate::app_manager::AppManager;
use crate::registry::Registry;
use crate::service::monitor::MonitorService;
use crate::service::store::StoreService;

/// 全局应用状态
#[derive(Clone)]
pub struct AppState {
    pub config: pnos::config::PnosConfig,
    pub registry: Registry,
    pub app_manager: Arc<AppManager>,
    pub store_service: Arc<StoreService>,
    pub monitor_service: Arc<MonitorService>,
    pub http_client: Client,
}

impl AppState {
    pub async fn new(config: pnos::config::PnosConfig) -> anyhow::Result<Self> {
        let registry = Registry::new(config.heartbeat_timeout);
        registry.start_heartbeat_checker();

        let http_client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        Ok(AppState {
            config: config.clone(),
            registry,
            app_manager: Arc::new(AppManager::new(config.clone())),
            store_service: Arc::new(StoreService::new(config.clone())),
            monitor_service: Arc::new(MonitorService::new()),
            http_client,
        })
    }
}
