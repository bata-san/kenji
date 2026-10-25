use std::sync::{Arc, RwLock};
use std::time::Duration;

use serde::Serialize;
use sysinfo::{Disks, System};

use crate::jobs::JobManager;

#[derive(Debug, Clone, Serialize, Default)]
pub struct GpuInfo {
    pub name: String,
    pub utilization_percent: f32,
    pub memory_total_bytes: u64,
    pub memory_used_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CpuInfo {
    pub cores: usize,
    pub usage_percent: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemInfo {
    pub total_bytes: u64,
    pub used_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiskInfo {
    pub label: String,
    pub mount_point: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Overview {
    pub hostname: String,
    pub os: String,
    pub version: String,
    pub uptime_secs: u64,
    pub cpu: CpuInfo,
    pub memory: MemInfo,
    pub disks: Vec<DiskInfo>,
    pub gpus: Vec<GpuInfo>,
}

pub struct MetricsCache {
    pub value: RwLock<Overview>,
}

impl MetricsCache {
    pub fn new() -> Arc<MetricsCache> {
        let initial = Overview {
            hostname: hostname(),
            os: os_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            uptime_secs: 0,
            cpu: CpuInfo { cores: 0, usage_percent: 0.0 },
            memory: MemInfo { total_bytes: 0, used_bytes: 0 },
            disks: Vec::new(),
            gpus: Vec::new(),
        };
        Arc::new(MetricsCache { value: RwLock::new(initial) })
    }

    pub fn snapshot(&self) -> Overview {
        self.value.read().unwrap().clone()
    }
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".into())
}

fn os_string() -> String {
    System::long_os_version().unwrap_or_else(|| "Windows".into())
}

pub fn start_poller(cache: Arc<MetricsCache>, _jobs: Arc<JobManager>) {
    tokio::spawn(async move {
        let mut sys = System::new_all();
        let nvml = nvml_wrapper::Nvml::init().ok();
        let mut tick: u64 = 0;
        loop {
            sys.refresh_cpu();
            sys.refresh_memory();
            let hostname = hostname();
            let os = os_string();
            let uptime = System::uptime();
            let cpu = CpuInfo {
                cores: sys.cpus().len(),
                usage_percent: sys.global_cpu_info().cpu_usage(),
            };
            let memory = MemInfo {
                total_bytes: sys.total_memory(),
                used_bytes: sys.used_memory(),
            };
            let disks: Vec<DiskInfo> = if tick % 10 == 0 {
                let d = Disks::new_with_refreshed_list();
                d.list()
                    .iter()
                    .map(|disk| DiskInfo {
                        label: disk.mount_point().to_string_lossy().to_string(),
                        mount_point: disk.mount_point().to_string_lossy().to_string(),
                        total_bytes: disk.total_space(),
                        available_bytes: disk.available_space(),
                    })
                    .filter(|d| d.total_bytes > 0)
                    .collect()
            } else {
                let cur = cache.value.read().unwrap();
                cur.disks.clone()
            };
            let gpus = collect_gpus(nvml.as_ref());
            let v = Overview {
                hostname,
                os,
                version: env!("CARGO_PKG_VERSION").to_string(),
                uptime_secs: uptime,
                cpu,
                memory,
                disks,
                gpus,
            };
            *cache.value.write().unwrap() = v;
            tick += 1;
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}

fn collect_gpus(nvml: Option<&nvml_wrapper::Nvml>) -> Vec<GpuInfo> {
    let Some(nvml) = nvml else { return Vec::new() };
    let mut out = Vec::new();
    let Ok(count) = nvml.device_count() else { return out };
    for i in 0..count {
        let Ok(dev) = nvml.device_by_index(i) else { continue };
        let (Ok(name), Ok(util), Ok(mem)) = (dev.name(), dev.utilization_rates(), dev.memory_info()) else {
            continue;
        };
        out.push(GpuInfo {
            name,
            utilization_percent: util.gpu as f32,
            memory_total_bytes: mem.total,
            memory_used_bytes: mem.used,
        });
    }
    out
}
