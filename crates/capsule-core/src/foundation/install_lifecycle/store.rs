//! `InstallInstanceStore` — filesystem layout for installed-app instances and revisions.
//!
//! # Directory structure
//!
//! ```text
//! <store_root>/instances/<installed_app_id>/
//!   app.json                     # AppRecord (publisher, slug, version, …)
//!   profiles/
//!     <profile_id>/
//!       profile.json             # LaunchProfile (env_refs, secret_refs, port_policy, …)
//!       current_revision         # symlink → ../../revisions/<install_revision_id>
//!   state/                       # instance-scoped mutable state
//!   sessions/                    # session records (indexed by capsule_instance_key)
//!   receipts/                    # execution receipts
//!   integrations/                # OS integration metadata (shortcuts, dock entries)
//!   archived_metadata/           # pre-update snapshots
//!
//! <store_root>/revisions/<install_revision_id>/
//!   artifact_manifest.json       # immutable artifact manifest
//!   output/                      # materialized build output (frozen)
//!   source_provenance/           # git ref, GitHub release tag, OCI digest, …
//!   lock/                        # resolved lock files
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::ids::{InstallRevisionId, InstalledAppId, ProfileId};
use super::records::{
    ArtifactBuild, InstallReceipt, InstallRevision, RequirementGraphSnapshot, StateContractSnapshot,
};

// ── AppRecord ──────────────────────────────────────────────────────────────

/// Metadata persisted in `instances/<installed_app_id>/app.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppRecord {
    pub installed_app_id: InstalledAppId,
    pub publisher: String,
    pub slug: String,
    /// The capsule handle used for launching (e.g. `"publisher/slug"`).
    /// Stored here so `ato launch` can resolve the correct run target without
    /// hard-coding assumptions about the filesystem layout.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub capsule_handle: String,
    /// The version string of the *first* install; updated on update.
    pub version: String,
    /// RFC 3339 timestamp of first install.
    pub installed_at: String,
    /// RFC 3339 timestamp of most recent update.
    pub updated_at: String,
}

// ── LaunchProfile ──────────────────────────────────────────────────────────

/// Mutable profile configuration persisted under `profiles/<id>/profile.json`.
///
/// Secret *values* are never stored here — only `secret_ref` keys.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct LaunchProfile {
    pub profile_id: ProfileId,
    /// Environment variable *references* (not values): `"ENV_NAME"` → `"${secret:my_key}"`.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub env_refs: std::collections::HashMap<String, String>,
    /// Secret ref keys required by this profile.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_refs: Vec<String>,
    /// Extra CLI args appended at launch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Port policy: `"auto"` (default) or `"fixed:<port>"`.
    #[serde(default = "default_port_policy")]
    pub port_policy: String,
    /// Concurrency policy: `"single"` (default) or `"multi"`.
    #[serde(default = "default_concurrency_policy")]
    pub concurrency_policy: String,
    /// Isolation preference: `"default"` | `"strict"` | `"relaxed"`.
    #[serde(default = "default_isolation")]
    pub isolation: String,
}

fn default_port_policy() -> String {
    "auto".to_owned()
}

fn default_concurrency_policy() -> String {
    "single".to_owned()
}

fn default_isolation() -> String {
    "default".to_owned()
}

// ── InstallInstanceStore ───────────────────────────────────────────────────

/// Filesystem-backed store for installed-app instances and immutable revisions.
#[derive(Debug, Clone)]
pub struct InstallInstanceStore {
    /// Root directory, e.g. `~/.ato/instances_v1/`.
    root: PathBuf,
}

