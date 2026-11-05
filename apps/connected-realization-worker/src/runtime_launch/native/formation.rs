//! Native Pixel candidate for the common Formation attempt.
//!
//! The caller provides a validated portable bundle and trusted Runner helper.
//! The attempt owns the slot until its journal and physical resources are
//! settled; an old journal or unexpected owned resource refuses new work.

use std::{
    any::Any,
    cell::RefCell,
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use ato_adapter_oci::{OciOwner, OwnedResourceScanner};
use ato_formation::{
    request::{AttemptFailure, RuntimeProfile},
    verify::VerificationExecutionEvidence,
};
use ato_ipc::runtime_launch::{LaunchContextV1, RuntimeLaunchSpecV1};
use ato_objects::{PortableApplicationBundle, PortableOciArchive};
use ato_portable_application::{
    ValidatedPortableApplication, materialize_tree, validate_bundle_for_derivation,
};
use ato_runtime_attempt::{
    launch::resolved::ResolvedRuntimeLaunchContext,
    native_launch::NativeLaunchDeclaration,
    realize::{
        CandidateRealizer, CandidateStopFailure, RealizeFailure, Realized, RunningCandidate,
    },
};

use super::{NativeLeaseRun, NativeLeaseSurfaceConfig, pixel::PixelHelper};
use crate::{
    runtime_launch::recovery::{RunJournal, RunJournalEntry, RunPhase},
    slot_state::SlotGuard,
};

/// A single native attempt. `work_root` and the Runner/slot identifiers are
/// trusted operator inputs and must be dedicated to this Formation worker.
pub struct NativePortableRealizer<'a> {
    bundle: &'a PortableApplicationBundle,
    route: ValidatedPortableApplication,
    selected: String,
    spec: RuntimeLaunchSpecV1,
    work_root: PathBuf,
    owner: OciOwner,
    helper: PixelHelper,
    archive: RefCell<Option<PortableOciArchive>>,
    config: RefCell<Option<NativeLeaseSurfaceConfig>>,
    interrupt: Option<&'a std::sync::atomic::AtomicBool>,
    journal: RunJournal,
    slot: RefCell<Option<SlotGuard>>,
}

/// A private, attempt-scoped gateway used only for Formation observations.
/// The random assertion key is never returned in a receipt or log.
pub fn private_surface_config(
    session_id: &str,
    surface_id: &str,
) -> Result<NativeLeaseSurfaceConfig> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    drop(listener);
    Ok(NativeLeaseSurfaceConfig {
        scope: netd::surface_authorization::SurfaceGatewayScope {
            session_id: session_id.to_owned(),
            surface_id: surface_id.to_owned(),
        },
        verification_key: hex::encode(rand::random::<[u8; 32]>()),
        origin: "https://formation.invalid".into(),
        public_base_origin: None,
        surface_listen: address,
        startup_timeout: std::time::Duration::from_secs(30),
        idle_timeout: std::time::Duration::from_secs(120),
        maximum_lifetime: std::time::Duration::from_secs(300),
        controls: netd::pixel_gateway::PixelGatewayControls::default(),
    })
}

