//! 性能埋点（企业级 P8 可观测 + P1/P2/P3 可量化）
//!
//! 提供：每端点请求计数/错误计数/耗时直方图、进程内指标、业务计数、后台任务心跳。
//! 通过全局单例 `metrics::global()` 访问，由 axum 中间件自动采集，无需侵入业务 handler。
//! 新增端点 `/api/v1/metrics`（JSON，或 `?format=prom` 输出 Prometheus 文本）。

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{MatchedPath, Query, State};
use axum::http::{header::CONTENT_TYPE, Request, StatusCode};
use axum::middleware::Next;
use axum::response::Response;

use crate::config::AppState;

/// 每端点保留的样本数上限（环形缓冲，避免无界增长）
const SAMPLE_CAP: usize = 4096;

/// 单端点的统计
struct EndpointStat {
    count: u64,
    errors: u64,        // >= 400
    server_errors: u64, // >= 500
    total_us: u128,
    samples: VecDeque<f64>, // 最近若干耗时样本（微秒），用于算高分位
}

impl EndpointStat {
    fn new() -> Self {
        Self {
            count: 0,
            errors: 0,
            server_errors: 0,
            total_us: 0,
            samples: VecDeque::with_capacity(SAMPLE_CAP),
        }
    }
    fn record(&mut self, dur_us: f64, status: u16) {
        self.count += 1;
        if status >= 400 {
            self.errors += 1;
        }
        if status >= 500 {
            self.server_errors += 1;
        }
        self.total_us += dur_us as u128;
        if self.samples.len() >= SAMPLE_CAP {
            self.samples.pop_front();
        }
        self.samples.push_back(dur_us);
    }

    /// 计算分位（q ∈ [0,1]）。样本不足时返回最新值。
    fn percentile(&self, q: f64) -> f64 {
        if self.samples.is_empty() {
            return 0.0;
        }
        let mut v: Vec<f64> = self.samples.iter().copied().collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let idx = ((v.len() as f64) * q).ceil() as usize;
        let idx = idx.min(v.len() - 1);
        v[idx]
    }
}

/// 全局指标
pub struct Metrics {
    endpoints: Mutex<HashMap<String, EndpointStat>>,
    in_flight: AtomicUsize,
    agent_crashes: AtomicU64,
    agent_restarts: AtomicU64,
    store_refresh_ms: AtomicU64,                           // 0 = 尚未刷新
    task_ticks: Mutex<HashMap<String, Arc<AtomicU64>>>,    // 任务名 -> 最后成功 tick 的 epoch ms
    proc_cache: Mutex<Option<(Instant, ProcessSnapshot)>>, // 进程指标 1s 缓存
}

impl Metrics {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            endpoints: Mutex::new(HashMap::new()),
            in_flight: AtomicUsize::new(0),
            agent_crashes: AtomicU64::new(0),
            agent_restarts: AtomicU64::new(0),
            store_refresh_ms: AtomicU64::new(0),
            task_ticks: Mutex::new(HashMap::new()),
            proc_cache: Mutex::new(None),
        })
    }

    pub fn in_flight_inc(&self) {
        self.in_flight.fetch_add(1, Ordering::Relaxed);
    }
    pub fn in_flight_dec(&self) {
        self.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
    pub fn record(&self, key: &str, dur: Duration, status: u16) {
        let dur_us = dur.as_secs_f64() * 1_000_000.0;
        let mut map = self.endpoints.lock().unwrap();
        let stat = map.entry(key.to_string()).or_insert_with(EndpointStat::new);
        stat.record(dur_us, status);
    }
    pub fn incr_agent_crash(&self) {
        self.agent_crashes.fetch_add(1, Ordering::Relaxed);
    }
    pub fn incr_agent_restart(&self) {
        self.agent_restarts.fetch_add(1, Ordering::Relaxed);
    }
    pub fn set_store_refresh_ms(&self, ms: u64) {
        self.store_refresh_ms.store(ms, Ordering::Relaxed);
    }
    /// 标记后台任务仍存活（在循环体末尾调用）
    pub fn mark_task(&self, name: &str) {
        let now = now_ms();
        let mut map = self.task_ticks.lock().unwrap();
        let cell = map
            .entry(name.to_string())
            .or_insert_with(|| Arc::new(AtomicU64::new(now)));
        cell.store(now, Ordering::Relaxed);
    }
    /// 任务距上次 tick 的毫秒数（None = 从未 tick）
    pub fn task_age_ms(&self, name: &str) -> Option<u64> {
        let map = self.task_ticks.lock().unwrap();
        map.get(name).map(|c| {
            let now = now_ms();
            now.saturating_sub(c.load(Ordering::Relaxed))
        })
    }

    fn proc_snapshot(&self) -> ProcessSnapshot {
        // 1s 缓存，避免每次请求都刷 sysinfo
        {
            let cache = self.proc_cache.lock().unwrap();
            if let Some((ts, snap)) = cache.as_ref() {
                if ts.elapsed() < Duration::from_secs(1) {
                    return snap.clone();
                }
            }
        }
        let snap = collect_process();
        let mut cache = self.proc_cache.lock().unwrap();
        *cache = Some((Instant::now(), snap.clone()));
        snap
    }
}

