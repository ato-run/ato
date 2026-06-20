use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ato_protocol::handle::{ResolvedSnapshot, TrustState};
use blake3::Hasher;
use capsule_core::common::paths::ato_path;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

const LAUNCH_CACHE_ROOT_ENV: &str = "ATO_DESKTOP_LAUNCH_CACHE_ROOT";
pub const MATERIALIZED_LAUNCH_RECORD_SCHEMA_VERSION: u32 = 2;
const RUN_CONFIG_HASH_VERSION: &str = "ato-run-config-v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MaterializedLaunchRecord {
    pub schema_version: u32,
    pub launch_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_session_id: Option<String>,
    pub handle: String,
    pub normalized_handle: String,
    pub canonical_handle: Option<String>,
    pub trust_state: TrustState,
    pub source: Option<String>,
    pub restricted: bool,
    pub snapshot: Option<ResolvedSnapshot>,
    pub target_label: String,
    pub manifest_path: String,
    pub app_root: String,
    pub platform: String,
    pub launch_digest: String,
    pub run_config_hash: String,
    pub created_at_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_receipt_schema_version: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterializedLaunchStaleReason {
    SchemaTooOld,
    MissingManifestPath,
    MissingAppRoot,
    ManifestOutsideAppRoot,
}

impl MaterializedLaunchStaleReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SchemaTooOld => "schema-too-old",
            Self::MissingManifestPath => "missing-manifest-path",
            Self::MissingAppRoot => "missing-app-root",
            Self::ManifestOutsideAppRoot => "manifest-outside-app-root",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaterializedLaunchValidationOutcome {
    Valid,
    Stale {
        reason: MaterializedLaunchStaleReason,
    },
}

pub fn launch_cache_root() -> Result<PathBuf> {
    if let Ok(path) = std::env::var(LAUNCH_CACHE_ROOT_ENV) {
        return Ok(PathBuf::from(path));
    }
    ato_path("apps/ato-desktop/launch-cache")
        .context("failed to resolve ato home for materialized launch cache")
}

pub fn materialized_launch_record_path(root: &Path, launch_key: &str) -> PathBuf {
    root.join(format!("{}.json", launch_key.trim_start_matches("blake3:")))
}

pub fn read_materialized_launch_record(path: &Path) -> Result<MaterializedLaunchRecord> {
    let raw = fs::read_to_string(path).with_context(|| {
        format!(
            "failed to read materialized launch record {}",
            path.display()
        )
    })?;
    serde_json::from_str(&raw).with_context(|| {
        format!(
            "failed to parse materialized launch record {}",
            path.display()
        )
    })
}

pub fn read_materialized_launch_records(root: &Path) -> Result<Vec<MaterializedLaunchRecord>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    let entries = fs::read_dir(root)
        .with_context(|| format!("failed to read launch cache root {}", root.display()))?;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                debug!(error = %err, "skipping unreadable launch cache entry");
                continue;
            }
        };
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        match read_materialized_launch_record(&path) {
            Ok(record) => records.push(record),
            Err(err) => warn!(
                path = %path.display(),
                error = %err,
                "skipping malformed materialized launch record"
            ),
        }
    }
    Ok(records)
}

pub fn write_materialized_launch_record_atomic(
    root: &Path,
    record: &MaterializedLaunchRecord,
) -> Result<()> {
    fs::create_dir_all(root)
        .with_context(|| format!("failed to create launch cache root {}", root.display()))?;
    let final_path = materialized_launch_record_path(root, &record.launch_key);
    let tmp_path = root.join(format!(
        ".{}.json.tmp.{}",
        record.launch_key.trim_start_matches("blake3:"),
        std::process::id()
    ));
    let payload = serde_json::to_vec_pretty(record).with_context(|| {
        format!(
            "failed to encode materialized launch record {}",
            record.launch_key
        )
    })?;

    {
        let mut tmp = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp_path)
            .with_context(|| format!("failed to open temp record {}", tmp_path.display()))?;
        tmp.write_all(&payload)
            .with_context(|| format!("failed to write temp record {}", tmp_path.display()))?;
        let _ = tmp.sync_all();
    }

    if let Err(err) = fs::rename(&tmp_path, &final_path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(err).with_context(|| {
            format!(
                "failed to rename {} -> {}",
                tmp_path.display(),
                final_path.display()
            )
        });
    }
    Ok(())
}