impl<'a> NativePortableRealizer<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        bundle: &'a PortableApplicationBundle,
        selected: &str,
        context: LaunchContextV1,
        work_root: &Path,
        owner: OciOwner,
        helper: PixelHelper,
        archive: Option<PortableOciArchive>,
        config: NativeLeaseSurfaceConfig,
    ) -> Result<Self> {
        let route = validate_bundle_for_derivation(bundle, selected)?;
        ensure!(
            route
                .derivation
                .native_surface
                .as_ref()
                .is_some_and(|n| n.surface.kind() == "pixel_stream"),
            "Formation native executor requires a Pixel D"
        );
        ensure!(
            context.run_id == owner.run_id,
            "native owner does not match the attempt"
        );
        ensure!(
            ato_adapter_oci::is_label_value(&owner.lease_id)
                && !owner.lease_id.contains(['.', ':']),
            "native attempt id must be a plain identifier"
        );
        let spec = ato_portable_application::portable_run::native_launch_spec(
            bundle,
            selected,
            context,
            route.tree_ref.to_string(),
        )?;
        // Formation must not turn an absent local archive into a Docker
        // registry pull. Verification of its complete graph is performed by
        // the Pixel adapter before the first container is created.
        let supplied = archive
            .as_ref()
            .context("native Formation requires an application archive")?;
        let ato_ipc::runtime_launch::LaunchRealizationV1::Oci(oci) = &spec.realization else {
            unreachable!("native launch projection is OCI")
        };
        ensure!(
            oci.image_reference.as_deref() == Some(supplied.image.as_str())
                && oci.platform.as_deref() == Some(supplied.platform.as_str()),
            "native Formation archive does not match the selected D"
        );
        owner.labels(Some("main"))?;
        let slot = SlotGuard::acquire(work_root, &owner.slot_id)?;
        let journal = RunJournal::new(work_root, &owner.runner_id, &owner.slot_id)?;
        ensure!(
            journal.load()?.is_empty(),
            "native Formation slot needs journal recovery"
        );
        let scan = OwnedResourceScanner::new(&owner.runner_id, &owner.slot_id)?.scan()?;
        ensure!(
            scan.containers.is_empty() && scan.networks.is_empty(),
            "native Formation slot has unrecovered resources"
        );
        helper.check_ready(work_root)?;
        let archive_reserve = u64::try_from(supplied.bytes.len())?
            .checked_mul(2)
            .context("archive memory overflow")?;
        helper.admit_spec_with_reserve(&spec, archive_reserve)?;
        Ok(Self {
            bundle,
            route,
            selected: selected.to_owned(),
            spec,
            work_root: work_root.to_owned(),
            owner,
            helper,
            archive: RefCell::new(archive),
            config: RefCell::new(Some(config)),
            interrupt: None,
            journal,
            slot: RefCell::new(Some(slot)),
        })
    }

    pub fn route(&self) -> &ValidatedPortableApplication {
        &self.route
    }

    pub fn with_interrupt(mut self, interrupt: Option<&'a std::sync::atomic::AtomicBool>) -> Self {
        self.interrupt = interrupt;
        self
    }
}

/// Run the already selected portable D through the same durable attempt and
/// Rust verifier used by the ordinary Formation executor. The physical
/// helper and archive are trusted runtime inputs, not Contract fields.
pub fn run_portable_native_attempt(
    request: &ato_portable_application::portable_run::PortableBundleAttempt<'_>,
    owner: OciOwner,
    helper: PixelHelper,
    archive: PortableOciArchive,
    config: NativeLeaseSurfaceConfig,
    permit: Box<dyn ato_runtime_attempt::journal::AttemptPermit>,
) -> Result<ato_portable_application::portable_run::PortableBundleAttemptOutcome> {
    use ato_runtime_attempt::attempt::{
        AttemptRequest, Continuation, ReceiptContext, run_reserved_attempt,
    };
    ensure!(
        request.binding_environment.is_empty(),
        "native Formation cannot silently ignore Binding values"
    );
    admit_native_policy(request.network)?;
    if request.authorization
        == ato_runtime_attempt::admission::EffectAuthorization::PrivateIsolatedFormation
    {
        helper.verify_private_formation_boundary()?;
    }
    let work_root = request
        .runtime_root
        .parent()
        .context("native attempt has no work root")?;
    let context = LaunchContextV1 {
        run_id: owner.run_id.clone(),
        compute_id: "formation".into(),
        compute_schema_id: "formation".into(),
        compute_instance_id: owner.run_id.clone(),
    };
    let realizer = NativePortableRealizer::new(
        request.bundle,
        request.derivation_ref,
        context,
        work_root,
        owner,
        helper,
        Some(archive),
        config,
    )?
    .with_interrupt(request.interrupt);
    let route = realizer.route();
    let spec = route.attempt_spec(None);
    let transport = ato_portable_application::bundle_sha256(request.bundle_bytes);
    let profile = request
        .bundle
        .portability
        .as_ref()
        .map(|p| match p.profile {
            ato_objects::PortableDependencyProfile::Thin => "thin",
            ato_objects::PortableDependencyProfile::Cached => "cached",
            ato_objects::PortableDependencyProfile::Offline => "offline",
        });
    let result = run_reserved_attempt(
        &AttemptRequest {
            request_id: request.request_id,
            attempt_id: request.attempt_id,
            label: request.label,
            spec: &spec,
            contract_ref: route.contract_ref.as_str(),
            runtime_id: request.runtime_id,
            profile: request.profile,
            authorization: request.authorization,
            network: request.network,
            browser: None,
            attempt_root: request.runtime_root,
            continuation: Continuation::Stop,
            receipt: ReceiptContext {
                target: request.receipt_target,
                bundle_sha256: Some(&transport),
                portability_profile: profile,
                run_id: request.run_id,
            },
            interrupt: request.interrupt,
            control: None,
        },
        &realizer,
        Ok(permit),
    );
    let ato_runtime_attempt::attempt::AttemptOutcome {
        attempt,
        attempt_record,
        verified: _,
        live: _,
        error,
        stop,
    } = result;
    Ok(
        ato_portable_application::portable_run::PortableBundleAttemptOutcome {
            receipt: attempt.receipt.clone(),
            verification: attempt.verification.clone(),
            realization: attempt.realization.clone(),
            outcomes: attempt.outcomes.clone(),
            attempt,
            attempt_record,
            stop,
            error,
        },
    )
}

