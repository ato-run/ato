//! `ato workspace` — local orchestration for one multi-app work surface.
//!
//! A workspace is **not** a Capsule: it has no ContractRef and adds no
//! semantic root. It names digest-pinned capsules, a split layout and the
//! view state a person asked to restore, and every pane is loaded and
//! validated as its own Capsule (all declared Ds, then the selected one)
//! before any runtime starts. The runtime shell, Checkpoint/Stop/Resume
//! wiring and the per-app Instances are added by the follow-up increment;
//! this module owns the manifest and checkpoint model they share.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

pub const WORKSPACE_SCHEMA: &str = "ato.workspace/1";
pub const WORKSPACE_CHECKPOINT_SCHEMA: &str = "ato.workspace-checkpoint/1";

/// At most one pane per split side; keeps layout arithmetic and the shell
/// tractable without imposing an app-specific limit.
const MAX_PANES: usize = 8;
const MAX_ID_BYTES: usize = 64;
const MAX_CAPSULE_PATH_BYTES: usize = 1024;
const MAX_INSTANCE_ID_BYTES: usize = 128;
const MAX_RESTORE_HINT_BYTES: usize = 64;
/// Layout weights must sum to one; the tolerance only absorbs float storage.
const SPLIT_SUM_TOLERANCE: f64 = 1e-6;

#[derive(Debug, Args)]
pub struct WorkspaceArgs {
    #[command(subcommand)]
    pub command: WorkspaceCommand,
}

#[derive(Debug, Subcommand)]
pub enum WorkspaceCommand {
    /// Validate a workspace file and every capsule it names; print the plan.
    Inspect(InspectArgs),
    /// Validate a workspace checkpoint file (no runtime is started).
    Checkpoint(CheckpointArgs),
}

