//! The v1 producer lane: build a guest, observe it, mint its identity, publish
//! it, and read it back.
//!
//! ADR-015 step 5-3. Before this, `ato build`'s only relationship with a v1
//! Execution Contract was to READ one out of `capsule.lock` and check the three
//! facets it could measure (the CLI `build` command's
//! `attempt_v1_execution_identity`). Nothing anywhere MINTED one, and the
//! reason was structural rather than missing plumbing: the Ready-State seal
//! path hands the fake backend the `.capsule` archive's bytes, so there was no
//! bootable guest image to observe. `resolved_argv`, `working_directory` and
//! `target` had no observation subject.
//!
//! This lane produces one. It runs the recipe producer over the PROJECTED
//! program source, measures the image that comes out, and only then mints:
//!
//! ```text
//! workspace → pinned source → projection → recipe build → guest image
//!           → runtime artifact → guest target → observe → mint
//!           → capsule.lock (atomic) → trusted-load → recompute → compare
//! ```
//!
//! # What this lane refuses to do
//!
//! It never promotes a v0.3 build result into a v1 envelope, never reads a lock
//! to fill in a facet it could not measure, and never falls back to the v0.3
//! lane when a step fails. Each of those would put a value into an Execution
//! Identity that no measurement produced, and the identity's only claim is that
//! every value in it was measured. A v1 build that cannot complete fails.
//!
//! # The identity names the guest's contents, not the image's bytes
//!
//! `filesystem.view_digest` used to be blake3 over the packed ext4. Running two
//! builds proved that wrong: `mke2fs` stamps every inode it creates with the
//! wall clock and ignores `SOURCE_DATE_EPOCH` (measured, e2fsprogs 1.47.0 —
//! ~9,400 timestamp fields differed between two packs of one tree, while the
//! UUID, hash seed and superblock clocks were already pinned). Two builds of one
//! program source were therefore two different executions.
//!
//! Pinning the rest was possible but would have made the identity hostage to
//! e2fsprogs: an `apt upgrade` on a builder that changed block allocation would
//! change every capsule's id with no source change. So the lane exports the
//! guest filesystem, digests its CONTENTS with
//! [`crate::guest_filesystem_digest`], and packs afterwards. The ext4 is an
//! artifact, not the identity.
//!
//! That digest is taken HERE rather than in [`V1GuestProducer`] on purpose: the
//! producer is the seam a test replaces, and a producer that could supply this
//! digest could supply an identity.
//!
//! # Host-privileged steps
//!
//! Assembling the app image needs `docker`; packing it into a bootable ext4
//! needs `mount`, hence root. Those two are behind [`V1GuestProducer`] so the
//! lane above them is exercised without either. Everything else here — the
//! projection, the resolution, the observation, the mint, the write, the
//! read-back — is the same code in a test as on a builder host. The real
//! producer running end to end on hardware is ADR-015 step 6.

use std::path::{Path, PathBuf};

use crate::docker_import::{
    BuildTool, ResolvedRuntimeArtifact, measure_guest_target, resolve_runtime_artifact,
};
use crate::guest_filesystem_digest::guest_filesystem_digest;
use crate::rootfs_builder::{
    AssembledGuestImage, RootfsBuildSpecV1, SourceProbe, V1_GUEST_WORKING_DIRECTORY,
    assemble_app_image_v1, derive_build_spec_v1, discard_app_image_v1, export_guest_rootfs_v1,
    mkfs_guest_rootfs_v1, v1_filesystem_uuid,
};
use crate::v1_materialization::{V1MaterializationReceipt, measure_guest_artifact, target_triple};
use capsule::capsule_lock::{self, CapsuleLock, LockEnvironmentValue, LockLaunchSection};
use capsule::common::lock_presence::CAPSULE_LOCK_FILE_NAME;
use capsule::execution_contract::{
    ContentDigest, DigestAlgorithm, EnvironmentValuePayloadV1, ExecutionContractEnvelopeV1,
    ExecutionContractV1, ResolvedTargetContract,
};
use capsule::execution_contract_finalize::{FinalizationError, environment_value_digest};
use capsule::program_source_projection::{
    MaterializedProgramSource, VerifiedPinnedSourceMaterialization,
    materialize_program_source_projection,
};
use capsule::routing::input_resolver::resolve_canonical_lock_path;
use capsule::types::manifest_v1::CapsuleManifestV1;

use crate::observe_v1::{V1BuildObservation, observe_v1};

/// Where in the lane a v1 build stopped.
///
/// Deliberately not one `V1BuildFailed`. Each variant names a different thing
/// that is wrong with the world and a different fix: a projection failure is
/// the author's tree, a runtime-resolution failure is the registry, a
/// trusted-load failure is this lane having written something it cannot read
/// back. Collapsing them would make every one of those read as "the build
/// broke".
///
/// No variant carries a secret, a credential, or a token: the only values
/// interpolated below are paths inside the workspace, image references, facet
/// names, and digests.
#[derive(Debug, thiserror::Error)]
pub enum V1BuildError {
    #[error("this workspace cannot be pinned for a v1 build: {reason}")]
    SourceNotPinnable { reason: String },

    #[error("the program source could not be projected into the guest: {reason}")]
    ProgramSourceProjectionFailed { reason: String },

    #[error("no v1 recipe covers this capsule: {reason}")]
    RecipeDerivationFailed { reason: String },

    #[error("the recipe build did not produce a guest image: {reason}")]
    RecipeBuildFailed { reason: String },

    #[error("the runtime artifact {image_ref} could not be resolved: {reason}")]
    RuntimeArtifactResolutionFailed { image_ref: String, reason: String },

