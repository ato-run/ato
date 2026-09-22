//! Temporary realization: bring a candidate up on the local Runtime, measure
//! it, then destroy it (ADR-019).
//!
//! A Formation is not done until the candidate is observed satisfying the
//! Contract. For a process lane that means a real process on a real port, so
//! the candidate is launched — but NOT by a Formation-specific spawn. It goes
//! through the Runtime's own process executor:
//!
//! ```text
//! RuntimeLaunchSpecV1 + ResolvedRuntimeLaunchContext
//!     -> launch_process_with   (bwrap namespaces, Landlock shim, cleared env,
//!                               allocated host port, isolated process group)
//!     -> wait_until_ready      (the Runtime's loopback readiness probe)
//!     -> LaunchedProcess::stop (TERM -> KILL on the group, proven gone)
//! ```
//!
//! Only the lifecycle differs from a Run. There is no lease, no state
//! attachment, no stable endpoint and no record pipeline: the realization
//! exists to be measured inside one attempt, and [`TemporaryRealization`] is
//! destroyed before the attempt returns — on every path, because `Drop` does
//! it when nothing else did.
//!
//! The workspace the candidate sees is a DISPOSABLE COPY of the build output.
//! The Runtime mounts it read-only at `/app` and gives the candidate a tmpfs
//! `/tmp`; whatever the candidate writes goes with the realization, and the
//! artifact that is kept is packed from the untouched build output instead.

use std::collections::BTreeMap;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ato_connected_realization_worker::runtime_launch::process_executor::{
    LaunchedProcess, LoopbackReadinessProbe, ProcessLaunchHost, launch_process_with,
    wait_until_ready,
};
use ato_connected_realization_worker::runtime_launch::resolved::{
    ResolvedEndpoint, ResolvedRuntimeLaunchContext,
};
use ato_connected_realization_worker::runtime_launch::sandbox::endpoint_port_env_name;
use ato_formation::intent::ProgramIntentV1;
use ato_formation::verify::RuntimeHttpObservation;
use ato_ipc::runtime_launch::{
    EndpointAllocationV1, EndpointV1, LaunchContextV1, LaunchRealizationV1, LaunchWorkspaceV1,
    LifecycleV1, ProcessRealizationV1, PublicEnvV1, RUNTIME_LAUNCH_SPEC_V1_PROTOCOL, ReadinessV1,
    RuntimeLaunchSpecV1,
};

use crate::job::copy_tree;

/// How long a candidate gets to come up before the attempt is failed.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);
/// Per-request budget once the candidate is up.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Bodies larger than this are not hashed into evidence.
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
/// How much of the candidate's output a failure message carries.
const OUTPUT_TAIL_BYTES: u64 = 4 * 1024;
/// The teardown bounds handed to the Runtime's stop.
const LIFECYCLE: LifecycleV1 = LifecycleV1 {
    graceful_shutdown_ms: 2_000,
    force_kill_after_ms: 4_000,
};

/// A port the Contract observes, as the Derivation declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredPort {
    /// The Contract's port id (`app.http`).
    pub port_id: String,
    /// The port the Derivation says the workload listens on.
    pub guest_port: u16,
}

/// An HTTP observation the Contract needs, by logical port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredObservation {
    pub port_id: String,
    pub path: String,
}

/// What to realize, temporarily.
pub struct TemporaryRealizationRequest<'a> {
    /// The immutable build output. Copied, never mounted: the copy is what
    /// the candidate sees, so nothing it does can reach this tree.
    pub workspace: &'a Path,
    /// Scratch owned by this realization and removed with it.
    pub scratch: &'a Path,
    pub intent: &'a ProgramIntentV1,
    pub ports: &'a [RequiredPort],
    /// The binary bwrap re-enters as `sandbox-exec`.
    pub shim: &'a Path,
    /// Correlates the realization with its attempt.
    pub attempt_id: &'a str,
}

/// Where the verifier reaches a logical port.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RealizedEndpoint {
    pub port_id: String,
    pub guest_port: u16,
    pub host_port: u16,
}