#[derive(Debug, Args)]
pub struct InspectArgs {
    /// Workspace file (`ato.workspace/1`).
    pub file: PathBuf,
    /// Emit one JSON object instead of a human-readable summary.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CheckpointArgs {
    /// Workspace checkpoint file (`ato.workspace-checkpoint/1`).
    pub file: PathBuf,
    /// Emit one JSON object instead of a human-readable summary.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceManifestV1 {
    pub schema: String,
    pub layout: WorkspaceLayoutV1,
    pub pane: Vec<WorkspacePaneV1>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceLayoutV1 {
    pub direction: Direction,
    /// One weight per pane, in pane order. Values are positive and sum to 1.
    pub split: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspacePaneV1 {
    /// Pane identity inside this workspace; sorted-unique in checkpoints.
    pub id: String,
    /// Free authoring label (for the shell). It is not a semantic kind and
    /// never selects a runtime path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// Relative path to the pane's `.capsule` file.
    pub capsule: String,
    /// `sha256:` of the exact capsule file bytes (transport equality).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// Declared DerivationRef to run; absent requires exactly one declared D.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derivation: Option<String>,
    /// Declared, restorable view hint (e.g. a scene name). Opaque to Ato.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceCheckpointV1 {
    pub schema: String,
    /// SHA-256 of the workspace file bytes this checkpoint was taken from.
    pub workspace_digest: String,
    pub view: WorkspaceViewStateV1,
    /// One entry per pane, sorted by `pane_id`.
    pub pane: Vec<WorkspaceCheckpointPaneV1>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceViewStateV1 {
    pub direction: Direction,
    pub split: Vec<f64>,
    pub active_pane: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pane: Vec<WorkspacePaneViewStateV1>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspacePaneViewStateV1 {
    pub pane_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore: Option<String>,
    /// App-declared, restorable view value. Ato stores it and never parses it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceCheckpointPaneV1 {
    pub pane_id: String,
    /// Local Instance this pane was started as.
    pub instance_id: String,
    /// `sha256:` of the `InstanceSnapshotV1` this pane saved.
    pub snapshot_ref: String,
    /// Optional receipt of the Run that produced the checkpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_ref: Option<String>,
}

/// One validated pane of an inspected workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectedPane {
    pub pane_id: String,
    pub app: Option<String>,
    pub capsule: PathBuf,
    pub capsule_sha256: String,
    pub contract_ref: String,
    pub derivation_ref: String,
    pub realization: String,
    pub tree_ref: String,
}

/// Result of `ato workspace inspect`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectedWorkspace {
    pub workspace_digest: String,
    pub panes: Vec<InspectedPane>,
}

impl WorkspaceManifestV1 {
    pub fn validate(&self) -> Result<()> {
        if self.schema != WORKSPACE_SCHEMA {
            bail!(
                "unsupported workspace schema `{}`; expected `{WORKSPACE_SCHEMA}`",
                self.schema
            );
        }
        if self.pane.is_empty() || self.pane.len() > MAX_PANES {
            bail!("a workspace declares between 1 and {MAX_PANES} panes");
        }
        validate_split(&self.layout.split, self.pane.len())?;
        let mut ids = BTreeSet::new();
        for pane in &self.pane {
            validate_pane_id(&pane.id)?;
            if !ids.insert(pane.id.as_str()) {
                bail!("duplicate pane id `{}`", pane.id);
            }
            if let Some(app) = &pane.app {
                validate_label(app, "pane app")?;
            }
            validate_relative_path(&pane.capsule)?;
            if let Some(digest) = &pane.digest {
                validate_sha256(digest, "pane digest")?;
            }
            if let Some(derivation) = &pane.derivation {
                validate_sha256(derivation, "pane derivation")?;
            }
            if let Some(restore) = &pane.restore {
                validate_label(restore, "pane restore hint")?;
            }
        }
        Ok(())
    }
}

impl WorkspaceCheckpointV1 {
    pub fn validate(&self) -> Result<()> {
        if self.schema != WORKSPACE_CHECKPOINT_SCHEMA {
            bail!(
                "unsupported workspace checkpoint schema `{}`; expected `{WORKSPACE_CHECKPOINT_SCHEMA}`",
                self.schema
            );
        }
        validate_sha256(&self.workspace_digest, "checkpoint workspace_digest")?;
        validate_split(&self.view.split, self.pane.len())?;
        validate_label(&self.view.active_pane, "checkpoint active pane")?;
        if self.pane.is_empty() || self.pane.len() > MAX_PANES {
            bail!("a workspace checkpoint declares between 1 and {MAX_PANES} panes");
        }
        let mut previous: Option<&str> = None;
        let mut ids = BTreeSet::new();
        for pane in &self.pane {
            validate_pane_id(&pane.pane_id)?;
            if previous.is_some_and(|previous| previous >= pane.pane_id.as_str()) {
                bail!("checkpoint panes must be sorted by unique pane_id");
            }
            previous = Some(&pane.pane_id);
            ids.insert(pane.pane_id.as_str());
            validate_instance_id(&pane.instance_id)?;
            validate_sha256(&pane.snapshot_ref, "checkpoint snapshot_ref")?;
            if let Some(receipt) = &pane.receipt_ref {
                validate_sha256(receipt, "checkpoint receipt_ref")?;
            }
        }
        let mut previous_view: Option<&str> = None;
        for view in &self.view.pane {
            validate_pane_id(&view.pane_id)?;
            if previous_view.is_some_and(|previous| previous >= view.pane_id.as_str()) {
                bail!("checkpoint view entries must be sorted by unique pane_id");
            }
            previous_view = Some(&view.pane_id);
            if !ids.contains(view.pane_id.as_str()) {
                bail!(
                    "checkpoint view names pane `{}` with no checkpoint entry",
                    view.pane_id
                );
            }
            if let Some(restore) = &view.restore {
                validate_label(restore, "checkpoint restore hint")?;
            }
            if view.state.as_ref().is_some_and(|state| state.is_null()) {
                bail!("checkpoint pane state must not be null; omit it instead");
            }
        }
        Ok(())
    }
}

fn validate_split(split: &[f64], pane_count: usize) -> Result<()> {
    if split.len() != pane_count {
        bail!(
            "layout split declares {} weights for {pane_count} panes",
            split.len()
        );
    }
    if split
        .iter()
        .any(|weight| !weight.is_finite() || *weight <= 0.0)
    {
        bail!("layout split weights must be finite and positive");
    }
    let sum: f64 = split.iter().sum();
    if !(1.0 - SPLIT_SUM_TOLERANCE..=1.0 + SPLIT_SUM_TOLERANCE).contains(&sum) {
        bail!("layout split weights must sum to 1 (got {sum})");
    }
    Ok(())
}

fn validate_pane_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > MAX_ID_BYTES {
        bail!("pane id must be 1..={MAX_ID_BYTES} bytes");
    }
    if !id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        bail!("pane id `{id}` contains unsupported characters");
    }
    Ok(())
}

