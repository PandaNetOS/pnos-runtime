//! 全局过载保护（企业级 P7 过载优雅降级）
//!
//! - 限流：令牌桶，超限返回 429 + Retry-After。
//! - 超时：包裹请求，超时返回 504（API 30s / 代理 120s）。
//! - 并发上限：信号量（1024），超限返回 429，防止请求无限排队拖垮 worker。
//!
//! 均采用 axum `from_fn` 中间件实现，避免 tower layer 的错误类型与 axum
//! `Into<Infallible>` 约束冲突。

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

// ---------------- 限流（令牌桶） ----------------

struct Bucket {
    tokens: f64,
    last: Instant,
    rate: f64,  // 每秒补充速率（env 可覆盖）
    burst: f64, // 桶容量（env 可覆盖）
}

struct MutexBucket {
    inner: Mutex<Bucket>,
}

static RATE_LIMIT: OnceLock<Arc<MutexBucket>> = OnceLock::new();

pub const RATE: f64 = 20_000.0; // 每秒允许的请求数（默认，生产）
pub const BURST: f64 = 20_000.0; // 允许的最大突发（默认，生产）

fn global() -> Option<&'static Arc<MutexBucket>> {
    RATE_LIMIT.get()
}

/// 读取环境变量覆盖的 f64 配置（不存在/非法时回退默认）
fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| *v > 0.0)
        .unwrap_or(default)
}

/// 读取环境变量覆盖的并发上限（不存在/非法时回退默认）
fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

pub fn init(burst: f64, _rate: f64) {
    // 运维可调：环境变量可压低阈值（如测试时 PNOS_RATE_LIMIT_BURST=30），
    // 不设置则使用生产默认值。
    let eff_burst = env_f64("PNOS_RATE_LIMIT_BURST", burst);
    let eff_rate = env_f64("PNOS_RATE_LIMIT_RATE", _rate);
    let _ = RATE_LIMIT.set(Arc::new(MutexBucket {
        inner: Mutex::new(Bucket {
            tokens: eff_burst,
            last: Instant::now(),
            rate: eff_rate,
            burst: eff_burst,
        }),
    }));
    let limit = env_usize("PNOS_CONCURRENCY_LIMIT", CONCURRENCY_LIMIT);
    let _ = CONCURRENCY.set(Arc::new(Semaphore::new(limit)));
}

/// 限流中间件
pub async fn rate_limit_middleware(req: Request<Body>, next: Next) -> Response {
    let allow = {
        let Some(g) = global() else {
            return next.run(req).await;
        };
        let mut b = g.inner.lock().unwrap();
        let now = Instant::now();
        let elapsed = now.duration_since(b.last).as_secs_f64();
        b.last = now;
        b.tokens = (b.tokens + elapsed * b.rate).min(b.burst);
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            true
        } else {
            false
        }
    };
    if allow {
        next.run(req).await
    } else {
        (StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded").into_response()
    }
}

// ---------------- 并发上限 ----------------

static CONCURRENCY: OnceLock<Arc<Semaphore>> = OnceLock::new();
const CONCURRENCY_LIMIT: usize = 1024;

/// 并发上限中间件：超限返回 429，避免请求排队雪崩
pub async fn concurrency_middleware(req: Request<Body>, next: Next) -> Response {
    let sem = match CONCURRENCY.get() {
        Some(s) => s,
        None => return next.run(req).await,
    };
    let permit: OwnedSemaphorePermit = match Arc::clone(sem).try_acquire_owned() {
        Ok(p) => p,
        Err(_) => {
            return (StatusCode::TOO_MANY_REQUESTS, "service overloaded").into_response();
        }
    };
    let resp = next.run(req).await;
    drop(permit);
    resp
}

// ---------------- 超时 ----------------

/// 通用超时中间件（API 默认 30s）
pub async fn timeout_middleware(req: Request<Body>, next: Next) -> Response {
    match tokio::time::timeout(Duration::from_secs(30), next.run(req)).await {
        Ok(resp) => resp,
        Err(_) => StatusCode::GATEWAY_TIMEOUT.into_response(),
    }
}

/// 代理专用超时中间件（120s，容忍大文件转发）
pub async fn proxy_timeout_middleware(req: Request<Body>, next: Next) -> Response {
    match tokio::time::timeout(Duration::from_secs(120), next.run(req)).await {
        Ok(resp) => resp,
        Err(_) => StatusCode::GATEWAY_TIMEOUT.into_response(),
    }
}
