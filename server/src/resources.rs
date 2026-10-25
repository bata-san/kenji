//! OS-enforced limits for managed background workloads, never the API/UI process.
use anyhow::{Context, Result};
use serde::Serialize;
use std::sync::Mutex;

#[derive(Clone, Serialize)]
pub struct ResourceStatus {
    pub enabled: bool,
    pub supported: bool,
    pub cpu_limit_percent: u32,
    pub memory_limit_bytes: u64,
    pub scope: &'static str,
    pub error: Option<String>,
}

pub struct ResourceGuard {
    inner: Mutex<Inner>,
}
struct Inner {
    status: ResourceStatus,
    #[cfg(windows)]
    job: NativeJob,
}

impl ResourceGuard {
    pub fn new() -> Result<Self> {
        Ok(Self {
            inner: Mutex::new(Inner {
                status: ResourceStatus {
                    enabled: false,
                    supported: cfg!(windows),
                    cpu_limit_percent: 60,
                    memory_limit_bytes: memory_budget(),
                    scope: "managed_jobs",
                    error: None,
                },
                #[cfg(windows)]
                job: NativeJob::new()?,
            }),
        })
    }
    pub fn status(&self) -> ResourceStatus {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .status
            .clone()
    }
    pub fn set_enabled(&self, enabled: bool) -> Result<()> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        #[cfg(windows)]
        {
            let old = inner.status.enabled;
            let budget = inner.status.memory_limit_bytes;
            if let Err(e) = inner.job.configure(enabled, budget) {
                let _ = inner.job.configure(old, budget);
                inner.status.error = Some(format!("{e:#}"));
                return Err(e);
            }
        }
        #[cfg(not(windows))]
        if enabled {
            anyhow::bail!("PC操作優先モードはWindows専用です");
        }
        inner.status.enabled = enabled;
        inner.status.error = None;
        Ok(())
    }
    /// Always attach jobs, even when limits are off, so toggling affects existing
    /// jobs and their descendants too. Failure must fail the launch closed.
    pub fn attach(&self, pid: u32) -> Result<()> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        #[cfg(windows)]
        return inner.job.attach(pid);
        #[cfg(not(windows))]
        {
            let _ = (inner, pid);
            Ok(())
        }
    }
}

fn memory_budget() -> u64 {
    let sys = sysinfo::System::new_all();
    let total = sys.total_memory();
    // Reserve at least 4 GiB or 30%, whichever is greater. A commit limit is not
    // a guarantee of free physical RAM (other applications remain unrestricted).
    total
        .saturating_sub((4 * 1024 * 1024 * 1024).max(total * 30 / 100))
        .max(256 * 1024 * 1024)
}

#[cfg(windows)]
struct NativeJob(windows_sys::Win32::Foundation::HANDLE);
// HANDLE is owned exclusively here; all access is serialized by ResourceGuard.
#[cfg(windows)]
unsafe impl Send for NativeJob {}
#[cfg(windows)]
impl NativeJob {
    fn new() -> Result<Self> {
        use windows_sys::Win32::System::JobObjects::CreateJobObjectW;
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error()).context("Job Object作成に失敗");
        }
        let job = Self(handle);
        job.configure(false, 0)?;
        Ok(job)
    }
    fn configure(&self, enabled: bool, memory: u64) -> Result<()> {
        use windows_sys::Win32::System::JobObjects::*;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if enabled {
            limits.BasicLimitInformation.LimitFlags |=
                JOB_OBJECT_LIMIT_JOB_MEMORY | JOB_OBJECT_LIMIT_PRIORITY_CLASS;
            limits.BasicLimitInformation.PriorityClass =
                windows_sys::Win32::System::Threading::BELOW_NORMAL_PRIORITY_CLASS;
            limits.JobMemoryLimit = usize::try_from(memory).unwrap_or(usize::MAX);
        }
        self.set(JobObjectExtendedLimitInformation, &limits)?;
        let mut cpu = JOBOBJECT_CPU_RATE_CONTROL_INFORMATION {
            ControlFlags: JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP,
            ..Default::default()
        };
        cpu.Anonymous.CpuRate = if enabled { 6000 } else { 10000 };
        self.set(JobObjectCpuRateControlInformation, &cpu)
    }
    fn set<T>(&self, class: i32, value: &T) -> Result<()> {
        let ok = unsafe {
            windows_sys::Win32::System::JobObjects::SetInformationJobObject(
                self.0,
                class,
                (value as *const T).cast(),
                std::mem::size_of::<T>() as u32,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error()).context("OSリソース制限の適用に失敗");
        }
        Ok(())
    }
    fn attach(&self, pid: u32) -> Result<()> {
        use windows_sys::Win32::System::Threading::*;
        let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
        if process.is_null() {
            return Err(std::io::Error::last_os_error()).context("ジョブプロセスを開けません");
        }
        let ok = unsafe {
            windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(self.0, process)
        };
        let error = std::io::Error::last_os_error();
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(process);
        }
        if ok == 0 {
            return Err(error).context("プロセスのリソース保護に失敗");
        }
        Ok(())
    }
}
#[cfg(windows)]
impl Drop for NativeJob {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

/// Resume a freshly CREATE_SUSPENDED process only after Job Object assignment.
/// A suspended process has not run user code and therefore cannot spawn an
/// escaping child before its resource/kill-on-close policy is installed.
#[cfg(windows)]
pub fn resume_suspended(pid: u32) -> Result<()> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
    use windows_sys::Win32::System::Threading::*;
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error()).context("スレッド一覧を取得できません");
    }
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut ok = unsafe { Thread32First(snapshot, &mut entry) };
    let mut result = Err(anyhow::anyhow!(
        "停止中プロセスの初期スレッドが見つかりません"
    ));
    while ok != 0 {
        if entry.th32OwnerProcessID == pid {
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if thread.is_null() {
                result = Err(std::io::Error::last_os_error().into());
            } else {
                let count = unsafe { ResumeThread(thread) };
                let error = std::io::Error::last_os_error();
                unsafe {
                    CloseHandle(thread);
                }
                result = if count == u32::MAX {
                    Err(error.into())
                } else {
                    Ok(())
                };
            }
            break;
        }
        ok = unsafe { Thread32Next(snapshot, &mut entry) };
    }
    unsafe {
        CloseHandle(snapshot);
    }
    result
}
#[cfg(not(windows))]
pub fn resume_suspended(_: u32) -> Result<()> {
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn toggles_os_limits() {
        let guard = ResourceGuard::new().unwrap();
        assert!(!guard.status().enabled);
        guard.set_enabled(true).unwrap();
        assert!(guard.status().enabled);
        guard.set_enabled(false).unwrap();
        assert!(!guard.status().enabled);
    }
}
