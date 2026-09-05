//! 应用商店服务
//!
//! 管理商店源，缓存应用清单，提供应用查询。

use std::collections::HashMap;

use tokio::sync::RwLock;

use pnos::app::AppManifest;
use pnos::config::PnosConfig;
use pnos::error::{ErrorCode, PnosError};

pub struct StoreService {
    config: PnosConfig,
    apps: RwLock<HashMap<String, AppManifest>>,
}

impl StoreService {
    pub fn new(config: PnosConfig) -> Self {
        StoreService {
            config,
            apps: RwLock::new(HashMap::new()),
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

        let resp = reqwest::get(url)
            .await
            .map_err(|e| PnosError::new(ErrorCode::StoreSourceUnreachable, format!("请求失败: {}", e)))?;

        if !resp.status().is_success() {
            return Err(PnosError::new(
                ErrorCode::StoreSourceUnreachable,
                format!("HTTP {}", resp.status()),
            ));
        }

        let index: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| PnosError::new(ErrorCode::StoreSourceUnreachable, format!("解析失败: {}", e)))?;

        let apps_list = index["apps"].as_array().cloned().unwrap_or_default();
        let base_url = url.trim_end_matches("index.json");
        let mut apps = HashMap::new();

        for app_info in apps_list {
            if let (Some(id), Some(app_yml_path)) = (
                app_info["id"].as_str(),
                app_info["app_yml"].as_str(),
            ) {
                let app_yml_url = format!("{}{}", base_url, app_yml_path);
                match self.fetch_app_manifest(&app_yml_url).await {
                    Ok(manifest) => {
                        apps.insert(id.to_string(), manifest);
                    }
                    Err(e) => {
                        tracing::warn!("加载应用 {} 失败: {}", id, e);
                    }
                }
            }
        }

        let mut guard = self.apps.write().await;
        *guard = apps;
        tracing::info!("商店刷新完成，共 {} 个应用", guard.len());
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
        let manifest: AppManifest = serde_yaml::from_str(&content)
            .map_err(|e| PnosError::new(ErrorCode::AppManifestInvalid, format!("解析失败: {}", e)))?;
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
