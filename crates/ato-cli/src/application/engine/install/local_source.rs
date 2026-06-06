//! Hermetic local-directory install source (`ato install --from-local <dir>`).
//!
//! Purpose: a deterministic, offline install path for Desktop/AODD relaunch
//! regression. It takes a local capsule directory (`<dir>/capsule.toml` + its
//! packed source), builds a `.capsule` artifact from it with **no** network /
//! registry / GitHub access, and installs it into the current `ATO_HOME`
//! reusing the **normal** install pipeline:
//!
//! 1. validate the directory + parse the manifest (typed, actionable errors;
//!    nothing is written if the manifest is missing/invalid),
//! 2. pack the directory into a `.capsule` via the same `execute_pack_command`
//!    used by every other build,
//! 3. install from those bytes via [`complete_install_from_bytes`] with a
//!    [`InstallSource::LocalFixture`] (so no remote chunk-sync is attempted),
//! 4. record the **same** installed-state ledger the registry path records
//!    (storage admission + `record_install_launch_ledger`) — the Installed-State
//!    DB is the source of truth and is never bypassed.
//!
//! The resulting installed app relaunches through the identical installed
//! relaunch path as any registry/GitHub install (`ato launch <ipk>` /
//! `ato launch capsule://<handle>`).
//!
//! This is NOT a general package importer and NOT `ato run <dir>`: it mutates no
//! source files, surfaces no raw host paths in user-facing fields, and produces
//! a durable installed-app record.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use capsule_core::AtoError;
use capsule_core::common::paths::ato_path_or_workspace_tmp;
use capsule_core::execution_plan::error::AtoExecutionError;
use capsule_core::installed_state::InstalledStateDb;
use capsule_core::types::CapsuleManifest;

use crate::reporters;

use super::{
    InstallExecutionOptions, InstallResult, InstallSource, ProjectionPreference, ScopedCapsuleRef,
    StorageAdmissionOutcome, complete_install_from_bytes, enforce_storage_admission,
    extract_manifest_toml_from_capsule, install_volume_probe, is_valid_segment,
    normalize_install_segment, record_install_launch_ledger, record_install_storage_claim,
};

/// Default publisher segment used for a local fixture whose manifest declares no
/// publisher. Kept stable so `capsule://local/<slug>` reverse lookup is
/// deterministic across machines.
const LOCAL_FIXTURE_PUBLISHER: &str = "local";

/// Options for [`install_local_directory`]. Mirrors the relevant subset of the
/// registry install options; there is no `version`/`registry`/`yes` knob because
/// the version comes from the manifest and there is no remote to confirm against.
pub struct LocalInstallOptions {
    pub output_dir: Option<PathBuf>,
    pub projection_preference: ProjectionPreference,
    pub json_output: bool,
}

