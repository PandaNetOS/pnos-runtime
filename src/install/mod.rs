//! 应用安装服务
//!
//! 管理应用的安装、升级（蓝绿部署）、卸载。
//! 包格式：manifest.json + 二进制文件（tar.gz 或 zip）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{error, info, warn};

use crate::agent::{AgentConfig, AgentManager};

/// 应用包清单
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct PackageManifest {
    /// 组件 ID
    pub id: String,
    /// 组件名称
    pub name: String,
    /// 版本
    pub version: String,
    /// 入口命令（相对包根目录）
    pub entrypoint: String,
    /// 监听端口（0 表示自动分配）
    pub port: u16,
    /// 健康检查路径
    #[serde(default = "default_health_path")]
    pub health_check_path: String,
    /// 启动超时（秒）
    #[serde(default = "default_startup_timeout")]
    pub startup_timeout: u64,
    /// 优雅关闭超时（秒）
    #[serde(default = "default_shutdown_timeout")]
    pub shutdown_timeout: u64,
    /// 环境变量
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// 内存限制（字节，0 表示不限制）
    #[serde(default)]
    pub memory_limit: u64,
    /// CPU 限制（核数，0 表示不限制）
    #[serde(default)]
    pub cpu_limit: f32,
    /// 包下载 URL
    pub download_url: String,
    /// 包 SHA256 校验和
    pub sha256: Option<String>,
}

fn default_health_path() -> String {
    "/health/ready".to_string()
}

fn default_startup_timeout() -> u64 {
    30
}

fn default_shutdown_timeout() -> u64 {
    10
}

/// 已安装应用信息
#[derive(Debug, Clone)]
pub struct InstalledApp {
    /// 组件 ID
    pub id: String,
    /// 当前版本
    pub version: String,
    /// 安装目录（蓝绿部署中的当前激活目录）
    pub active_dir: PathBuf,
    /// 蓝目录
    pub blue_dir: PathBuf,
    /// 绿目录
    pub green_dir: PathBuf,
    /// 当前激活颜色
    pub active_color: Color,
    /// 数据目录
    pub data_dir: PathBuf,
    /// 包清单
    pub manifest: PackageManifest,
}

/// 蓝绿部署颜色
#[derive(Debug, Clone, PartialEq, Copy)]
pub enum Color {
    Blue,
    Green,
}

impl Color {
    pub fn other(&self) -> Color {
        match self {
            Color::Blue => Color::Green,
            Color::Green => Color::Blue,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Color::Blue => "blue",
            Color::Green => "green",
        }
    }
}

/// 安装进度
#[derive(Debug, Clone, serde::Serialize)]
pub struct InstallProgress {
    /// 阶段：downloading / extracting / starting / done
    pub phase: String,
    /// 已下载字节
    pub downloaded: u64,
    /// 总字节（未知为 None）
    pub total: Option<u64>,
    /// 附加信息（如失败原因）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// 安装服务
#[derive(Clone)]
pub struct InstallService {
    apps_dir: PathBuf,
    data_dir: PathBuf,
    installed: Arc<RwLock<HashMap<String, InstalledApp>>>,
    /// 安装进度表（含历史，重复安装时覆盖）
    progress: Arc<std::sync::Mutex<HashMap<String, InstallProgress>>>,
    agent_manager: Arc<AgentManager>,
    http_client: reqwest::Client,
}

impl InstallService {
    pub fn new(apps_dir: PathBuf, data_dir: PathBuf, agent_manager: Arc<AgentManager>) -> Self {
        Self {
            apps_dir,
            data_dir,
            installed: Arc::new(RwLock::new(HashMap::new())),
            progress: Arc::new(std::sync::Mutex::new(HashMap::new())),
            agent_manager,
            http_client: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(30))
                .timeout(std::time::Duration::from_secs(600))
                .build()
                .unwrap_or_default(),
        }
    }

    /// 查询安装进度
    pub fn progress(&self, id: &str) -> Option<InstallProgress> {
        self.progress.lock().ok()?.get(id).cloned()
    }

