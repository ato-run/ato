//! Phase 9 — persistent state volume SAFETY layer (DESIGN + SCAFFOLD).
//!
//! This module sits ABOVE the v1.6 (ato#983) durable-state lifecycle in
//! [`crate::state_volume`] (which already creates/locks the host backing file,
//! attaches it as a writable non-root Firecracker drive via
//! `state_volume::state_drive_configs` / `firecracker::configure_state_drives`,
//! and releases the lock on `stop()`). Rather than duplicate any of that, Phase
//! 9 adds the *persistence-safety envelope* that the raw lifecycle does not yet
//! have:
//!
//!   1. **Artifact/state separation types** — an explicit, typed boundary
//!      between the immutable rootfs+memory *artifact* and the mutable external
//!      *state volume*.
//!   2. **Schema/revision compatibility gate** — refuse to attach a state
//!      volume whose on-disk schema is incompatible with the artifact that
//!      wants to consume it.
//!   3. **Dirty-detach quarantine** — a volume that was NOT cleanly detached
//!      (crash, `kill()`ed VMM) is quarantined, not reattached blindly.
//!   4. **Quota** — per-volume and per-owner total size ceilings.
//!   5. **Backup/export hook** — a point-in-time export of the backing file
//!      that never touches the artifact.
//!
//! # THE INVIOLABLE INVARIANT
//!
//! The persistent state volume is a **mutable external block device**, kept
//! **strictly separate** from the **immutable** rootfs+memory snapshot
//! artifact. Nothing in this module — or anything built on it — may ever write
//! durable state back into the Ready-State artifact. That boundary is the whole
//! reason these two things are different types here:
//! [`ArtifactStateContract`] carries only the artifact's *identity* and the
//! schema it *expects*; it never carries a writable handle to the artifact.
//!
//! # Scaffolding status
//!
//! The *decision* functions ([`compat_gate`], [`plan_attach`],
//! [`attach_decision`], [`check_quota`]) and the fs helpers for the dirty
//! marker / quarantine / backup are implemented and unit-tested here. Wiring
//! them into `firecracker::build_ready_state` / `restore` (so the gate runs
//! before `state_volume::prepare_volumes`, the quarantine actually diverts a
//! dirty file, and quota is enforced against real usage) plus the guest-side
//! mount are deliberately OUT OF SCOPE for this PR and marked
//! scaffolded-not-wired in the PR body.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// 1. Artifact / state separation
// ---------------------------------------------------------------------------

/// A durable state volume's on-disk schema identity.
///
/// `schema_name` names a migration *lineage* (e.g. `"sqlite-app-db"`) — two
/// volumes with different lineages are never interchangeable. `version` is the
/// app's schema generation (a bump = a breaking on-disk layout change).
/// `revision` is a monotonic marker WITHIN a version (a forward-compatible
/// additive change: a reader at revision N understands any volume written at
/// revision ≤ N).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateSchemaVersion {
    pub schema_name: String,
    pub version: u32,
    pub revision: u32,
}

impl StateSchemaVersion {
    pub fn new(schema_name: impl Into<String>, version: u32, revision: u32) -> Self {
        Self { schema_name: schema_name.into(), version, revision }
    }
}

/// The immutable artifact's *contract* with any state volume attached to it.
///
/// Carries ONLY the artifact's identity ([`artifact_digest`], the digest of the
/// sealed rootfs+memory snapshot) and the schema it expects of a state volume
/// ([`expects`]). It deliberately holds NO writable handle to the artifact —
/// the type system is the first line of defense for the "never write state into
/// the artifact" invariant. This is what the runner presents to [`compat_gate`]
/// before attaching a volume.
///
/// [`artifact_digest`]: ArtifactStateContract::artifact_digest
/// [`expects`]: ArtifactStateContract::expects
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactStateContract {
    /// Digest of the immutable Ready-State artifact (rootfs + memory snapshot).
    /// Never mutated by state I/O — recorded here purely for provenance so a
    /// backup/export can be attributed to the artifact it came from.
    pub artifact_digest: String,
    /// The schema the artifact's app was built against and can consume.
    pub expects: StateSchemaVersion,
}

