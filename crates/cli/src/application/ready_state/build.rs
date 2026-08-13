//! Ready-State build: Boot/Snapshot/Seal (E3/E4), driven against the selected
//! snapshot backend (Fake on a KVM-less host).
//!
//! The caller assembles raw [`BuildLayers`] from the frozen build outputs; this
//! module derives the restore/sanitizer contracts + declared secret markers
//! from the manifest, runs the GPU fail-closed guard, and calls
//! `build_ready_state` (whose no-secret gate fails the build closed). On success
//! it persists the sealed [`ReadyStateManifest`] next to its CAS store.
//!
//! When [`V1SealRequest`] is supplied, `seal` additionally mints a Capsule v1
//! Snapshot for the SAME immutable layers: it migrates the sealed legacy
//! manifest into a [`SnapshotManifestV1`], runs it through the REAL
//! disposable-restore acceptance loop (`snapshot::acceptance`, #1088/#1102 —
//! never a caller-supplied bool, never self-attested), and — only on
//! acceptance — commits an authenticated [`ArtifactEnvelopeV1`] to the v1
//! on-disk store (`ready_state::store::V1StagingArtifact`). A rejection or
//! ineligible workload leaves the legacy artifact sealed but publishes no v1
//! sidecar; the build call itself fails so the caller can act on it.

use std::path::Path;

use anyhow::{Context, Result};
use capsule::execution_contract::{ExecutionContractEnvelopeV1, ExecutionId};
use capsule::types::CapsuleManifest;
use snapshot::acceptance::{
    AcceptanceCancellation, AcceptanceConfig, AcceptanceDisposition, RunningSnapshotAcceptance,
    SystemClock, VerifiedRunningSnapshotEligibility,
};
use snapshot::layer_store::CasStore;
use snapshot::{
    ArtifactEnvelopeV1, BuildLayers, BuildReadyStateInput, BuildReadyStateReceipt, RestoreContract,
    SanitizerContract, SanitizerLayer, SanitizerStep, SnapshotBackend, WarmupRecipe,
    ensure_gpu_not_in_snapshot,
};

use super::store;

/// Derive the restore contract (ports / healthcheck / SLO / warmup) from the manifest.
pub(crate) fn restore_contract_from_manifest(m: &CapsuleManifest) -> RestoreContract {
    let mut ports: Vec<u16> = Vec::new();
    if let Some(targets) = m.targets.as_ref() {
        if let Some(p) = targets.port {
            ports.push(p);
        }
        for nt in targets.named_targets().values() {
            if let Some(p) = nt.port {
                ports.push(p);
            }
        }
    }
    ports.sort_unstable();
    ports.dedup();

    // Healthcheck: the first concrete http_get probe path on any target.
    let healthcheck = m.targets.as_ref().and_then(|t| {
        t.named_targets()
            .values()
            .find_map(|nt| nt.readiness_probe.as_ref().and_then(|p| p.http_get.clone()))
    });

    let snapshot_cfg = m.snapshot_config();
    let expected_ready_ms = snapshot_cfg
        .max_restore_seconds
        .map(|s| s.saturating_mul(1000));
    // The author's first-screen warmup recipe rides the sealed artifact. Paths
    // are enforced by the backend's warmup gate (one enforcement point, shared
    // with the builder lanes), so this stays a pure copy.
    let warmup = WarmupRecipe::from_snapshot_config(&snapshot_cfg);

    RestoreContract {
        expected_ready_ms,
        ports,
        healthcheck,
        warmup_paths: warmup.warmup_paths,
        stable_successes: warmup.stable_successes,
        stable_interval_ms: warmup.stable_interval_ms,
        content_ready_path: warmup.content_ready_path,
        ..Default::default()
    }
}

