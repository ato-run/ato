//! On-disk location + persistence of the sealed Ready-State artifact.
//!
//! Legacy artifacts live under `<root>/ready-state/<capsule_manifest_hash>/`.
//! Capsule v1 artifacts live under
//! `<root>/snapshots/<execution_id>/<snapshot_id>/`, which retains every resolved
//! target and every immutable cache independently. Identity components are
//! sanitized so untrusted values cannot escape the root.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
#[cfg(not(test))]
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use capsule::execution_contract::ExecutionId;
use capsulefs::CasStore;
use snapshot::{
    ARTIFACT_ENVELOPE_V1_FILENAME, ArtifactEnvelopeV1, ReadyStateManifest,
    SNAPSHOT_MANIFEST_V1_FILENAME, SnapshotManifestV1,
};

const ACCEPTANCE_RECEIPT_SCHEMA: &str = "ato.snapshot-local-acceptance-receipt/v1";
const ACCEPTANCE_RECEIPT_DOMAIN: &[u8] = b"ato.snapshot-local-acceptance-receipt/v1\0";
const ACCEPTANCE_RECEIPT_DIR: &str = "acceptance";
#[cfg(not(test))]
const ACCEPTANCE_SIGNER_HELPER_ENV: &str = "ATO_SNAPSHOT_ACCEPTANCE_SIGNER_HELPER";
#[cfg(not(test))]
const ACCEPTANCE_SIGNER_PROTOCOL: &str = "ato.snapshot-acceptance-signer/v1";
static LOCAL_PUBLICATION_NONCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalAcceptanceReceiptV1 {
    schema: String,
    execution_id: ExecutionId,
    snapshot_id: String,
    envelope_id: String,
    acceptance_receipt_id: String,
    key_id: String,
    authenticator: String,
}

#[derive(serde::Serialize)]
struct LocalAcceptanceReceiptProjection<'a> {
    schema: &'a str,
    execution_id: &'a ExecutionId,
    snapshot_id: &'a str,
    envelope_id: &'a str,
    acceptance_receipt_id: &'a str,
}

#[cfg(not(test))]
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum AcceptanceSignerOperation {
    Issue,
    Verify,
}

#[cfg(not(test))]
#[derive(Debug, serde::Serialize)]
struct AcceptanceSignerRequest<'a> {
    schema: &'static str,
    operation: AcceptanceSignerOperation,
    payload_hex: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    key_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    authenticator: Option<&'a str>,
}

#[cfg(not(test))]
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptanceSignerResponse {
    schema: String,
    #[serde(default)]
    key_id: Option<String>,
    #[serde(default)]
    authenticator: Option<String>,
    #[serde(default)]
    valid: Option<bool>,
}

struct IssuedAuthenticator {
    key_id: String,
    authenticator: String,
}

/// Sanitize a `blake3:<hex>`-style id into one safe path component (hex/dash
/// only); anything else collapses to `_`.
fn safe_component(id: &str) -> String {
    let s: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() { "_".to_string() } else { s }
}

/// Directory holding one sealed artifact.
pub(crate) fn artifact_dir(root: &Path, capsule_manifest_hash: &str) -> PathBuf {
    root.join("ready-state")
        .join(safe_component(capsule_manifest_hash))
}

/// Open (creating if needed) the CapsuleFS store for an artifact.
pub(crate) fn open_store(root: &Path, capsule_manifest_hash: &str) -> Result<CasStore> {
    let dir = artifact_dir(root, capsule_manifest_hash).join("cas");
    CasStore::open(&dir).with_context(|| format!("open CapsuleFS store at {}", dir.display()))
}

fn manifest_path(root: &Path, capsule_manifest_hash: &str) -> PathBuf {
    artifact_dir(root, capsule_manifest_hash).join("manifest.json")
}

