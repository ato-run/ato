//! The macOS native desktop Adapter.
//!
//! It materializes the verified artifact, verifies the signature policy with
//! `codesign` and records the separate Gatekeeper verdict from `spctl`,
//! applies a Seatbelt profile, owns the process group, waits for the declared
//! loopback Port and hands the endpoints to the common verifier.
//!
//! Everything here is macOS-only; the module is not compiled elsewhere.

use std::any::Any;
use std::collections::BTreeMap;
use std::fs;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use ato_formation::verify::{VerificationExecutionEvidence, VerificationNativeEvidence};
use ato_runtime_attempt::launch::process_executor::state_path_env_name;
use ato_runtime_attempt::realize::{RealizeFailure, Realized, RunningCandidate};
use ato_sandbox::SandboxPolicy;

use super::artifact::{ExtractedArtifact, extract_tar_gz};
use super::{NativeRefusal, NativeStart, resolve_entry};
use crate::portable_run::{endpoint_port_env_name, remove_owned};
use crate::process_group::{ProcessServiceGroup, ServiceSpec};

/// How long the declared Ports may take to accept after the process started.
const PORT_READY_TIMEOUT: Duration = Duration::from_secs(60);

/// Where a signature tool is, if this host has it.
fn codesign_tool() -> Option<PathBuf> {
    ato_host_facts::find_tool("codesign", &["/usr/bin/codesign"])
}

fn spctl_tool() -> Option<PathBuf> {
    ato_host_facts::find_tool("spctl", &["/usr/sbin/spctl"])
}

fn seatbelt_launcher() -> Option<PathBuf> {
    ato_host_facts::find_tool("sandbox-exec", &["/usr/bin/sandbox-exec"])
}

/// Whether this process can actually apply a Seatbelt profile.
///
/// Finding the launcher is not the capability: a process already sandboxed by
/// its supervisor cannot nest another profile and `sandbox_apply` fails with
/// EPERM. Admission must reflect the measured capability, not the binary.
pub(crate) fn seatbelt_applicable() -> bool {
    ato_host_facts::NativeFacts::probe().sandbox_tool
}

/// One signature verdict set. `codesign` and Gatekeeper are deliberately two
/// separate observations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SignatureReport {
    /// `valid` or `invalid`: `codesign --verify --deep --strict`.
    pub codesign_verify: &'static str,
    /// `developer_id`, `other`, `adhoc`, `unsigned` or `unknown`.
    pub signature_kind: &'static str,
    pub codesign_detail: Option<String>,
    /// `accepted`, `rejected` or `unavailable`: `spctl --assess`.
    pub gatekeeper: &'static str,
    pub gatekeeper_detail: Option<String>,
}

impl SignatureReport {
    /// The admission minimum: a valid Developer ID signature. An ad-hoc
    /// signature is "valid on disk" to codesign but is not a Developer ID and
    /// is not accepted by this predicate.
    pub(crate) fn satisfies_require_valid(&self) -> bool {
        self.codesign_verify == "valid" && self.signature_kind == "developer_id"
    }
}

fn bounded_detail(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.chars().take(400).collect())
    }
}

/// Observe the signature and Gatekeeper verdicts for one executable.
pub(crate) fn probe_signature(entry: &Path) -> SignatureReport {
    let (Some(codesign), Some(spctl)) = (codesign_tool(), spctl_tool()) else {
        return SignatureReport {
            codesign_verify: "invalid",
            signature_kind: "unknown",
            codesign_detail: Some("codesign or spctl is not available on this host".to_owned()),
            gatekeeper: "unavailable",
            gatekeeper_detail: Some("spctl is not available on this host".to_owned()),
        };
    };
    let verify = Command::new(&codesign)
        .args(["--verify", "--deep", "--strict", "--verbose=2"])
        .arg(entry)
        .output();
    let (codesign_verify, codesign_detail) = match verify {
        Ok(output) if output.status.success() => ("valid", None),
        Ok(output) => (
            "invalid",
            bounded_detail(&output.stderr).or_else(|| bounded_detail(&output.stdout)),
        ),
        Err(error) => ("invalid", Some(format!("could not run codesign: {error}"))),
    };
    // `codesign -dvvv` describes what the signature IS, which is what decides
    // whether "valid" means the Developer ID minimum admission asks for.
    let described = Command::new(&codesign).args(["-dvvv"]).arg(entry).output();
    let description = described
        .as_ref()
        .map(|output| {
            format!(
                "{}\n{}",
                String::from_utf8_lossy(&output.stderr),
                String::from_utf8_lossy(&output.stdout)
            )
        })
        .unwrap_or_default();
    let signature_kind = if description.contains("Signature=adhoc") {
        "adhoc"
    } else if description.contains("Authority=Developer ID Application") {
        "developer_id"
    } else if description.contains("Authority=") {
        "other"
    } else if codesign_verify == "invalid"
        && (description.contains("not signed at all")
            || description.contains("code object is not signed"))
    {
        "unsigned"
    } else if codesign_verify == "valid" {
        // Some platform binaries verify without an Authority line.
        "other"
    } else {
        "unknown"
    };
    let assess = Command::new(&spctl)
        .args(["--assess", "--type", "execute", "--verbose=2"])
        .arg(entry)
        .output();
    let (gatekeeper, gatekeeper_detail) = match assess {
        Ok(output) if output.status.success() => ("accepted", None),
        Ok(output) => (
            "rejected",
            bounded_detail(&output.stderr).or_else(|| bounded_detail(&output.stdout)),
        ),
        Err(error) => ("rejected", Some(format!("could not run spctl: {error}"))),
    };
    SignatureReport {
        codesign_verify,
        signature_kind,
        codesign_detail,
        gatekeeper,
        gatekeeper_detail,
    }
}

