//! `ato workspace` — local orchestration for one multi-app work surface.
//!
//! A workspace is **not** a Capsule: it has no ContractRef and adds no
//! semantic root. It names digest-pinned capsules, a split layout and the
//! view state a person asked to restore, and every pane is loaded and
//! validated as its own Capsule (all declared Ds, then the selected one)
//! before any runtime starts. The runtime shell, Checkpoint/Stop/Resume
//! wiring and the per-app Instances are added by the follow-up increment;
//! this module owns the manifest and checkpoint model they share.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
#[cfg(unix)]
use std::time::Duration;

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
    /// Start the workspace shell and its pane servers on loopback.
    ///
    /// Ctrl-C stops the workspace; with `--checkpoint-out` the pane state and
    /// the shell view state are captured first. `--checkpoint` resumes a
    /// previous capture (same workspace file required).
    Open(OpenArgs),
}

#[derive(Debug, Args)]
pub struct OpenArgs {
    /// Workspace file (`ato.workspace/1`).
    pub file: PathBuf,
    /// Resume from this workspace checkpoint.
    #[arg(long)]
    pub checkpoint: Option<PathBuf>,
    /// Write the next checkpoint here on stop.
    #[arg(long)]
    pub checkpoint_out: Option<PathBuf>,
    /// Print the shell URL without opening a browser.
    #[arg(long)]
    pub no_open: bool,
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
    /// The canonical `ato.portable-instance-snapshot/1` document this pane
    /// saved. It carries one `ato.browser-instance-state@1` resource whose
    /// bytes are stored beside the checkpoint. Local orchestration data,
    /// never K and never a Receipt.
    pub snapshot: serde_json::Value,
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
            validate_captured_snapshot(&pane.snapshot, &pane.snapshot_ref)?;
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

type BrowserState = BTreeMap<String, String>;

/// Build the canonical `InstanceSnapshotV1` document for one pane's browser
/// state. The returned object bytes are the resource this snapshot names; the
/// snapshot bytes give the `snapshot_ref` the checkpoint records.
fn browser_snapshot(local_storage: &BrowserState) -> Result<(serde_json::Value, Vec<u8>, String)> {
    let encoded = ato_portable_application::instance_snapshot::encode_browser_state(local_storage)
        .context("failed to encode the pane's browser state")?;
    let content_ref = sha256_hex(&encoded);
    let snapshot = json!({
        "schema": ato_portable_application::instance_snapshot::INSTANCE_SNAPSHOT_SCHEMA,
        "resources": [{
            "slot": "browser",
            "protocol": ato_portable_application::instance_snapshot::BROWSER_INSTANCE_STATE_PROTOCOL,
            "content_ref": content_ref,
        }],
        "assets": [],
    });
    let snapshot_bytes =
        serde_jcs::to_vec(&snapshot).context("failed to canonicalize the pane snapshot")?;
    let snapshot_ref = sha256_hex(&snapshot_bytes);
    Ok((snapshot, encoded, snapshot_ref))
}

/// Build the snapshot document and its reference for a browser-state map.
/// Used by the runtime wiring and by tests that write checkpoint fixtures.
pub fn browser_snapshot_for(
    local_storage: &BTreeMap<String, String>,
) -> Result<(serde_json::Value, String)> {
    browser_snapshot(local_storage).map(|(snapshot, _object, reference)| (snapshot, reference))
}

fn validate_captured_snapshot(snapshot: &serde_json::Value, snapshot_ref: &str) -> Result<()> {
    let typed: ato_portable_application::instance_snapshot::InstanceSnapshotV1 =
        serde_json::from_value(snapshot.clone())
            .context("checkpoint pane snapshot must be a valid InstanceSnapshotV1")?;
    if typed.schema != ato_portable_application::instance_snapshot::INSTANCE_SNAPSHOT_SCHEMA {
        bail!("checkpoint pane snapshot schema must be `ato.portable-instance-snapshot/1`");
    }
    if typed.resources.len() != 1
        || typed.resources[0].protocol
            != ato_portable_application::instance_snapshot::BROWSER_INSTANCE_STATE_PROTOCOL
    {
        bail!("checkpoint pane snapshot must carry exactly one browser instance-state resource");
    }
    if typed.resources[0].slot.is_empty()
        || !typed.resources[0]
            .slot
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("checkpoint pane snapshot resource slot is not a valid slot");
    }
    validate_sha256(&typed.resources[0].content_ref, "snapshot content_ref")?;
    if !typed.assets.is_empty() || !typed.asset_bindings.is_empty() {
        bail!("workspace checkpoint v0 snapshots carry no assets");
    }
    let canonical =
        serde_jcs::to_vec(snapshot).context("failed to canonicalize the pane snapshot")?;
    if sha256_hex(&canonical) != snapshot_ref {
        bail!("checkpoint snapshot_ref does not match its snapshot bytes");
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
        let loaded = load_pane_capsule(base, pane)?;
        panes.push(InspectedPane {
            pane_id: pane.id.clone(),
            app: pane.app.clone(),
            capsule: loaded.capsule_path,
            capsule_sha256: loaded.capsule_sha256,
            contract_ref: loaded.selected.contract_ref.to_string(),
            derivation_ref: loaded.selected.derivation_ref.to_string(),
            realization: realization_label(loaded.selected.realization).to_owned(),
            tree_ref: loaded.selected.tree_ref.to_string(),
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

fn wait_for_shutdown() -> Result<()> {
    #[cfg(unix)]
    {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        let flag = Arc::new(AtomicBool::new(false));
        signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&flag))?;
        signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&flag))?;
        while !flag.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        // v0: a line on stdin is the stop signal on platforms where the
        // signal flag cannot be registered yet.
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        Ok(())
    }
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

/// Write the objects a checkpoint names, then the checkpoint itself.
///
/// Objects go first: the checkpoint is the commit point, so it never references
/// bytes that were not written. Each object is written atomically in the same
/// directory and verified by its `sha256` name.
pub fn save_workspace_checkpoint(
    path: &Path,
    running: &RunningWorkspace,
) -> Result<WorkspaceCheckpointV1> {
    let (checkpoint, objects) = running.capture()?;
    let directory = checkpoint_state_directory(path);
    std::fs::create_dir_all(&directory).with_context(|| {
        format!(
            "failed to create the checkpoint state directory {}",
            directory.display()
        )
    })?;
    for (reference, bytes) in &objects {
        let hex = reference
            .strip_prefix("sha256:")
            .with_context(|| format!("snapshot object reference `{reference}` is not sha256"))?;
        let mut temporary = tempfile::Builder::new()
            .prefix(".ato-workspace-object-")
            .tempfile_in(&directory)
            .context("failed to create a temporary snapshot object")?;
        use std::io::Write as _;
        temporary
            .write_all(bytes)
            .context("failed to write a snapshot object")?;
        temporary
            .as_file()
            .sync_all()
            .context("failed to fsync a snapshot object")?;
        temporary.persist(directory.join(hex)).map_err(|error| {
            anyhow::anyhow!(
                "failed to move the snapshot object into place: {}",
                error.error
            )
        })?;
    }
    write_checkpoint_atomic(path, &checkpoint)?;
    Ok(checkpoint)
}

/// Read a checkpoint and the snapshot objects it names, verifying every digest.
pub fn load_workspace_checkpoint(path: &Path) -> Result<LoadedWorkspaceCheckpoint> {
    let checkpoint = read_checkpoint(path)?;
    let directory = checkpoint_state_directory(path);
    let mut state = BTreeMap::new();
    for pane in &checkpoint.pane {
        let reference = snapshot_content_ref(&pane.snapshot)?;
        let hex = reference.strip_prefix("sha256:").with_context(|| {
            format!("pane `{}` snapshot content_ref is not sha256", pane.pane_id)
        })?;
        let object_path = directory.join(hex);
        let bytes = std::fs::read(&object_path).with_context(|| {
            format!(
                "pane `{}` snapshot object {} is missing",
                pane.pane_id,
                object_path.display()
            )
        })?;
        if sha256_hex(&bytes) != reference {
            bail!(
                "pane `{}` snapshot object does not match its content_ref",
                pane.pane_id
            );
        }
        let storage = ato_portable_application::instance_snapshot::decode_browser_state(&bytes)
            .with_context(|| {
                format!(
                    "pane `{}` snapshot object is not browser state",
                    pane.pane_id
                )
            })?;
        state.insert(pane.pane_id.clone(), storage);
    }
    Ok(LoadedWorkspaceCheckpoint { checkpoint, state })
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
        WorkspaceCommand::Open(args) => {
            let checkpoint = args
                .checkpoint
                .as_deref()
                .map(load_workspace_checkpoint)
                .transpose()?;
            let running = start(&args.file, checkpoint.as_ref())?;
            println!("workspace: {}", running.shell_url);
            if !args.no_open {
                crate::open_browser(&running.shell_url)?;
            }
            if args.checkpoint_out.is_some() {
                println!("edit the panes, then press Ctrl-C to checkpoint and stop");
            } else {
                println!("press Ctrl-C to stop (no --checkpoint-out was given)");
            }
            wait_for_shutdown()?;
            if let Some(path) = args.checkpoint_out.as_deref() {
                let checkpoint = save_workspace_checkpoint(path, &running)?;
                println!(
                    "checkpoint: {} ({} panes)",
                    path.display(),
                    checkpoint.pane.len()
                );
            }
            drop(running);
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

const VIEW_STATE_KEY: &str = "ato.workspace.view";

/// The shell's own view state: layout and which pane was active. It is stored
/// in the shell origin's localStorage (through the static server's state
/// bridge), never in a Capsule's K.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellViewState {
    pub direction: Direction,
    pub split: Vec<f64>,
    pub active_pane: String,
}

impl ShellViewState {
    fn for_manifest(manifest: &WorkspaceManifestV1) -> Self {
        Self {
            direction: manifest.layout.direction,
            split: manifest.layout.split.clone(),
            active_pane: manifest.pane[0].id.clone(),
        }
    }
}

struct LoadedPaneCapsule {
    capsule_path: PathBuf,
    capsule_sha256: String,
    bundle: ato_objects::PortableApplicationBundle,
    selected: ato_portable_application::ValidatedPortableApplication,
}

fn load_pane_capsule(base: &Path, pane: &WorkspacePaneV1) -> Result<LoadedPaneCapsule> {
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
    let bundle = match ato_objects::decode_capsule_bundle_document(&bytes) {
        Ok(ato_objects::CapsuleBundleDocument::PortableApplicationV3(bundle))
        | Ok(ato_objects::CapsuleBundleDocument::PortableApplicationV4(bundle)) => bundle,
        _ => unreachable!("the document was already dispatched"),
    };
    Ok(LoadedPaneCapsule {
        capsule_path,
        capsule_sha256,
        bundle,
        selected,
    })
}

/// One running pane: its static server and the token the state bridge uses.
pub struct WorkspacePaneRuntime {
    pub pane_id: String,
    pub server: ato_runtime_attempt::static_server::StaticApplicationServer,
    persistence_token: String,
}

/// A running workspace shell and its pane servers.
pub struct RunningWorkspace {
    pub shell: ato_runtime_attempt::static_server::StaticApplicationServer,
    pub panes: Vec<WorkspacePaneRuntime>,
    pub shell_url: String,
    pub workspace_digest: String,
    shell_persistence_token: String,
    manifest: WorkspaceManifestV1,
    _runtime_root: tempfile::TempDir,
}

fn random_token() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).context("failed to generate a workspace state token")?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn pane_routes(
    destination: &Path,
    selected: &ato_portable_application::ValidatedPortableApplication,
) -> Result<BTreeMap<String, (PathBuf, String)>> {
    let mut routes = BTreeMap::new();
    for entry in &selected.tree.entries {
        routes.insert(
            format!("/{}", entry.path),
            (destination.join(&entry.path), entry.media_type.clone()),
        );
    }
    if routes.is_empty() {
        bail!("a pane's static tree is empty");
    }
    Ok(routes)
}

fn pane_entry_route(selected: &ato_portable_application::ValidatedPortableApplication) -> String {
    selected
        .application
        .surfaces
        .first()
        .and_then(|surface| surface.entry.clone())
        .map(|entry| format!("/{entry}"))
        .unwrap_or_else(|| "/index.html".to_owned())
}

fn pane_spa_fallback(selected: &ato_portable_application::ValidatedPortableApplication) -> bool {
    selected
        .application
        .surfaces
        .first()
        .and_then(|surface| surface.spa_fallback)
        .unwrap_or(false)
}

/// A checkpoint whose snapshot resources were read and verified.
#[derive(Debug, Clone)]
pub struct LoadedWorkspaceCheckpoint {
    pub checkpoint: WorkspaceCheckpointV1,
    /// Resumed per-pane browser state, keyed by pane id.
    pub state: BTreeMap<String, BrowserState>,
}

fn checkpoint_state_directory(checkpoint: &Path) -> PathBuf {
    let name = checkpoint
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "checkpoint".to_owned());
    checkpoint.with_file_name(format!("{name}.state"))
}

