//! pnos-runtime 入口

mod agent;
mod api;
mod app_manager;
mod config;
mod download;
mod install;
mod proxy;
mod registry;
mod service;
mod metrics;
mod rate_limit;
mod ws;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{routing::get, Router};
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tracing::info;

use crate::config::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 初始化日志
    pnos::logging::init_logging_pretty("info");
    info!("pnos-runtime 启动中...");

    // 加载配置
    let mut config = pnos::config::PnosConfig::load()?;
    // 运维可调：环境变量覆盖端口与数据目录（测试拉起独立实例 / 容器部署）
    if let Ok(p) = std::env::var("PNOS_PORT") {
        if let Ok(p) = p.parse::<u16>() {
            config.port = p;
        }
    }
    if let Ok(d) = std::env::var("PNOS_DATA_DIR") {
        config.data_dir = d.clone();
    }
    // 商店源默认走 ghfast.top 国内 CDN（缓存5分钟，更新快；pnos-spec 默认的 raw.githubusercontent.com 国内不通）
    config.default_store_url =
        "https://ghfast.top/https://raw.githubusercontent.com/PandaNetOS/pnos-store/main/index.json".to_string();
    // 应用安装目录跟随 data_dir（pnos-spec 默认 /pnos/data/apps 在 Windows 上会解析到盘根目录）
    config.app_data_dir = format!("{}/apps", config.data_dir);
    info!(
        "配置加载完成: 端口={}, 数据目录={}, 应用目录={}, 商店源={}",
        config.port, config.data_dir, config.app_data_dir, config.default_store_url
    );

    // 初始化应用状态
    let state = AppState::new(config).await?;
    let state = Arc::new(state);

    // 启动 Agent 监控循环（崩溃检测 + 健康检查 + 自动重启）
    state.agent_manager.clone().start_monitor(state.clone());

    // 商店同步依赖外部网络，不能阻塞控制面监听。服务先就绪，目录在后台刷新。
    let store_service = state.store_service.clone();
    tokio::spawn(async move {
        let start = std::time::Instant::now();
        if let Err(e) = store_service.refresh_all().await {
            tracing::warn!("商店刷新失败（将使用缓存）: {}", e);
        } else {
            crate::metrics::global()
                .map(|m| m.set_store_refresh_ms(start.elapsed().as_millis() as u64));
        }
        crate::metrics::global().map(|m| m.mark_task("store_refresh"));
    });

    let port = state.config.port;

    // 过载保护初始化（令牌桶 + 并发信号量）
    rate_limit::init(rate_limit::BURST, rate_limit::RATE);

    // API 路由层：埋点 + 限流 + 超时(30s) + 并发上限(1024) + body 限制(16MB)
    let api_routes = api::routes(state.clone())
        .layer(axum::middleware::from_fn(metrics::metrics_middleware))
        .layer(axum::middleware::from_fn(rate_limit::rate_limit_middleware))
        .layer(axum::middleware::from_fn(rate_limit::timeout_middleware))
        .layer(axum::middleware::from_fn(rate_limit::concurrency_middleware))
        .layer(axum::extract::DefaultBodyLimit::max(16 * 1024 * 1024));

    // 反向代理体量大：单独放宽超时（120s），并解除 axum 默认 2MB body 限制
    // （全量缓冲的 OOM 风险已在性能 SLO 文档 P9 标注，待后续流式转发修复）。
    // 构建路由
    let app = Router::new()
        .nest("/api/v1", api_routes)
        .route("/app/:id", axum::routing::any(proxy::proxy_root_handler))
        .route("/app/:id/*path", axum::routing::any(proxy::proxy_handler))
        .layer(axum::middleware::from_fn(rate_limit::proxy_timeout_middleware))
        .layer(axum::extract::DefaultBodyLimit::disable())
        // WebSocket 事件端点：刻意放在代理超时层之外，避免长连接被 30s/120s 超时强断
        .route("/api/v1/ws", get(crate::ws::ws_handler))
        // 健康检查（liveness）：保持无超时，便于探针快速返回
        .route("/health", get(health))
        // 静态文件（pnos-web）
        .fallback_service(ServeDir::new("/var/www/pnos-web"))
        .layer(CorsLayer::permissive())
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("pnos-runtime 监听: http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn health() -> &'static str {
    "ok"
}
