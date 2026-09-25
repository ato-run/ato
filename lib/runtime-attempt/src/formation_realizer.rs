//! The Formation realizer: build the candidate from source, then bring it
//! up to be observed — a process candidate in a temporary realization, a
//! static candidate served from the bundle it produced.
//!
//! Everything here is how a Formation candidate comes to be running. What
//! is observed and decided about it is `run_attempt`'s.

use std::any::Any;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ato_formation::authoring::HTTP_CONTRACT_VERIFIER;
use ato_formation::request::{AttemptFailure, RealizationEvidence, RuntimeProfile};
use ato_formation::verify::VerificationExecutionEvidence;

use crate::build_sandbox::{NetworkPolicy, TOOLCHAIN_ROOT};
use crate::ephemeral::{RequiredPort, TemporaryRealization, TemporaryRealizationRequest};
use crate::executor::{AttemptExecution, AttemptExecutor, ExecutedCandidate};
use crate::plan::PlannedCandidate;
use crate::realize::{CandidateRealizer, RealizeFailure, Realized, RunningCandidate};
use crate::static_server::StaticApplicationServer;

/// Realize a planned Formation candidate: `builder` executes its build
/// plan; this starts what the build produced.
pub struct FormationRealizer<'a> {
    pub planned: &'a PlannedCandidate,
    pub source_root: &'a Path,
    pub builder: &'a dyn AttemptExecutor,
    /// The binary bwrap re-enters as `sandbox-exec`.
    pub shim: &'a Path,
    /// What the build may reach. The candidate itself never gets egress.
    pub network: NetworkPolicy,
}

fn refused(code: &str, message: impl Into<String>) -> Option<AttemptFailure> {
    Some(AttemptFailure {
        code: code.to_owned(),
        stage: "admission".to_owned(),
        message: message.into(),
    })
}

impl CandidateRealizer for FormationRealizer<'_> {
    fn admit(&self, profile: &RuntimeProfile) -> Option<AttemptFailure> {
        let planned = self.planned;
        if !planned.plan.actions.is_empty()
            && profile.get("formation.containment") != Some("bwrap+landlock")
        {
            return refused(
                "runtime_cannot_contain_build",
                "this candidate needs build steps and this Runtime cannot contain one (no \
                 bwrap); it was not attempted",
            );
        }
        if !planned.plan.actions.is_empty() && profile.get("formation.toolchain_root").is_none() {
            return refused(
                "runtime_has_no_toolchain_root",
                format!(
                    "this candidate's build provisions toolchains into {TOOLCHAIN_ROOT}, which \
                     this Runtime does not have; it was not attempted"
                ),
            );
        }
        if planned.plan.lane.is_process()
            && profile.get("formation.containment") != Some("bwrap+landlock")
        {
            return refused(
                "runtime_cannot_contain_candidate",
                "verifying this candidate means running it, and this Runtime cannot contain a \
                 process (no bwrap); it was not attempted",
            );
        }
        if planned.plan.needs_network(&planned.derivation) && self.network == NetworkPolicy::Denied
        {
            return refused(
                "network_denied",
                "this candidate's build resolves dependencies from the network and the request \
                 denies it; it was not attempted",
            );
        }
        None
    }

    fn realize(&self, attempt_id: &str, attempt_root: &Path) -> Result<Realized, RealizeFailure> {
        let executed = self.builder.execute(&AttemptExecution {
            attempt_id,
            candidate: self.planned,
            source_root: self.source_root,
            attempt_root,
        })?;
        let (candidate, evidence, realization) = match &executed {
            ExecutedCandidate::Process { workspace_root } => {
                let (candidate, evidence) =
                    self.realize_process(workspace_root, attempt_id, attempt_root)?;
                (candidate, evidence, "process")
            }
            ExecutedCandidate::StaticWeb { output } => {
                let (candidate, evidence) =
                    realize_static(output, self.planned).map_err(|error| {
                        RealizeFailure::Launch {
                            error,
                            evidence: None,
                        }
                    })?;
                (candidate, evidence, "static_web")
            }
        };
        let endpoint = candidate
            .endpoints()
            .values()
            .next()
            .map(|base| format!("{base}/"));
        Ok(Realized {
            candidate,
            evidence: Some(evidence),
            execution: execution_evidence(realization, endpoint),
            kept: Some(executed),
        })
    }
}

