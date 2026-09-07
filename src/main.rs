//! pnos-runtime 入口

mod api;
mod app_manager;
mod config;
mod proxy;
mod registry;
mod service;

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
    // 商店源默认走 jsDelivr 国内 CDN（pnos-spec 默认的 raw.githubusercontent.com 国内不通）
    config.default_store_url =
        "https://cdn.jsdelivr.net/gh/PandaNetOS/pnos-store@main/index.json".to_string();
    // 应用安装目录跟随 data_dir（pnos-spec 默认 /pnos/data/apps 在 Windows 上会解析到盘根目录）
    config.app_data_dir = format!("{}/apps", config.data_dir);
    info!(
        "配置加载完成: 端口={}, 数据目录={}, 应用目录={}, 商店源={}",
        config.port, config.data_dir, config.app_data_dir, config.default_store_url
    );

    // 初始化应用状态
    let state = AppState::new(config).await?;
    let state = Arc::new(state);

    // 商店同步依赖外部网络，不能阻塞控制面监听。服务先就绪，目录在后台刷新。
    let store_service = state.store_service.clone();
    tokio::spawn(async move {
        if let Err(e) = store_service.refresh_all().await {
            tracing::warn!("商店刷新失败（将使用缓存）: {}", e);
        }
    });

    let port = state.config.port;

    // 构建路由
    let app = Router::new()
        // API 路由
        .nest("/api/v1", api::routes(state.clone()))
        // 反向代理：/app/{id}/*
        // axum 0.7 使用 `:param` 和 `*catch_all` 路由语法。
        .route("/app/:id", axum::routing::any(proxy::proxy_root_handler))
        .route("/app/:id/*path", axum::routing::any(proxy::proxy_handler))
        // 健康检查
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
