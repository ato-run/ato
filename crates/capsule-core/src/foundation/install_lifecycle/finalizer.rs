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

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::ids::{
    ArtifactBuildId, InstallProfileKey, InstallRevisionId, InstalledAppId, ProfileId,
    derive_install_profile_key, revision_id_for_build,
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

/// All typed IDs produced by the finalizer.
///
/// **Note:** [`CapsuleInstanceKey`] is intentionally absent here.
/// The finalizer does not have an [`ExecutionId`], which is required to derive the CIK.
/// Callers should mint the `CapsuleInstanceKey` at launch time using
/// [`derive_capsule_instance_key(profile_key, revision_id, execution_id)`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinalizerOutput {
    pub installed_app_id: InstalledAppId,
    pub profile_id: ProfileId,
    pub install_profile_key: InstallProfileKey,
    pub install_revision_id: InstallRevisionId,
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
        if let Err(e) = input.artifact_build_id.validate() {
            anyhow::bail!("invalid artifact_build_id: {e}");
        }

        // 2. Derive stable IDs.
        let install_profile_key =
            derive_install_profile_key(&input.installed_app_id, &input.profile_id);
        let install_revision_id = revision_id_for_build(&input.artifact_build_id);

        // 3. Scaffold the immutable revision root.
        self.store.scaffold_revision(&install_revision_id)?;

        // 4. Copy build output into the frozen revision output dir (safe copy).
        let rev_output = self.store.revision_output_dir(&install_revision_id);
        safe_copy_output_tree(&input.output_dir, &rev_output).with_context(|| {
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
            artifact_build_id: input.artifact_build_id,
            revision_dir,
        })
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn iso8601_now() -> String {
    // Use chrono if available; fall back to a placeholder.
    chrono::Utc::now().to_rfc3339()
}

/// Safely copy the build output tree into a frozen revision directory.
///
/// Safety rules:
/// - Symlinks are rejected (could point outside the revision root).
/// - Special files (block devices, char devices, FIFOs, sockets) are rejected.
/// - On Unix, hard-linked files (`nlink > 1`) are rejected to avoid aliasing.
/// - Path traversal components (`..`) are rejected.
/// - If destination already exists it is overwritten (idempotent re-finalization).
fn safe_copy_output_tree(src: &std::path::Path, dst: &std::path::Path) -> Result<()> {
    // Reject path traversal in source root itself.
    for comp in src.components() {
        if comp == std::path::Component::ParentDir {
            anyhow::bail!("path traversal in source path: {}", src.display());
        }
    }
    safe_copy_output_tree_inner(src, dst, src)
}

fn safe_copy_output_tree_inner(
    src: &std::path::Path,
    dst: &std::path::Path,
    root: &std::path::Path,
) -> Result<()> {
    fs::create_dir_all(dst).with_context(|| format!("create output dir {}", dst.display()))?;

    for entry in fs::read_dir(src).with_context(|| format!("read dir {}", src.display()))? {
        let entry = entry?;
        let name = entry.file_name();

        // Reject path traversal component names.
        if name == ".." || name == "." {
            anyhow::bail!("rejected path traversal entry: {:?}", name);
        }

        let src_path = entry.path();
        let dst_path = dst.join(&name);

        // Use symlink_metadata so we see the symlink itself, not its target.
        let meta = fs::symlink_metadata(&src_path)
            .with_context(|| format!("stat {}", src_path.display()))?;
        let ft = meta.file_type();

        if ft.is_symlink() {
            anyhow::bail!(
                "symlink rejected in build output ({}); only regular files and directories are allowed",
                src_path.strip_prefix(root).unwrap_or(&src_path).display()
            );
        }
        if !ft.is_file() && !ft.is_dir() {
            anyhow::bail!(
                "special file rejected in build output ({}); only regular files and directories are allowed",
                src_path.strip_prefix(root).unwrap_or(&src_path).display()
            );
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if ft.is_file() && meta.nlink() > 1 {
                anyhow::bail!(
                    "hard-linked file rejected in build output ({}); nlink={}",
                    src_path.strip_prefix(root).unwrap_or(&src_path).display(),
                    meta.nlink()
                );
            }
        }

        if ft.is_dir() {
            safe_copy_output_tree_inner(&src_path, &dst_path, root)?;
        } else {
            fs::copy(&src_path, &dst_path)
                .with_context(|| format!("copy {} → {}", src_path.display(), dst_path.display()))?;
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
                capsule_handle: "test/finalizer".into(),
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

    fn valid_build_id(suffix: &str) -> ArtifactBuildId {
        // Produce a well-formed build_<64 hex> id using distinct hex-char repetitions.
        let hex: String = suffix.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        let padded = format!("{:0<64}", hex);
        ArtifactBuildId::new(format!("build_{}", &padded[..64]))
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
                artifact_build_id: valid_build_id("abc"),
                output_dir,
                artifact_manifest_json: None,
                source_provenance_json: None,
                oci_lock_json: None,
            })
            .unwrap();

        assert_eq!(result.installed_app_id, app);
        assert_eq!(result.profile_id, profile_id);
        assert!(result.install_revision_id.as_str().starts_with("rev_"));
        assert!(result.install_profile_key.as_str().starts_with("ipk_"));
        // CapsuleInstanceKey is NOT present on FinalizerOutput by design.
    }

    #[test]
    fn finalizer_revision_id_is_deterministic() {
        let (dir, store, app, profile_id) = setup();
        let build_id = valid_build_id("deadbeef");
        let finalizer = InstallRevisionFinalizer::new(&store);

        let r1 = finalizer
            .finalize(FinalizerInput {
                installed_app_id: app.clone(),
                profile_id: profile_id.clone(),
                artifact_build_id: build_id.clone(),
                output_dir: make_output_dir(dir.path()),
                artifact_manifest_json: None,
                source_provenance_json: None,
                oci_lock_json: None,
            })
            .unwrap();

        // Same build id → same revision id (idempotent).
        let r2 = finalizer
            .finalize(FinalizerInput {
                installed_app_id: app.clone(),
                profile_id: profile_id.clone(),
                artifact_build_id: build_id.clone(),
                output_dir: make_output_dir(&dir.path().join("second")),
                artifact_manifest_json: None,
                source_provenance_json: None,
                oci_lock_json: None,
            })
            .unwrap();

        assert_eq!(r1.install_revision_id, r2.install_revision_id);
        assert_eq!(r1.install_profile_key, r2.install_profile_key);
    }

    #[test]
    fn finalizer_swaps_current_revision() {
        let (dir, store, app, profile_id) = setup();
        let finalizer = InstallRevisionFinalizer::new(&store);

        let r1 = finalizer
            .finalize(FinalizerInput {
                installed_app_id: app.clone(),
                profile_id: profile_id.clone(),
                artifact_build_id: valid_build_id("1111"),
                output_dir: make_output_dir(&dir.path().join("build1")),
                artifact_manifest_json: None,
                source_provenance_json: None,
                oci_lock_json: None,
            })
            .unwrap();

        let r2 = finalizer
            .finalize(FinalizerInput {
                installed_app_id: app.clone(),
                profile_id: profile_id.clone(),
                artifact_build_id: valid_build_id("2222"),
                output_dir: make_output_dir(&dir.path().join("build2")),
                artifact_manifest_json: None,
                source_provenance_json: None,
                oci_lock_json: None,
            })
            .unwrap();

        // profile key must be stable across revisions
        assert_eq!(r1.install_profile_key, r2.install_profile_key);
        // different builds → different revision ids
        assert_ne!(r1.install_revision_id, r2.install_revision_id);

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
            artifact_build_id: ArtifactBuildId::new(format!("exec_{}", "a".repeat(64))),
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
                artifact_build_id: valid_build_id("cafebabe"),
                output_dir,
                artifact_manifest_json: Some(r#"{"name":"test"}"#.into()),
                source_provenance_json: Some(r#"{"git_ref":"main"}"#.into()),
                oci_lock_json: None,
            })
            .unwrap();

        assert!(result.revision_dir.join("output").join("index.js").exists());
        assert!(result.revision_dir.join("artifact_manifest.json").exists());
        assert!(
            result
                .revision_dir
                .join("source_provenance")
                .join("provenance.json")
                .exists()
        );
    }

    // ── safe_copy_output_tree ─────────────────────────────────────────────

    #[test]
    #[cfg(unix)]
    fn safe_copy_rejects_symlinks() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();
        // Create a symlink inside the source tree.
        symlink("/etc/passwd", src.join("evil_link")).unwrap();
        let dst = dir.path().join("dst");
        let err = safe_copy_output_tree(&src, &dst);
        assert!(err.is_err(), "symlink should be rejected");
        let msg = format!("{}", err.unwrap_err());
        assert!(msg.contains("symlink"), "error must mention symlink");
    }

    #[test]
    #[cfg(unix)]
    fn safe_copy_rejects_hardlinks() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();
        let real_file = dir.path().join("real.txt");
        fs::write(&real_file, b"hello").unwrap();
        // Create a hard link inside the source tree (nlink == 2).
        fs::hard_link(&real_file, src.join("hardlink.txt")).unwrap();
        let dst = dir.path().join("dst");
        let err = safe_copy_output_tree(&src, &dst);
        assert!(err.is_err(), "hard link should be rejected");
        let msg = format!("{}", err.unwrap_err());
        assert!(msg.contains("hard-link"), "error must mention hard-link");
    }

    #[test]
    fn safe_copy_copies_regular_files_and_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let sub = src.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(src.join("a.txt"), b"aaa").unwrap();
        fs::write(sub.join("b.txt"), b"bbb").unwrap();
        let dst = dir.path().join("dst");
        safe_copy_output_tree(&src, &dst).unwrap();
        assert!(dst.join("a.txt").exists());
        assert!(dst.join("sub").join("b.txt").exists());
    }
}