/// Derive the post-resume sanitizer steps. When `sanitize_after_restore` is on
/// (the default), emit the standard ordered step set (plan §8.2); else empty.
pub(crate) fn sanitizer_contract_from_manifest(m: &CapsuleManifest) -> SanitizerContract {
    if !m.snapshot_config().sanitize_after_restore {
        return SanitizerContract::default();
    }
    let steps = vec![
        SanitizerStep {
            step: "regenerate_ids".into(),
            layer: SanitizerLayer::GuestAgent,
        },
        SanitizerStep {
            step: "reseed_entropy".into(),
            layer: SanitizerLayer::GuestAgent,
        },
        SanitizerStep {
            step: "refresh_clock".into(),
            layer: SanitizerLayer::GuestAgent,
        },
        SanitizerStep {
            step: "reset_sockets".into(),
            layer: SanitizerLayer::GuestAgent,
        },
        SanitizerStep {
            step: "reconnect_net".into(),
            layer: SanitizerLayer::HostAndGuest,
        },
        SanitizerStep {
            step: "port_remap".into(),
            layer: SanitizerLayer::Host,
        },
    ];
    SanitizerContract { steps }
}

/// Declared secret markers to scan the sealed layers for: the `[secrets.*]`
/// names and their target env-var names (the build holds no values — these are
/// names a leaked value would likely be labeled with).
pub(crate) fn declared_secret_markers(m: &CapsuleManifest) -> Vec<String> {
    let mut markers = Vec::new();
    for (name, spec) in m.secrets.iter() {
        markers.push(name.clone());
        if let Some(env) = spec.env.as_ref() {
            markers.push(env.clone());
        }
    }
    markers.sort();
    markers.dedup();
    markers
}

/// An explicit request to additionally mint a Capsule v1 Snapshot for the same
/// immutable layers `seal` is about to build. `None` at the call site keeps
/// `ato build` byte-for-byte legacy-only.
///
/// `ato build` constructs one through [`v1_seal_request`] — i.e. only when the
/// manifest declares `[seal_at]` AND a v1 Execution Identity was confirmed.
pub(crate) struct V1SealRequest<'a> {
    /// The verified Execution Contract this Snapshot is subordinate to.
    /// Required (not a bare [`ExecutionId`]) because the running-capture
    /// eligibility proof and the identity binding MUST come from the SAME
    /// verified contract (see
    /// [`VerifiedRunningSnapshotEligibility::analyze_execution_contract`]).
    pub execution_contract_envelope: &'a ExecutionContractEnvelopeV1,
    /// `seal_at.command` as exact argv (RFC §6.1): the disposable-restore
    /// acceptance loop runs this against the restored candidate and accepts
    /// on and only on an observed exit 0. Executed as a real host-side
    /// subprocess — see [`BackendDisposableLifecycle::execute_exact_argv`]'s
    /// doc comment for why (no in-guest exec transport exists yet).
    pub seal_at_argv: Vec<String>,
    /// Bounds for the acceptance run. `None` uses [`default_acceptance_config`]
    /// (a single real attempt — see its doc for why more than one is deferred).
    pub acceptance_config: Option<AcceptanceConfig>,
}

