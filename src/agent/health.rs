//! Agent 健康检查
//!
//! 定期调用 Agent 的 /health/ready 端点，检查 Agent 是否就绪。

use std::sync::Arc;
use std::time::Duration;

use tracing::{debug, warn};

use crate::config::AppState;

/// 检查 Agent 健康状态
pub async fn check_health(agent_id: &str, state: Arc<AppState>) -> bool {
    // 从注册中心获取组件信息
    let component = {
        let registry = &state.registry;
        registry.get(agent_id).await
    };

    let component = match component {
        Some(c) => c,
        None => {
            debug!("Agent {} 未在注册中心注册，跳过健康检查", agent_id);
            return false;
        }
    };

    // 构建健康检查 URL
    let health_url = format!(
        "http://{}:{}/health/ready",
        component
            .serve_host
            .unwrap_or_else(|| "127.0.0.1".to_string()),
        component.serve_port.unwrap_or(component.port)
    );

    // 发送健康检查请求
    let client = &state.http_client;
    match client
        .get(&health_url)
        .timeout(Duration::from_secs(3))
        .send()
        .await
    {
        Ok(resp) => {
            let healthy = resp.status().is_success();
            if !healthy {
                debug!("Agent {} 健康检查失败: HTTP {}", agent_id, resp.status());
            }
            healthy
        }
        Err(e) => {
            warn!("Agent {} 健康检查请求失败: {}", agent_id, e);
            false
        }
    }
}
