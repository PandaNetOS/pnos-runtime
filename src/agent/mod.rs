//! Agent 生命周期管理
//!
//! 管理 Agent 进程的启动、停止、崩溃重启、健康检查。
//! Agent 作为独立进程运行，由 runtime 管理生命周期。

pub mod health;
pub mod process;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use tracing::{error, info, warn};

use crate::config::AppState;

/// Agent 配置
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// 组件 ID
    pub id: String,
    /// 组件名称
    pub name: String,
    /// 版本
    pub version: String,
    /// 监听端口
    pub port: u16,
    /// 入口命令（相对安装目录）
    pub entrypoint: String,
    /// 安装目录
    pub install_dir: PathBuf,
    /// 数据目录
    pub data_dir: PathBuf,
    /// 环境变量
    pub env: HashMap<String, String>,
    /// 内存限制（字节，0 表示不限制）
    pub memory_limit: u64,
    /// CPU 限制（核数，0 表示不限制）
    pub cpu_limit: f32,
    /// 健康检查路径
    pub health_check_path: String,
    /// 启动超时（秒）
    pub startup_timeout: u64,
    /// 优雅关闭超时（秒）
    pub shutdown_timeout: u64,
}

/// Agent 运行状态
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    /// 未安装
    NotInstalled,
    /// 已安装，未启动
    Stopped,
    /// 启动中
    Starting,
    /// 运行中
    Running,
    /// 停止中
    Stopping,
    /// 错误（连续崩溃超过阈值）
    Error,
}

impl AgentStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentStatus::NotInstalled => "not_installed",
            AgentStatus::Stopped => "stopped",
            AgentStatus::Starting => "starting",
            AgentStatus::Running => "running",
            AgentStatus::Stopping => "stopping",
            AgentStatus::Error => "error",
        }
    }
}

/// Agent 句柄（运行时状态）
pub struct AgentHandle {
    /// 配置
    pub config: AgentConfig,
    /// 状态
    pub status: AgentStatus,
    /// 子进程（运行中时存在）
    pub child: Option<tokio::process::Child>,
    /// 连续崩溃次数
    pub crash_count: u32,
    /// 上次崩溃时间
    pub last_crash: Option<std::time::Instant>,
    /// 启动时间
    pub started_at: Option<std::time::Instant>,
    /// 健康检查是否通过
    pub healthy: bool,
    /// 连续健康检查失败次数
    pub health_failures: u32,
}

impl AgentHandle {
    pub fn new(config: AgentConfig) -> Self {
        Self {
            config,
            status: AgentStatus::Stopped,
            child: None,
            crash_count: 0,
            last_crash: None,
            started_at: None,
            healthy: false,
            health_failures: 0,
        }
    }
}

/// Agent 管理器
#[derive(Clone)]
pub struct AgentManager {
    agents: Arc<RwLock<HashMap<String, AgentHandle>>>,
    runtime_url: String,
}

impl AgentManager {
    pub fn new(runtime_url: String) -> Self {
        Self {
            agents: Arc::new(RwLock::new(HashMap::new())),
            runtime_url,
        }
    }

    /// 注册 Agent（安装后调用，初始状态 Stopped）
    pub async fn register(&self, config: AgentConfig) {
        let id = config.id.clone();
        info!("注册 Agent: {} (port={})", id, config.port);
        self.agents
            .write()
            .await
            .insert(id, AgentHandle::new(config));
    }

    /// 注销 Agent（卸载后调用）
    pub async fn unregister(&self, id: &str) -> Option<AgentConfig> {
        self.agents.write().await.remove(id).map(|h| h.config)
    }

    /// 启动 Agent
    pub async fn start(&self, id: &str) -> anyhow::Result<()> {
        let mut agents = self.agents.write().await;
        let handle = agents
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("Agent 未注册: {}", id))?;

        if handle.status == AgentStatus::Running || handle.status == AgentStatus::Starting {
            return Err(anyhow::anyhow!("Agent 已在运行: {}", id));
        }

        handle.status = AgentStatus::Starting;
        let config = handle.config.clone();
        drop(agents);

        info!("启动 Agent: {}", id);

        // 构建命令
        let cmd = config.entrypoint.clone();
        let work_dir = config.install_dir.clone();

