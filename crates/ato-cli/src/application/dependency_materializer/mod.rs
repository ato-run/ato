#![allow(dead_code)]

pub mod freeze;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use capsule_core::blob::BlobManifest;
use capsule_core::common::paths::{
    ato_run_layout, ato_store_attestations_dir, ato_store_blobs_dir, ato_store_refs_dir,
    ato_trust_policies_dir, ato_trust_roots_dir, AtoRunLayout,
};
use capsule_core::common::store::{ato_store_dep_ref_path, BlobAddress};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheStrategy {
    None,
    DerivationCache,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttestationStrategy {
    None,
    LocalSign,
    RemoteLog,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReproducibilityClass {
    Portable,
    HostBound,
    BestEffort,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheLookupResult {
    Disabled,
    Hit { blob_hash: String },
    Miss,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeSelection {
    pub name: String,
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformTriple {
    pub os: String,
    pub arch: String,
    pub libc: Option<String>,
    pub abi: Option<String>,
}

impl PlatformTriple {
    pub fn current() -> Self {
        Self {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            libc: detect_libc(),
            abi: None,
        }
    }

    pub fn as_string(&self) -> String {
        let mut value = format!("{}-{}", self.os, self.arch);
        if let Some(libc) = self.libc.as_deref().filter(|value| !value.is_empty()) {
            value.push('-');
            value.push_str(libc);
        }
        if let Some(abi) = self.abi.as_deref().filter(|value| !value.is_empty()) {
            value.push('-');
            value.push_str(abi);
        }
        value
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ManifestInputs {
    pub lockfile_digest: Option<String>,
    pub package_manifest_digest: Option<String>,
    pub workspace_manifest_digest: Option<String>,
    pub path_dependency_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct InstallPolicies {
    pub lifecycle_script_policy: String,
    pub registry_policy: String,
    pub network_policy: String,
    pub env_allowlist_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyMaterializationRequest {
    pub session_id: String,
    pub capsule_id: String,
    pub source_root: PathBuf,
    pub workspace_root: PathBuf,
    pub ecosystem: String,
    pub package_manager: Option<String>,
    pub package_manager_version: Option<String>,
    pub runtime: RuntimeSelection,
    pub manifests: ManifestInputs,
    pub policies: InstallPolicies,
    pub platform: PlatformTriple,
    pub cache_strategy: CacheStrategy,
    pub attestation_strategy: AttestationStrategy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyPlan {
    pub derivation_hash: String,
    pub reproducibility: ReproducibilityClass,
    pub cache_lookup: CacheLookupResult,
    pub required_runtime_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ReproducibilityMeta {
    pub class: Option<ReproducibilityClass>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyProjection {
    pub derivation_hash: Option<String>,
    pub blob_hash: Option<String>,
    pub execution_deps_path: PathBuf,
    pub run_workspace: PathBuf,
    pub env: BTreeMap<String, String>,
    pub cache_dirs: BTreeMap<String, PathBuf>,
    pub reproducibility_metadata: ReproducibilityMeta,
    pub attestation_refs: Vec<String>,
    pub dependency_cache_status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationResult {
    pub ok: bool,
    pub advisory: bool,
    pub messages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepDerivationKeyV1 {
    pub schema_version: u32,
    pub ecosystem: String,
    pub package_manager: Option<String>,
    pub package_manager_compat_class: Option<String>,
    pub runtime_compat_class: String,
    pub platform_triple: String,
    pub lockfile_digest: Option<String>,
    pub manifest_digest: Option<String>,
    pub path_dependency_digest: Option<String>,
    pub install_policy_digest: String,
    pub env_allowlist_digest: Option<String>,
}

impl DepDerivationKeyV1 {
    pub fn from_request(req: &DependencyMaterializationRequest) -> Self {
        Self {
            schema_version: 1,
            ecosystem: req.ecosystem.clone(),
            package_manager: req.package_manager.clone(),
            package_manager_compat_class: package_manager_compat_class(
                req.package_manager.as_deref(),
                req.package_manager_version.as_deref(),
            ),
            runtime_compat_class: runtime_compat_class(
                &req.runtime.name,
                req.runtime.version.as_deref(),
            ),
            platform_triple: req.platform.as_string(),
            lockfile_digest: req.manifests.lockfile_digest.clone(),
            manifest_digest: req
                .manifests
                .package_manifest_digest
                .clone()
                .or_else(|| req.manifests.workspace_manifest_digest.clone()),
            path_dependency_digest: req.manifests.path_dependency_digest.clone(),
            install_policy_digest: install_policy_digest(&req.policies),
            env_allowlist_digest: req.policies.env_allowlist_digest.clone(),
        }
    }

    pub fn derivation_hash(&self) -> Result<String> {
        let canonical = serde_jcs::to_vec(self).context("failed to canonicalize derivation key")?;
        Ok(format!(
            "sha256:{}",
            hex::encode(Sha256::digest(&canonical))
        ))
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        serde_jcs::to_vec(self).context("failed to canonicalize derivation key")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceResolutionRecord {
    pub authority: String,
    pub repository: Option<String>,
    pub requested_ref: Option<String>,
    pub resolved_commit: String,
    pub resolved_at: String,
    pub commit_signature_verdict: Option<String>,
}

impl SourceResolutionRecord {
    pub fn identity_hash(&self) -> Result<String> {
        #[derive(Serialize)]
        struct IdentityProjection<'a> {
            authority: &'a str,
            repository: &'a Option<String>,
            resolved_commit: &'a str,
        }

        let canonical = serde_jcs::to_vec(&IdentityProjection {
            authority: &self.authority,
            repository: &self.repository,
            resolved_commit: &self.resolved_commit,
        })
        .context("failed to canonicalize source identity")?;
        Ok(format!(
            "sha256:{}",
            hex::encode(Sha256::digest(&canonical))
        ))
    }
}

pub(crate) fn write_source_resolution_record(
    path: &Path,
    record: &SourceResolutionRecord,
) -> Result<()> {
    write_json(path, record)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreRefRecord {
    pub schema_version: String,
    pub ecosystem: String,
    pub derivation_hash: String,
    pub blob_hash: Option<String>,
    pub cache_status: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttestationRef {
    pub subject_hash: String,
    pub path: PathBuf,
    pub kind: String,
}

pub trait DependencyMaterializer {
    fn plan(&self, req: &DependencyMaterializationRequest) -> Result<DependencyPlan>;
    fn materialize(&self, req: &DependencyMaterializationRequest) -> Result<DependencyProjection>;
    fn verify(&self, projection: &DependencyProjection) -> Result<VerificationResult>;
    fn gc_hint(&self, session_id: &str) -> Result<()>;
}

#[derive(Debug, Default)]
pub struct SessionDependencyMaterializer;

impl SessionDependencyMaterializer {
    pub fn new() -> Self {
        Self
    }
}

impl DependencyMaterializer for SessionDependencyMaterializer {
    fn plan(&self, req: &DependencyMaterializationRequest) -> Result<DependencyPlan> {
        let key = DepDerivationKeyV1::from_request(req);
        let derivation_hash = key.derivation_hash()?;
        let cache_lookup = match req.cache_strategy {
            CacheStrategy::None => CacheLookupResult::Disabled,
            CacheStrategy::DerivationCache => lookup_dep_cache(&req.ecosystem, &derivation_hash),
        };
        Ok(DependencyPlan {
            derivation_hash,
            reproducibility: ReproducibilityClass::BestEffort,
            cache_lookup,
            required_runtime_refs: req
                .runtime
                .version
                .as_ref()
                .map(|version| vec![format!("{}@{version}", req.runtime.name)])
                .unwrap_or_default(),
        })
    }

    fn materialize(&self, req: &DependencyMaterializationRequest) -> Result<DependencyProjection> {
        let plan = self.plan(req)?;
        ensure_store_scaffold()?;
        let layout = ato_run_layout(&req.session_id);
        create_run_layout(&layout)?;
        let cache_status_str = cache_status(&plan.cache_lookup);
        let cache_strategy_str = cache_strategy_label(req.cache_strategy);
        let blob_hash_for_log = match &plan.cache_lookup {
            CacheLookupResult::Hit { blob_hash } => Some(blob_hash.clone()),
            _ => None,
        };
        let session = serde_json::json!({
            "schema_version": "1",
            "session_id": req.session_id,
            "capsule_id": req.capsule_id,
            "source_root": req.source_root,
            "workspace_root": req.workspace_root,
            "derivation_hash": plan.derivation_hash,
            "dependency_cache": {
                "status": cache_status_str,
                "strategy": cache_strategy_str,
                "blob_hash": blob_hash_for_log,
            },
        });
        write_json(&layout.session_json, &session)?;
        write_store_ref(req, &plan)?;
        tracing::info!(
            capsule_id = %req.capsule_id,
            session_id = %req.session_id,
            derivation_hash = %plan.derivation_hash,
            cache_strategy = cache_strategy_str,
            cache_result = cache_status_str,
            blob_hash = blob_hash_for_log.as_deref().unwrap_or(""),
            "dependency materialization projected isolated run workspace"
        );

        let mut cache_dirs = BTreeMap::new();
        cache_dirs.insert("session".to_string(), layout.cache.clone());

        Ok(DependencyProjection {
            derivation_hash: Some(plan.derivation_hash),
            blob_hash: blob_hash_for_log,
            execution_deps_path: layout.deps,
            run_workspace: layout.root,
            env: BTreeMap::new(),
            cache_dirs,
            reproducibility_metadata: ReproducibilityMeta {
                class: Some(plan.reproducibility),
                notes: vec!["A0 isolated session materialization; whole-tree cache disabled unless explicitly enabled".to_string()],
            },
            attestation_refs: Vec::new(),
            dependency_cache_status: cache_status_str.to_string(),
        })
    }

    fn verify(&self, projection: &DependencyProjection) -> Result<VerificationResult> {
        let mut messages = Vec::new();
        if !projection
            .execution_deps_path
            .starts_with(capsule_core::common::paths::ato_runs_dir())
        {
            messages.push(format!(
                "dependency projection is outside ~/.ato/runs: {}",
                projection.execution_deps_path.display()
            ));
        }

        Ok(VerificationResult {
            ok: messages.is_empty(),
            advisory: true,
            messages,
        })
    }

    fn gc_hint(&self, _session_id: &str) -> Result<()> {
        Ok(())
    }
}

pub(crate) fn digest_file(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(Some(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(bytes))
    )))
}

fn write_store_ref(req: &DependencyMaterializationRequest, plan: &DependencyPlan) -> Result<()> {
    let path = ato_store_refs_dir()
        .join("deps")
        .join(&req.ecosystem)
        .join(hash_path_component(&plan.derivation_hash))
        .with_extension("json");
    let record = StoreRefRecord {
        schema_version: "1".to_string(),
        ecosystem: req.ecosystem.clone(),
        derivation_hash: plan.derivation_hash.clone(),
        blob_hash: None,
        cache_status: cache_status(&plan.cache_lookup).to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    write_json(&path, &record)
}

fn ensure_store_scaffold() -> Result<()> {
    for path in [
        ato_store_blobs_dir(),
        ato_store_refs_dir().join("deps"),
        ato_store_attestations_dir().join("payloads"),
        ato_store_attestations_dir().join("blobs"),
        ato_trust_roots_dir(),
        ato_trust_policies_dir(),
    ] {
        fs::create_dir_all(&path)
            .with_context(|| format!("failed to create {}", path.display()))?;
    }
    Ok(())
}

fn create_run_layout(layout: &AtoRunLayout) -> Result<()> {
    for path in [
        &layout.workspace_source,
        &layout.workspace_build,
        &layout.deps,
        &layout.cache,
        &layout.tmp,
        &layout.log,
    ] {
        fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;
    }
    Ok(())
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let bytes = serde_json::to_vec_pretty(value).context("failed to serialize json")?;
    fs::write(path, [bytes, b"\n".to_vec()].concat())
        .with_context(|| format!("failed to write {}", path.display()))
}

/// Reads the weak ref for a derivation key and decides whether to call it a hit.
///
/// Pure, read-only: never writes to the file system. A "hit" requires the ref
/// file to exist with a `blob_hash`, plus a manifest at the expected blob path
/// claiming that same hash. Anything else (missing ref, missing manifest,
/// blob_hash mismatch, IO/parse error) is reported as a miss so the caller
/// can fall back to the install path.
fn lookup_dep_cache(ecosystem: &str, derivation_hash: &str) -> CacheLookupResult {
    let ref_path = ato_store_dep_ref_path(ecosystem, derivation_hash);
    let bytes = match fs::read(&ref_path) {
        Ok(bytes) => bytes,
        Err(_) => return CacheLookupResult::Miss,
    };
    let record: StoreRefRecord = match serde_json::from_slice(&bytes) {
        Ok(record) => record,
        Err(_) => return CacheLookupResult::Miss,
    };
    let Some(blob_hash) = record.blob_hash else {
        return CacheLookupResult::Miss;
    };
    if record.derivation_hash != derivation_hash {
        return CacheLookupResult::Miss;
    }
    let address = match BlobAddress::parse(&blob_hash) {
        Ok(address) => address,
        Err(_) => return CacheLookupResult::Miss,
    };
    if !address.payload_dir().is_dir() {
        return CacheLookupResult::Miss;
    }
    match BlobManifest::read_from(&address.manifest_path()) {
        Ok(manifest) if manifest.matches_blob_hash(&blob_hash) => {
            CacheLookupResult::Hit { blob_hash }
        }
        _ => CacheLookupResult::Miss,
    }
}

fn cache_status(cache_lookup: &CacheLookupResult) -> &'static str {
    match cache_lookup {
        CacheLookupResult::Disabled => "disabled",
        CacheLookupResult::Hit { .. } => "hit",
        CacheLookupResult::Miss => "miss",
    }
}

fn cache_strategy_label(strategy: CacheStrategy) -> &'static str {
    match strategy {
        CacheStrategy::None => "none",
        CacheStrategy::DerivationCache => "derivation",
    }
}

fn hash_path_component(hash: &str) -> String {
    hash.replace(':', "-")
}

fn detect_libc() -> Option<String> {
    #[cfg(target_env = "musl")]
    {
        Some("musl".to_string())
    }
    #[cfg(all(target_os = "linux", not(target_env = "musl")))]
    {
        Some("glibc".to_string())
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

fn package_manager_compat_class(
    package_manager: Option<&str>,
    package_manager_version: Option<&str>,
) -> Option<String> {
    let package_manager = package_manager?;
    let major = package_manager_version
        .and_then(|version| version.split('.').next())
        .filter(|value| !value.is_empty());
    Some(match major {
        Some(major) => format!("{package_manager}-{major}"),
        None => package_manager.to_string(),
    })
}

fn runtime_compat_class(runtime_name: &str, runtime_version: Option<&str>) -> String {
    let Some(version) = runtime_version.filter(|value| !value.trim().is_empty()) else {
        return runtime_name.to_string();
    };
    let mut parts = version.split('.');
    let Some(major) = parts.next().filter(|value| !value.is_empty()) else {
        return runtime_name.to_string();
    };
    if runtime_name == "python" {
        if let Some(minor) = parts.next().filter(|value| !value.is_empty()) {
            return format!("{runtime_name}-{major}.{minor}");
        }
    }
    format!("{runtime_name}-{major}")
}

fn install_policy_digest(policies: &InstallPolicies) -> String {
    #[derive(Serialize)]
    struct PolicyProjection<'a> {
        lifecycle_script_policy: &'a str,
        registry_policy: &'a str,
        network_policy: &'a str,
    }

    let canonical = serde_jcs::to_vec(&PolicyProjection {
        lifecycle_script_policy: &policies.lifecycle_script_policy,
        registry_policy: &policies.registry_policy,
        network_policy: &policies.network_policy,
    })
    .expect("policy projection is JCS-serializable");
    format!("sha256:{}", hex::encode(Sha256::digest(canonical)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_request(
        requested_ref: Option<&str>,
    ) -> (DependencyMaterializationRequest, SourceResolutionRecord) {
        let request = DependencyMaterializationRequest {
            session_id: "test".to_string(),
            capsule_id: "capsule".to_string(),
            source_root: PathBuf::from("/repo"),
            workspace_root: PathBuf::from("/repo"),
            ecosystem: "node".to_string(),
            package_manager: Some("pnpm".to_string()),
            package_manager_version: Some("9.0.0".to_string()),
            runtime: RuntimeSelection {
                name: "node".to_string(),
                version: Some("20.11.0".to_string()),
            },
            manifests: ManifestInputs {
                lockfile_digest: Some("sha256:aaa".to_string()),
                package_manifest_digest: Some("sha256:bbb".to_string()),
                workspace_manifest_digest: None,
                path_dependency_digest: None,
            },
            policies: InstallPolicies {
                lifecycle_script_policy: "sandbox".to_string(),
                registry_policy: "default".to_string(),
                network_policy: "default".to_string(),
                env_allowlist_digest: None,
            },
            platform: PlatformTriple {
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
                libc: Some("glibc".to_string()),
                abi: None,
            },
            cache_strategy: CacheStrategy::None,
            attestation_strategy: AttestationStrategy::None,
        };
        let source = SourceResolutionRecord {
            authority: "github.com".to_string(),
            repository: Some("acme/app".to_string()),
            requested_ref: requested_ref.map(str::to_string),
            resolved_commit: "3f2e9c1".to_string(),
            resolved_at: "2026-05-02T00:00:00Z".to_string(),
            commit_signature_verdict: None,
        };
        (request, source)
    }

    #[test]
    fn derivation_key_hash_is_stable_under_jcs() {
        let (request, _) = sample_request(None);
        let key = DepDerivationKeyV1::from_request(&request);

        assert_eq!(
            key.derivation_hash().unwrap(),
            key.derivation_hash().unwrap()
        );
        assert!(key.derivation_hash().unwrap().starts_with("sha256:"));
    }

    #[test]
    fn requested_ref_is_not_part_of_source_identity() {
        let (_, main) = sample_request(Some("main"));
        let (_, tag) = sample_request(Some("v1.2.3"));

        assert_eq!(main.identity_hash().unwrap(), tag.identity_hash().unwrap());
    }

    #[test]
    fn request_changes_derivation_hash_when_lock_digest_changes() {
        let (mut first, _) = sample_request(None);
        let (mut second, _) = sample_request(None);
        first.manifests.lockfile_digest = Some("sha256:first".to_string());
        second.manifests.lockfile_digest = Some("sha256:second".to_string());

        let first_hash = DepDerivationKeyV1::from_request(&first)
            .derivation_hash()
            .unwrap();
        let second_hash = DepDerivationKeyV1::from_request(&second)
            .derivation_hash()
            .unwrap();

        assert_ne!(first_hash, second_hash);
    }
}