#[derive(Clone)]
struct ProcessSnapshot {
    pid: u32,
    cpu_percent: f32,
    memory_bytes: u64,
    thread_count: u64,
    open_files: i64, // -1 = 未知（Windows 不支持）
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 读取当前进程资源占用
fn collect_process() -> ProcessSnapshot {
    use sysinfo::{Pid, System};
    let pid = std::process::id();
    let mut sys = System::new_all();
    sys.refresh_processes();
    let mut snap = ProcessSnapshot {
        pid,
        cpu_percent: 0.0,
        memory_bytes: 0,
        thread_count: 0,
        open_files: -1,
    };
    if let Some(p) = sys.process(Pid::from_u32(pid)) {
        snap.cpu_percent = p.cpu_usage();
        snap.memory_bytes = p.memory();
    }
    snap
}

// ---------------- 全局单例 ----------------

static GLOBAL: OnceLock<Arc<Metrics>> = OnceLock::new();

/// 初始化全局指标（应在 AppState::new 中调用一次）
pub fn init_global(m: Arc<Metrics>) {
    let _ = GLOBAL.set(m);
}

/// 获取全局指标（中间件/后台任务用）
pub fn global() -> Option<&'static Arc<Metrics>> {
    GLOBAL.get()
}

// ---------------- 中间件 ----------------

/// axum 中间件：自动记录每个请求的耗时与状态
pub async fn metrics_middleware(req: Request<axum::body::Body>, next: Next) -> Response {
    let Some(m) = global() else {
        return next.run(req).await;
    };
    let key = match req.extensions().get::<MatchedPath>() {
        Some(mp) => format!("{} {}", req.method(), mp.as_str()),
        None => format!("{} {}", req.method(), req.uri().path()),
    };
    let start = Instant::now();
    m.in_flight_inc();
    let resp = next.run(req).await;
    m.in_flight_dec();
    m.record(&key, start.elapsed(), resp.status().as_u16());
    resp
}

// ---------------- /api/v1/metrics 端点 ----------------

#[derive(serde::Deserialize)]
pub struct MetricsQuery {
    pub format: Option<String>,
}

