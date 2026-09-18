//! 应用商店服务
//!
//! 管理商店源，缓存应用清单，提供应用查询。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tokio::sync::RwLock;

use pnos::app::AppManifest;
use pnos::config::PnosConfig;
use pnos::error::{ErrorCode, PnosError};

/// 商店清单磁盘缓存文件名（位于运行时 data_dir 下）。
///
/// 商店源（如 ghfast.top 代理）刷新时会出现**单个** app.yml 拉取失败，
/// 缓存用于跨重启保留上一次成功获取到的清单，避免一重启商店就缺应用。
const STORE_CACHE_FILE: &str = "store-cache.json";

pub struct StoreService {
    config: PnosConfig,
    apps: RwLock<HashMap<String, AppManifest>>,
    cache_path: PathBuf,
}

impl StoreService {
    pub fn new(config: PnosConfig) -> Self {
        let cache_path = Path::new(&config.data_dir).join(STORE_CACHE_FILE);
        let apps = Self::load_cache(&cache_path);
        if !apps.is_empty() {
            tracing::info!(
                "已从磁盘缓存加载 {} 个商店应用: {}",
                apps.len(),
                cache_path.display()
            );
        }
        StoreService {
            config,
            apps: RwLock::new(apps),
            cache_path,
        }
    }

    /// 读取磁盘缓存；缓存不存在或损坏时返回空表，不影响启动
    fn load_cache(path: &Path) -> HashMap<String, AppManifest> {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str::<HashMap<String, AppManifest>>(&raw).ok())
            .unwrap_or_default()
    }

    /// 写磁盘缓存；失败只告警，不影响本次刷新结果
    fn save_cache(&self, apps: &HashMap<String, AppManifest>) {
        if let Some(dir) = self.cache_path.parent() {
            if let Err(e) = std::fs::create_dir_all(dir) {
                tracing::warn!("创建商店缓存目录失败: {}", e);
                return;
            }
        }
        match serde_json::to_string(apps) {
            Ok(raw) => {
                if let Err(e) = std::fs::write(&self.cache_path, raw) {
                    tracing::warn!("写入商店缓存失败: {}", e);
                }
            }
            Err(e) => tracing::warn!("序列化商店缓存失败: {}", e),
        }
    }

    /// 列出商店源
    pub fn list_sources(&self) -> Vec<serde_json::Value> {
        vec![serde_json::json!({
            "id": "default",
            "name": "pnos 官方商店",
            "url": self.config.default_store_url,
            "enabled": true,
        })]
    }

    /// 刷新所有商店源
    pub async fn refresh_all(&self) -> Result<(), PnosError> {
        self.refresh_source("default").await
    }

    /// 刷新指定商店源
    pub async fn refresh_source(&self, _id: &str) -> Result<(), PnosError> {
        let url = &self.config.default_store_url;
        tracing::info!("刷新商店源: {}", url);

        let resp = reqwest::get(url).await.map_err(|e| {
            PnosError::new(
                ErrorCode::StoreSourceUnreachable,
                format!("请求失败: {}", e),
            )
        })?;

        if !resp.status().is_success() {
            return Err(PnosError::new(
                ErrorCode::StoreSourceUnreachable,
                format!("HTTP {}", resp.status()),
            ));
        }

        let index: serde_json::Value = resp.json().await.map_err(|e| {
            PnosError::new(
                ErrorCode::StoreSourceUnreachable,
                format!("解析失败: {}", e),
            )
        })?;

        let apps_list = index["apps"].as_array().cloned().unwrap_or_default();
        let base_url = url.trim_end_matches("index.json");
        // 拉取前先快照上一次的清单：单个 app.yml 失败时用它兜底，
        // 避免一次网络抖动就把整个商店清空（原实现是整体替换，越刷新应用越少）
        let previous = self.apps.read().await.clone();
        let mut apps = HashMap::new();
        let mut missing: Vec<String> = Vec::new();

        for app_info in apps_list {
            if let (Some(id), Some(app_yml_path)) =
                (app_info["id"].as_str(), app_info["app_yml"].as_str())
            {
                let app_yml_url = format!("{}{}", base_url, app_yml_path);
                match self.fetch_app_manifest(&app_yml_url).await {
                    Ok(manifest) => {
                        apps.insert(id.to_string(), manifest);
                    }
                    Err(e) => match previous.get(id) {
                        Some(cached) => {
                            tracing::warn!("加载应用 {} 失败: {}（保留上次缓存清单）", id, e);
                            apps.insert(id.to_string(), cached.clone());
                        }
                        None => {
                            tracing::warn!("加载应用 {} 失败: {}（且无缓存可回退）", id, e);
                            missing.push(id.to_string());
                        }
                    },
                }
            }
        }

        let count = apps.len();
        {
            let mut guard = self.apps.write().await;
            *guard = apps.clone();
        }
        self.save_cache(&apps);
        tracing::info!(
            "商店刷新完成，共 {} 个应用{}",
            count,
            if missing.is_empty() {
                String::new()
            } else {
                format!(
                    "（{} 个未获取到清单: {}）",
                    missing.len(),
                    missing.join(", ")
                )
            }
        );
        Ok(())
    }

    async fn fetch_app_manifest(&self, url: &str) -> Result<AppManifest, PnosError> {
        let resp = reqwest::get(url)
            .await
            .map_err(|e| PnosError::External(format!("请求 app.yml 失败: {}", e)))?;
        let content = resp
            .text()
            .await
            .map_err(|e| PnosError::External(format!("读取 app.yml 失败: {}", e)))?;
        let manifest: AppManifest = serde_yaml::from_str(&content).map_err(|e| {
            PnosError::new(ErrorCode::AppManifestInvalid, format!("解析失败: {}", e))
        })?;
        Ok(manifest)
    }

    /// 获取应用清单
    pub async fn get_app_manifest(&self, id: &str) -> Option<AppManifest> {
        self.apps.read().await.get(id).cloned()
    }

    /// 列出所有应用
    pub async fn list_apps(&self) -> Vec<serde_json::Value> {
        let apps = self.apps.read().await;
        apps.values()
            .map(|m| serde_json::to_value(m).unwrap_or_default())
            .collect()
    }

    /// 获取应用详情
    pub async fn get_app(&self, id: &str) -> Option<serde_json::Value> {
        let apps = self.apps.read().await;
        apps.get(id)
            .map(|m| serde_json::to_value(m).unwrap_or_default())
    }
}
