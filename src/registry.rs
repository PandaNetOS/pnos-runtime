//! 应用注册中心
//!
//! 内存存储所有已注册应用的信息，提供注册、心跳、注销、发现接口。
//! 后台任务定期检查心跳超时，标记离线应用。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;
use tracing::{info, warn};

use pnos::app::AppStatus;
use pnos::registry::{AppInfo, AppRegisterRequest, AppRegisterResponse};

/// 注册的应用记录（含内部状态）
struct RegisteredApp {
    info: AppInfo,
    last_heartbeat: Instant,
    token: String,
}

/// 应用注册中心
#[derive(Clone)]
pub struct Registry {
    apps: Arc<RwLock<HashMap<String, RegisteredApp>>>,
    heartbeat_timeout: Duration,
}

impl Registry {
    /// 创建注册中心
    pub fn new(heartbeat_timeout_secs: u64) -> Self {
        Self {
            apps: Arc::new(RwLock::new(HashMap::new())),
            heartbeat_timeout: Duration::from_secs(heartbeat_timeout_secs),
        }
    }

    /// 注册应用
    pub async fn register(&self, req: AppRegisterRequest) -> AppRegisterResponse {
        let token = uuid::Uuid::new_v4().to_string();
        let now = pnos::time::now_rfc3339();

        let info = AppInfo {
            id: req.id.clone(),
            name: req.name,
            version: req.version,
            address: "127.0.0.1".to_string(),
            port: req.port,
            status: AppStatus::Running,
            health_check_path: req.health_check_path,
            web_path: req.web_path,
            dependencies: req.dependencies,
            registered_at: now.clone(),
            last_heartbeat: now.clone(),
        };

        let app = RegisteredApp {
            info: info.clone(),
            last_heartbeat: Instant::now(),
            token: token.clone(),
        };

        self.apps.write().await.insert(req.id.clone(), app);
        info!("应用注册: {} (port={})", info.id, info.port);

        AppRegisterResponse {
            token,
            app_id: info.id,
            registered_at: now.clone(),
        }
    }

    /// 注销应用
    pub async fn unregister(&self, app_id: &str) -> bool {
        let removed = self.apps.write().await.remove(app_id).is_some();
        if removed {
            info!("应用注销: {}", app_id);
        }
        removed
    }

    /// 心跳
    pub async fn heartbeat(&self, app_id: &str, status: AppStatus) -> bool {
        let mut apps = self.apps.write().await;
        if let Some(app) = apps.get_mut(app_id) {
            app.last_heartbeat = Instant::now();
            app.info.status = status;
            app.info.last_heartbeat = pnos::time::now_rfc3339();
            true
        } else {
            false
        }
    }

    /// 获取应用信息
    pub async fn get(&self, app_id: &str) -> Option<AppInfo> {
        self.apps.read().await.get(app_id).map(|a| a.info.clone())
    }

    /// 列出所有应用
    pub async fn list(&self) -> Vec<AppInfo> {
        self.apps
            .read()
            .await
            .values()
            .map(|a| a.info.clone())
            .collect()
    }

    /// 验证 Token
    pub async fn verify_token(&self, app_id: &str, token: &str) -> bool {
        self.apps
            .read()
            .await
            .get(app_id)
            .map(|a| a.token == token)
            .unwrap_or(false)
    }

    /// 启动心跳超时检查任务
    pub fn start_heartbeat_checker(&self) {
        let registry = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;
                registry.check_heartbeats().await;
            }
        });
    }

    /// 检查心跳超时
    async fn check_heartbeats(&self) {
        let mut apps = self.apps.write().await;
        let now = Instant::now();
        for app in apps.values_mut() {
            if now.duration_since(app.last_heartbeat) > self.heartbeat_timeout {
                if app.info.status != AppStatus::Error {
                    warn!("应用心跳超时，标记为错误: {}", app.info.id);
                    app.info.status = AppStatus::Error;
                }
            }
        }
    }
}
