//! 应用状态与配置

use std::sync::Arc;

use reqwest::Client;

use crate::agent::AgentManager;
use crate::app_manager::AppManager;
use crate::install::InstallService;
use crate::metrics::Metrics;
use crate::registry::Registry;
use crate::service::monitor::MonitorService;
use crate::service::store::StoreService;

/// 全局应用状态
#[derive(Clone)]
pub struct AppState {
    pub config: pnos::config::PnosConfig,
    pub metrics: Arc<Metrics>,
    pub registry: Registry,
    pub app_manager: Arc<AppManager>,
    pub store_service: Arc<StoreService>,
    pub monitor_service: Arc<MonitorService>,
    pub agent_manager: Arc<AgentManager>,
    pub install_service: Arc<InstallService>,
    pub http_client: Client,
}

impl AppState {
    pub async fn new(config: pnos::config::PnosConfig) -> anyhow::Result<Self> {
        // 性能埋点全局单例：必须最先初始化，其后启动的后台任务才能 mark_task
        let metrics = Metrics::new();
        crate::metrics::init_global(metrics.clone());
        // 事件总线全局单例：供各模块发布生命周期事件（WS 端点消费）
        crate::ws::init();

        let registry = Registry::new(config.heartbeat_timeout);
        registry.start_heartbeat_checker();

        let http_client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        let runtime_url = format!("http://127.0.0.1:{}", config.port);
        let agent_manager = Arc::new(AgentManager::new(runtime_url));

        let apps_dir = std::path::PathBuf::from(&config.app_data_dir);
        let data_dir = std::path::PathBuf::from(&config.data_dir);
        let install_service = Arc::new(InstallService::new(
            apps_dir,
            data_dir,
            agent_manager.clone(),
        ));

        let monitor_service = Arc::new(MonitorService::new());
        // 启动后台指标采集（快照模式，避免 /system/stats 阻塞 worker）
        monitor_service.start();

        Ok(AppState {
            config: config.clone(),
            metrics,
            registry,
            app_manager: Arc::new(AppManager::new(config.clone())),
            store_service: Arc::new(StoreService::new(config.clone())),
            monitor_service,
            agent_manager,
            install_service,
            http_client,
        })
    }
}