/// A running candidate, alive only while this value is.
pub struct TemporaryRealization {
    launched: Option<LaunchedProcess>,
    endpoints: Vec<RealizedEndpoint>,
    scratch: PathBuf,
    output: PathBuf,
}

impl TemporaryRealization {
    /// Launch the candidate through the Runtime's process executor and wait
    /// until every required port accepts connections.
    pub fn launch(request: &TemporaryRealizationRequest<'_>) -> Result<Self> {
        let intent = request.intent;
        if !intent.cwd_relative.is_empty() {
            // The Runtime's contained launch starts every workload at the
            // workspace root. Launching this candidate there would observe a
            // different process than the Derivation describes.
            bail!(
                "the Derivation starts in {:?}, and the local Runtime's process executor starts \
                 workloads at the workspace root",
                intent.cwd_relative
            );
        }
        if intent.launch_argv.is_empty() {
            bail!("the intent declares no launch argv");
        }

        // Owned from the first byte written: any error below drops it, and
        // dropping it removes the scratch and stops anything launched.
        let mut realization = Self {
            launched: None,
            endpoints: Vec::new(),
            scratch: request.scratch.to_path_buf(),
            output: request.scratch.join("candidate.log"),
        };
        let workspace_root = request.scratch.join("workspace");
        std::fs::create_dir_all(&workspace_root)
            .context("cannot create the realization workspace")?;
        copy_tree(request.workspace, &workspace_root)
            .context("cannot copy the build output into the realization")?;
        let runtime_root = request.scratch.join("runtime");
        std::fs::create_dir_all(&runtime_root)
            .context("cannot create the realization runtime root")?;

        // ── ports ───────────────────────────────────────────────────────────
        //
        // The Derivation names a guest port; the host port is the Runtime's to
        // choose. A process realization has no NAT, so the allocated port is
        // what the workload must bind: the Runtime exports it as
        // ATO_ENDPOINT_<NAME>_PORT and Landlock admits a bind on it and on
        // nothing else. An argv or env value that states the guest port
        // verbatim is lowered to the allocated one — the only translation a
        // NAT-less realization has.
        let mut endpoints = Vec::new();
        let mut resolved = Vec::new();
        let mut declared = Vec::new();
        for port in request.ports {
            if endpoints
                .iter()
                .any(|endpoint: &RealizedEndpoint| endpoint.port_id == port.port_id)
            {
                continue;
            }
            let host_port = allocate_host_port()?;
            let name = endpoint_name(&port.port_id);
            declared.push(EndpointV1 {
                name: name.clone(),
                protocol: "http".to_owned(),
                guest_port: Some(port.guest_port),
                allocation: EndpointAllocationV1::Automatic,
                preferred_port: None,
            });
            resolved.push(ResolvedEndpoint {
                name,
                guest_port: Some(port.guest_port),
                host_port,
            });
            endpoints.push(RealizedEndpoint {
                port_id: port.port_id.clone(),
                guest_port: port.guest_port,
                host_port,
            });
        }
        let lower = |value: &str| lower_port(value, &endpoints);

        let argv: Vec<String> = intent.launch_argv.iter().map(|arg| lower(arg)).collect();
        let public_env: BTreeMap<String, String> = intent
            .public_env
            .iter()
            .map(|(name, value)| (name.clone(), lower(value)))
            .collect();

        let spec = RuntimeLaunchSpecV1 {
            protocol: RUNTIME_LAUNCH_SPEC_V1_PROTOCOL.to_owned(),
            context: LaunchContextV1 {
                run_id: format!("formation-{}", request.attempt_id),
                compute_id: "formation".to_owned(),
                compute_schema_id: "formation".to_owned(),
                compute_instance_id: format!("formation-{}", request.attempt_id),
            },
            workspace: LaunchWorkspaceV1 {
                materialization_ref: format!("formation-attempt:{}", request.attempt_id),
                cwd_relative: String::new(),
            },
            realization: LaunchRealizationV1::Process(ProcessRealizationV1 {
                argv,
                executable: None,
            }),
            public_env: public_env
                .iter()
                .map(|(name, value)| PublicEnvV1 {
                    name: name.clone(),
                    value: value.clone(),
                })
                .collect(),
            secret_grants: Vec::new(),
            state_attachments: Vec::new(),
            endpoints: declared,
            readiness: ReadinessV1::Process {
                timeout_ms: LAUNCH_TIMEOUT.as_millis() as u64,
            },
            lifecycle: LIFECYCLE,
        };
        let context = ResolvedRuntimeLaunchContext::new(
            workspace_root,
            "",
            public_env,
            Vec::new(),
            Vec::new(),
            resolved,
        )
        .map_err(|error| anyhow::anyhow!("{error}"))?;

        let launched = launch_process_with(
            &spec,
            &context,
            &ProcessLaunchHost {
                shim: request.shim.to_path_buf(),
                runtime_root,
                output: Some(realization.output.clone()),
            },
        )?;
        realization.launched = Some(launched);
        realization.endpoints = endpoints;

        // Readiness: every observed port accepts a connection. The Runtime's
        // probe and its "exited before ready" check, one endpoint at a time.
        let probe = LoopbackReadinessProbe::new(http_client()?);
        for endpoint in &spec.endpoints {
            let mut per_endpoint = spec.clone();
            per_endpoint.readiness = ReadinessV1::Tcp {
                endpoint_name: endpoint.name.clone(),
                timeout_ms: LAUNCH_TIMEOUT.as_millis() as u64,
            };
            let launched = realization
                .launched
                .as_mut()
                .expect("launched until destroyed");
            if let Err(error) = wait_until_ready(&per_endpoint, &context, launched, &probe) {
                let tail = realization.output_tail();
                return Err(error.context(format!("candidate output: {tail}")));
            }
        }
        Ok(realization)
    }

