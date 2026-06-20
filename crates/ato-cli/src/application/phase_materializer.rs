//! Local phase materialization primitives.
//!
//! The first phase-level MVP only freezes declared build outputs into the
//! immutable blob store. Dependency outputs stay on their ecosystem-specific
//! path until relocation contracts exist for npm/pnpm/yarn/uv adapters.

use std::fs;
use std::path::{Path, PathBuf};

#[cfg(test)]
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result};
use blake3::Hasher;
use capsule::blob::{BlobManifest, hash_tree};
use capsule::common::paths::{ato_cache_dir, workspace_tmp_dir};
use capsule::common::store::BlobAddress;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use crate::application::build_materialization::BuildObservation;
use crate::application::dependency_materializer::freeze::DerivationLock;
use crate::application::projection::project_payload;
use crate::application::source_inventory::{OutputSpec, normalize_outputs};

const BUILD_OUTPUT_KEY_VERSION: &str = "ato-phase-build-output-key-v1";
pub(crate) const MATERIALIZER_SCHEMA_VERSION: &str = "ato-phase-materializer-schema-v1";
pub(crate) const PROJECTION_ALGORITHM_VERSION: &str = "ato-build-output-projection-v1";

/// Store handle for a relocatable build-output layer.
///
/// The layer commits to one exact output contract. The build observation still
/// owns the phase input digest; this record is the local CAS projection handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BuildOutputLayerRecord {
    pub(crate) materialization_key: String,
    pub(crate) blob_hash: String,
    pub(crate) output_contract_digest: String,
    pub(crate) platform_profile: String,
    pub(crate) outputs: Vec<String>,
}

/// Capture build outputs, acquiring the build output lock internally.
///
/// Suitable for standalone use (e.g. tests). Production callers should prefer
/// [`capture_build_outputs_locked`] when the lock is already held to close the
/// race window between build execution and capture.
pub(crate) fn capture_build_outputs(
    workspace_root: &Path,
    observation: &BuildObservation,
) -> Result<Option<BuildOutputLayerRecord>> {
    let key = materialization_key_for_observation(observation)?;
    let lock = DerivationLock::acquire(&key)?;
    capture_build_outputs_locked(&lock, workspace_root, observation)
}

/// Capture build outputs when the caller already holds the build output lock.
///
/// Use this in the execute path so that workspace-output reading (capture) and
/// state-record writing happen inside the same lock region as the build
/// executor — preventing concurrent runs from racing on projection/capture.
pub(crate) fn capture_build_outputs_locked(
    _lock: &DerivationLock,
    workspace_root: &Path,
    observation: &BuildObservation,
) -> Result<Option<BuildOutputLayerRecord>> {
    let outputs = normalize_outputs(&observation.outputs)
        .context("failed to normalize build output contract for capture")?;
    if outputs.is_empty() {
        return Ok(None);
    }

    let working_dir = working_dir(workspace_root, &observation.working_dir_relative);
    if !all_outputs_exist(&working_dir, &outputs) {
        return Ok(None);
    }

    let platform_profile = current_platform_profile();
    let output_contract_digest = output_contract_digest(&outputs);
    let materialization_key = build_output_materialization_key(
        &observation.input_digest,
        &output_contract_digest,
        &platform_profile,
    );
    let capture_root = capture_root(&materialization_key);
    let _cleanup = CaptureCleanup(capture_root.clone());
    copy_declared_outputs(&working_dir, &capture_root, &outputs)?;

    let blob_hash = freeze_build_output_tree_unlocked(&capture_root, &materialization_key)?;
    Ok(Some(BuildOutputLayerRecord {
        materialization_key,
        blob_hash,
        output_contract_digest,
        platform_profile,
        outputs: normalized_output_paths(&outputs),
    }))
}