    #[error("the guest target could not be measured from {image_ref}: {reason}")]
    GuestTargetMeasurementFailed { image_ref: String, reason: String },

    #[error("a required facet has no measurement: {facet}")]
    ObservationIncomplete { facet: String },

    #[error("a measurement contradicts another: {detail}")]
    ObservationConflict { detail: String },

    #[error("the execution identity could not be minted: {reason}")]
    MintFailed { reason: String },

    #[error("the lock at {path} could not be published: {reason}")]
    LockPersistFailed { path: PathBuf, reason: String },

    #[error("the lock written at {path} does not read back: {reason}")]
    TrustedLoadFailed { path: PathBuf, reason: String },

    #[error(
        "the lock at {path} reads back as a different execution than was minted \
         ({field}: minted {minted}, read back {persisted})"
    )]
    PersistedEnvelopeMismatch {
        path: PathBuf,
        field: &'static str,
        minted: String,
        persisted: String,
    },
}

/// Which producer output a facet came from.
///
/// Not part of the identity — the contract commits values, not their
/// provenance. It exists so a refusal can say WHICH producer failed to supply
/// the facet the mint is missing, instead of naming a contract field the reader
/// then has to trace backwards. [`V1BuildError::ObservationIncomplete`] carries
/// it; the mapping is asserted in this module's tests.
fn facet_provenance(facet: &str) -> &'static str {
    match facet {
        "source.digest" | "source.projection_digest" => {
            "the materialized program-source projection"
        }
        "target" => "measure_guest_target over the assembled guest image",
        "runtime.digest" => "resolve_runtime_artifact over the recipe's base image",
        // NOT the registry resolution: the family is decided by the source
        // probe over the projection, and the dynamic contract is built from the
        // family plus the recipe's observed invocation prefix.
        "runtime.kind" | "runtime.dynamic_contract_digest" => {
            "the recipe derivation over the projected source"
        }
        "launch.argv" | "launch.cwd" => "the recipe's guest launch descriptor",
        facet if facet.starts_with("filesystem.") => "the packed guest image",
        _ => "the v1 manifest, through the Step-4 subset gate",
    }
}

/// A v1 build's inputs.
pub struct V1BuildRequest<'a> {
    /// The workspace `ato build` was pointed at. Read only.
    pub workspace_root: &'a Path,
    /// An ALREADY-FROZEN source archive to project, instead of freezing the
    /// workspace here.
    ///
    /// `ato build` leaves this `None`: it is pointed at a live checkout, and
    /// the freeze is what turns that into something `source.digest` can name.
    /// The snapshot builder's pinned lane sets it, because by the time it gets
    /// here the archive has already been proved to be the one the Source
    /// Revision names (both the byte digest and the reconstructed tree digest),
    /// and re-freezing an extraction of it would mint the identity from bytes
    /// nothing checked rather than from the bytes that were checked.
    ///
    /// It does NOT widen where source can come from: `workspace_root` is still
    /// required and is still where the manifest and the lock are read and
    /// written, so a caller supplying this must supply the extraction of that
    /// same archive.
    pub pinned_source_archive: Option<&'a Path>,
    /// A directory this lane may use for the frozen source archive, the
    /// materialized projection, and the packed image. Must exist.
    pub work_root: &'a Path,
    /// Where the bootable guest image is written.
    pub guest_image_path: &'a Path,
    pub rootfs_size_mib: u64,
    /// The local tag the assembled image is given before it is packed. Must be
    /// unique on the builder host for the duration of the build.
    pub image_ref: &'a str,
}

/// What a completed v1 build produced.
///
/// Everything here is a value the lane MEASURED and then verified survived a
/// round trip through the lock — `trusted_load_verified` is only ever `true`
/// because there is no path that returns this without the read-back having
/// agreed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V1BuildOutcome {
    pub execution_id: String,
    pub lock_path: PathBuf,
    pub guest_image_path: PathBuf,
    pub guest_image_bytes: u64,
    /// Blake3 over the packed ext4, as a MATERIALIZATION receipt.
    ///
    /// Deliberately not in the contract. Two packs of one guest filesystem
    /// differ here — `mke2fs` stamps every inode with the wall clock — so
    /// committing it would make a rebuild a different execution. It is recorded
    /// because an artifact still needs a name: it says WHICH file this build
    /// produced, and lets a later reader check that the file on disk is the one
    /// the build reported. `filesystem_view_digest` is the identity-bearing
    /// value, and the two must never be swapped.
    pub guest_image_digest: String,
    /// The identity-bearing digest of the guest's CONTENTS, as committed by
    /// `filesystem.view_digest`.
    pub filesystem_view_digest: String,
    pub source_digest: String,
    pub runtime_resolved_ref: String,
    pub target: ResolvedTargetContract,
    pub trusted_load_verified: bool,
    /// The `[web] port` the recipe built the guest to serve on.
    ///
    /// Reported rather than re-read from the manifest by a consumer: the
    /// Ready-State tail has to health-probe the port this build actually
    /// wired into the guest's init, and a second read of the manifest is a
    /// second opinion that can drift from the one the image was built with.
    pub port: u16,
    /// The AUTHORED `[run] command`, before the runtime's invocation prefix.
    ///
    /// Provenance only — the launch contract commits `resolved_argv`. Carried
    /// so a consumer can report both without re-parsing the manifest.
    pub authored_argv: Vec<String>,
}