/// Install a capsule directly from a local capsule directory (`--from-local`).
///
/// Hermetic: performs no network, registry, or GitHub access. Reuses the normal
/// pack → install-from-bytes → installed-state-ledger pipeline so the installed
/// artifact is indistinguishable (for relaunch) from a registry/GitHub install.
pub async fn install_local_directory(
    dir: &Path,
    options: LocalInstallOptions,
) -> Result<InstallResult> {
    let LocalInstallOptions {
        output_dir,
        projection_preference,
        json_output,
    } = options;

    // ── 1. Validate the source directory + manifest (no partial install) ──────
    // Typed, actionable errors (no panic, no E999 "internal_error"): invalid
    // input is an EntrypointInvalid, a missing/invalid manifest is a Manifest*
    // error. Nothing is packed or written before all of these pass.
    if !dir.exists() {
        return Err(entrypoint_invalid(
            format!("local install source does not exist: {}", dir.display()),
            "Pass an existing directory that contains a capsule.toml manifest.",
        ));
    }
    if !dir.is_dir() {
        return Err(entrypoint_invalid(
            format!("local install source is not a directory: {}", dir.display()),
            "Pass the directory that contains capsule.toml, not a file.",
        ));
    }
    // Canonicalize for provenance only; this is the one place we touch the host
    // path, and it never reaches a user-facing field/receipt.
    let canonical_dir = dir
        .canonicalize()
        .with_context(|| format!("failed to resolve local install source: {}", dir.display()))?;
    let manifest_path = canonical_dir.join("capsule.toml");
    if !manifest_path.is_file() {
        return Err(anyhow::Error::new(AtoExecutionError::from_ato_error(
            AtoError::ManifestRequiredFieldMissing {
                message: format!(
                    "local install source is missing capsule.toml: {}",
                    manifest_path.display()
                ),
                hint: Some(
                    "A --from-local directory must contain a capsule.toml manifest.".to_string(),
                ),
                path: Some(manifest_path.display().to_string()),
                field: Some("capsule.toml".to_string()),
            },
        )));
    }
    let manifest_text = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("failed to read manifest: {}", manifest_path.display()))?;
    let manifest = CapsuleManifest::from_toml(&manifest_text).map_err(|err| {
        anyhow::Error::new(AtoExecutionError::from_ato_error(
            AtoError::ManifestSchemaInvalid {
                message: format!("failed to parse local capsule manifest: {err}"),
                hint: Some("Fix the capsule.toml manifest so it parses, then retry.".to_string()),
                path: Some(manifest_path.display().to_string()),
                field: None,
            },
        ))
    })?;

    let scoped_ref = derive_local_scoped_ref(&manifest_text, &manifest)?;
    let version = resolve_local_version(&manifest);
    let display_slug = scoped_ref.slug.clone();
    // Stable, host-path-free identity for the synthetic capsule id and the
    // source cache label. The canonical host path is intentionally NOT embedded.
    let capsule_id = format!("local:{}", scoped_ref.scoped_id);
    let cache_label = format!("local-fixture:{}", scoped_ref.scoped_id);

    // Storage admission BEFORE any packing/persisting, mirroring `install_app`:
    // a declared disk requirement that does not fit is rejected up front, so we
    // never leave a persisted-but-rejected install behind.
    let storage_db = InstalledStateDb::open_default()
        .context("open installed-state DB for local install storage admission")?;
    let storage_admission = enforce_storage_admission(
        &storage_db,
        Some(manifest_text.as_str()),
        &install_volume_probe(output_dir.as_deref()),
    )?;

    // ── 2. Pack the directory into a .capsule artifact (offline) ─────────────
    // The source directory must NEVER be mutated: provisioning (e.g. `npm
    // install`) runs in the build directory and rewrites lockfiles. So copy the
    // source into a hermetic staging dir under the ATO tmp root and pack THAT.
    // The staging dir is removed on the way out (best-effort).
    let staging = LocalPackStaging::new(&canonical_dir)?;
    let pack_dir = staging.dir().to_path_buf();
    let pack_json = json_output;
    let build_result = tokio::task::spawn_blocking(move || {
        let reporter = std::sync::Arc::new(reporters::CliReporter::new(pack_json));
        crate::commands::build::execute_pack_command(
            pack_dir,
            /* init_if_missing */ false,
            /* key */ None,
            /* standalone */ false,
            /* force_large_payload */ false,
            /* paid_large_payload */ false,
            /* keep_failed_artifacts */ false,
            // Not strict-manifest: a local fixture has no CAS-registered
            // source_digest, so strict-v3 would reject the local source
            // packaging fallback. This mirrors the GitHub-source install path.
            /* strict_manifest */
            false,
            crate::EnforcementMode::Strict.as_str().to_string(),
            reporter,
            /* timings */ false,
            /* cli_json */ pack_json,
            /* nacelle_override */ None,
        )
    })
    .await
    .context("local capsule pack task failed")?
    .with_context(|| {
        format!(
            "failed to pack local capsule from {}",
            canonical_dir.display()
        )
    })?;

    let artifact_path = build_result.artifact.ok_or_else(|| {
        anyhow::anyhow!(
            "local capsule directory {} did not produce an installable .capsule artifact",
            canonical_dir.display()
        )
    })?;
    let artifact_bytes = std::fs::read(&artifact_path).with_context(|| {
        format!(
            "failed to read packed local capsule artifact: {}",
            artifact_path.display()
        )
    })?;
    // The staged copy (and any provisioning side effects) is no longer needed.
    drop(staging);

    // Defensive: the packed artifact must round-trip its own manifest. This also
    // guarantees the slug used for storage matches the artifact, not just the
    // on-disk source (they can differ if pack normalizes the manifest).
    let packed_manifest_text = extract_manifest_toml_from_capsule(&artifact_bytes)
        .context("packed local capsule artifact is missing capsule.toml")?;

    let normalized_file_name = format!("{}-{}.capsule", scoped_ref.slug, version);

    // ── 3. Install from bytes via the normal pipeline ────────────────────────
    let result = complete_install_from_bytes(
        capsule_id,
        scoped_ref,
        display_slug,
        version,
        artifact_bytes,
        normalized_file_name,
        InstallExecutionOptions {
            output_dir: output_dir.clone(),
            // No interactive consent: a local fixture install is explicit and
            // permissions are bypassed by design for hermetic regression.
            yes: true,
            projection_preference,
            json_output,
            can_prompt_interactively: false,
            promotion_source: None,
            keep_progressive_flow_open: false,
        },
        InstallSource::LocalFixture(cache_label),
    )
    .await?;

    // ── 4. Record the installed-state ledger (SOT — never bypassed) ───────────
    // Mirrors the registry path in `install_app`: record the storage reservation
    // admitted above, then the strict launch-condition ledger so
    // relaunch/admission read a complete condition set. The packed manifest is
    // the authority for launch-condition extraction (pack may normalize it).
    let required_bytes = match storage_admission {
        StorageAdmissionOutcome::Admitted { required_bytes } => {
            record_install_storage_claim(&storage_db, &result, required_bytes);
            Some(required_bytes)
        }
        StorageAdmissionOutcome::Skipped => None,
    };
    record_install_launch_ledger(
        &storage_db,
        &result,
        required_bytes,
        Some(packed_manifest_text.as_str()),
    )?;

    Ok(result)
}

