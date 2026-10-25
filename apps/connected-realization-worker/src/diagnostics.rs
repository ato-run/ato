//! Read-only connection/host diagnostics, independent of cloud credentials.
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Parser;
use serde_json::{Value, json};

use crate::{WorkerConfig, host_resources};

pub const OPEN_COMPUTE_VERSION: &str = "ato.open-compute/1";

#[derive(Parser)]
#[command(about = "Measure this host without registering or starting a workload")]
pub struct DoctorArgs {
    /// Existing directory where workload/cache data will be stored.
    #[arg(long, default_value = ".")]
    work_root: PathBuf,
}

pub fn doctor(args: DoctorArgs) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(&report(&args.work_root))?
    );
    Ok(())
}

pub fn report(work_root: &Path) -> Value {
    json!({
        "protocol_versions": [OPEN_COMPUTE_VERSION],
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "host_resources": host_resources::HostResources::probe(work_root),
        "oci_available": ato_adapter_oci::docker_runtime_available(),
        "namespace_isolation_available": ato_sandbox::bubblewrap_containment_available(),
        "host_boundary_isolation_available": cfg!(target_os = "linux") && ato_sandbox::is_sandbox_supported(),
        "control": "outbound_poll",
        "host_deletion": false,
    })
}

/// The lifecycle boundary comes from the enrollment receipt, never host names,
/// provider environment variables, or the workload. Older credentials remain
/// legacy/unreported until the device is enrolled through the current contract.
pub(crate) fn connection_capabilities(
    config: &WorkerConfig,
    persistent_volumes: bool,
) -> Option<Value> {
    let credentials: Value =
        serde_json::from_slice(&std::fs::read(config.runner_credentials_file.as_ref()?).ok()?)
            .ok()?;
    let mode = match credentials.get("management_kind")?.as_str()? {
        "user_managed" => "user_attached",
        "ato_managed" => "provider_managed",
        _ => return None,
    };
    let mut execution = vec!["process"];
    if ato_adapter_oci::docker_runtime_available() {
        execution.push("oci");
    }
    let isolation = if crate::runtime_launch::host_boundary::active().is_some() {
        vec!["host-boundary-v1"]
    } else if ato_sandbox::bubblewrap_containment_available() {
        vec!["untrusted-v1"]
    } else {
        vec![]
    };
    Some(json!({
        "version": OPEN_COMPUTE_VERSION, "mode": mode, "execution": execution, "isolation": isolation,
        "control": "outbound_poll", "surface": if config.public_base_url.is_some() { "direct_http" } else { "none" },
        "storage": if persistent_volumes { "persistent" } else { "unknown" },
        "lifetime_seconds": null, "interruptible": null,
        // This Runner controls its workload only; provider operations are a
        // separately authorized Adapter capability held by the Coordinator.
        "lifecycle": { "provision": false, "delete_host": false, "pricing": false }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doctor_needs_no_identity_and_does_not_create_a_work_root() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing");
        let diagnostic = report(&missing);
        assert!(!missing.exists());
        assert!(diagnostic["host_resources"]["scratch_mib"].is_null());
        assert_eq!(diagnostic["host_deletion"], false);
        assert!(diagnostic.get("runner_token").is_none());
    }
}
