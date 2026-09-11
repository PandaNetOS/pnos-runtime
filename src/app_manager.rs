//! 应用管理器
//!
//! 负责应用的下载、安装、启动、停止、重启。
//! 应用以独立子进程方式运行，由 pnos-runtime 管理生命周期。

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::Stdio;

use flate2::read::GzDecoder;
use tar::Archive;
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

        let binary_path = app_dir.join(&manifest.binary.binary_name);
        if !binary_path.exists() {
            let download_url =
                crate::download::apply_download_mirror(&manifest.binary.download_url);
            info!("下载应用 {}: {}", manifest.id, download_url);

            let resp = match reqwest::get(&download_url).await {
                Ok(r) => r,
                Err(e) => {
                    error!("下载失败 {}: {}", manifest.id, e);
                    anyhow::bail!("下载失败: {}", e);
                }
            };

            if !resp.status().is_success() {
                error!("下载 HTTP 错误 {}: {}", manifest.id, resp.status());
                anyhow::bail!("下载失败: HTTP {}", resp.status());
            }

            let bytes = match resp.bytes().await {
                Ok(b) => b,
                Err(e) => {
                    error!("读取下载内容失败 {}: {}", manifest.id, e);
                    anyhow::bail!("读取下载内容失败: {}", e);
                }
            };

            // 校验 SHA256
            if let Some(expected) = &manifest.binary.sha256 {
                if !expected.is_empty() {
                    use sha2::{Digest, Sha256};
                    let mut hasher = Sha256::new();
                    hasher.update(&bytes);
                    let actual = format!("{:x}", hasher.finalize());
                    if &actual != expected {
                        error!(
                            "SHA256 校验失败 {}: 期望={}, 实际={}",
                            manifest.id, expected, actual
                        );
                        anyhow::bail!("SHA256 校验失败: 期望={}, 实际={}", expected, actual);
                    }
                }
            }

            // 解压 tar.gz 或直接写入
            let binary_data = if manifest.binary.download_url.ends_with(".tar.gz")
                || manifest.binary.download_url.ends_with(".tgz")
            {
                info!("解压 tar.gz: {}", manifest.id);
                let decoder = GzDecoder::new(&bytes[..]);
                let mut archive = Archive::new(decoder);
                let mut binary_data: Option<Vec<u8>> = None;
                let entries = match archive.entries() {
                    Ok(e) => e,
                    Err(e) => {
                        error!("读取 tar.gz 条目失败 {}: {}", manifest.id, e);
                        anyhow::bail!("解压失败: {}", e);
                    }
                };
                for entry in entries {
                    let mut entry = match entry {
                        Ok(e) => e,
                        Err(e) => {
                            error!("tar.gz 条目错误 {}: {}", manifest.id, e);
                            continue;
                        }
                    };
                    let path = match entry.path() {
                        Ok(p) => p.into_owned(),
                        Err(_) => continue,
                    };
                    let is_target = path
                        .file_name()
                        .map(|n| n == manifest.binary.binary_name.as_str())
                        .unwrap_or(false);
                    if is_target {
                        let mut buf = Vec::new();
                        if let Err(e) = entry.read_to_end(&mut buf) {
                            error!("读取 tar.gz 内文件失败 {}: {}", manifest.id, e);
                            anyhow::bail!("解压失败: {}", e);
                        }
                        binary_data = Some(buf);
                        break;
                    }
                }
                match binary_data {
                    Some(d) => d,
                    None => {
                        error!("tar.gz 中找不到二进制文件 {}", manifest.binary.binary_name);
                        anyhow::bail!("tar.gz 中找不到二进制文件 {}", manifest.binary.binary_name);
                    }
                }
            } else {
                bytes.to_vec()
            };

            // 保存二进制
            if let Err(e) = tokio::fs::write(&binary_path, &binary_data).await {
                error!("写入二进制失败 {}: {}", manifest.id, e);
                anyhow::bail!("写入二进制失败: {}", e);
            }

            // 加执行权限（unix）
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let perms = std::fs::Permissions::from_mode(0o755);
                let _ = tokio::fs::set_permissions(&binary_path, perms).await;
            }
        }

        // 保存 app.yml
        let manifest_path = app_dir.join("app.yml");
        let yaml = serde_yaml::to_string(manifest)?;
        tokio::fs::write(manifest_path, yaml).await?;

        info!("应用安装完成: {} -> {:?}", manifest.id, binary_path);
        Ok(())
    }

    /// 启动应用
    pub async fn start(&self, manifest: &AppManifest) -> anyhow::Result<()> {
        let app_dir = self.app_dir(&manifest.id);
        let binary_path = app_dir.join(&manifest.binary.binary_name);

        if !binary_path.exists() {
            error!("二进制文件不存在: {:?}", binary_path);
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
        cmd.env(
            "PNOS_RUNTIME_URL",
            format!("http://127.0.0.1:{}", self.config.port),
        )
        .env("PNOS_APP_ID", &manifest.id)
        .env("PNOS_DATA_DIR", &self.config.data_dir)
        .env("PNOS_MEDIA_DIR", &self.config.media_dir);

        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                error!("启动应用失败 {}: {}", manifest.id, e);
                anyhow::bail!("启动失败: {}", e);
            }
        };
        info!("应用启动: {} (pid={:?})", manifest.id, child.id());

        self.processes.write().await.insert(
            manifest.id.clone(),
            RunningApp {
                process: child,
                manifest: manifest.clone(),
            },
        );

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