/// Derive a stable `publisher/slug` identity for a local fixture.
///
/// The capsule manifest model carries no publisher (publisher is a
/// registry/GitHub concept), so we read an optional top-level `publisher` key
/// from the raw TOML when present, else fall back to the reserved `local`
/// publisher. The slug is the manifest `name`. Both are normalized to the
/// lowercase kebab-case segment form the rest of the install pipeline requires,
/// so `ato launch <ipk>` and `capsule://<publisher>/<slug>` reverse lookup work.
fn derive_local_scoped_ref(
    manifest_text: &str,
    manifest: &CapsuleManifest,
) -> Result<ScopedCapsuleRef> {
    let raw_name = manifest.name.trim();
    if raw_name.is_empty() {
        bail!("local capsule manifest is missing a `name`");
    }
    let slug = normalize_install_segment(raw_name).with_context(|| {
        format!("local capsule manifest name '{raw_name}' is not a valid capsule slug")
    })?;

    let publisher = match raw_manifest_publisher(manifest_text) {
        Some(declared) => normalize_install_segment(&declared).with_context(|| {
            format!("local capsule manifest publisher '{declared}' is not a valid segment")
        })?,
        None => LOCAL_FIXTURE_PUBLISHER.to_string(),
    };

    debug_assert!(is_valid_segment(&publisher) && is_valid_segment(&slug));
    Ok(ScopedCapsuleRef {
        scoped_id: format!("{publisher}/{slug}"),
        publisher,
        slug,
    })
}

/// Read an optional top-level `publisher = "…"` key from the raw manifest TOML.
/// Returns `None` when absent or not a string. Kept deliberately narrow: the
/// strongly-typed [`CapsuleManifest`] does not model a publisher, and we do not
/// want to invent a schema field here.
fn raw_manifest_publisher(manifest_text: &str) -> Option<String> {
    let value: toml::Value = toml::from_str(manifest_text).ok()?;
    let publisher = value
        .get("publisher")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    Some(publisher.to_string())
}

/// Build a typed `EntrypointInvalid` error for a bad `--from-local` input path,
/// so it is classified/rendered as an actionable input error (not E999).
fn entrypoint_invalid(message: String, hint: &str) -> anyhow::Error {
    anyhow::Error::new(AtoExecutionError::from_ato_error(
        AtoError::EntrypointInvalid {
            message,
            hint: Some(hint.to_string()),
            field: None,
        },
    ))
}