        // 环境变量
        let mut env = config.env.clone();
        env.insert("PNOS_RUNTIME_URL".to_string(), self.runtime_url.clone());
        env.insert(
            "PNOS_DATA_DIR".to_string(),
            config.data_dir.to_string_lossy().to_string(),
        );
        env.insert("PORT".to_string(), config.port.to_string());
        env.insert("PNOS_COMPONENT_ID".to_string(), id.to_string());

        // 启动进程
        let mut command = tokio::process::Command::new(&cmd);
        command
            .current_dir(&work_dir)
            .envs(&env)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        let child = command
            .spawn()
            .map_err(|e| anyhow::anyhow!("启动 Agent 失败: {} - {}", id, e))?;

        let mut agents = self.agents.write().await;
        if let Some(handle) = agents.get_mut(id) {
            handle.child = Some(child);
            handle.status = AgentStatus::Running;
            handle.started_at = Some(std::time::Instant::now());
            handle.crash_count = 0;
            handle.health_failures = 0;
        }

        info!("Agent 已启动: {}", id);
        Ok(())
    }

    /// 停止 Agent（优雅关闭）
    pub async fn stop(&self, id: &str) -> anyhow::Result<()> {
        let mut agents = self.agents.write().await;
        let handle = agents
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("Agent 未注册: {}", id))?;

        if handle.status != AgentStatus::Running {
            return Err(anyhow::anyhow!("Agent 未在运行: {}", id));
        }

        handle.status = AgentStatus::Stopping;
        let timeout = handle.config.shutdown_timeout;
        let child = handle.child.take();
        drop(agents);

        info!("停止 Agent: {} (超时 {}s)", id, timeout);

        if let Some(mut child) = child {
            // 发送 SIGTERM（Windows 上是 CTRL_C_EVENT）
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                let _ = child.start_kill();
            }
            #[cfg(windows)]
            {
                // Windows 上 tokio 的 Child 没有直接的 kill 方法发送 CTRL_C
                // 先用 kill（相当于 TerminateProcess）
                let _ = child.start_kill();
            }

            // 等待退出
            match tokio::time::timeout(Duration::from_secs(timeout), child.wait()).await {
                Ok(_) => {
                    info!("Agent 已停止: {}", id);
                }
                Err(_) => {
                    warn!("Agent 停止超时，强制终止: {}", id);
                    let _ = child.kill().await;
                }
            }
        }

        let mut agents = self.agents.write().await;
        if let Some(handle) = agents.get_mut(id) {
            handle.status = AgentStatus::Stopped;
            handle.child = None;
            handle.healthy = false;
        }

        Ok(())
    }

    /// 重启 Agent
    pub async fn restart(&self, id: &str) -> anyhow::Result<()> {
        info!("重启 Agent: {}", id);
        let status = self.get_status(id).await;
        if status == Some(AgentStatus::Running) {
            self.stop(id).await?;
        }
        self.start(id).await
    }

    /// 获取 Agent 状态
    pub async fn get_status(&self, id: &str) -> Option<AgentStatus> {
        self.agents.read().await.get(id).map(|h| h.status.clone())
    }

    /// 列出所有 Agent
    pub async fn list(&self) -> Vec<AgentInfo> {
        let agents = self.agents.read().await;
        agents
            .values()
            .map(|h| AgentInfo {
                id: h.config.id.clone(),
                name: h.config.name.clone(),
                version: h.config.version.clone(),
                port: h.config.port,
                status: h.status.clone(),
                healthy: h.healthy,
                crash_count: h.crash_count,
                started_at: h.started_at.map(|t| t.elapsed().as_secs()),
            })
            .collect()
    }

    /// 获取 Agent 详情
    pub async fn get(&self, id: &str) -> Option<AgentInfo> {
        self.agents.read().await.get(id).map(|h| AgentInfo {
            id: h.config.id.clone(),
            name: h.config.name.clone(),
            version: h.config.version.clone(),
            port: h.config.port,
            status: h.status.clone(),
            healthy: h.healthy,
            crash_count: h.crash_count,
            started_at: h.started_at.map(|t| t.elapsed().as_secs()),
        })
    }

    /// 标记崩溃（由监控循环调用）
    pub async fn mark_crash(&self, id: &str) {
        let mut agents = self.agents.write().await;
        if let Some(handle) = agents.get_mut(id) {
            handle.crash_count += 1;
            handle.last_crash = Some(std::time::Instant::now());
            handle.child = None;
            handle.healthy = false;

            // 连续崩溃超过 5 次，标记为 Error
            if handle.crash_count >= 5 {
                error!(
                    "Agent {} 连续崩溃 {} 次，标记为 Error",
                    id, handle.crash_count
                );
                handle.status = AgentStatus::Error;
            } else {
                handle.status = AgentStatus::Stopped;
            }
        }
    }

    /// 重置崩溃计数（手动重启后调用）
    pub async fn reset_crash_count(&self, id: &str) {
        let mut agents = self.agents.write().await;
        if let Some(handle) = agents.get_mut(id) {
            handle.crash_count = 0;
            if handle.status == AgentStatus::Error {
                handle.status = AgentStatus::Stopped;
            }
        }
    }

    /// 更新健康状态
    pub async fn update_health(&self, id: &str, healthy: bool) {
        let mut agents = self.agents.write().await;
        if let Some(handle) = agents.get_mut(id) {
            if healthy {
                handle.healthy = true;
                handle.health_failures = 0;
            } else {
                handle.health_failures += 1;
                if handle.health_failures >= 3 {
                    handle.healthy = false;
                }
            }
        }
    }

    /// 启动监控循环（崩溃检测 + 健康检查 + 自动重启）
    pub fn start_monitor(self: Arc<Self>, state: Arc<AppState>) {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            loop {
                interval.tick().await;
                self.monitor_tick(state.clone()).await;
            }
        });
    }

    /// 单次监控检查
    async fn monitor_tick(&self, state: Arc<AppState>) {
        // 1. 收集所有 Running 状态的 agent id（只读锁）
        let running_ids: Vec<String> = {
            let agents = self.agents.read().await;
            agents
                .iter()
                .filter(|(_, h)| h.status == AgentStatus::Running)
                .map(|(id, _)| id.clone())
                .collect()
        };

        // 2. 崩溃检测（对每个 running agent 获取写锁检查进程）
        for id in &running_ids {
            let crashed = {
                let mut agents = self.agents.write().await;
                if let Some(handle) = agents.get_mut(id) {
                    if let Some(child) = &mut handle.child {
                        match child.try_wait() {
                            Ok(Some(status)) => {
                                warn!("Agent {} 进程已退出: {}", id, status);
                                true
                            }
                            Ok(None) => false,
                            Err(e) => {
                                warn!("检查 Agent {} 进程状态失败: {}", id, e);
                                false
                            }
                        }
                    } else {
                        false
                    }
                } else {
                    false
                }
            };

            if crashed {
                self.mark_crash(id).await;
                crate::metrics::global().map(|m| m.incr_agent_crash());
                // 自动重启（如果崩溃次数 < 5）
                let crash_count = self
                    .agents
                    .read()
                    .await
                    .get(id)
                    .map(|h| h.crash_count)
                    .unwrap_or(0);
                if crash_count < 5 {
                    // 指数退避
                    let delay = std::cmp::min(1u64 << crash_count, 16);
                    info!(
                        "Agent {} 崩溃，{}s 后自动重启（第 {} 次）",
                        id, delay, crash_count
                    );
                    tokio::time::sleep(Duration::from_secs(delay)).await;
                    match self.start(id).await {
                        Ok(_) => {
                            crate::metrics::global().map(|m| m.incr_agent_restart());
                        }
                        Err(e) => error!("Agent {} 自动重启失败: {}", id, e),
                    }
                }
            }
        }

        // 3. 健康检查
        for id in &running_ids {
            let should_check = {
                let agents = self.agents.read().await;
                if let Some(handle) = agents.get(id) {
                    handle.status == AgentStatus::Running
                        && handle
                            .started_at
                            .map(|t| t.elapsed().as_secs() > 5)
                            .unwrap_or(false)
                } else {
                    false
                }
            };

            if should_check {
                let healthy = health::check_health(id, state.clone()).await;
                self.update_health(id, healthy).await;
            }
        }
    }
}

/// Agent 信息（用于 API 响应）
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub port: u16,
    pub status: AgentStatus,
    pub healthy: bool,
    pub crash_count: u32,
    /// 已运行秒数
    #[serde(rename = "uptime_seconds")]
    pub started_at: Option<u64>,
}