impl V1BuildOutcome {
    /// The first 12 hex characters after the algorithm prefix — enough to
    /// recognize a build in a terminal, never enough to quote as the identity.
    #[must_use]
    pub fn short_execution_id(&self) -> String {
        self.execution_id
            .split_once(':')
            .map(|(algorithm, hex)| format!("{algorithm}:{}", &hex[..hex.len().min(12)]))
            .unwrap_or_else(|| self.execution_id.clone())
    }

    /// The materialization receipt this build publishes.
    ///
    /// One constructor, because a receipt is what a CONSUMER checks the build
    /// against ([`crate::v1_materialization`] and the snapshot builder's v1
    /// intake) — and two hand-assembled copies of it would let one of them
    /// drift into attesting something the build did not report.
    #[must_use]
    pub fn materialization_receipt(&self) -> V1MaterializationReceipt {
        V1MaterializationReceipt {
            execution_id: self.execution_id.clone(),
            lock: self.lock_path.clone(),
            guest_image: self.guest_image_path.clone(),
            guest_image_bytes: self.guest_image_bytes,
            guest_image_digest: self.guest_image_digest.clone(),
            filesystem_view_digest: self.filesystem_view_digest.clone(),
            source_digest: self.source_digest.clone(),
            runtime: self.runtime_resolved_ref.clone(),
            target: target_triple(&self.target),
            trusted_load_verified: self.trusted_load_verified,
        }
    }
}

/// The two operations this lane cannot perform without docker and root.
///
/// The seam is here rather than around the whole build because everything else
/// — projection, resolution, observation, mint, persist, read-back — is
/// host-independent and must run identically in a test and on a builder.
pub trait V1GuestProducer {
    /// Build the app image from the PROJECTED source tree, `FROM` the pinned
    /// base reference. Must leave the image addressable: the guest target is
    /// measured from it before it is packed.
    fn assemble(
        &self,
        projected_source: &Path,
        spec: &RootfsBuildSpecV1,
        pinned_base_ref: &str,
        image_ref: &str,
    ) -> Result<AssembledGuestImage, String>;

    /// Measure the guest platform of an assembled image.
    fn measure_target(&self, image_ref: &str) -> Result<ResolvedTargetContract, String>;

    /// Resolve a base image reference to its immutable artifact identity.
    fn resolve_runtime(&self, image_ref: &str) -> Result<ResolvedRuntimeArtifact, String>;

    /// Export an assembled image's filesystem into `rootfs_dir` and install the
    /// guest init. Consumes the image.
    ///
    /// Separate from the pack because what lands in `rootfs_dir` is what the
    /// identity commits, and the lane — not the producer — is what digests it.
    /// A producer that could supply the digest could supply an identity.
    fn export_rootfs(
        &self,
        image: AssembledGuestImage,
        spec: &RootfsBuildSpecV1,
        rootfs_dir: &Path,
    ) -> Result<(), String>;

    /// Turn an exported rootfs into a bootable guest image at `out`, returning
    /// its size in bytes.
    fn pack_rootfs(
        &self,
        rootfs_dir: &Path,
        out: &Path,
        size_mib: u64,
        filesystem_uuid: &str,
    ) -> Result<u64, String>;

    /// Drop an assembled image that will not be packed.
    fn discard(&self, image: AssembledGuestImage);
}

/// The production producer: the probed container tool for assembly and
/// inspection, `mke2fs` for packing.
///
/// The tool is probed ONCE and used for every step. It has to be: resolving the
/// base image and measuring the guest go through one tool's local image store,
/// and building through another's would look up a digest in a store that does
/// not hold the image the build produced.
pub struct HostV1GuestProducer {
    runner: crate::docker_import::build::SystemImportCommandRunner,
    tool: BuildTool,
}

impl HostV1GuestProducer {
    /// Probe the builder host for its container tool. Fails closed when none is
    /// available rather than deferring the discovery to a half-run build.
    pub fn probe() -> Result<Self, String> {
        let runner = crate::docker_import::build::SystemImportCommandRunner;
        let probe = crate::docker_import::build::probe_build_tool(&runner)?;
        Ok(Self {
            runner,
            tool: probe.tool,
        })
    }
}

impl V1GuestProducer for HostV1GuestProducer {
    fn assemble(
        &self,
        projected_source: &Path,
        spec: &RootfsBuildSpecV1,
        pinned_base_ref: &str,
        image_ref: &str,
    ) -> Result<AssembledGuestImage, String> {
        assemble_app_image_v1(
            projected_source,
            spec,
            pinned_base_ref,
            image_ref,
            self.tool.as_str(),
        )
    }

    fn measure_target(&self, image_ref: &str) -> Result<ResolvedTargetContract, String> {
        measure_guest_target(&self.runner, self.tool, image_ref)
    }

    fn resolve_runtime(&self, image_ref: &str) -> Result<ResolvedRuntimeArtifact, String> {
        resolve_runtime_artifact(&self.runner, self.tool, image_ref)
    }

    fn export_rootfs(
        &self,
        image: AssembledGuestImage,
        spec: &RootfsBuildSpecV1,
        rootfs_dir: &Path,
    ) -> Result<(), String> {
        export_guest_rootfs_v1(image, spec, rootfs_dir, self.tool.as_str())
    }

    fn pack_rootfs(
        &self,
        rootfs_dir: &Path,
        out: &Path,
        size_mib: u64,
        filesystem_uuid: &str,
    ) -> Result<u64, String> {
        mkfs_guest_rootfs_v1(rootfs_dir, out, size_mib, filesystem_uuid)
    }

    fn discard(&self, image: AssembledGuestImage) {
        discard_app_image_v1(image, self.tool.as_str());
    }
}

