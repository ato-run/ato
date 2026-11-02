//! Read-only connection/host diagnostics, independent of cloud credentials.
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Parser;
use serde_json::{Value, json};

use crate::{WorkerConfig, host_capabilities, host_resources};

pub const OPEN_COMPUTE_VERSION: &str = "ato.open-compute/1";

#[derive(Parser)]
#[command(about = "Measure this host without registering or starting a workload")]
pub struct DoctorArgs {
    /// Existing directory where workload/cache data will be stored.
    #[arg(long, default_value = ".")]
    work_root: PathBuf,
    /// Report the admission a Runner started in explicit placement mode would
    /// use. Same flag and environment variable as the worker.
    #[arg(
        long,
        env = "ATO_RUNNER_REQUIRE_EXPLICIT_RUNTIME_PLACEMENT",
        default_value_t = false
    )]
    require_explicit_runtime_placement: bool,
    /// Runner-local volume root the worker would use. Doctor checks access
    /// without creating it.
    #[arg(long, env = "ATO_RUNNER_STATE_VOLUME_ROOT")]
    state_volume_root: Option<PathBuf>,
}

/// Operator configuration the report has to reflect. The defaults describe a
/// worker started with neither explicit placement nor a volume root, which is
/// also what the plain `doctor` command means.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DoctorOptions {
    pub require_explicit_runtime_placement: bool,
    pub state_volume_root: Option<PathBuf>,
}

impl From<&DoctorArgs> for DoctorOptions {
    fn from(args: &DoctorArgs) -> Self {
        Self {
            require_explicit_runtime_placement: args.require_explicit_runtime_placement,
            state_volume_root: args.state_volume_root.clone(),
        }
    }
}

pub fn doctor(args: DoctorArgs) -> Result<()> {
    let options = DoctorOptions::from(&args);
    println!(
        "{}",
        serde_json::to_string_pretty(&report_with(&args.work_root, &options))?
    );
    Ok(())
}

/// Measure with the default operator configuration.
pub fn report(work_root: &Path) -> Value {
    report_with(work_root, &DoctorOptions::default())
}

pub fn report_with(work_root: &Path, options: &DoctorOptions) -> Value {
    let facts = host_capabilities::HostFacts::probe(
        options.state_volume_root.as_deref(),
        options.require_explicit_runtime_placement,
    );
    let capabilities = host_capabilities::capability_report(&facts);
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
        // Every capability this host would NOT advertise, with a stable reason
        // code and a sentence an operator can act on. A capability that goes
        // missing silently is a diagnosis nobody can act on.
        "capabilities": {
            "advertised_lease_kinds": capabilities.advertised_lease_kinds,
            "capabilities": capabilities.capabilities,
            // Slot recovery is worker state, not a host fact, and doctor reads
            // no worker state: a fresh worker starts recovered.
            "slot_recovery": "not_evaluated_by_doctor",
            "inputs": {
                // Doctor never enables host-boundary isolation (it needs the
                // workload uid/gid and changes how the worker runs), so the
                // admission it reports is the namespace one.
                "isolation_mode": "namespace",
                "explicit_placement": options.require_explicit_runtime_placement,
                "state_volume_root": options
                    .state_volume_root
                    .as_ref()
                    .map(|path| path.display().to_string()),
            },
        },
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

    /// Every capability doctor does not advertise must carry a stable reason
    /// code and an operator-readable sentence, and every advertised one must
    /// carry none. This is the acceptance condition for the whole change.
    #[test]
    fn q52_doctor_explains_every_capability_it_does_not_advertise() {
        let root = tempfile::tempdir().unwrap();
        let options = DoctorOptions {
            require_explicit_runtime_placement: true,
            state_volume_root: Some(root.path().join("volumes")),
        };
        let diagnostic = report_with(root.path(), &options);
        let capabilities = &diagnostic["capabilities"];
        assert_eq!(capabilities["inputs"]["explicit_placement"], json!(true));
        assert_eq!(
            capabilities["slot_recovery"],
            json!("not_evaluated_by_doctor")
        );
        let advertised = capabilities["advertised_lease_kinds"].as_array().unwrap();
        assert!(
            advertised.iter().all(|kind| kind == "runtime_launch"),
            "explicit placement keeps runtime_launch only: {advertised:?}"
        );
        let entries = capabilities["capabilities"].as_array().unwrap();
        assert!(entries.len() >= 4, "one entry per known capability");
        for entry in entries {
            let state = entry["state"].as_str().unwrap();
            let reasons = entry["reasons"].as_array().unwrap();
            match state {
                // Nothing is advertised under explicit placement here, but if
                // it were it must not also carry reasons.
                "advertised" => assert!(reasons.is_empty(), "{entry}"),
                "unavailable" | "unverified" => {
                    assert!(!reasons.is_empty(), "{entry}");
                    for reason in reasons {
                        assert!(
                            reason["code"].as_str().is_some_and(|code| !code.is_empty()),
                            "{entry}"
                        );
                        assert!(
                            reason["message"]
                                .as_str()
                                .is_some_and(|message| message.trim().len() > 20),
                            "{entry}"
                        );
                    }
                }
                other => panic!("unknown capability state {other}: {entry}"),
            }
        }
        // Explicit placement is named for the lease kind it removes, so the
        // operator sees the restriction instead of a vanished kind.
        let volumes = entries
            .iter()
            .find(|entry| entry["capability"] == "lease_kind:state_volume_maintenance")
            .expect("state_volume_maintenance is always reported");
        assert!(
            volumes["reasons"]
                .as_array()
                .unwrap()
                .iter()
                .any(|reason| reason["code"] == "explicit_placement_restricts_lease_kinds"),
            "{volumes}"
        );
    }

    #[test]
    fn q92_doctor_names_every_known_lease_kind() {
        let root = tempfile::tempdir().unwrap();
        let diagnostic = report_with(root.path(), &DoctorOptions::default());
        let entries = diagnostic["capabilities"]["capabilities"]
            .as_array()
            .unwrap();
        for kind in [
            "portable_capsule_v2",
            "runtime_launch",
            "state_volume_maintenance",
            "activity_browser_executor_v0",
        ] {
            let capability = format!("lease_kind:{kind}");
            assert!(
                entries
                    .iter()
                    .any(|entry| entry["capability"] == capability),
                "{capability} must be reported even when it is not advertised"
            );
        }
        assert!(
            entries
                .iter()
                .any(|entry| entry["capability"] == "execution_abi:oci"),
            "the Docker-backed execution ABI must be reported"
        );
    }

    #[test]
    fn q92_doctor_creates_neither_the_work_root_nor_the_volume_root() {
        let root = tempfile::tempdir().unwrap();
        let work_root = root.path().join("missing-work");
        let volume_root = root.path().join("missing-volumes");
        let options = DoctorOptions {
            require_explicit_runtime_placement: false,
            state_volume_root: Some(volume_root.clone()),
        };
        let diagnostic = report_with(&work_root, &options);
        assert!(!work_root.exists());
        assert!(!volume_root.exists());
        assert_eq!(diagnostic["host_deletion"], false);
        assert!(diagnostic["capabilities"]["capabilities"].is_array());
    }
}