    /// Where each logical port is reachable.
    pub fn endpoints(&self) -> &[RealizedEndpoint] {
        &self.endpoints
    }

    /// GET every required path through the endpoint its logical port maps to.
    pub fn observe(&self, required: &[RequiredObservation]) -> Result<Vec<RuntimeHttpObservation>> {
        let client = http_client()?;
        let mut observed = Vec::with_capacity(required.len());
        for observation in required {
            let endpoint = self
                .endpoints
                .iter()
                .find(|endpoint| endpoint.port_id == observation.port_id)
                .with_context(|| format!("port {} was not realized", observation.port_id))?;
            let url = format!(
                "http://127.0.0.1:{}{}",
                endpoint.host_port, observation.path
            );
            let response = client
                .get(&url)
                .send()
                .with_context(|| format!("GET {} failed", observation.path))?;
            let status = response.status().as_u16();
            let body = response.bytes().context("cannot read the response body")?;
            if body.len() > MAX_BODY_BYTES {
                bail!(
                    "GET {} returned more than {MAX_BODY_BYTES} bytes",
                    observation.path
                );
            }
            observed.push(RuntimeHttpObservation::from_response(
                observation.port_id.clone(),
                "GET",
                observation.path.clone(),
                status,
                &body,
            ));
        }
        Ok(observed)
    }

    /// Stop the candidate through the Runtime and discard everything it
    /// touched. The error is the Runtime's own: a process group that
    /// survived termination is reported, not assumed gone.
    pub fn destroy(mut self) -> Result<()> {
        self.teardown()
    }

    fn teardown(&mut self) -> Result<()> {
        let stopped = match self.launched.take() {
            Some(launched) => launched.stop(&LIFECYCLE).map(|_| ()),
            None => Ok(()),
        };
        let removed = if self.scratch.exists() {
            remove_scratch(&self.scratch)
        } else {
            Ok(())
        };
        stopped.and(removed)
    }

    fn output_tail(&self) -> String {
        let Ok(mut file) = std::fs::File::open(&self.output) else {
            return "(none)".to_owned();
        };
        let length = file.metadata().map(|meta| meta.len()).unwrap_or(0);
        let _ = file.seek(SeekFrom::Start(length.saturating_sub(OUTPUT_TAIL_BYTES)));
        let mut tail = String::new();
        let _ = file.read_to_string(&mut tail);
        let tail = tail.trim();
        if tail.is_empty() {
            "(none)".to_owned()
        } else {
            tail.to_owned()
        }
    }
}