/// Run the v1 producer lane end to end.
///
/// The step order is load-bearing and is the ADR-015 §5-3 order verbatim: the
/// mint cannot move ahead of the guest image, because every facet it commits is
/// a measurement of that image or of the projection that went into it, and the
/// lock cannot be published ahead of the mint, because what it publishes IS the
/// mint. Reordering either would let a lock name a build that does not exist.
pub fn run(
    request: V1BuildRequest<'_>,
    producer: &dyn V1GuestProducer,
) -> Result<V1BuildOutcome, V1BuildError> {
    // Neither the scratch nor the output may live inside the workspace. The
    // archive that freezes the workspace walks everything under it, so a guest
    // image or a materialized projection left in the tree becomes part of the
    // NEXT build's `source.digest` — and since both are functions of the
    // source, there is no fixed point: every build would mint a new identity.
    //
    // Refused rather than quietly excluded. `.ato/` is not on ADR-014 §1's
    // control-file list ("the manifest and the ONE resolved lock, nothing
    // else"), and widening that list is a normative change to what a Capsule
    // Program's source IS — the same reason a root-level `.git` is refused
    // instead of being skipped.
    refuse_path_inside_workspace(request.workspace_root, request.work_root, "the work root")?;
    refuse_path_inside_workspace(
        request.workspace_root,
        request.guest_image_path,
        "the guest image",
    )?;

    // The lock's NAME is resolved before anything else runs: it decides which
    // control file the projection withholds (below), and its fail-closed rules
    // — a workspace carrying both spellings has no single authoritative lock —
    // must stop the build before it spends a registry round trip on it.
    let lock_path = resolve_lock_path(request.workspace_root)?;

    // 1–3. Freeze the workspace, then project it. The projected tree — not the
    // checkout — is what the guest gets and what `source.digest` names.
    let projection_root = request.work_root.join("projected-source");
    std::fs::create_dir_all(&projection_root).map_err(|source| {
        V1BuildError::ProgramSourceProjectionFailed {
            reason: format!("create the projection directory: {source}"),
        }
    })?;
    let projected = project_pinned_source(
        request.workspace_root,
        request.pinned_source_archive,
        request.work_root,
        &projection_root,
    )?;

    // The manifest is read from the WORKSPACE, not from the projection: the
    // projection is precisely the tree with the manifest removed.
    let manifest = read_v1_manifest(request.workspace_root)?;

    // 4. Derive the recipe from the projected tree. Probing the projection
    // rather than the checkout matters: the probe decides the runtime family
    // from the files present, and those are the files the guest will have.
    let probe = SourceProbe::scan(&projection_root);
    let spec = derive_build_spec_v1(&manifest, &probe)
        .map_err(|reason| V1BuildError::RecipeDerivationFailed { reason })?;

    // 5. Resolve the base image to an immutable digest BEFORE building, so the
    // image is built `FROM` the same bytes the contract records. Resolving
    // afterwards would leave a window in which the tag moves and the contract
    // names a runtime the guest never ran.
    let runtime = producer
        .resolve_runtime(&spec.base_image)
        .map_err(|reason| V1BuildError::RuntimeArtifactResolutionFailed {
            image_ref: spec.base_image.clone(),
            reason,
        })?;
    verify_resolution_matches_recipe(&spec, &runtime)?;

    let source_digest =
        ContentDigest::new(DigestAlgorithm::Sha256, projected.contract.digest.bytes());

    // 6. Assemble the guest image from the projection.
    let image = producer
        .assemble(
            &projection_root,
            &spec,
            &runtime.resolved_ref,
            request.image_ref,
        )
        .map_err(|reason| V1BuildError::RecipeBuildFailed { reason })?;

    // 7. Measure the guest from the image that was just built — not from the
    // base image it derived from, and never from this host. Any failure from
    // here until the image is packed has to drop it.
    let target = match producer.measure_target(image.image_ref()) {
        Ok(target) => target,
        Err(reason) => {
            let image_ref = image.image_ref().to_string();
            producer.discard(image);
            return Err(V1BuildError::GuestTargetMeasurementFailed { image_ref, reason });
        }
    };
    if let Err(error) = verify_target_agrees_with_runtime(&target, &spec) {
        producer.discard(image);
        return Err(error);
    }

    // 8. Export the guest filesystem, digest THAT, then pack it.
    //
    // The digest is over the exported tree and not over the packed image, and
    // the reason is measured rather than aesthetic: `mke2fs` stamps every inode
    // it creates with the wall clock and ignores `SOURCE_DATE_EPOCH`, so two
    // packs of one tree differ in thousands of timestamp fields. Committing
    // those bytes would have made a rebuild a different execution, and pinning
    // them would have made the identity hostage to the builder's e2fsprogs
    // version. The contents are the thing anyone means by "the same guest".
    //
    // It is measured HERE rather than in the producer on purpose: the producer
    // is the seam a test replaces, and a producer that could supply this digest
    // could supply an identity.
    let rootfs_dir = request.work_root.join("guest-rootfs");
    std::fs::create_dir_all(&rootfs_dir).map_err(|source| V1BuildError::RecipeBuildFailed {
        reason: format!("create the guest rootfs directory: {source}"),
    })?;
    producer
        .export_rootfs(image, &spec, &rootfs_dir)
        .map_err(|reason| V1BuildError::RecipeBuildFailed { reason })?;
    let filesystem_view_digest = guest_filesystem_digest(&rootfs_dir)
        .map_err(|reason| V1BuildError::RecipeBuildFailed { reason })?;

    // The filesystem UUID is derived from inputs already fixed — the projected
    // source, the pinned base, the exact argv — rather than drawn by `mke2fs`.
    // Not identity-bearing any more, but a filesystem UUID that is stable for
    // one program and distinct between programs is what a UUID is for.
    let filesystem_uuid = v1_filesystem_uuid(
        &source_digest.to_string(),
        &runtime.resolved_ref,
        &spec.resolved_argv,
    );
    let guest_image_bytes = producer
        .pack_rootfs(
            &rootfs_dir,
            request.guest_image_path,
            request.rootfs_size_mib,
            &filesystem_uuid,
        )
        .map_err(|reason| V1BuildError::RecipeBuildFailed { reason })?;

    // The packed artifact's own digest, for the receipt and NOT for the
    // contract. It names which file this build wrote; two packs of one guest
    // filesystem differ here, which is exactly why the identity commits the
    // contents above instead.
    let guest_image_digest =
        measure_guest_artifact(request.guest_image_path).map_err(|source| {
            V1BuildError::RecipeBuildFailed {
                reason: format!(
                    "hash the packed guest image at {}: {source}",
                    request.guest_image_path.display()
                ),
            }
        })?;

    // 9. Observe every facet, then mint. `V1BuildObservation`'s fields are all
    // required, so an observation that reaches `observe_v1` is complete by
    // construction — there is no "9 of 10" state to count.
    let observation = observe_v1(V1BuildObservation {
        manifest: &manifest,
        source_digest,
        excluded_control_files: withheld_control_files(&projected, &lock_path),
        runtime_kind: runtime_kind_name(&spec).to_string(),
        runtime: &runtime,
        runtime_invocation_prefix: spec.runtime_invocation_prefix.clone(),
        filesystem_view_digest,
        target: target.clone(),
        resolved_argv: spec.resolved_argv.clone(),
        working_directory: V1_GUEST_WORKING_DIRECTORY.to_string(),
    })
    .map_err(|error| V1BuildError::ObservationConflict {
        detail: format!("{error:#}"),
    })?;

    let minted = observation
        .into_minted_envelope()
        .map_err(mint_error_from_finalization)?;

    // 10. Publish. Atomic: a reader sees the whole old lock or the whole new
    // one, and a crash leaves the previous one intact.
    // Captured before the write so a failed read-back can put back exactly what
    // was there. `persist_execution_contract` MERGES into the existing lock, so
    // deleting the merged file on failure would take the caller's other
    // sections with it — worse than the stale lock the removal exists to avoid.
    let previous_lock_bytes = std::fs::read(&lock_path).ok();
    persist_execution_contract(&lock_path, &manifest, &minted)?;

    // 11. Read it back from disk through the trusted path — not from the value
    // still in memory, which would prove nothing about what was written.
    //
    // EVERY failure from here on goes through `unpublish`. This lane wrote the
    // file, so a lock it cannot vouch for is its own output and must not be
    // left where the next reader will trust it — and that includes the two
    // failures that are about READING rather than comparing. A lock that will
    // not load is no more publishable than one describing another build.
    let verified = capsule_lock::load_verified_from_path(&lock_path)
        .map_err(|source| V1BuildError::TrustedLoadFailed {
            path: lock_path.clone(),
            reason: source.to_string(),
        })
        .and_then(|persisted| {
            let read_back = persisted.execution_contract.as_ref().ok_or_else(|| {
                V1BuildError::TrustedLoadFailed {
                    path: lock_path.clone(),
                    reason: "the lock read back without an execution contract".to_string(),
                }
            })?;
            compare_persisted_to_minted(&lock_path, &minted, read_back)
        });
    if let Err(error) = verified {
        return Err(unpublish(&lock_path, previous_lock_bytes.as_deref(), error));
    }

    Ok(V1BuildOutcome {
        execution_id: minted.execution_id.as_str().to_string(),
        lock_path,
        guest_image_path: request.guest_image_path.to_path_buf(),
        guest_image_bytes,
        guest_image_digest: guest_image_digest.to_string(),
        filesystem_view_digest: minted.execution_contract.filesystem.view_digest.to_string(),
        source_digest: minted.execution_contract.source.digest.to_string(),
        runtime_resolved_ref: runtime.resolved_ref,
        target,
        trusted_load_verified: true,
        port: spec.port,
        authored_argv: manifest.run.command.clone(),
    })
}