    /// 启动已安装应用
    pub async fn start(&self, id: &str) -> anyhow::Result<()> {
        self.installed
            .read()
            .await
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("应用未安装: {}", id))?;
        self.agent_manager.start(id).await
    }

    /// 停止已安装应用
    pub async fn stop(&self, id: &str) -> anyhow::Result<()> {
        self.installed
            .read()
            .await
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("应用未安装: {}", id))?;
        self.agent_manager.stop(id).await
    }

    fn set_progress(
        &self,
        id: &str,
        phase: &str,
        downloaded: u64,
        total: Option<u64>,
        message: Option<String>,
    ) {
        if let Ok(mut map) = self.progress.lock() {
            map.insert(
                id.to_string(),
                InstallProgress {
                    phase: phase.to_string(),
                    downloaded,
                    total,
                    message,
                },
            );
        }
    }

    /// 安装应用
    pub async fn install(&self, manifest: PackageManifest) -> anyhow::Result<()> {
        let id = manifest.id.clone();
        info!("安装应用: {} v{}", id, manifest.version);

        // 检查是否已安装
        if self.installed.read().await.contains_key(&id) {
            return Err(anyhow::anyhow!("应用已安装: {}，请使用升级", id));
        }

        // 1. 创建目录结构
        let app_dir = self.apps_dir.join(&id);
        let blue_dir = app_dir.join("blue");
        let green_dir = app_dir.join("green");
        let data_dir = self.data_dir.join(&id);

        tokio::fs::create_dir_all(&blue_dir).await?;
        tokio::fs::create_dir_all(&green_dir).await?;
        tokio::fs::create_dir_all(&data_dir).await?;

        // 2. 下载并解压到 blue 目录
        self.download_and_extract(&manifest, &blue_dir).await?;

        // 3. 试运行（启动 → 健康检查 → 停止）
        self.set_progress(&id, "starting", 0, None, None);
        let port = self.allocate_port(manifest.port).await?;
        let trial_config = self.build_agent_config(&manifest, &blue_dir, &data_dir, port);
        self.agent_manager.register(trial_config.clone()).await;

        info!("试运行应用: {}", id);
        if let Err(e) = self.agent_manager.start(&id).await {
            error!("试运行启动失败: {} - {}", id, e);
            self.agent_manager.unregister(&id).await;
            return Err(anyhow::anyhow!("试运行启动失败: {}", e));
        }

        // 等待健康检查通过
        let healthy = self.wait_for_health(&id, manifest.startup_timeout).await;
        if !healthy {
            warn!("试运行健康检查失败，停止应用: {}", id);
            let _ = self.agent_manager.stop(&id).await;
            self.agent_manager.unregister(&id).await;
            return Err(anyhow::anyhow!("试运行健康检查失败"));
        }

        // 停止试运行
        self.agent_manager.stop(&id).await?;
        self.agent_manager.unregister(&id).await;

        // 4. 正式注册并启动
        let agent_config = self.build_agent_config(&manifest, &blue_dir, &data_dir, port);
        self.agent_manager.register(agent_config.clone()).await;

        // 记录已安装
        let version = manifest.version.clone();
        let installed = InstalledApp {
            id: id.clone(),
            version: manifest.version.clone(),
            active_dir: blue_dir.clone(),
            blue_dir,
            green_dir,
            active_color: Color::Blue,
            data_dir,
            manifest,
        };
        self.installed.write().await.insert(id.clone(), installed);

        // 启动应用
        self.agent_manager.start(&id).await?;

        self.set_progress(&id, "done", 0, None, None);
        info!("应用安装完成: {} v{}", id, version);
        Ok(())
    }

    /// 升级应用（蓝绿部署）
    pub async fn upgrade(&self, manifest: PackageManifest) -> anyhow::Result<()> {
        let id = manifest.id.clone();
        info!("升级应用: {} -> v{}", id, manifest.version);

        let installed = { self.installed.read().await.get(&id).cloned() };
        let installed = installed.ok_or_else(|| anyhow::anyhow!("应用未安装: {}", id))?;

        if installed.version == manifest.version {
            return Err(anyhow::anyhow!("已是最新版本: {}", manifest.version));
        }

        // 1. 确定目标颜色（非激活颜色）
        let target_color = installed.active_color.other();
        let target_dir = match target_color {
            Color::Blue => installed.blue_dir.clone(),
            Color::Green => installed.green_dir.clone(),
        };

        // 2. 清空目标目录
        if target_dir.exists() {
            tokio::fs::remove_dir_all(&target_dir).await?;
        }
        tokio::fs::create_dir_all(&target_dir).await?;

        // 3. 下载并解压到目标目录
        self.download_and_extract(&manifest, &target_dir).await?;

        // 4. 停止旧版本
        self.set_progress(&id, "starting", 0, None, None);
        info!("停止旧版本: {}", id);
        self.agent_manager.stop(&id).await?;

        // 5. 切换到新版本（更新 AgentConfig）
        let port = installed.manifest.port;
        let new_config = self.build_agent_config(&manifest, &target_dir, &installed.data_dir, port);

        // 注销旧的，注册新的
        self.agent_manager.unregister(&id).await;
        self.agent_manager.register(new_config).await;

        // 6. 启动新版本
        info!("启动新版本: {} v{}", id, manifest.version);
        if let Err(e) = self.agent_manager.start(&id).await {
            error!("新版本启动失败，回滚: {} - {}", id, e);
            // 回滚
            self.agent_manager.unregister(&id).await;
            let old_config = self.build_agent_config(
                &installed.manifest,
                &installed.active_dir,
                &installed.data_dir,
                installed.manifest.port,
            );
            self.agent_manager.register(old_config).await;
            self.agent_manager.start(&id).await?;
            return Err(anyhow::anyhow!("新版本启动失败，已回滚: {}", e));
        }

        // 7. 健康检查
        let healthy = self.wait_for_health(&id, manifest.startup_timeout).await;
        if !healthy {
            error!("新版本健康检查失败，回滚: {}", id);
            self.agent_manager.stop(&id).await?;
            self.agent_manager.unregister(&id).await;
            let old_config = self.build_agent_config(
                &installed.manifest,
                &installed.active_dir,
                &installed.data_dir,
                installed.manifest.port,
            );
            self.agent_manager.register(old_config).await;
            self.agent_manager.start(&id).await?;
            return Err(anyhow::anyhow!("新版本健康检查失败，已回滚"));
        }

        // 8. 更新已安装记录
        let mut installed_map = self.installed.write().await;
        if let Some(installed) = installed_map.get_mut(&id) {
            installed.version = manifest.version.clone();
            installed.active_dir = target_dir.clone();
            installed.active_color = target_color;
            installed.manifest = manifest;
        }

        self.set_progress(&id, "done", 0, None, None);
        info!("应用升级完成: {} v{}", id, installed.version);
        Ok(())
    }

    /// 卸载应用
    pub async fn uninstall(&self, id: &str, keep_data: bool) -> anyhow::Result<()> {
        info!("卸载应用: {} (保留数据={})", id, keep_data);

        let installed = { self.installed.read().await.get(id).cloned() };
        let installed = installed.ok_or_else(|| anyhow::anyhow!("应用未安装: {}", id))?;

        // 1. 停止应用
        if self.agent_manager.get_status(id).await == Some(crate::agent::AgentStatus::Running) {
            self.agent_manager.stop(id).await?;
        }

        // 2. 注销 Agent
        self.agent_manager.unregister(id).await;

        // 3. 删除安装目录
        let app_dir = self.apps_dir.join(id);
        if app_dir.exists() {
            tokio::fs::remove_dir_all(&app_dir).await?;
        }

        // 4. 删除数据目录（如果不保留）
        if !keep_data {
            let data_dir = self.data_dir.join(id);
            if data_dir.exists() {
                tokio::fs::remove_dir_all(&data_dir).await?;
            }
        }

        // 5. 移除记录
        self.installed.write().await.remove(id);

        info!("应用卸载完成: {}", id);
        Ok(())
    }

    /// 列出已安装应用
    pub async fn list_installed(&self) -> Vec<InstalledAppInfo> {
        let installed = self.installed.read().await;
        let mut result = Vec::with_capacity(installed.len());
        for a in installed.values() {
            let status = self
                .agent_manager
                .get_status(&a.id)
                .await
                .map(|s| s.as_str().to_string())
                .unwrap_or_else(|| "unknown".to_string());
            result.push(InstalledAppInfo {
                id: a.id.clone(),
                name: a.manifest.name.clone(),
                version: a.version.clone(),
                active_color: a.active_color.as_str().to_string(),
                port: a.manifest.port,
                status,
            });
        }
        result
    }

    /// 获取已安装应用详情
    pub async fn get_installed(&self, id: &str) -> Option<InstalledApp> {
        self.installed.read().await.get(id).cloned()
    }

    // ---- 内部方法 ----

    /// 下载并解压包（流式下载，实时更新进度）
    async fn download_and_extract(
        &self,
        manifest: &PackageManifest,
        target_dir: &Path,
    ) -> anyhow::Result<()> {
        use futures_util::StreamExt;
        use sha2::Digest;
        use tokio::io::AsyncWriteExt;

        let download_url = crate::download::apply_download_mirror(&manifest.download_url);
        info!("下载应用包: {}", download_url);
        self.set_progress(&manifest.id, "downloading", 0, None, None);

        // 下载
        let resp = self
            .http_client
            .get(&download_url)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("下载失败: {}", e))?;

        if !resp.status().is_success() {
            return Err(anyhow::anyhow!("下载失败: HTTP {}", resp.status()));
        }

        let total = resp.content_length();
        let tmp_file = target_dir.join("package.tar.gz");

        // 流式写盘 + 计算哈希 + 更新进度
        let mut hasher = sha2::Sha256::new();
        let mut downloaded: u64 = 0;
        let mut writer = tokio::fs::File::create(&tmp_file).await?;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| anyhow::anyhow!("读取下载内容失败: {}", e))?;
            hasher.update(&chunk);
            writer.write_all(&chunk).await?;
            downloaded += chunk.len() as u64;
            self.set_progress(&manifest.id, "downloading", downloaded, total, None);
        }
        writer.flush().await?;
        drop(writer);

        // SHA256 校验：manifest 未提供（None 或空串）时跳过
        if let Some(expected_sha) = &manifest.sha256 {
            let expected_sha = expected_sha.trim();
            if !expected_sha.is_empty() {
                let actual_sha = format!("{:x}", hasher.finalize());
                if actual_sha != expected_sha {
                    return Err(anyhow::anyhow!(
                        "SHA256 校验失败: 期望={}, 实际={}",
                        expected_sha,
                        actual_sha
                    ));
                }
                info!("SHA256 校验通过");
            }
        }

        // 解压（tar.gz）
        self.set_progress(&manifest.id, "extracting", downloaded, total, None);
        let tmp_file_clone = tmp_file.clone();
        let target_dir_clone = target_dir.to_path_buf();
        let result = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let file = std::fs::File::open(&tmp_file_clone)?;
            let gz = flate2::read::GzDecoder::new(file);
            let mut archive = tar::Archive::new(gz);
            archive.unpack(&target_dir_clone)?;
            Ok(())
        })
        .await;

        // 删除临时文件
        let _ = tokio::fs::remove_file(&tmp_file).await;

        result.map_err(|e| anyhow::anyhow!("解压失败: {}", e))??;

        info!("应用包解压完成: {:?}", target_dir);
        Ok(())
    }

    /// 构建 AgentConfig
    fn build_agent_config(
        &self,
        manifest: &PackageManifest,
        install_dir: &Path,
        data_dir: &Path,
        port: u16,
    ) -> AgentConfig {
        AgentConfig {
            id: manifest.id.clone(),
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            port,
            entrypoint: manifest.entrypoint.clone(),
            install_dir: install_dir.to_path_buf(),
            data_dir: data_dir.to_path_buf(),
            env: manifest.env.clone(),
            memory_limit: manifest.memory_limit,
            cpu_limit: manifest.cpu_limit,
            health_check_path: manifest.health_check_path.clone(),
            startup_timeout: manifest.startup_timeout,
            shutdown_timeout: manifest.shutdown_timeout,
        }
    }

    /// 分配端口
    async fn allocate_port(&self, preferred: u16) -> anyhow::Result<u16> {
        if preferred == 0 {
            // 自动分配：从 9000 开始找空闲端口
            for port in 9000..10000 {
                if self.is_port_free(port).await {
                    return Ok(port);
                }
            }
            return Err(anyhow::anyhow!("无法分配空闲端口"));
        }
        if self.is_port_free(preferred).await {
            Ok(preferred)
        } else {
            Err(anyhow::anyhow!("端口已被占用: {}", preferred))
        }
    }

    /// 检查端口是否空闲
    async fn is_port_free(&self, port: u16) -> bool {
        tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .is_ok()
    }

    /// 等待健康检查通过
    async fn wait_for_health(&self, id: &str, timeout_secs: u64) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
        while std::time::Instant::now() < deadline {
            // 从 AgentManager 获取健康状态
            let agents = self.agent_manager.list().await;
            if let Some(info) = agents.iter().find(|a| a.id == id) {
                if info.healthy {
                    return true;
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        false
    }
}

/// 已安装应用信息（API 响应用）
#[derive(Debug, Clone, serde::Serialize)]
pub struct InstalledAppInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub active_color: String,
    pub port: u16,
    pub status: String,
}