/// Persist a sealed manifest as JSON next to its CAS store.
pub(crate) fn save_manifest(root: &Path, manifest: &ReadyStateManifest) -> Result<PathBuf> {
    let path = manifest_path(root, &manifest.capsule_manifest_hash);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(manifest)?;
    std::fs::write(&path, json).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

/// Load a previously sealed manifest, if present.
pub(crate) fn load_manifest(
    root: &Path,
    capsule_manifest_hash: &str,
) -> Result<Option<ReadyStateManifest>> {
    let path = manifest_path(root, capsule_manifest_hash);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let manifest =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    Ok(Some(manifest))
}

#[derive(Debug, Clone)]
pub(crate) struct StoredSnapshotV1 {
    pub artifact_dir: PathBuf,
    pub legacy_manifest: ReadyStateManifest,
    pub snapshot_manifest: SnapshotManifestV1,
    pub envelope: ArtifactEnvelopeV1,
}

pub(crate) struct V1StagingArtifact {
    dir: PathBuf,
    committed: bool,
}

impl V1StagingArtifact {
    pub(crate) fn create(root: &Path, execution_id: &ExecutionId) -> Result<Self> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before UNIX epoch")?
            .as_nanos();
        let sequence = LOCAL_PUBLICATION_NONCE.fetch_add(1, Ordering::Relaxed);
        let dir = root.join("snapshots").join(".staging").join(format!(
            "{}-{}-{nonce}-{sequence}",
            safe_component(execution_id.as_str()),
            std::process::id()
        ));
        std::fs::create_dir_all(dir.join("cas"))?;
        Ok(Self {
            dir,
            committed: false,
        })
    }

    pub(crate) fn artifact_dir(&self) -> &Path {
        &self.dir
    }

    pub(crate) fn open_store(&self) -> Result<CasStore> {
        let dir = self.dir.join("cas");
        CasStore::open(&dir).with_context(|| format!("open CapsuleFS store at {}", dir.display()))
    }

    pub(crate) fn commit(
        mut self,
        root: &Path,
        legacy: &ReadyStateManifest,
        snapshot: &SnapshotManifestV1,
        envelope: &ArtifactEnvelopeV1,
    ) -> Result<PathBuf> {
        envelope
            .verify(legacy, snapshot)
            .map_err(anyhow::Error::new)?;
        write_json(&self.dir.join("manifest.json"), legacy)?;
        write_json(&self.dir.join(SNAPSHOT_MANIFEST_V1_FILENAME), snapshot)?;
        write_json(&self.dir.join(ARTIFACT_ENVELOPE_V1_FILENAME), envelope)?;
        let final_dir = snapshot_dir(root, &snapshot.execution_id, &snapshot.snapshot_id);
        if final_dir.exists() {
            let existing = load_v1_snapshot_with_expected_envelope(
                root,
                &snapshot.execution_id,
                &snapshot.snapshot_id,
                &envelope.envelope_id,
                &envelope.acceptance.receipt_id.to_string(),
            )?
            .ok_or_else(|| anyhow::anyhow!("existing Snapshot v1 directory is incomplete"))?;
            if existing.legacy_manifest != *legacy
                || existing.snapshot_manifest != *snapshot
                || existing.envelope != *envelope
            {
                anyhow::bail!("immutable Snapshot v1 directory already contains different data");
            }
            record_accepted_envelope(root, snapshot, envelope)?;
            return Ok(final_dir);
        }
        if let Some(parent) = final_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&self.dir, &final_dir).with_context(|| {
            format!(
                "publish Snapshot v1 {} to {}",
                snapshot.snapshot_id,
                final_dir.display()
            )
        })?;
        self.committed = true;
        record_accepted_envelope(root, snapshot, envelope)?;
        Ok(final_dir)
    }
}

