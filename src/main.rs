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
    let config = pnos::config::PnosConfig::load()?;
    info!(
        "配置加载完成: 端口={}, 数据目录={}",
        config.port, config.data_dir
    );

    // 初始化应用状态
    let state = AppState::new(config).await?;
    let state = Arc::new(state);

    // 刷新商店缓存
    if let Err(e) = state.store_service.refresh_all().await {
        tracing::warn!("商店刷新失败（将使用缓存）: {}", e);
    }

    // 构建路由
    let app = Router::new()
        // API 路由
        .nest("/api/v1", api::routes(state.clone()))
        // 反向代理：/app/{id}/*
        .route("/app/{id}", axum::routing::any(proxy::proxy_handler))
        .route("/app/{id}/{*path}", axum::routing::any(proxy::proxy_handler))
        // 健康检查
        .route("/health", get(health))
        // 静态文件（pnos-web）
        .fallback_service(ServeDir::new("/var/www/pnos-web"))
        .layer(CorsLayer::permissive())
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state);

    let port = state.config.port;
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("pnos-runtime 监听: http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn health() -> &'static str {
    "ok"
}