impl InstallInstanceStore {
    /// Create (or open) a store rooted at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("instances"))
            .with_context(|| format!("create instances dir under {}", root.display()))?;
        fs::create_dir_all(root.join("revisions"))
            .with_context(|| format!("create revisions dir under {}", root.display()))?;
        Ok(Self { root })
    }

    // ── paths ──────────────────────────────────────────────────────────────

    pub fn instance_dir(&self, app: &InstalledAppId) -> PathBuf {
        self.root.join("instances").join(app.as_str())
    }

    pub fn app_record_path(&self, app: &InstalledAppId) -> PathBuf {
        self.instance_dir(app).join("app.json")
    }

    pub fn profile_dir(&self, app: &InstalledAppId, profile: &ProfileId) -> PathBuf {
        self.instance_dir(app)
            .join("profiles")
            .join(profile.as_str())
    }

    pub fn profile_json_path(&self, app: &InstalledAppId, profile: &ProfileId) -> PathBuf {
        self.profile_dir(app, profile).join("profile.json")
    }

    /// Returns the path of the `current_revision` symlink for a profile.
    pub fn current_revision_link(&self, app: &InstalledAppId, profile: &ProfileId) -> PathBuf {
        self.profile_dir(app, profile).join("current_revision")
    }

    pub fn revision_dir(&self, rev: &InstallRevisionId) -> PathBuf {
        self.root.join("revisions").join(rev.as_str())
    }

    pub fn revision_artifact_manifest_path(&self, rev: &InstallRevisionId) -> PathBuf {
        self.revision_dir(rev).join("artifact_manifest.json")
    }

    pub fn revision_output_dir(&self, rev: &InstallRevisionId) -> PathBuf {
        self.revision_dir(rev).join("output")
    }

    pub fn revision_source_provenance_dir(&self, rev: &InstallRevisionId) -> PathBuf {
        self.revision_dir(rev).join("source_provenance")
    }

    pub fn revision_lock_dir(&self, rev: &InstallRevisionId) -> PathBuf {
        self.revision_dir(rev).join("lock")
    }

    pub fn state_dir(&self, app: &InstalledAppId) -> PathBuf {
        self.instance_dir(app).join("state")
    }

    pub fn sessions_dir(&self, app: &InstalledAppId) -> PathBuf {
        self.instance_dir(app).join("sessions")
    }

    pub fn receipts_dir(&self, app: &InstalledAppId) -> PathBuf {
        self.instance_dir(app).join("receipts")
    }

    // ── app record ─────────────────────────────────────────────────────────

    /// Persist a new [`AppRecord`], creating the instance directory layout.
    pub fn write_app_record(&self, record: &AppRecord) -> Result<()> {
        let app = &record.installed_app_id;
        let inst_dir = self.instance_dir(app);
        fs::create_dir_all(&inst_dir)
            .with_context(|| format!("create instance dir {}", inst_dir.display()))?;
        for sub in &[
            "profiles",
            "state",
            "sessions",
            "receipts",
            "integrations",
            "archived_metadata",
        ] {
            fs::create_dir_all(inst_dir.join(sub))?;
        }
        let json = serde_json::to_string_pretty(record)?;
        atomic_write(&self.app_record_path(app), json.as_bytes())?;
        Ok(())
    }

    /// Read the [`AppRecord`] for an installed app.
    pub fn read_app_record(&self, app: &InstalledAppId) -> Result<AppRecord> {
        let path = self.app_record_path(app);
        let bytes =
            fs::read(&path).with_context(|| format!("read app record {}", path.display()))?;
        serde_json::from_slice(&bytes).with_context(|| "parse app record")
    }

    // ── profiles ───────────────────────────────────────────────────────────

    /// Persist a [`LaunchProfile`] for an app.
    pub fn write_profile(&self, app: &InstalledAppId, profile: &LaunchProfile) -> Result<()> {
        let dir = self.profile_dir(app, &profile.profile_id);
        fs::create_dir_all(&dir)?;
        let json = serde_json::to_string_pretty(profile)?;
        atomic_write(
            &self.profile_json_path(app, &profile.profile_id),
            json.as_bytes(),
        )?;
        Ok(())
    }

    /// Read the [`LaunchProfile`] for an app/profile.
    pub fn read_profile(&self, app: &InstalledAppId, profile: &ProfileId) -> Result<LaunchProfile> {
        let path = self.profile_json_path(app, profile);
        let bytes = fs::read(&path).with_context(|| format!("read profile {}", path.display()))?;
        serde_json::from_slice(&bytes).with_context(|| "parse launch profile")
    }

    // ── revision directory scaffolding ────────────────────────────────────

    /// Create the directory skeleton for a new revision.
    pub fn scaffold_revision(&self, rev: &InstallRevisionId) -> Result<()> {
        let rev_dir = self.revision_dir(rev);
        for sub in &["output", "source_provenance", "lock"] {
            fs::create_dir_all(rev_dir.join(sub))
                .with_context(|| format!("create revision subdir in {}", rev_dir.display()))?;
        }
        Ok(())
    }

    /// Atomically update the `current_revision` symlink for a profile.
    ///
    /// Uses a temp-then-rename strategy to avoid leaving a broken symlink if
    /// the process is interrupted.
    pub fn set_current_revision(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
        rev: &InstallRevisionId,
    ) -> Result<()> {
        let link = self.current_revision_link(app, profile);
        let rev_dir = self.revision_dir(rev);

        // Write via temp file / link then rename for atomicity.
        let tmp_link = link.with_extension("tmp");
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let _ = fs::remove_file(&tmp_link);
            symlink(&rev_dir, &tmp_link).with_context(|| {
                format!(
                    "create tmp symlink {} → {}",
                    tmp_link.display(),
                    rev_dir.display()
                )
            })?;
            fs::rename(&tmp_link, &link)
                .with_context(|| format!("rename symlink to {}", link.display()))?;
        }
        #[cfg(not(unix))]
        {
            // On non-unix (Windows), write a plain text file with the revision id as fallback.
            atomic_write(&link, rev.as_str().as_bytes())?;
        }
        // Track revision in per-profile log. Failure here is returned — callers that want
        // best-effort behaviour should ignore the error explicitly.
        self.append_revision_to_log(app, profile, rev)?;
        Ok(())
    }

    /// Read the current revision id for a profile.
    pub fn current_revision(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
    ) -> Result<InstallRevisionId> {
        let link = self.current_revision_link(app, profile);

        #[cfg(unix)]
        {
            let target =
                fs::read_link(&link).with_context(|| format!("read symlink {}", link.display()))?;
            // The symlink points to the revision directory; extract the final component.
            let rev_id = target
                .file_name()
                .and_then(|n| n.to_str())
                .map(InstallRevisionId::new)
                .with_context(|| "extract revision id from symlink target")?;
            Ok(rev_id)
        }
        #[cfg(not(unix))]
        {
            let bytes = fs::read(&link)
                .with_context(|| format!("read current_revision file {}", link.display()))?;
            let rev_id =
                String::from_utf8(bytes).with_context(|| "current_revision is not valid UTF-8")?;
            Ok(InstallRevisionId::new(rev_id.trim()))
        }
    }

    /// List all installed app IDs.
    pub fn list_installed_apps(&self) -> Result<Vec<InstalledAppId>> {
        let instances_dir = self.root.join("instances");
        let mut apps = Vec::new();
        for entry in fs::read_dir(&instances_dir)
            .with_context(|| format!("read instances dir {}", instances_dir.display()))?
        {
            let entry = entry?;
            if entry.file_type()?.is_dir()
                && let Some(name) = entry.file_name().to_str()
            {
                apps.push(InstalledAppId::new(name));
            }
        }
        Ok(apps)
    }

    // ── Revision log ──────────────────────────────────────────────────────

    fn profile_revision_log_path(&self, app: &InstalledAppId, profile: &ProfileId) -> PathBuf {
        self.profile_dir(app, profile).join("revision_log.json")
    }

    /// Append a revision entry to the profile's revision log.
    /// Called automatically by `set_current_revision`.
    fn append_revision_to_log(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
        rev: &InstallRevisionId,
    ) -> Result<()> {
        let log_path = self.profile_revision_log_path(app, profile);
        let mut log: Vec<String> = if log_path.exists() {
            let raw = fs::read(&log_path)
                .with_context(|| format!("read revision log {}", log_path.display()))?;
            serde_json::from_slice(&raw)
                .with_context(|| format!("parse revision log {}", log_path.display()))?
        } else {
            Vec::new()
        };
        let rev_str = rev.as_str().to_owned();
        if !log.contains(&rev_str) {
            log.push(rev_str);
        }
        let json = serde_json::to_vec_pretty(&log)?;
        atomic_write(&log_path, &json)?;
        Ok(())
    }

    /// List all revision IDs ever set as current for a profile, in insertion order.
    pub fn list_profile_revisions(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
    ) -> Result<Vec<InstallRevisionId>> {
        let log_path = self.profile_revision_log_path(app, profile);
        if !log_path.exists() {
            return Ok(vec![]);
        }
        let raw = fs::read(&log_path)
            .with_context(|| format!("read revision log {}", log_path.display()))?;
        let entries: Vec<String> = serde_json::from_slice(&raw)
            .with_context(|| format!("parse revision log {}", log_path.display()))?;
        Ok(entries.into_iter().map(InstallRevisionId::new).collect())
    }

    /// Read the raw `artifact_manifest.json` for a revision as a JSON value.
    /// Returns `None` if the manifest does not exist yet.
    pub fn read_revision_manifest(
        &self,
        rev: &InstallRevisionId,
    ) -> Result<Option<serde_json::Value>> {
        let path = self.revision_artifact_manifest_path(rev);
        if !path.exists() {
            return Ok(None);
        }
        let raw = fs::read(&path)
            .with_context(|| format!("read artifact manifest {}", path.display()))?;
        let val = serde_json::from_slice(&raw)
            .with_context(|| format!("parse artifact manifest {}", path.display()))?;
        Ok(Some(val))
    }

    // ── Install output records (#581 wave 2) ────────────────────────────────
    //
    // The typed launch-reuse records an install revision produces, persisted
    // alongside the existing `artifact_manifest.json` / `output/` layout under
    // `revisions/<install_revision_id>/`:
    //
    //   revision.json            -> InstallRevision (self-contained authority)
    //   artifact-build.json      -> ArtifactBuild
    //   requirement-graph.json   -> RequirementGraphSnapshot
    //   state-contracts.json     -> Vec<StateContractSnapshot>
    //   install-receipt.json     -> InstallReceipt
    //
    // `revision.json` is written *last* by the finalizer and is the per-revision
    // finalization marker: a revision whose sub-records were only partially
    // written (no `revision.json`) is not considered finalized — see
    // [`is_revision_finalized`] / [`read_install_revision`].
    //
    // These records carry only install-time identity/metadata. They never hold
    // secret values, nor any session/runtime/observed fact (session id, dynamic
    // port, pid, container id, live route, log cursor, observed status).

    pub fn revision_install_record_path(&self, rev: &InstallRevisionId) -> PathBuf {
        self.revision_dir(rev).join("revision.json")
    }

    pub fn revision_artifact_build_path(&self, rev: &InstallRevisionId) -> PathBuf {
        self.revision_dir(rev).join("artifact-build.json")
    }

    pub fn revision_requirement_graph_path(&self, rev: &InstallRevisionId) -> PathBuf {
        self.revision_dir(rev).join("requirement-graph.json")
    }

    pub fn revision_state_contracts_path(&self, rev: &InstallRevisionId) -> PathBuf {
        self.revision_dir(rev).join("state-contracts.json")
    }

    pub fn revision_install_receipt_path(&self, rev: &InstallRevisionId) -> PathBuf {
        self.revision_dir(rev).join("install-receipt.json")
    }

    /// Atomically write any revision-scoped JSON record, creating the revision
    /// directory if needed.
    fn write_revision_json<T: Serialize>(&self, path: &Path, value: &T) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create revision dir {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(value)
            .with_context(|| format!("serialize record {}", path.display()))?;
        atomic_write(path, json.as_bytes())
    }

    /// Persist the [`ArtifactBuild`] record for a revision (atomic).
    pub fn write_artifact_build(
        &self,
        rev: &InstallRevisionId,
        build: &ArtifactBuild,
    ) -> Result<()> {
        self.write_revision_json(&self.revision_artifact_build_path(rev), build)
    }

    /// Persist the [`RequirementGraphSnapshot`] for a revision (atomic).
    pub fn write_requirement_graph_snapshot(
        &self,
        rev: &InstallRevisionId,
        snapshot: &RequirementGraphSnapshot,
    ) -> Result<()> {
        self.write_revision_json(&self.revision_requirement_graph_path(rev), snapshot)
    }

    /// Persist the state-contract snapshots for a revision (atomic). An empty
    /// slice is persisted explicitly as `[]`.
    pub fn write_state_contracts(
        &self,
        rev: &InstallRevisionId,
        contracts: &[StateContractSnapshot],
    ) -> Result<()> {
        self.write_revision_json(&self.revision_state_contracts_path(rev), &contracts)
    }

    /// Persist the [`InstallReceipt`] for a revision (atomic).
    pub fn write_install_receipt(
        &self,
        rev: &InstallRevisionId,
        receipt: &InstallReceipt,
    ) -> Result<()> {
        self.write_revision_json(&self.revision_install_receipt_path(rev), receipt)
    }

    /// Persist the [`InstallRevision`] marker (atomic). Must be written only
    /// after every sub-record above has been written successfully.
    pub fn write_install_revision(&self, revision: &InstallRevision) -> Result<()> {
        self.write_revision_json(
            &self.revision_install_record_path(&revision.install_revision_id),
            revision,
        )
    }

    /// True if `revision.json` exists for this revision — i.e. finalization
    /// reached the marker write. A scaffolded-but-incomplete revision (output
    /// copied, sub-records partially written) returns `false`.
    pub fn is_revision_finalized(&self, rev: &InstallRevisionId) -> bool {
        self.revision_install_record_path(rev).exists()
    }

    /// Read the [`InstallRevision`] for a revision. Errors if `revision.json`
    /// is missing (revision not finalized) or malformed.
    pub fn read_install_revision(&self, rev: &InstallRevisionId) -> Result<InstallRevision> {
        let path = self.revision_install_record_path(rev);
        if !path.exists() {
            anyhow::bail!(
                "revision '{}' is not finalized: revision.json is missing",
                rev.as_str()
            );
        }
        let raw = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_slice(&raw).with_context(|| format!("parse {}", path.display()))
    }

    /// Read the [`ArtifactBuild`] record for a revision.
    pub fn read_artifact_build(&self, rev: &InstallRevisionId) -> Result<ArtifactBuild> {
        let path = self.revision_artifact_build_path(rev);
        let raw = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_slice(&raw).with_context(|| format!("parse {}", path.display()))
    }

    /// Read the [`RequirementGraphSnapshot`] for a revision.
    pub fn read_requirement_graph_snapshot(
        &self,
        rev: &InstallRevisionId,
    ) -> Result<RequirementGraphSnapshot> {
        let path = self.revision_requirement_graph_path(rev);
        let raw = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_slice(&raw).with_context(|| format!("parse {}", path.display()))
    }

    /// Read the state-contract snapshots for a revision.
    pub fn read_state_contracts(
        &self,
        rev: &InstallRevisionId,
    ) -> Result<Vec<StateContractSnapshot>> {
        let path = self.revision_state_contracts_path(rev);
        let raw = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_slice(&raw).with_context(|| format!("parse {}", path.display()))
    }

    /// Read the [`InstallReceipt`] for a revision.
    pub fn read_install_receipt(&self, rev: &InstallRevisionId) -> Result<InstallReceipt> {
        let path = self.revision_install_receipt_path(rev);
        let raw = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_slice(&raw).with_context(|| format!("parse {}", path.display()))
    }

    /// Convenience readback for launch reuse: follow the `current_revision`
    /// pointer for `(app, profile)` and load the finalized [`InstallRevision`].
    ///
    /// `(app, profile)` determines the [`InstallProfileKey`](super::ids::InstallProfileKey),
    /// so this is the `install_profile_key -> current revision -> records` chain
    /// a launcher needs without recomputing the install.
    pub fn read_current_install_revision(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
    ) -> Result<InstallRevision> {
        let rev = self.current_revision(app, profile)?;
        self.read_install_revision(&rev)
    }

    // ── Profile list ──────────────────────────────────────────────────────

    /// List all profile IDs for an installed app.
    // ── GC helpers ─────────────────────────────────────────────────────────
    ///
    /// Path of the per-revision pin marker file.
    pub fn revision_pin_path(&self, rev: &InstallRevisionId) -> PathBuf {
        self.revision_dir(rev).join(".pinned")
    }

    /// Mark a revision as user-pinned (prevents GC from deleting it).
    pub fn pin_revision(&self, rev: &InstallRevisionId) -> Result<()> {
        let rev_dir = self.revision_dir(rev);
        if !rev_dir.exists() {
            anyhow::bail!("revision '{}' does not exist", rev.as_str());
        }
        let pin = self.revision_pin_path(rev);
        fs::write(&pin, b"pinned").with_context(|| format!("write pin {}", pin.display()))?;
        Ok(())
    }

    /// Remove a user pin from a revision.
    pub fn unpin_revision(&self, rev: &InstallRevisionId) -> Result<()> {
        let pin = self.revision_pin_path(rev);
        if pin.exists() {
            fs::remove_file(&pin).with_context(|| format!("remove pin {}", pin.display()))?;
        }
        Ok(())
    }

    /// Returns `true` if the revision has a user pin.
    pub fn is_pinned(&self, rev: &InstallRevisionId) -> bool {
        self.revision_pin_path(rev).exists()
    }

    /// List every revision directory present in `revisions/`.
    pub fn list_all_revisions(&self) -> Result<Vec<InstallRevisionId>> {
        let revs_dir = self.root.join("revisions");
        if !revs_dir.exists() {
            return Ok(vec![]);
        }
        let mut revisions = Vec::new();
        for entry in
            fs::read_dir(&revs_dir).with_context(|| format!("read {}", revs_dir.display()))?
        {
            let entry = entry?;
            if entry.file_type()?.is_dir()
                && let Some(name) = entry.file_name().to_str()
            {
                revisions.push(InstallRevisionId::new(name));
            }
        }
        Ok(revisions)
    }

    /// Delete a single revision directory, refusing to touch a revision that
    /// is currently the `current_revision` of any profile.
    ///
    /// This is the safe wrapper used by `ato gc`. It walks every installed
    /// app's profiles and bails out with `Err` if `rev` matches any
    /// `current_revision` symlink. For test fixtures and other contexts where
    /// the caller has already proven safety, use [`delete_revision_unchecked`].
    pub fn delete_revision(&self, rev: &InstallRevisionId) -> Result<()> {
        let apps = self
            .list_installed_apps()
            .with_context(|| "delete_revision: enumerate installed apps")?;
        for app in &apps {
            let profiles = self
                .list_profiles(app)
                .with_context(|| format!("delete_revision: list profiles for {}", app.as_str()))?;
            for profile in &profiles {
                let link = self.current_revision_link(app, profile);
                if !link.exists() {
                    continue;
                }
                let current = self.current_revision(app, profile).with_context(|| {
                    format!(
                        "delete_revision: read current_revision for {}/{}",
                        app.as_str(),
                        profile.as_str()
                    )
                })?;
                if current.as_str() == rev.as_str() {
                    anyhow::bail!(
                        "refusing to delete revision '{}': still current for profile {}/{}",
                        rev.as_str(),
                        app.as_str(),
                        profile.as_str()
                    );
                }
            }
        }
        self.delete_revision_unchecked(rev)
    }

    /// Delete a revision directory without checking protection rules.
    ///
    /// Caller is fully responsible for verifying the revision is not
    /// referenced by any profile, active session, or pin marker. Misuse
    /// will corrupt installed apps. Prefer [`delete_revision`].
    pub fn delete_revision_unchecked(&self, rev: &InstallRevisionId) -> Result<()> {
        let rev_dir = self.revision_dir(rev);
        if !rev_dir.exists() {
            return Ok(());
        }
        fs::remove_dir_all(&rev_dir)
            .with_context(|| format!("delete revision dir {}", rev_dir.display()))?;
        Ok(())
    }

    /// Determine which revisions can be safely deleted.
    ///
    /// A revision is **protected** (kept) if any of these hold:
    /// - It appears in `protected_rev_ids` (current + active-session + user-pinned)
    /// - It is user-pinned on disk
    /// - It falls within the last `keep_last_n` revisions in the log of any profile
    /// - Its `finalized_at` in the artifact manifest is within `retention_days` of now
    ///
    /// Everything else is reclaimable.
    ///
    /// `all_revisions` is the full list from [`list_all_revisions`].
    ///
    /// # Errors
    ///
    /// GC is a destructive operation, so this function is **fail-closed**:
    /// any I/O error or parse failure while walking the installed apps,
    /// profiles, revision logs, or revision manifests is propagated as
    /// `Err`. A corrupt `revision_log.json` or `artifact_manifest.json`
    /// must stop GC entirely rather than cause a revision to be wrongly
    /// classified as reclaimable.
    pub fn collect_reclaimable_revisions(
        &self,
        protected_rev_ids: &std::collections::HashSet<String>,
        all_revisions: &[InstallRevisionId],
        keep_last_n: usize,
        retention_days: u64,
    ) -> Result<Vec<InstallRevisionId>> {
        // Build set of revisions protected by recency within any profile log.
        // Any error here — listing apps/profiles or parsing a revision log —
        // aborts GC. We never want a corrupt log to be silently treated as
        // "no protected revisions for this profile".
        let mut recency_protected: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let apps = self
            .list_installed_apps()
            .with_context(|| "collect_reclaimable_revisions: list installed apps")?;
        for app in &apps {
            let profiles = self.list_profiles(app).with_context(|| {
                format!(
                    "collect_reclaimable_revisions: list profiles for {}",
                    app.as_str()
                )
            })?;
            for profile in &profiles {
                let log = self.list_profile_revisions(app, profile).with_context(|| {
                    format!(
                        "collect_reclaimable_revisions: read revision log for {}/{}",
                        app.as_str(),
                        profile.as_str()
                    )
                })?;
                for rev in log.iter().rev().take(keep_last_n) {
                    recency_protected.insert(rev.as_str().to_owned());
                }
            }
        }

        let retention_secs = retention_days.saturating_mul(86400);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut reclaimable = Vec::new();
        for rev in all_revisions {
            let id = rev.as_str();
            // Explicit protection (current_revision, active session, user pin arg).
            if protected_rev_ids.contains(id) {
                continue;
            }
            // Disk pin marker.
            if self.is_pinned(rev) {
                continue;
            }
            // Recency within profile log (keep_last_n).
            if recency_protected.contains(id) {
                continue;
            }
            // Retention window: keep if finalized_at is recent enough.
            // A corrupt manifest is an error — *not* an excuse to delete the
            // revision. If the manifest is missing entirely (None) we fall
            // through, since the retention window simply does not apply.
            let manifest = self.read_revision_manifest(rev).with_context(|| {
                format!(
                    "collect_reclaimable_revisions: read manifest for revision {}",
                    rev.as_str()
                )
            })?;
            if let Some(manifest) = manifest
                && let Some(ts) = manifest.get("finalized_at").and_then(|v| v.as_str())
            {
                let dt = chrono::DateTime::parse_from_rfc3339(ts).with_context(|| {
                    format!(
                        "collect_reclaimable_revisions: parse finalized_at '{}' for revision {}",
                        ts,
                        rev.as_str()
                    )
                })?;
                let age_secs = now.saturating_sub(dt.timestamp().max(0) as u64);
                if age_secs < retention_secs {
                    continue;
                }
            }
            reclaimable.push(rev.clone());
        }
        Ok(reclaimable)
    }

    pub fn list_profiles(&self, app: &InstalledAppId) -> Result<Vec<ProfileId>> {
        let profiles_dir = self.instance_dir(app).join("profiles");
        if !profiles_dir.exists() {
            return Ok(vec![]);
        }
        let mut profiles = Vec::new();
        for entry in fs::read_dir(&profiles_dir)
            .with_context(|| format!("read profiles dir {}", profiles_dir.display()))?
        {
            let entry = entry?;
            if entry.file_type()?.is_dir()
                && let Some(name) = entry.file_name().to_str()
            {
                profiles.push(ProfileId::new(name));
            }
        }
        Ok(profiles)
    }
}