fn validate_label(value: &str, field: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_RESTORE_HINT_BYTES {
        bail!("{field} must be 1..={MAX_RESTORE_HINT_BYTES} bytes");
    }
    if value.bytes().any(|byte| byte.is_ascii_control()) {
        bail!("{field} must not contain control characters");
    }
    Ok(())
}

fn validate_instance_id(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_INSTANCE_ID_BYTES {
        bail!("checkpoint instance_id must be 1..={MAX_INSTANCE_ID_BYTES} bytes");
    }
    if value
        .bytes()
        .any(|byte| byte.is_ascii_control() || byte == b'/')
    {
        bail!("checkpoint instance_id contains unsupported characters");
    }
    Ok(())
}

fn validate_sha256(value: &str, field: &str) -> Result<()> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        bail!("{field} must be `sha256:<64 lowercase hex>`");
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("{field} must be `sha256:<64 lowercase hex>`");
    }
    Ok(())
}

/// A workspace pane's capsule path is relative to the workspace file and may
/// not escape it. No absolute paths, no `.`/`..`, no NUL.
fn validate_relative_path(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_CAPSULE_PATH_BYTES {
        bail!("pane capsule path must be 1..={MAX_CAPSULE_PATH_BYTES} bytes");
    }
    if value.contains('\0') || value.contains('\\') {
        bail!("pane capsule path must be a relative path without NUL or backslashes");
    }
    let path = Path::new(value);
    if path.is_absolute() {
        bail!("pane capsule path must be relative to the workspace file");
    }
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            _ => bail!("pane capsule path must not contain `.` or `..` components"),
        }
    }
    Ok(())
}

pub fn parse_manifest(text: &str) -> Result<WorkspaceManifestV1> {
    let manifest: WorkspaceManifestV1 =
        toml::from_str(text).context("workspace file is not valid ato.workspace/1 TOML")?;
    manifest.validate()?;
    Ok(manifest)
}

pub fn parse_checkpoint(text: &str) -> Result<WorkspaceCheckpointV1> {
    let checkpoint: WorkspaceCheckpointV1 = toml::from_str(text)
        .context("workspace checkpoint is not valid ato.workspace-checkpoint/1 TOML")?;
    checkpoint.validate()?;
    Ok(checkpoint)
}

