//! 应用商店 API（浏览商店）

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};

use pnos::response::ApiResponse;

use crate::config::AppState;

pub fn routes(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/store/sources", get(list_sources))
        .route("/store/sources/{id}/refresh", post(refresh_source))
        .route("/store/apps", get(list_apps))
        .route("/store/apps/{id}", get(app_detail))
        .with_state(state)
}

async fn list_sources(
    State(state): State<Arc<AppState>>,
) -> Json<ApiResponse<Vec<serde_json::Value>>> {
    let sources = state.store_service.list_sources();
    Json(ApiResponse::success(sources))
}

async fn refresh_source(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<ApiResponse<()>> {
    match state.store_service.refresh_source(&id).await {
        Ok(_) => Json(ApiResponse::success_with_msg((), "刷新成功")),
        Err(e) => Json(ApiResponse::error(&e)),
    }
}

async fn list_apps(
    State(state): State<Arc<AppState>>,
) -> Json<ApiResponse<Vec<serde_json::Value>>> {
    let apps = state.store_service.list_apps().await;
    Json(ApiResponse::success(apps))
}

async fn app_detail(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<ApiResponse<serde_json::Value>> {
    match state.store_service.get_app(&id).await {
        Some(app) => Json(ApiResponse::success(app)),
        None => Json(ApiResponse::error(&pnos::error::PnosError::from(
            pnos::error::ErrorCode::AppNotFound,
        ))),
    }
}
