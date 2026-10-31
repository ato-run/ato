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
    /// The unreviewed private lane requires an isolated network for the
    /// verification candidate as well as for later Runs.
    pub require_private_isolation: bool,
}

fn refused(code: &str, message: impl Into<String>) -> Option<AttemptFailure> {
    Some(AttemptFailure {
        code: code.to_owned(),
        stage: "admission".to_owned(),
        message: message.into(),
    })
}

fn private_build_supported(
    actions: &[ato_formation::execution::BuildAction],
    toolchains: &BTreeMap<String, String>,
) -> bool {
    actions.iter().all(|action| match action {
        ato_formation::execution::BuildAction::Authored { .. } => false,
        ato_formation::execution::BuildAction::Prerequisite(step) => {
            let runtime = match step.name.as_str() {
                "verify-provisioned-python" => "python",
                "verify-provisioned-node" => "node",
                _ => return false,
            };
            let Some(version) = toolchains.get(runtime) else { return false };
            let parts: Vec<_> = version.split('.').map(str::parse::<u32>).collect();
            let [Ok(major), Ok(minor), Ok(patch)] = parts.as_slice() else { return false };
            let expected = if runtime == "python" {
                vec![format!("{}/bin/python3", ato_formation::intent::python_home(version)),
                    "-I".into(), "-S".into(), "-c".into(),
                    format!("import sys; assert sys.version_info[:3] == ({major}, {minor}, {patch}), 'pinned Python version mismatch'")]
            } else {
                vec![format!("{}/bin/node", ato_formation::intent::node_home(version)), "-e".into(),
                    format!("if (process.versions.node !== '{}') process.exit(1)", version)]
            };
            step.argv == expected && !step.needs_network
                && step.network_phase.is_none()
                && step.env.is_empty()
                && step.cwd_relative.is_empty()
                && step.toolchain_access == ato_formation::intent::ToolchainAccess::ReadOnly
        }
    })
}

impl CandidateRealizer for FormationRealizer<'_> {
    fn admit(&self, profile: &RuntimeProfile) -> Option<AttemptFailure> {
        let planned = self.planned;
        if self.require_private_isolation
            && (self.network != NetworkPolicy::Denied
                || !planned.plan.lane.is_process()
                || !planned.derivation.state.is_empty()
                || planned.derivation.ports.len() != 1
                || planned.derivation.ports[0].protocol != ato_formation::authoring::HTTP_PROTOCOL)
        {
            return refused(
                "private_formation_policy_unsupported",
                "private Formation requires a stateless, single-HTTP-port process with denied build network",
            );
        }
        if self.require_private_isolation
            && !private_build_supported(&planned.plan.actions, &planned.plan.toolchains)
        {
            return refused(
                "private_formation_build_unsupported",
                "private Formation accepts only preinstalled toolchains and no authored build steps",
            );
        }
        if planned.derivation.source_oci.is_some()
            || planned.derivation.runtimes.contains_key("oci.image")
            || (!planned.derivation.requirements.is_empty()
                && (!self.require_private_isolation
                    || planned.derivation.requirements.startup.is_some()
                    || planned
                        .derivation
                        .requirements
                        .within(&crate::admission::private_http_bind_ceiling())
                        .is_err()))
            || !planned.derivation.variable_bindings.is_empty()
            || !planned.derivation.runtime_port_operations.is_empty()
            || planned.derivation.steps.iter().any(|s| {
                matches!(
                    s.network,
                    ato_formation::authoring::StepNetwork::ScopedDependencies
                        | ato_formation::authoring::StepNetwork::ScopedBuild
                        | ato_formation::authoring::StepNetwork::ScopedRuntime
                )
            })
        {
            return refused(
                "exploration_requirement_enforcement_unavailable",
                "the legacy Formation realizer has no phase-scoped grant broker; host-unrestricted networking cannot satisfy scoped requirements",
            );
        }
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
        CandidateLauncher {
            planned: self.planned,
            shim: self.shim,
            network: self.network,
            require_private_isolation: self.require_private_isolation,
        }
        .realize(executed, attempt_id, attempt_root)
    }
}

