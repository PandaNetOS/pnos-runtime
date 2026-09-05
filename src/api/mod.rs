//! API 路由注册

pub mod apps;
pub mod store;
pub mod system;

use std::sync::Arc;

use axum::Router;

use crate::config::AppState;

/// 注册所有 API 路由
pub fn routes(state: Arc<AppState>) -> Router {
    Router::new()
        .merge(system::routes(state.clone()))
        .merge(apps::routes(state.clone()))
        .merge(store::routes(state))
}
