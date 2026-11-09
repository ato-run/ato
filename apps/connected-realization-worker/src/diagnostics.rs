//! Read-only connection/host diagnostics, independent of cloud credentials.
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Parser;
use serde_json::{Value, json};

use crate::{
    IsolationMode, SurfacePublication, WorkerConfig, device_key, host_capabilities, host_resources,
};

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
    /// Bearer credential file the worker would use. The device key sits beside
    /// it; without this flag doctor looks beside the default work-root file.
    #[arg(long, env = "ATO_RUNNER_CREDENTIALS_FILE")]
    runner_credentials_file: Option<PathBuf>,
    /// Public origin this slot's ingress serves the Surface from. Same flag and
    /// environment variable as the worker; unset means a loopback-only Surface.
    #[arg(long, env = "ATO_RUNNER_PUBLIC_BASE_URL")]
    public_base_url: Option<String>,
    /// Surface listener the worker would publish. Same flag and environment
    /// variable as the worker.
    #[arg(
        long,
        env = "ATO_RUNTIME_SURFACE_LISTEN",
        default_value = "127.0.0.1:8420"
    )]
    surface_listen: SocketAddr,
    /// Isolation mode the worker would run in. Same flag and environment
    /// variable as the worker: a host-boundary slot is public even without a
    /// public base URL.
    #[arg(
        long,
        env = "ATO_RUNNER_ISOLATION",
        value_enum,
        default_value_t = IsolationMode::Namespace
    )]
    isolation: IsolationMode,
}

/// Operator configuration the report has to reflect. The defaults describe a
/// worker started with neither explicit placement nor a volume root, which is
/// also what the plain `doctor` command means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorOptions {
    pub require_explicit_runtime_placement: bool,
    pub state_volume_root: Option<PathBuf>,
    pub credentials_file: Option<PathBuf>,
    pub public_base_url: Option<String>,
    pub surface_listen: SocketAddr,
    pub isolation: IsolationMode,
}

impl Default for DoctorOptions {
    fn default() -> Self {
        Self {
            require_explicit_runtime_placement: false,
            state_volume_root: None,
            credentials_file: None,
            public_base_url: None,
            surface_listen: "127.0.0.1:8420"
                .parse()
                .expect("the default Surface listener is a literal loopback address"),
            isolation: IsolationMode::Namespace,
        }
    }
}

impl From<&DoctorArgs> for DoctorOptions {
    fn from(args: &DoctorArgs) -> Self {
        Self {
            require_explicit_runtime_placement: args.require_explicit_runtime_placement,
            state_volume_root: args.state_volume_root.clone(),
            credentials_file: args.runner_credentials_file.clone(),
            public_base_url: args.public_base_url.clone(),
            surface_listen: args.surface_listen,
            isolation: args.isolation,
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
    let surface = SurfacePublication::from_parts(
        options.public_base_url.is_some(),
        options.isolation,
        options.surface_listen,
    );
    json!({
        "protocol_versions": [OPEN_COMPUTE_VERSION],
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "host_resources": host_resources::HostResources::probe(work_root),
        "oci_available": ato_adapter_oci::docker_runtime_available(),
        "namespace_isolation_available": ato_sandbox::bubblewrap_containment_available(),
        "host_boundary_isolation_available": cfg!(target_os = "linux") && ato_sandbox::is_sandbox_supported(),
        // Whether the Surface listener this configuration would start is
        // public, and therefore whether every request must carry a signed
        // assertion. The key itself is a per-lease fact, never a host fact.
        "surface_gate": surface_gate_report(surface, options.surface_listen),
        "control": "outbound_poll",
        "host_deletion": false,
        // Presence and epoch only: a diagnostic never contains key material.
        "device_key": device_key_report(work_root, options.credentials_file.as_deref()),
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
                // Doctor reports the isolation the operator selected, without
                // enabling it: host-boundary admission also needs the workload
                // uid/gid and changes how the worker runs.
                "isolation_mode": options.isolation.label(),
                "explicit_placement": options.require_explicit_runtime_placement,
                "state_volume_root": options
                    .state_volume_root
                    .as_ref()
                    .map(|path| path.display().to_string()),
            },
        },
    })
}