impl Drop for V1StagingArtifact {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

pub(crate) fn snapshot_dir(root: &Path, execution_id: &ExecutionId, snapshot_id: &str) -> PathBuf {
    root.join("snapshots")
        .join(safe_component(execution_id.as_str()))
        .join(safe_component(snapshot_id))
}

pub(crate) fn open_store_at_artifact_dir(artifact_dir: &Path) -> Result<CasStore> {
    let dir = artifact_dir.join("cas");
    CasStore::open(&dir).with_context(|| format!("open CapsuleFS store at {}", dir.display()))
}

#[cfg(test)]
pub(crate) fn load_v1_snapshot(
    root: &Path,
    execution_id: &ExecutionId,
    snapshot_id: &str,
) -> Result<Option<StoredSnapshotV1>> {
    let Some(receipt) = load_acceptance_receipt(root, execution_id, snapshot_id)? else {
        return Ok(None);
    };
    load_v1_snapshot_with_expected_envelope(
        root,
        execution_id,
        snapshot_id,
        &receipt.envelope_id,
        &receipt.acceptance_receipt_id,
    )
}

fn load_v1_snapshot_with_expected_envelope(
    root: &Path,
    execution_id: &ExecutionId,
    snapshot_id: &str,
    expected_envelope_id: &str,
    expected_acceptance_receipt_id: &str,
) -> Result<Option<StoredSnapshotV1>> {
    let dir = snapshot_dir(root, execution_id, snapshot_id);
    if !dir.is_dir() {
        return Ok(None);
    }
    let legacy_manifest: ReadyStateManifest = read_json(&dir.join("manifest.json"))?;
    let snapshot_manifest: SnapshotManifestV1 =
        read_json(&dir.join(SNAPSHOT_MANIFEST_V1_FILENAME))?;
    let envelope: ArtifactEnvelopeV1 = read_json(&dir.join(ARTIFACT_ENVELOPE_V1_FILENAME))?;
    if &snapshot_manifest.execution_id != execution_id
        || snapshot_manifest.snapshot_id != snapshot_id
    {
        anyhow::bail!("Snapshot v1 path identity does not match its authenticated metadata");
    }
    envelope
        .verify(&legacy_manifest, &snapshot_manifest)
        .map_err(anyhow::Error::new)?;
    if envelope.envelope_id != expected_envelope_id {
        anyhow::bail!("Snapshot v1 envelope does not match its authenticated acceptance receipt");
    }
    if envelope.acceptance.receipt_id.to_string() != expected_acceptance_receipt_id {
        anyhow::bail!(
            "Snapshot v1 acceptance metadata does not match its authenticated acceptance receipt"
        );
    }
    Ok(Some(StoredSnapshotV1 {
        artifact_dir: dir,
        legacy_manifest,
        snapshot_manifest,
        envelope,
    }))
}

pub(crate) fn load_v1_snapshots(
    root: &Path,
    execution_id: &ExecutionId,
) -> Result<Vec<StoredSnapshotV1>> {
    let mut snapshots = Vec::new();
    let dir = acceptance_receipt_dir(root, execution_id);
    if !dir.is_dir() {
        return Ok(snapshots);
    }
    let mut paths = std::fs::read_dir(&dir)
        .with_context(|| format!("read acceptance receipt directory {}", dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.sort_by_key(std::fs::DirEntry::file_name);
    for entry in paths {
        if !entry.file_type()?.is_file() {
            continue;
        }
        let receipt: LocalAcceptanceReceiptV1 = match read_json(&entry.path()) {
            Ok(receipt) => receipt,
            Err(_) => continue,
        };
        if verify_acceptance_receipt(&receipt, execution_id).is_err() {
            continue;
        }
        if let Ok(Some(snapshot)) = load_v1_snapshot_with_expected_envelope(
            root,
            execution_id,
            &receipt.snapshot_id,
            &receipt.envelope_id,
            &receipt.acceptance_receipt_id,
        ) {
            snapshots.push(snapshot);
        }
    }
    Ok(snapshots)
}

fn acceptance_receipt_dir(root: &Path, execution_id: &ExecutionId) -> PathBuf {
    root.join("snapshots")
        .join(safe_component(execution_id.as_str()))
        .join(ACCEPTANCE_RECEIPT_DIR)
}

fn acceptance_receipt_path(root: &Path, execution_id: &ExecutionId, snapshot_id: &str) -> PathBuf {
    acceptance_receipt_dir(root, execution_id).join(format!("{}.json", safe_component(snapshot_id)))
}

#[cfg(test)]
fn load_acceptance_receipt(
    root: &Path,
    execution_id: &ExecutionId,
    snapshot_id: &str,
) -> Result<Option<LocalAcceptanceReceiptV1>> {
    let path = acceptance_receipt_path(root, execution_id, snapshot_id);
    if !path.exists() {
        return Ok(None);
    }
    let receipt: LocalAcceptanceReceiptV1 = read_json(&path)?;
    if receipt.snapshot_id != snapshot_id {
        anyhow::bail!("Snapshot v1 acceptance receipt path identity mismatch");
    }
    verify_acceptance_receipt(&receipt, execution_id)?;
    Ok(Some(receipt))
}

fn record_accepted_envelope(
    root: &Path,
    snapshot: &SnapshotManifestV1,
    envelope: &ArtifactEnvelopeV1,
) -> Result<()> {
    let acceptance_receipt_id = envelope.acceptance.receipt_id.to_string();
    let mut receipt = LocalAcceptanceReceiptV1 {
        schema: ACCEPTANCE_RECEIPT_SCHEMA.to_string(),
        execution_id: snapshot.execution_id.clone(),
        snapshot_id: snapshot.snapshot_id.clone(),
        envelope_id: envelope.envelope_id.clone(),
        acceptance_receipt_id,
        key_id: String::new(),
        authenticator: String::new(),
    };
    let issued = issue_acceptance_authenticator(&acceptance_payload(&receipt)?)?;
    receipt.key_id = issued.key_id;
    receipt.authenticator = issued.authenticator;
    let path = acceptance_receipt_path(root, &snapshot.execution_id, &snapshot.snapshot_id);
    if path.exists() {
        let existing: LocalAcceptanceReceiptV1 = read_json(&path)?;
        verify_acceptance_receipt(&existing, &snapshot.execution_id)?;
        if existing != receipt {
            anyhow::bail!("immutable acceptance receipt already pins a different envelope");
        }
        return Ok(());
    }
    let parent = path.parent().context("acceptance receipt has no parent")?;
    std::fs::create_dir_all(parent)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before UNIX epoch")?
        .as_nanos();
    let sequence = LOCAL_PUBLICATION_NONCE.fetch_add(1, Ordering::Relaxed);
    let pending = parent.join(format!(
        ".{}-{}-{nonce}-{sequence}.pending",
        safe_component(&snapshot.snapshot_id),
        std::process::id()
    ));
    let json = serde_json::to_vec_pretty(&receipt)?;
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&pending)
        .with_context(|| format!("create {}", pending.display()))?;
    file.write_all(&json)?;
    file.sync_all()?;
    match std::fs::hard_link(&pending, &path) {
        Ok(()) => {
            std::fs::remove_file(&pending)?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            std::fs::remove_file(&pending)?;
            let existing: LocalAcceptanceReceiptV1 = read_json(&path)?;
            verify_acceptance_receipt(&existing, &snapshot.execution_id)?;
            if existing == receipt {
                Ok(())
            } else {
                anyhow::bail!(
                    "immutable acceptance receipt concurrently pinned a different envelope"
                )
            }
        }
        Err(error) => {
            let _ = std::fs::remove_file(&pending);
            Err(error).with_context(|| format!("publish acceptance receipt {}", path.display()))
        }
    }
}

fn verify_acceptance_receipt(
    receipt: &LocalAcceptanceReceiptV1,
    expected_execution_id: &ExecutionId,
) -> Result<()> {
    if receipt.schema != ACCEPTANCE_RECEIPT_SCHEMA
        || &receipt.execution_id != expected_execution_id
        || !valid_key_id(&receipt.key_id)
        || !verify_acceptance_authenticator(receipt, &acceptance_payload(receipt)?)?
    {
        anyhow::bail!("invalid Snapshot v1 authenticated acceptance receipt");
    }
    Ok(())
}

fn acceptance_payload(receipt: &LocalAcceptanceReceiptV1) -> Result<Vec<u8>> {
    let projection = LocalAcceptanceReceiptProjection {
        schema: &receipt.schema,
        execution_id: &receipt.execution_id,
        snapshot_id: &receipt.snapshot_id,
        envelope_id: &receipt.envelope_id,
        acceptance_receipt_id: &receipt.acceptance_receipt_id,
    };
    let canonical = serde_jcs::to_vec(&projection)?;
    let mut input = Vec::with_capacity(ACCEPTANCE_RECEIPT_DOMAIN.len() + canonical.len());
    input.extend_from_slice(ACCEPTANCE_RECEIPT_DOMAIN);
    input.extend_from_slice(&canonical);
    Ok(input)
}

fn valid_key_id(key_id: &str) -> bool {
    !key_id.is_empty()
        && key_id.len() <= 128
        && key_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
}

#[cfg(not(test))]
fn issue_acceptance_authenticator(payload: &[u8]) -> Result<IssuedAuthenticator> {
    let response = call_acceptance_signer(AcceptanceSignerRequest {
        schema: ACCEPTANCE_SIGNER_PROTOCOL,
        operation: AcceptanceSignerOperation::Issue,
        payload_hex: hex::encode(payload),
        key_id: None,
        authenticator: None,
    })?;
    let key_id = response
        .key_id
        .filter(|value| valid_key_id(value))
        .context("acceptance signer returned an invalid or missing key_id")?;
    let authenticator = response
        .authenticator
        .filter(|value| !value.trim().is_empty())
        .context("acceptance signer returned no authenticator")?;
    Ok(IssuedAuthenticator {
        key_id,
        authenticator,
    })
}

#[cfg(not(test))]
fn verify_acceptance_authenticator(
    receipt: &LocalAcceptanceReceiptV1,
    payload: &[u8],
) -> Result<bool> {
    let response = call_acceptance_signer(AcceptanceSignerRequest {
        schema: ACCEPTANCE_SIGNER_PROTOCOL,
        operation: AcceptanceSignerOperation::Verify,
        payload_hex: hex::encode(payload),
        key_id: Some(&receipt.key_id),
        authenticator: Some(&receipt.authenticator),
    })?;
    Ok(response.valid == Some(true))
}

#[cfg(not(test))]
fn call_acceptance_signer(
    request: AcceptanceSignerRequest<'_>,
) -> Result<AcceptanceSignerResponse> {
    let helper = std::env::var_os(ACCEPTANCE_SIGNER_HELPER_ENV).with_context(|| {
        format!(
            "Capsule v1 local Snapshot acceptance requires a caller-authenticating signing helper configured by {ACCEPTANCE_SIGNER_HELPER_ENV}"
        )
    })?;
    let mut child = Command::new(helper)
        .arg("--stdio-v1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_remove("ATO_SNAPSHOT_ACCEPTANCE_MAC_KEY")
        .spawn()
        .context("start Snapshot acceptance signing helper")?;
    let request_json = serde_json::to_vec(&request)?;
    child
        .stdin
        .take()
        .context("acceptance signer stdin unavailable")?
        .write_all(&request_json)?;
    let output = child
        .wait_with_output()
        .context("wait for Snapshot acceptance signing helper")?;
    if !output.status.success() {
        anyhow::bail!("Snapshot acceptance signing helper rejected the request");
    }
    let response: AcceptanceSignerResponse = serde_json::from_slice(&output.stdout)
        .context("parse Snapshot acceptance signing helper response")?;
    if response.schema != ACCEPTANCE_SIGNER_PROTOCOL {
        anyhow::bail!("Snapshot acceptance signing helper returned the wrong protocol schema");
    }
    Ok(response)
}

#[cfg(test)]
fn issue_acceptance_authenticator(payload: &[u8]) -> Result<IssuedAuthenticator> {
    Ok(IssuedAuthenticator {
        key_id: "test-key-v1".to_string(),
        authenticator: format!(
            "blake3:{}",
            blake3::keyed_hash(&[0x5a; 32], payload).to_hex()
        ),
    })
}

#[cfg(test)]
fn verify_acceptance_authenticator(
    receipt: &LocalAcceptanceReceiptV1,
    payload: &[u8],
) -> Result<bool> {
    Ok(receipt.key_id == "test-key-v1"
        && receipt.authenticator
            == format!(
                "blake3:{}",
                blake3::keyed_hash(&[0x5a; 32], payload).to_hex()
            ))
}

fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let json = serde_json::to_vec_pretty(value)?;
    std::fs::write(path, json).with_context(|| format!("write {}", path.display()))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_component_strips_path_separators() {
        assert_eq!(safe_component("blake3:../../etc"), "blake3_______etc");
        assert_eq!(safe_component("blake3:abcDEF123"), "blake3_abcDEF123");
        assert_eq!(safe_component(""), "_");
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path().join("seed")).unwrap();
        let rootfs = capsulefs::store_blob(
            &store,
            capsulefs::LayerKind::Rootfs,
            b"rootfs-bytes",
            capsulefs::ChunkingKind::ContentDefined,
        )
        .unwrap();
        let manifest = ReadyStateManifest {
            schema: snapshot::READY_STATE_SCHEMA.to_string(),
            capsule_manifest_hash: "blake3:deadbeef".to_string(),
            has_vsock: false,
            runner_class_id: None,
            execution_id: None,
            execution_identity_schema: None,
            surface_requirement: None,
            layers: snapshot::ReadyStateLayers {
                rootfs: Some(rootfs),
                ..Default::default()
            },
            hotset_profile: Default::default(),
            snapshot_backend: snapshot::SnapshotBackendInfo {
                kind: "fake".to_string(),
                version: "0.1.0".to_string(),
                snapshot_format_version: "fake-v1".to_string(),
                cpu_template: None,
            },
            restore_contract: Default::default(),
            sanitizer_contract: Default::default(),
            no_secret_proof: None,
            build_receipt_id: None,
            supervisor_build: None,
        };
        let root = dir.path();
        assert!(load_manifest(root, "blake3:deadbeef").unwrap().is_none());
        save_manifest(root, &manifest).unwrap();
        let back = load_manifest(root, "blake3:deadbeef").unwrap().unwrap();
        assert_eq!(back, manifest);
    }

    #[test]
    fn v1_store_retains_multiple_execution_ids_for_one_capsule_manifest() {
        use capsule::execution_contract::EXECUTION_CONTRACT_V1_SCHEMA;
        use snapshot::{
            BuildLayers, BuildReadyStateInput, FakeSnapshotBackend, RestoreContract,
            SanitizerContract, SnapshotBackend, migrate_legacy_manifest,
        };

        fn persist(root: &Path, execution_id: ExecutionId) -> (String, PathBuf) {
            let backend = FakeSnapshotBackend::new();
            let staging = V1StagingArtifact::create(root, &execution_id).unwrap();
            let store = staging.open_store().unwrap();
            let legacy = backend
                .build_ready_state(BuildReadyStateInput {
                    store: &store,
                    capsule_manifest_hash: format!("blake3:{}", "c".repeat(64)),
                    runner_class: Some(
                        capsule::foundation::install_lifecycle::RunnerClassFacts::from_host().id(),
                    ),
                    surface_requirement: None,
                    layers: BuildLayers {
                        rootfs: format!("rootfs-{execution_id}").into_bytes(),
                        runtime: None,
                        dependency: None,
                        app: None,
                        vmstate: vec![1; 64],
                        memory: vec![2; 4096],
                    },
                    restore_contract: RestoreContract::default(),
                    sanitizer_contract: SanitizerContract::default(),
                    declared_secret_markers: Vec::new(),
                    execution_id: Some(execution_id.to_string()),
                    execution_identity_schema: Some(EXECUTION_CONTRACT_V1_SCHEMA.to_string()),
                    supervisor: None,
                })
                .unwrap()
                .manifest;
            let sidecar = migrate_legacy_manifest(
                &legacy,
                execution_id,
                backend.snapshot_compatibility_contract().unwrap(),
            )
            .unwrap();
            let envelope = ArtifactEnvelopeV1::accepted(&legacy, &sidecar).unwrap();
            let snapshot_id = sidecar.snapshot_id.clone();
            let path = staging.commit(root, &legacy, &sidecar, &envelope).unwrap();
            (snapshot_id, path)
        }

        let root = tempfile::tempdir().unwrap();
        let first = ExecutionId::new(format!("blake3:{}", "1".repeat(64))).unwrap();
        let second = ExecutionId::new(format!("blake3:{}", "2".repeat(64))).unwrap();
        let (first_snapshot, first_path) = persist(root.path(), first.clone());
        let (second_snapshot, second_path) = persist(root.path(), second.clone());

        assert_ne!(first_path, second_path);
        assert!(first_path.is_dir());
        assert!(second_path.is_dir());
        assert!(
            load_v1_snapshot(root.path(), &first, &first_snapshot)
                .unwrap()
                .is_some()
        );
        assert!(
            load_v1_snapshot(root.path(), &second, &second_snapshot)
                .unwrap()
                .is_some()
        );
        assert_eq!(load_v1_snapshots(root.path(), &first).unwrap().len(), 1);
        assert_eq!(load_v1_snapshots(root.path(), &second).unwrap().len(), 1);
        assert!(
            load_manifest(root.path(), &format!("blake3:{}", "c".repeat(64)))
                .unwrap()
                .is_none(),
            "Capsule v1 must not overwrite the legacy manifest-keyed store"
        );
    }

    #[test]
    fn rehashed_envelope_and_receipt_are_rejected_without_signer_authenticator() {
        use capsule::execution_contract::EXECUTION_CONTRACT_V1_SCHEMA;
        use snapshot::{
            BuildLayers, BuildReadyStateInput, FakeSnapshotBackend, RestoreContract,
            SanitizerContract, SnapshotBackend, migrate_legacy_manifest,
        };

        let root = tempfile::tempdir().unwrap();
        let execution_id = ExecutionId::new(format!("blake3:{}", "3".repeat(64))).unwrap();
        let backend = FakeSnapshotBackend::new();
        let staging = V1StagingArtifact::create(root.path(), &execution_id).unwrap();
        let cas = staging.open_store().unwrap();
        let legacy = backend
            .build_ready_state(BuildReadyStateInput {
                store: &cas,
                capsule_manifest_hash: format!("blake3:{}", "c".repeat(64)),
                runner_class: None,
                surface_requirement: None,
                layers: BuildLayers {
                    rootfs: b"rootfs".to_vec(),
                    runtime: None,
                    dependency: None,
                    app: None,
                    vmstate: vec![1; 64],
                    memory: vec![2; 4096],
                },
                restore_contract: RestoreContract::default(),
                sanitizer_contract: SanitizerContract::default(),
                declared_secret_markers: Vec::new(),
                execution_id: Some(execution_id.to_string()),
                execution_identity_schema: Some(EXECUTION_CONTRACT_V1_SCHEMA.to_string()),
                supervisor: None,
            })
            .unwrap()
            .manifest;
        let snapshot = migrate_legacy_manifest(
            &legacy,
            execution_id.clone(),
            backend.snapshot_compatibility_contract().unwrap(),
        )
        .unwrap();
        let accepted = ArtifactEnvelopeV1::accepted(&legacy, &snapshot).unwrap();
        staging
            .commit(root.path(), &legacy, &snapshot, &accepted)
            .unwrap();

        // An attacker that can rewrite the artifact root can create a new,
        // self-consistent accepted envelope by changing legacy metadata and can
        // also rewrite the receipt's public fields. The external signer authenticator is
        // the trust anchor: without it the jointly rehashed files stay invalid.
        let mut tampered_legacy = legacy.clone();
        tampered_legacy.capsule_manifest_hash = format!("blake3:{}", "d".repeat(64));
        let tampered_envelope = ArtifactEnvelopeV1::accepted(&tampered_legacy, &snapshot).unwrap();
        let artifact = snapshot_dir(root.path(), &execution_id, &snapshot.snapshot_id);
        write_json(&artifact.join("manifest.json"), &tampered_legacy).unwrap();
        write_json(
            &artifact.join(ARTIFACT_ENVELOPE_V1_FILENAME),
            &tampered_envelope,
        )
        .unwrap();
        let receipt_path =
            acceptance_receipt_path(root.path(), &execution_id, &snapshot.snapshot_id);
        let mut tampered_receipt: LocalAcceptanceReceiptV1 = read_json(&receipt_path).unwrap();
        tampered_receipt.envelope_id = tampered_envelope.envelope_id;
        tampered_receipt.authenticator = format!("blake3:{}", "0".repeat(64));
        write_json(&receipt_path, &tampered_receipt).unwrap();

        let error =
            load_v1_snapshot(root.path(), &execution_id, &snapshot.snapshot_id).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("authenticated acceptance receipt")
        );
    }

