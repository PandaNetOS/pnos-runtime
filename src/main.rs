//! pnos-runtime 入口

mod agent;
mod api;
mod app_manager;
mod config;
mod download;
mod install;
mod metrics;
mod proxy;
mod rate_limit;
mod registry;
mod service;
mod shutdown;
mod workdir;
mod ws;

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use axum::extract::Request;
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use axum::{routing::get, Router};
use tower_http::compression::CompressionLayer;
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};
use tracing::info;

use crate::config::AppState;

/// pnos-web 静态文件根目录的**相对**默认值（相对数据目录），
/// 生产容器用 `PNOS_WEB_DIR` 指向镜像里的静态目录
const WEB_DIR_SUBDIR: &str = "web";

/// 默认监听地址：0.0.0.0 允许内网其他机器访问；
/// 本机预览/CI 可设环境变量 PNOS_BIND_ADDR=127.0.0.1 避免防火墙弹窗
const DEFAULT_BIND_ADDR: &str = "0.0.0.0";

/// 内置商店源：国内可达的 GitHub 代理（可通过配置文件 default_store_url 或
/// PNOS_STORE_URL 环境变量覆盖，例如换用其它镜像或直连）
const DEFAULT_STORE_URL: &str =
    "https://ghfast.top/https://raw.githubusercontent.com/PandaNetOS/pnos-store/main/index.json";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 初始化日志
    pnos::logging::init_logging_pretty("info");
    info!("pnos-runtime 启动中...");
    // 关闭广播：所有后台循环统一订阅，收到信号后在循环边界退出
    shutdown::init();

    // 启动参数：--config 优先于一切；--work-dir 优先于 PNOS_DATA_DIR
    let cli = config::CliArgs::from_env_args();
    if let Some(path) = &cli.config {
        std::env::set_var("PNOS_CONFIG", path);
    }

    // 加载配置
    let mut config = pnos::config::PnosConfig::load()?;
    // 运维可调：环境变量覆盖端口与数据目录（测试拉起独立实例 / 容器部署）
    if let Ok(p) = std::env::var("PNOS_PORT") {
        if let Ok(p) = p.parse::<u16>() {
            config.port = p;
        }
    }
    if let Ok(d) = std::env::var("PNOS_DATA_DIR") {
        config.data_dir = d.clone();
    }
    if let Some(dir) = &cli.work_dir {
        config.data_dir = dir.clone();
    }
    info!(
        "启动参数: config={:?}, work-dir={:?}",
        cli.config, cli.work_dir
    );
    // 目录骨架（config/data/logs）：启动即创建，避免运行期写入失败
    config::ensure_dirs(&config)?;
    // 静态文件目录：PNOS_WEB_DIR 优先，默认 <data_dir>/web
    // （在 config 被 AppState 接管前解析，避免后续借用已移动的值）
    let web_dir = std::env::var("PNOS_WEB_DIR")
        .unwrap_or_else(|_| format!("{}/{}", config.data_dir, WEB_DIR_SUBDIR));
    info!("pnos-web 静态文件目录: {}", web_dir);
    // 商店源优先级：PNOS_STORE_URL 环境变量 > 配置文件 default_store_url > 内置国内镜像。
    // 内置镜像默认走 ghfast.top（pnos-spec 默认的 raw.githubusercontent.com 国内不通），
    // 但代理站会限流/抖动，运维需要能换源而不必重新编译。
    if config.default_store_url == pnos::config::PnosConfig::default().default_store_url {
        config.default_store_url = DEFAULT_STORE_URL.to_string();
    }
    if let Ok(url) = std::env::var("PNOS_STORE_URL") {
        config.default_store_url = url;
    }
    // 应用安装目录跟随 data_dir（pnos-spec 默认 /pnos/data/apps 在 Windows 上会解析到盘根目录）
    config.app_data_dir = format!("{}/apps", config.data_dir);
    info!(
        "配置加载完成: 端口={}, 数据目录={}, 应用目录={}, 商店源={}",
        config.port, config.data_dir, config.app_data_dir, config.default_store_url
    );

    // 初始化应用状态
    let state = AppState::new(config).await?;
    let state = Arc::new(state);

    // 启动 Agent 监控循环（崩溃检测 + 健康检查 + 自动重启）
    state.agent_manager.clone().start_monitor(state.clone());

    // 商店同步依赖外部网络，不能阻塞控制面监听。服务先就绪，目录在后台刷新。
    let store_service = state.store_service.clone();
    tokio::spawn(async move {
        let start = std::time::Instant::now();
        if let Err(e) = store_service.refresh_all().await {
            tracing::warn!("商店刷新失败（将使用缓存）: {}", e);
        } else {
            if let Some(m) = crate::metrics::global() {
                m.set_store_refresh_ms(start.elapsed().as_millis() as u64);
            }
        }
        if let Some(m) = crate::metrics::global() {
            m.mark_task("store_refresh");
        }
    });

    let port = state.config.port;

    // 过载保护初始化（令牌桶 + 并发信号量）
    rate_limit::init(rate_limit::BURST, rate_limit::RATE);

    // API 路由层：埋点 + 限流 + 超时(30s) + 并发上限(1024) + body 限制(16MB)
    let api_routes = api::routes(state.clone())
        .layer(axum::middleware::from_fn(metrics::metrics_middleware))
        .layer(axum::middleware::from_fn(rate_limit::rate_limit_middleware))
        .layer(axum::middleware::from_fn(rate_limit::timeout_middleware))
        .layer(axum::middleware::from_fn(
            rate_limit::concurrency_middleware,
        ))
        .layer(axum::extract::DefaultBodyLimit::max(16 * 1024 * 1024));

    // 反向代理体量大：单独放宽超时（120s），并解除 axum 默认 2MB body 限制
    // （全量缓冲的 OOM 风险已在性能 SLO 文档 P9 标注，待后续流式转发修复）。
    // 反向代理前缀统一定义在 pnos-spec 的 protocol::APP_PROXY_PREFIX：
    // 路由注册与 /system/config 对外展示共用同一个值，避免路由、界面、文档三处漂移。
    let proxy_prefix = pnos::protocol::APP_PROXY_PREFIX;
    // 构建路由
    let app = Router::new()
        .nest("/api/v1", api_routes)
        .route(
            &format!("{}/:id", proxy_prefix),
            axum::routing::any(proxy::proxy_root_handler),
        )
        .route(
            &format!("{}/:id/*path", proxy_prefix),
            axum::routing::any(proxy::proxy_handler),
        )
        .layer(axum::middleware::from_fn(
            rate_limit::proxy_timeout_middleware,
        ))
        .layer(axum::extract::DefaultBodyLimit::disable())
        // WebSocket 事件端点：刻意放在代理超时层之外，避免长连接被 30s/120s 超时强断
        .route("/api/v1/ws", get(crate::ws::ws_handler))
        // 健康检查（liveness）：保持无超时，便于探针快速返回
        .route("/health", get(health))
        // 静态文件（pnos-web）：命中文件返回文件，否则回落 index.html（SPA 深链/刷新必需）
        .fallback_service(
            ServeDir::new(&web_dir)
                .not_found_service(ServeFile::new(Path::new(&web_dir).join("index.html"))),
        )
        .layer(axum::middleware::from_fn(static_response_policy))
        .layer(CompressionLayer::new())
        .layer(CorsLayer::permissive())
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state);

    // 监听地址可配：默认 0.0.0.0（内网可达）；本机预览/CI 建议 127.0.0.1 ——
    // 只绑回环时 Windows 防火墙不会弹"允许应用通过防火墙"提示框（避免本地反复重启被打扰）
    let bind_ip = std::env::var("PNOS_BIND_ADDR").unwrap_or_else(|_| DEFAULT_BIND_ADDR.to_string());
    let addr: SocketAddr = format!("{}:{}", bind_ip, port).parse().map_err(|e| {
        anyhow::anyhow!(
            "监听地址非法（PNOS_BIND_ADDR={} port={}）: {}",
            bind_ip,
            port,
            e
        )
    })?;
    info!("pnos-runtime 监听: http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    // Ctrl+C（容器里是 SIGTERM/SIGINT）→ 广播关闭信号，让后台循环在下一个循环边界退出
    tokio::select! {
        served = axum::serve(listener, app) => served?,
        _ = tokio::signal::ctrl_c() => {
            info!("收到关闭信号，正在停止后台任务…");
            shutdown::trigger();
            tokio::time::sleep(std::time::Duration::from_millis(config::SHUTDOWN_GRACE_MS)).await;
        }
    }

    Ok(())
}

