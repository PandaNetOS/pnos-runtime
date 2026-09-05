//! 系统监控服务

use sysinfo::{Disks, System};

use pnos::system::{DiskInfo, NetworkStats, SystemInfo, SystemStats};

pub struct MonitorService {
    sys: std::sync::Mutex<System>,
    hostname: String,
}

impl MonitorService {
    pub fn new() -> Self {
        let mut sys = System::new_all();
        sys.refresh_all();
        let hostname = System::host_name().unwrap_or_else(|| "pnos".to_string());
        MonitorService {
            sys: std::sync::Mutex::new(sys),
            hostname,
        }
    }

    /// 获取系统信息
    pub fn get_system_info(&self) -> SystemInfo {
        let sys = self.sys.lock().unwrap();
        SystemInfo {
            hostname: self.hostname.clone(),
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
        }
    }

    /// 获取实时监控数据
    pub fn get_stats(&self) -> SystemStats {
        let mut sys = self.sys.lock().unwrap();
        sys.refresh_all();

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

        let disks = Disks::new_with_refreshed_list();
        let disks_info: Vec<DiskInfo> = disks
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
            .collect();

        let load_avg = System::load_average();

        SystemStats {
            cpu_usage,
            cpu_per_core,
            memory_total: total_memory,
            memory_used: used_memory,
            memory_usage,
            swap_total: sys.total_swap(),
            swap_used: sys.used_swap(),
            disks: disks_info,
            network: NetworkStats::default(),
            load_average: [
                load_avg.one as f32,
                load_avg.five as f32,
                load_avg.fifteen as f32,
            ],
            process_count: sys.processes().len() as u32,
        }
    }
}

impl Default for MonitorService {
    fn default() -> Self {
        Self::new()
    }
}