impl FormationRealizer<'_> {
    /// Launch a process candidate in a temporary realization: a disposable
    /// copy of the build output, read-only at /app, no egress.
    fn realize_process(
        &self,
        workspace_root: &Path,
        attempt_id: &str,
        attempt_root: &Path,
    ) -> Result<(Box<dyn RunningCandidate>, RealizationEvidence), RealizeFailure> {
        let planned = self.planned;
        let mut ports: Vec<RequiredPort> = Vec::new();
        for port_id in http_ports(planned) {
            let Some(guest_port) = planned
                .derivation
                .ports
                .iter()
                .find(|port| port.id == port_id)
                .and_then(|port| port.guest_port)
            else {
                continue;
            };
            if !ports.iter().any(|port| port.port_id == port_id) {
                ports.push(RequiredPort {
                    port_id,
                    guest_port,
                });
            }
        }
        let scratch = attempt_root.join("realization");
        let mut evidence = RealizationEvidence {
            executor: "runtime-process".to_owned(),
            containment: "bwrap+landlock".to_owned(),
            workspace: "disposable-copy, read-only at /app; /tmp is tmpfs".to_owned(),
            // The Runtime's process policy, whatever the build was allowed:
            // no egress, TCP bind only on the allocated host ports. A
            // `dependency-resolution` request widens the BUILD, never the run.
            build_network: crate::attempt::network_name(self.network).to_owned(),
            candidate_network: "no-egress; tcp bind limited to allocated host ports".to_owned(),
            endpoints: BTreeMap::new(),
            destroyed: false,
        };
        let realization = match TemporaryRealization::launch(&TemporaryRealizationRequest {
            workspace: workspace_root,
            scratch: &scratch,
            derivation: &planned.derivation,
            plan: &planned.plan,
            ports: &ports,
            shim: self.shim,
            attempt_id,
        }) {
            Ok(realization) => realization,
            Err(error) => {
                // Dropped on the error path: gone unless the Runtime said
                // otherwise, which it did loudly in the log.
                evidence.destroyed = !scratch.exists();
                return Err(RealizeFailure::Launch {
                    error,
                    evidence: Some(Box::new(evidence)),
                });
            }
        };
        evidence.endpoints = realization
            .endpoints()
            .iter()
            .map(|endpoint| {
                (
                    endpoint.port_id.clone(),
                    if endpoint.host_port == endpoint.guest_port {
                        format!(
                            "guest {} -> host {}",
                            endpoint.guest_port, endpoint.host_port
                        )
                    } else {
                        format!(
                            "guest {} -> host {} (guest port in use; carried by {})",
                            endpoint.guest_port,
                            endpoint.host_port,
                            crate::ephemeral::endpoint_env_name(&endpoint.port_id)
                        )
                    },
                )
            })
            .collect();
        let endpoints = realization
            .endpoints()
            .iter()
            .map(|endpoint| {
                (
                    endpoint.port_id.clone(),
                    format!("http://127.0.0.1:{}", endpoint.host_port),
                )
            })
            .collect();
        Ok((
            Box::new(ProcessCandidate {
                realization: Some(realization),
                endpoints,
            }),
            evidence,
        ))
    }
}

/// Serve a static candidate's produced bundle on loopback — the bytes that
/// would be kept, requested like any other candidate.
fn realize_static(
    output: &crate::static_lane::StaticFormationOutput,
    planned: &PlannedCandidate,
) -> Result<(Box<dyn RunningCandidate>, RealizationEvidence)> {
    let manifest: ato_materializer_static_web::StaticWebManifestV1 =
        serde_json::from_slice(&output.bundle.manifest_bytes)
            .context("read the produced static web manifest")?;
    let blobs = output.bundle.bundle_root.join("blobs");
    let mut routes = BTreeMap::new();
    for (path, file) in &manifest.files {
        let (algorithm, hex) = file
            .blob
            .split_once(':')
            .with_context(|| format!("{path}: blob {} is not a digest", file.blob))?;
        let location: PathBuf = blobs.join(algorithm).join(hex);
        routes.insert(format!("/{path}"), (location, file.media_type.clone()));
    }
    let server = serve_off_pending_ports(
        routes,
        format!("/{}", manifest.entry_path),
        manifest.routing.spa_fallback,
    )?;
    let base = server.base_url();
    let mut evidence = RealizationEvidence {
        executor: "runtime-static-loopback".to_owned(),
        containment: "none; serves the produced bundle's files, runs nothing".to_owned(),
        workspace: "the produced static web bundle, read-only".to_owned(),
        build_network: "n/a".to_owned(),
        candidate_network: "loopback only".to_owned(),
        endpoints: BTreeMap::new(),
        destroyed: false,
    };
    let mut endpoints = BTreeMap::new();
    for port_id in http_ports(planned) {
        if planned
            .derivation
            .ports
            .iter()
            .any(|port| port.id == port_id)
        {
            evidence
                .endpoints
                .insert(port_id.clone(), "loopback static server".to_owned());
            endpoints.insert(port_id, base.clone());
        }
    }
    Ok((
        Box::new(StaticCandidate {
            server: Some(server),
            endpoints,
        }),
        evidence,
    ))
}