async fn health() -> &'static str {
    "ok"
}

/// 静态响应策略（只作用于非 `/api/` 路径）：
/// - `/assets/*`：文件名自带内容 hash → 长期强缓存
/// - 其它静态文件与 SPA 入口：`no-cache`，保证发版后立刻生效、不残留旧页面
/// - `/api/*`：不参与静态回落。接口路径写错时若被回落成 `200 + HTML`，
///   排查问题会误判为"接口正常"，这里显式改回 404
async fn static_response_policy(req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let mut resp = next.run(req).await;

    if path.starts_with("/api/") {
        if resp.status() == StatusCode::OK && is_html(&resp) {
            *resp.status_mut() = StatusCode::NOT_FOUND;
        }
        return resp;
    }

    // SPA 深链归一化：`ServeDir::not_found_service` 回落 index.html 时会保留 404 状态码，
    // 而 `/dashboard`、`/apps/pk` 都是有效前端路由，必须回 200。
    // 只对"不含扩展名且不在 /assets/ 下"的路径生效，缺失的静态资源继续 404。
    let is_spa_route = !path.starts_with("/assets/")
        && !path
            .rsplit('/')
            .next()
            .is_some_and(|segment| segment.contains('.'));
    if is_spa_route && resp.status() == StatusCode::NOT_FOUND && is_html(&resp) {
        *resp.status_mut() = StatusCode::OK;
    }

    let cache = if path.starts_with("/assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    resp
}

fn is_html(resp: &Response) -> bool {
    resp.headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/html"))
}
