//! 应用管理器
//!
//! 负责应用的下载、安装、启动、停止、重启。
//! 应用以独立子进程方式运行，由 pnos-runtime 管理生命周期。

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;

use tokio::process::{Child, Command};
use tokio::sync::RwLock;
use tracing::{error, info};

use pnos::app::{AppManifest, AppStatus};

/// 运行中的应用进程
struct RunningApp {
    process: Child,
    manifest: AppManifest,
}

/// 应用管理器
pub struct AppManager {
    config: pnos::config::PnosConfig,
    processes: RwLock<HashMap<String, RunningApp>>,
}

impl AppManager {
    /// 创建应用管理器
    pub fn new(config: pnos::config::PnosConfig) -> Self {
        Self {
            config,
            processes: RwLock::new(HashMap::new()),
        }
    }

    /// 安装应用（下载二进制）
    pub async fn install(&self, manifest: &AppManifest) -> anyhow::Result<()> {
        let app_dir = self.app_dir(&manifest.id);
        tokio::fs::create_dir_all(&app_dir).await?;

        // 下载二进制
        let binary_path = app_dir.join(&manifest.binary.binary_name);
        if !binary_path.exists() {
            info!("下载应用 {}: {}", manifest.id, manifest.binary.download_url);
            let resp = reqwest::get(&manifest.binary.download_url).await?;
            let bytes = resp.bytes().await?;

            // 校验 SHA256
            if let Some(expected) = &manifest.binary.sha256 {
                use sha2::{Digest, Sha256};
                let mut hasher = Sha256::new();
                hasher.update(&bytes);
                let actual = format!("{:x}", hasher.finalize());
                if &actual != expected {
                    anyhow::bail!("SHA256 校验失败: 期望={}, 实际={}", expected, actual);
                }
            }

            // 保存文件
            tokio::fs::write(&binary_path, &bytes).await?;

            // 加执行权限
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let perms = std::fs::Permissions::from_mode(0o755);
                tokio::fs::set_permissions(&binary_path, perms).await?;
            }
        }

        // 保存 app.yml
        let manifest_path = app_dir.join("app.yml");
        let yaml = serde_yaml::to_string(manifest)?;
        tokio::fs::write(manifest_path, yaml).await?;

        info!("应用安装完成: {}", manifest.id);
        Ok(())
    }

    /// 启动应用
    pub async fn start(&self, manifest: &AppManifest) -> anyhow::Result<()> {
        let app_dir = self.app_dir(&manifest.id);
        let binary_path = app_dir.join(&manifest.binary.binary_name);

        if !binary_path.exists() {
            anyhow::bail!("二进制文件不存在: {:?}", binary_path);
        }

        let working_dir = self
            .config
            .render_vars(&manifest.run.working_dir)
            .replace("{{app_data}}", &app_dir.to_string_lossy());

        let mut cmd = Command::new(&binary_path);
        cmd.args(&manifest.run.args)
            .current_dir(&working_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // 环境变量
        for env in &manifest.run.env {
            cmd.env(&env.name, &env.value);
        }
        // 注入 pnos 环境变量
        cmd.env("PNOS_RUNTIME_URL", format!("http://127.0.0.1:{}", self.config.port))
            .env("PNOS_APP_ID", &manifest.id)
            .env("PNOS_DATA_DIR", &self.config.data_dir)
            .env("PNOS_MEDIA_DIR", &self.config.media_dir);

        let child = cmd.spawn()?;
        info!("应用启动: {} (pid={:?})", manifest.id, child.id());

        self.processes
            .write()
            .await
            .insert(manifest.id.clone(), RunningApp {
                process: child,
                manifest: manifest.clone(),
            });

        Ok(())
    }

    /// 停止应用
    pub async fn stop(&self, app_id: &str) -> anyhow::Result<()> {
        let mut processes = self.processes.write().await;
        if let Some(mut app) = processes.remove(app_id) {
            info!("停止应用: {}", app_id);
            let _ = app.process.kill().await;
            let _ = app.process.wait().await;
        }
        Ok(())
    }

    /// 获取应用状态
    pub async fn status(&self, app_id: &str) -> AppStatus {
        let mut processes = self.processes.write().await;
        if let Some(app) = processes.get_mut(app_id) {
            // 检查进程是否还活着
            if app.process.try_wait().unwrap_or(None).is_none() {
                AppStatus::Running
            } else {
                AppStatus::Stopped
            }
        } else {
            AppStatus::NotInstalled
        }
    }

    /// 列出已安装应用
    pub async fn installed_apps(&self) -> Vec<String> {
        self.processes.read().await.keys().cloned().collect()
    }

    /// 应用数据目录
    fn app_dir(&self, app_id: &str) -> PathBuf {
        PathBuf::from(&self.config.app_data_dir).join(app_id)
    }
}