pub(crate) fn project_build_outputs(
    workspace_root: &Path,
    observation: &BuildObservation,
    layer: &BuildOutputLayerRecord,
) -> Result<()> {
    let plan = build_projection_plan(workspace_root, observation, layer)?;
    let staging_root = projection_staging_root(workspace_root, &layer.materialization_key);
    remove_path_if_exists(&staging_root)?;
    fs::create_dir_all(&staging_root)
        .with_context(|| format!("failed to create {}", staging_root.display()))?;
    let mut committed = Vec::new();

    let result = (|| -> Result<()> {
        for (index, entry) in plan.iter().enumerate() {
            let staging_target = staging_root.join(index.to_string());
            project_output_entry(&entry.source, &staging_target, &entry.metadata)?;
        }

        for (index, entry) in plan.iter().enumerate() {
            run_projection_commit_hook(index, &entry.target);
            ensure_projection_target_absent(&entry.target)?;
            if let Some(parent) = entry.target.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
            let staging_target = staging_root.join(index.to_string());
            fs::rename(&staging_target, &entry.target).with_context(|| {
                format!(
                    "failed to commit projected build output {} -> {}",
                    staging_target.display(),
                    entry.target.display()
                )
            })?;
            committed.push(entry.target.clone());
        }
        Ok(())
    })();

    let cleanup_result = remove_path_if_exists(&staging_root);
    if let Err(error) = result {
        rollback_committed_outputs(&committed);
        cleanup_result?;
        return Err(error);
    }
    cleanup_result?;
    Ok(())
}

pub(crate) fn acquire_build_output_lock_for_observation(
    observation: &BuildObservation,
) -> Result<DerivationLock> {
    DerivationLock::acquire(&materialization_key_for_observation(observation)?)
}

fn build_projection_plan(
    workspace_root: &Path,
    observation: &BuildObservation,
    layer: &BuildOutputLayerRecord,
) -> Result<Vec<ProjectionPlanEntry>> {
    validate_build_output_layer_metadata(observation, layer)?;
    let address = verify_local_build_output_blob(&layer.blob_hash)?;
    let payload = address.payload_dir();
    let outputs = normalize_outputs(&observation.outputs)
        .context("failed to normalize build output contract for projection")?;
    let working_dir = working_dir(workspace_root, &observation.working_dir_relative);
    let mut plan = Vec::new();
    for output in &outputs {
        let source = payload.join(&output.relative_path);
        let metadata = validate_output_entry(&source).with_context(|| {
            format!(
                "build output layer {} is missing declared output {}",
                layer.blob_hash,
                output.relative_path.display()
            )
        })?;
        let target = working_dir.join(&output.relative_path);
        ensure_projection_target_absent(&target)?;
        plan.push(ProjectionPlanEntry {
            source,
            target,
            metadata,
        });
    }
    Ok(plan)
}

pub(crate) fn validate_build_output_layer_metadata(
    observation: &BuildObservation,
    layer: &BuildOutputLayerRecord,
) -> Result<()> {
    let outputs = normalize_outputs(&observation.outputs)
        .context("failed to normalize build output contract for projection")?;
    let expected_paths = normalized_output_paths(&outputs);
    let expected_contract = output_contract_digest(&outputs);
    if layer.outputs != expected_paths || layer.output_contract_digest != expected_contract {
        anyhow::bail!("build output layer contract does not match the current build observation");
    }

    let expected_key = build_output_materialization_key(
        &observation.input_digest,
        &expected_contract,
        &layer.platform_profile,
    );
    if layer.materialization_key != expected_key {
        anyhow::bail!(
            "build output layer materialization key does not match the current build inputs"
        );
    }
    if layer.platform_profile != current_platform_profile() {
        anyhow::bail!(
            "build output layer platform '{}' does not match '{}'",
            layer.platform_profile,
            current_platform_profile()
        );
    }
    Ok(())
}

pub(crate) fn build_output_materialization_key(
    input_digest: &str,
    output_contract_digest: &str,
    platform_profile: &str,
) -> String {
    let mut hasher = Hasher::new();
    update_text(&mut hasher, BUILD_OUTPUT_KEY_VERSION);
    update_text(&mut hasher, "build");
    update_text(&mut hasher, MATERIALIZER_SCHEMA_VERSION);
    update_text(&mut hasher, PROJECTION_ALGORITHM_VERSION);
    update_text(&mut hasher, input_digest);
    update_text(&mut hasher, output_contract_digest);
    update_text(&mut hasher, platform_profile);
    format!("blake3:{}", hasher.finalize().to_hex())
}