/// A loopback static server, never on a port a process realization chose
/// and has not bound yet: holding it would make that candidate fail to bind.
pub fn serve_off_pending_ports(
    routes: BTreeMap<String, (PathBuf, String)>,
    entry_route: String,
    spa_fallback: bool,
) -> Result<StaticApplicationServer> {
    serve_state_off_pending_ports(routes, entry_route, spa_fallback, None)
}

/// [`serve_off_pending_ports`] with the in-page state of a static
/// application.
pub fn serve_state_off_pending_ports(
    routes: BTreeMap<String, (PathBuf, String)>,
    entry_route: String,
    spa_fallback: bool,
    state: Option<crate::static_server::StaticApplicationState>,
) -> Result<StaticApplicationServer> {
    for _ in 0..64 {
        let server = StaticApplicationServer::listen(
            routes.clone(),
            entry_route.clone(),
            spa_fallback,
            state.clone(),
        )
        .map_err(|error| anyhow::anyhow!("cannot serve the static candidate: {error}"))?;
        if !crate::ephemeral::port_is_pending(server.address().port()) {
            return Ok(server);
        }
    }
    anyhow::bail!("cannot find a loopback port for the static candidate")
}

/// The ports the Contract's HTTP observations name.
fn http_ports(planned: &PlannedCandidate) -> Vec<String> {
    planned
        .contract
        .requirements
        .iter()
        .filter(|requirement| requirement.verifier == HTTP_CONTRACT_VERIFIER)
        .filter(|requirement| requirement.method.as_deref().is_none_or(|m| m == "GET"))
        .filter_map(|requirement| requirement.port.clone())
        .collect()
}

fn execution_evidence(
    realization: &str,
    endpoint: Option<String>,
) -> VerificationExecutionEvidence {
    VerificationExecutionEvidence {
        realization: realization.to_owned(),
        runtime_executable: None,
        runtime_version: None,
        pid: None,
        container_id: None,
        image: None,
        // A Formation attempt names the platform it verified on.
        platform: Some(format!(
            "{}/{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )),
        endpoint,
        run_id: None,
        lease_id: None,
        attempt_id: None,
        request_id: None,
        dependency_fetches: Vec::new(),
        portability_profile: None,
        embedded_oci_image_loaded: None,
        services: Vec::new(),
    }
}

/// A process candidate in a temporary realization.
struct ProcessCandidate {
    realization: Option<TemporaryRealization>,
    endpoints: BTreeMap<String, String>,
}

impl RunningCandidate for ProcessCandidate {
    fn endpoints(&self) -> &BTreeMap<String, String> {
        &self.endpoints
    }

    fn exited(&mut self) -> Result<Option<String>> {
        // The realization waited for readiness before returning; a candidate
        // that exits later fails its observation instead.
        Ok(None)
    }

    fn stop(mut self: Box<Self>) -> Result<()> {
        match self.realization.take() {
            Some(realization) => realization
                .destroy()
                .context("the candidate could not be destroyed"),
            None => Ok(()),
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A static candidate's loopback server.
struct StaticCandidate {
    server: Option<StaticApplicationServer>,
    endpoints: BTreeMap<String, String>,
}

impl RunningCandidate for StaticCandidate {
    fn endpoints(&self) -> &BTreeMap<String, String> {
        &self.endpoints
    }

    fn exited(&mut self) -> Result<Option<String>> {
        Ok(None)
    }

    fn stop(mut self: Box<Self>) -> Result<()> {
        drop(self.server.take());
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