// ---------------------------------------------------------------------------
// 2. Schema / revision compatibility gate
// ---------------------------------------------------------------------------

/// Why a state volume may not be attached to a given artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompatError {
    /// Different migration lineage entirely — never interchangeable.
    SchemaMismatch { expected: String, found: String },
    /// Same lineage, different (breaking) schema generation. Migration across
    /// versions is out of scope — fail closed rather than corrupt.
    VersionMismatch { expected: u32, found: u32 },
    /// The volume was written by a NEWER revision than this artifact
    /// understands (forward-incompatible). Attaching it read-write risks the
    /// older app silently dropping fields it does not know about.
    RevisionTooNew { artifact_understands: u32, volume_written_at: u32 },
}

impl std::fmt::Display for CompatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompatError::SchemaMismatch { expected, found } => write!(
                f,
                "state schema lineage mismatch: artifact expects {expected:?}, volume is {found:?} \
                 — these are not the same durable store"
            ),
            CompatError::VersionMismatch { expected, found } => write!(
                f,
                "state schema version mismatch: artifact expects v{expected}, volume is v{found} \
                 — cross-version migration is not supported; fail closed"
            ),
            CompatError::RevisionTooNew { artifact_understands, volume_written_at } => write!(
                f,
                "state volume revision {volume_written_at} is newer than the artifact understands \
                 ({artifact_understands}) — refusing to attach forward-incompatible state read-write"
            ),
        }
    }
}

