use anyhow::Result;
use std::process::Command;

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct GpuReport {
    pub count: usize,
    pub total_vram_mb: Option<u64>,
}

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

    vram.and_then(parse_memory_to_bytes)
        .map(|bytes| bytes > 0)
        .unwrap_or(false)
}

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

fn parse_memory_to_bytes(raw: &str) -> Option<u64> {
    let trimmed = raw.trim();
    let upper = trimmed.to_ascii_uppercase();
    if let Some(num) = upper.strip_suffix("GB") {
        return num.trim().parse::<u64>().ok().map(|v| v * 1_073_741_824);
    }
    if let Some(num) = upper.strip_suffix("MB") {
        return num.trim().parse::<u64>().ok().map(|v| v * 1_048_576);
    }
    None
}
