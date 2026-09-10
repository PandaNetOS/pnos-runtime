//! 组件注册中心（统一应用+Agent）
//!
//! 内存存储所有已注册组件的信息，提供注册、心跳、注销、发现接口。
//! 后台任务定期检查心跳超时，标记离线组件。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;
use tracing::{info, warn};

use pnos::component::{ComponentStatus, ComponentType};
use pnos::health::HealthStatus;
use pnos::registry::{ComponentInfo, ComponentRegisterRequest, ComponentRegisterResponse};

/// 注册的组件记录（含内部状态）
struct RegisteredComponent {
    info: ComponentInfo,
    last_heartbeat: Instant,
    token: String,
}

/// 组件注册中心
#[derive(Clone)]
pub struct Registry {
    components: Arc<RwLock<HashMap<String, RegisteredComponent>>>,
    heartbeat_timeout: Duration,
}

impl Registry {
    /// 创建注册中心
    pub fn new(heartbeat_timeout_secs: u64) -> Self {
        Self {
            components: Arc::new(RwLock::new(HashMap::new())),
            heartbeat_timeout: Duration::from_secs(heartbeat_timeout_secs),
        }
    }

    /// 注册组件
    pub async fn register(&self, req: ComponentRegisterRequest) -> ComponentRegisterResponse {
        let token = uuid::Uuid::new_v4().to_string();
        let now = pnos::time::now_rfc3339();
        let base_url = format!("http://127.0.0.1:{}", req.port);
        let serve_url = match (&req.serve_host, req.serve_port) {
            (Some(host), Some(port)) => Some(format!("http://{}:{}", host, port)),
            _ => None,
        };

        let info = ComponentInfo {
            id: req.id.clone(),
            name: req.name,
            version: req.version,
            component_type: req.component_type,
            address: "127.0.0.1".to_string(),
            port: req.port,
            serve_host: req.serve_host,
            serve_port: req.serve_port,
            capabilities: req.capabilities,
            region: req.region,
            hostname: req.hostname,
            platform: req.platform,
            arch: req.arch,
            labels: req.labels,
            max_concurrent: req.max_concurrent,
            max_bandwidth_bps: req.max_bandwidth_bps,
            status: ComponentStatus::Running,
            load: 0.0,
            active_tasks: 0,
            bytes_downloaded: 0,
            health: HealthStatus::Ok,
            last_heartbeat: now.clone(),
            registered_at: now.clone(),
            base_url,
            serve_url,
            web_path: req.web_path,
        };

        let component = RegisteredComponent {
            info: info.clone(),
            last_heartbeat: Instant::now(),
            token: token.clone(),
        };

        self.components
            .write()
            .await
            .insert(req.id.clone(), component);
        info!(
            "组件注册: {} (type={}, port={})",
            info.id, info.component_type, info.port
        );

        ComponentRegisterResponse {
            token,
            component_id: info.id,
            registered_at: now,
        }
    }

    /// 注销组件
    pub async fn unregister(&self, component_id: &str) -> bool {
        let removed = self.components.write().await.remove(component_id).is_some();
        if removed {
            info!("组件注销: {}", component_id);
        }
        removed
    }

    /// 心跳（统一组件心跳，含状态+负载+任务统计）
    pub async fn heartbeat(
        &self,
        component_id: &str,
        status: ComponentStatus,
        load: f32,
        active_tasks: u32,
        bytes_downloaded: u64,
    ) -> bool {
        let mut components = self.components.write().await;
        if let Some(component) = components.get_mut(component_id) {
            component.last_heartbeat = Instant::now();
            component.info.status = status;
            component.info.load = load;
            component.info.active_tasks = active_tasks;
            component.info.bytes_downloaded = bytes_downloaded;
            component.info.last_heartbeat = pnos::time::now_rfc3339();
            true
        } else {
            false
        }
    }

    /// 获取组件信息
    pub async fn get(&self, component_id: &str) -> Option<ComponentInfo> {
        self.components
            .read()
            .await
            .get(component_id)
            .map(|c| c.info.clone())
    }

    /// 列出所有组件
    pub async fn list(&self) -> Vec<ComponentInfo> {
        self.components
            .read()
            .await
            .values()
            .map(|c| c.info.clone())
            .collect()
    }

    /// 按类型筛选组件
    pub async fn list_by_type(&self, component_type: ComponentType) -> Vec<ComponentInfo> {
        self.components
            .read()
            .await
            .values()
            .filter(|c| c.info.component_type == component_type)
            .map(|c| c.info.clone())
            .collect()
    }

    /// 验证 Token
    pub async fn verify_token(&self, component_id: &str, token: &str) -> bool {
        self.components
            .read()
            .await
            .get(component_id)
            .map(|c| c.token == token)
            .unwrap_or(false)
    }

    /// 启动心跳超时检查任务
    pub fn start_heartbeat_checker(&self) {
        let registry = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;
                registry.check_heartbeats().await;
                crate::metrics::global().map(|m| m.mark_task("heartbeat_checker"));
            }
        });
    }

    /// 检查心跳超时
    async fn check_heartbeats(&self) {
        let mut components = self.components.write().await;
        let now = Instant::now();
        for component in components.values_mut() {
            if now.duration_since(component.last_heartbeat) > self.heartbeat_timeout {
                if component.info.status != ComponentStatus::Offline {
                    warn!("组件心跳超时，标记为离线: {}", component.info.id);
                    component.info.status = ComponentStatus::Offline;
                }
            }
        }
    }
}