#[cfg(test)]
mod private_build_tests {
    use super::*;
    use ato_formation::execution::BuildAction;
    use ato_formation::intent::{BuildStepV1, ToolchainAccess};

    #[test]
    fn private_build_rejects_authored_and_privileged_work() {
        let mut step = BuildStepV1 {
            name: "verify-provisioned-python".into(),
            argv: vec!["/opt/ato/toolchains/python/3.12.7/bin/python3".into(), "-I".into(), "-S".into(), "-c".into(),
                "import sys; assert sys.version_info[:3] == (3, 12, 7), 'pinned Python version mismatch'".into()],
            needs_network: false,
            network_phase: None,
            cwd_relative: String::new(),
            env: BTreeMap::new(),
            toolchain_access: ToolchainAccess::ReadOnly,
        };
        let tools = BTreeMap::from([("python".to_owned(), "3.12.7".to_owned())]);
        assert!(private_build_supported(&[], &tools));
        assert!(private_build_supported(
            &[BuildAction::Prerequisite(step.clone())],
            &tools
        ));
        assert!(!private_build_supported(
            &[BuildAction::Authored { step: 0 }],
            &tools
        ));
        let mut forged = step.clone();
        forged.argv = vec!["/app/attacker".into()];
        assert!(!private_build_supported(
            &[BuildAction::Prerequisite(forged)],
            &tools
        ));
        step.needs_network = true;
        assert!(!private_build_supported(
            &[BuildAction::Prerequisite(step.clone())],
            &tools
        ));
        step.needs_network = false;
        step.toolchain_access = ToolchainAccess::Provision;
        assert!(!private_build_supported(
            &[BuildAction::Prerequisite(step)],
            &tools
        ));
    }
}

/// What a process launch actually resolved: the executable path and, when the
/// path names one, its toolchain release. Fed to the receipt's existing
/// execution evidence; `None` when the realizer launched no process.
pub(crate) type ResolvedRuntime = Option<(String, Option<String>)>;

/// Shared launch boundary for built and retained candidates. No build/source
/// authority lives here; callers supply an already validated materialization.
pub(crate) struct CandidateLauncher<'a> {
    pub planned: &'a PlannedCandidate,
    pub shim: &'a Path,
    pub network: NetworkPolicy,
    pub require_private_isolation: bool,
}

#[derive(Default)]
pub(crate) struct RuntimeBindings<'a> {
    pub gate: Option<&'a Path>,
    pub variables: &'a [crate::variables::ResolvedVariable],
    pub state: Option<&'a crate::state_bindings::VerificationStateBindings<'a>>,
}
impl CandidateLauncher<'_> {
    pub(crate) fn realize(
        &self,
        executed: ExecutedCandidate,
        attempt_id: &str,
        attempt_root: &Path,
    ) -> Result<Realized, RealizeFailure> {
        self.realize_controlled(
            executed,
            attempt_id,
            attempt_root,
            &RuntimeBindings::default(),
            None,
        )
    }

    pub(crate) fn realize_controlled(
        &self,
        executed: ExecutedCandidate,
        attempt_id: &str,
        attempt_root: &Path,
        bindings: &RuntimeBindings<'_>,
        control: Option<&crate::control::ExecutionControl>,
    ) -> Result<Realized, RealizeFailure> {
        let _launch = control
            .map(|c| c.phase(crate::control::AttemptPhase::Launch))
            .transpose()?;
        let (candidate, mut evidence, realization, runtime) = match &executed {
            ExecutedCandidate::Process { workspace_root } => {
                let (candidate, evidence, runtime) = self.realize_process(
                    workspace_root,
                    attempt_id,
                    attempt_root,
                    bindings,
                    control,
                )?;
                (candidate, evidence, "process", runtime)
            }
            ExecutedCandidate::StaticWeb { output } => {
                if bindings.state.is_some() {
                    return Err(RealizeFailure::Launch {
                        error: anyhow::anyhow!("verification_state_requires_process_route"),
                        evidence: None,
                    });
                }
                let (candidate, evidence) =
                    realize_static(output, self.planned).map_err(|error| {
                        RealizeFailure::Launch {
                            error,
                            evidence: None,
                        }
                    })?;
                (candidate, evidence, "static_web", None)
            }
        };
        if let Err(error) = crate::port_operations::execute(
            &self.planned.derivation,
            candidate.endpoints(),
            bindings.variables,
            bindings.gate.is_some(),
            control,
            &mut evidence.port_operations,
        ) {
            let _cleanup = control.map(|c| c.cleanup());
            let stopped = candidate.stop();
            let class = crate::realize::StopClass::of(&stopped);
            if let crate::realize::StopClass::Unconfirmed { reason, resources } = class {
                return Err(RealizeFailure::Abandoned {
                    error,
                    cleanup: reason,
                    resources,
                });
            }
            evidence.destroyed = stopped.is_ok();
            return Err(RealizeFailure::Stopped {
                error,
                evidence: Box::new(evidence),
                stop: class,
            });
        }
        let endpoint = candidate
            .endpoints()
            .values()
            .next()
            .map(|base| format!("{base}/"));
        Ok(Realized {
            candidate,
            evidence: Some(evidence),
            execution: execution_evidence(realization, endpoint, runtime),
            kept: Some(executed),
        })
    }
}