fn update_hash_text(hasher: &mut Hasher, value: &str) {
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

pub fn current_platform_tag() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

pub fn compute_run_config_hash(
    plain_configs: &[(String, String)],
    secret_keys: &[String],
    platform: &str,
) -> String {
    let mut plain_configs = plain_configs.to_vec();
    plain_configs.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

    let mut secret_keys = secret_keys.to_vec();
    secret_keys.sort();
    secret_keys.dedup();

    let mut hasher = Hasher::new();
    update_hash_text(&mut hasher, RUN_CONFIG_HASH_VERSION);
    update_hash_text(&mut hasher, platform);
    update_hash_text(&mut hasher, "plain-configs");
    for (key, value) in plain_configs {
        update_hash_text(&mut hasher, &key);
        update_hash_text(&mut hasher, &value);
    }
    update_hash_text(&mut hasher, "secret-keys");
    for key in secret_keys {
        update_hash_text(&mut hasher, &key);
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

pub fn validate_materialized_launch_record(
    record: &MaterializedLaunchRecord,
) -> Result<MaterializedLaunchValidationOutcome> {
    if record.schema_version < MATERIALIZED_LAUNCH_RECORD_SCHEMA_VERSION {
        return Ok(MaterializedLaunchValidationOutcome::Stale {
            reason: MaterializedLaunchStaleReason::SchemaTooOld,
        });
    }

    let manifest_path = PathBuf::from(&record.manifest_path);
    if !manifest_path.exists() {
        return Ok(MaterializedLaunchValidationOutcome::Stale {
            reason: MaterializedLaunchStaleReason::MissingManifestPath,
        });
    }

    let app_root = PathBuf::from(&record.app_root);
    if !app_root.exists() {
        return Ok(MaterializedLaunchValidationOutcome::Stale {
            reason: MaterializedLaunchStaleReason::MissingAppRoot,
        });
    }

    let canonical_manifest = manifest_path
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", manifest_path.display()))?;
    let canonical_root = app_root
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", app_root.display()))?;
    if !canonical_manifest.starts_with(&canonical_root) {
        return Ok(MaterializedLaunchValidationOutcome::Stale {
            reason: MaterializedLaunchStaleReason::ManifestOutsideAppRoot,
        });
    }

    Ok(MaterializedLaunchValidationOutcome::Valid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ato_protocol::handle::TrustState;
    use tempfile::tempdir;

    fn sample_record(dir: &Path) -> MaterializedLaunchRecord {
        let app_root = dir.join("workspace");
        let manifest_path = app_root.join("capsule.toml");
        fs::create_dir_all(&app_root).expect("app root");
        fs::write(&manifest_path, "name = 'demo'\n").expect("manifest");
        MaterializedLaunchRecord {
            schema_version: MATERIALIZED_LAUNCH_RECORD_SCHEMA_VERSION,
            launch_key: "blake3:launch-key".to_string(),
            last_session_id: Some("ato-desktop-session-1".to_string()),
            handle: "github.com/example/demo".to_string(),
            normalized_handle: "github.com/example/demo".to_string(),
            canonical_handle: None,
            trust_state: TrustState::Untrusted,
            source: Some("github".to_string()),
            restricted: true,
            snapshot: None,
            target_label: "main".to_string(),
            manifest_path: manifest_path.display().to_string(),
            app_root: app_root.display().to_string(),
            platform: current_platform_tag(),
            launch_digest: "blake3:test".to_string(),
            run_config_hash: "blake3:cfg".to_string(),
            created_at_unix_ms: 1,
            execution_id: None,
            execution_receipt_schema_version: None,
        }
    }

    #[test]
    fn write_then_read_round_trips_materialized_record() {
        let dir = tempdir().expect("tempdir");
        let record = sample_record(dir.path());
        write_materialized_launch_record_atomic(dir.path(), &record).expect("write");

        let path = materialized_launch_record_path(dir.path(), &record.launch_key);
        let loaded = read_materialized_launch_record(&path).expect("read");
        assert_eq!(loaded, record);
    }

    #[test]
    fn validation_rejects_missing_manifest() {
        let dir = tempdir().expect("tempdir");
        let mut record = sample_record(dir.path());
        fs::remove_file(PathBuf::from(&record.manifest_path)).expect("remove manifest");

        let outcome = validate_materialized_launch_record(&record).expect("validate");
        assert_eq!(
            outcome,
            MaterializedLaunchValidationOutcome::Stale {
                reason: MaterializedLaunchStaleReason::MissingManifestPath
            }
        );

        record.schema_version = MATERIALIZED_LAUNCH_RECORD_SCHEMA_VERSION - 1;
        let outcome = validate_materialized_launch_record(&record).expect("validate");
        assert_eq!(
            outcome,
            MaterializedLaunchValidationOutcome::Stale {
                reason: MaterializedLaunchStaleReason::SchemaTooOld
            }
        );
    }

    #[test]
    fn run_config_hash_is_stable_across_key_order() {
        let a = compute_run_config_hash(
            &[
                ("MODEL".to_string(), "gpt-5".to_string()),
                ("PORT".to_string(), "3000".to_string()),
            ],
            &["API_KEY".to_string(), "TOKEN".to_string()],
            "macos-arm64",
        );
        let b = compute_run_config_hash(
            &[
                ("PORT".to_string(), "3000".to_string()),
                ("MODEL".to_string(), "gpt-5".to_string()),
            ],
            &["TOKEN".to_string(), "API_KEY".to_string()],
            "macos-arm64",
        );
        assert_eq!(a, b);
    }

    #[test]
    fn run_config_hash_changes_when_plain_config_changes() {
        let before = compute_run_config_hash(
            &[("MODEL".to_string(), "gpt-5".to_string())],
            &["API_KEY".to_string()],
            "macos-arm64",
        );
        let after = compute_run_config_hash(
            &[("MODEL".to_string(), "gpt-5-mini".to_string())],
            &["API_KEY".to_string()],
            "macos-arm64",
        );
        assert_ne!(before, after);
    }

    #[test]
    fn run_config_hash_changes_when_secret_key_set_changes() {
        let before = compute_run_config_hash(&[], &["API_KEY".to_string()], "macos-arm64");
        let after = compute_run_config_hash(
            &[],
            &["API_KEY".to_string(), "TOKEN".to_string()],
            "macos-arm64",
        );
        assert_ne!(before, after);
    }
}
