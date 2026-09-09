//! Agent 管理 API
//!
//! 提供 Agent 的启动、停止、重启、状态查询等接口。

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};

use pnos::response::ApiResponse;

use crate::config::AppState;

pub fn routes(state: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
        .route("/agents", get(list_agents))
        .route("/agents/{id}", get(get_agent))
        .route("/agents/{id}/start", post(start_agent))
        .route("/agents/{id}/stop", post(stop_agent))
        .route("/agents/{id}/restart", post(restart_agent))
        .with_state(state)
}

/// 列出所有 Agent
async fn list_agents(
    State(state): State<Arc<AppState>>,
) -> Json<ApiResponse<Vec<crate::agent::AgentInfo>>> {
    let agents = state.agent_manager.list().await;
    Json(ApiResponse::success(agents))
}

/// 获取 Agent 详情
async fn get_agent(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<ApiResponse<crate::agent::AgentInfo>> {
    match state.agent_manager.get(&id).await {
        Some(agent) => Json(ApiResponse::success(agent)),
        None => Json(ApiResponse::error(&pnos::error::PnosError::from(
            pnos::error::ErrorCode::AppNotFound,
        ))),
    }
}

/// 启动 Agent
async fn start_agent(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<ApiResponse<()>> {
    match state.agent_manager.start(&id).await {
        Ok(_) => Json(ApiResponse::success_with_msg((), "启动成功")),
        Err(e) => Json(ApiResponse::error(&pnos::error::PnosError::External(
            e.to_string(),
        ))),
    }
}

/// 停止 Agent
async fn stop_agent(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<ApiResponse<()>> {
    match state.agent_manager.stop(&id).await {
        Ok(_) => Json(ApiResponse::success_with_msg((), "停止成功")),
        Err(e) => Json(ApiResponse::error(&pnos::error::PnosError::External(
            e.to_string(),
        ))),
    }
}

/// 重启 Agent
async fn restart_agent(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<ApiResponse<()>> {
    match state.agent_manager.restart(&id).await {
        Ok(_) => Json(ApiResponse::success_with_msg((), "重启成功")),
        Err(e) => Json(ApiResponse::error(&pnos::error::PnosError::External(
            e.to_string(),
        ))),
    }
}