// ── Atomic write helper ────────────────────────────────────────────────────

/// Write `bytes` to `path` atomically via a temporary file in the same directory.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .with_context(|| format!("no parent for {}", path.display()))?;
    let tmp = dir.join(format!(
        ".tmp_{}",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    fs::write(&tmp, bytes).with_context(|| format!("write tmp {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::foundation::install_lifecycle::ids::{InstalledAppId, ProfileId};

    fn temp_store() -> (tempfile::TempDir, InstallInstanceStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = InstallInstanceStore::new(dir.path()).unwrap();
        (dir, store)
    }

    #[test]
    fn write_and_read_app_record() {
        let (_dir, store) = temp_store();
        let app = InstalledAppId::new("app_test_001");
        let record = AppRecord {
            installed_app_id: app.clone(),
            publisher: "acme".into(),
            slug: "hello".into(),
            capsule_handle: "acme/hello".into(),
            version: "1.0.0".into(),
            installed_at: "2025-01-01T00:00:00Z".into(),
            updated_at: "2025-01-01T00:00:00Z".into(),
        };
        store.write_app_record(&record).unwrap();
        let read_back = store.read_app_record(&app).unwrap();
        assert_eq!(record, read_back);
    }

    #[test]
    fn write_and_read_profile() {
        let (_dir, store) = temp_store();
        let app = InstalledAppId::new("app_test_002");
        let profile_id = ProfileId::new("default");
        // Must create instance dir first.
        let record = AppRecord {
            installed_app_id: app.clone(),
            publisher: "acme".into(),
            slug: "world".into(),
            capsule_handle: "acme/world".into(),
            version: "1.0.0".into(),
            installed_at: "2025-01-01T00:00:00Z".into(),
            updated_at: "2025-01-01T00:00:00Z".into(),
        };
        store.write_app_record(&record).unwrap();

        let profile = LaunchProfile {
            profile_id: profile_id.clone(),
            env_refs: [("API_KEY".into(), "${secret:api_key}".into())]
                .into_iter()
                .collect(),
            secret_refs: vec!["api_key".into()],
            ..Default::default()
        };
        store.write_profile(&app, &profile).unwrap();
        let back = store.read_profile(&app, &profile_id).unwrap();
        assert_eq!(profile, back);
    }

    #[cfg(unix)]
    #[test]
    fn current_revision_atomic_swap() {
        let (_dir, store) = temp_store();
        let app = InstalledAppId::new("app_rev_swap");
        let profile_id = ProfileId::new("default");

        let record = AppRecord {
            installed_app_id: app.clone(),
            publisher: "acme".into(),
            slug: "swap_test".into(),
            capsule_handle: "acme/swap_test".into(),
            version: "1.0.0".into(),
            installed_at: "2025-01-01T00:00:00Z".into(),
            updated_at: "2025-01-01T00:00:00Z".into(),
        };
        store.write_app_record(&record).unwrap();
        store
            .write_profile(
                &app,
                &LaunchProfile {
                    profile_id: profile_id.clone(),
                    ..Default::default()
                },
            )
            .unwrap();

        let rev1 = InstallRevisionId::new("rev_001");
        let rev2 = InstallRevisionId::new("rev_002");
        store.scaffold_revision(&rev1).unwrap();
        store.scaffold_revision(&rev2).unwrap();

        store
            .set_current_revision(&app, &profile_id, &rev1)
            .unwrap();
        assert_eq!(store.current_revision(&app, &profile_id).unwrap(), rev1);

        store
            .set_current_revision(&app, &profile_id, &rev2)
            .unwrap();
        assert_eq!(store.current_revision(&app, &profile_id).unwrap(), rev2);
    }

    // ── rollback: current_revision reverts to previous revision ──────────────

    #[cfg(unix)]
    #[test]
    fn rollback_reverts_current_revision_to_old_rev() {
        let (_dir, store) = temp_store();
        let app = InstalledAppId::new("app_rollback");
        let profile_id = ProfileId::new("default");

        let record = AppRecord {
            installed_app_id: app.clone(),
            publisher: "acme".into(),
            slug: "rollback_test".into(),
            capsule_handle: "acme/rollback_test".into(),
            version: "1.0.0".into(),
            installed_at: "2025-01-01T00:00:00Z".into(),
            updated_at: "2025-01-01T00:00:00Z".into(),
        };
        store.write_app_record(&record).unwrap();
        store
            .write_profile(
                &app,
                &LaunchProfile {
                    profile_id: profile_id.clone(),
                    ..Default::default()
                },
            )
            .unwrap();

        let rev1 = InstallRevisionId::new("rev_old");
        let rev2 = InstallRevisionId::new("rev_new");
        store.scaffold_revision(&rev1).unwrap();
        store.scaffold_revision(&rev2).unwrap();

        // Install rev1, then update to rev2.
        store
            .set_current_revision(&app, &profile_id, &rev1)
            .unwrap();
        store
            .set_current_revision(&app, &profile_id, &rev2)
            .unwrap();
        assert_eq!(
            store.current_revision(&app, &profile_id).unwrap(),
            rev2,
            "current should be rev2 after update"
        );

        // Rollback to rev1 by swapping current_revision back.
        store
            .set_current_revision(&app, &profile_id, &rev1)
            .unwrap();
        assert_eq!(
            store.current_revision(&app, &profile_id).unwrap(),
            rev1,
            "after rollback, current_revision must be rev_old, not rev_new"
        );
    }

    #[test]
    fn list_installed_apps() {
        let (_dir, store) = temp_store();
        for id in &["app_a", "app_b", "app_c"] {
            let record = AppRecord {
                installed_app_id: InstalledAppId::new(*id),
                publisher: "p".into(),
                slug: id.to_string(),
                capsule_handle: format!("p/{id}"),
                version: "1.0.0".into(),
                installed_at: "2025-01-01T00:00:00Z".into(),
                updated_at: "2025-01-01T00:00:00Z".into(),
            };
            store.write_app_record(&record).unwrap();
        }
        let mut apps = store.list_installed_apps().unwrap();
        apps.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        let ids: Vec<&str> = apps.iter().map(|a| a.as_str()).collect();
        assert_eq!(ids, vec!["app_a", "app_b", "app_c"]);
    }
}
