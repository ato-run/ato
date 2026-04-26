use std::fs;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use crate::common::paths::nacelle_home_dir_or_workspace_tmp;
use crate::error::{CapsuleError, Result};
use crate::handle::{
    CanonicalHandle, LocalTrustDecisionRecord, ResolvedMetadataCacheEntry, TrustState,
};

const GITHUB_METADATA_TTL_SECONDS: u64 = 60;
const REGISTRY_METADATA_TTL_SECONDS: u64 = 300;
const LOCAL_METADATA_TTL_SECONDS: u64 = 30;

pub fn metadata_cache_ttl_seconds(canonical: &CanonicalHandle) -> u64 {
    match canonical {
        CanonicalHandle::GithubRepo { .. } => GITHUB_METADATA_TTL_SECONDS,
        CanonicalHandle::RegistryCapsule { .. } => REGISTRY_METADATA_TTL_SECONDS,
        CanonicalHandle::LocalPath { .. } => LOCAL_METADATA_TTL_SECONDS,
    }
}

pub fn load_metadata_cache(
    canonical: &CanonicalHandle,
) -> Result<Option<ResolvedMetadataCacheEntry>> {
    let path = metadata_cache_path(canonical);
    if !path.exists() {
        return Ok(None);
    }

    let raw = fs::read_to_string(&path).map_err(|error| {
        CapsuleError::Config(format!(
            "failed to read handle metadata cache {}: {error}",
            path.display()
        ))
    })?;
    serde_json::from_str(&raw).map(Some).map_err(|error| {
        CapsuleError::Config(format!(
            "failed to parse handle metadata cache {}: {error}",
            path.display()
        ))
    })
}

pub fn store_metadata_cache(entry: &ResolvedMetadataCacheEntry) -> Result<()> {
    let path = metadata_cache_path(&entry.canonical);
    write_json(&path, entry)
}

pub fn metadata_cache_is_fresh(entry: &ResolvedMetadataCacheEntry) -> bool {
    let Ok(fetched_at) = DateTime::parse_from_rfc3339(&entry.fetched_at) else {
        return false;
    };
    let age_seconds = Utc::now()
        .signed_duration_since(fetched_at.with_timezone(&Utc))
        .num_seconds();
    age_seconds >= 0 && age_seconds as u64 <= entry.ttl_seconds
}

pub fn load_local_trust_decision(
    canonical: &CanonicalHandle,
) -> Result<Option<LocalTrustDecisionRecord>> {
    let path = local_trust_path(canonical);
    if !path.exists() {
        return Ok(None);
    }

    let raw = fs::read_to_string(&path).map_err(|error| {
        CapsuleError::Config(format!(
            "failed to read local handle trust state {}: {error}",
            path.display()
        ))
    })?;
    serde_json::from_str(&raw).map(Some).map_err(|error| {
        CapsuleError::Config(format!(
            "failed to parse local handle trust state {}: {error}",
            path.display()
        ))
    })
}

pub fn store_local_trust_decision(record: &LocalTrustDecisionRecord) -> Result<()> {
    let path = local_trust_path(&record.canonical);
    write_json(&path, record)
}

pub fn resolve_trust_state(canonical: &CanonicalHandle, default: TrustState) -> Result<TrustState> {
    Ok(load_local_trust_decision(canonical)?
        .map(|record| record.trust_state)
        .unwrap_or(default))
}

pub fn handle_state_root() -> PathBuf {
    nacelle_home_dir_or_workspace_tmp()
        .join("apps")
        .join("desky")
        .join("handles")
}

fn metadata_cache_path(canonical: &CanonicalHandle) -> PathBuf {
    handle_state_root()
        .join("metadata")
        .join(format!("{}.json", handle_key(canonical)))
}

fn local_trust_path(canonical: &CanonicalHandle) -> PathBuf {
    handle_state_root()
        .join("trust")
        .join(format!("{}.json", handle_key(canonical)))
}

fn handle_key(canonical: &CanonicalHandle) -> String {
    let digest = Sha256::digest(canonical.display_string().as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn write_json<T: serde::Serialize>(path: &PathBuf, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            CapsuleError::Config(format!(
                "failed to create handle state directory {}: {error}",
                parent.display()
            ))
        })?;
    }

    let raw = serde_json::to_vec_pretty(value).map_err(|error| {
        CapsuleError::Config(format!(
            "failed to serialize handle state {}: {error}",
            path.display()
        ))
    })?;
    fs::write(path, raw).map_err(|error| {
        CapsuleError::Config(format!(
            "failed to write handle state {}: {error}",
            path.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handle::{CanonicalHandle, RegistryIdentity, ResolvedSnapshot};
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    #[serial_test::serial]
    fn metadata_cache_and_trust_store_are_separate() {
        let _guard = env_lock().lock().expect("env lock");
        let temp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("ATO_HOME", temp.path());

        let canonical = CanonicalHandle::RegistryCapsule {
            registry: RegistryIdentity::ato_official(),
            publisher: "acme".to_string(),
            slug: "chat".to_string(),
            version: None,
        };
        let cache_entry = ResolvedMetadataCacheEntry {
            canonical: canonical.clone(),
            normalized_input: "acme/chat".to_string(),
            manifest_summary: Some("desktop target".to_string()),
            snapshot: Some(ResolvedSnapshot::RegistryRelease {
                version: "1.2.3".to_string(),
                release_id: Some("rel_123".to_string()),
                content_hash: Some("sha256:abc".to_string()),
                fetched_at: Utc::now().to_rfc3339(),
            }),
            fetched_at: Utc::now().to_rfc3339(),
            ttl_seconds: metadata_cache_ttl_seconds(&canonical),
        };
        let trust_record = LocalTrustDecisionRecord {
            canonical: canonical.clone(),
            trust_state: TrustState::Trusted,
            session_scoped: false,
            recorded_at: Utc::now().to_rfc3339(),
            reason: Some("manual-grant".to_string()),
        };

        store_metadata_cache(&cache_entry).expect("store cache");
        store_local_trust_decision(&trust_record).expect("store trust");

        let loaded_cache = load_metadata_cache(&canonical)
            .expect("load cache")
            .expect("cache entry");
        let loaded_trust = load_local_trust_decision(&canonical)
            .expect("load trust")
            .expect("trust entry");

        assert_eq!(
            loaded_cache.manifest_summary.as_deref(),
            Some("desktop target")
        );
        assert_eq!(loaded_trust.trust_state, TrustState::Trusted);
        assert!(metadata_cache_path(&canonical).exists());
        assert!(local_trust_path(&canonical).exists());

        std::env::remove_var("ATO_HOME");
    }
}