/// Decide whether a volume with on-disk schema `volume` may be attached to an
/// artifact whose contract is `contract`.
///
/// Accept iff: same lineage AND same version AND the volume's revision is not
/// newer than the artifact understands (`volume.revision <=
/// contract.expects.revision`). An OLDER-revision volume is accepted (the
/// reader is forward-compatible within a version); a NEWER-revision volume is
/// rejected ([`CompatError::RevisionTooNew`]).
pub fn compat_gate(
    contract: &ArtifactStateContract,
    volume: &StateSchemaVersion,
) -> Result<(), CompatError> {
    let want = &contract.expects;
    if want.schema_name != volume.schema_name {
        return Err(CompatError::SchemaMismatch {
            expected: want.schema_name.clone(),
            found: volume.schema_name.clone(),
        });
    }
    if want.version != volume.version {
        return Err(CompatError::VersionMismatch { expected: want.version, found: volume.version });
    }
    if volume.revision > want.revision {
        return Err(CompatError::RevisionTooNew {
            artifact_understands: want.revision,
            volume_written_at: volume.revision,
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 3. Dirty-detach quarantine
// ---------------------------------------------------------------------------

/// Whether the previous session detached the volume cleanly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetachState {
    /// The previous session removed its in-use marker on a graceful stop.
    Clean,
    /// An in-use marker is still present — the previous session crashed or its
    /// VMM was `kill()`ed without a graceful `sync`+detach. The backing file
    /// may hold torn writes and must NOT be reattached blindly.
    Dirty,
}

/// Sidecar marker file that records "this volume is live". Created at attach,
/// removed at clean detach. Its mere PRESENCE at the next attach means the last
/// session did not detach cleanly. Lives next to the backing image (`<img>` →
/// `<img>.inuse`), never inside the artifact.
pub fn dirty_marker_path(image_path: &Path) -> PathBuf {
    let mut name = image_path.file_name().unwrap_or_default().to_os_string();
    name.push(".inuse");
    image_path.with_file_name(name)
}

/// Write the in-use marker (idempotent). Call immediately BEFORE handing the
/// backing file to Firecracker as a writable drive, so any crash before the
/// matching [`mark_clean_detach`] leaves the marker behind → `Dirty`.
pub fn mark_in_use(image_path: &Path) -> Result<(), String> {
    let marker = dirty_marker_path(image_path);
    if let Some(parent) = marker.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    std::fs::write(&marker, b"in-use\n").map_err(|e| format!("write {}: {e}", marker.display()))
}

/// Remove the in-use marker on a GRACEFUL detach (after the guest has `sync`ed
/// and unmounted and the VMM has been stopped cleanly). Idempotent — a missing
/// marker is not an error.
pub fn mark_clean_detach(image_path: &Path) {
    let _ = std::fs::remove_file(dirty_marker_path(image_path));
}

/// Read the current detach state from the marker's presence.
pub fn detach_state(image_path: &Path) -> DetachState {
    if dirty_marker_path(image_path).exists() { DetachState::Dirty } else { DetachState::Clean }
}

/// Where a dirty volume is moved so it is preserved for inspection/recovery but
/// never reattached in place. A timestamped sibling under a `quarantine/`
/// subdir of the volume's own directory (so it stays on the same filesystem →
/// `rename` is atomic, and stays OUTSIDE the artifact).
pub fn quarantine_path(image_path: &Path, now_unix_secs: u64) -> PathBuf {
    let dir = image_path.parent().unwrap_or_else(|| Path::new(".")).join("quarantine");
    let stem = image_path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    dir.join(format!("{stem}.dirty.{now_unix_secs}"))
}

/// Atomically move a dirty backing file into quarantine, returning the
/// quarantine path. The volume is preserved (never deleted) so an operator /
/// backup tool can recover it; the caller then re-creates a fresh empty volume
/// at the original path. Fails closed if the move fails (better to refuse the
/// run than to reattach a torn volume).
pub fn quarantine_volume(image_path: &Path, quarantine_path: &Path) -> Result<(), String> {
    if let Some(parent) = quarantine_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    std::fs::rename(image_path, quarantine_path).map_err(|e| {
        format!("quarantine {} -> {}: {e}", image_path.display(), quarantine_path.display())
    })?;
    // The stale in-use marker follows the volume out of the way so the freshly
    // created replacement starts Clean.
    let _ = std::fs::remove_file(dirty_marker_path(image_path));
    Ok(())
}

// ---------------------------------------------------------------------------
// 4. Quota
// ---------------------------------------------------------------------------

/// Size ceilings for a single owner's durable state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaPolicy {
    /// Max bytes any single state volume may request.
    pub max_volume_bytes: u64,
    /// Max total bytes across ALL of one owner's state volumes.
    pub max_owner_total_bytes: u64,
}

/// Why a requested volume size is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuotaError {
    PerVolume { requested: u64, limit: u64 },
    OwnerTotal { existing: u64, requested: u64, limit: u64 },
}

impl std::fmt::Display for QuotaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QuotaError::PerVolume { requested, limit } => write!(
                f,
                "state volume size {requested} B exceeds the per-volume quota of {limit} B"
            ),
            QuotaError::OwnerTotal { existing, requested, limit } => write!(
                f,
                "state volume of {requested} B would push this owner's total durable state to \
                 {} B, over the {limit} B quota (currently {existing} B)",
                existing + requested
            ),
        }
    }
}

