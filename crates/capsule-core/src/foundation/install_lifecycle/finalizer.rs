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

use super::hashing::canonical_hash;
use super::ids::{
    ArtifactBuildId, InstallProfileKey, InstallRevisionId, InstalledAppId, ProfileId,
    derive_install_profile_key, revision_id_for_build,
};
use super::records::{
    ArtifactBuild, InstallReceipt, InstallRevision, RequirementGraph, RequirementGraphSnapshot,
    StateContractSnapshot,
};
use super::store::InstallInstanceStore;

// ── Input ──────────────────────────────────────────────────────────────────

/// Structured build facts a caller may supply so the finalizer can persist
/// meaningful install-output records (#581 wave 2).
///
/// Every field is optional/defaulted: callers that do not yet have a fact omit
/// it and the finalizer persists an explicit, typed placeholder rather than
/// fabricating one. None of these are session/runtime/observed facts, and none
/// is a secret value — they describe the build artifact and its requirements.
#[derive(Debug, Clone, Default)]
pub struct InstallBuildFacts {
    /// Canonical capsule reference (e.g. `"<publisher>/<slug>@<version>"`).
    pub capsule_ref: Option<String>,
    /// Source provenance reference (git commit/tag, or the registry content
    /// hash for a pre-built artifact). Never a secret.
    pub source_provenance_ref: Option<String>,
    /// Content hash of the produced output (`blake3:<hex>`).
    pub output_content_hash: Option<String>,
    /// Content hash of resolved dependency outputs, if any.
    pub dependency_output_hash: Option<String>,
    /// Build platform profile (`"linux/x86_64"`, …).
    pub platform: Option<String>,
    /// A fully compiled requirement-graph snapshot, if the caller has one.
    /// When `None`, the finalizer persists an explicitly-minimal empty graph.
    pub requirement_graph: Option<RequirementGraphSnapshot>,
    /// State-contract snapshots. Empty until the install path analyses storage.
    pub state_contracts: Vec<StateContractSnapshot>,
    /// Hash of the normalized profile defaults, if computed by the caller.
    pub profile_defaults_hash: Option<String>,
}

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
    /// Optional structured build facts used to persist typed install-output
    /// records (#581 wave 2). When absent, conservative typed placeholders are
    /// persisted.
    pub build_facts: Option<InstallBuildFacts>,
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
    /// The persisted install-output revision record (#581 wave 2). Self-contained
    /// authority binding the artifact build, requirement graph, state contracts,
    /// and install receipt for this revision.
    pub install_revision: InstallRevision,
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
    pub fn finalize(&self, mut input: FinalizerInput) -> Result<FinalizerOutput> {
        // 1. Validate build id.
        if let Err(e) = input.artifact_build_id.validate() {
            anyhow::bail!("invalid artifact_build_id: {e}");
        }

        // 2. Derive stable IDs.
        let install_profile_key =
            derive_install_profile_key(&input.installed_app_id, &input.profile_id);
        let install_revision_id = revision_id_for_build(&input.artifact_build_id);
        let build_facts = input.build_facts.take().unwrap_or_default();

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

        // 5. Write artifact_manifest.json. One timestamp is computed for the
        //    whole finalize() so the manifest and the typed records below carry
        //    the same instant for this install event.
        let now = iso8601_now();
        let rev_manifest = ArtifactRevisionManifest {
            install_revision_id: install_revision_id.clone(),
            artifact_build_id: input.artifact_build_id.clone(),
            finalized_at: now.clone(),
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

        // 8. Build and persist the typed install-output records (#581 wave 2).
        //    Written before the current_revision swap so an interruption leaves
        //    the revision un-finalized (revision.json is the marker).
        let created_at = now;

        // 8a. ArtifactBuild — keyed by the build id passed in (content-addressed
        //     by the caller), never a re-derived one, so the revision id stays
        //     deterministic. No session/runtime/observed fields.
        let output_content_hash = build_facts.output_content_hash.clone();
        let output_ref = output_content_hash
            .as_deref()
            .map(artifact_output_ref)
            .unwrap_or_else(|| format!("/revisions/{}/output", install_revision_id.as_str()));
        let artifact_build = ArtifactBuild {
            artifact_build_id: input.artifact_build_id.clone(),
            capsule_ref: build_facts
                .capsule_ref
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            source_provenance_ref: build_facts
                .source_provenance_ref
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            output_ref,
            // Explicit display placeholder when the caller supplied no content
            // hash. Never conflated with a real hash: `output_hashes` below is
            // driven off the typed Option, not this string.
            output_content_hash: output_content_hash
                .clone()
                .unwrap_or_else(|| "unset".to_string()),
            dependency_output_hash: build_facts.dependency_output_hash.clone(),
            platform: build_facts.platform.clone(),
            // TODO(#581): no typed build-receipt ref is produced by the current
            // install path; populate once the artifact build producer surfaces one.
            build_receipt_ref: None,
            created_at: created_at.clone(),
        };

        // 8b. RequirementGraphSnapshot — the caller's compiled graph if present,
        //     otherwise an explicitly-minimal, deterministic empty graph (the
        //     requirement-graph compiler is a later #581 wave).
        let requirement_graph = match build_facts.requirement_graph.clone() {
            Some(snapshot) => snapshot,
            None => minimal_requirement_graph_snapshot(
                &install_revision_id,
                build_facts.profile_defaults_hash.as_deref(),
            )?,
        };

        // 8c. StateContractSnapshot[] — empty until the install path analyses
        //     storage contracts (later wave). Persisted explicitly as `[]`.
        let state_contracts = build_facts.state_contracts.clone();

        // 8d. InstallReceipt — install-time audit record (NOT the execution
        //     receipt). Deterministic id per revision.
        let install_receipt = InstallReceipt {
            receipt_id: format!("irecpt-{}", install_revision_id.as_str()),
            install_profile_key: install_profile_key.clone(),
            install_revision_id: install_revision_id.clone(),
            artifact_build_id: input.artifact_build_id.clone(),
            resolved_input_refs: vec![],
            // Driven off the typed Option: empty when no content hash was
            // supplied, otherwise exactly the one hash.
            output_hashes: output_content_hash.iter().cloned().collect(),
            occurred_at: created_at.clone(),
        };

        // 8e. InstallRevision — self-contained authority binding the above.
        //     Launch templates + compatibility index require resolved bindings,
        //     runtime requirements, and runner placement and are deferred to the
        //     next #581 wave: persisted empty/None here, never a fake template.
        let install_revision = InstallRevision {
            install_revision_id: install_revision_id.clone(),
            install_profile_key: install_profile_key.clone(),
            artifact_build_id: input.artifact_build_id.clone(),
            requirement_graph: requirement_graph.clone(),
            state_contracts: state_contracts.clone(),
            install_receipt: install_receipt.clone(),
            created_at: created_at.clone(),
            launch_templates: vec![],
            compatibility_index: None,
        };

        // 8f. Re-finalize safety: drop any existing marker so the revision is
        //     transiently un-finalized while sub-records are rewritten. Then
        //     persist the sub-records, and write the revision.json marker LAST.
        let app = &input.installed_app_id;
        let profile = &input.profile_id;
        self.store
            .remove_install_revision_marker(app, profile, &install_revision_id)?;
        self.store
            .write_artifact_build(app, profile, &install_revision_id, &artifact_build)?;
        self.store.write_requirement_graph_snapshot(
            app,
            profile,
            &install_revision_id,
            &requirement_graph,
        )?;
        self.store
            .write_state_contracts(app, profile, &install_revision_id, &state_contracts)?;
        self.store
            .write_install_receipt(app, profile, &install_revision_id, &install_receipt)?;
        self.store
            .write_install_revision(app, profile, &install_revision)?;

        // 9. Atomically swap current_revision (the commit point).
        self.store.set_current_revision(
            &input.installed_app_id,
            &input.profile_id,
            &install_revision_id,
        )?;

        // 10. Return.
        let revision_dir = self.store.revision_dir(&install_revision_id);
        Ok(FinalizerOutput {
            installed_app_id: input.installed_app_id,
            profile_id: input.profile_id,
            install_profile_key,
            install_revision_id,
            artifact_build_id: input.artifact_build_id,
            revision_dir,
            install_revision,
        })
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn iso8601_now() -> String {
    // Use chrono if available; fall back to a placeholder.
    chrono::Utc::now().to_rfc3339()
}

/// Map a content hash like `blake3:<hex>` to a content-addressed artifact ref
/// `/artifacts/blake3/<hex>` (RFC ResourcePath form). Falls back to a single
/// `/artifacts/<hash>` segment when the hash has no `algo:hex` shape.
fn artifact_output_ref(content_hash: &str) -> String {
    match content_hash.split_once(':') {
        Some((algo, hex)) => format!("/artifacts/{algo}/{hex}"),
        None => format!("/artifacts/{content_hash}"),
    }
}

/// Build an explicitly-minimal, deterministic requirement-graph snapshot for an
/// install path that does not yet compile a real application requirement graph.
///
/// Deterministic from the revision id, so re-finalizing the same build yields
/// the same `graph_hash` (no timestamp or host-specific input). The empty graph
/// + `reqgraph-…-minimal:<rev>` ids make the placeholder explicit; it is not a
/// successful compiled graph.
fn minimal_requirement_graph_snapshot(
    rev: &InstallRevisionId,
    profile_defaults_hash: Option<&str>,
) -> Result<RequirementGraphSnapshot> {
    let graph = RequirementGraph {
        graph_id: format!("reqgraph-minimal:{}", rev.as_str()),
        nodes: vec![],
        edges: vec![],
    };
    let profile_defaults_hash = match profile_defaults_hash {
        Some(h) => h.to_string(),
        None => canonical_hash(&"ato.install.profile_defaults.minimal.v0")?,
    };
    RequirementGraphSnapshot::new(
        format!("reqgraph-snapshot-minimal:{}", rev.as_str()),
        graph,
        Some(rev.as_str().to_string()),
        profile_defaults_hash,
    )
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
    use crate::foundation::install_lifecycle::ids::InstallRevisionId;
    use crate::foundation::install_lifecycle::records::ArtifactBuild;
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
                build_facts: None,
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
                build_facts: None,
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
                build_facts: None,
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
                build_facts: None,
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
                build_facts: None,
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
            build_facts: None,
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
                build_facts: None,
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

    // ── #581 wave 2: install-output record persistence ──────────────────────

    fn sample_facts() -> InstallBuildFacts {
        InstallBuildFacts {
            capsule_ref: Some("acme/pgweb@1.2.3".into()),
            // For a pre-built registry artifact the content hash is a legitimate
            // provenance ref; it is not a secret.
            source_provenance_ref: Some("blake3:cafef00d".into()),
            output_content_hash: Some("blake3:cafef00d".into()),
            dependency_output_hash: None,
            platform: Some("linux/x86_64".into()),
            requirement_graph: None,
            state_contracts: vec![],
            profile_defaults_hash: None,
        }
    }

    fn finalize_default(
        store: &InstallInstanceStore,
        app: &InstalledAppId,
        profile: &ProfileId,
        output_dir: PathBuf,
        build_id: ArtifactBuildId,
        facts: Option<InstallBuildFacts>,
    ) -> FinalizerOutput {
        InstallRevisionFinalizer::new(store)
            .finalize(FinalizerInput {
                installed_app_id: app.clone(),
                profile_id: profile.clone(),
                artifact_build_id: build_id,
                output_dir,
                artifact_manifest_json: None,
                source_provenance_json: None,
                oci_lock_json: None,
                build_facts: facts,
            })
            .unwrap()
    }

    // Required tests 1-4: finalizer writes each record file.
    #[test]
    fn finalizer_writes_install_output_record_files() {
        let (dir, store, app, profile_id) = setup();
        let out = finalize_default(
            &store,
            &app,
            &profile_id,
            make_output_dir(dir.path()),
            valid_build_id("aa11"),
            Some(sample_facts()),
        );
        let rev = &out.install_revision_id;
        assert!(
            store
                .revision_install_record_path(&app, &profile_id, rev)
                .exists(),
            "revision.json must be written"
        );
        assert!(
            store
                .revision_artifact_build_path(&app, &profile_id, rev)
                .exists(),
            "artifact-build.json must be written"
        );
        assert!(
            store
                .revision_requirement_graph_path(&app, &profile_id, rev)
                .exists(),
            "requirement-graph.json must be written"
        );
        assert!(
            store
                .revision_install_receipt_path(&app, &profile_id, rev)
                .exists(),
            "install-receipt.json must be written"
        );
        assert!(
            store
                .revision_state_contracts_path(&app, &profile_id, rev)
                .exists(),
            "state-contracts.json must be written"
        );
        // The pre-existing (shared, content-keyed) artifact_manifest.json is still written too.
        assert!(store.revision_artifact_manifest_path(rev).exists());
        assert!(store.is_revision_finalized(&app, &profile_id, rev));
    }

    // Required test 5: readback reconstructs the same typed records.
    #[test]
    fn readback_reconstructs_typed_records() {
        let (dir, store, app, profile_id) = setup();
        let out = finalize_default(
            &store,
            &app,
            &profile_id,
            make_output_dir(dir.path()),
            valid_build_id("bb22"),
            Some(sample_facts()),
        );
        let rev = &out.install_revision_id;

        let revision = store.read_install_revision(&app, &profile_id, rev).unwrap();
        assert_eq!(revision.install_revision_id, *rev);
        assert_eq!(revision.artifact_build_id, out.artifact_build_id);
        assert_eq!(revision.install_profile_key, out.install_profile_key);
        // The on-disk record round-trips to exactly what the finalizer returned.
        assert_eq!(revision, out.install_revision);

        let build = store.read_artifact_build(&app, &profile_id, rev).unwrap();
        assert_eq!(build.artifact_build_id, out.artifact_build_id);
        assert_eq!(build.output_content_hash, "blake3:cafef00d");
        assert_eq!(build.output_ref, "/artifacts/blake3/cafef00d");
        assert_eq!(build.capsule_ref, "acme/pgweb@1.2.3");
        assert_eq!(build.platform.as_deref(), Some("linux/x86_64"));

        // The standalone sub-record files must equal the copies embedded in
        // revision.json (they are written from the same in-memory values, so
        // they cannot diverge — assert it rather than assume it).
        let graph = store
            .read_requirement_graph_snapshot(&app, &profile_id, rev)
            .unwrap();
        assert_eq!(graph, revision.requirement_graph);
        let receipt = store.read_install_receipt(&app, &profile_id, rev).unwrap();
        assert_eq!(receipt, revision.install_receipt);
        assert_eq!(receipt.output_hashes, vec!["blake3:cafef00d".to_string()]);
        let contracts = store.read_state_contracts(&app, &profile_id, rev).unwrap();
        assert_eq!(contracts, revision.state_contracts);

        // Launch reuse readback chain: (app, profile) -> current revision -> records.
        #[cfg(unix)]
        {
            let via_current = store
                .read_current_install_revision(&app, &profile_id)
                .unwrap();
            assert_eq!(via_current.install_revision_id, *rev);
        }
    }

    // Required test 6: artifact_build_id distinct from install_revision_id.
    #[test]
    fn artifact_build_id_distinct_from_install_revision_id_on_disk() {
        let (dir, store, app, profile_id) = setup();
        let out = finalize_default(
            &store,
            &app,
            &profile_id,
            make_output_dir(dir.path()),
            valid_build_id("cc33"),
            Some(sample_facts()),
        );
        let build = store
            .read_artifact_build(&app, &profile_id, &out.install_revision_id)
            .unwrap();
        assert!(build.artifact_build_id.as_str().starts_with("build_"));
        assert!(out.install_revision_id.as_str().starts_with("rev_"));
        assert_ne!(
            build.artifact_build_id.as_str(),
            out.install_revision_id.as_str(),
            "revision identity must not alias the build identity"
        );
    }

    // Required test 7: install-time records contain no session/runtime/observed fields.
    #[test]
    fn install_records_have_no_session_or_observed_fields() {
        let (dir, store, app, profile_id) = setup();
        let out = finalize_default(
            &store,
            &app,
            &profile_id,
            make_output_dir(dir.path()),
            valid_build_id("dd44"),
            Some(sample_facts()),
        );
        let rev = &out.install_revision_id;
        let forbidden = [
            "session_id",
            "dynamic_port",
            "process_id",
            "container_id",
            "live_route",
            "log_cursor",
            "observed_status",
            "secret_value",
        ];
        for path in [
            store.revision_install_record_path(&app, &profile_id, rev),
            store.revision_artifact_build_path(&app, &profile_id, rev),
            store.revision_requirement_graph_path(&app, &profile_id, rev),
            store.revision_state_contracts_path(&app, &profile_id, rev),
            store.revision_install_receipt_path(&app, &profile_id, rev),
        ] {
            let raw = fs::read_to_string(&path).unwrap();
            for term in forbidden {
                assert!(
                    !raw.contains(term),
                    "persisted record {} must not contain '{}'",
                    path.display(),
                    term
                );
            }
        }
    }

    // Required test 8: re-finalizing the same deterministic inputs ⇒ stable hashes.
    #[test]
    fn refinalize_same_inputs_produces_stable_hashes() {
        let (dir, store, app, profile_id) = setup();
        let build_id = valid_build_id("ee55");
        let o1 = finalize_default(
            &store,
            &app,
            &profile_id,
            make_output_dir(dir.path()),
            build_id.clone(),
            Some(sample_facts()),
        );
        let g1 = store
            .read_requirement_graph_snapshot(&app, &profile_id, &o1.install_revision_id)
            .unwrap();

        let o2 = finalize_default(
            &store,
            &app,
            &profile_id,
            make_output_dir(&dir.path().join("second")),
            build_id,
            Some(sample_facts()),
        );
        let g2 = store
            .read_requirement_graph_snapshot(&app, &profile_id, &o2.install_revision_id)
            .unwrap();

        assert_eq!(
            o1.install_revision_id, o2.install_revision_id,
            "revision id deterministic from build id"
        );
        assert_eq!(o1.artifact_build_id, o2.artifact_build_id);
        assert_eq!(
            g1.graph_hash, g2.graph_hash,
            "requirement graph hash stable across re-finalize (no timestamp/host input)"
        );
        // Receipt id is deterministic per revision (timestamps differ but are not identity).
        let r = store
            .read_install_receipt(&app, &profile_id, &o1.install_revision_id)
            .unwrap();
        assert_eq!(
            r.receipt_id,
            format!("irecpt-{}", o1.install_revision_id.as_str())
        );
    }

    // State contracts persisted as an explicit empty list (no fabricated contracts).
    #[test]
    fn state_contracts_persisted_as_explicit_empty_list() {
        let (dir, store, app, profile_id) = setup();
        let out = finalize_default(
            &store,
            &app,
            &profile_id,
            make_output_dir(dir.path()),
            valid_build_id("ff66"),
            Some(sample_facts()),
        );
        let contracts = store
            .read_state_contracts(&app, &profile_id, &out.install_revision_id)
            .unwrap();
        assert!(
            contracts.is_empty(),
            "no state contracts are resolved on the standard install path yet"
        );
        let raw = fs::read_to_string(store.revision_state_contracts_path(
            &app,
            &profile_id,
            &out.install_revision_id,
        ))
        .unwrap();
        assert_eq!(raw.trim(), "[]", "empty list persisted explicitly");
    }

    // Records are written even when no build facts are supplied (typed placeholders).
    #[test]
    fn finalizer_persists_records_without_build_facts() {
        let (dir, store, app, profile_id) = setup();
        let out = finalize_default(
            &store,
            &app,
            &profile_id,
            make_output_dir(dir.path()),
            valid_build_id("1234"),
            None,
        );
        let build = store
            .read_artifact_build(&app, &profile_id, &out.install_revision_id)
            .unwrap();
        // Explicit placeholders, not fabricated values.
        assert_eq!(build.output_content_hash, "unset");
        assert_eq!(build.capsule_ref, "unknown");
        assert_eq!(build.platform, None, "no platform persisted without facts");
        let receipt = store
            .read_install_receipt(&app, &profile_id, &out.install_revision_id)
            .unwrap();
        assert!(
            receipt.output_hashes.is_empty(),
            "no output hash recorded when no content hash was supplied"
        );
        let revision = store
            .read_install_revision(&app, &profile_id, &out.install_revision_id)
            .unwrap();
        assert!(
            revision.launch_templates.is_empty(),
            "no fake launch templates are persisted"
        );
        assert!(
            revision.compatibility_index.is_none(),
            "no fake compatibility index is persisted"
        );
    }

    // Required test 9 (store guard): a revision without revision.json is not finalized.
    #[test]
    fn partial_revision_is_not_treated_as_finalized() {
        let (_dir, store, app, profile_id) = setup();
        // Write only one sub-record — never the revision.json marker — to
        // simulate an interrupted finalize.
        let rev = InstallRevisionId::new(format!("rev_{}", "a".repeat(32)));
        store
            .write_artifact_build(
                &app,
                &profile_id,
                &rev,
                &ArtifactBuild {
                    artifact_build_id: valid_build_id("a1b2"),
                    capsule_ref: "acme/x@1".into(),
                    source_provenance_ref: "blake3:00".into(),
                    output_ref: "/artifacts/blake3/00".into(),
                    output_content_hash: "blake3:00".into(),
                    dependency_output_hash: None,
                    platform: None,
                    build_receipt_ref: None,
                    created_at: "2026-06-08T00:00:00Z".into(),
                },
            )
            .unwrap();

        assert!(
            !store.is_revision_finalized(&app, &profile_id, &rev),
            "a revision without revision.json must not be considered finalized"
        );
        assert!(
            store
                .read_install_revision(&app, &profile_id, &rev)
                .is_err(),
            "reading an un-finalized revision must fail rather than silently succeed"
        );
    }

    // Required test 9 (finalize ordering): if a sub-record write fails mid-way,
    // the revision.json marker is never written, so the revision stays
    // un-finalized. This exercises the finalizer's "marker written last"
    // guarantee through finalize(), not just the store read guard above.
    #[cfg(unix)]
    #[test]
    fn finalize_failure_midway_leaves_revision_unfinalized() {
        let (dir, store, app, profile_id) = setup();
        let build_id = valid_build_id("9f9f");
        let rev = revision_id_for_build(&build_id);

        // Sabotage the requirement-graph record path by pre-creating a directory
        // where the file should go, so its atomic write (rename) fails.
        let rev_records_dir = store.profile_revision_dir(&app, &profile_id, &rev);
        fs::create_dir_all(&rev_records_dir).unwrap();
        fs::create_dir_all(rev_records_dir.join("requirement-graph.json")).unwrap();

        let result = InstallRevisionFinalizer::new(&store).finalize(FinalizerInput {
            installed_app_id: app.clone(),
            profile_id: profile_id.clone(),
            artifact_build_id: build_id,
            output_dir: make_output_dir(dir.path()),
            artifact_manifest_json: None,
            source_provenance_json: None,
            oci_lock_json: None,
            build_facts: Some(sample_facts()),
        });

        assert!(
            result.is_err(),
            "finalize must fail when a sub-record write fails"
        );
        // The earlier sub-record was written, but the marker was not — proving
        // revision.json is written only after all sub-records succeed.
        assert!(
            store
                .revision_artifact_build_path(&app, &profile_id, &rev)
                .exists(),
            "the sub-record written before the failure exists"
        );
        assert!(
            !store.is_revision_finalized(&app, &profile_id, &rev),
            "marker must not exist when a prior sub-record write failed"
        );
    }

    // Regression for the cross-(app,profile) clobber: two profiles installing the
    // SAME artifact content share one content-keyed revision id, but their
    // install-output records live in per-(app,profile) dirs and must not clobber.
    #[cfg(unix)]
    #[test]
    fn same_artifact_across_profiles_does_not_clobber_records() {
        let (dir, store, app, default_profile) = setup();
        let staging = ProfileId::new("staging");
        store
            .write_profile(
                &app,
                &LaunchProfile {
                    profile_id: staging.clone(),
                    ..Default::default()
                },
            )
            .unwrap();

        // Same build id (same artifact content) for both profiles.
        let build_id = valid_build_id("c0ffee");
        let o_default = finalize_default(
            &store,
            &app,
            &default_profile,
            make_output_dir(&dir.path().join("d")),
            build_id.clone(),
            Some(sample_facts()),
        );
        let o_staging = finalize_default(
            &store,
            &app,
            &staging,
            make_output_dir(&dir.path().join("s")),
            build_id,
            Some(sample_facts()),
        );

        // Same revision id (content-addressed), different install_profile_key.
        assert_eq!(o_default.install_revision_id, o_staging.install_revision_id);
        assert_ne!(o_default.install_profile_key, o_staging.install_profile_key);

        // Each profile reads back ITS OWN install_profile_key — no clobber.
        let rev = &o_default.install_revision_id;
        let r_default = store
            .read_install_revision(&app, &default_profile, rev)
            .unwrap();
        let r_staging = store.read_install_revision(&app, &staging, rev).unwrap();
        assert_eq!(
            r_default.install_profile_key, o_default.install_profile_key,
            "default profile's record must keep its own install_profile_key"
        );
        assert_eq!(
            r_staging.install_profile_key, o_staging.install_profile_key,
            "staging profile's record must keep its own install_profile_key"
        );
        assert_ne!(r_default.install_profile_key, r_staging.install_profile_key);
    }
}
