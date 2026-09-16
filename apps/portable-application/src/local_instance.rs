use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use ato_objects::{CapsuleBundleDocument, decode_capsule_bundle_document};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{bundle_sha256, validate_bundle_for_derivation};

pub const LOCAL_APPLICATION_SCHEMA: &str = "ato.local-application/1";
pub const LOCAL_INSTANCE_SCHEMA: &str = "ato.local-instance/1";
pub const LOCAL_INSTANCE_RUN_SCHEMA: &str = "ato.local-instance-run/1";

static LOCAL_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Error)]
pub enum LocalInstanceError {
    #[error("portable bundle is invalid: {0}")]
    Bundle(String),
    #[error("local import accepts only portable application bundles")]
    NotPortableApplication,
    #[error(
        "portable Capsule declares {count} derivations; select one with --derivation <sha256:...>"
    )]
    DerivationRequired { count: usize },
    #[error("invalid local identifier `{0}`")]
    InvalidIdentifier(String),
    #[error("local instance `{0}` does not exist")]
    UnknownInstance(String),
    #[error("local instance `{instance_id}` already has Run `{run_id}` in status `{status}`")]
    ActiveRunConflict {
        instance_id: String,
        run_id: String,
        status: String,
    },
    #[error("local instance Run lease changed while updating `{0}`")]
    RunLeaseChanged(String),
    #[error("local instance data is invalid: {0}")]
    InvalidState(String),
    #[error("local instance JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("local instance I/O failed at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalApplicationMetadata {
    pub schema: String,
    pub application_id: String,
    pub application_ref: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalInstanceMetadata {
    pub schema: String,
    pub instance_id: String,
    pub bundle_sha256: String,
    pub contract_ref: String,
    pub application_ref: String,
    pub application_id: String,
    pub available_derivation_refs: Vec<String>,
    pub selected_derivation_ref: String,
    pub created_at: String,
    pub data_snapshot_ref: Option<String>,
    pub bindings: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstanceRunStatus {
    Starting,
    Active,
}

impl LocalInstanceRunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Active => "active",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalInstanceRun {
    pub schema: String,
    pub run_id: String,
    pub instance_id: String,
    pub token: String,
    pub status: LocalInstanceRunStatus,
    pub pid: Option<u32>,
    pub process_start_time: Option<String>,
    pub process_group: Option<u32>,
    pub boot_session: Option<String>,
    pub url: Option<String>,
    pub receipt_path: Option<String>,
    pub created_at: String,
}

pub struct LocalRunActivation<'a> {
    pub pid: u32,
    pub process_start_time: String,
    pub process_group: u32,
    pub boot_session: String,
    pub url: String,
    pub receipt: &'a [u8],
}

#[derive(Debug, Clone)]
pub struct LocalApplicationStore {
    root: PathBuf,
}