/// Freeze the workspace into a content-addressed archive, mint the pinned proof
/// by extracting it, and materialize the projection into `destination`.
///
/// The archive round trip is not ceremony. A live checkout can change while it
/// is being read, and ADR-014 §1 admits only a pinned materialization for
/// exactly that reason: `source.digest` has to name a tree that cannot move
/// under the build. It is also what refuses a Git working tree — a root-level
/// `.git` is neither a control file the projection may withhold nor content
/// whose bytes are reproducible, so a checkout is not a source a v1 identity
/// can be minted from.
fn project_pinned_source(
    workspace_root: &Path,
    pinned_source_archive: Option<&Path>,
    work_root: &Path,
    destination: &Path,
) -> Result<MaterializedProgramSource, V1BuildError> {
    // One projection, two ways of getting the archive it reads — and only the
    // archive step differs. A caller that already holds a PROVED archive hands
    // it in; a caller pointed at a live checkout freezes it here. Neither can
    // reach the projection with a tree that was never frozen.
    let archive = match pinned_source_archive {
        Some(archive) => archive.to_path_buf(),
        None => {
            let archive = work_root.join("source.tar.zst");
            capsule::blob::materialize_source_archive(workspace_root, &archive).map_err(
                |source| V1BuildError::SourceNotPinnable {
                    reason: source.to_string(),
                },
            )?;
            archive
        }
    };
    let pinned =
        VerifiedPinnedSourceMaterialization::from_source_archive(&archive).map_err(|source| {
            V1BuildError::SourceNotPinnable {
                reason: source.to_string(),
            }
        })?;
    materialize_program_source_projection(&pinned, destination).map_err(|source| {
        V1BuildError::ProgramSourceProjectionFailed {
            reason: source.to_string(),
        }
    })
}