fn snapshot_content_ref(snapshot: &serde_json::Value) -> Result<&str> {
    snapshot
        .get("resources")
        .and_then(serde_json::Value::as_array)
        .and_then(|resources| resources.first())
        .and_then(|resource| resource.get("content_ref"))
        .and_then(serde_json::Value::as_str)
        .context("checkpoint pane snapshot has no browser resource content_ref")
}

fn pane_state<'a>(
    checkpoint: Option<&'a LoadedWorkspaceCheckpoint>,
    pane_id: &str,
) -> &'a BrowserState {
    static EMPTY: std::sync::OnceLock<BrowserState> = std::sync::OnceLock::new();
    checkpoint
        .and_then(|checkpoint| checkpoint.state.get(pane_id))
        .unwrap_or_else(|| EMPTY.get_or_init(BTreeMap::new))
}

fn shell_html(
    manifest: &WorkspaceManifestV1,
    pane_urls: &BTreeMap<String, String>,
    view: &ShellViewState,
) -> Result<String> {
    let direction = match view.direction {
        Direction::Horizontal => "row",
        Direction::Vertical => "column",
    };
    let mut panes = String::new();
    let mut dividers = String::new();
    for (index, pane) in manifest.pane.iter().enumerate() {
        let url = pane_urls
            .get(&pane.id)
            .with_context(|| format!("shell has no URL for pane `{}`", pane.id))?;
        let title = pane.app.as_deref().unwrap_or(&pane.id);
        let active = if view.active_pane == pane.id {
            " active"
        } else {
            ""
        };
        panes.push_str(&format!(
            r#"<section class="pane{active}" data-pane="{id}" style="flex:{weight} 1 0"><header>{title}</header><iframe title="{title}" src="{url}/"></iframe></section>"#,
            id = html_escape(&pane.id),
            weight = view.split[index],
            title = html_escape(title),
            url = html_escape(url),
        ));
        if index + 1 < manifest.pane.len() {
            dividers.push_str(&format!(
                r#"<div class="divider" data-left="{index}" data-right="{next}"></div>"#,
                index = index,
                next = index + 1,
            ));
        }
    }
    // Interleave panes and dividers: pane, divider, pane, divider, pane.
    let mut body = String::new();
    let mut divider_iter = dividers.split_inclusive("/>").peekable();
    let pane_list: Vec<&str> = panes.split_inclusive("</section>").collect();
    for (index, pane) in pane_list.iter().enumerate() {
        body.push_str(pane);
        if index + 1 < pane_list.len()
            && let Some(divider) = divider_iter.next()
        {
            body.push_str(divider);
        }
    }
    let view_json = serde_json::to_string(view).context("failed to encode the shell view state")?;
    let weights_json = serde_json::to_string(&view.split).context("failed to encode the split")?;
    Ok(format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<title>ato workspace</title>
<style>
html, body {{ height: 100%; margin: 0; font: 13px/1.4 -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif; }}
body {{ display: flex; flex-direction: column; background: #f2f4f6; }}
header.bar {{ padding: 0.5rem 0.8rem; border-bottom: 1px solid #e6e8ea; color: #4a4f54; background: #fff; }}
main {{ flex: 1; min-height: 0; display: flex; flex-direction: {direction}; gap: 0; }}
section.pane {{ min-width: 0; min-height: 0; display: flex; flex-direction: column; background: #fff; border: 1px solid #e0e3e6; border-radius: 8px; overflow: hidden; margin: 6px; }}
section.pane.active {{ border-color: #1f6feb; }}
section.pane header {{ padding: 0.35rem 0.6rem; border-bottom: 1px solid #eef0f2; color: #6b7075; }}
iframe {{ flex: 1; width: 100%; border: 0; }}
divider, .divider {{ flex: 0 0 6px; cursor: {cursor}; background: #dfe3e7; }}
</style>
</head>
<body>
<header class="bar">ato workspace — drag a divider to change the split; press Ctrl-C to stop</header>
<main id="split" data-direction="{direction}">{body}</main>
<script>
(function () {{
  var key = "{key}";
  var fallback = {view_json};
  var fallbackSplit = {weights_json};
  var current = fallback;
  try {{
    var stored = localStorage.getItem(key);
    if (stored) {{
      var parsed = JSON.parse(stored);
      if (parsed && Array.isArray(parsed.split) && parsed.split.length === fallbackSplit.length) current = parsed;
    }}
  }} catch (_) {{}}
  if (current.active_pane !== fallback.active_pane) current.active_pane = fallback.active_pane;
  var root = document.getElementById("split");
  var sections = function () {{ return [].slice.call(root.querySelectorAll("section.pane")); }};
  function apply() {{
    sections().forEach(function (pane, index) {{
      var weight = current.split[index] || fallbackSplit[index];
      pane.style.flex = weight + " 1 0";
    }});
  }}
  function persist() {{
    try {{ localStorage.setItem(key, JSON.stringify(current)); }} catch (_) {{}}
  }}
  apply();
  persist();
  root.querySelectorAll("section.pane").forEach(function (pane) {{
    pane.addEventListener("pointerdown", function () {{
      current.active_pane = pane.dataset.pane;
      sections().forEach(function (other) {{ other.classList.toggle("active", other === pane); }});
      persist();
    }});
  }});
  var horizontal = root.dataset.direction === "row";
  root.querySelectorAll(".divider").forEach(function (divider) {{
    divider.addEventListener("pointerdown", function (event) {{
      var left = Number(divider.dataset.left);
      var right = Number(divider.dataset.right);
      var paneNodes = sections();
      var size = horizontal ? root.clientWidth : root.clientHeight;
      var start = horizontal ? event.clientX : event.clientY;
      var before = current.split.slice();
      function move(moveEvent) {{
        var delta = ((horizontal ? moveEvent.clientX : moveEvent.clientY) - start) / size;
        var leftWeight = Math.max(0.05, before[left] + delta);
        var rightWeight = Math.max(0.05, before[right] - delta);
        var total = before[left] + before[right];
        if (leftWeight + rightWeight !== total) {{
          rightWeight = total - leftWeight;
        }}
        var next = before.slice();
        next[left] = leftWeight;
        next[right] = rightWeight;
        current.split = next;
        paneNodes[left].style.flex = next[left] + " 1 0";
        paneNodes[right].style.flex = next[right] + " 1 0";
      }}
      function up() {{
        persist();
        window.removeEventListener("pointermove", move);
        window.removeEventListener("pointerup", up);
      }}
      window.addEventListener("pointermove", move);
      window.addEventListener("pointerup", up);
      event.preventDefault();
    }});
  }});
}})();
</script>
</body>
</html>"#,
        direction = direction,
        cursor = if matches!(view.direction, Direction::Horizontal) {
            "col-resize"
        } else {
            "row-resize"
        },
        body = body,
        key = VIEW_STATE_KEY,
    ))
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Start every pane and the workspace shell on loopback.
///
/// Pure local orchestration: each pane is materialized from its own verified
/// tree, served on its own origin (so its browser state is separate), and the
/// shell is a page with one iframe per pane. No Capsule identity is created or
/// changed.
pub fn start(
    file: &Path,
    checkpoint: Option<&LoadedWorkspaceCheckpoint>,
) -> Result<RunningWorkspace> {
    let text = std::fs::read_to_string(file)
        .with_context(|| format!("failed to read workspace {}", file.display()))?;
    let manifest = parse_manifest(&text)?;
    let base = file.parent().unwrap_or_else(|| Path::new("."));
    let workspace_digest = sha256_hex(text.as_bytes());
    if let Some(loaded) = checkpoint
        && loaded.checkpoint.workspace_digest != workspace_digest
    {
        bail!(
            "checkpoint was taken from a different workspace file ({} != {})",
            loaded.checkpoint.workspace_digest,
            workspace_digest
        );
    }
    let view = checkpoint
        .map(|loaded| ShellViewState {
            direction: loaded.checkpoint.view.direction,
            split: loaded.checkpoint.view.split.clone(),
            active_pane: loaded.checkpoint.view.active_pane.clone(),
        })
        .unwrap_or_else(|| ShellViewState::for_manifest(&manifest));

    let runtime_root = tempfile::Builder::new()
        .prefix("ato-workspace-")
        .tempdir()
        .context("failed to create the workspace runtime directory")?;
    let mut panes = Vec::with_capacity(manifest.pane.len());
    let mut pane_urls = BTreeMap::new();
    for pane in &manifest.pane {
        let loaded = load_pane_capsule(base, pane)?;
        let destination = runtime_root.path().join(&pane.id);
        ato_portable_application::materialize_tree(&loaded.bundle, &loaded.selected, &destination)
            .with_context(|| format!("pane `{}` tree could not be materialized", pane.id))?;
        let routes = pane_routes(&destination, &loaded.selected)?;
        let token = random_token()?;
        let state = ato_runtime_attempt::static_server::StaticApplicationState {
            persistence_token: token.clone(),
            local_storage: pane_state(checkpoint, &pane.id).clone(),
            assets: Vec::new(),
        };
        let server = ato_runtime_attempt::static_server::StaticApplicationServer::listen(
            routes,
            pane_entry_route(&loaded.selected),
            pane_spa_fallback(&loaded.selected),
            Some(state),
        )
        .with_context(|| format!("pane `{}` server could not start", pane.id))?;
        pane_urls.insert(pane.id.clone(), server.base_url());
        panes.push(WorkspacePaneRuntime {
            pane_id: pane.id.clone(),
            server,
            persistence_token: token,
        });
    }

    let shell_directory = runtime_root.path().join("__workspace_shell");
    std::fs::create_dir_all(&shell_directory)
        .context("failed to create the workspace shell directory")?;
    let shell_path = shell_directory.join("index.html");
    std::fs::write(&shell_path, shell_html(&manifest, &pane_urls, &view)?)
        .context("failed to write the workspace shell")?;
    let shell_view_storage = BTreeMap::from([(
        VIEW_STATE_KEY.to_owned(),
        serde_json::to_string(&view).context("failed to encode the shell view state")?,
    )]);
    let shell_persistence_token = random_token()?;
    let shell = ato_runtime_attempt::static_server::StaticApplicationServer::listen(
        BTreeMap::from([(
            "/index.html".to_owned(),
            (shell_path, "text/html; charset=utf-8".to_owned()),
        )]),
        "/index.html".to_owned(),
        false,
        Some(ato_runtime_attempt::static_server::StaticApplicationState {
            persistence_token: shell_persistence_token.clone(),
            local_storage: shell_view_storage,
            assets: Vec::new(),
        }),
    )
    .context("the workspace shell server could not start")?;
    let shell_url = format!("{}/", shell.base_url());
    Ok(RunningWorkspace {
        shell,
        panes,
        shell_url,
        workspace_digest,
        shell_persistence_token,
        manifest,
        _runtime_root: runtime_root,
    })
}

impl RunningWorkspace {
    /// Capture the state the panes actually hold and the shell view state.
    ///
    /// Returns the checkpoint plus the snapshot resource objects it names; the
    /// caller writes both (`save_workspace_checkpoint`) so a checkpoint never
    /// points at bytes that do not exist.
    pub fn capture(&self) -> Result<(WorkspaceCheckpointV1, BTreeMap<String, Vec<u8>>)> {
        let view = self
            .shell
            .local_storage()?
            .and_then(|mut storage| storage.remove(VIEW_STATE_KEY))
            .and_then(|value| serde_json::from_str::<ShellViewState>(&value).ok())
            .unwrap_or_else(|| ShellViewState::for_manifest(&self.manifest));
        let mut pane = Vec::with_capacity(self.panes.len());
        let mut objects = BTreeMap::new();
        for runtime in &self.panes {
            let storage = runtime.server.local_storage()?.unwrap_or_default();
            let (snapshot, object, snapshot_ref) = browser_snapshot(&storage)?;
            let content_ref = snapshot["resources"][0]["content_ref"]
                .as_str()
                .expect("browser_snapshot always names a content_ref")
                .to_owned();
            objects.insert(content_ref, object);
            pane.push(WorkspaceCheckpointPaneV1 {
                pane_id: runtime.pane_id.clone(),
                instance_id: format!("ephemeral-{}", runtime.pane_id),
                snapshot_ref,
                receipt_ref: None,
                snapshot,
            });
        }
        pane.sort_by(|left, right| left.pane_id.cmp(&right.pane_id));
        let mut view_panes = Vec::new();
        for manifest_pane in &self.manifest.pane {
            view_panes.push(WorkspacePaneViewStateV1 {
                pane_id: manifest_pane.id.clone(),
                restore: manifest_pane.restore.clone(),
                state: None,
            });
        }
        view_panes.sort_by(|left, right| left.pane_id.cmp(&right.pane_id));
        let checkpoint = WorkspaceCheckpointV1 {
            schema: WORKSPACE_CHECKPOINT_SCHEMA.to_owned(),
            workspace_digest: self.workspace_digest.clone(),
            view: WorkspaceViewStateV1 {
                direction: view.direction,
                split: view.split,
                active_pane: view.active_pane,
                pane: view_panes,
            },
            pane,
        };
        checkpoint.validate()?;
        Ok((checkpoint, objects))
    }

    /// The shell origin's state-bridge token (same purpose as a pane's).
    pub fn shell_persistence_token(&self) -> &str {
        &self.shell_persistence_token
    }

    /// Tokens are exposed so a test can drive the state bridge as a browser
    /// would; they are per-run loopback CSRF tokens, not secrets.
    pub fn persistence_token(&self, pane_id: &str) -> Option<&str> {
        self.panes
            .iter()
            .find(|pane| pane.pane_id == pane_id)
            .map(|pane| pane.persistence_token.as_str())
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
        let (snapshot, _object, snapshot_ref) = browser_snapshot(&BTreeMap::new()).unwrap();
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
                    snapshot_ref: snapshot_ref.clone(),
                    receipt_ref: None,
                    snapshot: snapshot.clone(),
                },
                WorkspaceCheckpointPaneV1 {
                    pane_id: "todo".to_owned(),
                    instance_id: "instance-2".to_owned(),
                    snapshot_ref: snapshot_ref.clone(),
                    receipt_ref: Some(sha256_hex(b"receipt")),
                    snapshot: snapshot.clone(),
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