/// Boot/Snapshot/Seal: GPU fail-closed guard → build_ready_state (no-secret gate
/// inside) → persist the sealed manifest. When `v1` is `Some`, additionally
/// mints and disposable-restore-accepts a Capsule v1 Snapshot for the same
/// layers before publishing (see the module doc). Returns the build receipt
/// (unchanged: legacy-manifest-shaped either way).
pub(crate) fn seal(
    state_root: &Path,
    capsule_manifest_hash: String,
    manifest: &CapsuleManifest,
    layers: BuildLayers,
    backend: &dyn SnapshotBackend,
    v1: Option<V1SealRequest<'_>>,
) -> Result<BuildReadyStateReceipt> {
    // C guard: never seal an in-VM GPU into the snapshot.
    ensure_gpu_not_in_snapshot(manifest.gpu_mode())
        .context("Ready-State build refused: GPU state is not snapshottable")?;

    // Phase 8 hard invariant: a Ready-State seal is ALWAYS produced from a pre-bind
    // boot — never from a bound running session (post-bind state is dirty). `ato build`
    // boots fresh with no bindings attached, so this is `false` here; the guard makes
    // the invariant explicit at the one place a seal is produced and fails closed if a
    // future path ever tries to seal a bound session.
    super::binding_host::ensure_pre_bind_before_seal(/* session_is_bound = */ false)?;

    // A v1 request's execution_id is verified UP FRONT (before any store is
    // opened) — an unverifiable envelope must never even stage bytes.
    let verified_execution_id = v1
        .as_ref()
        .map(|request| {
            request
                .execution_contract_envelope
                .verified_execution_id()
                .context("verify Capsule v1 execution contract envelope")
        })
        .transpose()?;

    let mut v1_staging = verified_execution_id
        .as_ref()
        .map(|verified| store::V1StagingArtifact::create(state_root, verified.as_execution_id()))
        .transpose()?;
    let store = match &v1_staging {
        Some(staging) => staging.open_store()?,
        None => store::open_store(state_root, &capsule_manifest_hash)?,
    };
    // Delegate runner-class resolution to the backend (same contract as the
    // snapshot-builder daemon and `runner serve`): `None` lets Firecracker pin
    // the seal to its real facts (snapshot format, VMM version, guest kernel
    // hash) instead of the KVM-free `from_host()` probe whose backend facets
    // are sentinels. The Fake backend seals unpinned, matching builder-driven
    // fake seals.
    let runner_class = None;
    let surface_requirement = manifest.resolve_default_target()?.surface.clone();

    let receipt = backend
        .build_ready_state(BuildReadyStateInput {
            store: &store,
            capsule_manifest_hash: capsule_manifest_hash.clone(),
            runner_class,
            surface_requirement,
            layers,
            restore_contract: restore_contract_from_manifest(manifest),
            sanitizer_contract: sanitizer_contract_from_manifest(manifest),
            declared_secret_markers: declared_secret_markers(manifest),
            execution_id: verified_execution_id
                .as_ref()
                .map(|verified| verified.as_execution_id().to_string()),
            supervisor: None,
        })
        .context("snapshot backend build_ready_state failed")?;

    match (v1, verified_execution_id) {
        (Some(request), Some(verified)) => {
            let staging = v1_staging
                .take()
                .expect("v1_staging was created whenever verified_execution_id is Some");
            seal_v1_via_disposable_acceptance(
                state_root,
                backend,
                &store,
                &receipt,
                verified.as_execution_id().clone(),
                request,
                staging,
            )?;
        }
        _ => {
            store::save_manifest(state_root, &receipt.manifest)?;
        }
    }
    Ok(receipt)
}

/// The acceptance bounds, shared with the interactive hold that runs the SAME
/// authored `seal_at.command` — see
/// [`snapshot::acceptance::default_acceptance_config`] for why there is one
/// attempt, and why this lives in `snapshot` rather than in either caller.
pub(crate) use snapshot::acceptance::default_acceptance_config;

/// Build the Capsule v1 Snapshot request for a CONFIRMED Execution Identity out
/// of the manifest's `[seal_at]` table (RFC §6/§6.3).
///
/// `None` when the manifest declares no `[seal_at]`: with no authored
/// acceptance command there is nothing that could accept a candidate (the
/// acceptance loop only ever accepts on an OBSERVED exit 0 of a real argv), so
/// the caller stays on its legacy-only path byte-for-byte.
///
/// An authored `timeout_seconds` is applied by the shared
/// [`snapshot::acceptance::acceptance_config_for_seal_at`] — the same derivation
/// the interactive hold uses, so one capsule gets one budget whichever path
/// seals it. When `timeout_seconds` is absent the request carries no config at
/// all, so `seal` uses the default verbatim.
pub(crate) fn v1_seal_request<'a>(
    manifest: &CapsuleManifest,
    execution_contract_envelope: &'a ExecutionContractEnvelopeV1,
) -> Option<V1SealRequest<'a>> {
    let seal_at = manifest.seal_at.as_ref()?;
    let acceptance_config = seal_at
        .timeout_seconds
        .map(|_| snapshot::acceptance::acceptance_config_for_seal_at(seal_at));
    Some(V1SealRequest {
        execution_contract_envelope,
        // Exact argv, cloned element-for-element: argument boundaries (including
        // an argument that contains a space, and an empty argument) are the
        // authored command's meaning (RFC §6.1), never re-split or re-joined.
        seal_at_argv: seal_at.command.clone(),
        acceptance_config,
    })
}

