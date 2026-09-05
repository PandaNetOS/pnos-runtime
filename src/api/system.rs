//! 系统 API

use std::sync::Arc;

use axum::{extract::State, routing::get, Json, Router};

use pnos::response::ApiResponse;
use pnos::system::{SystemInfo, SystemStats};

use crate::config::AppState;

pub fn routes(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/system/info", get(system_info))
        .route("/system/stats", get(system_stats))
        .with_state(state)
}

async fn system_info(State(state): State<Arc<AppState>>) -> Json<ApiResponse<SystemInfo>> {
    let info = state.monitor_service.get_system_info();
    Json(ApiResponse::success(info))
}

async fn system_stats(State(state): State<Arc<AppState>>) -> Json<ApiResponse<SystemStats>> {
    let stats = state.monitor_service.get_stats();
    Json(ApiResponse::success(stats))
}