pub(crate) fn materialization_key_for_observation(
    observation: &BuildObservation,
) -> Result<String> {
    let (output_contract_digest, _) = build_output_contract_for_observation(observation)?;
    Ok(build_output_materialization_key(
        &observation.input_digest,
        &output_contract_digest,
        &current_platform_profile(),
    ))
}

pub(crate) fn build_output_contract_for_observation(
    observation: &BuildObservation,
) -> Result<(String, Vec<String>)> {
    let outputs = normalize_outputs(&observation.outputs)
        .context("failed to normalize build output contract for materialization lock")?;
    Ok((
        output_contract_digest(&outputs),
        normalized_output_paths(&outputs),
    ))
}

/// Freeze `tree_root` into the local CAS. Caller must already hold the
/// `DerivationLock` for `materialization_key`.
fn freeze_build_output_tree_unlocked(
    tree_root: &Path,
    materialization_key: &str,
) -> Result<String> {
    let tree = hash_tree(tree_root)
        .with_context(|| format!("failed to hash build output tree {}", tree_root.display()))?;
    let blob_hash = tree.blob_hash.clone();
    let address = BlobAddress::parse(&blob_hash)
        .with_context(|| format!("blob hash {blob_hash} could not be parsed"))?;

    let already_frozen = address.payload_dir().is_dir()
        && BlobManifest::read_from(&address.manifest_path())
            .ok()
            .map(|manifest| manifest.matches_blob_hash(&blob_hash))
            .unwrap_or(false);
    if already_frozen {
        return Ok(blob_hash);
    }

    let suffix = format!("{:016x}", rand::random::<u64>());
    let staging = address.staging_dir(&suffix);
    remove_path_if_exists(&staging)?;
    if let Some(parent) = staging.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let staging_payload = staging.join("payload");
    project_payload(tree_root, &staging_payload).with_context(|| {
        format!(
            "failed to copy build output tree into staging {}",
            staging_payload.display()
        )
    })?;
    BlobManifest::from_tree_hash(&tree, materialization_key, chrono::Utc::now().to_rfc3339())
        .write_to(&staging.join("manifest.json"))
        .context("failed to write build output blob manifest")?;

    if let Some(parent) = address.dir().parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    match fs::rename(&staging, address.dir()) {
        Ok(()) => {}
        Err(err) if address.dir().is_dir() => {
            // Another process raced to freeze the same blob. Remove our
            // staging and verify the winning blob is actually valid before
            // claiming success — a corrupt CAS entry must not be recorded.
            let _ = fs::remove_dir_all(&staging);
            tracing::debug!(
                blob_dir = %address.dir().display(),
                "build output blob freeze observed existing target: {err}"
            );
            verify_local_build_output_blob(&blob_hash).with_context(|| {
                format!(
                    "existing build output blob target is present but failed integrity check \
                     after freeze race: {}",
                    address.dir().display()
                )
            })?;
        }
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "failed to rename {} -> {}",
                    staging.display(),
                    address.dir().display()
                )
            });
        }
    }

    Ok(blob_hash)
}

pub(crate) fn verify_local_build_output_blob(blob_hash: &str) -> Result<BlobAddress> {
    let address = BlobAddress::parse(blob_hash)
        .with_context(|| format!("blob hash {blob_hash} could not be parsed"))?;
    let manifest = BlobManifest::read_from(&address.manifest_path())
        .with_context(|| format!("failed to read build output blob manifest for {blob_hash}"))?;
    if !manifest.matches_blob_hash(blob_hash) {
        anyhow::bail!("build output blob manifest hash mismatch for {blob_hash}");
    }
    let actual = hash_tree(&address.payload_dir())
        .with_context(|| format!("failed to verify build output blob {blob_hash}"))?;
    if actual.blob_hash != blob_hash {
        anyhow::bail!(
            "build output blob payload hash mismatch: expected {}, got {}",
            blob_hash,
            actual.blob_hash
        );
    }
    Ok(address)
}