/// Resolve the version for a local install. Manifest version when present, else a
/// stable placeholder so the store layout (`store/<pub>/<slug>/<version>/`) and
/// `<slug>-<version>.capsule` filename are well-formed for a versionless fixture.
fn resolve_local_version(manifest: &CapsuleManifest) -> String {
    let trimmed = manifest.version.trim();
    if trimmed.is_empty() {
        "0.0.0-local".to_string()
    } else {
        trimmed.to_string()
    }
}

/// A hermetic staging copy of a local capsule source directory.
///
/// Packing runs provisioning (e.g. `npm install`) inside the build directory,
/// which mutates lockfiles. To honor the "do not mutate the source dir"
/// contract, the source is copied into a unique directory under the ATO tmp
/// root, packed there, and removed on drop.
struct LocalPackStaging {
    root: PathBuf,
    dir: PathBuf,
}

impl LocalPackStaging {
    fn new(source_dir: &Path) -> Result<Self> {
        let staging_root = ato_path_or_workspace_tmp("tmp/local-install");
        std::fs::create_dir_all(&staging_root).with_context(|| {
            format!(
                "failed to create local-install staging root: {}",
                staging_root.display()
            )
        })?;
        let unique = format!("stage-{}-{}", std::process::id(), rand::random::<u32>());
        let root = staging_root.join(unique);
        let dir = root.join("source");
        copy_dir_recursive(source_dir, &dir).with_context(|| {
            format!(
                "failed to stage local capsule source {} -> {}",
                source_dir.display(),
                dir.display()
            )
        })?;
        Ok(Self { root, dir })
    }

    fn dir(&self) -> &Path {
        &self.dir
    }
}

impl Drop for LocalPackStaging {
    fn drop(&mut self) {
        if let Err(err) = std::fs::remove_dir_all(&self.root)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            tracing::debug!(
                path = %self.root.display(),
                error = %err,
                "failed to remove local-install staging dir"
            );
        }
    }
}

/// Recursively copy a directory tree. Symlinks are copied as their target
/// contents (fixtures are plain files); special files are skipped.
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &dst_path)?;
        } else if file_type.is_file() || file_type.is_symlink() {
            std::fs::copy(entry.path(), &dst_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_from(text: &str) -> CapsuleManifest {
        CapsuleManifest::from_toml(text).expect("parse manifest")
    }

    #[test]
    fn derives_local_publisher_when_manifest_has_none() {
        let text = r#"
schema_version = "0.3"
name = "basic-web"
version = "0.1.0"
type = "app"
default_target = "main"

[targets.main]
runtime = "source"
driver = "node"
run = "node server.js"
"#;
        let manifest = manifest_from(text);
        let scoped = derive_local_scoped_ref(text, &manifest).unwrap();
        assert_eq!(scoped.publisher, "local");
        assert_eq!(scoped.slug, "basic-web");
        assert_eq!(scoped.scoped_id, "local/basic-web");
    }

    #[test]
    fn honors_declared_publisher() {
        let text = r#"
schema_version = "0.3"
name = "Basic Web"
version = "0.1.0"
type = "app"
default_target = "main"
publisher = "Acme Corp"

[targets.main]
runtime = "source"
driver = "node"
run = "node server.js"
"#;
        let manifest = manifest_from(text);
        let scoped = derive_local_scoped_ref(text, &manifest).unwrap();
        assert_eq!(scoped.publisher, "acme-corp");
        assert_eq!(scoped.slug, "basic-web");
    }

    #[test]
    fn versionless_manifest_gets_stable_placeholder() {
        let text = r#"
schema_version = "0.3"
name = "basic-web"
type = "app"
default_target = "main"

[targets.main]
runtime = "source"
driver = "node"
run = "node server.js"
"#;
        let manifest = manifest_from(text);
        assert_eq!(resolve_local_version(&manifest), "0.0.0-local");
    }

    #[test]
    fn raw_manifest_publisher_ignores_missing_and_non_string() {
        assert_eq!(raw_manifest_publisher("name = \"x\"\n"), None);
        assert_eq!(raw_manifest_publisher("publisher = 7\n"), None);
        assert_eq!(
            raw_manifest_publisher("publisher = \"koh0920\"\n").as_deref(),
            Some("koh0920")
        );
    }
}