/// Check a new volume request against the policy given the owner's existing
/// total usage. Pure — the caller supplies `existing_owner_bytes` (summed from
/// the owner's state dir) and the `requested_bytes` for the new/growing volume.
pub fn check_quota(
    policy: &QuotaPolicy,
    existing_owner_bytes: u64,
    requested_bytes: u64,
) -> Result<(), QuotaError> {
    if requested_bytes > policy.max_volume_bytes {
        return Err(QuotaError::PerVolume {
            requested: requested_bytes,
            limit: policy.max_volume_bytes,
        });
    }
    let total = existing_owner_bytes.saturating_add(requested_bytes);
    if total > policy.max_owner_total_bytes {
        return Err(QuotaError::OwnerTotal {
            existing: existing_owner_bytes,
            requested: requested_bytes,
            limit: policy.max_owner_total_bytes,
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 5. Backup / export hook
// ---------------------------------------------------------------------------

/// A point-in-time export of a state volume's backing file to `dest`.
///
/// Implementations MUST NOT touch the immutable artifact — a backup copies the
/// mutable state block device only. The caller is responsible for quiescing the
/// volume (guest `sync`ed, or a filesystem-level snapshot) before invoking this;
/// the hook just moves bytes.
pub trait StateBackupHook {
    fn export(&self, image_path: &Path, dest: &Path) -> Result<(), String>;
}

/// Real, working export: a plain file copy of the backing image. Sufficient for
/// a cold (VM-stopped) backup; a live/consistent backup is a follow-up that
/// would snapshot the block device first.
pub struct FileCopyBackupHook;

impl StateBackupHook for FileCopyBackupHook {
    fn export(&self, image_path: &Path, dest: &Path) -> Result<(), String> {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        std::fs::copy(image_path, dest)
            .map(|_| ())
            .map_err(|e| format!("backup {} -> {}: {e}", image_path.display(), dest.display()))
    }
}

// ---------------------------------------------------------------------------
// 6. Attach/detach lifecycle interface (pure decision)
// ---------------------------------------------------------------------------

/// The runner's decision, per volume, at attach time — the composition of the
/// dirty-detach check, the compatibility gate and quota into one verdict. Pure:
/// [`plan_attach`] performs NO fs mutation; the caller acts on the verdict
/// (quarantine + recreate, run `state_volume::prepare_volumes`, or abort).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachPlan {
    /// No backing file yet — create a fresh, empty volume then attach.
    CreateFresh,
    /// An existing, clean, compatible volume — attach and reuse it as-is.
    ReuseExisting,
    /// The volume was left dirty — quarantine it, then create a fresh one.
    QuarantineThenFresh,
    /// The volume is intact but incompatible with the artifact — abort the run.
    RejectIncompatible(CompatError),
    /// The request violates quota — abort the run.
    RejectQuota(QuotaError),
}

/// Compose the attach decision for one volume.
///
/// - `contract`: the artifact's schema expectation.
/// - `existing`: `Some((header, detach_state))` if a backing file already
///   exists (its parsed schema header + whether it was cleanly detached);
///   `None` for a first-ever attach.
/// - `quota` / `existing_owner_bytes` / `requested_bytes`: the quota inputs
///   (checked for BOTH fresh and reuse — a reused volume still counts, and a
///   fresh one must fit).
///
/// Order of checks: quota first (cheapest, applies to every path) → dirty
/// (quarantine supersedes compat: a torn volume is replaced fresh, so its
/// possibly-garbage header is never gated) → compat.
pub fn plan_attach(
    contract: &ArtifactStateContract,
    existing: Option<(&StateSchemaVersion, DetachState)>,
    quota: &QuotaPolicy,
    existing_owner_bytes: u64,
    requested_bytes: u64,
) -> AttachPlan {
    if let Err(e) = check_quota(quota, existing_owner_bytes, requested_bytes) {
        return AttachPlan::RejectQuota(e);
    }
    match existing {
        None => AttachPlan::CreateFresh,
        Some((_, DetachState::Dirty)) => AttachPlan::QuarantineThenFresh,
        Some((header, DetachState::Clean)) => match compat_gate(contract, header) {
            Ok(()) => AttachPlan::ReuseExisting,
            Err(e) => AttachPlan::RejectIncompatible(e),
        },
    }
}

/// The `attach_decision` a caller uses when it only cares about the
/// dirty-detach dimension (e.g. a maintenance sweep that quarantines torn
/// volumes independent of any artifact). A thin, deliberately-named alias over
/// [`detach_state`] returning the action, not the state.
pub fn attach_decision(image_path: &Path) -> DirtyAction {
    match detach_state(image_path) {
        DetachState::Clean => DirtyAction::AttachClean,
        DetachState::Dirty => DirtyAction::Quarantine,
    }
}

/// The action implied by a volume's [`DetachState`] at attach time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirtyAction {
    AttachClean,
    Quarantine,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract(name: &str, version: u32, revision: u32) -> ArtifactStateContract {
        ArtifactStateContract {
            artifact_digest: "artifact-digest-abc".to_string(),
            expects: StateSchemaVersion::new(name, version, revision),
        }
    }

    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tmpdir")
    }

    // --- compat gate -------------------------------------------------------

    #[test]
    fn compat_gate_accepts_identical_schema() {
        let c = contract("sqlite-app-db", 3, 7);
        assert!(compat_gate(&c, &StateSchemaVersion::new("sqlite-app-db", 3, 7)).is_ok());
    }

    #[test]
    fn compat_gate_accepts_older_revision_within_the_same_version() {
        // reader at revision 7 understands a volume written at revision 5.
        let c = contract("sqlite-app-db", 3, 7);
        assert!(compat_gate(&c, &StateSchemaVersion::new("sqlite-app-db", 3, 5)).is_ok());
    }

    #[test]
    fn compat_gate_rejects_newer_revision() {
        let c = contract("sqlite-app-db", 3, 7);
        let err = compat_gate(&c, &StateSchemaVersion::new("sqlite-app-db", 3, 9)).unwrap_err();
        assert_eq!(
            err,
            CompatError::RevisionTooNew { artifact_understands: 7, volume_written_at: 9 }
        );
    }

    #[test]
    fn compat_gate_rejects_version_mismatch() {
        let c = contract("sqlite-app-db", 3, 7);
        let err = compat_gate(&c, &StateSchemaVersion::new("sqlite-app-db", 4, 0)).unwrap_err();
        assert_eq!(err, CompatError::VersionMismatch { expected: 3, found: 4 });
    }

    #[test]
    fn compat_gate_rejects_different_lineage() {
        let c = contract("sqlite-app-db", 3, 7);
        let err = compat_gate(&c, &StateSchemaVersion::new("postgres-app-db", 3, 7)).unwrap_err();
        assert!(matches!(err, CompatError::SchemaMismatch { .. }));
    }

    // --- dirty-detach quarantine ------------------------------------------

    #[test]
    fn a_clean_detach_leaves_no_marker_and_attaches_clean() {
        let dir = tmpdir();
        let img = dir.path().join("dbdata.img");
        std::fs::write(&img, b"x").unwrap();
        mark_in_use(&img).unwrap();
        assert_eq!(detach_state(&img), DetachState::Dirty, "in-use before detach");
        mark_clean_detach(&img);
        assert_eq!(detach_state(&img), DetachState::Clean);
        assert_eq!(attach_decision(&img), DirtyAction::AttachClean);
    }

    #[test]
    fn a_missing_clean_detach_is_seen_as_dirty_at_next_attach() {
        // Simulate a crash: mark_in_use ran, the matching mark_clean_detach did
        // not (the VMM was kill()ed). The next attach must see Dirty.
        let dir = tmpdir();
        let img = dir.path().join("dbdata.img");
        std::fs::write(&img, b"x").unwrap();
        mark_in_use(&img).unwrap();
        assert_eq!(detach_state(&img), DetachState::Dirty);
        assert_eq!(attach_decision(&img), DirtyAction::Quarantine);
    }

    #[test]
    fn quarantine_moves_the_dirty_volume_aside_and_clears_the_marker() {
        let dir = tmpdir();
        let img = dir.path().join("dbdata.img");
        std::fs::write(&img, b"torn-contents").unwrap();
        mark_in_use(&img).unwrap();

        let q = quarantine_path(&img, 1_700_000_000);
        quarantine_volume(&img, &q).unwrap();

        assert!(!img.exists(), "dirty volume moved out of the attach path");
        assert!(q.exists(), "dirty volume preserved in quarantine for recovery");
        assert_eq!(std::fs::read(&q).unwrap(), b"torn-contents", "contents preserved, not deleted");
        // A freshly-created replacement at the original path starts Clean.
        assert_eq!(detach_state(&img), DetachState::Clean);
    }

    #[test]
    fn quarantine_path_is_a_sibling_under_a_quarantine_dir_not_inside_the_artifact() {
        let img = Path::new("/work/state/owner/dbdata.img");
        let q = quarantine_path(img, 42);
        assert_eq!(q, Path::new("/work/state/owner/quarantine/dbdata.img.dirty.42"));
    }

    // --- quota -------------------------------------------------------------

    #[test]
    fn quota_accepts_a_request_within_both_ceilings() {
        let p = QuotaPolicy { max_volume_bytes: 100, max_owner_total_bytes: 500 };
        assert!(check_quota(&p, 300, 100).is_ok());
    }

    #[test]
    fn quota_rejects_an_oversize_single_volume() {
        let p = QuotaPolicy { max_volume_bytes: 100, max_owner_total_bytes: 500 };
        assert_eq!(
            check_quota(&p, 0, 101).unwrap_err(),
            QuotaError::PerVolume { requested: 101, limit: 100 }
        );
    }

    #[test]
    fn quota_rejects_when_owner_total_would_be_exceeded() {
        let p = QuotaPolicy { max_volume_bytes: 100, max_owner_total_bytes: 500 };
        assert_eq!(
            check_quota(&p, 450, 100).unwrap_err(),
            QuotaError::OwnerTotal { existing: 450, requested: 100, limit: 500 }
        );
    }

    #[test]
    fn quota_total_check_saturates_and_does_not_overflow() {
        let p = QuotaPolicy { max_volume_bytes: u64::MAX, max_owner_total_bytes: u64::MAX };
        // existing + requested would overflow u64; saturating_add keeps it at
        // MAX which is <= the MAX ceiling, so this is accepted (no panic).
        assert!(check_quota(&p, u64::MAX, 10).is_ok());
    }

    // --- backup hook -------------------------------------------------------

    #[test]
    fn file_copy_backup_exports_a_faithful_copy_without_touching_the_source() {
        let dir = tmpdir();
        let img = dir.path().join("dbdata.img");
        std::fs::write(&img, b"durable-bytes").unwrap();
        let dest = dir.path().join("backups").join("dbdata.bak");

        FileCopyBackupHook.export(&img, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"durable-bytes");
        assert!(img.exists(), "source (state volume) is untouched by an export");
        assert_eq!(std::fs::read(&img).unwrap(), b"durable-bytes");
    }

    // --- attach plan (lifecycle composition) ------------------------------

    #[test]
    fn plan_attach_creates_fresh_when_no_backing_file_exists() {
        let c = contract("s", 1, 0);
        let q = QuotaPolicy { max_volume_bytes: 1000, max_owner_total_bytes: 1000 };
        assert_eq!(plan_attach(&c, None, &q, 0, 100), AttachPlan::CreateFresh);
    }

    #[test]
    fn plan_attach_reuses_a_clean_compatible_volume() {
        let c = contract("s", 1, 5);
        let q = QuotaPolicy { max_volume_bytes: 1000, max_owner_total_bytes: 1000 };
        let header = StateSchemaVersion::new("s", 1, 3);
        assert_eq!(
            plan_attach(&c, Some((&header, DetachState::Clean)), &q, 100, 100),
            AttachPlan::ReuseExisting
        );
    }

    #[test]
    fn plan_attach_quarantines_a_dirty_volume_before_checking_compat() {
        // Even a dirty volume whose header would be INCOMPATIBLE is quarantined
        // (replaced fresh), not reported as incompatible — its header is not
        // trusted after a torn detach.
        let c = contract("s", 1, 5);
        let q = QuotaPolicy { max_volume_bytes: 1000, max_owner_total_bytes: 1000 };
        let incompat_header = StateSchemaVersion::new("other", 9, 9);
        assert_eq!(
            plan_attach(&c, Some((&incompat_header, DetachState::Dirty)), &q, 100, 100),
            AttachPlan::QuarantineThenFresh
        );
    }

    #[test]
    fn plan_attach_rejects_a_clean_but_incompatible_volume() {
        let c = contract("s", 1, 5);
        let q = QuotaPolicy { max_volume_bytes: 1000, max_owner_total_bytes: 1000 };
        let header = StateSchemaVersion::new("s", 2, 0); // version mismatch
        assert_eq!(
            plan_attach(&c, Some((&header, DetachState::Clean)), &q, 100, 100),
            AttachPlan::RejectIncompatible(CompatError::VersionMismatch { expected: 1, found: 2 })
        );
    }

    #[test]
    fn plan_attach_rejects_on_quota_before_anything_else() {
        let c = contract("s", 1, 5);
        let q = QuotaPolicy { max_volume_bytes: 50, max_owner_total_bytes: 1000 };
        // requested 100 > per-volume 50 → quota reject even for a fresh volume.
        assert_eq!(
            plan_attach(&c, None, &q, 0, 100),
            AttachPlan::RejectQuota(QuotaError::PerVolume { requested: 100, limit: 50 })
        );
    }
}
