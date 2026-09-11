//! 应用商店 API
//!
//! 提供商店浏览、应用安装、升级、卸载等接口。

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    routing::{delete, get, post},
    Json, Router,
};

use pnos::response::ApiResponse;

use crate::config::AppState;
use crate::install::PackageManifest;

pub fn routes(state: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
        // 商店浏览
        .route("/store/sources", get(list_sources))
        .route("/store/sources/:id/refresh", post(refresh_source))
        .route("/store/apps", get(list_apps))
        .route("/store/apps/:id", get(app_detail))
        // 已安装应用管理
        .route("/installed", get(list_installed))
        .route("/installed/:id", get(installed_detail))
        .route("/installed/:id/install", post(install_app))
        .route("/installed/:id/upgrade", post(upgrade_app))
        .route("/installed/:id/uninstall", delete(uninstall_app))
        .route("/installed/:id/progress", get(install_progress))
        .route("/installed/:id/start", post(install_start))
        .route("/installed/:id/stop", post(install_stop))
        .with_state(state)
}

// ---- 商店浏览 ----

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

// ---- 已安装应用管理 ----

/// 列出已安装应用
async fn list_installed(
    State(state): State<Arc<AppState>>,
) -> Json<ApiResponse<Vec<crate::install::InstalledAppInfo>>> {
    let apps = state.install_service.list_installed().await;
    Json(ApiResponse::success(apps))
}

/// 获取已安装应用详情
async fn installed_detail(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<ApiResponse<serde_json::Value>> {
    match state.install_service.get_installed(&id).await {
        Some(app) => Json(ApiResponse::success(serde_json::json!({
            "id": app.id,
            "version": app.version,
            "active_color": app.active_color.as_str(),
            "active_dir": app.active_dir,
            "data_dir": app.data_dir,
            "manifest": app.manifest,
        }))),
        None => Json(ApiResponse::error(&pnos::error::PnosError::from(
            pnos::error::ErrorCode::AppNotFound,
        ))),
    }
}

/// 安装应用
async fn install_app(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<ApiResponse<()>> {
    // 从商店获取应用清单
    let manifest = match state.store_service.get_app_manifest(&id).await {
        Some(m) => m,
        None => {
            return Json(ApiResponse::error(&pnos::error::PnosError::from(
                pnos::error::ErrorCode::AppNotFound,
            )));
        }
    };

    // 转换为 PackageManifest
    let env: std::collections::HashMap<String, String> = manifest
        .run
        .env
        .iter()
        .map(|e| (e.name.clone(), e.value.clone()))
        .collect();

    let package = PackageManifest {
        id: manifest.id.clone(),
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        entrypoint: format!("./{}", manifest.binary.binary_name),
        port: manifest.run.port,
        health_check_path: manifest
            .health_check
            .as_ref()
            .and_then(|h| h.url.clone())
            .unwrap_or_else(|| "/health/ready".to_string()),
        startup_timeout: 30,
        shutdown_timeout: 10,
        env,
        memory_limit: 0,
        cpu_limit: 0.0,
        download_url: manifest.binary.download_url,
        sha256: manifest.binary.sha256,
    };

    match state.install_service.install(package).await {
        Ok(_) => Json(ApiResponse::success_with_msg((), "安装成功")),
        Err(e) => Json(ApiResponse::error(&pnos::error::PnosError::External(
            e.to_string(),
        ))),
    }
}

/// 升级应用
async fn upgrade_app(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<ApiResponse<()>> {
    // 从商店获取最新应用清单
    let manifest = match state.store_service.get_app_manifest(&id).await {
        Some(m) => m,
        None => {
            return Json(ApiResponse::error(&pnos::error::PnosError::from(
                pnos::error::ErrorCode::AppNotFound,
            )));
        }
    };

    let env: std::collections::HashMap<String, String> = manifest
        .run
        .env
        .iter()
        .map(|e| (e.name.clone(), e.value.clone()))
        .collect();

    let package = PackageManifest {
        id: manifest.id.clone(),
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        entrypoint: format!("./{}", manifest.binary.binary_name),
        port: manifest.run.port,
        health_check_path: manifest
            .health_check
            .as_ref()
            .and_then(|h| h.url.clone())
            .unwrap_or_else(|| "/health/ready".to_string()),
        startup_timeout: 30,
        shutdown_timeout: 10,
        env,
        memory_limit: 0,
        cpu_limit: 0.0,
        download_url: manifest.binary.download_url,
        sha256: manifest.binary.sha256,
    };

    match state.install_service.upgrade(package).await {
        Ok(_) => Json(ApiResponse::success_with_msg((), "升级成功")),
        Err(e) => Json(ApiResponse::error(&pnos::error::PnosError::External(
            e.to_string(),
        ))),
    }
}

/// 查询安装进度
async fn install_progress(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<ApiResponse<Option<crate::install::InstallProgress>>> {
    Json(ApiResponse::success(state.install_service.progress(&id)))
}

/// 启动已安装应用
async fn install_start(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<ApiResponse<()>> {
    match state.install_service.start(&id).await {
        Ok(_) => Json(ApiResponse::success_with_msg((), "启动成功")),
        Err(e) => Json(ApiResponse::error(&pnos::error::PnosError::External(
            e.to_string(),
        ))),
    }
}

/// 停止已安装应用
async fn install_stop(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<ApiResponse<()>> {
    match state.install_service.stop(&id).await {
        Ok(_) => Json(ApiResponse::success_with_msg((), "停止成功")),
        Err(e) => Json(ApiResponse::error(&pnos::error::PnosError::External(
            e.to_string(),
        ))),
    }
}

/// 卸载应用
async fn uninstall_app(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Json<ApiResponse<()>> {
    let keep_data = params.get("keep_data").map(|v| v == "true").unwrap_or(true);

    match state.install_service.uninstall(&id, keep_data).await {
        Ok(_) => Json(ApiResponse::success_with_msg((), "卸载成功")),
        Err(e) => Json(ApiResponse::error(&pnos::error::PnosError::External(
            e.to_string(),
        ))),
    }
}