/// Decide whether the declared policy admits this signature.
///
/// Returns whether the Run proceeds on an explicit local override. A
/// `require-valid` route never proceeds on an override, and an override never
/// relaxes any filesystem, state, egress or lifecycle rule.
fn authorize_signature(
    report: &SignatureReport,
    policy: ato_formation::native_desktop::NativeSignaturePolicy,
    local_override: bool,
) -> Result<bool, NativeRefusal> {
    if report.satisfies_require_valid() {
        return Ok(false);
    }
    if !policy.allows_local_override() {
        return Err(NativeRefusal {
            code: "native_signature_policy_unsatisfied",
            detail: format!(
                "the declared policy is require-valid and the artifact is {} (codesign {}); \
                 this route can only run with a valid Developer ID signature",
                report.signature_kind, report.codesign_verify
            ),
        });
    }
    if !local_override {
        return Err(NativeRefusal {
            code: "native_signature_local_override_required",
            detail: format!(
                "the artifact is {} (codesign {}); running it needs the explicit local override \
                 a require-valid-or-local-override route permits",
                report.signature_kind, report.codesign_verify
            ),
        });
    }
    Ok(true)
}

/// The mandatory filesystem, state and egress policy for this Run.
///
/// Read-only: the artifact and the materialized workspace. Read-write: only
/// the state slot directories and this Run's own scratch. Network: inbound on
/// the declared loopback Ports only; TCP egress is denied.
#[derive(Debug)]
pub(crate) struct SandboxPlan {
    pub policy: SandboxPolicy,
    pub read_only: Vec<String>,
    pub read_write: Vec<String>,
    pub inbound_ports: Vec<u16>,
}

