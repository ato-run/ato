use tracing::warn;

#[derive(Debug, Clone, Copy)]
pub enum EnforcementMode {
    Strict,
    BestEffort,
}

impl EnforcementMode {
    pub fn parse_mode(value: &str) -> Self {
        match value {
            "strict" => EnforcementMode::Strict,
            _ => EnforcementMode::BestEffort,
        }
    }
}

pub fn check_enforcement(mode: EnforcementMode) -> Result<(), String> {
    #[cfg(not(target_os = "linux"))]
    {
        match mode {
            EnforcementMode::Strict => {
                Err("Enforcement is only supported on Linux (strict)".to_string())
            }
            EnforcementMode::BestEffort => {
                warn!("Enforcement not supported on this OS; continuing in best_effort mode");
                Ok(())
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        let has_cgroup_v2 = std::path::Path::new("/sys/fs/cgroup/cgroup.controllers").exists();
        let has_cap_bpf = has_cap_bpf();

        if has_cgroup_v2 && has_cap_bpf {
            return Ok(());
        }

        let reason = format!(
            "Missing requirements: cgroup v2 = {}, CAP_BPF = {}",
            has_cgroup_v2, has_cap_bpf
        );

        match mode {
            EnforcementMode::Strict => Err(format!("Enforcement strict failed: {reason}")),
            EnforcementMode::BestEffort => {
                warn!("Enforcement best_effort: {reason}");
                Ok(())
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn has_cap_bpf() -> bool {
    const CAP_BPF: u32 = 39;
    let status = match std::fs::read_to_string("/proc/self/status") {
        Ok(value) => value,
        Err(_) => return false,
    };

    for line in status.lines() {
        if let Some(hex) = line.strip_prefix("CapEff:\t") {
            if let Ok(bits) = u128::from_str_radix(hex.trim(), 16) {
                return (bits & (1u128 << CAP_BPF)) != 0;
            }
            return false;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    #[cfg(not(target_os = "linux"))]
    use super::*;

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn best_effort_allows_on_non_linux() {
        let result = check_enforcement(EnforcementMode::BestEffort);
        assert!(result.is_ok());
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn strict_fails_on_non_linux() {
        let result = check_enforcement(EnforcementMode::Strict);
        assert!(result.is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn best_effort_allows_on_linux() {
        let result = check_enforcement(EnforcementMode::BestEffort);
        assert!(result.is_ok());
    }
}
