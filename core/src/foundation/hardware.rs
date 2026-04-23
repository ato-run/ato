use std::process::Command;

use crate::error::Result;

use crate::types::utils::parse_memory_string;

/// GPU presence and VRAM capacity as reported by nvidia-smi.
#[derive(Debug, Clone)]
pub struct GpuReport {
    pub count: usize,
    pub total_vram_mb: Option<u64>,
}

/// Returns `true` if the manifest requests GPU access via `[build] gpu = true`
/// or a non-zero `[requirements] vram_min`.
pub fn requires_gpu(manifest: &toml::Value) -> bool {
    let build_gpu = manifest
        .get("build")
        .and_then(|b| b.get("gpu"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if build_gpu {
        return true;
    }

    let vram = manifest
        .get("requirements")
        .and_then(|r| r.get("vram_min"))
        .and_then(|v| v.as_str());

    vram.and_then(|s| parse_memory_string(s).ok())
        .map(|bytes| bytes > 0)
        .unwrap_or(false)
}

/// Queries `nvidia-smi` for available GPU count and average VRAM (MiB).
///
/// Returns `Ok(None)` when `nvidia-smi` is not on `PATH` or reports no GPUs.
/// Returns `Err` only on unexpected process I/O failures.
pub fn detect_nvidia_gpus() -> Result<Option<GpuReport>> {
    if which::which("nvidia-smi").is_err() {
        return Ok(None);
    }

    let output = Command::new("nvidia-smi")
        .arg("--query-gpu=memory.total")
        .arg("--format=csv,noheader,nounits")
        .output()?;

    if !output.status.success() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut totals = Vec::new();
    for line in stdout.lines() {
        if let Ok(val) = line.trim().parse::<u64>() {
            totals.push(val);
        }
    }

    if totals.is_empty() {
        return Ok(None);
    }

    let avg = totals.iter().copied().sum::<u64>() / totals.len() as u64;
    Ok(Some(GpuReport {
        count: totals.len(),
        total_vram_mb: Some(avg),
    }))
}

#[cfg(test)]
mod tests {
    use super::requires_gpu;

    fn toml(s: &str) -> toml::Value {
        toml::from_str(s).unwrap()
    }

    #[test]
    fn requires_gpu_false_when_no_gpu_section() {
        let manifest = toml(r#"name = "test""#);
        assert!(!requires_gpu(&manifest));
    }

    #[test]
    fn requires_gpu_true_when_build_gpu_true() {
        let manifest = toml("[build]\ngpu = true");
        assert!(requires_gpu(&manifest));
    }

    #[test]
    fn requires_gpu_false_when_build_gpu_false() {
        let manifest = toml("[build]\ngpu = false");
        assert!(!requires_gpu(&manifest));
    }

    #[test]
    fn requires_gpu_true_when_vram_min_nonzero() {
        let manifest = toml("[requirements]\nvram_min = \"8GB\"");
        assert!(requires_gpu(&manifest));
    }

    #[test]
    fn requires_gpu_false_when_vram_min_zero() {
        let manifest = toml("[requirements]\nvram_min = \"0GB\"");
        assert!(!requires_gpu(&manifest));
    }

    #[test]
    fn requires_gpu_false_when_vram_min_invalid() {
        let manifest = toml("[requirements]\nvram_min = \"not-a-size\"");
        assert!(!requires_gpu(&manifest));
    }
}