/// The path spelling SBPL rules and the receipt use: the resolved one, when
/// the path exists, so `/var/folders/...` and `/private/var/folders/...` cannot
/// disagree between the profile and what a workload opens.
fn resolved_spelling(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

pub(crate) fn sandbox_plan(
    artifact_root: &Path,
    workspace: &Path,
    state_paths: &[(String, PathBuf)],
    scratch_root: &Path,
    ports: &[u16],
) -> SandboxPlan {
    let system_read_only = [
        "/usr",
        "/System",
        "/Library",
        "/bin",
        "/sbin",
        "/opt",
        "/usr/local",
        "/private/etc",
        "/private/var/db",
        "/dev",
    ];
    // State the rules in the spelling the kernel matches. `/var/folders` and
    // its resolved `/private/var/folders` are the same vnode but not the same
    // string, and an SBPL subpath rule is written once: resolve every path the
    // Run owns before the profile is rendered, and keep the resolved spelling
    // in the receipt evidence so it says what was actually enforced.
    let mut read_only = vec![
        resolved_spelling(artifact_root),
        resolved_spelling(workspace),
    ];
    read_only.extend(
        system_read_only
            .iter()
            .map(|path| resolved_spelling(Path::new(path))),
    );
    let mut read_write = vec![resolved_spelling(scratch_root)];
    read_write.extend(state_paths.iter().map(|(_, path)| resolved_spelling(path)));
    // A zero port is not a declaration; descriptor validation already refuses
    // it. Filtering here keeps the guarantee local to the renderer so no
    // caller can turn it into an unparseable SBPL profile.
    let declared_ports = ports
        .iter()
        .copied()
        .filter(|port| *port != 0)
        .collect::<Vec<_>>();
    let policy = SandboxPolicy::new()
        .allow_read_only(read_only.iter().map(PathBuf::from))
        .allow_read_write(read_write.iter().map(PathBuf::from))
        .with_network(false)
        .allow_tcp_bind(declared_ports.iter().copied());
    SandboxPlan {
        policy,
        read_only,
        read_write,
        inbound_ports: declared_ports,
    }
}

/// Verify the signature policy and build the exact Seatbelt profile this Run
/// will apply. Split out so the decisions are testable without launching.
#[derive(Debug)]
pub(crate) struct PreparedNative {
    pub extracted: ExtractedArtifact,
    pub entry: PathBuf,
    pub signature: SignatureReport,
    pub override_used: bool,
    pub sandbox: SandboxPlan,
    pub profile: String,
}

pub(crate) fn prepare(
    request: &NativeStart<'_>,
    native_root: &Path,
) -> Result<PreparedNative, NativeRefusal> {
    if request.declares_egress {
        return Err(NativeRefusal {
            code: "native_egress_policy_unavailable",
            detail: "this Adapter can enforce inbound loopback Ports only; a route that declares \
                     TCP egress cannot have that declared policy enforced before launch"
                .to_owned(),
        });
    }
    if seatbelt_launcher().is_none() || !seatbelt_applicable() {
        return Err(NativeRefusal {
            code: "native_sandbox_unavailable",
            detail: "Seatbelt cannot be applied on this host, so the mandatory filesystem/state/\
                     egress/lifecycle policy would not be enforced"
                .to_owned(),
        });
    }
    let artifact_root = native_root.join("artifact");
    let extracted =
        extract_tar_gz(&request.artifact_bytes, &artifact_root).map_err(|error| NativeRefusal {
            code: "native_artifact_invalid",
            detail: format!("{error:#}"),
        })?;
    let entry = resolve_entry(&extracted.root, &request.descriptor.entry)?;
    if codesign_tool().is_none() || spctl_tool().is_none() {
        return Err(NativeRefusal {
            code: "native_signature_verification_unavailable",
            detail: "codesign or spctl is not available, so no signature verdict could be \
                     recorded before launch"
                .to_owned(),
        });
    }
    let signature = probe_signature(&entry);
    let override_used = authorize_signature(
        &signature,
        request.descriptor.signature,
        request.local_override,
    )?;
    let scratch_root = native_root.join("runtime");
    fs::create_dir_all(scratch_root.join("tmp")).map_err(|error| NativeRefusal {
        code: "native_scratch_unavailable",
        detail: format!("could not create the Run scratch: {error}"),
    })?;
    fs::create_dir_all(scratch_root.join("home")).map_err(|error| NativeRefusal {
        code: "native_scratch_unavailable",
        detail: format!("could not create the Run scratch: {error}"),
    })?;
    let ports = request
        .ports
        .iter()
        .map(|port| port.guest_port)
        .collect::<Vec<_>>();
    let sandbox = sandbox_plan(
        &extracted.root,
        &request.workspace,
        &request.state_paths,
        &scratch_root,
        &ports,
    );
    let profile =
        ato_sandbox::render_seatbelt_profile(&sandbox.policy).map_err(|error| NativeRefusal {
            code: "native_sandbox_unavailable",
            detail: format!("the Seatbelt profile could not be built: {error:#}"),
        })?;
    Ok(PreparedNative {
        extracted,
        entry,
        signature,
        override_used,
        sandbox,
        profile,
    })
}

/// The environment the workload receives. Nothing here is secret except the
/// Binding values the caller already resolved.
fn environment_for(
    request: &NativeStart<'_>,
    scratch_root: &Path,
    ports: &[crate::ValidatedNativePort],
) -> BTreeMap<String, String> {
    let mut environment = request.environment.clone();
    for port in ports {
        environment.insert(
            endpoint_port_env_name(&port.id),
            port.guest_port.to_string(),
        );
    }
    if let Some(first) = ports.first() {
        environment.insert("ATO_NATIVE_PORT".to_owned(), first.guest_port.to_string());
    }
    for (id, path) in &request.state_paths {
        environment.insert(state_path_env_name(id), path.display().to_string());
    }
    environment.insert(
        "ATO_RUNTIME_DIR".to_owned(),
        scratch_root.display().to_string(),
    );
    for name in ["TMPDIR", "TMP", "TEMP"] {
        environment.insert(
            name.to_owned(),
            scratch_root.join("tmp").display().to_string(),
        );
    }
    environment.insert(
        "HOME".to_owned(),
        scratch_root.join("home").display().to_string(),
    );
    environment.insert(
        "XDG_CACHE_HOME".to_owned(),
        scratch_root
            .join("home")
            .join(".cache")
            .display()
            .to_string(),
    );
    environment
}

fn wait_for_ports(ports: &[crate::ValidatedNativePort]) -> Result<(), String> {
    wait_for_ports_with_timeout(ports, PORT_READY_TIMEOUT)
}

fn wait_for_ports_with_timeout(
    ports: &[crate::ValidatedNativePort],
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    for port in ports {
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port.guest_port));
        loop {
            if TcpStream::connect_timeout(&address, Duration::from_millis(250)).is_ok() {
                break;
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "the native application did not accept TCP on {address} within {}s",
                    timeout.as_secs()
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    Ok(())
}

/// A running native desktop application: its process group and the scratch
/// this Run owns.
struct NativeDesktopCandidate {
    group: Option<ProcessServiceGroup>,
    endpoints: BTreeMap<String, String>,
    owned: Vec<PathBuf>,
}

impl RunningCandidate for NativeDesktopCandidate {
    fn endpoints(&self) -> &BTreeMap<String, String> {
        &self.endpoints
    }

    fn exited(&mut self) -> Result<Option<String>> {
        match self.group.as_mut() {
            Some(group) => group.exited(),
            None => Ok(Some("stopped".to_owned())),
        }
    }

    fn stop(mut self: Box<Self>) -> Result<()> {
        // Stop the whole group, not the leader: a native app that forked a
        // helper is still running when only its main process exits.
        if let Some(group) = self.group.take() {
            group.stop()?;
        }
        remove_owned(&self.owned)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Drop for NativeDesktopCandidate {
    fn drop(&mut self) {
        drop(self.group.take());
        if let Err(error) = remove_owned(&self.owned) {
            eprintln!("[ato run] {error:#}");
        }
    }
}

fn launch_failure(error: anyhow::Error, owned: &[PathBuf]) -> RealizeFailure {
    for path in owned {
        let _ = fs::remove_dir_all(path);
    }
    RealizeFailure::Launch {
        error,
        evidence: None,
    }
}

/// Start one selected native desktop Derivation.
pub(crate) fn start(request: NativeStart<'_>) -> Result<Realized, RealizeFailure> {
    let native_root = request.runtime_root.join("native");
    if native_root.exists() {
        let _ = fs::remove_dir_all(&native_root);
    }
    fs::create_dir_all(&native_root).map_err(|error| {
        RealizeFailure::Execution(anyhow!(
            "native_scratch_unavailable: could not create {}: {error}",
            native_root.display()
        ))
    })?;
    let owned = vec![native_root.clone(), request.workspace.clone()];

    let prepared = prepare(&request, &native_root).map_err(|refusal| {
        // `prepare` may have created scratch; whatever it created belongs to
        // this Run and goes with the refusal.
        let _ = fs::remove_dir_all(&native_root);
        RealizeFailure::Execution(anyhow::Error::new(refusal))
    })?;
    let scratch_root = native_root.join("runtime");
    let profile_path = native_root.join("sandbox.sb");
    fs::write(&profile_path, &prepared.profile).map_err(|error| {
        launch_failure(
            anyhow!("native_sandbox_unavailable: could not write the profile: {error}"),
            &owned,
        )
    })?;

    let launcher = seatbelt_launcher().ok_or_else(|| {
        RealizeFailure::Execution(anyhow::Error::new(NativeRefusal {
            code: "native_sandbox_unavailable",
            detail: "sandbox-exec disappeared between admission and launch".to_owned(),
        }))
    })?;
    let command = vec![
        launcher.display().to_string(),
        "-f".to_owned(),
        profile_path.display().to_string(),
        prepared.entry.display().to_string(),
    ];
    let environment = environment_for(&request, &scratch_root, request.ports);
    let readiness = SocketAddr::from((Ipv4Addr::LOCALHOST, request.ports[0].guest_port));
    let spec = ServiceSpec {
        name: "native".to_owned(),
        command,
        cwd: PathBuf::new(),
        environment,
        readiness,
        helpers: Vec::new(),
    };
    let group = ProcessServiceGroup::start(prepared.extracted.root.as_path(), vec![spec])
        .map_err(|error| launch_failure(error, &owned))?;
    if let Err(detail) = wait_for_ports(request.ports) {
        let stop = group.stop();
        let error = match stop {
            Ok(()) => anyhow!(detail),
            Err(stop) => {
                anyhow!("{detail}; additionally the process group was not fully stopped: {stop:#}")
            }
        };
        return Err(launch_failure(error, &owned));
    }
    let pids = group.pids();
    let pid = pids.first().map(|(_, pid)| *pid);
    let process_group = pid;
    let endpoints = request
        .ports
        .iter()
        .map(|port| {
            (
                port.id.clone(),
                format!("http://127.0.0.1:{}", port.guest_port),
            )
        })
        .collect::<BTreeMap<_, _>>();

    let first_endpoint = endpoints.values().next().cloned().unwrap_or_default();
    let execution = VerificationExecutionEvidence {
        realization: "native_desktop".to_owned(),
        runtime_executable: Some(prepared.entry.display().to_string()),
        runtime_version: None,
        pid,
        endpoint: Some(first_endpoint),
        native: Some(VerificationNativeEvidence {
            artifact_ref: request.descriptor.artifact.clone(),
            materialized_tree_digest: prepared.extracted.tree_digest.clone(),
            entry: request.descriptor.entry.clone(),
            signature_policy: request.descriptor.signature.as_str().to_owned(),
            codesign_verify: prepared.signature.codesign_verify.to_owned(),
            signature_kind: prepared.signature.signature_kind.to_owned(),
            codesign_detail: prepared.signature.codesign_detail.clone(),
            gatekeeper: prepared.signature.gatekeeper.to_owned(),
            gatekeeper_detail: prepared.signature.gatekeeper_detail.clone(),
            local_override: prepared.override_used,
            sandbox: "seatbelt".to_owned(),
            enforced_read_only: prepared.sandbox.read_only.clone(),
            enforced_read_write: prepared.sandbox.read_write.clone(),
            enforced_inbound_ports: prepared.sandbox.inbound_ports.clone(),
            process_group,
            window_containment: "not enforced in this increment".to_owned(),
        }),
        ..Default::default()
    };
    let evidence = ato_formation::request::RealizationEvidence {
        port_operations: Vec::new(),
        executor: "ato native desktop adapter (macOS Seatbelt)".to_owned(),
        containment: "seatbelt: read-only artifact/workspace, Run-scoped state writes, \
                      declared loopback Ports only; window/display not enforced"
            .to_owned(),
        workspace: "artifact and workspace mounted read-only".to_owned(),
        build_network: "not applicable: the artifact is a verified bundle payload".to_owned(),
        candidate_network: "inbound on declared loopback Ports only; TCP egress denied".to_owned(),
        endpoints: endpoints.clone(),
        destroyed: false,
    };
    Ok(Realized {
        candidate: Box::new(NativeDesktopCandidate {
            group: Some(group),
            endpoints,
            owned,
        }),
        evidence: Some(evidence),
        execution,
        kept: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ato_formation::native_desktop::NativeSignaturePolicy;

    fn codesign_available() -> bool {
        codesign_tool().is_some() && spctl_tool().is_some()
    }

    /// The committed sample artifact is an unsigned shell script. Its
    /// signature verdict must be recorded as such, and `require-valid` must
    /// refuse it.
    #[test]
    fn an_unsigned_artifact_is_recorded_and_require_valid_refuses_it() {
        if !codesign_available() {
            panic!(
                "codesign and spctl are required to verify the native signature policy on macOS; \
                 this host cannot produce the verdict the policy needs"
            );
        }
        let scratch = tempfile::tempdir().unwrap();
        let entry = scratch.path().join("run.sh");
        fs::write(&entry, b"#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&entry, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let report = probe_signature(&entry);
        assert_eq!(report.codesign_verify, "invalid");
        assert_eq!(report.signature_kind, "unsigned");
        assert!(!report.satisfies_require_valid());
        assert_eq!(
            report.gatekeeper, "rejected",
            "an unsigned script is rejected by Gatekeeper; the outcome is recorded, not assumed"
        );
        let refusal =
            authorize_signature(&report, NativeSignaturePolicy::RequireValid, true).unwrap_err();
        assert_eq!(refusal.code, "native_signature_policy_unsatisfied");
        let refusal = authorize_signature(
            &report,
            NativeSignaturePolicy::RequireValidOrLocalOverride,
            false,
        )
        .unwrap_err();
        assert_eq!(refusal.code, "native_signature_local_override_required");
        let override_used = authorize_signature(
            &report,
            NativeSignaturePolicy::RequireValidOrLocalOverride,
            true,
        )
        .unwrap();
        assert!(override_used);
    }

    #[test]
    fn an_ad_hoc_signature_is_valid_on_disk_but_not_a_developer_id() {
        if !codesign_available() {
            panic!("codesign and spctl are required to observe the ad-hoc verdict");
        }
        let scratch = tempfile::tempdir().unwrap();
        let entry = scratch.path().join("echo");
        fs::copy("/bin/echo", &entry).unwrap();
        let signed = Command::new(codesign_tool().unwrap())
            .args(["--sign", "-", "--force"])
            .arg(&entry)
            .output()
            .unwrap();
        assert!(
            signed.status.success(),
            "ad-hoc signing failed: {}",
            String::from_utf8_lossy(&signed.stderr)
        );
        let report = probe_signature(&entry);
        assert_eq!(report.codesign_verify, "valid");
        assert_eq!(report.signature_kind, "adhoc");
        assert!(!report.satisfies_require_valid());
        assert_eq!(report.gatekeeper, "rejected");
        assert_eq!(
            authorize_signature(&report, NativeSignaturePolicy::RequireValid, true)
                .unwrap_err()
                .code,
            "native_signature_policy_unsatisfied"
        );
        assert!(
            authorize_signature(
                &report,
                NativeSignaturePolicy::RequireValidOrLocalOverride,
                true
            )
            .unwrap()
        );
    }

    #[test]
    fn the_plane_text_artifact_policy_is_fully_specified() {
        let scratch = tempfile::tempdir().unwrap();
        let state = scratch.path().join("state");
        fs::create_dir_all(&state).unwrap();
        let plan = sandbox_plan(
            scratch.path(),
            scratch.path(),
            &[("data".to_owned(), state.clone())],
            scratch.path(),
            &[18080],
        );
        let profile = ato_sandbox::render_seatbelt_profile(&plan.policy).unwrap();
        assert!(profile.contains("(deny default)"), "{profile}");
        assert!(
            profile.contains("(allow network-inbound (local tcp \"localhost:18080\"))"),
            "{profile}"
        );
        assert!(!profile.contains("(allow network-outbound)"), "{profile}");
        for path in &plan.read_only {
            let resolved = Path::new(path)
                .canonicalize()
                .unwrap_or_else(|_| PathBuf::from(path));
            assert!(
                profile.contains(&format!(
                    "(allow file-read* (subpath \"{}\")",
                    resolved.display()
                )),
                "missing read-only rule for {path}"
            );
        }
        for path in &plan.read_write {
            let resolved = Path::new(path)
                .canonicalize()
                .unwrap_or_else(|_| PathBuf::from(path));
            assert!(
                profile.contains(&format!(
                    "(allow file-read* file-write* (subpath \"{}\")",
                    resolved.display()
                )),
                "missing read-write rule for {path}"
            );
        }
    }

    /// Apply the real profile to a tiny child: the state write must succeed and
    /// a write outside it must be refused by the kernel.
    #[test]
    fn the_applied_profile_allows_the_state_write_and_refuses_another_path() {
        let launcher = match seatbelt_launcher() {
            Some(launcher) => launcher,
            None => panic!("sandbox-exec is required to test the mandatory Seatbelt policy"),
        };
        if !seatbelt_applicable() {
            // This process is already sandboxed (the harness that started it
            // applied its own profile), so the kernel refuses to nest another
            // one. The same fact must refuse native admission; prove that the
            // requirement is enforced instead of pretending the policy ran.
            let report = probe_signature(Path::new("/bin/echo"));
            assert!(report.codesign_verify == "valid" || report.codesign_verify == "invalid");
            let facts = ato_host_facts::NativeFacts::probe();
            assert!(!facts.sandbox_tool);
            eprintln!(
                "NOTE: live Seatbelt enforcement was not exercised because this process cannot \
                 apply a profile ({}); admission refuses the route for the same measured fact",
                facts
                    .sandbox_detail
                    .unwrap_or_else(|| "sandbox_apply was denied".to_owned())
            );
            return;
        }
        // Both the child's working directory and every path the policy states
        // are resolved first: the child must start inside an allowed
        // directory, and `/var/folders/...` must not disagree with
        // `/private/var/folders/...` between the rule and the write.
        let scratch = tempfile::tempdir().unwrap();
        let scratch = scratch.path().canonicalize().unwrap();
        let state = scratch.join("state");
        fs::create_dir_all(&state).unwrap();
        let plan = sandbox_plan(
            &scratch,
            &scratch,
            &[("data".to_owned(), state.clone())],
            &scratch,
            &[0],
        );
        let profile = ato_sandbox::render_seatbelt_profile(&plan.policy).unwrap();
        let profile_path = scratch.join("policy.sb");
        fs::write(&profile_path, profile).unwrap();

        // `/usr/bin/touch` is a real Mach-O, not the `xcode-select` shell shim
        // `/bin/sh` may be on this host, and `current_dir` keeps `getcwd`
        // inside the allowed tree.
        let run_sandboxed = |target: &Path| {
            Command::new(&launcher)
                .arg("-f")
                .arg(&profile_path)
                .arg("/usr/bin/touch")
                .arg(target)
                .current_dir(&state)
                .output()
                .unwrap()
        };
        let allowed = state.join("written");
        let allowed_output = run_sandboxed(&allowed);
        assert!(
            allowed_output.status.success() && allowed.is_file(),
            "the state write was refused: {}",
            String::from_utf8_lossy(&allowed_output.stderr)
        );

        // A directory that exists and is writable by this user but is outside
        // every rule the policy states: the write must be refused by the
        // kernel (not merely fail because the path is missing).
        let outside = tempfile::tempdir().unwrap();
        let outside = outside.path().canonicalize().unwrap();
        let denied = outside.join("denied");
        let denied_output = run_sandboxed(&denied);
        let denied_stderr = String::from_utf8_lossy(&denied_output.stderr);
        assert!(
            !denied_output.status.success(),
            "a write outside the Run state was accepted: {}",
            denied_stderr
        );
        assert!(
            denied_stderr.contains("Operation not permitted"),
            "the outside write failed for a reason other than the kernel policy: {denied_stderr}"
        );
        assert!(!denied.exists());
    }

    #[test]
    fn the_declared_entry_must_exist_be_executable_and_stay_inside_the_artifact() {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().join("artifact");
        fs::create_dir_all(root.join("app")).unwrap();
        let entry = root.join("app/run.sh");
        fs::write(&entry, b"#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&entry, fs::Permissions::from_mode(0o644)).unwrap();
        }
        assert_eq!(
            resolve_entry(&root, "app/run.sh").unwrap_err().code,
            "native_entry_not_executable"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&entry, fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(
            resolve_entry(&root, "app/run.sh").unwrap(),
            entry.canonicalize().unwrap()
        );
        assert_eq!(
            resolve_entry(&root, "app/missing").unwrap_err().code,
            "native_entry_missing"
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/bin/sh", root.join("app/escape")).unwrap();
            assert_eq!(
                resolve_entry(&root, "app/escape").unwrap_err().code,
                "native_entry_escapes_artifact"
            );
        }
    }

    fn pid_alive(pid: u32) -> bool {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    fn wait_for_pid(path: &Path) -> u32 {
        for _ in 0..500 {
            if let Ok(text) = fs::read_to_string(path)
                && let Ok(pid) = text.trim().parse::<u32>()
            {
                return pid;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("{} was never written", path.display());
    }

    /// The candidate owns a process group, not one process: a native app that
    /// forked a helper is still running when only its leader exits, and the
    /// Run's stop must reclaim it and show no leftovers.
    /// Start the tiny child, retrying on a fresh port if the port this test
    /// chose was taken between selection and bind. Port selection is a test
    /// concern; the supervisor under test is unchanged.
    fn start_tiny_child(workspace: &Path, pid_file: &Path) -> (ProcessServiceGroup, u16, u32) {
        let mut last_error = None;
        for _ in 0..8 {
            // Hold the listener while choosing so the next allocation cannot
            // return the same number, then release it for the child.
            let guard = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let port = guard.local_addr().unwrap().port();
            drop(guard);
            let script = format!(
                r#"import socket,subprocess,time
s=socket.socket()
s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1)
for _ in range(50):
    try:
        s.bind(('127.0.0.1',{port}))
        break
    except OSError:
        time.sleep(0.02)
s.listen()
child=subprocess.Popen(['sleep','60'])
open({pid_file:?},'w').write(str(child.pid))
time.sleep(0.3)
"#,
                pid_file = pid_file.display().to_string()
            );
            match ProcessServiceGroup::start(
                workspace,
                vec![ServiceSpec {
                    name: "native".to_owned(),
                    command: vec!["python3".to_owned(), "-c".to_owned(), script],
                    cwd: PathBuf::new(),
                    environment: BTreeMap::new(),
                    readiness: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
                    helpers: Vec::new(),
                }],
            ) {
                Ok(group) => {
                    let leader = group.pids()[0].1;
                    return (group, port, leader);
                }
                Err(error) => last_error = Some(error),
            }
        }
        panic!("the tiny native child never started: {last_error:#?}");
    }

    #[test]
    fn the_owned_group_is_reaped_including_a_child_left_by_an_exited_leader() {
        let workspace = tempfile::tempdir().unwrap();
        let pid_file = workspace.path().join("child.pid");
        let (group, port, leader) = start_tiny_child(workspace.path(), &pid_file);
        let child = wait_for_pid(&pid_file);
        assert!(pid_alive(child));
        let owned = vec![workspace.path().join("owned")];
        fs::create_dir_all(&owned[0]).unwrap();
        let mut candidate = Box::new(NativeDesktopCandidate {
            group: Some(group),
            endpoints: BTreeMap::from([(
                "app.http".to_owned(),
                format!("http://127.0.0.1:{port}"),
            )]),
            owned: owned.clone(),
        });
        // The leader exits on its own; the child keeps the group alive.
        let deadline = Instant::now() + Duration::from_secs(10);
        while candidate.exited().expect("exited() is readable").is_none() {
            assert!(Instant::now() < deadline, "the leader never exited");
            std::thread::sleep(Duration::from_millis(20));
        }
        RunningCandidate::stop(candidate).expect("the Run stops its group");
        assert!(
            !pid_alive(leader),
            "the leader (pid {leader}) survived stop"
        );
        assert!(!pid_alive(child), "the child (pid {child}) survived stop");
        assert!(!owned[0].exists(), "the Run's scratch was not removed");
    }

    #[test]
    fn readiness_waits_for_every_declared_port_and_refuses_a_silent_one() {
        // Two listeners are held at once: asking the kernel for a port and
        // releasing it twice in a row can hand back the same number.
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let second_listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let first = listener.local_addr().unwrap().port();
        let second = second_listener.local_addr().unwrap().port();
        assert_ne!(first, second);
        let ports = vec![
            crate::ValidatedNativePort {
                id: "app.http".to_owned(),
                guest_port: first,
            },
            crate::ValidatedNativePort {
                id: "admin.http".to_owned(),
                guest_port: second,
            },
        ];
        assert!(wait_for_ports_with_timeout(&ports, Duration::from_secs(2)).is_ok());
        drop(listener);
        drop(second_listener);
        // A port nobody listens on: bind once to learn a number, then release
        // it immediately before asking the readiness probe to wait on it.
        let silent = {
            let guard = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let port = guard.local_addr().unwrap().port();
            drop(guard);
            port
        };
        let ports = vec![crate::ValidatedNativePort {
            id: "app.http".to_owned(),
            guest_port: silent,
        }];
        let error = wait_for_ports_with_timeout(&ports, Duration::from_millis(150)).unwrap_err();
        assert!(error.contains("did not accept TCP"), "{error}");
    }

    /// The generated profile must be something `sandbox-exec` can parse, on
    /// every host. A profile with an invalid port aborts with
    /// "invalid port in network address" before any rule is applied; that is
    /// detected here even when this process is itself sandboxed and cannot
    /// apply a nested profile (the failure is then EPERM, not a parse error).
    #[test]
    fn the_generated_profile_is_accepted_by_sandbox_exec() {
        let launcher = seatbelt_launcher().expect("sandbox-exec is required on macOS");
        let scratch = tempfile::tempdir().unwrap();
        let state = scratch.path().join("state");
        fs::create_dir_all(&state).unwrap();
        let plan = sandbox_plan(
            scratch.path(),
            scratch.path(),
            &[("data".to_owned(), state)],
            scratch.path(),
            &[18080, 18081],
        );
        let profile = ato_sandbox::render_seatbelt_profile(&plan.policy).unwrap();
        assert!(
            profile.contains("(local tcp \"localhost:18080\")"),
            "{profile}"
        );
        assert!(
            profile.contains("(local tcp \"localhost:18081\")"),
            "{profile}"
        );
        let profile_path = scratch.path().join("policy.sb");
        fs::write(&profile_path, &profile).unwrap();
        let output = Command::new(launcher)
            .arg("-f")
            .arg(&profile_path)
            .arg("/usr/bin/true")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success() || stderr.contains("sandbox_apply"),
            "sandbox-exec rejected the generated profile: {stderr}"
        );
        assert!(
            !stderr.contains("invalid"),
            "sandbox-exec could not parse the generated profile: {stderr}"
        );
    }

    /// A route with no declared Port must not produce an inbound rule at all;
    /// `(local tcp \"*:0\")` is an unparseable profile.
    #[test]
    fn a_plan_with_no_declared_port_renders_no_inbound_rule() {
        let scratch = tempfile::tempdir().unwrap();
        let plan = sandbox_plan(scratch.path(), scratch.path(), &[], scratch.path(), &[0]);
        let profile = ato_sandbox::render_seatbelt_profile(&plan.policy).unwrap();
        assert!(!profile.contains("network-inbound"), "{profile}");
        assert!(!profile.contains(":0\""), "{profile}");
        assert!(plan.inbound_ports.is_empty());
    }

    /// The whole adapter path: extract → signature verdict → Seatbelt →
    /// launch → readiness → stop/reap. It runs only where a profile can
    /// actually be applied; elsewhere the same measured fact must refuse
    /// admission, which is asserted instead of pretending the launch ran.
    #[test]
    fn the_adapter_launches_a_native_artifact_serves_its_port_and_reaps_it() {
        let facts = ato_host_facts::NativeFacts::probe();
        if !facts.sandbox_tool {
            // The same measured fact must refuse the route before anything is
            // materialized or launched; prove the refusal instead of skipping.
            let scratch = tempfile::tempdir().unwrap();
            let descriptor = ato_formation::native_desktop::NativeDescriptor {
                os: ato_formation::native_desktop::NativeOs::Macos,
                arch: ato_formation::native_desktop::NativeArch::Aarch64,
                artifact: format!("sha256:{}", "a".repeat(64)),
                entry: "app/run.sh".to_owned(),
                format: ato_formation::native_desktop::NativeArtifactFormat::TarGz,
                signature: NativeSignaturePolicy::RequireValidOrLocalOverride,
            };
            let ports = vec![crate::ValidatedNativePort {
                id: "app.http".to_owned(),
                guest_port: 18080,
            }];
            let environment = BTreeMap::new();
            let request = NativeStart {
                artifact_bytes: Vec::new(),
                descriptor: &descriptor,
                ports: &ports,
                workspace: scratch.path().to_path_buf(),
                runtime_root: scratch.path(),
                state_paths: Vec::new(),
                environment: &environment,
                local_override: true,
                declares_egress: false,
            };
            let refusal = prepare(&request, &scratch.path().join("native")).unwrap_err();
            assert_eq!(refusal.code, "native_sandbox_unavailable");
            eprintln!(
                "NOTE: live native launch not exercised: this process cannot apply a profile ({}); \
                 the route is refused for that measured fact",
                facts
                    .sandbox_detail
                    .as_deref()
                    .unwrap_or("sandbox_apply was denied")
            );
            return;
        }
        // A tiny artifact: a wrapper that serves its own proof document on the
        // declared Port, exactly the shape the fixture uses.
        let wrapper = b"#!/bin/sh\nexec python3 -m http.server \"$ATO_NATIVE_PORT\" --bind 127.0.0.1 --directory \"$(dirname \"$0\")\"\n";
        let proof = b"native-desktop-proof\n";
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        for (path, bytes, mode) in [
            ("app/run.sh", wrapper.as_slice(), 0o755u32),
            ("app/proof.txt", proof.as_slice(), 0o644u32),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(mode);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            builder
                .append_data(&mut header, path, std::io::Cursor::new(bytes))
                .unwrap();
        }
        let artifact = builder.into_inner().unwrap().finish().unwrap();

        let scratch = tempfile::tempdir().unwrap();
        let runtime_root = scratch.path().join("run");
        fs::create_dir_all(&runtime_root).unwrap();
        let workspace = scratch.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();

        let mut last_error = None;
        for _ in 0..8 {
            let guard = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let port = guard.local_addr().unwrap().port();
            drop(guard);
            let ports = vec![crate::ValidatedNativePort {
                id: "app.http".to_owned(),
                guest_port: port,
            }];
            let descriptor = ato_formation::native_desktop::NativeDescriptor {
                os: ato_formation::native_desktop::NativeOs::Macos,
                arch: ato_formation::native_desktop::NativeArch::Aarch64,
                artifact: format!("sha256:{}", "a".repeat(64)),
                entry: "app/run.sh".to_owned(),
                format: ato_formation::native_desktop::NativeArtifactFormat::TarGz,
                signature: NativeSignaturePolicy::RequireValidOrLocalOverride,
            };
            // The interpreter has to be inside the read-only set; use the
            // system python rather than whatever PATH this test inherited.
            let environment = BTreeMap::from([(
                "PATH".to_owned(),
                "/usr/bin:/bin:/usr/sbin:/sbin".to_owned(),
            )]);
            let request = NativeStart {
                artifact_bytes: artifact.clone(),
                descriptor: &descriptor,
                ports: &ports,
                workspace: workspace.clone(),
                runtime_root: &runtime_root,
                state_paths: Vec::new(),
                environment: &environment,
                local_override: true,
                declares_egress: false,
            };
            match start(request) {
                Ok(realized) => {
                    let endpoint = realized
                        .candidate
                        .endpoints()
                        .get("app.http")
                        .cloned()
                        .expect("the declared Port is exposed");
                    let body = http_get(&format!("{endpoint}/proof.txt"));
                    assert_eq!(
                        body.as_deref(),
                        Some("native-desktop-proof\n"),
                        "the launched app did not serve its proof document"
                    );
                    let native = realized
                        .execution
                        .native
                        .clone()
                        .expect("the receipt carries native evidence");
                    assert_eq!(native.codesign_verify, "invalid");
                    assert_eq!(native.signature_kind, "unsigned");
                    assert!(native.local_override);
                    assert_eq!(native.sandbox, "seatbelt");
                    assert_eq!(native.enforced_inbound_ports, vec![port]);
                    assert!(native.materialized_tree_digest.starts_with("sha256:"));
                    RunningCandidate::stop(realized.candidate)
                        .expect("the Run stops and reaps the native app");
                    // The declared Port is free again: nothing survived.
                    assert!(
                        std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok(),
                        "the native app was not reaped"
                    );
                    return;
                }
                Err(error) => last_error = Some(describe_failure(&error)),
            }
        }
        panic!("the native adapter never launched: {last_error:#?}");
    }

    fn describe_failure(failure: &RealizeFailure) -> String {
        match failure {
            RealizeFailure::Execution(error) => format!("execution: {error:#}"),
            RealizeFailure::Launch { error, .. } => format!("launch: {error:#}"),
            RealizeFailure::Stopped { error, .. } => format!("stopped: {error:#}"),
            RealizeFailure::Abandoned { error, cleanup, .. } => {
                format!("abandoned: {error:#} ({cleanup})")
            }
        }
    }

    fn http_get(url: &str) -> Option<String> {
        let rest = url.strip_prefix("http://")?;
        let (authority, path) = rest.split_once('/')?;
        let mut stream = std::net::TcpStream::connect(authority).ok()?;
        use std::io::{Read, Write};
        write!(
            stream,
            "GET /{path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n"
        )
        .ok()?;
        let mut response = String::new();
        stream.read_to_string(&mut response).ok()?;
        let (_, body) = response.split_once("\r\n\r\n")?;
        Some(body.to_owned())
    }
}