    #[test]
    fn parallel_snapshot_acceptance_keeps_both_immutable_receipts() {
        use capsule::execution_contract::EXECUTION_CONTRACT_V1_SCHEMA;
        use snapshot::{
            BuildLayers, BuildReadyStateInput, FakeSnapshotBackend, RestoreContract,
            SanitizerContract, SnapshotBackend, migrate_legacy_manifest,
        };

        fn candidate(
            root: &Path,
            execution_id: &ExecutionId,
            marker: u8,
        ) -> (
            V1StagingArtifact,
            ReadyStateManifest,
            SnapshotManifestV1,
            ArtifactEnvelopeV1,
        ) {
            let backend = FakeSnapshotBackend::new();
            let staging = V1StagingArtifact::create(root, execution_id).unwrap();
            let store = staging.open_store().unwrap();
            let legacy = backend
                .build_ready_state(BuildReadyStateInput {
                    store: &store,
                    capsule_manifest_hash: format!("blake3:{}", "c".repeat(64)),
                    runner_class: None,
                    surface_requirement: None,
                    layers: BuildLayers {
                        rootfs: vec![marker; 128],
                        runtime: None,
                        dependency: None,
                        app: None,
                        vmstate: vec![marker; 64],
                        memory: vec![marker; 4096],
                    },
                    restore_contract: RestoreContract::default(),
                    sanitizer_contract: SanitizerContract::default(),
                    declared_secret_markers: Vec::new(),
                    execution_id: Some(execution_id.to_string()),
                    execution_identity_schema: Some(EXECUTION_CONTRACT_V1_SCHEMA.to_string()),
                    supervisor: None,
                })
                .unwrap()
                .manifest;
            let snapshot = migrate_legacy_manifest(
                &legacy,
                execution_id.clone(),
                backend.snapshot_compatibility_contract().unwrap(),
            )
            .unwrap();
            let envelope = ArtifactEnvelopeV1::accepted(&legacy, &snapshot).unwrap();
            (staging, legacy, snapshot, envelope)
        }

        let root = tempfile::tempdir().unwrap();
        let execution_id = ExecutionId::new(format!("blake3:{}", "4".repeat(64))).unwrap();
        let first = candidate(root.path(), &execution_id, 1);
        let second = candidate(root.path(), &execution_id, 2);
        let first_id = first.2.snapshot_id.clone();
        let second_id = second.2.snapshot_id.clone();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                first
                    .0
                    .commit(root.path(), &first.1, &first.2, &first.3)
                    .unwrap();
            });
            scope.spawn(|| {
                second
                    .0
                    .commit(root.path(), &second.1, &second.2, &second.3)
                    .unwrap();
            });
        });

        let loaded = load_v1_snapshots(root.path(), &execution_id).unwrap();
        let ids = loaded
            .into_iter()
            .map(|snapshot| snapshot.snapshot_manifest.snapshot_id)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ids, std::collections::BTreeSet::from([first_id, second_id]));
    }
}