impl LocalApplicationStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, LocalInstanceError> {
        let root = root.into();
        create_dir_all(&root.join("apps"))?;
        create_dir_all(&root.join("instances"))?;
        Ok(Self { root })
    }

    pub fn import(
        &self,
        bundle_bytes: &[u8],
        selected_derivation: Option<&str>,
    ) -> Result<LocalInstanceMetadata, LocalInstanceError> {
        let bundle = match decode_capsule_bundle_document(bundle_bytes)
            .map_err(|error| LocalInstanceError::Bundle(error.to_string()))?
        {
            CapsuleBundleDocument::PortableApplicationV3(bundle)
            | CapsuleBundleDocument::PortableApplicationV4(bundle) => bundle,
            CapsuleBundleDocument::ComputationV2(_) => {
                return Err(LocalInstanceError::NotPortableApplication);
            }
        };
        let selected_derivation = match selected_derivation {
            Some(reference) => reference,
            None if bundle.index.derivations.len() == 1 => &bundle.index.derivations[0],
            None => {
                return Err(LocalInstanceError::DerivationRequired {
                    count: bundle.index.derivations.len(),
                });
            }
        };
        let validated = validate_bundle_for_derivation(&bundle, selected_derivation)
            .map_err(|error| LocalInstanceError::Bundle(error.to_string()))?;
        let application_id = application_id(&validated.application_ref.to_string())?;
        let created_at = observed_at();
        let application = LocalApplicationMetadata {
            schema: LOCAL_APPLICATION_SCHEMA.to_owned(),
            application_id: application_id.clone(),
            application_ref: validated.application_ref.to_string(),
            created_at: created_at.clone(),
        };
        let application_root = self.application_root(&application_id)?;
        create_dir_all(&application_root.join("bundles"))?;
        ensure_application(&application_root.join("application.json"), &application)?;

        let transport_sha256 = bundle_sha256(bundle_bytes);
        let bundle_path = application_root
            .join("bundles")
            .join(format!("{}.capsule", digest_hex(&transport_sha256)?));
        create_or_verify(&bundle_path, bundle_bytes)?;

        let instance_id = new_id("linst");
        let instance = LocalInstanceMetadata {
            schema: LOCAL_INSTANCE_SCHEMA.to_owned(),
            instance_id: instance_id.clone(),
            bundle_sha256: transport_sha256,
            contract_ref: validated.contract_ref.to_string(),
            application_ref: validated.application_ref.to_string(),
            application_id,
            available_derivation_refs: bundle.index.derivations,
            selected_derivation_ref: validated.derivation_ref.to_string(),
            created_at,
            data_snapshot_ref: None,
            bindings: BTreeMap::new(),
        };
        let instance_root = self.instance_root(&instance_id)?;
        create_dir(&instance_root)?;
        create_dir_all(&instance_root.join("runs"))?;
        create_canonical(&instance_root.join("instance.json"), &instance)?;
        Ok(instance)
    }

    pub fn instance(&self, instance_id: &str) -> Result<LocalInstanceMetadata, LocalInstanceError> {
        let path = self.instance_root(instance_id)?.join("instance.json");
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(LocalInstanceError::UnknownInstance(instance_id.to_owned()));
            }
            Err(source) => return Err(LocalInstanceError::Io { path, source }),
        };
        let instance: LocalInstanceMetadata = serde_json::from_slice(&bytes)?;
        if instance.schema != LOCAL_INSTANCE_SCHEMA || instance.instance_id != instance_id {
            return Err(LocalInstanceError::InvalidState(format!(
                "instance metadata does not match `{instance_id}`"
            )));
        }
        if serde_jcs::to_vec(&instance)? != bytes {
            return Err(LocalInstanceError::InvalidState(format!(
                "instance metadata for `{instance_id}` is not canonical"
            )));
        }
        Ok(instance)
    }

    pub fn bundle_bytes(
        &self,
        instance: &LocalInstanceMetadata,
    ) -> Result<Vec<u8>, LocalInstanceError> {
        let path = self
            .application_root(&instance.application_id)?
            .join("bundles")
            .join(format!("{}.capsule", digest_hex(&instance.bundle_sha256)?));
        let bytes = read(&path)?;
        let actual = bundle_sha256(&bytes);
        if actual != instance.bundle_sha256 {
            return Err(LocalInstanceError::InvalidState(format!(
                "stored bundle digest mismatch: expected {}, got {actual}",
                instance.bundle_sha256
            )));
        }
        Ok(bytes)
    }

    pub fn claim_run(&self, instance_id: &str) -> Result<LocalInstanceRun, LocalInstanceError> {
        self.instance(instance_id)?;
        let active_path = self.active_run_path(instance_id)?;
        if let Some(active) = self.active_run(instance_id)? {
            return Err(LocalInstanceError::ActiveRunConflict {
                instance_id: instance_id.to_owned(),
                run_id: active.run_id,
                status: active.status.as_str().to_owned(),
            });
        }
        let run = LocalInstanceRun {
            schema: LOCAL_INSTANCE_RUN_SCHEMA.to_owned(),
            run_id: new_id("lrun"),
            instance_id: instance_id.to_owned(),
            token: new_id("token"),
            status: LocalInstanceRunStatus::Starting,
            pid: None,
            process_start_time: None,
            process_group: None,
            boot_session: None,
            url: None,
            receipt_path: None,
            created_at: observed_at(),
        };
        create_dir_all(&self.run_root(instance_id, &run.run_id)?)?;
        create_canonical(&active_path, &run)?;
        Ok(run)
    }

    pub fn active_run(
        &self,
        instance_id: &str,
    ) -> Result<Option<LocalInstanceRun>, LocalInstanceError> {
        let path = self.active_run_path(instance_id)?;
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(LocalInstanceError::Io { path, source }),
        };
        let run: LocalInstanceRun = serde_json::from_slice(&bytes)?;
        if run.schema != LOCAL_INSTANCE_RUN_SCHEMA || run.instance_id != instance_id {
            return Err(LocalInstanceError::InvalidState(format!(
                "active Run does not match instance `{instance_id}`"
            )));
        }
        if serde_jcs::to_vec(&run)? != bytes {
            return Err(LocalInstanceError::InvalidState(format!(
                "active Run for `{instance_id}` is not canonical"
            )));
        }
        Ok(Some(run))
    }

    pub fn activate_run(
        &self,
        starting: &LocalInstanceRun,
        activation: LocalRunActivation<'_>,
    ) -> Result<LocalInstanceRun, LocalInstanceError> {
        let current = self
            .active_run(&starting.instance_id)?
            .ok_or_else(|| LocalInstanceError::RunLeaseChanged(starting.instance_id.clone()))?;
        if current.token != starting.token
            || current.run_id != starting.run_id
            || current.status != LocalInstanceRunStatus::Starting
        {
            return Err(LocalInstanceError::RunLeaseChanged(
                starting.instance_id.clone(),
            ));
        }
        let run_root = self.run_root(&starting.instance_id, &starting.run_id)?;
        let receipt_path = run_root.join("verification-receipt.json");
        create_or_verify(&receipt_path, activation.receipt)?;
        let mut active = current;
        active.status = LocalInstanceRunStatus::Active;
        active.pid = Some(activation.pid);
        active.process_start_time = Some(activation.process_start_time);
        active.process_group = Some(activation.process_group);
        active.boot_session = Some(activation.boot_session);
        active.url = Some(activation.url);
        active.receipt_path = Some("verification-receipt.json".to_owned());
        replace_canonical(&self.active_run_path(&starting.instance_id)?, &active)?;
        Ok(active)
    }

    pub fn release_run(&self, instance_id: &str, token: &str) -> Result<(), LocalInstanceError> {
        let Some(active) = self.active_run(instance_id)? else {
            return Ok(());
        };
        if active.token != token {
            return Err(LocalInstanceError::RunLeaseChanged(instance_id.to_owned()));
        }
        let path = self.active_run_path(instance_id)?;
        fs::remove_file(&path).map_err(|source| LocalInstanceError::Io { path, source })
    }

    pub fn run_root(&self, instance_id: &str, run_id: &str) -> Result<PathBuf, LocalInstanceError> {
        validate_id(run_id)?;
        Ok(self.instance_root(instance_id)?.join("runs").join(run_id))
    }

    pub fn stop_request_path(
        &self,
        instance_id: &str,
        run_id: &str,
    ) -> Result<PathBuf, LocalInstanceError> {
        Ok(self.run_root(instance_id, run_id)?.join("stop.request"))
    }

    pub fn stop_ack_path(
        &self,
        instance_id: &str,
        run_id: &str,
    ) -> Result<PathBuf, LocalInstanceError> {
        Ok(self.run_root(instance_id, run_id)?.join("stop.ack"))
    }

    fn application_root(&self, application_id: &str) -> Result<PathBuf, LocalInstanceError> {
        validate_id(application_id)?;
        Ok(self.root.join("apps").join(application_id))
    }

    fn instance_root(&self, instance_id: &str) -> Result<PathBuf, LocalInstanceError> {
        validate_id(instance_id)?;
        Ok(self.root.join("instances").join(instance_id))
    }

    fn active_run_path(&self, instance_id: &str) -> Result<PathBuf, LocalInstanceError> {
        Ok(self.instance_root(instance_id)?.join("active-run.json"))
    }
}