fn read_v1_manifest(workspace_root: &Path) -> Result<CapsuleManifestV1, V1BuildError> {
    let path = workspace_root.join("capsule.toml");
    let text =
        std::fs::read_to_string(&path).map_err(|source| V1BuildError::SourceNotPinnable {
            reason: format!("read {}: {source}", path.display()),
        })?;
    CapsuleManifestV1::from_toml(&text).map_err(|source| V1BuildError::RecipeDerivationFailed {
        reason: source.to_string(),
    })
}

/// The runtime family name the contract records. Resolved from the recipe, so
/// it can never disagree with the base image the recipe chose.
fn runtime_kind_name(spec: &RootfsBuildSpecV1) -> &'static str {
    use crate::rootfs_builder::RuntimeKind;
    match spec.runtime {
        RuntimeKind::Python => "python",
        RuntimeKind::Node => "node",
        RuntimeKind::StaticWeb => "static-web",
    }
}

/// The artifact that was resolved must be the one the recipe asked for.
///
/// `resolve_runtime_artifact` echoes back the reference it was given; if that
/// ever stopped matching the recipe's base image, the contract would record a
/// runtime the build did not use, and nothing downstream would notice.
fn verify_resolution_matches_recipe(
    spec: &RootfsBuildSpecV1,
    runtime: &ResolvedRuntimeArtifact,
) -> Result<(), V1BuildError> {
    if runtime.original_ref != spec.base_image {
        return Err(V1BuildError::RuntimeArtifactResolutionFailed {
            image_ref: spec.base_image.clone(),
            reason: format!(
                "the resolution answered for {} instead",
                runtime.original_ref
            ),
        });
    }
    if !runtime.resolved_ref.contains("@sha256:") {
        return Err(V1BuildError::RuntimeArtifactResolutionFailed {
            image_ref: spec.base_image.clone(),
            reason: format!(
                "{} is not pinned to a digest, so it cannot be an identity input",
                runtime.resolved_ref
            ),
        });
    }
    Ok(())
}

/// The measured target and the recipe must describe the same machine.
///
/// `measure_guest_target` already refuses a non-Linux image and an
/// unclassifiable libc. What it cannot know is that this lane's recipes only
/// produce Linux guests — so an image that measured as something else means the
/// recipe and the artifact have come apart, and minting would commit a target
/// the recipe cannot have built.
fn verify_target_agrees_with_runtime(
    target: &ResolvedTargetContract,
    spec: &RootfsBuildSpecV1,
) -> Result<(), V1BuildError> {
    if target.os != "linux" {
        return Err(V1BuildError::ObservationConflict {
            detail: format!(
                "the guest measured os {:?}, but the {} recipe builds a linux guest",
                target.os,
                runtime_kind_name(spec)
            ),
        });
    }
    match (target.abi.as_str(), target.libc.as_deref()) {
        ("gnu", Some("glibc")) | ("musl", Some("musl")) => Ok(()),
        (abi, libc) => Err(V1BuildError::ObservationConflict {
            detail: format!(
                "the guest measured abi {abi:?} with libc {libc:?}; the two disagree, and \
                 an abi that does not follow from the measured libc is not resolved"
            ),
        }),
    }
}

/// Map the mint's refusal onto the lane's vocabulary.
///
/// `UnmeasuredFacet` is the one that must not be flattened: it names a facet
/// whose producer did not run, which is a different problem from a contract
/// that will not serialize.
fn mint_error_from_finalization(error: FinalizationError) -> V1BuildError {
    match error {
        FinalizationError::UnmeasuredFacet(facet) => V1BuildError::ObservationIncomplete {
            facet: format!("{facet} (produced by {})", facet_provenance(facet)),
        },
        other => V1BuildError::MintFailed {
            reason: other.to_string(),
        },
    }
}

/// Write the minted envelope into the workspace's canonical lock.
///
/// The lock's NAME is decided by the shared resolver, never by this lane: a
/// workspace already carrying the deprecated `ato.lock.json` alias keeps it,
/// and one carrying neither gets `capsule.lock`. Adding a rule here would give
/// a workspace two locks.
///
/// An existing lock is loaded through the VERIFIED path. A lock that does not
/// verify is not a base to build on — overwriting it would silently repair a
/// tampered file, and preserving its other sections would carry whatever was
/// wrong with it into the new one.
fn persist_execution_contract(
    lock_path: &Path,
    manifest: &CapsuleManifestV1,
    minted: &ExecutionContractEnvelopeV1,
) -> Result<(), V1BuildError> {
    let mut lock = if lock_path.exists() {
        capsule_lock::load_verified_from_path(lock_path).map_err(|source| {
            V1BuildError::LockPersistFailed {
                path: lock_path.to_path_buf(),
                reason: format!(
                    "the existing lock does not verify, so it is not a base to build on: {source}"
                ),
            }
        })?
    } else {
        CapsuleLock::default()
    };

    lock.execution_contract = Some(minted.clone());
    lock.launch = d5_launch_section(manifest, minted)?;

    capsule_lock::write_pretty_to_path(&lock, lock_path).map_err(|source| {
        V1BuildError::LockPersistFailed {
            path: lock_path.to_path_buf(),
            reason: source.to_string(),
        }
    })
}