impl CandidateLauncher<'_> {
    /// Launch a process candidate in a temporary realization: a disposable
    /// copy of the build output, read-only at /app, no egress.
    fn realize_process(
        &self,
        workspace_root: &Path,
        attempt_id: &str,
        attempt_root: &Path,
        bindings: &RuntimeBindings<'_>,
        control: Option<&crate::control::ExecutionControl>,
    ) -> Result<
        (
            Box<dyn RunningCandidate>,
            RealizationEvidence,
            ResolvedRuntime,
        ),
        RealizeFailure,
    > {
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
            port_operations: vec![],
            executor: "runtime-process".to_owned(),
            containment: "bwrap+landlock".to_owned(),
            workspace: "disposable-copy, read-only at /app; /tmp is tmpfs".to_owned(),
            // The Runtime's process policy, whatever the build was allowed:
            // no egress, TCP bind only on the allocated host ports. A
            // `dependency-resolution` request widens the BUILD, never the run.
            build_network: crate::attempt::network_name(self.network).to_owned(),
            candidate_network: if self.require_private_isolation {
                "isolated namespace; declared ingress; no egress"
            } else if bindings.gate.is_some() {
                "isolated namespace; declared ingress; phase-scoped HTTPS egress broker"
            } else {
                "no-egress; tcp bind limited to allocated host ports"
            }
            .to_owned(),
            endpoints: BTreeMap::new(),
            destroyed: false,
        };
        let request = TemporaryRealizationRequest {
            workspace: workspace_root,
            scratch: &scratch,
            derivation: &planned.derivation,
            plan: &planned.plan,
            ports: &ports,
            shim: self.shim,
            attempt_id,
        };
        let launched = if self.require_private_isolation {
            if bindings.gate.is_some() || !bindings.variables.is_empty() || bindings.state.is_some()
            {
                Err(anyhow::anyhow!(
                    "private Formation does not accept external bindings"
                ))
            } else {
                TemporaryRealization::launch_private_isolated(&request, control)
            }
        } else {
            TemporaryRealization::launch_controlled_with_state(
                &request,
                bindings.gate,
                bindings.variables,
                bindings.state,
                control,
            )
        };
        let realization = match launched {
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
        // What the launch actually resolved, for the receipt's existing
        // execution evidence fields. A bare `node` recorded as "node" would
        // not say which release ran; the resolved path and its toolchain
        // version do.
        let runtime = realization.runtime_executable().map(|executable| {
            (
                executable.to_owned(),
                realization.runtime_version().map(str::to_owned),
            )
        });
        Ok((
            Box::new(ProcessCandidate {
                realization: Some(realization),
                endpoints,
            }),
            evidence,
            runtime,
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
        port_operations: vec![],
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
    runtime: ResolvedRuntime,
) -> VerificationExecutionEvidence {
    let (runtime_executable, runtime_version) = match runtime {
        Some((executable, version)) => (Some(executable), version),
        None => (None, None),
    };
    VerificationExecutionEvidence {
        realization: realization.to_owned(),
        runtime_executable,
        runtime_version,
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