fn application_id(reference: &str) -> Result<String, LocalInstanceError> {
    let digest = digest_hex(reference)?;
    Ok(format!("lapp_{}", &digest[..32]))
}

fn digest_hex(reference: &str) -> Result<&str, LocalInstanceError> {
    let Some(digest) = reference.strip_prefix("sha256:") else {
        return Err(LocalInstanceError::InvalidState(format!(
            "expected SHA-256 reference, got `{reference}`"
        )));
    };
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(LocalInstanceError::InvalidState(format!(
            "invalid SHA-256 reference `{reference}`"
        )));
    }
    Ok(digest)
}

fn new_id(prefix: &str) -> String {
    let counter = LOCAL_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |value| value.as_nanos());
    let mut digest = Sha256::new();
    digest.update(prefix.as_bytes());
    digest.update(std::process::id().to_le_bytes());
    digest.update(nanos.to_le_bytes());
    digest.update(counter.to_le_bytes());
    format!("{prefix}_{:x}", digest.finalize())
}

fn observed_at() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or_else(|_| "0".to_owned(), |value| value.as_millis().to_string())
}

fn validate_id(value: &str) -> Result<(), LocalInstanceError> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(LocalInstanceError::InvalidIdentifier(value.to_owned()));
    }
    Ok(())
}