/// Load, validate and inspect a workspace: every pane's capsule is read,
/// digest-checked when declared, decoded as a contract-root bundle, validated
/// for **all** declared Ds, and the selected D is validated explicitly.
pub fn inspect(file: &Path) -> Result<InspectedWorkspace> {
    let text = std::fs::read_to_string(file)
        .with_context(|| format!("failed to read workspace {}", file.display()))?;
    let manifest = parse_manifest(&text)?;
    let base = file.parent().unwrap_or_else(|| Path::new("."));
    let workspace_digest = sha256_hex(text.as_bytes());

    let mut panes = Vec::with_capacity(manifest.pane.len());
    for pane in &manifest.pane {
        let capsule_path = base.join(&pane.capsule);
        let bytes = std::fs::read(&capsule_path).with_context(|| {
            format!(
                "pane `{}` capsule {} is not readable",
                pane.id,
                capsule_path.display()
            )
        })?;
        let capsule_sha256 = sha256_hex(&bytes);
        if let Some(expected) = &pane.digest
            && *expected != capsule_sha256
        {
            bail!(
                "pane `{}` capsule digest mismatch: the workspace pins {expected}, the file is {capsule_sha256}",
                pane.id
            );
        }
        match ato_objects::decode_capsule_bundle_document(&bytes) {
            Ok(ato_objects::CapsuleBundleDocument::PortableApplicationV3(_))
            | Ok(ato_objects::CapsuleBundleDocument::PortableApplicationV4(_)) => {}
            Ok(ato_objects::CapsuleBundleDocument::ComputationV2(_)) => bail!(
                "pane `{}` is computation-root wire v2; a workspace pane must be a contract-root v3/v4 Capsule",
                pane.id
            ),
            Err(error) => bail!("pane `{}` is not a valid Capsule bundle: {error}", pane.id),
        }
        let (_, validated) = ato_portable_application::validate_bytes_all(&bytes)
            .with_context(|| format!("pane `{}` failed all-Derivation validation", pane.id))?;
        let selected = match pane.derivation.as_deref() {
            Some(reference) => {
                ato_portable_application::validate_bytes_for_derivation(&bytes, reference)
                    .with_context(|| format!("pane `{}` selected Derivation was refused", pane.id))?
                    .1
            }
            None => {
                if validated.len() != 1 {
                    bail!(
                        "pane `{}` declares {} Derivations; set `derivation` to select one",
                        pane.id,
                        validated.len()
                    );
                }
                validated.into_iter().next().expect("one validated route")
            }
        };
        panes.push(InspectedPane {
            pane_id: pane.id.clone(),
            app: pane.app.clone(),
            capsule: capsule_path,
            capsule_sha256,
            contract_ref: selected.contract_ref.to_string(),
            derivation_ref: selected.derivation_ref.to_string(),
            realization: realization_label(selected.realization).to_owned(),
            tree_ref: selected.tree_ref.to_string(),
        });
    }
    Ok(InspectedWorkspace {
        workspace_digest,
        panes,
    })
}

fn realization_label(
    realization: ato_portable_application::PortableRealizationKind,
) -> &'static str {
    use ato_portable_application::PortableRealizationKind as Kind;
    match realization {
        Kind::StaticWeb => "static_web",
        Kind::LocalProcess => "local_process",
        Kind::OciContainer => "oci_container",
        Kind::OciServiceGroup => "oci_service_group",
        Kind::ProcessServiceGroup => "process_service_group",
        Kind::NativeDesktop => "native_desktop",
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

/// Read and validate a checkpoint file.
pub fn read_checkpoint(path: &Path) -> Result<WorkspaceCheckpointV1> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read workspace checkpoint {}", path.display()))?;
    parse_checkpoint(&text)
}

/// Write a checkpoint atomically: same-directory temporary file, fsync, rename.
/// A failed write never leaves a partial checkpoint at the destination.
///
/// Used by the runtime wiring (next increment) and by the tests here.
#[allow(dead_code)]
pub fn write_checkpoint_atomic(path: &Path, checkpoint: &WorkspaceCheckpointV1) -> Result<()> {
    checkpoint.validate()?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        bail!("checkpoint directory {} does not exist", parent.display());
    }
    let text = toml::to_string(checkpoint).context("failed to encode workspace checkpoint")?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".ato-workspace-checkpoint-")
        .tempfile_in(parent)
        .with_context(|| {
            format!(
                "failed to create a temporary checkpoint in {}",
                parent.display()
            )
        })?;
    use std::io::Write as _;
    temporary
        .write_all(text.as_bytes())
        .context("failed to write the temporary checkpoint")?;
    temporary
        .as_file()
        .sync_all()
        .context("failed to fsync the temporary checkpoint")?;
    temporary.persist(path).map_err(|error| {
        anyhow::anyhow!("failed to move the checkpoint into place: {}", error.error)
    })?;
    Ok(())
}