fn admit_native_policy(network: ato_runtime_attempt::build_sandbox::NetworkPolicy) -> Result<()> {
    ensure!(
        network == ato_runtime_attempt::build_sandbox::NetworkPolicy::Denied,
        "native Formation requires denied networking"
    );
    Ok(())
}

impl CandidateRealizer for NativePortableRealizer<'_> {
    fn native_surface_binding(&self) -> Option<(&str, &str)> {
        Some(("ato.pixel-stream.v1", &self.selected))
    }

    fn admit(&self, _profile: &RuntimeProfile) -> Option<AttemptFailure> {
        let ready = self
            .helper
            .check_ready(&self.work_root)
            .and_then(|()| self.helper.admit_spec(&self.spec));
        ready.err().map(|_| AttemptFailure {
            code: "native_surface_unavailable".into(),
            stage: "admission".into(),
            message: "native Pixel resources are unavailable".into(),
        })
    }

    fn realize(&self, attempt_id: &str, attempt_root: &Path) -> Result<Realized, RealizeFailure> {
        if attempt_id != self.owner.lease_id {
            return Err(anyhow::anyhow!("native owner must name the current attempt").into());
        }
        let root = self
            .work_root
            .canonicalize()
            .context("canonicalize Formation work root")?;
        let attempt = attempt_root
            .canonicalize()
            .context("canonicalize Formation attempt")?;
        if attempt != root.join(attempt_id) {
            return Err(anyhow::anyhow!("native attempt must be under its owned work root").into());
        }
        let declaration = NativeLaunchDeclaration::from_derivation(&self.route.derivation)?;
        let descriptor = declaration.session_surface_descriptor(
            &self
                .config
                .borrow()
                .as_ref()
                .context("native attempt already used")?
                .scope
                .surface_id,
        )?;
        let workspace = attempt.join("native-workspace");
        if workspace.exists() {
            return Err(anyhow::anyhow!("native attempt workspace already exists").into());
        }
        if let Err(error) = materialize_tree(self.bundle, &self.route, &workspace) {
            if workspace.exists() {
                std::fs::remove_dir_all(&workspace)
                    .context("remove incomplete native workspace")?;
            }
            return Err(anyhow::Error::from(error).into());
        }
        let context = match ResolvedRuntimeLaunchContext::new(
            workspace.clone(),
            &self.spec.workspace.cwd_relative,
            self.spec
                .public_env
                .iter()
                .map(|e| (e.name.clone(), e.value.clone()))
                .collect(),
            vec![],
            vec![],
            vec![],
        ) {
            Ok(context) => context,
            Err(error) => {
                std::fs::remove_dir_all(&workspace).context("remove rejected native workspace")?;
                return Err(anyhow::Error::from(error).into());
            }
        };
        let mut entry = RunJournalEntry::new(&self.owner);
        if let Err(error) = self.journal.record(&entry) {
            std::fs::remove_dir_all(&workspace).context("remove unstarted native workspace")?;
            return Err(error.into());
        }
        let config = self
            .config
            .borrow_mut()
            .take()
            .context("native attempt already used")?;
        let mut check_startup = || -> Result<()> {
            if self
                .interrupt
                .is_some_and(|stop| stop.load(std::sync::atomic::Ordering::Acquire))
            {
                return Err(ato_runtime_attempt::startup::StartupCancelled.into());
            }
            Ok(())
        };
        let started = NativeLeaseRun::start_without_network(
            &self.spec,
            &self.selected,
            &descriptor,
            &context,
            &self.owner,
            config,
            Some(self.helper.clone()),
            self.archive.borrow_mut().take(),
            &mut check_startup,
        );
        let run = match started {
            Ok(run) => run,
            Err(failure) if failure.stop.is_confirmed() => {
                for path in [&workspace, &attempt.join("oci-runtime")] {
                    if path.exists() {
                        std::fs::remove_dir_all(path).context("remove failed native scratch")?;
                    }
                }
                self.journal.remove(&self.owner.lease_id)?;
                return Err(RealizeFailure::Execution(failure.error));
            }
            Err(failure) => {
                return Err(RealizeFailure::Abandoned {
                    cleanup: format!("{:?}", failure.stop),
                    error: failure.error,
                    resources: vec![
                        format!("lease:{}", self.owner.lease_id),
                        format!("scratch:{}", attempt.display()),
                    ],
                });
            }
        };
        entry.phase = RunPhase::Active;
        if let Err(error) = self.journal.record(&entry) {
            let stop = run.stop();
            return Err(RealizeFailure::Abandoned {
                error,
                cleanup: format!("journal write failed; stop: {:?}", stop.stop),
                resources: vec![
                    format!("lease:{}", self.owner.lease_id),
                    format!("scratch:{}", attempt.display()),
                ],
            });
        }
        let base = format!("http://{}", run.observation_addr());
        let e = run.execution_evidence();
        let execution = VerificationExecutionEvidence {
            realization: e.realization.into(),
            runtime_executable: e.runtime_executable.clone(),
            runtime_version: e.runtime_version.clone(),
            pid: e.pid,
            container_id: e.container_id.clone(),
            image: e.image.clone(),
            platform: e.platform.clone(),
            endpoint: Some(base.clone()),
            run_id: None,
            // This is a temporary Formation candidate, not a hosted lease.
            // The common attempt supplies its request and attempt IDs below.
            lease_id: None,
            attempt_id: None,
            request_id: None,
            dependency_fetches: vec![],
            portability_profile: None,
            embedded_oci_image_loaded: None,
            services: e
                .services
                .iter()
                .map(|s| ato_formation::verify::VerificationServiceEvidence {
                    name: s.name.clone(),
                    container_id: s.container_id.clone(),
                    image: s.image.clone(),
                })
                .collect(),
            ..Default::default()
        };
        Ok(Realized {
            candidate: Box::new(NativeCandidate {
                run: Some(run),
                endpoints: BTreeMap::from([(declaration.adapter.probe_port, base)]),
                workspace,
                runtime: attempt.join("oci-runtime"),
                journal: RunJournal::new(
                    &self.work_root,
                    &self.owner.runner_id,
                    &self.owner.slot_id,
                )?,
                lease_id: self.owner.lease_id.clone(),
                _slot: self
                    .slot
                    .borrow_mut()
                    .take()
                    .context("native slot already handed off")?,
            }),
            evidence: None,
            execution,
            kept: None,
        })
    }
}