fn create_dir(path: &Path) -> Result<(), LocalInstanceError> {
    fs::create_dir(path).map_err(|source| LocalInstanceError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn create_dir_all(path: &Path) -> Result<(), LocalInstanceError> {
    fs::create_dir_all(path).map_err(|source| LocalInstanceError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn read(path: &Path) -> Result<Vec<u8>, LocalInstanceError> {
    fs::read(path).map_err(|source| LocalInstanceError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn create_canonical<T: Serialize>(path: &Path, value: &T) -> Result<(), LocalInstanceError> {
    let bytes = serde_jcs::to_vec(value)?;
    create_new(path, &bytes)
}

fn ensure_application(
    path: &Path,
    expected: &LocalApplicationMetadata,
) -> Result<(), LocalInstanceError> {
    match fs::read(path) {
        Ok(bytes) => {
            let existing: LocalApplicationMetadata = serde_json::from_slice(&bytes)?;
            if serde_jcs::to_vec(&existing)? != bytes
                || existing.schema != LOCAL_APPLICATION_SCHEMA
                || existing.application_id != expected.application_id
                || existing.application_ref != expected.application_ref
            {
                return Err(LocalInstanceError::InvalidState(format!(
                    "existing local Application differs at {}",
                    path.display()
                )));
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            create_canonical(path, expected)
        }
        Err(source) => Err(LocalInstanceError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn create_or_verify(path: &Path, bytes: &[u8]) -> Result<(), LocalInstanceError> {
    match create_new(path, bytes) {
        Ok(()) => Ok(()),
        Err(LocalInstanceError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::AlreadyExists =>
        {
            let existing = read(path)?;
            if existing == bytes {
                Ok(())
            } else {
                Err(LocalInstanceError::InvalidState(format!(
                    "existing immutable file differs at {}",
                    path.display()
                )))
            }
        }
        Err(error) => Err(error),
    }
}

fn create_new(path: &Path, bytes: &[u8]) -> Result<(), LocalInstanceError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| LocalInstanceError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|source| LocalInstanceError::Io {
            path: path.to_path_buf(),
            source,
        })
}

fn replace_canonical<T: Serialize>(path: &Path, value: &T) -> Result<(), LocalInstanceError> {
    let bytes = serde_jcs::to_vec(value)?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("state");
    let temporary = path.with_file_name(format!(".{file_name}.{}.tmp", new_id("write")));
    create_new(&temporary, &bytes)?;
    match fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(source) => {
            let _ = fs::remove_file(&temporary);
            Err(LocalInstanceError::Io {
                path: path.to_path_buf(),
                source,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{build_multi_derivation_bundle, build_static_bundle};

    fn fixture_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-static-k")
    }

    fn multi_fixture_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-multi-derivation")
    }

    #[test]
    fn imports_independent_instances_while_reusing_immutable_application_bytes() {
        let root = tempfile::tempdir().unwrap();
        let (bytes, bundle) = build_static_bundle(&fixture_root(), "fixture").unwrap();
        let store = LocalApplicationStore::open(root.path()).unwrap();

        let first = store.import(&bytes, None).unwrap();
        let second = store.import(&bytes, None).unwrap();

        assert_ne!(first.instance_id, second.instance_id);
        assert_eq!(first.application_id, second.application_id);
        assert_eq!(first.bundle_sha256, second.bundle_sha256);
        assert_eq!(first.contract_ref, bundle.index.root_contract_ref);
        assert_eq!(store.bundle_bytes(&first).unwrap(), bytes);
    }

    #[test]
    fn claims_only_one_active_run_and_fences_release_by_token() {
        let root = tempfile::tempdir().unwrap();
        let (bytes, _) = build_static_bundle(&fixture_root(), "fixture").unwrap();
        let store = LocalApplicationStore::open(root.path()).unwrap();
        let instance = store.import(&bytes, None).unwrap();
        let run = store.claim_run(&instance.instance_id).unwrap();

        assert!(store.claim_run(&instance.instance_id).is_err());
        assert!(
            store
                .release_run(&instance.instance_id, "wrong-token")
                .is_err()
        );
        store
            .release_run(&instance.instance_id, &run.token)
            .unwrap();
        assert!(store.active_run(&instance.instance_id).unwrap().is_none());
    }

    #[test]
    fn import_requires_an_explicit_route_when_multiple_derivations_are_declared() {
        let root = tempfile::tempdir().unwrap();
        let (bytes, bundle) =
            build_multi_derivation_bundle(&multi_fixture_root(), "fixture").unwrap();
        let store = LocalApplicationStore::open(root.path()).unwrap();

        assert!(matches!(
            store.import(&bytes, None),
            Err(LocalInstanceError::DerivationRequired { count: 2 })
        ));
        let instance = store
            .import(&bytes, Some(&bundle.index.derivations[0]))
            .unwrap();
        assert_eq!(
            instance.selected_derivation_ref,
            bundle.index.derivations[0]
        );
    }

    #[test]
    fn stored_bundle_tamper_is_rejected_before_a_run_can_start() {
        let root = tempfile::tempdir().unwrap();
        let (bytes, _) = build_static_bundle(&fixture_root(), "fixture").unwrap();
        let store = LocalApplicationStore::open(root.path()).unwrap();
        let instance = store.import(&bytes, None).unwrap();
        let bundle_path = store
            .application_root(&instance.application_id)
            .unwrap()
            .join("bundles")
            .join(format!(
                "{}.capsule",
                digest_hex(&instance.bundle_sha256).unwrap()
            ));

        fs::write(bundle_path, b"tampered").unwrap();

        assert!(
            store
                .bundle_bytes(&instance)
                .unwrap_err()
                .to_string()
                .contains("stored bundle digest mismatch")
        );
    }
}