/// Refuse a build path that lives inside the source tree.
///
/// Compared after canonicalizing both sides, so a symlink or a `..` cannot
/// smuggle a path back in. `path` may not exist yet (the output file is created
/// by the pack), so its nearest existing ancestor is what gets canonicalized —
/// a directory that will hold a file inside the workspace is itself inside it.
fn refuse_path_inside_workspace(
    workspace_root: &Path,
    path: &Path,
    what: &str,
) -> Result<(), V1BuildError> {
    let refuse = |reason: String| V1BuildError::SourceNotPinnable { reason };

    // A relative path is refused rather than resolved. The comparison below is
    // between canonical paths, and a relative one only becomes comparable by
    // resolving it against the process CWD — which for `ato build` is wherever
    // the user happened to be standing, so a path that is outside the workspace
    // from one directory is inside it from another. Refusing keeps the answer a
    // property of the arguments.
    if path.is_relative() {
        return Err(refuse(format!(
            "{what} ({}) is a relative path; it must be absolute so that whether it lies \
             inside the workspace does not depend on the current directory",
            path.display()
        )));
    }

    let workspace = workspace_root.canonicalize().map_err(|source| {
        refuse(format!(
            "resolve the workspace {}: {source}",
            workspace_root.display()
        ))
    })?;

    // Walk up to the nearest ancestor that exists — the output file does not
    // yet, and neither does a directory a caller is about to create.
    let mut existing = path;
    let resolved = loop {
        match existing.canonicalize() {
            Ok(resolved) => break resolved,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                match existing.parent() {
                    Some(parent) if parent != existing => existing = parent,
                    // Nothing on this path exists, so it cannot be under the
                    // workspace, which does.
                    _ => return Ok(()),
                }
            }
            Err(source) => {
                return Err(refuse(format!(
                    "resolve {what} {}: {source}",
                    path.display()
                )));
            }
        }
    };

    if resolved.starts_with(&workspace) {
        return Err(refuse(format!(
            "{what} ({}) is inside the workspace, so the next build would freeze it as \
             program source and hash it into source.digest. A build output is not part of \
             the program it was built from — put it somewhere outside the source tree.",
            path.display()
        )));
    }
    Ok(())
}

/// Which file this build will publish its lock to.
///
/// Decided by the shared resolver, never by this lane: a workspace already
/// carrying the deprecated `ato.lock.json` alias keeps it, one carrying neither
/// gets `capsule.lock`, and one carrying both is refused here rather than given
/// a third rule that would leave it with two locks.
fn resolve_lock_path(workspace_root: &Path) -> Result<PathBuf, V1BuildError> {
    resolve_canonical_lock_path(workspace_root)
        .map_err(|source| V1BuildError::LockPersistFailed {
            path: workspace_root.join(CAPSULE_LOCK_FILE_NAME),
            reason: source.to_string(),
        })
        .map(|existing| existing.unwrap_or_else(|| workspace_root.join(CAPSULE_LOCK_FILE_NAME)))
}

/// The control files a v1 build withholds from the program source, as the
/// identity records them.
///
/// The projection reports what it actually removed: the manifest, plus whatever
/// lock the workspace already carried. For a PRODUCER that is not stable —
/// this lane WRITES the lock, so a first build (nothing to withhold but the
/// manifest) and every build after it (manifest and lock) would report
/// different sets. The set is identity-bearing, so the same program source
/// would mint one identity the first time and a different one forever after,
/// and `ato build` twice in a row would disagree with itself.
///
/// The lock is a control file of this workspace whether or not it exists yet:
/// this build is about to write it, at the name already resolved. Declaring it
/// consistently is what makes the identity a function of the program source
/// rather than of build history. Which NAME is withheld still varies, and still
/// should — a repository that spells its lock `ato.lock.json` held a different
/// file out than one that spells it `capsule.lock`, and that is the difference
/// the payload exists to record.
fn withheld_control_files(projected: &MaterializedProgramSource, lock_path: &Path) -> Vec<String> {
    let mut withheld = projected.excluded_control_files.clone();
    let lock_name = lock_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| CAPSULE_LOCK_FILE_NAME.to_string());
    if !withheld.contains(&lock_name) {
        withheld.push(lock_name);
    }
    // `SourceProjectionPayloadV1::a1v2` sorts and dedups, but the value is
    // compared in tests and read in errors, so it leaves here canonical.
    withheld.sort();
    withheld.dedup();
    withheld
}

/// The D5 `launch.environment` section the lock's read path requires.
///
/// When the envelope commits a non-empty `launch.environment`, the lock must
/// carry every committed non-secret name with a matching value digest —
/// `verify_environment_values` enforces set equality in both directions, so
/// writing the envelope alone would produce a lock that cannot be read back.
/// The values come from the same manifest the observation read, and the digests
/// are re-derived rather than copied, so a disagreement fails here rather than
/// at the reader.
fn d5_launch_section(
    manifest: &CapsuleManifestV1,
    minted: &ExecutionContractEnvelopeV1,
) -> Result<Option<LockLaunchSection>, V1BuildError> {
    if minted.execution_contract.launch.environment.is_empty() {
        return Ok(None);
    }
    let mut environment = Vec::with_capacity(manifest.env.len());
    for (name, value) in &manifest.env {
        let payload = EnvironmentValuePayloadV1::utf8(value.clone());
        let digest =
            environment_value_digest(&payload).map_err(|source| V1BuildError::MintFailed {
                reason: format!("digest the value of [env] {name}: {source}"),
            })?;
        environment.push(LockEnvironmentValue {
            name: name.clone(),
            value: payload,
            value_digest: digest.to_string(),
        });
    }
    // `manifest.env` is a BTreeMap, so this is already sorted; the lock's read
    // path requires strictly increasing names and would reject anything else.
    environment.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Some(LockLaunchSection { environment }))
}