struct NativeCandidate {
    run: Option<NativeLeaseRun>,
    endpoints: BTreeMap<String, String>,
    workspace: PathBuf,
    runtime: PathBuf,
    journal: RunJournal,
    lease_id: String,
    _slot: SlotGuard,
}
impl RunningCandidate for NativeCandidate {
    fn endpoints(&self) -> &BTreeMap<String, String> {
        &self.endpoints
    }
    fn exited(&mut self) -> Result<Option<String>> {
        Ok(self
            .run
            .as_ref()
            .filter(|run| run.is_finished())
            .map(|_| "native workload ended".into()))
    }
    fn stop(mut self: Box<Self>) -> Result<()> {
        let completion = self
            .run
            .take()
            .context("native run already stopped")?
            .stop();
        if !completion.stop.is_confirmed() {
            return Err(CandidateStopFailure::Unconfirmed {
                reason: format!("{:?}", completion.stop),
                resources: vec![format!("lease:{}", self.lease_id)],
            }
            .into());
        }
        for path in [&self.workspace, &self.runtime] {
            if let Err(error) = std::fs::remove_dir_all(path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                return Err(CandidateStopFailure::ScratchKept {
                    reason: format!("remove {}: {error}", path.display()),
                }
                .into());
            }
        }
        self.journal
            .remove(&self.lease_id)
            .map_err(|error| CandidateStopFailure::ScratchKept {
                reason: format!("remove native journal: {error:#}"),
            })?;
        Ok(())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::admit_native_policy;
    use ato_runtime_attempt::build_sandbox::NetworkPolicy;

    #[test]
    fn native_formation_refuses_network_grants() {
        assert!(admit_native_policy(NetworkPolicy::Denied).is_ok());
        assert!(admit_native_policy(NetworkPolicy::DependencyResolution).is_err());
    }
}
