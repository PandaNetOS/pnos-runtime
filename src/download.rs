//! 应用下载镜像支持
//!
//! GitHub Release 资产（`github.com/.../releases/download/...`）在国内直连不稳，
//! 默认经 `ghfast.top` 镜像加速，与商店源（`default_store_url`）保持一致。
//!
//! 通过环境变量 `PNOS_DOWNLOAD_MIRROR` 覆盖：
//!   - 未设置：使用默认镜像 `https://ghfast.top/`
//!   - 设为空字符串：直连（不做任何改写）
//!   - 设为镜像前缀（须以 `/` 结尾）：前缀拼接，如 `https://gh-proxy.com/`
//!
//! 仅改写 `https://github.com/` 开头的地址，其余（自建源等）保持原样。

/// 默认下载镜像前缀（与商店源一致）。
const DEFAULT_DOWNLOAD_MIRROR: &str = "https://ghfast.top/";

/// 当前生效的下载镜像前缀；`None` 表示直连。
pub fn download_mirror() -> Option<String> {
    match std::env::var("PNOS_DOWNLOAD_MIRROR") {
        // 显式置空 → 直连
        Ok(v) if v.trim().is_empty() => None,
        Ok(v) => Some(v.trim().to_string()),
        Err(_) => Some(DEFAULT_DOWNLOAD_MIRROR.to_string()),
    }
}

/// 将 GitHub 下载地址按镜像前缀改写。
///
/// 仅处理 `https://github.com/` 前缀；镜像前缀直接拼在完整 URL 之前
/// （如 `https://ghfast.top/https://github.com/...`）。
pub fn apply_download_mirror(url: &str) -> String {
    match download_mirror() {
        Some(mirror) if url.starts_with("https://github.com/") => {
            let mirrored = format!("{}{}", mirror, url);
            tracing::info!("下载经镜像加速: {} -> {}", url, mirrored);
            mirrored
        }
        _ => url.to_string(),
    }
}
