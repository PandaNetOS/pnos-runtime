//! 工作目录规范（运行时侧实现）
//!
//! 目录布局与命名遵循生态规范（见根 `AGENTS.md`「统一 Agent 工作目录规范」）：
//!
//! ```text
//! <root>/
//! ├── config/config.yaml
//! ├── data/<name>.db
//! ├── data/node_id
//! └── logs/{stdout,stderr,crash}.log
//! ```
//!
//! 说明：规范要求统一使用 `pnos::workdir::WorkDir`，但 pnos-spec 当前（含 pinned rev）
//! **尚未提供该模块**；这里先落地同语义实现，待 pnos-spec 补齐后改为直接依赖并删除本模块。
//!
//! 本模块按规范提供**完整 API**（含尚未接入的 cache/tmp 与各类日志路径），
//! 以免调用方再次散落路径拼接；未接入的方法统一在此豁免 dead_code。
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// 运行时数据根目录下的子目录名
const CONFIG_DIR: &str = "config";
const DATA_DIR: &str = "data";
const LOGS_DIR: &str = "logs";
const CACHE_DIR: &str = "cache";
const TMP_DIR: &str = "tmp";

/// 工作目录（由根目录派生全部标准路径）
#[derive(Debug, Clone)]
pub struct WorkDir {
    root: PathBuf,
}

impl WorkDir {
    /// 以指定根目录构造
    pub fn new(root: impl Into<PathBuf>) -> Self {
        WorkDir { root: root.into() }
    }

    /// 自动探测工作目录根：
    /// - 设置 `PNOS_APP_ID` → 应用商店托管模式 `<data_root>/apps/<app_id>`
    /// - 否则 → Standalone 模式 `<work_dir>/<app_name>`（`PNOS_WORK_DIR` 可指定 work_dir）
    pub fn auto_detect(app_name: &str) -> Self {
        if let Ok(app_id) = std::env::var("PNOS_APP_ID") {
            if !app_id.trim().is_empty() {
                let data_root =
                    std::env::var("PNOS_DATA_DIR").unwrap_or_else(|_| DATA_DIR.to_string());
                return WorkDir::new(Path::new(&data_root).join("apps").join(app_id));
            }
        }
        let work_dir = std::env::var("PNOS_WORK_DIR").unwrap_or_else(|_| ".".to_string());
        WorkDir::new(Path::new(&work_dir).join(app_name))
    }

    /// 根目录
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn config_dir(&self) -> PathBuf {
        self.root.join(CONFIG_DIR)
    }

    pub fn data_dir(&self) -> PathBuf {
        self.root.join(DATA_DIR)
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root.join(LOGS_DIR)
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.root.join(CACHE_DIR)
    }

    pub fn tmp_dir(&self) -> PathBuf {
        self.root.join(TMP_DIR)
    }

    /// `<root>/config/config.yaml`
    pub fn config_file(&self) -> PathBuf {
        self.config_dir().join("config.yaml")
    }

    /// `<root>/data/<name>.db`
    pub fn db_file(&self, name: &str) -> PathBuf {
        self.data_dir().join(format!("{name}.db"))
    }

    /// `<root>/data/node_id`
    pub fn node_id_file(&self) -> PathBuf {
        self.data_dir().join("node_id")
    }

    /// `<root>/logs/stdout.log`
    pub fn stdout_log(&self) -> PathBuf {
        self.logs_dir().join("stdout.log")
    }

    /// `<root>/logs/stderr.log`
    pub fn stderr_log(&self) -> PathBuf {
        self.logs_dir().join("stderr.log")
    }

    /// `<root>/logs/crash.log`
    pub fn crash_log(&self) -> PathBuf {
        self.logs_dir().join("crash.log")
    }

    /// 创建最小可运行目录（config / data / logs）
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        for dir in [self.config_dir(), self.data_dir(), self.logs_dir()] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }

    /// 创建全部目录（含可选的 cache / tmp）
    pub fn ensure_extended_dirs(&self) -> std::io::Result<()> {
        self.ensure_dirs()?;
        for dir in [self.cache_dir(), self.tmp_dir()] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_paths_follow_spec() {
        let wd = WorkDir::new(std::env::temp_dir().join("pnos-workdir-test"));
        assert!(wd.config_file().ends_with("config/config.yaml"));
        assert!(wd.db_file("pdc").ends_with("data/pdc.db"));
        assert!(wd.node_id_file().ends_with("data/node_id"));
        assert!(wd.stdout_log().ends_with("logs/stdout.log"));
        assert!(wd.stderr_log().ends_with("logs/stderr.log"));
        assert!(wd.crash_log().ends_with("logs/crash.log"));
    }

    #[test]
    fn ensure_dirs_creates_minimal_layout() {
        let base = std::env::temp_dir().join(format!("pnos-wd-{}", std::process::id()));
        let wd = WorkDir::new(&base);
        wd.ensure_dirs().expect("ensure_dirs");
        assert!(wd.config_dir().is_dir());
        assert!(wd.data_dir().is_dir());
        assert!(wd.logs_dir().is_dir());
        std::fs::remove_dir_all(&base).ok();
    }
}