/// Every identity-bearing field of the persisted envelope must equal the minted
/// one.
///
/// `load_verified_from_path` already recomputes the execution id and refuses a
/// mismatch, so a lock that reads back at all is internally consistent. What it
/// cannot tell is whether it is consistent about THIS build: a stale lock left
/// by an earlier run would verify perfectly and describe a different execution.
/// Comparing field by field rather than only by id is what makes the refusal
/// name what differs.
fn compare_persisted_to_minted(
    lock_path: &Path,
    minted: &ExecutionContractEnvelopeV1,
    persisted: &ExecutionContractEnvelopeV1,
) -> Result<(), V1BuildError> {
    let mismatch = |field: &'static str, minted: String, persisted: String| {
        V1BuildError::PersistedEnvelopeMismatch {
            path: lock_path.to_path_buf(),
            field,
            minted,
            persisted,
        }
    };

    let expected = &minted.execution_contract;
    let actual = &persisted.execution_contract;

    if expected.schema != actual.schema {
        return Err(mismatch(
            "schema",
            expected.schema.clone(),
            actual.schema.clone(),
        ));
    }
    if expected.source.digest != actual.source.digest {
        return Err(mismatch(
            "source.digest",
            expected.source.digest.to_string(),
            actual.source.digest.to_string(),
        ));
    }
    if expected.runtime != actual.runtime {
        return Err(mismatch(
            "runtime",
            expected.runtime.digest.to_string(),
            actual.runtime.digest.to_string(),
        ));
    }
    if expected.target != actual.target {
        return Err(mismatch(
            "target",
            describe_target(&expected.target),
            describe_target(&actual.target),
        ));
    }
    if expected.launch.argv != actual.launch.argv {
        return Err(mismatch(
            "launch.argv",
            format!("{:?}", expected.launch.argv),
            format!("{:?}", actual.launch.argv),
        ));
    }
    if expected.launch.cwd != actual.launch.cwd {
        return Err(mismatch(
            "launch.cwd",
            expected.launch.cwd.as_str().to_string(),
            actual.launch.cwd.as_str().to_string(),
        ));
    }
    if expected.filesystem != actual.filesystem {
        return Err(mismatch(
            "filesystem",
            expected.filesystem.view_digest.to_string(),
            actual.filesystem.view_digest.to_string(),
        ));
    }

    // The whole contract, canonicalized. Catches every field the explicit
    // comparisons above do not name — including any added later, which is why
    // this is not merely a slower repeat of them.
    let canonical = |contract: &ExecutionContractV1| {
        contract
            .canonical_bytes()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_else(|error| format!("<uncanonicalizable: {error}>"))
    };
    if canonical(expected) != canonical(actual) {
        return Err(mismatch(
            "canonical payload",
            canonical(expected),
            canonical(actual),
        ));
    }

    // Recomputed from the persisted contract, not read from its stored field:
    // the id has to be a function of what came back off the disk.
    let recomputed =
        actual
            .compute_execution_id()
            .map_err(|source| V1BuildError::TrustedLoadFailed {
                path: lock_path.to_path_buf(),
                reason: format!("recompute the execution id of the persisted contract: {source}"),
            })?;
    if recomputed != minted.execution_id {
        return Err(mismatch(
            "execution_id",
            minted.execution_id.as_str().to_string(),
            recomputed.as_str().to_string(),
        ));
    }
    Ok(())
}

fn describe_target(target: &ResolvedTargetContract) -> String {
    format!(
        "{}/{}/{} libc={:?}",
        target.os, target.architecture, target.abi, target.libc
    )
}

/// Take back a lock this lane wrote but cannot vouch for, leaving the workspace
/// as it found it.
///
/// Leaving the file would leave a lock that VERIFIES — its id matches its own
/// contract — while describing an execution this build did not produce, and the
/// next reader has no way to tell. But plain removal is wrong too:
/// `persist_execution_contract` merges into the existing lock, so deleting the
/// merged file would take the caller's `resolution`, `binding` and `policy`
/// sections with it, which this lane neither owns nor can reconstruct.
///
/// So: restore the exact bytes that were there before the write, or remove the
/// file if there were none. Either way the returned error is the original
/// cause — the failure to report is why the lock is untrustworthy, not the
/// bookkeeping that followed.
fn unpublish(lock_path: &Path, previous: Option<&[u8]>, cause: V1BuildError) -> V1BuildError {
    let restored = match previous {
        Some(bytes) => std::fs::write(lock_path, bytes),
        None => match std::fs::remove_file(lock_path) {
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        },
    };
    match restored {
        Ok(()) => cause,
        Err(source) => V1BuildError::LockPersistFailed {
            path: lock_path.to_path_buf(),
            reason: format!(
                "the lock did not verify ({cause}), and {} failed too: {source}. The file \
                 on disk is this build's output and must not be trusted.",
                if previous.is_some() {
                    "restoring the previous one"
                } else {
                    "removing it"
                }
            ),
        },
    }
}

#[cfg(test)]
mod tests;
