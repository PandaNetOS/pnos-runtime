//! 应用管理 API：注册、心跳、发现、安装、启动、停止

use std::sync::Arc;

use axum::{extract::State, routing::{get, post}, Json, Router};
use pnos::registry::{
    AppDiscoverResponse, AppRegisterRequest, AppRegisterResponse, HeartbeatRequest,
};
use pnos::response::ApiResponse;

use crate::config::AppState;

pub fn routes(_state: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
        .route("/apps/register", post(register))
        .route("/apps/unregister", post(unregister))
        .route("/apps/heartbeat", post(heartbeat))
        .route("/apps", get(list_apps))
        .route("/apps/:id", get(app_detail))
        .route("/apps/:id/discover", get(discover))
        .route("/apps/:id/install", post(install_app))
        .route("/apps/:id/start", post(start_app))
        .route("/apps/:id/stop", post(stop_app))
}

/// 注册应用
async fn register(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AppRegisterRequest>,
) -> Json<ApiResponse<AppRegisterResponse>> {
    let resp = state.registry.register(req).await;
    Json(ApiResponse::success(resp))
}

/// 注销应用
async fn unregister(
    State(state): State<Arc<AppState>>,
    Json(req): Json<serde_json::Value>,
) -> Json<ApiResponse<bool>> {
    let app_id = req["id"].as_str().unwrap_or("");
    let ok = state.registry.unregister(app_id).await;
    Json(ApiResponse::success(ok))
}

/// 心跳
async fn heartbeat(
    State(state): State<Arc<AppState>>,
    Json(req): Json<HeartbeatRequest>,
) -> Json<ApiResponse<bool>> {
    let ok = state.registry.heartbeat(&req.id, req.status).await;
    Json(ApiResponse::success(ok))
}

/// 列出所有应用
async fn list_apps(State(state): State<Arc<AppState>>) -> Json<ApiResponse<Vec<pnos::registry::AppInfo>>> {
    let apps = state.registry.list().await;
    Json(ApiResponse::success(apps))
}

/// 应用详情
async fn app_detail(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Json<ApiResponse<pnos::registry::AppInfo>> {
    match state.registry.get(&id).await {
        Some(app) => Json(ApiResponse::success(app)),
        None => Json(ApiResponse::error_code(
            pnos::error::ErrorCode::AppNotFound,
            "应用不存在",
        )),
    }
}

/// 发现应用（获取地址）
async fn discover(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Json<ApiResponse<AppDiscoverResponse>> {
    match state.registry.get(&id).await {
        Some(app) => Json(ApiResponse::success(AppDiscoverResponse::from_info(&app))),
        None => Json(ApiResponse::error_code(
            pnos::error::ErrorCode::AppNotFound,
            "应用不存在",
        )),
    }
}

/// 安装应用
async fn install_app(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Json<ApiResponse<String>> {
    // 从商店获取应用清单
    let manifest = match state.store_service.get_app_manifest(&id).await {
        Some(m) => m,
        None => {
            return Json(ApiResponse::error_code(
                pnos::error::ErrorCode::StoreAppNotFound,
                "商店中找不到该应用",
            ));
        }
    };

    match state.app_manager.install(&manifest).await {
        Ok(_) => Json(ApiResponse::success_with_msg(
            "installed".to_string(),
            "应用安装成功",
        )),
        Err(e) => Json(ApiResponse::error_code(
            pnos::error::ErrorCode::AppStartFailed,
            e.to_string(),
        )),
    }
}

/// 启动应用
async fn start_app(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Json<ApiResponse<String>> {
    let manifest = match state.store_service.get_app_manifest(&id).await {
        Some(m) => m,
        None => {
            return Json(ApiResponse::error_code(
                pnos::error::ErrorCode::AppNotInstalled,
                "应用未安装",
            ));
        }
    };

    match state.app_manager.start(&manifest).await {
        Ok(_) => Json(ApiResponse::success_with_msg(
            "started".to_string(),
            "应用启动成功",
        )),
        Err(e) => Json(ApiResponse::error_code(
            pnos::error::ErrorCode::AppStartFailed,
            e.to_string(),
        )),
    }
}

/// 停止应用
async fn stop_app(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Json<ApiResponse<String>> {
    match state.app_manager.stop(&id).await {
        Ok(_) => Json(ApiResponse::success_with_msg(
            "stopped".to_string(),
            "应用已停止",
        )),
        Err(e) => Json(ApiResponse::error_code(
            pnos::error::ErrorCode::AppStopFailed,
            e.to_string(),
        )),
    }
}