/// 返回内部指标快照（JSON 或 Prometheus 文本）
pub async fn get_metrics(
    State(state): State<Arc<AppState>>,
    Query(q): Query<MetricsQuery>,
) -> Response {
    let m = match global() {
        Some(m) => m,
        None => {
            return Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(axum::body::Body::from("metrics not initialized"))
                .unwrap()
        }
    };

    // 端点直方图
    let endpoints = {
        let map = m.endpoints.lock().unwrap();
        let mut out = serde_json::Map::new();
        for (k, stat) in map.iter() {
            out.insert(
                k.clone(),
                serde_json::json!({
                    "count": stat.count,
                    "errors": stat.errors,
                    "server_errors": stat.server_errors,
                    "avg_ms": if stat.count > 0 { stat.total_us as f64 / stat.count as f64 / 1000.0 } else { 0.0 },
                    "p50_ms": stat.percentile(0.50) / 1000.0,
                    "p95_ms": stat.percentile(0.95) / 1000.0,
                    "p99_ms": stat.percentile(0.99) / 1000.0,
                    "p999_ms": stat.percentile(0.999) / 1000.0,
                }),
            );
        }
        out
    };

    // 业务指标
    let components = state.registry.list().await;
    let total = components.len();
    let offline = components
        .iter()
        .filter(|c| matches!(c.status, pnos::component::ComponentStatus::Offline))
        .count();

    // 任务心跳
    let tasks = serde_json::json!({
        "monitor": m.task_age_ms("monitor").unwrap_or(u64::MAX),
        "heartbeat_checker": m.task_age_ms("heartbeat_checker").unwrap_or(u64::MAX),
        "store_refresh": m.task_age_ms("store_refresh").unwrap_or(u64::MAX),
    });

    let proc = m.proc_snapshot();

    let snapshot = serde_json::json!({
        "endpoints": endpoints,
        "in_flight": m.in_flight.load(Ordering::Relaxed),
        "process": {
            "pid": proc.pid,
            "cpu_percent": proc.cpu_percent,
            "memory_bytes": proc.memory_bytes,
            "thread_count": proc.thread_count,
            "open_files": proc.open_files,
        },
        "business": {
            "components": total,
            "offline": offline,
            "agent_crashes": m.agent_crashes.load(Ordering::Relaxed),
            "agent_restarts": m.agent_restarts.load(Ordering::Relaxed),
            "store_refresh_ms": m.store_refresh_ms.load(Ordering::Relaxed),
        },
        "tasks": tasks,
    });

    if q.format.as_deref() == Some("prom") {
        let text = to_prometheus(&snapshot);
        Response::builder()
            .header(CONTENT_TYPE, "text/plain; version=0.0.4")
            .body(axum::body::Body::from(text))
            .unwrap()
    } else {
        Response::builder()
            .header(CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(
                serde_json::to_string(&snapshot).unwrap_or_default(),
            ))
            .unwrap()
    }
}

/// 将 JSON 快照转换为 Prometheus 文本格式
fn to_prometheus(snap: &serde_json::Value) -> String {
    let mut out = String::new();
    // 端点指标
    if let Some(eps) = snap.get("endpoints").and_then(|v| v.as_object()) {
        for (ep, stat) in eps {
            let labels = format!("{{endpoint=\"{}\"}}", ep.replace('\"', "\\\""));
            let count = stat.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
            let errs = stat.get("errors").and_then(|v| v.as_u64()).unwrap_or(0);
            let p50 = stat.get("p50_ms").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let p99 = stat.get("p99_ms").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let p999 = stat.get("p999_ms").and_then(|v| v.as_f64()).unwrap_or(0.0);
            out.push_str(&format!("pnos_http_requests_total{} {}\n", labels, count));
            out.push_str(&format!("pnos_http_errors_total{} {}\n", labels, errs));
            out.push_str(&format!("pnos_http_duration_ms{{}}{} {}\n", labels, p50));
            out.push_str(&format!(
                "pnos_http_duration_p99_ms{{}}{} {}\n",
                labels, p99
            ));
            out.push_str(&format!(
                "pnos_http_duration_p999_ms{{}}{} {}\n",
                labels, p999
            ));
        }
    }
    if let Some(b) = snap.get("business") {
        let comp = b.get("components").and_then(|v| v.as_u64()).unwrap_or(0);
        let off = b.get("offline").and_then(|v| v.as_u64()).unwrap_or(0);
        let crashes = b.get("agent_crashes").and_then(|v| v.as_u64()).unwrap_or(0);
        let restarts = b
            .get("agent_restarts")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        out.push_str(&format!("pnos_components_total {}\n", comp));
        out.push_str(&format!("pnos_components_offline {}\n", off));
        out.push_str(&format!("pnos_agent_crashes_total {}\n", crashes));
        out.push_str(&format!("pnos_agent_restarts_total {}\n", restarts));
    }
    if let Some(p) = snap.get("process") {
        let mem = p.get("memory_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
        let cpu = p.get("cpu_percent").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let th = p.get("thread_count").and_then(|v| v.as_u64()).unwrap_or(0);
        out.push_str(&format!("pnos_process_resident_bytes {}\n", mem));
        out.push_str(&format!("pnos_process_cpu_percent {}\n", cpu));
        out.push_str(&format!("pnos_process_threads {}\n", th));
    }
    out
}
