//! `InstallRevisionFinalizer` — promotes a completed producer output into an
//! immutable revision and atomically advances `current_revision` for a profile.
//!
//! # Role
//!
//! The artifact build producer is a pure worker: it knows nothing about
//! `installed_app_id`, `profile_id`, or `install_revision_id`.  It is the
//! finalizer's job to:
//!
//! 1. Accept the producer response (or local build output / OCI lock / source provenance).
//! 2. Mint a new [`InstallRevisionId`].
//! 3. Scaffold the immutable revision root (`revisions/<rev_id>/…`) and copy the output.
//! 4. Write the `artifact_manifest.json` for the revision.
//! 5. Atomically swap `profiles/<profile_id>/current_revision` to the new revision.
//! 6. Return all new typed IDs to the caller for session / receipt attachment.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::ids::{
    derive_capsule_instance_key, derive_install_profile_key, ArtifactBuildId, CapsuleInstanceKey,
    InstallProfileKey, InstallRevisionId, InstalledAppId, ProfileId,
};
use super::store::InstallInstanceStore;

// ── Input ──────────────────────────────────────────────────────────────────

/// Everything the finalizer needs to promote a build output into a revision.
#[derive(Debug)]
pub struct FinalizerInput {
    pub installed_app_id: InstalledAppId,
    pub profile_id: ProfileId,
    /// The build id from the producer (must have `build_` prefix).
    pub artifact_build_id: ArtifactBuildId,
    /// Directory containing the materialized build output to freeze.
    pub output_dir: PathBuf,
    /// Optional structured artifact manifest (JSON string).
    pub artifact_manifest_json: Option<String>,
    /// Optional source provenance (JSON string).
    pub source_provenance_json: Option<String>,
    /// Optional OCI lock content (JSON string).
    pub oci_lock_json: Option<String>,
}

// ── Output ─────────────────────────────────────────────────────────────────

/// All typed IDs produced by the finalizer.  Attach these to session records
/// and execution receipts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinalizerOutput {
    pub installed_app_id: InstalledAppId,
    pub profile_id: ProfileId,
    pub install_profile_key: InstallProfileKey,
    pub install_revision_id: InstallRevisionId,
    pub capsule_instance_key: CapsuleInstanceKey,
    /// The artifact_build_id from the input, preserved for traceability.
    pub artifact_build_id: ArtifactBuildId,
    /// Absolute path to the frozen revision directory.
    pub revision_dir: PathBuf,
}

// ── ArtifactRevisionManifest ───────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct ArtifactRevisionManifest {
    pub install_revision_id: InstallRevisionId,
    pub artifact_build_id: ArtifactBuildId,
    pub finalized_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_manifest: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_provenance: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oci_lock: Option<serde_json::Value>,
}

// ── Finalizer ─────────────────────────────────────────────────────────────

/// Promotes a producer output into an immutable revision and swaps
/// `current_revision` atomically.
pub struct InstallRevisionFinalizer<'s> {
    store: &'s InstallInstanceStore,
}

impl<'s> InstallRevisionFinalizer<'s> {
    pub fn new(store: &'s InstallInstanceStore) -> Self {
        Self { store }
    }

