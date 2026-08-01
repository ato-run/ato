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

use super::ids::{
    InstallProfileKey, InstallRevisionId, InstalledAppId, ProfileId, derive_install_profile_key,
};
use super::launch_template::{BindingAssignmentSet, CompatibilityIndex};
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
                && entry.path().join("app.json").is_file()
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
    // per `(installed_app_id, profile_id, install_revision_id)` under the
    // profile directory:
    //
    //   instances/<app>/profiles/<profile>/revisions/<rev>/
    //     revision.json            -> InstallRevision (self-contained authority)
    //     artifact-build.json      -> ArtifactBuild
    //     requirement-graph.json   -> RequirementGraphSnapshot
    //     state-contracts.json     -> Vec<StateContractSnapshot>
    //     install-receipt.json     -> InstallReceipt
    //
    // Why under the profile dir and NOT the shared top-level `revisions/<rev>/`:
    // `install_revision_id` is content-addressed from the artifact build id
    // alone, so the shared `revisions/<rev>/` dir (which holds the frozen
    // `output/` + `artifact_manifest.json`) is the same for every app/profile
    // that installs the same artifact content. These records, however, embed
    // app/profile-scoped identity (`install_profile_key`, `InstallReceipt`), so
    // storing them in the shared dir would let two `(app, profile)` pairs
    // installing the same content clobber each other's records. Scoping the
    // record dir by `(app, profile)` keeps the shared dir purely
    // content-addressed and the records faithful to their `(app, profile)`.
    //
    // `revision.json` is written *last* by the finalizer and is the per-revision
    // finalization marker: a revision whose sub-records were only partially
    // written (no `revision.json`) is not considered finalized — see
    // [`is_revision_finalized`] / [`read_install_revision`].
    //
    // These records carry only install-time identity/metadata. They never hold
    // secret values, nor any session/runtime/observed fact (session id, dynamic
    // port, pid, container id, live route, log cursor, observed status).

    /// Directory holding the install-output records for one
    /// `(app, profile, revision)`: `instances/<app>/profiles/<profile>/revisions/<rev>/`.
    pub fn profile_revision_dir(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
        rev: &InstallRevisionId,
    ) -> PathBuf {
        self.profile_dir(app, profile)
            .join("revisions")
            .join(rev.as_str())
    }

    pub fn revision_install_record_path(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
        rev: &InstallRevisionId,
    ) -> PathBuf {
        self.profile_revision_dir(app, profile, rev)
            .join("revision.json")
    }

    pub fn revision_artifact_build_path(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
        rev: &InstallRevisionId,
    ) -> PathBuf {
        self.profile_revision_dir(app, profile, rev)
            .join("artifact-build.json")
    }

    pub fn revision_requirement_graph_path(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
        rev: &InstallRevisionId,
    ) -> PathBuf {
        self.profile_revision_dir(app, profile, rev)
            .join("requirement-graph.json")
    }

    pub fn revision_state_contracts_path(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
        rev: &InstallRevisionId,
    ) -> PathBuf {
        self.profile_revision_dir(app, profile, rev)
            .join("state-contracts.json")
    }

    pub fn revision_install_receipt_path(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
        rev: &InstallRevisionId,
    ) -> PathBuf {
        self.profile_revision_dir(app, profile, rev)
            .join("install-receipt.json")
    }

    pub fn revision_binding_assignment_set_path(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
        rev: &InstallRevisionId,
    ) -> PathBuf {
        self.profile_revision_dir(app, profile, rev)
            .join("binding-assignments.json")
    }

    pub fn revision_compatibility_index_path(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
        rev: &InstallRevisionId,
    ) -> PathBuf {
        self.profile_revision_dir(app, profile, rev)
            .join("compatibility-index.json")
    }

    /// Atomically write any revision-scoped JSON record, creating the record
    /// directory if needed.
    fn write_revision_json<T: Serialize>(&self, path: &Path, value: &T) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create revision record dir {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(value)
            .with_context(|| format!("serialize record {}", path.display()))?;
        atomic_write(path, json.as_bytes())
    }

    /// Persist the [`ArtifactBuild`] record for a revision (atomic).
    pub fn write_artifact_build(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
        rev: &InstallRevisionId,
        build: &ArtifactBuild,
    ) -> Result<()> {
        self.write_revision_json(&self.revision_artifact_build_path(app, profile, rev), build)
    }

    /// Persist the [`RequirementGraphSnapshot`] for a revision (atomic).
    pub fn write_requirement_graph_snapshot(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
        rev: &InstallRevisionId,
        snapshot: &RequirementGraphSnapshot,
    ) -> Result<()> {
        self.write_revision_json(
            &self.revision_requirement_graph_path(app, profile, rev),
            snapshot,
        )
    }

    /// Persist the state-contract snapshots for a revision (atomic). An empty
    /// slice is persisted explicitly as `[]`.
    pub fn write_state_contracts(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
        rev: &InstallRevisionId,
        contracts: &[StateContractSnapshot],
    ) -> Result<()> {
        self.write_revision_json(
            &self.revision_state_contracts_path(app, profile, rev),
            &contracts,
        )
    }

    /// Persist the [`BindingAssignmentSet`] for a revision (atomic).
    pub fn write_binding_assignment_set(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
        rev: &InstallRevisionId,
        bindings: &BindingAssignmentSet,
    ) -> Result<()> {
        self.write_revision_json(
            &self.revision_binding_assignment_set_path(app, profile, rev),
            bindings,
        )
    }

    /// Persist the [`CompatibilityIndex`] for a revision (atomic).
    pub fn write_compatibility_index(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
        rev: &InstallRevisionId,
        index: &CompatibilityIndex,
    ) -> Result<()> {
        self.write_revision_json(
            &self.revision_compatibility_index_path(app, profile, rev),
            index,
        )
    }

    /// Persist the [`InstallReceipt`] for a revision (atomic).
    pub fn write_install_receipt(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
        rev: &InstallRevisionId,
        receipt: &InstallReceipt,
    ) -> Result<()> {
        self.write_revision_json(
            &self.revision_install_receipt_path(app, profile, rev),
            receipt,
        )
    }

    // ── Launch templates (#581 wave 4C) ──────────────────────────────────────
    //
    // Reusable, session-independent launch templates are persisted as standalone
    // per-template files keyed by the template's `LaunchTemplateKey::key_hash`,
    // under the per-`(app, profile, revision)` record dir:
    //
    //   instances/<app>/profiles/<profile>/revisions/<rev>/
    //     launch-templates/
    //       <sanitized key_hash>.json   -> LaunchTemplate
    //
    // This mirrors the standalone-file pattern the other revision records use and
    // keeps the `revision.json` finalization marker untouched (a launch template
    // is a later, additive output — it is not part of finalization). The file is
    // keyed by the stable `key_hash`, so rebuilding a template from the same
    // stable inputs overwrites in place rather than duplicating. Templates carry
    // only install-time identity; never a session/runtime/observed/secret value.

    /// Directory holding the standalone launch-template records for one
    /// `(app, profile, revision)`.
    pub fn revision_launch_templates_dir(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
        rev: &InstallRevisionId,
    ) -> PathBuf {
        self.profile_revision_dir(app, profile, rev)
            .join("launch-templates")
    }

    /// Path of the standalone launch-template file for `key_hash`.
    ///
    /// `key_hash` is a `blake3:<hex>` digest; the `:` is replaced with `-` so the
    /// filename is portable (Windows forbids `:` in filenames). The mapping is
    /// total and collision-free over `blake3:` hashes.
    pub fn revision_launch_template_path(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
        rev: &InstallRevisionId,
        key_hash: &str,
    ) -> PathBuf {
        self.revision_launch_templates_dir(app, profile, rev)
            .join(format!("{}.json", launch_template_filename(key_hash)))
    }

    /// Persist a [`LaunchTemplate`] (atomic), keyed by its
    /// [`LaunchTemplateKey::key_hash`](super::launch_template::LaunchTemplateKey::key_hash).
    pub fn write_launch_template(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
        rev: &InstallRevisionId,
        template: &super::launch_template::LaunchTemplate,
    ) -> Result<()> {
        let key_hash = template.key.key_hash()?;
        self.write_revision_json(
            &self.revision_launch_template_path(app, profile, rev, &key_hash),
            template,
        )
    }

    /// Read a single [`LaunchTemplate`] by its `key_hash`.
    pub fn read_launch_template(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
        rev: &InstallRevisionId,
        key_hash: &str,
    ) -> Result<super::launch_template::LaunchTemplate> {
        let path = self.revision_launch_template_path(app, profile, rev, key_hash);
        let raw = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_slice(&raw).with_context(|| format!("parse {}", path.display()))
    }

    /// Read every persisted [`LaunchTemplate`] for a `(app, profile, revision)`.
    ///
    /// Returns an empty vec if the `launch-templates/` dir does not exist (the
    /// standard install path never writes one). Results are sorted by `key_hash`
    /// filename for a deterministic order.
    pub fn read_launch_templates(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
        rev: &InstallRevisionId,
    ) -> Result<Vec<super::launch_template::LaunchTemplate>> {
        let dir = self.revision_launch_templates_dir(app, profile, rev);
        if !dir.exists() {
            return Ok(vec![]);
        }
        let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
            .with_context(|| format!("read launch-templates dir {}", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
            .collect();
        entries.sort();
        let mut templates = Vec::with_capacity(entries.len());
        for path in entries {
            let raw = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
            let template: super::launch_template::LaunchTemplate = serde_json::from_slice(&raw)
                .with_context(|| format!("parse {}", path.display()))?;
            templates.push(template);
        }
        Ok(templates)
    }

    /// Persist the [`InstallRevision`] marker (atomic). Must be written only
    /// after every sub-record above has been written successfully.
    pub fn write_install_revision(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
        revision: &InstallRevision,
    ) -> Result<()> {
        self.write_revision_json(
            &self.revision_install_record_path(app, profile, &revision.install_revision_id),
            revision,
        )
    }

    // ── Per-session launch materialization records (#581 wave 5B) ─────────────
    //
    // A `LaunchMaterializationRecord` is session-scoped (frozen per session,
    // never reused as a launch template). It is stored under the existing
    // app-scoped sessions dir, indexed by `capsule_instance_key` (the session's
    // canonical replay key — see the directory-structure doc at the top of this
    // file: "sessions/  # session records (indexed by capsule_instance_key)"):
    //
    //   instances/<app>/sessions/<capsule_instance_key>/materialization.json
    //
    // The record carries only stable identity + projection *digests* — never a
    // secret value, dynamic port, pid, container id, live route, log cursor, or
    // observed/readiness status.

    /// Directory holding one session's records, keyed by `capsule_instance_key`.
    pub fn session_dir(
        &self,
        app: &InstalledAppId,
        capsule_instance_key: &super::ids::CapsuleInstanceKey,
    ) -> PathBuf {
        self.sessions_dir(app).join(capsule_instance_key.as_str())
    }

    /// Path of a session's `materialization.json`.
    pub fn session_materialization_path(
        &self,
        app: &InstalledAppId,
        capsule_instance_key: &super::ids::CapsuleInstanceKey,
    ) -> PathBuf {
        self.session_dir(app, capsule_instance_key)
            .join("materialization.json")
    }

    /// Persist a [`LaunchMaterializationRecord`](super::materialization::LaunchMaterializationRecord)
    /// for its session (atomic), keyed by the record's `capsule_instance_key`.
    pub fn write_launch_materialization_record(
        &self,
        app: &InstalledAppId,
        record: &super::materialization::LaunchMaterializationRecord,
    ) -> Result<()> {
        self.write_revision_json(
            &self.session_materialization_path(app, &record.capsule_instance_key),
            record,
        )
    }

    /// Read a session's [`LaunchMaterializationRecord`](super::materialization::LaunchMaterializationRecord)
    /// by its `capsule_instance_key`.
    pub fn read_launch_materialization_record(
        &self,
        app: &InstalledAppId,
        capsule_instance_key: &super::ids::CapsuleInstanceKey,
    ) -> Result<super::materialization::LaunchMaterializationRecord> {
        let path = self.session_materialization_path(app, capsule_instance_key);
        let raw = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_slice(&raw).with_context(|| format!("parse {}", path.display()))
    }

    /// Remove the `revision.json` finalization marker if present (best effort).
    ///
    /// Called before re-finalizing an existing revision so that the revision is
    /// transiently *un*-finalized while its sub-records are rewritten: a crash
    /// mid-rewrite then leaves no marker (read fails closed) rather than a marker
    /// pointing at half-rewritten sub-records.
    pub fn remove_install_revision_marker(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
        rev: &InstallRevisionId,
    ) -> Result<()> {
        let path = self.revision_install_record_path(app, profile, rev);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("remove marker {}", path.display())),
        }
    }

    /// True if `revision.json` exists for this `(app, profile, revision)` —
    /// i.e. finalization reached the marker write. A scaffolded-but-incomplete
    /// revision (output copied, sub-records partially written) returns `false`.
    pub fn is_revision_finalized(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
        rev: &InstallRevisionId,
    ) -> bool {
        self.revision_install_record_path(app, profile, rev)
            .exists()
    }

    /// Read the [`InstallRevision`] for a `(app, profile, revision)`. Errors if
    /// `revision.json` is missing (revision not finalized) or malformed.
    pub fn read_install_revision(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
        rev: &InstallRevisionId,
    ) -> Result<InstallRevision> {
        let path = self.revision_install_record_path(app, profile, rev);
        if !path.exists() {
            anyhow::bail!(
                "revision '{}' for {}/{} is not finalized: revision.json is missing",
                rev.as_str(),
                app.as_str(),
                profile.as_str()
            );
        }
        let raw = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_slice(&raw).with_context(|| format!("parse {}", path.display()))
    }

    /// Read the [`ArtifactBuild`] record for a `(app, profile, revision)`.
    pub fn read_artifact_build(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
        rev: &InstallRevisionId,
    ) -> Result<ArtifactBuild> {
        let path = self.revision_artifact_build_path(app, profile, rev);
        let raw = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_slice(&raw).with_context(|| format!("parse {}", path.display()))
    }

    /// Read the [`RequirementGraphSnapshot`] for a `(app, profile, revision)`.
    pub fn read_requirement_graph_snapshot(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
        rev: &InstallRevisionId,
    ) -> Result<RequirementGraphSnapshot> {
        let path = self.revision_requirement_graph_path(app, profile, rev);
        let raw = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_slice(&raw).with_context(|| format!("parse {}", path.display()))
    }

    /// Read the state-contract snapshots for a `(app, profile, revision)`.
    pub fn read_state_contracts(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
        rev: &InstallRevisionId,
    ) -> Result<Vec<StateContractSnapshot>> {
        let path = self.revision_state_contracts_path(app, profile, rev);
        let raw = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_slice(&raw).with_context(|| format!("parse {}", path.display()))
    }

    /// Read the [`BindingAssignmentSet`] for a `(app, profile, revision)`.
    pub fn read_binding_assignment_set(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
        rev: &InstallRevisionId,
    ) -> Result<BindingAssignmentSet> {
        let path = self.revision_binding_assignment_set_path(app, profile, rev);
        let raw = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_slice(&raw).with_context(|| format!("parse {}", path.display()))
    }

    /// Read the [`CompatibilityIndex`] for a `(app, profile, revision)`.
    pub fn read_compatibility_index(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
        rev: &InstallRevisionId,
    ) -> Result<CompatibilityIndex> {
        let path = self.revision_compatibility_index_path(app, profile, rev);
        let raw = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_slice(&raw).with_context(|| format!("parse {}", path.display()))
    }

    /// Read the [`InstallReceipt`] for a `(app, profile, revision)`.
    pub fn read_install_receipt(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
        rev: &InstallRevisionId,
    ) -> Result<InstallReceipt> {
        let path = self.revision_install_receipt_path(app, profile, rev);
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
        self.read_install_revision(app, profile, &rev)
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

    /// Remove one installed profile registration while preserving immutable
    /// revisions and app state. Returns `true` when this was the app's final
    /// profile and the app record was archived, making the app disappear from
    /// [`Self::list_installed_apps`].
    pub fn remove_profile_registration(
        &self,
        app: &InstalledAppId,
        profile: &ProfileId,
    ) -> Result<bool> {
        let profile_dir = self.profile_dir(app, profile);
        if !profile_dir.is_dir() {
            anyhow::bail!(
                "installed profile {}/{} does not exist",
                app.as_str(),
                profile.as_str()
            );
        }
        fs::remove_dir_all(&profile_dir)
            .with_context(|| format!("remove profile dir {}", profile_dir.display()))?;

        if !self.list_profiles(app)?.is_empty() {
            return Ok(false);
        }

        let app_record = self.app_record_path(app);
        let archived = self
            .instance_dir(app)
            .join("archived_metadata")
            .join("removed_app.json");
        if let Some(parent) = archived.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&app_record, &archived).with_context(|| {
            format!(
                "archive removed app record {} to {}",
                app_record.display(),
                archived.display()
            )
        })?;
        Ok(true)
    }

    /// Delete the app-owned persistent state directory. This does not touch
    /// external state-binding targets recorded elsewhere.
    pub fn purge_app_state(&self, app: &InstalledAppId) -> Result<()> {
        let state_dir = self.state_dir(app);
        if state_dir.exists() {
            fs::remove_dir_all(&state_dir)
                .with_context(|| format!("purge app state {}", state_dir.display()))?;
        }
        Ok(())
    }

    /// Resolve a capsule handle (a `publisher/slug` scoped id, tolerant of a
    /// `capsule://` prefix and ASCII case) to the **single** installed profile
    /// that owns it. Enumerates installed apps and matches each [`AppRecord`]'s
    /// `capsule_handle` (falling back to `publisher/slug`) under a light
    /// normalization.
    ///
    /// `capsule://` carries **no profile selector**, so resolution must be
    /// unambiguous:
    /// - within one app the **default** launch profile is preferred; with no
    ///   default, a single profile that has a current revision is accepted, but
    ///   two or more non-default profiles with current revisions are ambiguous;
    /// - if **multiple installed apps** match the same handle, that is ambiguous
    ///   even when each has a default profile (they may be distinct installs /
    ///   revisions / providers).
    ///
    /// Returns `Ok(None)` when nothing matches, `Ok(Some(..))` for exactly one
    /// launch target, and `Err(..)` — actionable, listing the candidate `ipk`s —
    /// when ambiguous, so relaunch query inputs are never applied to an
    /// arbitrarily chosen installed app.
    ///
    /// This is a pure identity lookup against the install ledger
    /// (`app.json` + `current_revision`) — it never reads `capsule.toml`, the
    /// artifact manifest, or a lockfile.
    pub fn find_profile_by_capsule_handle(
        &self,
        capsule_handle: &str,
    ) -> Result<
        Option<(
            InstalledAppId,
            ProfileId,
            InstallProfileKey,
            InstallRevisionId,
        )>,
    > {
        let target = normalize_handle_for_match(capsule_handle);
        if target.is_empty() {
            return Ok(None);
        }
        let mut candidates: Vec<CapsuleHandleProfileCandidate> = Vec::new();
        let mut ambiguous_within_app: Vec<CapsuleHandleProfileCandidate> = Vec::new();
        for app_id in self.list_installed_apps()? {
            let Ok(record) = self.read_app_record(&app_id) else {
                continue;
            };
            if !record_handle_matches(&record, &target) {
                continue;
            }
            match self.select_profile_for_app(&app_id)? {
                AppProfileSelection::None => {}
                AppProfileSelection::Single(candidate) => candidates.push(candidate),
                AppProfileSelection::Ambiguous(mut within) => {
                    ambiguous_within_app.append(&mut within)
                }
            }
        }

        // Fail closed on any ambiguity: more than one matching app target, or a
        // single app whose non-default profiles cannot be narrowed to one.
        if !ambiguous_within_app.is_empty() || candidates.len() > 1 {
            let all_ipks: Vec<String> = candidates
                .iter()
                .chain(ambiguous_within_app.iter())
                .map(|c| c.install_profile_key.as_str().to_string())
                .collect();
            let listing = all_ipks
                .iter()
                .map(|ipk| format!("  - {ipk}"))
                .collect::<Vec<_>>()
                .join("\n");
            let first = all_ipks.first().map(String::as_str).unwrap_or("ipk_…");
            anyhow::bail!(
                "ambiguous capsule relaunch target: capsule location '{capsule_handle}' \
                 matches multiple installed profiles:\n{listing}\n\
                 Launch by install profile key instead: ato launch {first}"
            );
        }

        Ok(candidates
            .into_iter()
            .next()
            .map(|c| (c.app_id, c.profile_id, c.install_profile_key, c.revision_id)))
    }

    /// Select the single launch profile for one installed app, applying the
    /// profile-resolution rules of [`find_profile_by_capsule_handle`]. Only
    /// profiles that have a current revision are considered; the default profile
    /// is preferred, otherwise a lone revisioned profile is accepted and two or
    /// more (without a default) are reported ambiguous.
    fn select_profile_for_app(&self, app_id: &InstalledAppId) -> Result<AppProfileSelection> {
        let default_id = ProfileId::default();
        let mut with_rev: Vec<(ProfileId, InstallRevisionId)> = Vec::new();
        for profile_id in self.list_profiles(app_id)? {
            if let Ok(rev_id) = self.current_revision(app_id, &profile_id) {
                with_rev.push((profile_id, rev_id));
            }
        }
        let make =
            |profile_id: &ProfileId, rev_id: &InstallRevisionId| CapsuleHandleProfileCandidate {
                app_id: app_id.clone(),
                profile_id: profile_id.clone(),
                install_profile_key: derive_install_profile_key(app_id, profile_id),
                revision_id: rev_id.clone(),
            };
        if let Some(idx) = with_rev.iter().position(|(p, _)| *p == default_id) {
            let (profile_id, rev_id) = &with_rev[idx];
            return Ok(AppProfileSelection::Single(make(profile_id, rev_id)));
        }
        match with_rev.as_slice() {
            [] => Ok(AppProfileSelection::None),
            [(profile_id, rev_id)] => Ok(AppProfileSelection::Single(make(profile_id, rev_id))),
            many => Ok(AppProfileSelection::Ambiguous(
                many.iter().map(|(p, r)| make(p, r)).collect(),
            )),
        }
    }
}

