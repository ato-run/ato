use anyhow::Result;
use tracing::warn;

/// Result of a scrub operation
#[derive(Debug, Clone)]
pub struct ScrubStats {
    pub bytes_targeted: u64,
    pub bytes_scrubbed: u64,
    pub chunks: u32,
    pub gpu_index: usize,
    pub message: Option<String>,
}

/// Backend abstraction for VRAM scrubbing (testable)
pub trait VramBackend: Send + Sync {
    fn gpu_index(&self) -> usize;
    fn total_bytes(&self) -> Result<u64>;
    fn free_bytes(&self) -> Result<u64>;
    fn zero_chunk(&self, bytes: usize) -> Result<()>;
}

/// Scrub a set of GPU indices using the provided scrubber factory
pub fn scrub_gpu_indices<F>(gpu_indices: &[usize], mut factory: F) -> Vec<ScrubStats>
where
    F: FnMut(usize) -> Result<VramScrubber>,
{
    gpu_indices
        .iter()
        .copied()
        .map(|idx| match factory(idx) {
            Ok(scrubber) => match scrubber.scrub() {
                Ok(stats) => stats,
                Err(e) => ScrubStats {
                    bytes_targeted: 0,
                    bytes_scrubbed: 0,
                    chunks: 0,
                    gpu_index: idx,
                    message: Some(e.to_string()),
                },
            },
            Err(e) => ScrubStats {
                bytes_targeted: 0,
                bytes_scrubbed: 0,
                chunks: 0,
                gpu_index: idx,
                message: Some(format!("init failed: {}", e)),
            },
        })
        .collect()
}

/// CUDA backend (Linux only, requires CUDA driver)
#[cfg(all(target_os = "linux", feature = "cuda"))]
mod cuda_backend {
    use super::VramBackend;
    use anyhow::Result;
    use cudarc::driver::CudaDevice;

    pub struct CudaVramBackend {
        device: CudaDevice,
        index: usize,
    }

    impl CudaVramBackend {
        pub fn new(index: usize) -> Result<Self> {
            let device = CudaDevice::new(index)?;
            Ok(Self { device, index })
        }
    }

    impl VramBackend for CudaVramBackend {
        fn gpu_index(&self) -> usize {
            self.index
        }

        fn total_bytes(&self) -> Result<u64> {
            Ok(self.device.memory_info()?.total)
        }

        fn free_bytes(&self) -> Result<u64> {
            Ok(self.device.memory_info()?.free)
        }

        fn zero_chunk(&self, bytes: usize) -> Result<()> {
            // Allocate zeroed buffer on device; allocation zero-fills the region
            let _buf = self.device.alloc_zeros::<u8>(bytes)?;
            // Dropping the buffer releases memory; allocation itself zeros it
            Ok(())
        }
    }

    pub fn new_backend(index: usize) -> Result<Box<dyn VramBackend>> {
        Ok(Box::new(CudaVramBackend::new(index)?))
    }
}

/// No-op backend for unsupported platforms
mod noop_backend {
    use super::{Result, VramBackend};

    pub struct NoopBackend {
        index: usize,
    }

    impl NoopBackend {
        pub fn new(index: usize) -> Self {
            Self { index }
        }
    }

    impl VramBackend for NoopBackend {
        fn gpu_index(&self) -> usize {
            self.index
        }
        fn total_bytes(&self) -> Result<u64> {
            Ok(0)
        }
        fn free_bytes(&self) -> Result<u64> {
            Ok(0)
        }
        fn zero_chunk(&self, _bytes: usize) -> Result<()> {
            Ok(())
        }
    }

    pub fn new_backend(index: usize) -> Result<Box<dyn VramBackend>> {
        Ok(Box::new(NoopBackend::new(index)))
    }
}

pub struct VramScrubber {
    backend: Box<dyn VramBackend>,
}

impl VramScrubber {
    /// Create a scrubber for the given GPU index
    pub fn new(gpu_index: usize) -> Result<Self> {
        #[cfg(all(target_os = "linux", feature = "cuda"))]
        {
            return Ok(Self {
                backend: crate::security::vram_scrubber::cuda_backend::new_backend(gpu_index)?,
            });
        }

        #[cfg(not(all(target_os = "linux", feature = "cuda")))]
        {
            warn!("VRAM scrubbing backend not available on this platform; using no-op");
            return Ok(Self {
                backend: crate::verification::vram::noop_backend::new_backend(gpu_index)?,
            });
        }
    }