fn copy_declared_outputs(
    working_dir: &Path,
    capture_root: &Path,
    outputs: &[OutputSpec],
) -> Result<()> {
    fs::create_dir_all(capture_root)
        .with_context(|| format!("failed to create {}", capture_root.display()))?;
    for output in outputs {
        let source = working_dir.join(&output.relative_path);
        let target = capture_root.join(&output.relative_path);
        let metadata = validate_output_entry(&source)
            .with_context(|| format!("declared build output {} is missing", source.display()))?;
        if metadata.is_dir() {
            project_payload(&source, &target).with_context(|| {
                format!(
                    "failed to capture build output directory {}",
                    source.display()
                )
            })?;
        } else if metadata.is_file() {
            copy_file(&source, &target)?;
        } else {
            anyhow::bail!("declared build output {} is unsupported", source.display());
        }
    }
    Ok(())
}

fn all_outputs_exist(working_dir: &Path, outputs: &[OutputSpec]) -> bool {
    outputs
        .iter()
        .all(|output| working_dir.join(&output.relative_path).exists())
}

fn output_contract_digest(outputs: &[OutputSpec]) -> String {
    let mut hasher = Hasher::new();
    update_text(&mut hasher, "ato-build-output-contract-v1");
    for output in normalized_output_paths(outputs) {
        update_text(&mut hasher, &output);
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn normalized_output_paths(outputs: &[OutputSpec]) -> Vec<String> {
    let mut paths = outputs
        .iter()
        .map(|output| output.relative_path.display().to_string())
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

fn working_dir(workspace_root: &Path, working_dir_relative: &str) -> PathBuf {
    if working_dir_relative.is_empty() || working_dir_relative == "." {
        workspace_root.to_path_buf()
    } else {
        workspace_root.join(working_dir_relative)
    }
}

fn current_platform_profile() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

fn capture_root(materialization_key: &str) -> PathBuf {
    ato_cache_dir()
        .join("phase-materialization")
        .join("build-output")
        .join(materialization_key_path_component(materialization_key))
        .join(format!("capture-{:016x}", rand::random::<u64>()))
}

pub(crate) fn materialization_key_path_component(value: &str) -> String {
    value.replace(':', "-")
}

fn update_text(hasher: &mut Hasher, value: &str) {
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

fn remove_path_if_exists(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))
        }
        Ok(_) => {
            fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("failed to inspect {}", path.display())),
    }
}

fn copy_file(source: &Path, target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::copy(source, target).with_context(|| {
        format!(
            "failed to copy {} -> {}",
            source.display(),
            target.display()
        )
    })?;
    Ok(())
}

fn project_output_entry(source: &Path, target: &Path, metadata: &fs::Metadata) -> Result<()> {
    if metadata.is_dir() {
        project_payload(source, target).with_context(|| {
            format!(
                "failed to stage build output {} -> {}",
                source.display(),
                target.display()
            )
        })?;
    } else if metadata.is_file() {
        copy_file(source, target)?;
    } else {
        anyhow::bail!(
            "build output layer entry {} is neither a file nor a directory",
            source.display()
        );
    }
    Ok(())
}

fn projection_staging_root(workspace_root: &Path, materialization_key: &str) -> PathBuf {
    workspace_tmp_dir(workspace_root)
        .join("phase-materialization")
        .join("build-output-projection")
        .join(materialization_key_path_component(materialization_key))
        .join(format!("projection-{:016x}", rand::random::<u64>()))
}

fn rollback_committed_outputs(paths: &[PathBuf]) {
    for path in paths.iter().rev() {
        let _ = remove_path_if_exists(path);
    }
}

#[cfg(test)]
type ProjectionCommitHook = Box<dyn Fn(usize, &Path) + Send + Sync>;

#[cfg(test)]
fn projection_commit_hook() -> &'static Mutex<Option<ProjectionCommitHook>> {
    static HOOK: OnceLock<Mutex<Option<ProjectionCommitHook>>> = OnceLock::new();
    HOOK.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
fn run_projection_commit_hook(index: usize, target: &Path) {
    if let Some(hook) = projection_commit_hook().lock().expect("hook lock").as_ref() {
        hook(index, target);
    }
}

#[cfg(not(test))]
fn run_projection_commit_hook(_index: usize, _target: &Path) {}

fn ensure_projection_target_absent(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => anyhow::bail!(
            "build output projection target {} already exists; refusing to replace local output",
            path.display()
        ),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("failed to inspect {}", path.display())),
    }
}

fn validate_output_entry(path: &Path) -> Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    validate_output_metadata(path, &metadata)?;
    if metadata.is_dir() {
        validate_output_tree(path)?;
    }
    Ok(metadata)
}

