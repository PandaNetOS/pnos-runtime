//! 应用状态与配置

use std::sync::Arc;
use std::time::Duration;

use reqwest::Client;

use crate::agent::AgentManager;
use crate::app_manager::AppManager;
use crate::install::InstallService;
use crate::metrics::Metrics;
use crate::registry::Registry;
use crate::service::monitor::MonitorService;
use crate::service::store::StoreService;

/// 心跳超时巡检间隔（秒）默认值
pub const DEFAULT_HEARTBEAT_CHECK_INTERVAL_SECS: u64 = 10;

/// Agent 巡检间隔（秒）默认值
pub const DEFAULT_AGENT_MONITOR_INTERVAL_SECS: u64 = 5;

/// 收到关闭信号后，留给后台循环收尾的时间（毫秒）
pub const SHUTDOWN_GRACE_MS: u64 = 300;

/// HTTP 客户端（对外请求）默认超时（秒）
pub const DEFAULT_HTTP_TIMEOUT_SECS: u64 = 30;

/// 进程指标快照默认缓存时间（秒）
pub const DEFAULT_METRICS_CACHE_TTL_SECS: u64 = 1;

/// 管理面 API 默认超时（秒）
pub const DEFAULT_API_TIMEOUT_SECS: u64 = 30;

/// 反向代理默认超时（秒，容忍大文件转发）
pub const DEFAULT_PROXY_TIMEOUT_SECS: u64 = 120;

/// Agent 健康检查默认超时（秒）
pub const DEFAULT_AGENT_HEALTH_TIMEOUT_SECS: u64 = 3;

/// 安装/升级时连接超时（秒）
pub const DEFAULT_INSTALL_CONNECT_TIMEOUT_SECS: u64 = 30;

/// 安装/升级整体超时（秒，含下载与解压）
pub const DEFAULT_INSTALL_TIMEOUT_SECS: u64 = 600;

/// 启动应用后等待其就绪的轮询间隔（秒）
pub const DEFAULT_INSTALL_SETTLE_INTERVAL_SECS: u64 = 2;

/// 运行时可调参数：进程启动时读取一次环境变量，避免在请求路径上反复访问环境。
///
/// 每一项都对应代码里的 `DEFAULT_*` 默认值，未配置环境变量时行为与默认值一致。
#[derive(Debug, Clone)]
pub struct Settings {
    pub http_timeout: Duration,
    pub metrics_cache_ttl: Duration,
    pub api_timeout: Duration,
    pub proxy_timeout: Duration,
    pub agent_health_timeout: Duration,
    pub install_connect_timeout: Duration,
    pub install_timeout: Duration,
    pub install_settle_interval: Duration,
    pub heartbeat_check_interval: Duration,
    pub agent_monitor_interval: Duration,
}

impl Settings {
    fn from_env() -> Self {
        Self {
            http_timeout: secs_from_env("PNOS_HTTP_TIMEOUT_SECS", DEFAULT_HTTP_TIMEOUT_SECS),
            metrics_cache_ttl: secs_from_env(
                "PNOS_METRICS_CACHE_TTL_SECS",
                DEFAULT_METRICS_CACHE_TTL_SECS,
            ),
            api_timeout: secs_from_env("PNOS_API_TIMEOUT_SECS", DEFAULT_API_TIMEOUT_SECS),
            proxy_timeout: secs_from_env("PNOS_PROXY_TIMEOUT_SECS", DEFAULT_PROXY_TIMEOUT_SECS),
            agent_health_timeout: secs_from_env(
                "PNOS_AGENT_HEALTH_TIMEOUT_SECS",
                DEFAULT_AGENT_HEALTH_TIMEOUT_SECS,
            ),
            install_connect_timeout: secs_from_env(
                "PNOS_INSTALL_CONNECT_TIMEOUT_SECS",
                DEFAULT_INSTALL_CONNECT_TIMEOUT_SECS,
            ),
            install_timeout: secs_from_env(
                "PNOS_INSTALL_TIMEOUT_SECS",
                DEFAULT_INSTALL_TIMEOUT_SECS,
            ),
            install_settle_interval: secs_from_env(
                "PNOS_INSTALL_SETTLE_INTERVAL_SECS",
                DEFAULT_INSTALL_SETTLE_INTERVAL_SECS,
            ),
            heartbeat_check_interval: secs_from_env(
                "PNOS_HEARTBEAT_CHECK_INTERVAL_SECS",
                DEFAULT_HEARTBEAT_CHECK_INTERVAL_SECS,
            ),
            agent_monitor_interval: secs_from_env(
                "PNOS_AGENT_MONITOR_INTERVAL_SECS",
                DEFAULT_AGENT_MONITOR_INTERVAL_SECS,
            ),
        }
    }
}

static SETTINGS: std::sync::OnceLock<Settings> = std::sync::OnceLock::new();

/// 全局运行时参数（首次访问时从环境变量加载）
pub fn settings() -> &'static Settings {
    SETTINGS.get_or_init(Settings::from_env)
}

/// 读取以秒为单位的配置：环境变量优先，取不到或非法时用默认值
fn secs_from_env(key: &str, default_secs: u64) -> Duration {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(default_secs))
}

/// 心跳超时巡检周期（`PNOS_HEARTBEAT_CHECK_INTERVAL_SECS` 可覆盖）
pub fn heartbeat_check_interval() -> Duration {
    settings().heartbeat_check_interval
}

/// Agent 巡检周期（`PNOS_AGENT_MONITOR_INTERVAL_SECS` 可覆盖）
pub fn agent_monitor_interval() -> Duration {
    settings().agent_monitor_interval
}

/// 启动参数：`--config <path>` / `--work-dir <dir>`
///
/// 优先级：`--config` > `--work-dir` > 环境变量（`PNOS_CONFIG` / `PNOS_DATA_DIR`）> 默认值。
/// 仅解析这两个参数，其余位置参数与未知参数忽略，保持与既有部署脚本兼容。
#[derive(Debug, Default, Clone)]
pub struct CliArgs {
    pub config: Option<String>,
    pub work_dir: Option<String>,
}

impl CliArgs {
    /// 从进程参数解析（跳过 argv[0]）
    pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Self {
        let mut out = CliArgs::default();
        let mut iter = args.into_iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--config" => out.config = iter.next(),
                "--work-dir" => out.work_dir = iter.next(),
                other => {
                    if let Some(v) = other.strip_prefix("--config=") {
                        out.config = Some(v.to_string());
                    } else if let Some(v) = other.strip_prefix("--work-dir=") {
                        out.work_dir = Some(v.to_string());
                    }
                }
            }
        }
        out
    }

    /// 当前进程的启动参数
    pub fn from_env_args() -> Self {
        Self::parse(std::env::args().skip(1))
    }
}

/// 创建运行时目录骨架（config / data / logs）。
///
/// 启动时调用一次；目录已存在时是空操作。
pub fn ensure_dirs(config: &pnos::config::PnosConfig) -> std::io::Result<()> {
    let root = crate::workdir::WorkDir::new(&config.data_dir);
    root.ensure_dirs()
}

/// CORS 允许来源：`["*"]` 表示允许任意来源。
///
/// 与 `main.rs` 挂载的 `CorsLayer::permissive()` 是同一份策略；
/// Web UI 的「设置 → 网络」经 `GET /api/v1/system/config` 展示此值。
pub const CORS_ALLOW_ORIGINS: &[&str] = &["*"];

/// 全局应用状态
#[derive(Clone)]
pub struct AppState {
    pub config: pnos::config::PnosConfig,
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

        let http_client = Client::builder().timeout(settings().http_timeout).build()?;

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
