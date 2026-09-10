//! 系统监控服务
//!
//! 设计要点（修复 P0 性能缺陷）：
//! - 主机指标由后台任务按固定间隔采集，写入 `snapshot`（`Arc<RwLock<SystemStats>`）。
//! - HTTP handler 只读快照，O(1) 返回，不阻塞 tokio worker，避免 `/system/stats`
//!   把整个 runtime 拖垮（原实现每次请求同步 `sys.refresh_all()` + 重建磁盘列表，
//!   在 `std::sync::Mutex` 下串行执行，会占满 worker 线程并连坐其他接口）。
//! - 磁盘列表刷新频率远低于整体采样间隔（磁盘变化极少），通过 TTL 缓存避免每次枚举卷。
//! - 静态系统信息（主机名/OS/核心数/总内存等）在构造时预计算，避免每次请求加锁；
//!   uptime 实时返回。

use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

use sysinfo::{Disks, System};
use tokio::sync::RwLock;

use pnos::system::{DiskInfo, NetworkStats, SystemInfo, SystemStats};

/// 默认整体采样间隔（CPU/内存/进程），可用环境变量 `PNOS_MONITOR_INTERVAL_SECS` 覆盖。
const DEFAULT_INTERVAL_SECS: u64 = 2;
/// 磁盘列表刷新间隔（磁盘变化极少，无需每次采样都枚举卷）。
const DISK_TTL_SECS: u64 = 30;

pub struct MonitorService {
    /// 仅由后台采集任务持有并刷新，请求路径不再加锁访问。
    sys: StdMutex<System>,
    /// 构造时预计算的静态信息，get_system_info 直接克隆返回。
    static_info: SystemInfo,
    /// 最新一次采集结果；handler 读它，O(1) 无阻塞。
    snapshot: Arc<RwLock<SystemStats>>,
    /// 磁盘列表缓存（TTL 刷新）。
    disks_cache: StdMutex<Vec<DiskInfo>>,
    disk_refresh_at: StdMutex<Instant>,
    disk_ttl: Duration,
    interval: Duration,
}

impl MonitorService {
    pub fn new() -> Self {
        let mut sys = System::new_all();
        sys.refresh_all();

        let static_info = SystemInfo {
            hostname: System::host_name().unwrap_or_else(|| "pnos".to_string()),
            os: System::name().unwrap_or_else(|| "Linux".to_string()),
            os_version: System::os_version().unwrap_or_default(),
            kernel: System::kernel_version().unwrap_or_default(),
            arch: std::env::consts::ARCH.to_string(),
            cpu_model: sys
                .cpus()
                .first()
                .map(|c| c.brand().to_string())
                .unwrap_or_default(),
            cpu_cores: sys.cpus().len() as u32,
            memory_total: sys.total_memory(),
            uptime: System::uptime(),
            pnos_version: env!("CARGO_PKG_VERSION").to_string(),
        };

        let disks = collect_disks();
        let initial = compute_stats(&sys, &disks);

        let interval = Duration::from_secs(
            std::env::var("PNOS_MONITOR_INTERVAL_SECS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(DEFAULT_INTERVAL_SECS),
        );

        MonitorService {
            sys: StdMutex::new(sys),
            static_info,
            snapshot: Arc::new(RwLock::new(initial)),
            disks_cache: StdMutex::new(disks),
            disk_refresh_at: StdMutex::new(Instant::now()),
            disk_ttl: Duration::from_secs(DISK_TTL_SECS),
            interval,
        }
    }

    /// 启动后台采集任务。在构造后调用一次即可。
    pub fn start(self: &Arc<Self>) {
        let svc = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(svc.interval);
            loop {
                ticker.tick().await;
                let stats = svc.collect();
                *svc.snapshot.write().await = stats;
            }
        });
    }

    /// 获取系统信息（静态字段预计算，uptime 实时返回，全程无锁）。
    pub fn get_system_info(&self) -> SystemInfo {
        let mut info = self.static_info.clone();
        info.uptime = System::uptime();
        info
    }

    /// 读取最新快照。O(1)，不阻塞 tokio worker。
    pub async fn get_stats(&self) -> SystemStats {
        self.snapshot.read().await.clone()
    }

    /// 后台采集：刷新易变指标，按需刷新磁盘列表，返回一次完整快照。
    fn collect(&self) -> SystemStats {
        let need_disks = {
            let last = *self.disk_refresh_at.lock().unwrap();
            last.elapsed() >= self.disk_ttl
        };

        let mut sys = self.sys.lock().unwrap();
        sys.refresh_cpu();
        sys.refresh_memory();
        sys.refresh_processes();

        let disks_info = if need_disks {
            let d = collect_disks();
            *self.disk_refresh_at.lock().unwrap() = Instant::now();
            *self.disks_cache.lock().unwrap() = d.clone();
            d
        } else {
            self.disks_cache.lock().unwrap().clone()
        };

        compute_stats(&*sys, &disks_info)
    }
}

/// 采集磁盘列表。可能较重（枚举卷），由调用方控制频率。
fn collect_disks() -> Vec<DiskInfo> {
    let disks = Disks::new_with_refreshed_list();
    disks
        .iter()
        .map(|d| {
            let total = d.total_space();
            let available = d.available_space();
            let used = total.saturating_sub(available);
            DiskInfo {
                device: d.name().to_string_lossy().to_string(),
                mount_point: d.mount_point().to_string_lossy().to_string(),
                fs_type: d.file_system().to_string_lossy().to_string(),
                total,
                used,
                available,
                usage: if total > 0 {
                    (used as f32 / total as f32) * 100.0
                } else {
                    0.0
                },
            }
        })
        .collect()
}

/// 由已刷新的 System 与磁盘列表计算 SystemStats。
fn compute_stats(sys: &System, disks: &[DiskInfo]) -> SystemStats {
    let cpu_per_core: Vec<f32> = sys.cpus().iter().map(|c| c.cpu_usage()).collect();
    let cpu_usage = if !cpu_per_core.is_empty() {
        cpu_per_core.iter().sum::<f32>() / cpu_per_core.len() as f32
    } else {
        0.0
    };

    let total_memory = sys.total_memory();
    let used_memory = sys.used_memory();
    let memory_usage = if total_memory > 0 {
        (used_memory as f32 / total_memory as f32) * 100.0
    } else {
        0.0
    };

    let load_avg = System::load_average();

    SystemStats {
        cpu_usage,
        cpu_per_core,
        memory_total: total_memory,
        memory_used: used_memory,
        memory_usage,
        swap_total: sys.total_swap(),
        swap_used: sys.used_swap(),
        disks: disks.to_vec(),
        network: NetworkStats::default(),
        load_average: [
            load_avg.one as f32,
            load_avg.five as f32,
            load_avg.fifteen as f32,
        ],
        process_count: sys.processes().len() as u32,
    }
}

impl Default for MonitorService {
    fn default() -> Self {
        Self::new()
    }
}