    /// Scrub VRAM by allocating and zeroing in chunks until free memory is exhausted
    pub fn scrub(&self) -> Result<ScrubStats> {
        let total = self.backend.total_bytes().unwrap_or(0);
        let mut remaining = self.backend.free_bytes().unwrap_or(0);

        if total == 0 {
            return Ok(ScrubStats {
                bytes_targeted: 0,
                bytes_scrubbed: 0,
                chunks: 0,
                gpu_index: self.backend.gpu_index(),
                message: Some("backend unavailable or zero VRAM".to_string()),
            });
        }
        // Avoid huge allocations; 256MB chunks default
        let chunk_size: u64 = 256 * 1024 * 1024;
        let mut scrubbed: u64 = 0;
        let mut chunks: u32 = 0;
        let mut last_err: Option<String> = None;

        while remaining > 0 {
            let size = remaining.min(chunk_size) as usize;
            match self.backend.zero_chunk(size) {
                Ok(_) => {
                    scrubbed = scrubbed.saturating_add(size as u64);
                    chunks += 1;
                }
                Err(e) => {
                    last_err = Some(e.to_string());
                    break;
                }
            }
            // reduce remaining defensively; stop if scrubbed >= total to avoid endless loop
            if remaining <= size as u64 {
                break;
            }
            remaining -= size as u64;
            if scrubbed >= total {
                break;
            }
        }

        if let Some(ref msg) = last_err {
            warn!("Partial VRAM scrub: {}", msg);
        }

        Ok(ScrubStats {
            bytes_targeted: total,
            bytes_scrubbed: scrubbed,
            chunks,
            gpu_index: self.backend.gpu_index(),
            message: last_err,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    struct MockBackend {
        total: u64,
        free: u64,
        fail_after: Option<u32>,
        calls: Mutex<u32>,
        idx: usize,
    }

    impl VramBackend for MockBackend {
        fn gpu_index(&self) -> usize {
            self.idx
        }
        fn total_bytes(&self) -> Result<u64> {
            Ok(self.total)
        }
        fn free_bytes(&self) -> Result<u64> {
            Ok(self.free)
        }
        fn zero_chunk(&self, _bytes: usize) -> Result<()> {
            let mut guard = self.calls.lock().unwrap();
            *guard += 1;
            if let Some(limit) = self.fail_after {
                if *guard > limit {
                    return Err(anyhow::anyhow!("mock failure"));
                }
            }
            Ok(())
        }
    }

    fn make_scrubber(total: u64, free: u64, fail_after: Option<u32>) -> VramScrubber {
        make_scrubber_with_index(total, free, fail_after, 0)
    }

    fn make_scrubber_with_index(
        total: u64,
        free: u64,
        fail_after: Option<u32>,
        idx: usize,
    ) -> VramScrubber {
        let backend: Box<dyn VramBackend> = Box::new(MockBackend {
            total,
            free,
            fail_after,
            calls: Mutex::new(0),
            idx,
        });
        VramScrubber { backend }
    }

    #[test]
    fn scrubs_in_chunks_until_free_consumed() {
        let scrubber = make_scrubber(1024 * 1024 * 1024, 512 * 1024 * 1024, None); // total 1GB, free 512MB
        let stats = scrubber.scrub().expect("scrub should succeed");
        assert_eq!(stats.bytes_scrubbed, 512 * 1024 * 1024);
        assert!(stats.chunks >= 2); // because chunk_size 256MB
        assert!(stats.message.is_none());
    }

    #[test]
    fn stops_on_backend_error() {
        let scrubber = make_scrubber(1024 * 1024 * 1024, 512 * 1024 * 1024, Some(1));
        let stats = scrubber.scrub().expect("scrub should still return stats");
        assert!(stats.bytes_scrubbed <= 256 * 1024 * 1024);
        assert!(stats.message.is_some());
    }

    #[test]
    fn scrub_gpu_indices_runs_factory_for_each_index() {
        let stats = scrub_gpu_indices(&[0, 1], |idx| {
            Ok(make_scrubber_with_index(
                1024 * 1024 * 1024,
                256 * 1024 * 1024,
                None,
                idx,
            ))
        });

        assert_eq!(stats.len(), 2);
        assert_eq!(stats[0].gpu_index, 0);
        assert_eq!(stats[1].gpu_index, 1);
        assert!(stats.iter().all(|s| s.message.is_none()));
    }

    #[test]
    fn scrub_gpu_indices_reports_factory_error() {
        let stats = scrub_gpu_indices(&[0], |idx| {
            if idx == 0 {
                Err(anyhow::anyhow!("factory failure"))
            } else {
                Ok(make_scrubber_with_index(0, 0, None, idx))
            }
        });

        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].gpu_index, 0);
        assert!(stats[0]
            .message
            .as_ref()
            .unwrap()
            .contains("factory failure"));
        assert_eq!(stats[0].bytes_scrubbed, 0);
    }
}
