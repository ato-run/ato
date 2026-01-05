use crate::hardware::GpuDetector;

pub const INSUFFICIENT_LOCAL_RESOURCES: &str = "INSUFFICIENT_LOCAL_RESOURCES";
pub const CLOUD_NOT_CONFIGURED: &str = "CLOUD_NOT_CONFIGURED";

pub fn local_gpu_satisfies(
    required_vram_bytes: u64,
    gpu_detector: &dyn GpuDetector,
) -> Result<bool, crate::hardware::GpuDetectionError> {
    if required_vram_bytes == 0 {
        return Ok(true);
    }

    let report = gpu_detector.detect_gpus()?;
    for gpu in report.gpus {
        if gpu_detector.get_available_vram_bytes(gpu.index as usize)? >= required_vram_bytes {
            return Ok(true);
        }
    }

    Ok(false)
}

pub fn compute_deploy_failure_codes(
    local_ok: bool,
    fallback_to_cloud: bool,
) -> Vec<String> {
    if local_ok {
        return vec![];
    }

    let mut codes = vec![INSUFFICIENT_LOCAL_RESOURCES.to_string()];

    if fallback_to_cloud {
        codes.push(CLOUD_NOT_CONFIGURED.to_string());
    }

    codes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::gpu_detector::MockGpuDetector;

    #[test]
    fn local_gpu_satisfies_true_when_enough_vram() {
        let gpu = MockGpuDetector::with_config(1, 16, "12.0".to_string());
        let required = 8 * 1_073_741_824;
        assert!(local_gpu_satisfies(required, &gpu).unwrap());
    }

    #[test]
    fn local_gpu_satisfies_false_when_not_enough_vram() {
        let gpu = MockGpuDetector::with_config(1, 8, "12.0".to_string());
        let required = 16 * 1_073_741_824;
        assert!(!local_gpu_satisfies(required, &gpu).unwrap());
    }

    #[test]
    fn compute_deploy_failure_codes_returns_both_when_cloud_fallback_and_not_configured() {
        let codes = compute_deploy_failure_codes(false, true);
        assert!(codes.contains(&INSUFFICIENT_LOCAL_RESOURCES.to_string()));
        assert!(codes.contains(&CLOUD_NOT_CONFIGURED.to_string()));
    }

    #[test]
    fn compute_deploy_failure_codes_returns_only_insufficient_when_no_fallback() {
        let codes = compute_deploy_failure_codes(false, false);
        assert_eq!(codes, vec![INSUFFICIENT_LOCAL_RESOURCES.to_string()]);
    }

    #[test]
    fn compute_deploy_failure_codes_empty_when_local_ok() {
        let codes = compute_deploy_failure_codes(true, true);
        assert!(codes.is_empty());
    }
}