    /// Run the finalization pipeline.
    pub fn finalize(&self, input: FinalizerInput) -> Result<FinalizerOutput> {
        // 1. Validate build id.
        anyhow::ensure!(
            input.artifact_build_id.is_valid(),
            "artifact_build_id must start with 'build_', got: {}",
            input.artifact_build_id
        );

        // 2. Derive stable IDs.
        let install_profile_key =
            derive_install_profile_key(&input.installed_app_id, &input.profile_id);
        let install_revision_id = mint_revision_id(&input.artifact_build_id);
        let capsule_instance_key =
            derive_capsule_instance_key(&install_profile_key, &install_revision_id);

        // 3. Scaffold the immutable revision root.
        self.store.scaffold_revision(&install_revision_id)?;

        // 4. Copy (or hard-link) build output into the frozen revision output dir.
        let rev_output = self.store.revision_output_dir(&install_revision_id);
        copy_dir_all(&input.output_dir, &rev_output).with_context(|| {
            format!(
                "copy build output {} → {}",
                input.output_dir.display(),
                rev_output.display()
            )
        })?;

        // 5. Write artifact_manifest.json.
        let finalized_at = iso8601_now();
        let rev_manifest = ArtifactRevisionManifest {
            install_revision_id: install_revision_id.clone(),
            artifact_build_id: input.artifact_build_id.clone(),
            finalized_at,
            artifact_manifest: input
                .artifact_manifest_json
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?,
            source_provenance: input
                .source_provenance_json
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?,
            oci_lock: input
                .oci_lock_json
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?,
        };
        let manifest_path = self
            .store
            .revision_artifact_manifest_path(&install_revision_id);
        let json = serde_json::to_string_pretty(&rev_manifest)?;
        fs::write(&manifest_path, json.as_bytes())
            .with_context(|| format!("write artifact manifest {}", manifest_path.display()))?;

        // 6. Write source provenance file if provided separately.
        if let Some(prov) = &input.source_provenance_json {
            let prov_path = self
                .store
                .revision_source_provenance_dir(&install_revision_id)
                .join("provenance.json");
            fs::write(&prov_path, prov.as_bytes())?;
        }

        // 7. Write OCI lock file if provided.
        if let Some(lock) = &input.oci_lock_json {
            let lock_path = self
                .store
                .revision_lock_dir(&install_revision_id)
                .join("oci.json");
            fs::write(&lock_path, lock.as_bytes())?;
        }

        // 8. Atomically swap current_revision.
        self.store.set_current_revision(
            &input.installed_app_id,
            &input.profile_id,
            &install_revision_id,
        )?;

        let revision_dir = self.store.revision_dir(&install_revision_id);
        Ok(FinalizerOutput {
            installed_app_id: input.installed_app_id,
            profile_id: input.profile_id,
            install_profile_key,
            install_revision_id,
            capsule_instance_key,
            artifact_build_id: input.artifact_build_id,
            revision_dir,
        })
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Generate a new [`InstallRevisionId`] from a build id + wall-clock epoch ms.
///
/// Format: `rev_<build_id_stem>_<epoch_ms>`
fn mint_revision_id(build_id: &ArtifactBuildId) -> InstallRevisionId {
    let stem = build_id
        .as_str()
        .strip_prefix("build_")
        .unwrap_or(build_id.as_str());
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    InstallRevisionId::new(format!("rev_{stem}_{ts}"))
}

fn iso8601_now() -> String {
    // Use chrono if available; fall back to a placeholder.
    chrono::Utc::now().to_rfc3339()
}

/// Recursively copy `src` into `dst`.
fn copy_dir_all(src: &std::path::Path, dst: &std::path::Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::foundation::install_lifecycle::store::{
        AppRecord, InstallInstanceStore, LaunchProfile,
    };
    use std::io::Write;
    use tempfile::TempDir;

    fn setup() -> (TempDir, InstallInstanceStore, InstalledAppId, ProfileId) {
        let dir = tempfile::tempdir().unwrap();
        let store = InstallInstanceStore::new(dir.path()).unwrap();
        let app = InstalledAppId::new("app_finalize_test");
        let profile_id = ProfileId::new("default");

        // Bootstrap the instance.
        store
            .write_app_record(&AppRecord {
                installed_app_id: app.clone(),
                publisher: "test".into(),
                slug: "finalizer".into(),
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

        (dir, store, app, profile_id)
    }

    fn make_output_dir(base: &std::path::Path) -> PathBuf {
        let out = base.join("output");
        fs::create_dir_all(&out).unwrap();
        let mut f = fs::File::create(out.join("index.js")).unwrap();
        f.write_all(b"console.log('hello')").unwrap();
        out
    }

    #[test]
    fn finalizer_produces_all_ids() {
        let (dir, store, app, profile_id) = setup();
        let output_dir = make_output_dir(dir.path());

        let finalizer = InstallRevisionFinalizer::new(&store);
        let result = finalizer
            .finalize(FinalizerInput {
                installed_app_id: app.clone(),
                profile_id: profile_id.clone(),
                artifact_build_id: ArtifactBuildId::new("build_abc123"),
                output_dir,
                artifact_manifest_json: None,
                source_provenance_json: None,
                oci_lock_json: None,
            })
            .unwrap();

        assert_eq!(result.installed_app_id, app);
        assert_eq!(result.profile_id, profile_id);
        assert!(result
            .install_revision_id
            .as_str()
            .starts_with("rev_abc123_"));
        assert!(!result.install_profile_key.as_str().is_empty());
        assert!(!result.capsule_instance_key.as_str().is_empty());
    }

    #[test]
    fn finalizer_swaps_current_revision() {
        let (dir, store, app, profile_id) = setup();
        let out1 = make_output_dir(&dir.path().join("build1"));
        let out2 = make_output_dir(&dir.path().join("build2"));
        let finalizer = InstallRevisionFinalizer::new(&store);

        let r1 = finalizer
            .finalize(FinalizerInput {
                installed_app_id: app.clone(),
                profile_id: profile_id.clone(),
                artifact_build_id: ArtifactBuildId::new("build_v1"),
                output_dir: out1,
                artifact_manifest_json: None,
                source_provenance_json: None,
                oci_lock_json: None,
            })
            .unwrap();

        // Small sleep to ensure different epoch_ms.
        std::thread::sleep(std::time::Duration::from_millis(2));

        let r2 = finalizer
            .finalize(FinalizerInput {
                installed_app_id: app.clone(),
                profile_id: profile_id.clone(),
                artifact_build_id: ArtifactBuildId::new("build_v2"),
                output_dir: make_output_dir(&dir.path().join("build3")),
                artifact_manifest_json: None,
                source_provenance_json: None,
                oci_lock_json: None,
            })
            .unwrap();

        // profile key must be stable across revisions
        assert_eq!(r1.install_profile_key, r2.install_profile_key);

        // instance key must differ because revision differs
        assert_ne!(r1.capsule_instance_key, r2.capsule_instance_key);

        // current_revision must point to r2
        #[cfg(unix)]
        {
            let current = store.current_revision(&app, &profile_id).unwrap();
            assert_eq!(current, r2.install_revision_id);
        }
    }

    #[test]
    fn finalizer_rejects_invalid_build_id() {
        let (_dir, store, app, profile_id) = setup();
        let out = _dir.path().join("bad_output");
        fs::create_dir_all(&out).unwrap();

        let finalizer = InstallRevisionFinalizer::new(&store);
        let err = finalizer.finalize(FinalizerInput {
            installed_app_id: app,
            profile_id,
            artifact_build_id: ArtifactBuildId::new("exec_not_a_build"),
            output_dir: out,
            artifact_manifest_json: None,
            source_provenance_json: None,
            oci_lock_json: None,
        });
        assert!(err.is_err(), "should reject exec_-prefixed build id");
    }

    #[test]
    fn finalizer_output_files_are_copied() {
        let (dir, store, app, profile_id) = setup();
        let output_dir = make_output_dir(dir.path());

        let finalizer = InstallRevisionFinalizer::new(&store);
        let result = finalizer
            .finalize(FinalizerInput {
                installed_app_id: app,
                profile_id,
                artifact_build_id: ArtifactBuildId::new("build_copy_test"),
                output_dir,
                artifact_manifest_json: Some(r#"{"name":"test"}"#.into()),
                source_provenance_json: Some(r#"{"git_ref":"main"}"#.into()),
                oci_lock_json: None,
            })
            .unwrap();

        assert!(result.revision_dir.join("output").join("index.js").exists());
        assert!(result.revision_dir.join("artifact_manifest.json").exists());
        assert!(result
            .revision_dir
            .join("source_provenance")
            .join("provenance.json")
            .exists());
    }
}
