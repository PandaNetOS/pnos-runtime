//! 系统 API

use std::sync::Arc;

use axum::{extract::State, routing::get, Json, Router};
use serde::Serialize;

use pnos::response::ApiResponse;
use pnos::system::{SystemInfo, SystemStats};

use crate::config::AppState;

pub fn routes(_state: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
        .route("/system/info", get(system_info))
        .route("/system/stats", get(system_stats))
        .route("/system/config", get(system_config))
        .route("/metrics", get(crate::metrics::get_metrics))
}

async fn system_info(State(state): State<Arc<AppState>>) -> Json<ApiResponse<SystemInfo>> {
    let info = state.monitor_service.get_system_info();
    Json(ApiResponse::success(info))
}

async fn system_stats(State(state): State<Arc<AppState>>) -> Json<ApiResponse<SystemStats>> {
    let stats = state.monitor_service.get_stats().await;
    Json(ApiResponse::success(stats))
}

/// 运行时参数（只读视图）
///
/// 供 Web UI 展示**真实生效值**，避免前端把端口 / 反代前缀 / CORS 写死。
/// 只暴露可公开的展示字段，不含日志级别、令牌等敏感配置。
#[derive(Debug, Clone, Serialize)]
pub struct RuntimeSettingsView {
    /// HTTP 服务监听端口（配置文件或 PNOS_PORT 环境变量）
    pub port: u16,
    /// 应用反向代理前缀（与 main.rs 注册的反代路由同源）
    pub proxy_prefix: String,
    /// CORS 允许来源，`["*"]` 表示允许任意来源
    pub cors_origins: Vec<String>,
}

async fn system_config(
    State(state): State<Arc<AppState>>,
) -> Json<ApiResponse<RuntimeSettingsView>> {
    Json(ApiResponse::success(RuntimeSettingsView {
        port: state.config.port,
        proxy_prefix: pnos::protocol::APP_PROXY_PREFIX.to_string(),
        cors_origins: crate::config::CORS_ALLOW_ORIGINS
            .iter()
            .map(|origin| origin.to_string())
            .collect(),
    }))
}