#[allow(clippy::too_many_arguments)]
fn seal_v1_via_disposable_acceptance(
    state_root: &Path,
    backend: &dyn SnapshotBackend,
    store: &CasStore,
    receipt: &BuildReadyStateReceipt,
    execution_id: ExecutionId,
    request: V1SealRequest<'_>,
    staging: store::V1StagingArtifact,
) -> Result<()> {
    // Holding this proof IS the fail-closed running-capture eligibility gate
    // (RFC §8.3) — minted from the SAME verified contract the caller's
    // execution_id came from, never a caller-supplied bool.
    let eligibility = VerifiedRunningSnapshotEligibility::analyze_execution_contract(
        request.execution_contract_envelope,
    )
    .map_err(|failure| {
        anyhow::anyhow!("Capsule v1 running-capture Snapshot is ineligible: {failure}")
    })?;

    let candidate =
        snapshot::disposable_lifecycle::build_v1_candidate_manifest(backend, execution_id, receipt)
            .map_err(|e| anyhow::anyhow!(e))?;
    let config = request
        .acceptance_config
        .unwrap_or_else(|| default_acceptance_config(request.seal_at_argv));

    let overlay_root = staging.artifact_dir().join("acceptance-overlay");
    let mut lifecycle = snapshot::disposable_lifecycle::BackendDisposableLifecycle {
        backend,
        store,
        candidate: snapshot::disposable_lifecycle::FixedCandidate {
            legacy: receipt.manifest.clone(),
            candidate,
        },
        overlay_root,
        session: None,
        last_candidate: None,
    };
    let run = RunningSnapshotAcceptance::accept(
        &mut lifecycle,
        eligibility,
        &config,
        &AcceptanceCancellation::default(),
        &SystemClock,
    )
    .map_err(|fault| anyhow::anyhow!("Capsule v1 disposable-restore acceptance: {fault}"))?;

    let AcceptanceDisposition::Accepted(record) = &run.disposition else {
        anyhow::bail!(
            "Capsule v1 candidate was not accepted by the disposable-restore verifier \
             (seal_at.command did not exit 0): {:?}",
            run.receipt.outcome
        );
    };
    let accepted = lifecycle
        .last_candidate
        .take()
        .context("acceptance run accepted without a captured candidate")?;
    let accepted_id = accepted
        .snapshot_id()
        .context("derive accepted snapshot_id")?;
    if accepted_id != record.snapshot_id {
        anyhow::bail!("accepted candidate snapshot_id does not match the acceptance receipt");
    }
    let envelope = ArtifactEnvelopeV1::accepted(&receipt.manifest, &accepted)
        .context("create authenticated Snapshot Artifact Envelope")?;
    staging.commit(state_root, &receipt.manifest, &accepted, &envelope)?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use std::time::Duration;

    // Named here rather than in the parent: the production code above builds no
    // contract of its own, so importing these at module scope would be an unused
    // import in every non-test build. (`ContentDigest`/`DigestAlgorithm` were
    // missing outright — this module did not compile under `cargo test`.)
    use capsule::execution_contract::{ContentDigest, DigestAlgorithm};

    use super::*;

    fn parse(extra: &str) -> CapsuleManifest {
        let base = r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
run = "python app.py"
port = 8080

[targets.app.readiness_probe]
type = "http"
path = "/health"
"#;
        CapsuleManifest::from_toml(&format!("{base}\n{extra}")).expect("parse")
    }

    #[test]
    fn restore_contract_maps_ports() {
        let c = restore_contract_from_manifest(&parse(
            "[snapshot]\nmode=\"warm\"\nmax_restore_seconds=8\n",
        ));
        assert!(c.ports.contains(&8080));
        assert_eq!(c.expected_ready_ms, Some(8000));
        // Defaults: no warmup, no content_ready_path ⇒ v1 healthcheck-only seal.
        assert!(c.warmup_paths.is_empty());
        assert_eq!(c.stable_successes, None);
        assert_eq!(c.stable_interval_ms, None);
        assert_eq!(c.content_ready_path, None);
    }

    #[test]
    fn restore_contract_copies_warmup_fields() {
        let c = restore_contract_from_manifest(&parse(
            "\
[snapshot]\n\
mode=\"warm\"\n\
warmup_paths=[\"/\",\"/api/health\"]\n\
stable_successes=3\n\
stable_interval_ms=200\n\
content_ready_path=\"/\"\n",
        ));
        assert_eq!(c.warmup_paths, vec!["/", "/api/health"]);
        assert_eq!(c.stable_successes, Some(3));
        assert_eq!(c.stable_interval_ms, Some(200));
        assert_eq!(c.content_ready_path.as_deref(), Some("/"));
    }

    #[test]
    fn sanitizer_contract_present_by_default_and_empty_when_disabled() {
        assert!(
            !sanitizer_contract_from_manifest(&parse("[snapshot]\nmode=\"warm\"\n"))
                .steps
                .is_empty()
        );
        let off = parse("[snapshot]\nmode=\"warm\"\nsanitize_after_restore=false\n");
        assert!(sanitizer_contract_from_manifest(&off).steps.is_empty());
    }

    #[test]
    fn declared_secret_markers_collects_names_and_env() {
        let m = parse("[secrets.openai_api_key]\nenv=\"OPENAI_API_KEY\"\n");
        let markers = declared_secret_markers(&m);
        assert!(markers.contains(&"openai_api_key".to_string()));
        assert!(markers.contains(&"OPENAI_API_KEY".to_string()));
    }

    #[test]
    fn seal_persists_manifest_and_runs_gates() {
        let dir = tempfile::tempdir().unwrap();
        let backend = snapshot::FakeSnapshotBackend::new();
        let m = parse("[snapshot]\nmode=\"warm\"\n");
        let layers = BuildLayers {
            rootfs: b"rootfs".to_vec(),
            runtime: None,
            dependency: None,
            app: Some(b"the app".to_vec()),
            vmstate: vec![0xAB; 256],
            memory: (0..100_000u32).map(|i| (i % 256) as u8).collect(),
        };
        let receipt = seal(
            dir.path(),
            "blake3:capsule".to_string(),
            &m,
            layers,
            &backend,
            None,
        )
        .unwrap();
        assert!(receipt.no_secret_proof.is_clean());
        // The sealed manifest is loadable from disk.
        let loaded = store::load_manifest(dir.path(), "blake3:capsule")
            .unwrap()
            .unwrap();
        assert_eq!(loaded.id(), receipt.manifest.id());
    }

    #[test]
    fn seal_refuses_in_vm_gpu() {
        let dir = tempfile::tempdir().unwrap();
        let backend = snapshot::FakeSnapshotBackend::new();
        let m = parse("[snapshot]\nmode=\"warm\"\n[requirements]\nvram_min=\"8GB\"\n");
        let layers = BuildLayers {
            rootfs: b"r".to_vec(),
            runtime: None,
            dependency: None,
            app: Some(b"a".to_vec()),
            vmstate: vec![0u8; 16],
            memory: vec![0u8; 16],
        };
        let err = seal(
            dir.path(),
            "blake3:gpu".to_string(),
            &m,
            layers,
            &backend,
            None,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("GPU"));
    }

    /// Wraps the Fake backend and records the `runner_class` the CLI hands to
    /// `build_ready_state`, so delegation is asserted explicitly rather than
    /// inferred from the sealed output.
    struct RecordingBackend {
        inner: snapshot::FakeSnapshotBackend,
        seen_runner_class:
            std::sync::Mutex<Option<Option<capsule::foundation::install_lifecycle::RunnerClassId>>>,
    }

    impl SnapshotBackend for RecordingBackend {
        fn id(&self) -> &str {
            self.inner.id()
        }
        fn probe(&self) -> snapshot::BackendCapabilities {
            self.inner.probe()
        }
        fn snapshot_compatibility_contract(
            &self,
        ) -> Result<
            capsule::snapshot_manifest::SnapshotCompatibilityContractV1,
            snapshot::SnapshotError,
        > {
            self.inner.snapshot_compatibility_contract()
        }
        fn build_ready_state(
            &self,
            input: BuildReadyStateInput<'_>,
        ) -> Result<BuildReadyStateReceipt, snapshot::SnapshotError> {
            *self.seen_runner_class.lock().unwrap() = Some(input.runner_class.clone());
            self.inner.build_ready_state(input)
        }
        fn inspect(
            &self,
            store: &snapshot::layer_store::CasStore,
            manifest: &snapshot::ReadyStateManifest,
        ) -> Result<snapshot::SnapshotInspection, snapshot::SnapshotError> {
            self.inner.inspect(store, manifest)
        }
        fn restore(
            &self,
            input: snapshot::RestoreReadyStateInput<'_>,
        ) -> Result<snapshot::RestoreReceipt, snapshot::SnapshotError> {
            self.inner.restore(input)
        }
        fn stop(
            &self,
            session: snapshot::RestoredSession,
        ) -> Result<snapshot::TeardownReceipt, snapshot::SnapshotError> {
            self.inner.stop(session)
        }
    }

    #[test]
    fn seal_delegates_runner_class_resolution_to_backend() {
        let dir = tempfile::tempdir().unwrap();
        let backend = RecordingBackend {
            inner: snapshot::FakeSnapshotBackend::new(),
            seen_runner_class: std::sync::Mutex::new(None),
        };
        let m = parse("[snapshot]\nmode=\"warm\"\n");
        let layers = BuildLayers {
            rootfs: b"r".to_vec(),
            runtime: None,
            dependency: None,
            app: Some(b"a".to_vec()),
            vmstate: vec![0u8; 64],
            memory: vec![0u8; 4096],
        };
        let receipt = seal(
            dir.path(),
            "blake3:rc".to_string(),
            &m,
            layers,
            &backend,
            None,
        )
        .unwrap();
        assert_eq!(
            *backend.seen_runner_class.lock().unwrap(),
            Some(None),
            "CLI seal must pass runner_class=None so the backend resolves its own class"
        );
        assert!(
            receipt.manifest.runner_class_id.is_none(),
            "Fake echoes the input verbatim: an unpinned seal proves the CLI delegated"
        );
    }

    /// A minimal, self-consistent `ExecutionContractV1` + envelope for tests
    /// that only need a VERIFIED execution identity, not a realistic contract.
    /// Shared crate-wide so no second copy of this fixture is needed.
    pub(crate) fn test_execution_envelope(seed: u8) -> ExecutionContractEnvelopeV1 {
        use capsule::execution_contract::{
            EXECUTION_CONTRACT_V1_SCHEMA, EnvironmentVariableContract, ExecutionContractV1,
            GuestPath, GuestSurfaceContract, OpaqueContractDomainV1, ResolvedArtifactContract,
            ResolvedBuildOutputContract, ResolvedDependencyContract, ResolvedFilesystemContract,
            ResolvedLaunchContract, ResolvedPolicyContract, ResolvedSourceContract,
            ResolvedTargetContract, opaque_subcontract_digest,
        };
        let placeholder = opaque_subcontract_digest(
            OpaqueContractDomainV1::SourceProjection,
            &serde_json::json!({}),
        )
        .unwrap();
        let digest = |fill: u8| ContentDigest::new(DigestAlgorithm::Blake3, [fill; 32]);
        let contract = ExecutionContractV1 {
            schema: EXECUTION_CONTRACT_V1_SCHEMA.to_string(),
            source: ResolvedSourceContract {
                digest: digest(seed),
                projection_digest: placeholder,
            },
            target: ResolvedTargetContract {
                os: "linux".to_string(),
                architecture: "x86_64".to_string(),
                abi: "gnu".to_string(),
                libc: None,
                observable_features: Default::default(),
            },
            runtime: ResolvedArtifactContract {
                kind: "python".to_string(),
                digest: digest(seed.wrapping_add(1)),
                dynamic_contract_digest: placeholder,
            },
            dependencies: Vec::<ResolvedDependencyContract>::new(),
            build_outputs: Vec::<ResolvedBuildOutputContract>::new(),
            launch: ResolvedLaunchContract {
                argv: vec!["python".to_string(), "app.py".to_string()],
                cwd: GuestPath::parse("/workspace").unwrap(),
                process_model_digest: placeholder,
                environment: Vec::<EnvironmentVariableContract>::new(),
                environment_policy_digest: placeholder,
                secret_bindings: Vec::new(),
            },
            filesystem: ResolvedFilesystemContract {
                view_digest: digest(seed.wrapping_add(2)),
                topology_digest: placeholder,
                // Non-empty by ADR-015 §6.3: an execution with no immutable
                // layer has no filesystem to identify.
                readonly_layers: vec![digest(seed.wrapping_add(3))],
                writable_paths: Vec::new(),
            },
            policy: ResolvedPolicyContract {
                network_digest: placeholder,
                capability_digest: placeholder,
                filesystem_digest: placeholder,
            },
            guest_surface: GuestSurfaceContract {
                bind_address: "0.0.0.0".to_string(),
                protocol: "ato-guest/v1".to_string(),
                port: std::num::NonZeroU16::new(8080),
                features: Vec::new(),
            },
            external_state: Vec::new(),
        };
        let execution_id = contract.compute_execution_id().expect("valid execution id");
        ExecutionContractEnvelopeV1 {
            execution_contract: contract,
            execution_id,
            // No ADR-014 parent-association claim: this fixture is a bare
            // execution envelope, matching `FinalizedExecutionIdentityV1::
            // into_envelope`'s own default.
            capsule_program_id: None,
            resolved_refs: Default::default(),
            generated_at: None,
            provenance: serde_json::Value::Null,
            diagnostics: serde_json::Value::Null,
            evidence: serde_json::Value::Null,
        }
    }

    #[test]
    #[cfg(unix)]
    fn v1_seal_accepts_via_real_disposable_restore_and_publishes_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let backend = snapshot::FakeSnapshotBackend::new();
        let m = parse("[snapshot]\nmode=\"warm\"\n");
        let layers = BuildLayers {
            rootfs: b"rootfs".to_vec(),
            runtime: None,
            dependency: None,
            app: Some(b"app".to_vec()),
            vmstate: vec![0u8; 64],
            memory: vec![1u8; 4096],
        };
        let envelope = test_execution_envelope(1);
        let execution_id = envelope.execution_id.clone();

        let receipt = seal(
            dir.path(),
            "blake3:v1".to_string(),
            &m,
            layers,
            &backend,
            Some(V1SealRequest {
                execution_contract_envelope: &envelope,
                // A real, always-succeeding host command — proves the
                // acceptance loop genuinely spawns and classifies a process.
                seal_at_argv: vec!["true".to_string()],
                acceptance_config: None,
            }),
        )
        .unwrap();

        assert_eq!(
            receipt.manifest.execution_id.as_deref(),
            Some(execution_id.as_str())
        );
        let snapshots = store::load_v1_snapshots(dir.path(), &execution_id).unwrap();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].snapshot_manifest.execution_id, execution_id);
        assert!(
            store::load_manifest(dir.path(), "blake3:v1")
                .unwrap()
                .is_none(),
            "v1 artifacts must not overwrite the legacy capsule-manifest keyed store"
        );
        // The acceptance overlay is cleaned up (disposable — never left mounted).
        assert!(
            !snapshots[0]
                .artifact_dir
                .join("acceptance-overlay")
                .join("v1-acceptance")
                .exists()
        );
    }

    #[test]
    #[cfg(unix)]
    fn v1_seal_rejects_when_seal_at_command_exits_nonzero() {
        let dir = tempfile::tempdir().unwrap();
        let backend = snapshot::FakeSnapshotBackend::new();
        let m = parse("[snapshot]\nmode=\"warm\"\n");
        let layers = BuildLayers {
            rootfs: b"rootfs".to_vec(),
            runtime: None,
            dependency: None,
            app: Some(b"app".to_vec()),
            vmstate: vec![0u8; 64],
            memory: vec![1u8; 4096],
        };
        let envelope = test_execution_envelope(2);

        let err = seal(
            dir.path(),
            "blake3:v1-reject".to_string(),
            &m,
            layers,
            &backend,
            Some(V1SealRequest {
                execution_contract_envelope: &envelope,
                seal_at_argv: vec!["false".to_string()],
                acceptance_config: None,
            }),
        )
        .unwrap_err();
        assert!(err.to_string().contains("was not accepted"), "{err}");
        // The legacy artifact itself still sealed (the receipt is returned by
        // `build_ready_state` regardless), but no v1 sidecar exists.
        assert!(
            store::load_v1_snapshots(dir.path(), &envelope.execution_id)
                .unwrap()
                .is_empty()
        );
    }

    // ── [seal_at] → V1SealRequest (the `ato build` wiring) ────────────────

    #[test]
    fn seal_at_declares_the_v1_request_with_the_exact_authored_argv() {
        let envelope = test_execution_envelope(3);
        // An argument containing a space and an empty argument: both are part of
        // the authored command's meaning (RFC §6.1) and must survive verbatim,
        // never re-split, re-joined, or dropped.
        let m = parse(
            "[snapshot]\nmode=\"warm\"\n\n[seal_at]\ncommand = [\"sh\", \"-lc\", \
             \"curl -fsS http://127.0.0.1:8080/ready\", \"--label\", \"\"]\n",
        );
        let request = v1_seal_request(&m, &envelope).expect("[seal_at] yields a v1 request");
        assert_eq!(
            request.seal_at_argv,
            [
                "sh",
                "-lc",
                "curl -fsS http://127.0.0.1:8080/ready",
                "--label",
                "",
            ]
        );
        assert_eq!(
            request.execution_contract_envelope.execution_id,
            envelope.execution_id
        );
        // No authored timeout ⇒ no override at all, so `seal` uses
        // `default_acceptance_config` verbatim (single attempt included).
        assert!(request.acceptance_config.is_none());
    }

    #[test]
    fn absent_seal_at_yields_no_v1_request() {
        let envelope = test_execution_envelope(4);
        let m = parse("[snapshot]\nmode=\"warm\"\n");
        assert!(
            v1_seal_request(&m, &envelope).is_none(),
            "a manifest without [seal_at] must behave exactly as before"
        );
    }

    #[test]
    fn seal_at_timeout_maps_onto_the_verification_budget_only() {
        let envelope = test_execution_envelope(5);
        let m = parse(
            "[snapshot]\nmode=\"warm\"\n\n[seal_at]\ncommand = [\"verify\"]\ntimeout_seconds = 120\n",
        );
        let config = v1_seal_request(&m, &envelope)
            .expect("request")
            .acceptance_config
            .expect("an authored timeout overrides the default bounds");
        assert_eq!(config.seal_at_argv, ["verify"]);
        assert_eq!(config.verification_timeout, Duration::from_secs(120));
        // The run deadline keeps the default's non-verification headroom
        // (60s total - 30s verification = 30s) so the authored 120s can
        // actually be spent instead of being truncated at 60s.
        assert_eq!(config.total_deadline, Duration::from_secs(150));
        // The single-attempt policy belongs to the caller's single-boot capture
        // pipeline; the manifest must not be able to widen it.
        assert_eq!(
            config.maximum_attempts,
            default_acceptance_config(Vec::new()).maximum_attempts
        );
    }

    #[test]
    #[cfg(unix)]
    fn seal_at_from_the_manifest_drives_a_real_acceptance_run() {
        // End-to-end through `seal`: the argv the MANIFEST declared is what the
        // acceptance loop actually spawns, so minting a v1 Snapshot is reachable
        // from authoring alone.
        let dir = tempfile::tempdir().unwrap();
        let backend = snapshot::FakeSnapshotBackend::new();
        let m = parse(
            "[snapshot]\nmode=\"warm\"\n\n[seal_at]\ncommand = [\"true\"]\ntimeout_seconds = 5\n",
        );
        let layers = BuildLayers {
            rootfs: b"rootfs".to_vec(),
            runtime: None,
            dependency: None,
            app: Some(b"app".to_vec()),
            vmstate: vec![0u8; 64],
            memory: vec![1u8; 4096],
        };
        let envelope = test_execution_envelope(6);
        let request = v1_seal_request(&m, &envelope).expect("request");
        seal(
            dir.path(),
            "blake3:seal-at".to_string(),
            &m,
            layers,
            &backend,
            Some(request),
        )
        .unwrap();
        let snapshots = store::load_v1_snapshots(dir.path(), &envelope.execution_id).unwrap();
        assert_eq!(snapshots.len(), 1);
    }
}