impl Drop for TemporaryRealization {
    fn drop(&mut self) {
        // Every path out of an attempt ends here if `destroy` was not reached:
        // an error, a timeout, a verifier failure, an unwinding panic.
        if let Err(error) = self.teardown() {
            eprintln!("[formation] temporary realization teardown failed: {error:#}");
        }
    }
}

/// The Runtime's endpoint name for a Contract port id. `app.http` becomes
/// `app_http`, exported as `ATO_ENDPOINT_APP_HTTP_PORT`.
fn endpoint_name(port_id: &str) -> String {
    port_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// The environment variable a candidate reads for a port's host port.
pub fn endpoint_env_name(port_id: &str) -> String {
    endpoint_port_env_name(&endpoint_name(port_id))
}

/// Replace a value that states a guest port verbatim — `8080`, or the
/// right-hand side of `--port=8080` / `host:8080` — with its host port.
fn lower_port(value: &str, endpoints: &[RealizedEndpoint]) -> String {
    for endpoint in endpoints {
        let guest = endpoint.guest_port.to_string();
        let host = endpoint.host_port.to_string();
        if value == guest {
            return host;
        }
        for separator in ['=', ':'] {
            if let Some(prefix) = value.strip_suffix(&guest)
                && prefix.ends_with(separator)
            {
                return format!("{prefix}{host}");
            }
        }
    }
    value.to_owned()
}

/// A free loopback port, chosen by the kernel.
///
/// Released before the candidate binds it: a process realization binds the
/// host port itself, so the window between release and bind is the price of
/// having no NAT. A collision there fails readiness rather than observing
/// somebody else, because the candidate's own bind is what fails.
fn allocate_host_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .context("cannot allocate a loopback port for the candidate")?;
    Ok(listener.local_addr()?.port())
}

fn http_client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        // A candidate has no business redirecting its Contract observation
        // somewhere else; what it answers directly is what is measured.
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

/// Remove the realization scratch. The copy is plain files the candidate
/// could only read, but a build may have produced read-only directories.
fn remove_scratch(path: &Path) -> Result<()> {
    if std::fs::remove_dir_all(path).is_ok() {
        return Ok(());
    }
    make_writable(path);
    std::fs::remove_dir_all(path)
        .with_context(|| format!("cannot remove realization scratch {}", path.display()))
}

fn make_writable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if let Ok(metadata) = std::fs::symlink_metadata(path) {
            if metadata.is_symlink() {
                return;
            }
            let mut permissions = metadata.permissions();
            permissions.set_mode(permissions.mode() | 0o700);
            let _ = std::fs::set_permissions(path, permissions);
            if metadata.is_dir()
                && let Ok(entries) = std::fs::read_dir(path)
            {
                for entry in entries.flatten() {
                    make_writable(&entry.path());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoints() -> Vec<RealizedEndpoint> {
        vec![RealizedEndpoint {
            port_id: "app.http".to_owned(),
            guest_port: 8080,
            host_port: 41234,
        }]
    }

    #[test]
    fn a_verbatim_guest_port_is_lowered_to_the_host_port() {
        let endpoints = endpoints();
        assert_eq!(lower_port("8080", &endpoints), "41234");
        assert_eq!(lower_port("--port=8080", &endpoints), "--port=41234");
        assert_eq!(lower_port("0.0.0.0:8080", &endpoints), "0.0.0.0:41234");
    }

    #[test]
    fn a_value_that_merely_contains_the_digits_is_left_alone() {
        let endpoints = endpoints();
        assert_eq!(lower_port("18080", &endpoints), "18080");
        assert_eq!(lower_port("/app/8080", &endpoints), "/app/8080");
        assert_eq!(lower_port("app.py", &endpoints), "app.py");
    }

    #[test]
    fn endpoint_names_follow_the_runtime_abi() {
        assert_eq!(endpoint_name("app.http"), "app_http");
        assert_eq!(endpoint_env_name("app.http"), "ATO_ENDPOINT_APP_HTTP_PORT");
    }
}