pub fn run(args: WorkspaceArgs) -> Result<()> {
    match args.command {
        WorkspaceCommand::Inspect(args) => {
            let inspected = inspect(&args.file)?;
            if args.json {
                let panes = inspected
                    .panes
                    .iter()
                    .map(|pane| {
                        json!({
                            "pane_id": pane.pane_id,
                            "app": pane.app,
                            "capsule": pane.capsule.display().to_string(),
                            "capsule_sha256": pane.capsule_sha256,
                            "contract_ref": pane.contract_ref,
                            "derivation_ref": pane.derivation_ref,
                            "realization": pane.realization,
                            "tree_ref": pane.tree_ref,
                        })
                    })
                    .collect::<Vec<_>>();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "workspace_digest": inspected.workspace_digest,
                        "panes": panes,
                    }))?
                );
            } else {
                println!("workspace: {}", inspected.workspace_digest);
                for pane in &inspected.panes {
                    println!(
                        "pane {} ({}): {} · K {} · D {} · {}",
                        pane.pane_id,
                        pane.app.as_deref().unwrap_or("-"),
                        pane.capsule.display(),
                        pane.contract_ref,
                        pane.derivation_ref,
                        pane.realization
                    );
                }
            }
            Ok(())
        }
        WorkspaceCommand::Checkpoint(args) => {
            let checkpoint = read_checkpoint(&args.file)?;
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "schema": checkpoint.schema,
                        "workspace_digest": checkpoint.workspace_digest,
                        "active_pane": checkpoint.view.active_pane,
                        "panes": checkpoint.pane.iter().map(|pane| json!({
                            "pane_id": pane.pane_id,
                            "instance_id": pane.instance_id,
                            "snapshot_ref": pane.snapshot_ref,
                            "receipt_ref": pane.receipt_ref,
                        })).collect::<Vec<_>>(),
                    }))?
                );
            } else {
                println!(
                    "checkpoint: {} · workspace {} · active pane {} · {} panes",
                    args.file.display(),
                    checkpoint.workspace_digest,
                    checkpoint.view.active_pane,
                    checkpoint.pane.len()
                );
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_with(split: &[f64], panes: &[(&str, &str)]) -> String {
        let mut text = format!(
            "schema = \"{WORKSPACE_SCHEMA}\"\n\n[layout]\ndirection = \"horizontal\"\nsplit = [{}]\n",
            split
                .iter()
                .map(|weight| weight.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        for (id, capsule) in panes {
            text.push_str(&format!(
                "\n[[pane]]\nid = \"{id}\"\ncapsule = \"{capsule}\"\n"
            ));
        }
        text
    }

    #[test]
    fn a_valid_manifest_parses_and_keeps_pane_order() {
        let text = manifest_with(
            &[0.6, 0.4],
            &[("board", "a.capsule"), ("calendar", "b.capsule")],
        );
        let manifest = parse_manifest(&text).unwrap();
        assert_eq!(manifest.layout.direction, Direction::Horizontal);
        assert_eq!(
            manifest
                .pane
                .iter()
                .map(|pane| pane.id.as_str())
                .collect::<Vec<_>>(),
            ["board", "calendar"]
        );
    }

    #[test]
    fn split_weights_must_match_panes_and_sum_to_one() {
        let wrong_count = manifest_with(&[1.0], &[("a", "a.capsule"), ("b", "b.capsule")]);
        assert!(
            parse_manifest(&wrong_count)
                .unwrap_err()
                .to_string()
                .contains("weights for")
        );
        let wrong_sum = manifest_with(&[0.5, 0.4], &[("a", "a.capsule"), ("b", "b.capsule")]);
        assert!(
            parse_manifest(&wrong_sum)
                .unwrap_err()
                .to_string()
                .contains("sum to 1")
        );
        let zero = manifest_with(&[1.0, 0.0], &[("a", "a.capsule"), ("b", "b.capsule")]);
        assert!(
            parse_manifest(&zero)
                .unwrap_err()
                .to_string()
                .contains("positive")
        );
    }

    #[test]
    fn pane_ids_are_unique_and_paths_stay_relative() {
        let duplicate = manifest_with(&[0.5, 0.5], &[("dup", "a.capsule"), ("dup", "b.capsule")]);
        assert!(
            parse_manifest(&duplicate)
                .unwrap_err()
                .to_string()
                .contains("duplicate pane id")
        );
        for path in [
            "/etc/passwd",
            "../a.capsule",
            "./a.capsule",
            "a/../b.capsule",
        ] {
            let text = manifest_with(&[1.0], &[("a", path)]);
            assert!(parse_manifest(&text).is_err(), "{path} must be refused");
        }
    }

    #[test]
    fn unknown_fields_and_bad_digests_fail_closed() {
        let mut text = manifest_with(&[1.0], &[("a", "a.capsule")]);
        text.push_str("surprise = true\n");
        assert!(
            parse_manifest(&text)
                .unwrap_err()
                .to_string()
                .contains("TOML")
        );
        let digest = manifest_with(&[1.0], &[("a", "a.capsule")]).replace(
            "capsule = \"a.capsule\"",
            "capsule = \"a.capsule\"\ndigest = \"sha256:XYZ\"",
        );
        assert!(
            parse_manifest(&digest)
                .unwrap_err()
                .to_string()
                .contains("sha256")
        );
    }

    #[test]
    fn checkpoint_round_trips_atomically_and_requires_sorted_panes() {
        let checkpoint = WorkspaceCheckpointV1 {
            schema: WORKSPACE_CHECKPOINT_SCHEMA.to_owned(),
            workspace_digest: sha256_hex(b"workspace"),
            view: WorkspaceViewStateV1 {
                direction: Direction::Vertical,
                split: vec![0.5, 0.5],
                active_pane: "board".to_owned(),
                pane: vec![WorkspacePaneViewStateV1 {
                    pane_id: "board".to_owned(),
                    restore: Some("scene".to_owned()),
                    state: Some(json!({"zoom": 1.5})),
                }],
            },
            pane: vec![
                WorkspaceCheckpointPaneV1 {
                    pane_id: "board".to_owned(),
                    instance_id: "instance-1".to_owned(),
                    snapshot_ref: sha256_hex(b"snapshot"),
                    receipt_ref: None,
                },
                WorkspaceCheckpointPaneV1 {
                    pane_id: "todo".to_owned(),
                    instance_id: "instance-2".to_owned(),
                    snapshot_ref: sha256_hex(b"snapshot-2"),
                    receipt_ref: Some(sha256_hex(b"receipt")),
                },
            ],
        };
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("checkpoint.toml");
        write_checkpoint_atomic(&path, &checkpoint).unwrap();
        let read = read_checkpoint(&path).unwrap();
        assert_eq!(read, checkpoint);
        assert!(!path.with_file_name("checkpoint.toml.tmp").exists());
        assert!(!std::fs::read_dir(directory.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".ato-workspace-checkpoint-")
        }));

        let mut unsorted = checkpoint.clone();
        unsorted.pane.swap(0, 1);
        assert!(
            unsorted
                .validate()
                .unwrap_err()
                .to_string()
                .contains("sorted")
        );
        let mut bad_view = checkpoint.clone();
        bad_view.view.pane[0].pane_id = "missing".to_owned();
        assert!(
            bad_view
                .validate()
                .unwrap_err()
                .to_string()
                .contains("no checkpoint entry")
        );
    }
}
