use crate::error::{CapsuleError, Result};
use crate::metrics::{MetricsSession, ResourceStats, RuntimeMetadata, UnifiedMetrics};
use crate::runtime::{Measurable, RuntimeHandle};
use async_trait::async_trait;
#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::mem;

/// Native/Nacelle 実行のメトリクスハンドル。
pub struct NativeHandle {
    session: MetricsSession,
    pid: u32,
}

impl NativeHandle {
    pub fn new(session_id: impl Into<String>, pid: u32) -> Self {
        Self {
            session: MetricsSession::new(session_id),
            pid,
        }
    }

    fn metadata(&self, exit_code: Option<i32>) -> RuntimeMetadata {
        RuntimeMetadata::Nacelle {
            pid: self.pid,
            exit_code,
        }
    }

    #[cfg(unix)]
    fn timeval_to_seconds(tv: libc::timeval) -> f64 {
        tv.tv_sec as f64 + (tv.tv_usec as f64 / 1_000_000.0)
    }

    #[cfg(unix)]
    fn rusage_cpu_seconds(usage: &libc::rusage) -> f64 {
        Self::timeval_to_seconds(usage.ru_utime) + Self::timeval_to_seconds(usage.ru_stime)
    }

    #[cfg(unix)]
    fn rusage_memory_bytes(usage: &libc::rusage) -> u64 {
        #[cfg(target_os = "linux")]
        {
            (usage.ru_maxrss as u64) * 1024
        }
        #[cfg(target_os = "macos")]
        {
            usage.ru_maxrss as u64
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            usage.ru_maxrss as u64
        }
    }
}

impl RuntimeHandle for NativeHandle {
    fn id(&self) -> &str {
        self.session.session_id()
    }

    fn kill(&mut self) -> Result<()> {
        #[cfg(unix)]
        {
            let res = unsafe { libc::kill(self.pid as i32, libc::SIGKILL) };
            if res == 0 {
                return Ok(());
            }
            Err(CapsuleError::Runtime(
                io::Error::last_os_error().to_string(),
            ))
        }

        #[cfg(not(unix))]
        {
            Err(CapsuleError::Runtime("kill is not implemented".to_string()))
        }
    }
}

#[async_trait]
impl Measurable for NativeHandle {
    async fn capture_metrics(&self) -> Result<UnifiedMetrics> {
        let resources = {
            #[cfg(target_os = "linux")]
            {
                let mut resources = ResourceStats {
                    duration_ms: self.session.elapsed_ms(),
                    ..ResourceStats::default()
                };
                if let Some(sample) = read_proc_sample(self.pid) {
                    resources.cpu_seconds = sample.cpu_seconds;
                    resources.peak_memory_bytes = sample.rss_bytes;
                }
                resources
            }

            #[cfg(not(target_os = "linux"))]
            {
                ResourceStats {
                    duration_ms: self.session.elapsed_ms(),
                    ..ResourceStats::default()
                }
            }
        };

        Ok(self.session.snapshot(resources, self.metadata(None)))
    }

    async fn wait_and_finalize(&self) -> Result<UnifiedMetrics> {
        #[cfg(unix)]
        {
            let pid = self.pid as i32;
            let (status, usage) = tokio::task::spawn_blocking(move || {
                let mut status: i32 = 0;
                let mut usage: libc::rusage = unsafe { mem::zeroed() };
                let res = unsafe { libc::wait4(pid, &mut status, 0, &mut usage) };
                if res < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok((status, usage))
            })
            .await
            .map_err(|err| CapsuleError::Runtime(format!("wait4 task failed: {err}")))
            .and_then(|res| res.map_err(|err| CapsuleError::Runtime(err.to_string())))?;

            let exit_code = decode_exit_code(status);

            let resources = ResourceStats {
                duration_ms: self.session.elapsed_ms(),
                cpu_seconds: Self::rusage_cpu_seconds(&usage),
                peak_memory_bytes: Self::rusage_memory_bytes(&usage),
                ..ResourceStats::default()
            };

            return Ok(self.session.finalize(resources, self.metadata(exit_code)));
        }

        #[cfg(not(unix))]
        {
            Err(CapsuleError::Runtime(
                "wait4 is not implemented".to_string(),
            ))
        }
    }
}

#[cfg(target_os = "linux")]
struct ProcSample {
    cpu_seconds: f64,
    rss_bytes: u64,
}

#[cfg(target_os = "linux")]
fn read_proc_sample(pid: u32) -> Option<ProcSample> {
    let stat_path = format!("/proc/{pid}/stat");
    let raw = std::fs::read_to_string(stat_path).ok()?;
    let end = raw.rfind(')')?;
    let after = raw.get(end + 2..)?;
    let fields: Vec<&str> = after.split_whitespace().collect();
    if fields.len() <= 21 {
        return None;
    }

    let utime_ticks: u64 = fields.get(11)?.parse().ok()?;
    let stime_ticks: u64 = fields.get(12)?.parse().ok()?;
    let rss_pages: i64 = fields.get(21)?.parse().ok()?;

    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let ticks = if ticks > 0 { ticks as f64 } else { 100.0 };

    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page_size = if page_size > 0 {
        page_size as u64
    } else {
        4096
    };

    let cpu_seconds = (utime_ticks + stime_ticks) as f64 / ticks;
    let rss_bytes = if rss_pages > 0 {
        (rss_pages as u64).saturating_mul(page_size)
    } else {
        0
    };

    Some(ProcSample {
        cpu_seconds,
        rss_bytes,
    })
}

#[cfg(unix)]
fn decode_exit_code(status: i32) -> Option<i32> {
    if libc::WIFEXITED(status) {
        Some(libc::WEXITSTATUS(status))
    } else if libc::WIFSIGNALED(status) {
        Some(128 + libc::WTERMSIG(status))
    } else {
        None
    }
}