fn validate_output_tree(root: &Path) -> Result<()> {
    for entry in WalkDir::new(root).min_depth(1).follow_links(false) {
        let entry = entry.with_context(|| format!("failed to walk {}", root.display()))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("failed to inspect {}", path.display()))?;
        validate_output_metadata(path, &metadata)?;
    }
    Ok(())
}

fn validate_output_metadata(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        anyhow::bail!(
            "build output entry {} must not be a symlink",
            path.display()
        );
    }
    if file_type.is_file() {
        reject_hardlinked_file(path, metadata)?;
        return Ok(());
    }
    if file_type.is_dir() {
        return Ok(());
    }
    anyhow::bail!(
        "build output entry {} has unsupported file type; only regular files and directories are supported",
        path.display()
    );
}

#[cfg(unix)]
fn reject_hardlinked_file(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    if metadata.nlink() > 1 {
        anyhow::bail!(
            "build output file {} has multiple hard links; refusing to capture ambiguous local state",
            path.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn reject_hardlinked_file(_path: &Path, _metadata: &fs::Metadata) -> Result<()> {
    Ok(())
}

struct ProjectionPlanEntry {
    source: PathBuf,
    target: PathBuf,
    metadata: fs::Metadata,
}

struct CaptureCleanup(PathBuf);

impl Drop for CaptureCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn materialization_key_changes_with_platform_profile() {
        let contract = "blake3:contract";
        let linux = build_output_materialization_key("blake3:input", contract, "linux-x86_64");
        let macos = build_output_materialization_key("blake3:input", contract, "macos-aarch64");
        assert_ne!(linux, macos);
    }

    #[test]
    #[serial_test::serial]
    fn capture_only_freezes_declared_build_outputs() {
        let home = tempfile::tempdir().expect("home");
        let _guard = TestHomeGuard::set(home.path());
        let workspace = tempfile::tempdir().expect("workspace");
        fs::create_dir_all(workspace.path().join("dist")).expect("dist");
        fs::write(workspace.path().join("dist/app.js"), "built").expect("built");
        fs::write(workspace.path().join("not-declared.txt"), "source").expect("source");

        let observation = observation(vec!["dist".to_string()]);
        let layer = capture_build_outputs(workspace.path(), &observation)
            .expect("capture")
            .expect("layer");
        let second_layer = capture_build_outputs(workspace.path(), &observation)
            .expect("repeat capture")
            .expect("repeat layer");
        let address = BlobAddress::parse(&layer.blob_hash).expect("address");

        assert_eq!(layer.materialization_key, second_layer.materialization_key);
        assert_eq!(layer.blob_hash, second_layer.blob_hash);
        assert!(address.payload_dir().join("dist/app.js").exists());
        assert!(!address.payload_dir().join("not-declared.txt").exists());
    }

    #[test]
    #[serial_test::serial]
    #[cfg(unix)]
    fn capture_rejects_symlink_output_root() {
        let home = tempfile::tempdir().expect("home");
        let _guard = TestHomeGuard::set(home.path());
        let workspace = tempfile::tempdir().expect("workspace");
        let outside = tempfile::tempdir().expect("outside");
        fs::write(outside.path().join("app.js"), "outside").expect("outside output");
        std::os::unix::fs::symlink(outside.path(), workspace.path().join("dist"))
            .expect("output symlink");

        let err = capture_build_outputs(workspace.path(), &observation(vec!["dist".to_string()]))
            .expect_err("symlink output root must fail");

        assert!(format!("{err:#}").contains("must not be a symlink"));
    }

    #[test]
    #[serial_test::serial]
    #[cfg(unix)]
    fn capture_rejects_nested_symlink_output_entry() {
        let home = tempfile::tempdir().expect("home");
        let _guard = TestHomeGuard::set(home.path());
        let workspace = tempfile::tempdir().expect("workspace");
        let outside = tempfile::tempdir().expect("outside");
        fs::create_dir_all(workspace.path().join("dist")).expect("dist");
        fs::write(outside.path().join("app.js"), "outside").expect("outside output");
        std::os::unix::fs::symlink(
            outside.path().join("app.js"),
            workspace.path().join("dist/app.js"),
        )
        .expect("nested output symlink");

        let err = capture_build_outputs(workspace.path(), &observation(vec!["dist".to_string()]))
            .expect_err("nested symlink output must fail");

        assert!(format!("{err:#}").contains("must not be a symlink"));
    }

    #[test]
    #[serial_test::serial]
    #[cfg(unix)]
    fn capture_rejects_hardlinked_output_file() {
        let home = tempfile::tempdir().expect("home");
        let _guard = TestHomeGuard::set(home.path());
        let workspace = tempfile::tempdir().expect("workspace");
        let outside = tempfile::tempdir().expect("outside");
        fs::create_dir_all(workspace.path().join("dist")).expect("dist");
        let shared = outside.path().join("shared.js");
        fs::write(&shared, "shared").expect("shared output");
        fs::hard_link(&shared, workspace.path().join("dist/app.js")).expect("hard link");

        let err = capture_build_outputs(workspace.path(), &observation(vec!["dist".to_string()]))
            .expect_err("hardlinked output must fail");

        assert!(format!("{err:#}").contains("multiple hard links"));
    }

    #[test]
    #[serial_test::serial]
    fn projection_refuses_to_replace_existing_output() {
        let home = tempfile::tempdir().expect("home");
        let _guard = TestHomeGuard::set(home.path());
        let workspace = tempfile::tempdir().expect("workspace");
        fs::create_dir_all(workspace.path().join("dist")).expect("dist");
        fs::write(workspace.path().join("dist/app.js"), "built").expect("built");

        let observation = observation(vec!["dist".to_string()]);
        let layer = capture_build_outputs(workspace.path(), &observation)
            .expect("capture")
            .expect("layer");

        let err = project_build_outputs(workspace.path(), &observation, &layer)
            .expect_err("projection must not replace local output");

        assert!(format!("{err:#}").contains("refusing to replace local output"));
        assert_eq!(
            fs::read_to_string(workspace.path().join("dist/app.js")).expect("local output"),
            "built"
        );
    }

    #[test]
    #[serial_test::serial]
    fn multi_output_projection_rolls_back_committed_outputs_after_later_failure() {
        let home = tempfile::tempdir().expect("home");
        let _guard = TestHomeGuard::set(home.path());
        let workspace = tempfile::tempdir().expect("workspace");
        fs::create_dir_all(workspace.path().join("dist")).expect("dist");
        fs::create_dir_all(workspace.path().join("assets")).expect("assets");
        fs::write(workspace.path().join("dist/app.js"), "built").expect("built");
        fs::write(workspace.path().join("assets/app.css"), "css").expect("css");

        let observation = observation(vec!["dist".to_string(), "assets".to_string()]);
        let layer = capture_build_outputs(workspace.path(), &observation)
            .expect("capture")
            .expect("layer");
        fs::remove_dir_all(workspace.path().join("dist")).expect("remove dist");
        fs::remove_dir_all(workspace.path().join("assets")).expect("remove assets");

        *projection_commit_hook().lock().expect("hook lock") = Some(Box::new(|index, target| {
            if index == 1 {
                fs::create_dir_all(target).expect("create blocker");
                fs::write(target.join("blocker"), "local").expect("write blocker");
            }
        }));
        let err = project_build_outputs(workspace.path(), &observation, &layer)
            .expect_err("second commit must fail");
        *projection_commit_hook().lock().expect("hook lock") = None;

        assert!(format!("{err:#}").contains("refusing to replace local output"));
        assert!(
            !workspace.path().join("dist").exists(),
            "first committed output must be rolled back"
        );
        assert_eq!(
            fs::read_to_string(workspace.path().join("assets/blocker")).expect("blocker"),
            "local"
        );
    }

    #[test]
    #[serial_test::serial]
    fn build_output_lock_serializes_same_materialization_key() {
        let home = tempfile::tempdir().expect("home");
        let _guard = TestHomeGuard::set(home.path());
        let observation = observation(vec!["dist".to_string()]);
        let first =
            acquire_build_output_lock_for_observation(&observation).expect("first lock acquire");
        let (tx, rx) = std::sync::mpsc::channel();
        let observation_for_thread = observation.clone();

        let handle = std::thread::spawn(move || {
            let second = acquire_build_output_lock_for_observation(&observation_for_thread)
                .expect("second lock acquire");
            tx.send(()).expect("send acquired");
            drop(second);
        });

        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(100))
                .is_err(),
            "second lock acquisition must block while first lock is held"
        );
        drop(first);
        rx.recv_timeout(std::time::Duration::from_secs(2))
            .expect("second lock should acquire after first drops");
        handle.join().expect("join lock waiter");
    }

    /// Regression test for Blocker 2: after a freeze race where the target blob
    /// dir already exists, `verify_blob` is called. If the existing dir is
    /// corrupt, capture must fail rather than silently returning `Ok`.
    #[test]
    #[serial_test::serial]
    fn capture_fails_when_existing_blob_dir_is_corrupt() {
        let home = tempfile::tempdir().expect("home");
        let _guard = TestHomeGuard::set(home.path());
        let workspace = tempfile::tempdir().expect("workspace");
        fs::create_dir_all(workspace.path().join("dist")).expect("dist");
        fs::write(workspace.path().join("dist/app.js"), "built").expect("built");

        let observation = observation(vec!["dist".to_string()]);

        // First capture: succeeds and writes the CAS blob.
        let layer = capture_build_outputs(workspace.path(), &observation)
            .expect("first capture")
            .expect("layer");

        let address = BlobAddress::parse(&layer.blob_hash).expect("address");

        // Corrupt the manifest so verify_blob fails.
        let manifest_path = address.manifest_path();
        fs::write(&manifest_path, b"corrupted manifest json").expect("corrupt manifest");

        // Second capture with the same outputs: the rename will race, find the
        // existing (now-corrupt) dir, and verify_blob must cause capture to fail.
        let err = capture_build_outputs(workspace.path(), &observation)
            .expect_err("capture must fail when existing CAS blob is corrupt");

        let msg = format!("{err:#}");
        assert!(
            msg.contains("integrity check") || msg.contains("manifest") || msg.contains("corrupt"),
            "expected integrity-check error, got: {msg}"
        );
    }

    fn observation(outputs: Vec<String>) -> BuildObservation {
        BuildObservation {
            source: crate::application::build_materialization::BuildSpecSource::Declared,
            command: "node build.mjs".to_string(),
            input_digest: "blake3:input".to_string(),
            outputs,
            target: "main".to_string(),
            working_dir_relative: ".".to_string(),
        }
    }

    struct TestHomeGuard {
        // Held for the guard's lifetime: crate convention is that every
        // env-mutating test serializes on the shared env lock.
        _env_lock: std::sync::MutexGuard<'static, ()>,
        prior_home: Option<std::ffi::OsString>,
        prior_userprofile: Option<std::ffi::OsString>,
        prior_ato_home: Option<std::ffi::OsString>,
    }

    impl TestHomeGuard {
        fn set(path: &Path) -> Self {
            let env_lock = crate::tests::env_lock().lock().expect("env lock");
            let prior_home = std::env::var_os("HOME");
            let prior_userprofile = std::env::var_os("USERPROFILE");
            let prior_ato_home = std::env::var_os("ATO_HOME");
            unsafe {
                std::env::set_var("HOME", path);
                // dirs::home_dir() reads USERPROFILE on Windows, not HOME.
                std::env::set_var("USERPROFILE", path);
                // Pin ATO_HOME explicitly so the store these tests freeze
                // blobs into can never resolve to the developer's real
                // ~/.ato regardless of platform home-dir semantics.
                std::env::set_var("ATO_HOME", path.join(".ato"));
            }
            Self {
                _env_lock: env_lock,
                prior_home,
                prior_userprofile,
                prior_ato_home,
            }
        }
    }

    impl Drop for TestHomeGuard {
        fn drop(&mut self) {
            unsafe {
                match self.prior_home.take() {
                    Some(value) => std::env::set_var("HOME", value),
                    None => std::env::remove_var("HOME"),
                }
                match self.prior_userprofile.take() {
                    Some(value) => std::env::set_var("USERPROFILE", value),
                    None => std::env::remove_var("USERPROFILE"),
                }
                match self.prior_ato_home.take() {
                    Some(value) => std::env::set_var("ATO_HOME", value),
                    None => std::env::remove_var("ATO_HOME"),
                }
            }
        }
    }
}
