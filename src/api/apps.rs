//! 组件管理 API：注册、心跳、发现、安装、启动、停止
//!
//! 统一 /components/* 为主路径，同时保留 /apps/* 作为兼容路径。

use std::sync::Arc;

use axum::{extract::State, routing::{get, post}, Json, Router};
use pnos::component::ComponentType;
use pnos::discovery::ComponentDiscoverResponse;
use pnos::registry::{
    ComponentRegisterRequest, ComponentRegisterResponse, HeartbeatRequest,
};
use pnos::response::ApiResponse;

use crate::config::AppState;

pub fn routes(_state: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
        // ===== 统一组件路径（主路径） =====
        .route("/components/register", post(register))
        .route("/components/unregister", post(unregister))
        .route("/components/heartbeat", post(heartbeat))
        .route("/components", get(list_components))
        .route("/components/:id", get(component_detail))
        .route("/components/:id/discover", get(discover))
        // ===== 兼容路径（旧版 /apps/*，已废弃） =====
        .route("/apps/register", post(register))
        .route("/apps/unregister", post(unregister))
        .route("/apps/heartbeat", post(heartbeat))
        .route("/apps", get(list_components))
        .route("/apps/:id", get(component_detail))
        .route("/apps/:id/discover", get(discover))
        // ===== 应用管理（商店安装的应用） =====
        .route("/apps/:id/install", post(install_app))
        .route("/apps/:id/start", post(start_app))
        .route("/apps/:id/stop", post(stop_app))
}

/// 注册组件
async fn register(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ComponentRegisterRequest>,
) -> Json<ApiResponse<ComponentRegisterResponse>> {
    let resp = state.registry.register(req).await;
    Json(ApiResponse::success(resp))
}

/// 注销组件
async fn unregister(
    State(state): State<Arc<AppState>>,
    Json(req): Json<serde_json::Value>,
) -> Json<ApiResponse<bool>> {
    let component_id = req["id"].as_str().unwrap_or("");
    let ok = state.registry.unregister(component_id).await;
    Json(ApiResponse::success(ok))
}

/// 心跳（统一组件心跳）
async fn heartbeat(
    State(state): State<Arc<AppState>>,
    Json(req): Json<HeartbeatRequest>,
) -> Json<ApiResponse<bool>> {
    let ok = state
        .registry
        .heartbeat(&req.id, req.status, req.load, req.active_tasks, req.bytes_downloaded)
        .await;
    Json(ApiResponse::success(ok))
}

/// 列出所有组件
async fn list_components(
    State(state): State<Arc<AppState>>,
) -> Json<ApiResponse<Vec<pnos::registry::ComponentInfo>>> {
    let components = state.registry.list().await;
    Json(ApiResponse::success(components))
}

/// 组件详情
async fn component_detail(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Json<ApiResponse<pnos::registry::ComponentInfo>> {
    match state.registry.get(&id).await {
        Some(component) => Json(ApiResponse::success(component)),
        None => Json(ApiResponse::error_code(
            pnos::error::ErrorCode::ComponentNotRegistered,
            "组件不存在",
        )),
    }
}

/// 发现组件（获取地址）
async fn discover(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Json<ApiResponse<ComponentDiscoverResponse>> {
    match state.registry.get(&id).await {
        Some(component) => {
            let resp = ComponentDiscoverResponse {
                id: component.id,
                name: component.name,
                version: component.version,
                component_type: component.component_type,
                address: component.address,
                port: component.port,
                capabilities: component.capabilities,
                region: component.region,
                status: component.status,
                base_url: component.base_url,
                serve_url: component.serve_url,
            };
            Json(ApiResponse::success(resp))
        }
        None => Json(ApiResponse::error_code(
            pnos::error::ErrorCode::ComponentNotRegistered,
            "组件不存在",
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

// 防止未使用警告
#[allow(dead_code)]
fn _unused_type_marker() -> ComponentType {
    ComponentType::App
}