/// What the worker's Surface assertion gate would do with this configuration.
/// The verification key is delivered inside each lease claim and is never a
/// host fact, so doctor reports the policy rather than a key value. Whenever
/// the gate would be off, the report names why no assertion is required.
pub(crate) fn surface_gate_report(publication: SurfacePublication, listen: SocketAddr) -> Value {
    let public = publication.is_public();
    json!({
        "listener": listen.to_string(),
        "public_listener": public,
        "publication_reasons": publication.reasons(),
        "gate": if public { "on" } else { "off" },
        "verification_key": if public { "per_lease_claim" } else { "not_required" },
        "lease_without_key": if public {
            "refused_before_workload_start"
        } else {
            "not_applicable"
        },
        "reason": if public {
            "a public Surface listener requires the proxy assertion key the control plane delivers in each lease claim; a lease without it is refused (surface_assertion_key_missing) before the workload or the listener starts"
        } else {
            "the Surface listener is loopback-only and no public base URL is configured, so no request from outside this host can reach it"
        },
    })
}

/// Report whether this Runner has a device key file and which epoch it is at.
/// The key itself — public or private — is never part of a diagnostic.
pub fn device_key_report(work_root: &Path, credentials_file: Option<&Path>) -> Value {
    let path = match credentials_file {
        Some(credentials) => device_key::path_beside_credentials(credentials),
        None => work_root.join(device_key::DEVICE_KEY_FILE_NAME),
    };
    match device_key::RunnerDeviceKey::load_if_present(&path) {
        Ok(Some(key)) => json!({ "present": true, "epoch": key.key_epoch() }),
        Ok(None) => json!({ "present": false, "epoch": null }),
        // Present but unusable: still a fact the operator must see, without
        // echoing the file.
        Err(_) => json!({ "present": true, "epoch": null, "readable": false }),
    }
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
            ..DoctorOptions::default()
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
            state_volume_root: Some(volume_root.clone()),
            ..DoctorOptions::default()
        };
        let diagnostic = report_with(&work_root, &options);
        assert!(!work_root.exists());
        assert!(!volume_root.exists());
        assert_eq!(diagnostic["host_deletion"], false);
        assert!(diagnostic["capabilities"]["capabilities"].is_array());
    }

    /// Doctor must show whether the listener this configuration would start is
    /// public and whether the assertion gate is on, and it must name why the
    /// gate is off whenever no assertion is required.
    #[test]
    fn q92_doctor_reports_the_public_listener_gate_and_its_reason() {
        let root = tempfile::tempdir().unwrap();

        let public = DoctorOptions {
            public_base_url: Some("https://s0.runner.example".to_owned()),
            ..DoctorOptions::default()
        };
        let diagnostic = report_with(root.path(), &public);
        let gate = &diagnostic["surface_gate"];
        assert_eq!(gate["public_listener"], json!(true));
        assert_eq!(gate["gate"], json!("on"));
        assert_eq!(gate["publication_reasons"], json!(["public_base_url"]));
        assert_eq!(gate["verification_key"], json!("per_lease_claim"));
        assert_eq!(
            gate["lease_without_key"],
            json!("refused_before_workload_start")
        );
        assert!(
            gate["reason"]
                .as_str()
                .is_some_and(|reason| !reason.is_empty())
        );

        let private = report_with(root.path(), &DoctorOptions::default());
        let gate = &private["surface_gate"];
        assert_eq!(gate["public_listener"], json!(false));
        assert_eq!(gate["gate"], json!("off"));
        assert_eq!(gate["verification_key"], json!("not_required"));
        assert_eq!(gate["publication_reasons"], json!([]));
        assert!(
            gate["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("loopback-only")),
            "a gate that is off must say why: {gate}"
        );

        // A host-boundary slot is public even with no public base URL, and a
        // non-loopback bind is public on its own.
        let boundary = DoctorOptions {
            isolation: IsolationMode::HostBoundary,
            ..DoctorOptions::default()
        };
        let diagnostic = report_with(root.path(), &boundary);
        assert_eq!(diagnostic["surface_gate"]["public_listener"], json!(true));
        assert_eq!(
            diagnostic["surface_gate"]["publication_reasons"],
            json!(["host_boundary_slot"])
        );
        assert_eq!(
            diagnostic["capabilities"]["inputs"]["isolation_mode"],
            json!("host-boundary")
        );
        let open = DoctorOptions {
            surface_listen: "0.0.0.0:8420".parse().unwrap(),
            ..DoctorOptions::default()
        };
        let diagnostic = report_with(root.path(), &open);
        assert_eq!(diagnostic["surface_gate"]["public_listener"], json!(true));
        assert_eq!(
            diagnostic["surface_gate"]["publication_reasons"],
            json!(["non_loopback_listener"])
        );
    }
}