/// One concrete installed launch target matched by capsule handle (internal to
/// [`InstallInstanceStore::find_profile_by_capsule_handle`]).
struct CapsuleHandleProfileCandidate {
    app_id: InstalledAppId,
    profile_id: ProfileId,
    install_profile_key: InstallProfileKey,
    revision_id: InstallRevisionId,
}

/// Outcome of selecting a single launch profile within one installed app.
enum AppProfileSelection {
    /// No profile has a current revision — the app contributes no launch target.
    None,
    /// Exactly one launch target (default preferred, else the sole rev profile).
    Single(CapsuleHandleProfileCandidate),
    /// Two or more non-default profiles have current revisions and there is no
    /// default — the app cannot be narrowed to a single target.
    Ambiguous(Vec<CapsuleHandleProfileCandidate>),
}

/// Light normalization for capsule-handle matching: trim, drop a `capsule://`
/// prefix and a trailing `/`, then lowercase. Mirrors the comparison used by the
/// Desktop launch-intent resolver so `ato launch` and Desktop agree on which
/// installed app a handle refers to.
fn normalize_handle_for_match(handle: &str) -> String {
    handle
        .trim()
        .trim_start_matches("capsule://")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

/// Whether an [`AppRecord`] refers to the same capsule as `target` (an
/// already-normalized handle). Compares the stored `capsule_handle` and, as a
/// fallback, the record's `publisher/slug`.
fn record_handle_matches(record: &AppRecord, target: &str) -> bool {
    if !record.capsule_handle.is_empty()
        && normalize_handle_for_match(&record.capsule_handle) == target
    {
        return true;
    }
    if !record.publisher.is_empty() {
        let publisher_slug = format!("{}/{}", record.publisher, record.slug);
        if normalize_handle_for_match(&publisher_slug) == target {
            return true;
        }
    }
    false
}

/// Map a `blake3:<hex>` key hash to a portable filename stem (replace `:` with
/// `-`). Total and collision-free over `blake3:` hashes.
fn launch_template_filename(key_hash: &str) -> String {
    key_hash.replace(':', "-")
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

    // ── find_profile_by_capsule_handle: reverse lookup (location → identity) ──

    #[cfg(unix)]
    fn scaffold_installed_app(
        store: &InstallInstanceStore,
        installed_app_id: &str,
        publisher: &str,
        slug: &str,
        capsule_handle: &str,
        rev: &str,
    ) -> (InstalledAppId, ProfileId, InstallRevisionId) {
        let app = InstalledAppId::new(installed_app_id);
        let profile_id = ProfileId::default();
        let rev_id = InstallRevisionId::new(rev);
        store
            .write_app_record(&AppRecord {
                installed_app_id: app.clone(),
                publisher: publisher.into(),
                slug: slug.into(),
                capsule_handle: capsule_handle.into(),
                version: "1.0.0".into(),
                installed_at: "2025-01-01T00:00:00Z".into(),
                updated_at: "2025-01-01T00:00:00Z".into(),
            })
            .unwrap();
        store
            .write_profile(
                &app,
                &LaunchProfile {
                    profile_id: profile_id.clone(),
                    ..Default::default()
                },
            )
            .unwrap();
        store.scaffold_revision(&rev_id).unwrap();
        store
            .set_current_revision(&app, &profile_id, &rev_id)
            .unwrap();
        (app, profile_id, rev_id)
    }

    /// Resolves a stored `capsule_handle` to its profile identity, tolerant of a
    /// `capsule://` prefix and ASCII case.
    #[cfg(unix)]
    #[test]
    fn find_profile_by_capsule_handle_resolves_stored_handle() {
        let (_dir, store) = temp_store();
        let (app, profile_id, rev_id) = scaffold_installed_app(
            &store,
            "app_handle_lookup_0000000000000001",
            "koh0920",
            "hello-capsule",
            "koh0920/hello-capsule",
            "rev_handle1",
        );
        let expected_ipk = derive_install_profile_key(&app, &profile_id);

        for handle in [
            "koh0920/hello-capsule",
            "capsule://koh0920/hello-capsule",
            "KOH0920/Hello-Capsule",
            "koh0920/hello-capsule/",
        ] {
            let (a, p, ipk, r) = store
                .find_profile_by_capsule_handle(handle)
                .unwrap()
                .unwrap_or_else(|| panic!("handle '{handle}' should resolve"));
            assert_eq!(a, app);
            assert_eq!(p, profile_id);
            assert_eq!(ipk, expected_ipk);
            assert_eq!(r, rev_id);
        }
    }

    /// Falls back to `publisher/slug` when `capsule_handle` was not persisted.
    #[cfg(unix)]
    #[test]
    fn find_profile_by_capsule_handle_falls_back_to_publisher_slug() {
        let (_dir, store) = temp_store();
        let (app, _profile, _rev) = scaffold_installed_app(
            &store,
            "app_handle_lookup_0000000000000002",
            "acme",
            "widget",
            "", // capsule_handle intentionally empty
            "rev_handle2",
        );
        let (a, _, _, _) = store
            .find_profile_by_capsule_handle("acme/widget")
            .unwrap()
            .expect("publisher/slug fallback should resolve");
        assert_eq!(a, app);
    }

    /// Unknown / empty handles resolve to `None`, never a wrong app.
    #[cfg(unix)]
    #[test]
    fn find_profile_by_capsule_handle_returns_none_for_unknown() {
        let (_dir, store) = temp_store();
        scaffold_installed_app(
            &store,
            "app_handle_lookup_0000000000000003",
            "acme",
            "hello",
            "acme/hello",
            "rev_handle3",
        );
        assert!(
            store
                .find_profile_by_capsule_handle("acme/not-installed")
                .unwrap()
                .is_none()
        );
        assert!(store.find_profile_by_capsule_handle("").unwrap().is_none());
    }

    /// Write an app record (and its instance dir) with no launch profile yet, so
    /// a test can attach specific profiles itself.
    #[cfg(unix)]
    fn write_app_record_only(
        store: &InstallInstanceStore,
        installed_app_id: &str,
        capsule_handle: &str,
    ) -> InstalledAppId {
        let app = InstalledAppId::new(installed_app_id);
        store
            .write_app_record(&AppRecord {
                installed_app_id: app.clone(),
                publisher: "acme".into(),
                slug: "multi".into(),
                capsule_handle: capsule_handle.into(),
                version: "1.0.0".into(),
                installed_at: "2025-01-01T00:00:00Z".into(),
                updated_at: "2025-01-01T00:00:00Z".into(),
            })
            .unwrap();
        app
    }

    /// Attach a profile with a current revision to an existing installed app.
    #[cfg(unix)]
    fn add_profile_with_rev(
        store: &InstallInstanceStore,
        app: &InstalledAppId,
        profile: &str,
        rev: &str,
    ) -> (ProfileId, InstallRevisionId) {
        let profile_id = ProfileId::new(profile);
        let rev_id = InstallRevisionId::new(rev);
        store
            .write_profile(
                app,
                &LaunchProfile {
                    profile_id: profile_id.clone(),
                    ..Default::default()
                },
            )
            .unwrap();
        store.scaffold_revision(&rev_id).unwrap();
        store
            .set_current_revision(app, &profile_id, &rev_id)
            .unwrap();
        (profile_id, rev_id)
    }

    /// CORE BLOCKER GUARD: two distinct installed apps sharing one capsule handle
    /// (each with a default profile) must fail closed, never silently pick one.
    #[cfg(unix)]
    #[test]
    fn find_profile_by_capsule_handle_errors_on_duplicate_installed_apps() {
        let (_dir, store) = temp_store();
        scaffold_installed_app(
            &store,
            "app_dup_a_000000000000000000001",
            "acme",
            "widget",
            "acme/widget",
            "rev_dup_a",
        );
        scaffold_installed_app(
            &store,
            "app_dup_b_000000000000000000002",
            "acme",
            "widget",
            "acme/widget",
            "rev_dup_b",
        );
        let err = store
            .find_profile_by_capsule_handle("acme/widget")
            .expect_err("duplicate installed apps must be ambiguous");
        let msg = format!("{err:#}");
        assert!(msg.contains("ambiguous"), "msg: {msg}");
        assert!(msg.contains("ato launch ipk_"), "msg: {msg}");
    }

    /// Within one app, the default profile is chosen even when other profiles
    /// also have current revisions.
    #[cfg(unix)]
    #[test]
    fn find_profile_by_capsule_handle_prefers_default_profile_within_app() {
        let (_dir, store) = temp_store();
        let (app, default_profile, _rev) = scaffold_installed_app(
            &store,
            "app_prefer_default_00000000000001",
            "acme",
            "widget",
            "acme/widget",
            "rev_default",
        );
        add_profile_with_rev(&store, &app, "beta", "rev_beta");

        let (a, p, ipk, _r) = store
            .find_profile_by_capsule_handle("acme/widget")
            .unwrap()
            .expect("default profile should resolve");
        assert_eq!(a, app);
        assert_eq!(p, default_profile, "default profile must be preferred");
        assert_eq!(ipk, derive_install_profile_key(&app, &default_profile));
    }

    /// No default + two non-default profiles with current revisions is ambiguous.
    #[cfg(unix)]
    #[test]
    fn find_profile_by_capsule_handle_errors_on_multiple_non_default_profiles_without_default() {
        let (_dir, store) = temp_store();
        let app = write_app_record_only(&store, "app_multi_nondefault_000000000001", "acme/widget");
        add_profile_with_rev(&store, &app, "alpha", "rev_alpha");
        add_profile_with_rev(&store, &app, "beta", "rev_beta");

        let err = store
            .find_profile_by_capsule_handle("acme/widget")
            .expect_err("multiple non-default profiles without a default must be ambiguous");
        assert!(format!("{err:#}").contains("ambiguous"));
    }

    /// No default but exactly one non-default profile with a current revision is
    /// allowed (it is the only launch target).
    #[cfg(unix)]
    #[test]
    fn find_profile_by_capsule_handle_single_non_default_profile_is_allowed() {
        let (_dir, store) = temp_store();
        let app = write_app_record_only(&store, "app_single_nondefault_00000000001", "acme/widget");
        let (solo, solo_rev) = add_profile_with_rev(&store, &app, "solo", "rev_solo");

        let (a, p, ipk, r) = store
            .find_profile_by_capsule_handle("acme/widget")
            .unwrap()
            .expect("a single non-default profile should resolve");
        assert_eq!(a, app);
        assert_eq!(p, solo);
        assert_eq!(ipk, derive_install_profile_key(&app, &solo));
        assert_eq!(r, solo_rev);
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

    #[test]
    fn removing_final_profile_hides_app_but_preserves_state() {
        let (_dir, store) = temp_store();
        let app = InstalledAppId::new("app_remove");
        let profile = ProfileId::default();
        store
            .write_app_record(&AppRecord {
                installed_app_id: app.clone(),
                publisher: "p".into(),
                slug: "remove".into(),
                capsule_handle: "p/remove".into(),
                version: "1.0.0".into(),
                installed_at: "2025-01-01T00:00:00Z".into(),
                updated_at: "2025-01-01T00:00:00Z".into(),
            })
            .unwrap();
        store
            .write_profile(
                &app,
                &LaunchProfile {
                    profile_id: profile.clone(),
                    ..Default::default()
                },
            )
            .unwrap();
        fs::write(store.state_dir(&app).join("data"), b"keep").unwrap();

        assert!(store.remove_profile_registration(&app, &profile).unwrap());
        assert!(store.list_installed_apps().unwrap().is_empty());
        assert_eq!(
            fs::read(store.state_dir(&app).join("data")).unwrap(),
            b"keep"
        );
        assert!(
            store
                .instance_dir(&app)
                .join("archived_metadata/removed_app.json")
                .is_file()
        );
    }

    #[test]
    fn removing_one_of_multiple_profiles_keeps_app_registered() {
        let (_dir, store) = temp_store();
        let app = InstalledAppId::new("app_multi_remove");
        store
            .write_app_record(&AppRecord {
                installed_app_id: app.clone(),
                publisher: "p".into(),
                slug: "multi".into(),
                capsule_handle: "p/multi".into(),
                version: "1.0.0".into(),
                installed_at: "2025-01-01T00:00:00Z".into(),
                updated_at: "2025-01-01T00:00:00Z".into(),
            })
            .unwrap();
        for profile in [ProfileId::default(), ProfileId::new("staging")] {
            store
                .write_profile(
                    &app,
                    &LaunchProfile {
                        profile_id: profile,
                        ..Default::default()
                    },
                )
                .unwrap();
        }

        assert!(
            !store
                .remove_profile_registration(&app, &ProfileId::new("staging"))
                .unwrap()
        );
        assert_eq!(store.list_installed_apps().unwrap(), vec![app]);
    }
}
