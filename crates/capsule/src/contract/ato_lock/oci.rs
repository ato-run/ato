//! Typed accessors for OCI resolution facts in `ato.lock.json`.
//!
//! Implements Phase 1 (read) and Phase 2 (write) of OCI lock migration.
//! Read path: dual-read from main lock with transparent sidecar fallback.
//! Write path: upsert OCI facts into ato.lock.json resolution section.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::AtoLock;
use crate::error::CapsuleError;
use crate::oci_compose_lock::{self, OciImageLockEntry as SidecarImageLockEntry};
use crate::types::OciPlatform;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OciImageLockEntry {
    pub declared_ref: String,
    pub resolved_ref: String,
    pub resolved_digest: String,
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub provider_semantics: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OciImportEntry {
    pub kind: String,
    pub source_path: String,
    pub source_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OciLockSource {
    MainLock,
    Sidecar,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciLockReadResult {
    pub images: BTreeMap<String, OciImageLockEntry>,
    pub imports: BTreeMap<String, OciImportEntry>,
    pub source: OciLockSource,
    pub warnings: Vec<OciLockReadWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OciLockReadWarning {
    SidecarIgnoredDueToMainLock,
    SidecarParseFailed(String),
}

impl std::fmt::Display for OciLockReadWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SidecarIgnoredDueToMainLock => {
                write!(
                    f,
                    "oci_sidecar_lock_ignored_due_to_main_lock: \
                     ato.lock.json has resolution.oci_images; \
                     ato.oci.lock.json is ignored"
                )
            }
            Self::SidecarParseFailed(detail) => {
                write!(f, "oci_sidecar_lock_parse_failed: {detail}")
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum OciMainLockError {
    #[error("oci_main_lock_parse_failed: {0}")]
    ParseFailed(String),
    #[error("oci_main_lock_validation_failed: {0}")]
    ValidationFailed(String),
}

pub fn oci_images_from_main_lock(
    lock: &AtoLock,
) -> std::result::Result<Option<BTreeMap<String, OciImageLockEntry>>, OciMainLockError> {
    let Some(oci_images_value) = lock.resolution.entries.get("oci_images") else {
        return Ok(None);
    };
    let entries: BTreeMap<String, OciImageLockEntry> =
        serde_json::from_value(oci_images_value.clone()).map_err(|err| {
            OciMainLockError::ParseFailed(format!("resolution.oci_images is invalid: {err}"))
        })?;
    for (target, entry) in &entries {
        validate_oci_image_entry(target, entry).map_err(OciMainLockError::ValidationFailed)?;
    }
    Ok(Some(entries))
}

pub fn oci_imports_from_main_lock(
    lock: &AtoLock,
) -> std::result::Result<Option<BTreeMap<String, OciImportEntry>>, OciMainLockError> {
    let Some(oci_imports_value) = lock.resolution.entries.get("oci_imports") else {
        return Ok(None);
    };
    let entries: BTreeMap<String, OciImportEntry> =
        serde_json::from_value(oci_imports_value.clone()).map_err(|err| {
            OciMainLockError::ParseFailed(format!("resolution.oci_imports is invalid: {err}"))
        })?;
    for (id, entry) in &entries {
        validate_oci_import_entry(id, entry).map_err(OciMainLockError::ValidationFailed)?;
    }
    Ok(Some(entries))
}

fn is_hex_char(ch: char) -> bool {
    ch.is_ascii_hexdigit()
}

fn validate_resolved_ref_shape(target: &str, resolved_ref: &str) -> Result<(), String> {
    if !resolved_ref.contains('@') {
        return Err(format!(
            "resolution.oci_images.{target}: resolved_ref must be a digest pull ref \
             (contain @), got '{resolved_ref}'"
        ));
    }
    let Some((_repo, digest_part)) = resolved_ref.rsplit_once('@') else {
        return Err(format!(
            "resolution.oci_images.{target}: resolved_ref must have exactly one @, \
             got '{resolved_ref}'"
        ));
    };
    if !digest_part.starts_with("sha256:") {
        return Err(format!(
            "resolution.oci_images.{target}: resolved_ref must reference \
             sha256 digest, got '{digest_part}'"
        ));
    }
    let hex_part = &digest_part["sha256:".len()..];
    if hex_part.len() != 64 || !hex_part.chars().all(is_hex_char) {
        return Err(format!(
            "resolution.oci_images.{target}: resolved_ref digest must be \
             sha256:<64 hex>, got '{digest_part}'"
        ));
    }
    Ok(())
}

fn validate_oci_image_entry(target: &str, entry: &OciImageLockEntry) -> Result<(), String> {
    if entry.declared_ref.is_empty() {
        return Err(format!(
            "resolution.oci_images.{target}: declared_ref must not be empty"
        ));
    }
    if entry.resolved_ref.is_empty() {
        return Err(format!(
            "resolution.oci_images.{target}: resolved_ref must not be empty"
        ));
    }
    validate_resolved_ref_shape(target, &entry.resolved_ref)?;
    if entry.resolved_digest.is_empty() {
        return Err(format!(
            "resolution.oci_images.{target}: resolved_digest must not be empty"
        ));
    }
    if !entry.resolved_digest.starts_with("sha256:") {
        return Err(format!(
            "resolution.oci_images.{target}: resolved_digest must start with sha256:"
        ));
    }
    let digest_of_resolved_ref = entry
        .resolved_ref
        .rsplit_once('@')
        .map(|(_, d)| d)
        .unwrap_or("");
    if entry.resolved_digest != digest_of_resolved_ref {
        return Err(format!(
            "resolution.oci_images.{target}: resolved_digest ({digest_dig}) \
             must match digest portion of resolved_ref ({ref_dig})",
            digest_dig = entry.resolved_digest,
            ref_dig = digest_of_resolved_ref
        ));
    }
    if entry.platform.is_empty() {
        return Err(format!(
            "resolution.oci_images.{target}: platform is required"
        ));
    }
    if entry.provider_semantics.is_empty() {
        return Err(format!(
            "resolution.oci_images.{target}: provider_semantics is required"
        ));
    }
    Ok(())
}

fn validate_oci_import_entry(id: &str, entry: &OciImportEntry) -> Result<(), String> {
    if entry.kind.is_empty() {
        return Err(format!(
            "resolution.oci_imports.{id}: kind must not be empty"
        ));
    }
    if entry.source_path.is_empty() {
        return Err(format!(
            "resolution.oci_imports.{id}: source_path must not be empty"
        ));
    }
    if entry.source_path.starts_with('/') || entry.source_path.starts_with('\\') {
        return Err(format!(
            "resolution.oci_imports.{id}: source_path must be project-relative, \
             got absolute: '{}'",
            entry.source_path
        ));
    }
    if entry.source_path.contains('\\') {
        return Err(format!(
            "resolution.oci_imports.{id}: source_path must use '/' separators, \
             not backslash"
        ));
    }
    for component in entry.source_path.split('/') {
        if component == ".." {
            return Err(format!(
                "resolution.oci_imports.{id}: source_path component must not \
                 be '..': '{}'",
                entry.source_path
            ));
        }
    }
    if entry.source_hash.is_empty() {
        return Err(format!(
            "resolution.oci_imports.{id}: source_hash must not be empty"
        ));
    }
    Ok(())
}

pub fn construct_resolved_ref_from_sidecar(declared_ref: &str, resolved_digest: &str) -> String {
    let name = if let Some(at_pos) = declared_ref.rfind('@') {
        &declared_ref[..at_pos]
    } else if let Some(col_pos) = declared_ref.rfind(':') {
        let after_colon = &declared_ref[col_pos + 1..];
        if after_colon.contains('/') {
            declared_ref
        } else {
            &declared_ref[..col_pos]
        }
    } else {
        declared_ref
    };
    let name = name.trim();
    if name.is_empty() {
        return format!("@{resolved_digest}");
    }
    format!("{name}@{resolved_digest}")
}

fn convert_sidecar_entry_to_main(entry: &SidecarImageLockEntry) -> OciImageLockEntry {
    let resolved_ref =
        construct_resolved_ref_from_sidecar(&entry.declared_ref, &entry.resolved_digest);
    OciImageLockEntry {
        declared_ref: entry.declared_ref.clone(),
        resolved_ref,
        resolved_digest: entry.resolved_digest.clone(),
        platform: entry.platform.clone(),
        provider_semantics: entry.provider_semantics.clone(),
        import_id: None,
    }
}

fn convert_sidecar_import_to_main(
    import: &crate::oci_compose_lock::OciImportMeta,
) -> OciImportEntry {
    OciImportEntry {
        kind: import.kind.clone(),
        source_path: import.source_path.clone(),
        source_hash: import.source_hash.clone(),
    }
}

pub fn read_oci_lock(
    lock: &AtoLock,
    project_dir: &Path,
) -> std::result::Result<OciLockReadResult, OciMainLockError> {
    let main_images = oci_images_from_main_lock(lock)?;
    let main_imports = oci_imports_from_main_lock(lock)?;

    match (main_images, main_imports) {
        (Some(images), imports) => {
            let mut warnings = Vec::new();

            match oci_compose_lock::load_from_dir(project_dir) {
                Ok(Some(_)) => {
                    warnings.push(OciLockReadWarning::SidecarIgnoredDueToMainLock);
                }
                Err(err) => {
                    warnings.push(OciLockReadWarning::SidecarParseFailed(err.to_string()));
                }
                Ok(None) => {}
            }

            Ok(OciLockReadResult {
                images,
                imports: imports.unwrap_or_default(),
                source: OciLockSource::MainLock,
                warnings,
            })
        }
        (None, _) => match oci_compose_lock::load_from_dir(project_dir) {
            Ok(Some(sidecar)) => {
                let imports: BTreeMap<String, OciImportEntry> = {
                    let entry = convert_sidecar_import_to_main(&sidecar.import);
                    let mut map = BTreeMap::new();
                    map.insert("default".to_string(), entry);
                    map
                };
                let images: BTreeMap<String, OciImageLockEntry> = sidecar
                    .images
                    .iter()
                    .map(|(name, entry)| (name.clone(), convert_sidecar_entry_to_main(entry)))
                    .collect();

                Ok(OciLockReadResult {
                    images,
                    imports,
                    source: OciLockSource::Sidecar,
                    warnings: Vec::new(),
                })
            }
            Ok(None) => Ok(OciLockReadResult {
                images: BTreeMap::new(),
                imports: BTreeMap::new(),
                source: OciLockSource::None,
                warnings: Vec::new(),
            }),
            Err(err) => {
                let warnings = vec![OciLockReadWarning::SidecarParseFailed(err.to_string())];
                Ok(OciLockReadResult {
                    images: BTreeMap::new(),
                    imports: BTreeMap::new(),
                    source: OciLockSource::None,
                    warnings,
                })
            }
        },
    }
}

pub fn parse_platform_str(s: &str) -> OciPlatform {
    oci_compose_lock::parse_platform_str(s)
}

pub fn upsert_oci_lock_facts(
    lock: &mut AtoLock,
    images: BTreeMap<String, OciImageLockEntry>,
    imports: BTreeMap<String, OciImportEntry>,
) -> std::result::Result<(), OciMainLockError> {
    for (target, entry) in &images {
        validate_oci_image_entry(target, entry).map_err(OciMainLockError::ValidationFailed)?;
    }
    for (id, entry) in &imports {
        validate_oci_import_entry(id, entry).map_err(OciMainLockError::ValidationFailed)?;
    }

    let images_value = serde_json::to_value(&images).map_err(|err| {
        OciMainLockError::ParseFailed(format!("failed to serialize oci_images: {err}"))
    })?;
    let imports_value = serde_json::to_value(&imports).map_err(|err| {
        OciMainLockError::ParseFailed(format!("failed to serialize oci_imports: {err}"))
    })?;

    lock.resolution
        .entries
        .insert("oci_images".to_string(), images_value);
    lock.resolution
        .entries
        .insert("oci_imports".to_string(), imports_value);

    Ok(())
}

pub fn write_oci_facts_to_main_lock(
    project_dir: &Path,
    images: BTreeMap<String, OciImageLockEntry>,
    imports: BTreeMap<String, OciImportEntry>,
) -> Result<(), CapsuleError> {
    use super::{load_unvalidated_from_path, write_pretty_to_path};

    let main_lock_path = project_dir.join("ato.lock.json");
    let mut lock = if main_lock_path.exists() {
        load_unvalidated_from_path(&main_lock_path)?
    } else {
        AtoLock::default()
    };

    upsert_oci_lock_facts(&mut lock, images, imports)
        .map_err(|err| CapsuleError::Config(format!("failed to upsert OCI lock facts: {err}")))?;

    write_pretty_to_path(&lock, &main_lock_path)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::ato_lock;
    use crate::ato_lock::AtoLock;
    use crate::ato_lock::{FeatureName, KnownFeature};
    use crate::oci_compose_lock::{
        OCI_COMPOSE_LOCK_FILE_NAME, OciComposeLock, OciImageLockEntry as SidecarImageLockEntry,
        OciImportMeta,
    };

    fn hex64(c: char) -> String {
        c.to_string().repeat(64)
    }

    fn sha(c: char) -> String {
        format!("sha256:{}", hex64(c))
    }

    fn sample_main_lock_with_oci() -> AtoLock {
        let mut lock = AtoLock::default();
        lock.resolution.entries.insert(
            "oci_images".to_string(),
            json!({
                "db": {
                    "declared_ref": "postgres:14",
                    "resolved_ref": format!("docker.io/library/postgres@{}", sha('a')),
                    "resolved_digest": sha('a'),
                    "platform": "linux/amd64",
                    "provider_semantics": "podman-rootless-native-v1"
                }
            }),
        );
        lock.resolution.entries.insert(
            "oci_imports".to_string(),
            json!({
                "compose-import": {
                    "kind": "compose",
                    "source_path": "docker-compose.yml",
                    "source_hash": sha('b')
                }
            }),
        );
        lock
    }

    fn sample_sidecar_lock() -> OciComposeLock {
        let mut images = BTreeMap::new();
        images.insert(
            "db".to_string(),
            SidecarImageLockEntry {
                declared_ref: "postgres:14".to_string(),
                resolved_digest: "sha256:cccc".to_string(),
                platform: "linux/amd64".to_string(),
                provider_semantics: "podman-rootless-native-v1".to_string(),
            },
        );
        OciComposeLock {
            version: 1,
            import: OciImportMeta {
                kind: "compose".to_string(),
                source_path: "docker-compose.yml".to_string(),
                source_hash: "sha256:dddd".to_string(),
            },
            images,
        }
    }

    fn write_sidecar_lock(dir: &TempDir, lock: &OciComposeLock) {
        let raw = serde_json::to_string_pretty(lock).unwrap();
        std::fs::write(dir.path().join(OCI_COMPOSE_LOCK_FILE_NAME), raw).unwrap();
    }

    #[test]
    fn dual_read_main_lock_wins_over_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let main_lock = sample_main_lock_with_oci();
        write_sidecar_lock(&dir, &sample_sidecar_lock());

        let result = read_oci_lock(&main_lock, dir.path()).unwrap();

        let db = result.images.get("db").unwrap();
        assert_eq!(db.declared_ref, "postgres:14");
        assert_eq!(db.resolved_digest, sha('a'));
        assert!(matches!(result.source, OciLockSource::MainLock));
        assert!(matches!(
            result.warnings.as_slice(),
            [OciLockReadWarning::SidecarIgnoredDueToMainLock]
        ));
    }

    #[test]
    fn dual_read_sidecar_fallback_preserves_execution_identity() {
        let dir = tempfile::tempdir().unwrap();
        let main_lock = AtoLock::default();
        let sidecar = sample_sidecar_lock();
        write_sidecar_lock(&dir, &sidecar);

        let result = read_oci_lock(&main_lock, dir.path()).unwrap();

        let db = result.images.get("db").unwrap();
        assert_eq!(db.declared_ref, "postgres:14");
        assert_eq!(db.resolved_digest, "sha256:cccc");
        assert_eq!(db.platform, "linux/amd64");
        assert_eq!(db.provider_semantics, "podman-rootless-native-v1");
        assert!(
            db.resolved_ref.contains('@'),
            "sidecar fallback must construct a digest pull ref, got: {}",
            db.resolved_ref
        );
        assert!(
            db.resolved_ref.contains("sha256:cccc"),
            "resolved_ref must embed resolved_digest, got: {}",
            db.resolved_ref
        );

        assert!(matches!(result.source, OciLockSource::Sidecar));
        assert!(result.warnings.is_empty());

        let imports = &result.imports;
        assert!(imports.contains_key("default"));
        assert_eq!(imports["default"].kind, "compose");
    }

    #[test]
    fn sidecar_malformed_ignored_when_main_lock_has_oci_entries() {
        let dir = tempfile::tempdir().unwrap();
        let main_lock = sample_main_lock_with_oci();
        std::fs::write(dir.path().join("ato.oci.lock.json"), b"not valid json {{{").unwrap();

        let result = read_oci_lock(&main_lock, dir.path()).unwrap();

        assert!(result.images.contains_key("db"));
        assert!(matches!(result.source, OciLockSource::MainLock));
        assert!(matches!(
            result.warnings.as_slice(),
            [OciLockReadWarning::SidecarParseFailed(_)]
        ));
    }

    #[test]
    fn main_lock_malformed_does_not_silently_fallback_to_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let mut lock = AtoLock::default();
        lock.resolution
            .entries
            .insert("oci_images".to_string(), json!("not_an_object"));
        write_sidecar_lock(&dir, &sample_sidecar_lock());

        let result = read_oci_lock(&lock, dir.path());
        assert!(result.is_err(), "malformed main lock must fail typed");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("oci_main_lock_parse_failed"), "got: {err}");
    }

    #[test]
    fn cached_digest_reuse_requires_matching_platform() {
        let mut lock = AtoLock::default();
        lock.resolution.entries.insert(
            "oci_images".to_string(),
            json!({
                "db": {
                    "declared_ref": "postgres:14",
                    "resolved_ref": format!("docker.io/library/postgres@{}", sha('a')),
                    "resolved_digest": sha('a'),
                    "platform": "linux/amd64",
                    "provider_semantics": "podman-rootless-native-v1"
                }
            }),
        );
        let db = &oci_images_from_main_lock(&lock).unwrap().unwrap()["db"];
        assert_eq!(db.platform, "linux/amd64");

        let mut lock_arm = AtoLock::default();
        lock_arm.resolution.entries.insert(
            "oci_images".to_string(),
            json!({
                "db": {
                    "declared_ref": "postgres:14",
                    "resolved_ref": format!("docker.io/library/postgres@{}", sha('a')),
                    "resolved_digest": sha('a'),
                    "platform": "linux/arm64",
                    "provider_semantics": "podman-rootless-native-v1"
                }
            }),
        );
        let db_arm = &oci_images_from_main_lock(&lock_arm).unwrap().unwrap()["db"];
        assert_eq!(db_arm.platform, "linux/arm64");
        assert_ne!(db.platform, db_arm.platform);
    }

    #[test]
    fn emulation_policy_drift_requires_relock() {
        let mut lock = AtoLock::default();
        lock.resolution.entries.insert(
            "oci_images".to_string(),
            json!({
                "db": {
                    "declared_ref": "postgres:14",
                    "resolved_ref": format!("docker.io/library/postgres@{}", sha('a')),
                    "resolved_digest": sha('a'),
                    "platform": "linux/amd64",
                    "provider_semantics": "podman-rootless-native-v1"
                }
            }),
        );
        let native = &oci_images_from_main_lock(&lock).unwrap().unwrap()["db"];

        let mut lock_emu = AtoLock::default();
        lock_emu.resolution.entries.insert(
            "oci_images".to_string(),
            json!({
                "db": {
                    "declared_ref": "postgres:14",
                    "resolved_ref": format!("docker.io/library/postgres@{}", sha('d')),
                    "resolved_digest": sha('d'),
                    "platform": "linux/amd64",
                    "provider_semantics": "podman-rootless-machine-v1"
                }
            }),
        );
        let emulated = &oci_images_from_main_lock(&lock_emu).unwrap().unwrap()["db"];
        assert_ne!(native.provider_semantics, emulated.provider_semantics);
        assert_ne!(native.resolved_digest, emulated.resolved_digest);
    }

    #[test]
    fn resolved_ref_must_be_digest_pull_ref() {
        let mut lock = AtoLock::default();
        lock.resolution.entries.insert(
            "oci_images".to_string(),
            json!({
                "db": {
                    "declared_ref": "postgres:14",
                    "resolved_ref": format!("docker.io/library/postgres@{}", sha('a')),
                    "resolved_digest": sha('a'),
                    "platform": "linux/amd64",
                    "provider_semantics": "podman-rootless-native-v1"
                }
            }),
        );
        assert!(oci_images_from_main_lock(&lock).is_ok());
    }

    #[test]
    fn resolved_ref_rejects_tag_only_ref() {
        let mut lock = AtoLock::default();
        lock.resolution.entries.insert(
            "oci_images".to_string(),
            json!({
                "db": {
                    "declared_ref": "postgres:14",
                    "resolved_ref": "postgres:14",
                    "resolved_digest": sha('a'),
                    "platform": "linux/amd64",
                    "provider_semantics": "podman-rootless-native-v1"
                }
            }),
        );

        let err = oci_images_from_main_lock(&lock).unwrap_err().to_string();
        assert!(
            err.contains("digest pull ref") || err.contains("contain @"),
            "got: {err}"
        );
    }

    #[test]
    fn resolved_ref_rejects_non_sha256_digest() {
        let mut lock = AtoLock::default();
        lock.resolution.entries.insert(
            "oci_images".to_string(),
            json!({
                "db": {
                    "declared_ref": "postgres:14",
                    "resolved_ref": format!("docker.io/library/postgres@md5:{}", hex64('a')),
                    "resolved_digest": sha('a'),
                    "platform": "linux/amd64",
                    "provider_semantics": "podman-rootless-native-v1"
                }
            }),
        );

        let err = oci_images_from_main_lock(&lock).unwrap_err().to_string();
        assert!(err.contains("sha256"), "got: {err}");
    }

    #[test]
    fn resolved_ref_rejects_wrong_digest_length() {
        let mut lock = AtoLock::default();
        lock.resolution.entries.insert(
            "oci_images".to_string(),
            json!({
                "db": {
                    "declared_ref": "postgres:14",
                    "resolved_ref": "docker.io/library/postgres@sha256:deadbeef",
                    "resolved_digest": sha('a'),
                    "platform": "linux/amd64",
                    "provider_semantics": "podman-rootless-native-v1"
                }
            }),
        );

        let err = oci_images_from_main_lock(&lock).unwrap_err().to_string();
        assert!(err.contains("64 hex"), "got: {err}");
    }

    #[test]
    fn resolved_ref_digest_must_match_resolved_digest() {
        let mut lock = AtoLock::default();
        lock.resolution.entries.insert(
            "oci_images".to_string(),
            json!({
                "db": {
                    "declared_ref": "postgres:14",
                    "resolved_ref": format!("docker.io/library/postgres@{}", sha('a')),
                    "resolved_digest": sha('b'),
                    "platform": "linux/amd64",
                    "provider_semantics": "podman-rootless-native-v1"
                }
            }),
        );

        let err = oci_images_from_main_lock(&lock).unwrap_err().to_string();
        assert!(err.contains("must match"), "got: {err}");
    }

    #[test]
    fn sidecar_fallback_resolved_ref_is_valid_digest_pull_ref() {
        let entry = SidecarImageLockEntry {
            declared_ref: "postgres:14".to_string(),
            resolved_digest: sha('c'),
            platform: "linux/amd64".to_string(),
            provider_semantics: "podman-rootless-native-v1".to_string(),
        };
        let main_entry = convert_sidecar_entry_to_main(&entry);
        assert!(main_entry.resolved_ref.contains('@'));
        assert!(main_entry.resolved_ref.contains(&entry.resolved_digest));
        assert!(main_entry.resolved_ref.starts_with("postgres@"));
    }

    #[test]
    fn construct_resolved_ref_from_sidecar_handles_tag() {
        let r = construct_resolved_ref_from_sidecar("postgres:14", "sha256:abc");
        assert_eq!(r, "postgres@sha256:abc");
    }

    #[test]
    fn construct_resolved_ref_from_sidecar_handles_digest_ref() {
        let r = construct_resolved_ref_from_sidecar("postgres@sha256:old", "sha256:new");
        assert_eq!(r, "postgres@sha256:new");
    }

    #[test]
    fn construct_resolved_ref_from_sidecar_handles_registry_path() {
        let r =
            construct_resolved_ref_from_sidecar("ghcr.io/blinkospace/blinko:latest", "sha256:abc");
        assert_eq!(r, "ghcr.io/blinkospace/blinko@sha256:abc");
    }

    #[test]
    fn oci_import_source_path_rejects_absolute_path() {
        let mut lock = AtoLock::default();
        lock.resolution.entries.insert(
            "oci_imports".to_string(),
            json!({
                "i": {
                    "kind": "compose",
                    "source_path": "/abs/compose.yml",
                    "source_hash": sha('a')
                }
            }),
        );

        let err = oci_imports_from_main_lock(&lock).unwrap_err().to_string();
        assert!(err.contains("project-relative"), "got: {err}");
    }

    #[test]
    fn oci_import_source_path_rejects_parent_component() {
        let mut lock = AtoLock::default();
        lock.resolution.entries.insert(
            "oci_imports".to_string(),
            json!({
                "i": {
                    "kind": "compose",
                    "source_path": "../compose.yml",
                    "source_hash": sha('a')
                }
            }),
        );

        let err = oci_imports_from_main_lock(&lock).unwrap_err().to_string();
        assert!(err.contains("must not be '..'"), "got: {err}");
    }

    #[test]
    fn oci_import_source_path_allows_dots_in_filenames() {
        let mut lock = AtoLock::default();
        lock.resolution.entries.insert(
            "oci_imports".to_string(),
            json!({
                "i": {
                    "kind": "compose",
                    "source_path": "configs/v1.backup/compose.yml",
                    "source_hash": sha('a')
                }
            }),
        );

        assert!(oci_imports_from_main_lock(&lock).is_ok());
    }

    #[test]
    fn oci_import_source_path_rejects_nested_parent_component() {
        let mut lock = AtoLock::default();
        lock.resolution.entries.insert(
            "oci_imports".to_string(),
            json!({
                "i": {
                    "kind": "compose",
                    "source_path": "sub/../compose.yml",
                    "source_hash": sha('a')
                }
            }),
        );

        assert!(oci_imports_from_main_lock(&lock).is_err());
    }

    #[test]
    fn oci_import_source_path_accepts_project_relative_path() {
        let mut lock = AtoLock::default();
        lock.resolution.entries.insert(
            "oci_imports".to_string(),
            json!({
                "i": {
                    "kind": "compose",
                    "source_path": "sub/compose.yml",
                    "source_hash": sha('a')
                }
            }),
        );

        let imports = oci_imports_from_main_lock(&lock).unwrap().unwrap();
        assert_eq!(imports["i"].source_path, "sub/compose.yml");
    }

    #[test]
    fn oci_images_entry_with_import_id_preserves_field() {
        let mut lock = AtoLock::default();
        lock.resolution.entries.insert(
            "oci_images".to_string(),
            json!({
                "db": {
                    "declared_ref": "postgres:14",
                    "resolved_ref": format!("docker.io/library/postgres@{}", sha('a')),
                    "resolved_digest": sha('a'),
                    "platform": "linux/amd64",
                    "provider_semantics": "podman-rootless-native-v1",
                    "import_id": "compose-import"
                }
            }),
        );

        let db = &oci_images_from_main_lock(&lock).unwrap().unwrap()["db"];
        assert_eq!(db.import_id.as_deref(), Some("compose-import"));
    }

    #[test]
    fn oci_lock_read_warning_display_is_observable() {
        let w = OciLockReadWarning::SidecarIgnoredDueToMainLock;
        let s = w.to_string();
        assert!(s.contains("oci_sidecar_lock_ignored_due_to_main_lock"));
        assert!(s.contains("ato.lock.json"));
        assert!(s.contains("ato.oci.lock.json"));

        let w2 = OciLockReadWarning::SidecarParseFailed("bad json".to_string());
        let s2 = w2.to_string();
        assert!(s2.contains("oci_sidecar_lock_parse_failed"));
        assert!(s2.contains("bad json"));
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Phase 2: Write path tests
    // ═══════════════════════════════════════════════════════════════════════════

    fn sample_image_entries() -> BTreeMap<String, OciImageLockEntry> {
        let mut images = BTreeMap::new();
        images.insert(
            "db".to_string(),
            OciImageLockEntry {
                declared_ref: "postgres:14".to_string(),
                resolved_ref: format!("docker.io/library/postgres@{}", sha('a')),
                resolved_digest: sha('a'),
                platform: "linux/amd64".to_string(),
                provider_semantics: "podman-rootless-native-v1".to_string(),
                import_id: Some("default".to_string()),
            },
        );
        images.insert(
            "app".to_string(),
            OciImageLockEntry {
                declared_ref: "example/myapp:1.0".to_string(),
                resolved_ref: format!("example/myapp@{}", sha('f')),
                resolved_digest: sha('f'),
                platform: "linux/arm64".to_string(),
                provider_semantics: "podman-rootless-native-v1".to_string(),
                import_id: Some("default".to_string()),
            },
        );
        images
    }

    fn sample_import_entries() -> BTreeMap<String, OciImportEntry> {
        let mut imports = BTreeMap::new();
        imports.insert(
            "default".to_string(),
            OciImportEntry {
                kind: "compose".to_string(),
                source_path: "docker-compose.yml".to_string(),
                source_hash: sha('b'),
            },
        );
        imports
    }

    #[test]
    fn main_lock_write_creates_ato_lock_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let main_lock_path = dir.path().join("ato.lock.json");
        assert!(!main_lock_path.exists());

        write_oci_facts_to_main_lock(dir.path(), sample_image_entries(), sample_import_entries())
            .expect("write_oci_facts_to_main_lock should create lock when missing");

        assert!(main_lock_path.exists(), "ato.lock.json should be created");

        let lock = ato_lock::load_unvalidated_from_path(&main_lock_path).unwrap();
        let images = oci_images_from_main_lock(&lock)
            .unwrap()
            .expect("oci_images should be present");
        let imports = oci_imports_from_main_lock(&lock)
            .unwrap()
            .expect("oci_imports should be present");

        assert!(images.contains_key("db"));
        assert!(images.contains_key("app"));
        assert_eq!(images["db"].declared_ref, "postgres:14");
        assert_eq!(imports["default"].kind, "compose");
    }

    #[test]
    fn main_lock_write_preserves_unrelated_resolution_entries() {
        let dir = tempfile::tempdir().unwrap();
        let main_lock_path = dir.path().join("ato.lock.json");

        let mut preexisting = AtoLock::default();
        preexisting.resolution.entries.insert(
            "runtime".to_string(),
            json!({"kind": "deno", "version": "2.1.3"}),
        );
        ato_lock::write_pretty_to_path(&preexisting, &main_lock_path).unwrap();

        write_oci_facts_to_main_lock(dir.path(), sample_image_entries(), sample_import_entries())
            .unwrap();

        let lock = ato_lock::load_unvalidated_from_path(&main_lock_path).unwrap();
        assert!(
            lock.resolution.entries.contains_key("runtime"),
            "existing runtime entry must be preserved"
        );
        assert!(
            lock.resolution.entries.contains_key("oci_images"),
            "oci_images must be added"
        );
        assert!(
            lock.resolution.entries.contains_key("oci_imports"),
            "oci_imports must be added"
        );

        let runtime = lock.resolution.entries.get("runtime").unwrap();
        assert_eq!(runtime["kind"], "deno");
    }

    #[test]
    fn main_lock_write_upserts_oci_images() {
        let dir = tempfile::tempdir().unwrap();
        let main_lock_path = dir.path().join("ato.lock.json");

        let initial_images = sample_image_entries();
        let initial_imports = sample_import_entries();
        write_oci_facts_to_main_lock(dir.path(), initial_images, initial_imports).unwrap();

        let mut updated_images = sample_image_entries();
        updated_images.insert(
            "cache".to_string(),
            OciImageLockEntry {
                declared_ref: "redis:7".to_string(),
                resolved_ref: format!("docker.io/library/redis@{}", sha('c')),
                resolved_digest: sha('c'),
                platform: "linux/amd64".to_string(),
                provider_semantics: "podman-rootless-native-v1".to_string(),
                import_id: Some("default".to_string()),
            },
        );
        write_oci_facts_to_main_lock(dir.path(), updated_images, sample_import_entries()).unwrap();

        let lock = ato_lock::load_unvalidated_from_path(&main_lock_path).unwrap();
        let images = oci_images_from_main_lock(&lock)
            .unwrap()
            .expect("oci_images");
        assert!(images.contains_key("db"));
        assert!(images.contains_key("app"));
        assert!(images.contains_key("cache"));
        assert_eq!(images["cache"].declared_ref, "redis:7");
    }

    #[test]
    fn main_lock_write_validates_resolved_ref_digest_match() {
        let mut images = BTreeMap::new();
        images.insert(
            "db".to_string(),
            OciImageLockEntry {
                declared_ref: "postgres:14".to_string(),
                resolved_ref: format!("docker.io/library/postgres@{}", sha('a')),
                resolved_digest: sha('b'),
                platform: "linux/amd64".to_string(),
                provider_semantics: "podman-rootless-native-v1".to_string(),
                import_id: Some("default".to_string()),
            },
        );
        let imports = BTreeMap::new();

        let mut lock = AtoLock::default();
        let err = upsert_oci_lock_facts(&mut lock, images, imports).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("validation"), "got: {msg}");
    }

    #[test]
    fn main_lock_write_is_stable_across_repeated_runs() {
        let dir = tempfile::tempdir().unwrap();
        let main_lock_path = dir.path().join("ato.lock.json");

        write_oci_facts_to_main_lock(dir.path(), sample_image_entries(), sample_import_entries())
            .unwrap();
        let first = std::fs::read_to_string(&main_lock_path).unwrap();

        std::fs::remove_file(&main_lock_path).unwrap();
        write_oci_facts_to_main_lock(dir.path(), sample_image_entries(), sample_import_entries())
            .unwrap();
        let second = std::fs::read_to_string(&main_lock_path).unwrap();

        assert_eq!(
            first, second,
            "lock output should be stable across repeated writes"
        );
    }

    #[test]
    fn main_lock_write_preserves_lock_id_after_upsert() {
        let dir = tempfile::tempdir().unwrap();
        let main_lock_path = dir.path().join("ato.lock.json");

        write_oci_facts_to_main_lock(dir.path(), sample_image_entries(), sample_import_entries())
            .unwrap();
        let _first = ato_lock::load_unvalidated_from_path(&main_lock_path).expect("first lock");

        write_oci_facts_to_main_lock(dir.path(), sample_image_entries(), sample_import_entries())
            .unwrap();
        let second = ato_lock::load_unvalidated_from_path(&main_lock_path).expect("second lock");

        assert!(second.lock_id.is_some());
        let images = oci_images_from_main_lock(&second)
            .unwrap()
            .expect("oci_images");
        assert_eq!(images.len(), 2);
    }

    #[test]
    fn upsert_oci_lock_facts_rejects_mismatched_digest() {
        let mut images = BTreeMap::new();
        images.insert(
            "db".to_string(),
            OciImageLockEntry {
                declared_ref: "postgres:14".to_string(),
                resolved_ref: format!("docker.io/library/postgres@{}", sha('a')),
                resolved_digest: sha('c'),
                platform: "linux/amd64".to_string(),
                provider_semantics: "podman-rootless-native-v1".to_string(),
                import_id: Some("default".to_string()),
            },
        );
        let imports = BTreeMap::new();
        let mut lock = AtoLock::default();
        assert!(upsert_oci_lock_facts(&mut lock, images, imports).is_err());
    }

    #[test]
    fn upsert_oci_lock_facts_rejects_invalid_import_path() {
        let images = sample_image_entries();
        let mut imports = BTreeMap::new();
        imports.insert(
            "bad".to_string(),
            OciImportEntry {
                kind: "compose".to_string(),
                source_path: "/absolute/path.yml".to_string(),
                source_hash: sha('a'),
            },
        );
        let mut lock = AtoLock::default();
        assert!(upsert_oci_lock_facts(&mut lock, images, imports).is_err());
    }

    #[test]
    fn dual_read_after_main_write_uses_main_lock() {
        let dir = tempfile::tempdir().unwrap();

        write_oci_facts_to_main_lock(dir.path(), sample_image_entries(), sample_import_entries())
            .unwrap();

        write_sidecar_lock(&dir, &sample_sidecar_lock());

        let main_lock_path = dir.path().join("ato.lock.json");
        let lock = ato_lock::load_unvalidated_from_path(&main_lock_path).unwrap();
        let result = read_oci_lock(&lock, dir.path()).unwrap();

        assert!(
            matches!(result.source, OciLockSource::MainLock),
            "must use main lock even when sidecar exists"
        );
        assert!(result.images.contains_key("db"));
        assert!(result.images.contains_key("app"));
        assert_eq!(result.images["db"].resolved_digest, sha('a'));
    }

    #[test]
    fn write_then_read_roundtrip_produces_valid_lock() {
        let dir = tempfile::tempdir().unwrap();

        write_oci_facts_to_main_lock(dir.path(), sample_image_entries(), sample_import_entries())
            .unwrap();

        let main_lock_path = dir.path().join("ato.lock.json");

        let lock = ato_lock::load_unvalidated_from_path(&main_lock_path).unwrap();
        assert_eq!(
            lock.schema_version,
            crate::ato_lock::ATO_LOCK_SCHEMA_VERSION
        );

        ato_lock::validate_structural_non_strict(&lock).unwrap();

        let images = oci_images_from_main_lock(&lock)
            .unwrap()
            .expect("oci_images");
        assert_eq!(images.len(), 2);
        let imports = oci_imports_from_main_lock(&lock)
            .unwrap()
            .expect("oci_imports");
        assert_eq!(imports.len(), 1);
    }

    #[test]
    fn write_oci_facts_preserves_existing_contract_and_features() {
        let dir = tempfile::tempdir().unwrap();
        let main_lock_path = dir.path().join("ato.lock.json");

        let mut preexisting = AtoLock::default();
        preexisting.features.declared = vec![FeatureName::Known(KnownFeature::ReadOnlyRootFs)];
        preexisting
            .contract
            .entries
            .insert("process".to_string(), json!({"driver": "deno"}));
        ato_lock::write_pretty_to_path(&preexisting, &main_lock_path).unwrap();

        write_oci_facts_to_main_lock(dir.path(), sample_image_entries(), sample_import_entries())
            .unwrap();

        let lock = ato_lock::load_unvalidated_from_path(&main_lock_path).unwrap();
        assert!(lock.contract.entries.contains_key("process"));
        assert_eq!(lock.features.declared.len(), 1);
    }
}
