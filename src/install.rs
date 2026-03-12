//! Install command implementation
//!
//! Downloads and installs capsules from the Store.
//! Primary path: `/v1/manifest/capsules/by/:publisher/:slug/distributions` (.capsule contract)

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use futures::stream::{FuturesUnordered, StreamExt};
use rand::RngCore;
use serde::{Deserialize, Serialize};
#[cfg(test)]
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Write;
use std::io::{self};
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

use capsule_core::packers::payload as manifest_payload;
use capsule_core::resource::cas::LocalCasIndex;
use capsule_core::types::identity::public_key_to_did;
use capsule_core::types::CapsuleManifest;

use crate::registry::RegistryResolver;
use crate::runtime_tree;

const DEFAULT_STORE_DIR: &str = ".ato/store";
const SEGMENT_MAX_LEN: usize = 63;
const LEASE_REFRESH_INTERVAL_SECS: u64 = 300;
const NEGOTIATE_DEFAULT_MAX_BYTES: u64 = 16 * 1024 * 1024;
const DELTA_RECONSTRUCT_ZSTD_LEVEL: i32 = 3;

#[derive(Debug, Serialize)]
pub struct InstallResult {
    pub capsule_id: String,
    pub scoped_id: String,
    pub publisher: String,
    pub slug: String,
    pub version: String,
    pub path: PathBuf,
    pub content_hash: String,
    pub install_kind: InstallKind,
    pub launchable: Option<LaunchableTarget>,
    pub local_derivation: Option<LocalDerivationInfo>,
    pub projection: Option<ProjectionInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub enum InstallKind {
    Standard,
    NativeRequiresLocalDerivation,
}

#[derive(Debug, Clone, Serialize)]
pub enum LaunchableTarget {
    CapsuleArchive { path: PathBuf },
    DerivedApp { path: PathBuf },
}

#[derive(Debug, Clone, Serialize)]
pub struct LocalDerivationInfo {
    pub schema_version: String,
    pub performed: bool,
    pub fetched_dir: PathBuf,
    pub derived_app_path: Option<PathBuf>,
    pub provenance_path: Option<PathBuf>,
    pub parent_digest: Option<String>,
    pub derived_digest: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectionInfo {
    pub performed: bool,
    pub projection_id: Option<String>,
    pub projected_path: Option<PathBuf>,
    pub state: Option<String>,
    pub schema_version: Option<String>,
    pub metadata_path: Option<PathBuf>,
}

pub struct GitHubCheckout {
    pub repository: String,
    pub publisher: String,
    pub checkout_dir: PathBuf,
    _temp_dir: tempfile::TempDir,
}

pub struct InstallExecutionOptions {
    pub output_dir: Option<PathBuf>,
    pub yes: bool,
    pub projection_preference: ProjectionPreference,
    pub json_output: bool,
    pub can_prompt_interactively: bool,
}

enum InstallSource {
    Registry(String),
    Local(String),
}

impl InstallSource {
    fn registry_url(&self) -> Option<&str> {
        match self {
            Self::Registry(url) => Some(url),
            Self::Local(_) => None,
        }
    }

    fn cache_label(&self) -> &str {
        match self {
            Self::Registry(url) | Self::Local(url) => url,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionPreference {
    Prompt,
    Force,
    Skip,
}

#[derive(Debug, Clone, Serialize)]
pub struct CapsuleDetailSummary {
    pub scoped_id: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub latest_version: Option<String>,
    pub permissions: Option<CapsulePermissions>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CapsulePermissions {
    #[serde(default)]
    pub network: Option<CapsuleNetworkPermissions>,
    #[serde(default)]
    pub isolation: Option<CapsuleIsolationPermissions>,
    #[serde(default)]
    pub filesystem: Option<CapsuleFilesystemPermissions>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CapsuleNetworkPermissions {
    #[serde(default)]
    pub egress_allow: Vec<String>,
    #[serde(default)]
    pub connect_allowlist: Vec<String>,
}

impl CapsuleNetworkPermissions {
    pub fn merged_endpoints(&self) -> Vec<String> {
        let mut merged = self.egress_allow.clone();
        for endpoint in &self.connect_allowlist {
            if !merged.iter().any(|existing| existing == endpoint) {
                merged.push(endpoint.clone());
            }
        }
        merged
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CapsuleIsolationPermissions {
    #[serde(default)]
    pub allow_env: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CapsuleFilesystemPermissions {
    #[serde(default, alias = "read")]
    pub read_only: Vec<String>,
    #[serde(default, alias = "write")]
    pub read_write: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct CapsuleDetail {
    id: String,
    #[serde(default, alias = "scopedId", alias = "scoped_id")]
    scoped_id: Option<String>,
    slug: String,
    name: String,
    description: String,
    price: u64,
    currency: String,
    #[serde(rename = "latestVersion", alias = "latest_version", default)]
    latest_version: Option<String>,
    releases: Vec<ReleaseInfo>,
    #[serde(default)]
    manifest_toml: Option<String>,
    #[serde(default)]
    capsule_lock: Option<String>,
    #[serde(default)]
    permissions: Option<CapsulePermissions>,
}

#[derive(Debug, Deserialize)]
struct ReleaseInfo {
    version: String,
}

#[derive(Debug, Deserialize)]
struct ManifestEpochResolveResponse {
    pointer: ManifestEpochPointer,
    public_key: String,
}

#[derive(Debug, Deserialize)]
struct ManifestEpochPointer {
    scoped_id: String,
    epoch: u64,
    manifest_hash: String,
    #[serde(default)]
    prev_epoch_hash: Option<String>,
    issued_at: String,
    signer_did: String,
    key_id: String,
    signature: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct ManifestChunkBloomRequest {
    m_bits: u64,
    k_hashes: u32,
    seed: u64,
    bitset_base64: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct ManifestNegotiateRequest {
    scoped_id: String,
    target_manifest_hash: String,
    #[serde(default)]
    have_chunks: Vec<String>,
    #[serde(default)]
    have_chunks_bloom: Option<ManifestChunkBloomRequest>,
    #[serde(default)]
    reuse_lease_id: Option<String>,
    #[serde(default)]
    max_bytes: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ManifestNegotiateResponse {
    required_chunks: Vec<String>,
    #[serde(default)]
    yanked: Option<bool>,
    #[serde(default)]
    lease_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct ManifestLeaseRefreshRequest {
    lease_id: String,
    ttl_secs: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ManifestLeaseRefreshResponse {
    lease_id: String,
}

#[derive(Debug, Serialize)]
struct ManifestLeaseReleaseRequest {
    lease_id: String,
}

#[derive(Debug)]
enum DeltaInstallResult {
    Artifact(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum V3SyncOutcome {
    Synced,
    SkippedUnsupportedRegistry,
    SkippedDisabledCas(capsule_core::capsule_v3::CasDisableReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChunkDownloadOutcome {
    Stored,
    UnsupportedRegistry,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct EpochGuardState {
    #[serde(default)]
    capsules: HashMap<String, EpochGuardEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
struct EpochGuardEntry {
    max_epoch: u64,
    manifest_hash: String,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScopedCapsuleRef {
    pub publisher: String,
    pub slug: String,
    pub scoped_id: String,
}

#[derive(Debug, Clone)]
pub struct ParsedCapsuleRequest {
    pub scoped_ref: ScopedCapsuleRef,
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScopedSuggestion {
    pub scoped_id: String,
    pub downloads: u64,
}

#[derive(Debug, Deserialize)]
struct SuggestionCapsulesResponse {
    capsules: Vec<SuggestionCapsuleRow>,
}

#[derive(Debug, Deserialize)]
struct SuggestionCapsuleRow {
    slug: String,
    #[serde(default, alias = "scopedId", alias = "scoped_id")]
    scoped_id: Option<String>,
    #[serde(default)]
    downloads: Option<u64>,
    #[serde(default)]
    publisher: Option<SuggestionPublisher>,
}

#[derive(Debug, Deserialize)]
struct SuggestionPublisher {
    handle: String,
}

#[derive(Debug, Deserialize)]
struct VersionManifestResolveResponse {
    scoped_id: String,
    version: String,
    manifest_hash: String,
    #[serde(default)]
    yanked_at: Option<String>,
}

#[derive(Debug)]
enum ManifestResolution {
    Current(ManifestEpochResolveResponse),
    Version(VersionManifestResolveResponse),
}

impl ManifestResolution {
    fn manifest_hash(&self) -> &str {
        match self {
            Self::Current(response) => &response.pointer.manifest_hash,
            Self::Version(response) => &response.manifest_hash,
        }
    }
}

fn is_valid_segment(value: &str) -> bool {
    if value.is_empty() || value.len() > SEGMENT_MAX_LEN {
        return false;
    }
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    let mut prev_hyphen = false;
    for ch in chars {
        let is_valid = ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-';
        if !is_valid {
            return false;
        }
        if ch == '-' && prev_hyphen {
            return false;
        }
        prev_hyphen = ch == '-';
    }
    !value.ends_with('-')
}

fn split_capsule_request(input: &str) -> Result<(String, Option<String>)> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        bail!("scoped_id_required: use publisher/slug (for example: koh0920/sample-capsule)");
    }

    let normalized = trimmed.strip_prefix('@').unwrap_or(trimmed).trim();
    if normalized.is_empty() {
        bail!("scoped_id_required: use publisher/slug (for example: koh0920/sample-capsule)");
    }

    if let Some((base, version)) = normalized.rsplit_once('@') {
        let base = base.trim();
        let version = version.trim();
        if base.is_empty() {
            bail!("scoped_id_required: use publisher/slug (for example: koh0920/sample-capsule)");
        }
        if version.is_empty() {
            bail!("version_required: use publisher/slug@version");
        }
        return Ok((base.to_string(), Some(version.to_string())));
    }

    Ok((normalized.to_string(), None))
}

pub fn parse_capsule_request(input: &str) -> Result<ParsedCapsuleRequest> {
    let (scoped_input, version) = split_capsule_request(input)?;
    let normalized = scoped_input.strip_prefix('@').unwrap_or(&scoped_input);
    let mut parts = normalized.split('/');
    let publisher = parts.next().unwrap_or_default().trim().to_lowercase();
    let slug = parts.next().unwrap_or_default().trim().to_lowercase();
    if parts.next().is_some() {
        bail!("invalid_capsule_ref: use publisher/slug (optionally @publisher/slug)");
    }
    if publisher.is_empty() || slug.is_empty() {
        bail!("scoped_id_required: use publisher/slug (for example: koh0920/sample-capsule)");
    }
    if !is_valid_segment(&publisher) || !is_valid_segment(&slug) {
        bail!("invalid_capsule_ref: publisher/slug must be lowercase kebab-case");
    }
    Ok(ParsedCapsuleRequest {
        scoped_ref: ScopedCapsuleRef {
            publisher: publisher.clone(),
            slug: slug.clone(),
            scoped_id: format!("{}/{}", publisher, slug),
        },
        version,
    })
}

pub fn parse_capsule_ref(input: &str) -> Result<ScopedCapsuleRef> {
    Ok(parse_capsule_request(input)?.scoped_ref)
}

pub fn is_slug_only_ref(input: &str) -> bool {
    let Ok((scoped_input, _)) = split_capsule_request(input) else {
        return false;
    };
    !scoped_input.contains('/')
}

pub fn normalize_github_repository(repository: &str) -> Result<String> {
    crate::publish_preflight::normalize_repository_value(repository)
}

pub async fn download_github_repository(repository: &str) -> Result<GitHubCheckout> {
    let normalized = normalize_github_repository(repository)?;
    let (owner, repo) = normalized
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("repository must include owner/repo"))?;
    let publisher = normalize_install_segment(owner)?;
    let client = reqwest::Client::new();
    let archive_url = format!("{}/repos/{owner}/{repo}/tarball", github_api_base_url());
    let response = client
        .get(&archive_url)
        .header(reqwest::header::USER_AGENT, "ato-cli")
        .send()
        .await
        .with_context(|| format!("Failed to fetch GitHub repository archive: {normalized}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!(
            "Failed to fetch GitHub repository archive (status={}): {}",
            status,
            body
        );
    }
    let archive_bytes = response
        .bytes()
        .await
        .with_context(|| format!("Failed to read GitHub repository archive: {normalized}"))?;
    let temp_root = github_checkout_root()?;
    let temp_dir = tempfile::Builder::new()
        .prefix("gh-install-")
        .tempdir_in(temp_root)
        .with_context(|| "Failed to create GitHub checkout directory")?;
    let checkout_dir = normalize_github_checkout_dir(
        unpack_github_tarball(&archive_bytes, temp_dir.path())?,
        repo,
    )?;
    Ok(GitHubCheckout {
        repository: normalized,
        publisher,
        checkout_dir,
        _temp_dir: temp_dir,
    })
}

pub async fn install_built_github_artifact(
    artifact_path: &Path,
    publisher: &str,
    repository: &str,
    options: InstallExecutionOptions,
) -> Result<InstallResult> {
    let artifact_bytes = std::fs::read(artifact_path)
        .with_context(|| format!("Failed to read built artifact: {}", artifact_path.display()))?;
    let manifest_toml = extract_manifest_toml_from_capsule(&artifact_bytes)
        .with_context(|| "Built artifact is missing capsule.toml")?;
    let manifest: CapsuleManifest = toml::from_str(&manifest_toml)
        .with_context(|| "Built artifact has invalid capsule.toml")?;
    let slug = normalize_install_segment(&manifest.name)?;
    let version = manifest.version.trim();
    if version.is_empty() {
        bail!("Built artifact capsule.toml is missing version");
    }
    let scoped_ref = parse_capsule_ref(&format!("{publisher}/{slug}"))?;
    let display_slug = scoped_ref.slug.clone();
    let normalized_file_name = format!("{}-{}.capsule", scoped_ref.slug, version);
    complete_install_from_bytes(
        format!("github:{repository}"),
        scoped_ref,
        display_slug,
        version.to_string(),
        artifact_bytes,
        normalized_file_name,
        options,
        InstallSource::Local(format!("github:{repository}")),
    )
    .await
}

pub fn merge_requested_version(
    embedded_version: Option<&str>,
    explicit_version: Option<&str>,
) -> Result<Option<String>> {
    match (
        embedded_version
            .map(str::trim)
            .filter(|value| !value.is_empty()),
        explicit_version
            .map(str::trim)
            .filter(|value| !value.is_empty()),
    ) {
        (Some(left), Some(right)) if left != right => {
            bail!(
                "conflicting_version_request: ref specifies version '{}' but --version requested '{}'",
                left,
                right
            );
        }
        (Some(left), _) => Ok(Some(left.to_string())),
        (None, Some(right)) => Ok(Some(right.to_string())),
        (None, None) => Ok(None),
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn install_app(
    capsule_ref: &str,
    registry_url: Option<&str>,
    version: Option<&str>,
    output_dir: Option<PathBuf>,
    _set_default: bool,
    yes: bool,
    projection_preference: ProjectionPreference,
    allow_unverified: bool,
    allow_downgrade: bool,
    json_output: bool,
    can_prompt_interactively: bool,
) -> Result<InstallResult> {
    let request = parse_capsule_request(capsule_ref)?;
    let scoped_ref = request.scoped_ref;
    let requested_version = merge_requested_version(request.version.as_deref(), version)?;
    let registry = resolve_registry_url(registry_url, !json_output).await?;

    let client = reqwest::Client::new();
    let capsule_url = format!(
        "{}/v1/manifest/capsules/by/{}/{}",
        registry,
        urlencoding::encode(&scoped_ref.publisher),
        urlencoding::encode(&scoped_ref.slug)
    );
    let capsule: CapsuleDetail = client
        .get(&capsule_url)
        .send()
        .await
        .with_context(|| format!("Failed to connect to registry: {}", registry))?
        .json()
        .await
        .with_context(|| format!("Capsule not found: {}", scoped_ref.scoped_id))?;

    if capsule.price > 0 {
        bail!(
            "This capsule costs {} {}. Beta only supports free apps.",
            capsule.price,
            capsule.currency
        );
    }

    if !json_output {
        let latest_display = capsule
            .latest_version
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("unknown");
        eprintln!("📦 Found: {} v{}", capsule.name, latest_display);
        if !capsule.description.is_empty() {
            eprintln!("   {}", capsule.description);
        }
    }

    let target_version_owned = match requested_version.as_deref() {
        Some(explicit) => explicit.to_string(),
        None => capsule
            .latest_version
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No installable version available for '{}'. This capsule has no published release version.",
                    scoped_ref.scoped_id
                )
            })?,
    };
    let target_version = target_version_owned.as_str();
    capsule
        .releases
        .iter()
        .find(|r| r.version == target_version)
        .with_context(|| format!("Version {} not found", target_version))?;
    let (bytes, normalized_file_name) = match install_manifest_delta_path(
        &client,
        &registry,
        &scoped_ref,
        requested_version.as_deref(),
        capsule.manifest_toml.as_deref(),
        capsule.capsule_lock.as_deref(),
    )
    .await?
    {
        DeltaInstallResult::Artifact(bytes) => {
            verify_manifest_supply_chain(
                &client,
                &registry,
                &scoped_ref,
                requested_version.as_deref(),
                &bytes,
                allow_unverified,
                allow_downgrade,
            )
            .await?;
            (
                bytes,
                format!("{}-{}.capsule", scoped_ref.slug, target_version),
            )
        }
    };

    complete_install_from_bytes(
        capsule.id,
        scoped_ref,
        capsule.slug,
        target_version_owned,
        bytes,
        normalized_file_name,
        InstallExecutionOptions {
            output_dir,
            yes,
            projection_preference,
            json_output,
            can_prompt_interactively,
        },
        InstallSource::Registry(registry),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn complete_install_from_bytes(
    capsule_id: String,
    scoped_ref: ScopedCapsuleRef,
    display_slug: String,
    version: String,
    bytes: Vec<u8>,
    normalized_file_name: String,
    options: InstallExecutionOptions,
    source: InstallSource,
) -> Result<InstallResult> {
    let InstallExecutionOptions {
        output_dir,
        yes,
        projection_preference,
        json_output,
        can_prompt_interactively,
    } = options;
    let computed_blake3 = compute_blake3(&bytes);
    if let Some(v3_manifest) = extract_payload_v3_manifest_from_capsule(&bytes)? {
        if let Some(registry_url) = source.registry_url() {
            match sync_v3_chunks_from_manifest(&reqwest::Client::new(), registry_url, &v3_manifest)
                .await?
            {
                V3SyncOutcome::Synced => {}
                V3SyncOutcome::SkippedUnsupportedRegistry => {
                    if !json_output {
                        eprintln!(
                            "ℹ️  Registry does not expose v3 chunk sync endpoint; falling back to embedded payload"
                        );
                    }
                }
                V3SyncOutcome::SkippedDisabledCas(reason) => {
                    emit_cas_disabled_performance_warning_once(&reason, json_output);
                }
            }
        }
    }

    let target_version = version.as_str();
    let native_spec = crate::native_delivery::detect_install_requires_local_derivation(&bytes)?;
    if let Some(_native_spec) = native_spec {
        if !crate::native_delivery::host_supports_finalize() {
            bail!(
                "This app requires local finalize, but this host does not support native finalize (macOS hosts only)."
            );
        }

        let finalize_allowed = if yes {
            true
        } else if can_prompt_interactively && !json_output {
            prompt_for_confirmation(
                "This app requires local setup to run on this machine.\nRun local finalize now? [Y/n] ",
                true,
            )?
        } else {
            false
        };

        if !finalize_allowed {
            bail!(
                "This app requires local finalize, but no interactive consent is available. Re-run with --yes."
            );
        }

        let fetch_result = crate::native_delivery::materialize_fetch_cache_from_artifact(
            &scoped_ref.scoped_id,
            target_version,
            source.cache_label(),
            &bytes,
        )?;

        if !json_output {
            eprintln!("Running local finalize...");
        }
        let finalize_result =
            crate::native_delivery::finalize_fetched_artifact(&fetch_result.cache_dir)?;

        let output_path = persist_installed_artifact(
            output_dir.clone(),
            &scoped_ref.publisher,
            &scoped_ref.slug,
            target_version,
            &normalized_file_name,
            &bytes,
            &computed_blake3,
        )?;

        let projection = match projection_preference {
            ProjectionPreference::Skip => {
                if !json_output {
                    eprintln!("Launcher projection skipped.");
                }
                ProjectionInfo {
                    performed: false,
                    projection_id: None,
                    projected_path: None,
                    state: Some("skipped".to_string()),
                    schema_version: Some(
                        crate::native_delivery::delivery_schema_version().to_string(),
                    ),
                    metadata_path: None,
                }
            }
            ProjectionPreference::Force => {
                match crate::native_delivery::execute_project(
                    &finalize_result.derived_app_path,
                    None,
                ) {
                    Ok(result) => ProjectionInfo {
                        performed: true,
                        projection_id: Some(result.projection_id),
                        projected_path: Some(result.projected_path),
                        state: Some(result.state),
                        schema_version: Some(
                            crate::native_delivery::delivery_schema_version().to_string(),
                        ),
                        metadata_path: Some(result.metadata_path),
                    },
                    Err(err) => {
                        if !json_output {
                            eprintln!("Launcher projection failed: {err}");
                            eprintln!(
                                "Run `ato project {}` to try again later.",
                                finalize_result.derived_app_path.display()
                            );
                        }
                        ProjectionInfo {
                            performed: false,
                            projection_id: None,
                            projected_path: None,
                            state: Some("failed".to_string()),
                            schema_version: Some(
                                crate::native_delivery::delivery_schema_version().to_string(),
                            ),
                            metadata_path: None,
                        }
                    }
                }
            }
            ProjectionPreference::Prompt => {
                let should_project = if yes {
                    true
                } else if can_prompt_interactively && !json_output {
                    prompt_for_confirmation(
                        "This app can also be added to your Applications launcher.\nCreate a launcher projection? [y/N] ",
                        false,
                    )?
                } else {
                    false
                };
                if should_project {
                    match crate::native_delivery::execute_project(
                        &finalize_result.derived_app_path,
                        None,
                    ) {
                        Ok(result) => ProjectionInfo {
                            performed: true,
                            projection_id: Some(result.projection_id),
                            projected_path: Some(result.projected_path),
                            state: Some(result.state),
                            schema_version: Some(
                                crate::native_delivery::delivery_schema_version().to_string(),
                            ),
                            metadata_path: Some(result.metadata_path),
                        },
                        Err(err) => {
                            if !json_output {
                                eprintln!("Launcher projection failed: {err}");
                                eprintln!(
                                    "Run `ato project {}` to try again later.",
                                    finalize_result.derived_app_path.display()
                                );
                            }
                            ProjectionInfo {
                                performed: false,
                                projection_id: None,
                                projected_path: None,
                                state: Some("failed".to_string()),
                                schema_version: Some(
                                    crate::native_delivery::delivery_schema_version().to_string(),
                                ),
                                metadata_path: None,
                            }
                        }
                    }
                } else {
                    if !json_output {
                        eprintln!("Launcher projection skipped.");
                    }
                    ProjectionInfo {
                        performed: false,
                        projection_id: None,
                        projected_path: None,
                        state: Some("skipped".to_string()),
                        schema_version: Some(
                            crate::native_delivery::delivery_schema_version().to_string(),
                        ),
                        metadata_path: None,
                    }
                }
            }
        };

        return Ok(InstallResult {
            capsule_id,
            scoped_id: scoped_ref.scoped_id.clone(),
            publisher: scoped_ref.publisher,
            slug: display_slug,
            version,
            path: output_path,
            content_hash: computed_blake3,
            install_kind: InstallKind::NativeRequiresLocalDerivation,
            launchable: Some(LaunchableTarget::DerivedApp {
                path: finalize_result.derived_app_path.clone(),
            }),
            local_derivation: Some(LocalDerivationInfo {
                schema_version: crate::native_delivery::delivery_schema_version().to_string(),
                performed: true,
                fetched_dir: fetch_result.cache_dir,
                derived_app_path: Some(finalize_result.derived_app_path),
                provenance_path: Some(finalize_result.provenance_path),
                parent_digest: Some(finalize_result.parent_digest),
                derived_digest: Some(finalize_result.derived_digest),
            }),
            projection: Some(projection),
        });
    }

    let output_path = persist_installed_artifact(
        output_dir,
        &scoped_ref.publisher,
        &scoped_ref.slug,
        target_version,
        &normalized_file_name,
        &bytes,
        &computed_blake3,
    )?;

    if !json_output {
        eprintln!("✅ Installed to: {}", output_path.display());
        eprintln!("   To run: ato run {}", output_path.display());
    }

    Ok(InstallResult {
        capsule_id,
        scoped_id: scoped_ref.scoped_id.clone(),
        publisher: scoped_ref.publisher,
        slug: display_slug,
        version,
        path: output_path.clone(),
        content_hash: computed_blake3,
        install_kind: InstallKind::Standard,
        launchable: Some(LaunchableTarget::CapsuleArchive {
            path: output_path.clone(),
        }),
        local_derivation: None,
        projection: None,
    })
}

fn persist_installed_artifact(
    output_dir: Option<PathBuf>,
    publisher: &str,
    slug: &str,
    version: &str,
    normalized_file_name: &str,
    bytes: &[u8],
    content_hash: &str,
) -> Result<PathBuf> {
    let store_root = output_dir.unwrap_or_else(|| {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(DEFAULT_STORE_DIR)
    });
    let install_dir = store_root.join(publisher).join(slug).join(version);
    std::fs::create_dir_all(&install_dir).with_context(|| {
        format!(
            "Failed to create store directory: {}",
            install_dir.display()
        )
    })?;

    let output_path = install_dir.join(normalized_file_name);
    sweep_stale_tmp_capsules(&install_dir)?;
    write_capsule_atomic(&output_path, bytes, content_hash)?;
    runtime_tree::prepare_runtime_tree(publisher, slug, version, bytes)?;
    Ok(output_path)
}

fn prompt_for_confirmation(prompt: &str, default_yes: bool) -> Result<bool> {
    eprint!("{prompt}");
    io::stderr().flush().context("Failed to flush prompt")?;
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("Failed to read interactive input")?;
    let trimmed = input.trim().to_ascii_lowercase();
    if trimmed.is_empty() {
        return Ok(default_yes);
    }
    Ok(matches!(trimmed.as_str(), "y" | "yes"))
}

fn normalize_install_segment(value: &str) -> Result<String> {
    let mut normalized = String::new();
    let mut prev_hyphen = false;
    for ch in value.trim().chars() {
        let ch = ch.to_ascii_lowercase();
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            normalized.push(ch);
            prev_hyphen = false;
        } else if !prev_hyphen && !normalized.is_empty() {
            normalized.push('-');
            prev_hyphen = true;
        }
    }
    while normalized.ends_with('-') {
        normalized.pop();
    }
    if !is_valid_segment(&normalized) {
        bail!(
            "invalid install identifier segment '{}': must contain lowercase letters or digits and may include single hyphens between them",
            value
        );
    }
    Ok(normalized)
}

fn github_checkout_root() -> Result<PathBuf> {
    let root = std::env::current_dir()
        .with_context(|| "Failed to resolve current directory for temporary checkout")?
        .join(".tmp")
        .join("ato")
        .join("gh-install");
    std::fs::create_dir_all(&root).with_context(|| {
        format!(
            "Failed to create temporary checkout root: {}",
            root.display()
        )
    })?;
    Ok(root)
}

/// Returns the GitHub API base URL for repository archive downloads.
///
/// `ATO_GITHUB_API_BASE_URL` is intended for local/mock CLI tests so the
/// `--from-gh-repo` flow can be exercised without real GitHub network access.
fn github_api_base_url() -> String {
    std::env::var("ATO_GITHUB_API_BASE_URL")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "https://api.github.com".to_string())
}

fn unpack_github_tarball(bytes: &[u8], destination: &Path) -> Result<PathBuf> {
    let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    let mut root_dir: Option<PathBuf> = None;
    for entry in archive
        .entries()
        .context("Failed to read GitHub repository archive")?
    {
        let mut entry = entry.context("Invalid GitHub repository archive entry")?;
        if !matches!(
            entry.header().entry_type(),
            tar::EntryType::Regular
                | tar::EntryType::Directory
                | tar::EntryType::Symlink
                | tar::EntryType::Link
        ) {
            continue;
        }
        let path = entry
            .path()
            .context("Failed to read GitHub archive entry path")?;
        let mut components = path.components();
        let first = components
            .next()
            .ok_or_else(|| anyhow::anyhow!("GitHub archive entry path is empty or invalid"))?;
        let Component::Normal(root_component) = first else {
            bail!(
                "GitHub archive entry must start with a top-level directory before repository files; found non-standard leading path component"
            );
        };
        // The first component is the expected top-level repository directory. Remaining
        // components must stay within that directory and must not traverse outward.
        if components.any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            bail!(
                "GitHub archive entry contains unsafe path traversal components (`..`, absolute paths, or prefixes)"
            );
        }
        let root_path = PathBuf::from(root_component);
        match &root_dir {
            Some(existing) if existing != &root_path => {
                bail!("GitHub archive contains multiple top-level directories")
            }
            None => root_dir = Some(root_path),
            _ => {}
        }
        entry
            .unpack_in(destination)
            .context("Failed to unpack GitHub repository archive")?;
    }
    let root_dir = root_dir.ok_or_else(|| anyhow::anyhow!("GitHub archive is empty"))?;
    Ok(destination.join(root_dir))
}

fn normalize_github_checkout_dir(extracted_root: PathBuf, repo: &str) -> Result<PathBuf> {
    let parent = extracted_root
        .parent()
        .ok_or_else(|| anyhow::anyhow!("GitHub checkout root is missing a parent directory"))?;
    let normalized = parent.join(repo.trim());
    if normalized == extracted_root {
        return Ok(extracted_root);
    }
    if normalized.exists() {
        bail!(
            "GitHub checkout directory already exists: {}",
            normalized.display()
        );
    }
    std::fs::rename(&extracted_root, &normalized).with_context(|| {
        format!(
            "Failed to normalize GitHub checkout directory {} -> {}",
            extracted_root.display(),
            normalized.display()
        )
    })?;
    Ok(normalized)
}

fn extract_payload_v3_manifest_from_capsule(
    bytes: &[u8],
) -> Result<Option<capsule_core::capsule_v3::CapsuleManifestV3>> {
    let mut archive = tar::Archive::new(std::io::Cursor::new(bytes));
    let entries = archive
        .entries()
        .context("Failed to read .capsule archive entries")?;

    for entry in entries {
        let mut entry = entry.context("Invalid .capsule entry")?;
        let entry_path = entry
            .path()
            .context("Failed to read archive entry path")?
            .to_string_lossy()
            .to_string();
        if entry_path != capsule_core::capsule_v3::V3_PAYLOAD_MANIFEST_PATH {
            continue;
        }

        let mut manifest_bytes = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut manifest_bytes)
            .context("Failed to read payload.v3.manifest.json from artifact")?;
        let manifest: capsule_core::capsule_v3::CapsuleManifestV3 =
            serde_json::from_slice(&manifest_bytes)
                .context("Failed to parse payload.v3.manifest.json from artifact")?;
        capsule_core::capsule_v3::verify_artifact_hash(&manifest)
            .context("Invalid payload.v3.manifest.json artifact_hash")?;
        return Ok(Some(manifest));
    }

    Ok(None)
}

async fn sync_v3_chunks_from_manifest(
    client: &reqwest::Client,
    registry: &str,
    manifest: &capsule_core::capsule_v3::CapsuleManifestV3,
) -> Result<V3SyncOutcome> {
    let cas = match capsule_core::capsule_v3::CasProvider::from_env() {
        capsule_core::capsule_v3::CasProvider::Enabled(store) => store,
        capsule_core::capsule_v3::CasProvider::Disabled(reason) => {
            capsule_core::capsule_v3::CasProvider::log_disabled_once(
                "install_v3_chunk_sync",
                &reason,
            );
            return Ok(V3SyncOutcome::SkippedDisabledCas(reason));
        }
    };
    let token = current_ato_token();
    let concurrency = sync_concurrency_limit();
    sync_v3_chunks_from_manifest_with_options(client, registry, manifest, cas, token, concurrency)
        .await
}

fn emit_cas_disabled_performance_warning_once(
    reason: &capsule_core::capsule_v3::CasDisableReason,
    json_output: bool,
) {
    if json_output {
        return;
    }
    static STDERR_WARN_ONCE: Once = Once::new();
    STDERR_WARN_ONCE.call_once(|| {
        eprintln!(
            "⚠️  Performance warning: CAS is disabled (reason: {}). Falling back to v2 legacy mode.",
            reason
        );
    });
}

async fn sync_v3_chunks_from_manifest_with_options(
    client: &reqwest::Client,
    registry: &str,
    manifest: &capsule_core::capsule_v3::CapsuleManifestV3,
    cas: capsule_core::capsule_v3::CasStore,
    token: Option<String>,
    concurrency: usize,
) -> Result<V3SyncOutcome> {
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let mut downloads = FuturesUnordered::new();

    for chunk in &manifest.chunks {
        if cas
            .has_chunk(&chunk.raw_hash)
            .with_context(|| format!("Failed to check local CAS chunk {}", chunk.raw_hash))?
        {
            continue;
        }
        let client = client.clone();
        let cas = cas.clone();
        let registry = registry.to_string();
        let token = token.clone();
        let raw_hash = chunk.raw_hash.clone();
        let raw_size = chunk.raw_size;
        let semaphore = Arc::clone(&semaphore);

        downloads.push(async move {
            let _permit = semaphore
                .acquire_owned()
                .await
                .map_err(|_| anyhow::anyhow!("v3 pull semaphore was closed"))?;
            download_chunk_to_cas_with_retry(
                &client,
                &registry,
                &cas,
                &raw_hash,
                raw_size,
                token.as_deref(),
            )
            .await
        });
    }

    while let Some(result) = downloads.next().await {
        match result? {
            ChunkDownloadOutcome::Stored => {}
            ChunkDownloadOutcome::UnsupportedRegistry => {
                return Ok(V3SyncOutcome::SkippedUnsupportedRegistry);
            }
        }
    }

    Ok(V3SyncOutcome::Synced)
}

async fn download_chunk_to_cas_with_retry(
    client: &reqwest::Client,
    registry: &str,
    cas: &capsule_core::capsule_v3::CasStore,
    raw_hash: &str,
    raw_size: u32,
    token: Option<&str>,
) -> Result<ChunkDownloadOutcome> {
    let endpoint = format!("{}/v1/chunks/{}", registry, urlencoding::encode(raw_hash));
    const MAX_RETRIES: usize = 4;

    for attempt in 0..=MAX_RETRIES {
        let mut req = client.get(&endpoint);
        if let Some(token) = token {
            req = req.bearer_auth(token);
        }

        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                let bytes = resp.bytes().await.with_context(|| {
                    format!("Failed to read downloaded chunk body {}", raw_hash)
                })?;
                verify_downloaded_chunk(raw_hash, raw_size, bytes.as_ref())?;
                cas.put_chunk_zstd(raw_hash, bytes.as_ref())
                    .with_context(|| format!("Failed to store downloaded chunk {}", raw_hash))?;
                return Ok(ChunkDownloadOutcome::Stored);
            }
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                if is_sync_not_supported_status(status) {
                    return Ok(ChunkDownloadOutcome::UnsupportedRegistry);
                }
                if is_transient_status(status) && attempt < MAX_RETRIES {
                    tokio::time::sleep(backoff_duration(attempt)).await;
                    continue;
                }
                bail!(
                    "v3 chunk download failed for {} ({}): {}",
                    raw_hash,
                    status.as_u16(),
                    body.trim()
                );
            }
            Err(err) => {
                if is_transient_reqwest_error(&err) && attempt < MAX_RETRIES {
                    tokio::time::sleep(backoff_duration(attempt)).await;
                    continue;
                }
                return Err(err).with_context(|| {
                    format!(
                        "v3 chunk download request failed for {} via {}",
                        raw_hash, endpoint
                    )
                });
            }
        }
    }

    bail!("v3 chunk download exhausted retries for {}", raw_hash)
}

fn verify_downloaded_chunk(raw_hash: &str, raw_size: u32, zstd_bytes: &[u8]) -> Result<()> {
    let cursor = std::io::Cursor::new(zstd_bytes);
    let mut decoder = zstd::Decoder::new(cursor)
        .with_context(|| format!("Failed to decode downloaded chunk {}", raw_hash))?;
    let mut hasher = blake3::Hasher::new();
    let mut total: u64 = 0;
    let mut buf = [0u8; 16 * 1024];
    loop {
        let n = std::io::Read::read(&mut decoder, &mut buf)
            .with_context(|| format!("Failed to read decoded bytes for {}", raw_hash))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total = total.saturating_add(n as u64);
    }

    if total != raw_size as u64 {
        bail!(
            "downloaded chunk raw_size mismatch for {}: expected {} got {}",
            raw_hash,
            raw_size,
            total
        );
    }

    let got = format!("blake3:{}", hex::encode(hasher.finalize().as_bytes()));
    if !equals_hash(raw_hash, &got) {
        bail!(
            "downloaded chunk hash mismatch for {}: expected {} got {}",
            raw_hash,
            raw_hash,
            got
        );
    }

    Ok(())
}

fn sync_concurrency_limit() -> usize {
    std::env::var("ATO_SYNC_CONCURRENCY")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .map(|v| v.clamp(1, 128))
        .unwrap_or(8)
}

fn is_sync_not_supported_status(status: reqwest::StatusCode) -> bool {
    matches!(
        status,
        reqwest::StatusCode::NOT_FOUND
            | reqwest::StatusCode::METHOD_NOT_ALLOWED
            | reqwest::StatusCode::NOT_IMPLEMENTED
    )
}

fn is_transient_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

fn is_transient_reqwest_error(err: &reqwest::Error) -> bool {
    err.is_timeout() || err.is_connect() || err.is_request()
}

fn backoff_duration(attempt: usize) -> Duration {
    let base_ms = 200u64.saturating_mul(1u64 << attempt.min(4));
    Duration::from_millis(base_ms.min(2_000))
}

pub async fn fetch_capsule_detail(
    capsule_ref: &str,
    registry_url: Option<&str>,
) -> Result<CapsuleDetailSummary> {
    let scoped_ref = parse_capsule_ref(capsule_ref)?;
    let registry = resolve_registry_url(registry_url, false).await?;
    let client = reqwest::Client::new();
    let capsule_url = format!(
        "{}/v1/manifest/capsules/by/{}/{}",
        registry,
        urlencoding::encode(&scoped_ref.publisher),
        urlencoding::encode(&scoped_ref.slug)
    );
    let capsule: CapsuleDetail = client
        .get(&capsule_url)
        .send()
        .await
        .with_context(|| format!("Failed to connect to registry: {}", registry))?
        .json()
        .await
        .with_context(|| format!("Capsule not found: {}", scoped_ref.scoped_id))?;

    Ok(CapsuleDetailSummary {
        scoped_id: capsule
            .scoped_id
            .unwrap_or_else(|| scoped_ref.scoped_id.clone()),
        slug: capsule.slug,
        name: capsule.name,
        description: capsule.description,
        latest_version: capsule
            .latest_version
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        permissions: capsule.permissions,
    })
}

pub async fn fetch_capsule_manifest_toml(
    capsule_ref: &str,
    registry_url: Option<&str>,
) -> Result<String> {
    let scoped_ref = parse_capsule_ref(capsule_ref)?;
    let registry = resolve_registry_url(registry_url, false).await?;
    let client = reqwest::Client::new();
    let capsule_url = format!(
        "{}/v1/manifest/capsules/by/{}/{}",
        registry,
        urlencoding::encode(&scoped_ref.publisher),
        urlencoding::encode(&scoped_ref.slug)
    );
    let response = client
        .get(&capsule_url)
        .send()
        .await
        .with_context(|| format!("Failed to connect to registry: {}", registry))?;

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        bail!("Capsule not found: {}", scoped_ref.scoped_id);
    }

    let capsule: CapsuleDetail = response
        .error_for_status()
        .with_context(|| format!("Failed to fetch capsule detail: {}", scoped_ref.scoped_id))?
        .json()
        .await
        .with_context(|| format!("Invalid registry response for {}", scoped_ref.scoped_id))?;

    capsule
        .manifest_toml
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("capsule.toml was not returned by registry"))
}

async fn resolve_registry_url(registry_url: Option<&str>, emit_log: bool) -> Result<String> {
    if let Some(url) = registry_url {
        return Ok(url.to_string());
    }

    let resolver = RegistryResolver::default();
    let info = resolver.resolve("localhost").await?;
    if emit_log {
        eprintln!(
            "📡 Using registry: {} ({})",
            info.url,
            format!("{:?}", info.source).to_lowercase()
        );
    }
    Ok(info.url)
}

async fn resolve_manifest_target(
    client: &reqwest::Client,
    base: &str,
    scoped_ref: &ScopedCapsuleRef,
    requested_version: Option<&str>,
    has_token: bool,
    require_current_epoch: bool,
) -> Result<ManifestResolution> {
    if let Some(version) = requested_version
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let endpoint = format!(
            "{}/v1/manifest/resolve/{}/{}/{}",
            base,
            urlencoding::encode(&scoped_ref.publisher),
            urlencoding::encode(&scoped_ref.slug),
            urlencoding::encode(version)
        );
        let response = with_ato_token(client.get(&endpoint))
            .send()
            .await
            .with_context(|| "Failed to resolve versioned manifest hash")?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED && !has_token {
            bail!(
                "{}: registry requires authentication for manifest read APIs. Run `ato login` or set `ATO_TOKEN=<token>`.",
                crate::error_codes::ATO_ERR_AUTH_REQUIRED
            );
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            if let Some(message) = parse_yanked_message(&body) {
                bail!(
                    "{}: {}",
                    crate::error_codes::ATO_ERR_INTEGRITY_FAILURE,
                    message
                );
            }
            bail!(
                "Failed to resolve manifest for {}@{} (status={}): {}",
                scoped_ref.scoped_id,
                version,
                status,
                body
            );
        }
        let payload = response
            .json::<VersionManifestResolveResponse>()
            .await
            .with_context(|| "Invalid version resolve response")?;
        if payload.scoped_id != scoped_ref.scoped_id {
            bail!(
                "version resolve scoped_id mismatch (expected {}, got {})",
                scoped_ref.scoped_id,
                payload.scoped_id
            );
        }
        if payload.version != version {
            bail!(
                "version resolve mismatch (expected {}, got {})",
                version,
                payload.version
            );
        }
        if let Some(yanked_at) = payload.yanked_at.as_deref() {
            bail!(
                "{}: manifest has been yanked by the publisher at {}",
                crate::error_codes::ATO_ERR_INTEGRITY_FAILURE,
                yanked_at
            );
        }
        return Ok(ManifestResolution::Version(payload));
    }

    let epoch_endpoint = format!("{}/v1/manifest/epoch/resolve", base);
    let epoch_response = with_ato_token(
        client
            .post(&epoch_endpoint)
            .json(&serde_json::json!({ "scoped_id": scoped_ref.scoped_id })),
    )
    .send()
    .await
    .with_context(|| "Failed to fetch manifest epoch pointer")?;
    if !epoch_response.status().is_success() {
        if epoch_response.status() == reqwest::StatusCode::UNAUTHORIZED && !has_token {
            bail!(
                "{}: registry requires authentication for manifest read APIs. Run `ato login` or set `ATO_TOKEN=<token>`.",
                crate::error_codes::ATO_ERR_AUTH_REQUIRED
            );
        }
        if require_current_epoch {
            bail!(
                "manifest epoch pointer is required for delta install (status={})",
                epoch_response.status()
            );
        }
        bail!(
            "manifest epoch pointer is required for verified install (status={})",
            epoch_response.status()
        );
    }
    let epoch = epoch_response
        .json::<ManifestEpochResolveResponse>()
        .await
        .with_context(|| "Invalid manifest epoch response")?;
    verify_epoch_signature(&epoch).with_context(|| "Epoch signature verification failed")?;
    Ok(ManifestResolution::Current(epoch))
}

async fn install_manifest_delta_path(
    client: &reqwest::Client,
    registry: &str,
    scoped_ref: &ScopedCapsuleRef,
    requested_version: Option<&str>,
    capsule_toml: Option<&str>,
    capsule_lock: Option<&str>,
) -> Result<DeltaInstallResult> {
    let mut lease_id: Option<String> = None;
    let result = install_manifest_delta_path_inner(
        client,
        registry,
        scoped_ref,
        requested_version,
        capsule_toml,
        capsule_lock,
        &mut lease_id,
    )
    .await;
    if let Some(lease_id) = lease_id {
        let _ = release_lease_best_effort(client, registry, &lease_id).await;
    }
    result
}

async fn install_manifest_delta_path_inner(
    client: &reqwest::Client,
    registry: &str,
    scoped_ref: &ScopedCapsuleRef,
    requested_version: Option<&str>,
    _capsule_toml: Option<&str>,
    capsule_lock: Option<&str>,
    lease_id: &mut Option<String>,
) -> Result<DeltaInstallResult> {
    let base = registry.trim_end_matches('/');
    let has_token = has_ato_token();
    let resolution =
        resolve_manifest_target(client, base, scoped_ref, requested_version, has_token, true)
            .await?;
    let target_manifest_hash = resolution.manifest_hash().to_string();

    let manifest_endpoint = format!(
        "{}/v1/manifest/documents/{}",
        base,
        urlencoding::encode(&target_manifest_hash)
    );
    let manifest_response = with_ato_token(client.get(&manifest_endpoint))
        .send()
        .await
        .with_context(|| "Failed to fetch manifest document for delta install")?;
    let manifest_status = manifest_response.status();
    if !manifest_status.is_success() {
        if manifest_status == reqwest::StatusCode::UNAUTHORIZED && !has_token {
            bail!(
                "{}: registry requires authentication for manifest read APIs. Run `ato login` or set `ATO_TOKEN=<token>`.",
                crate::error_codes::ATO_ERR_AUTH_REQUIRED
            );
        }
        let body = manifest_response.text().await.unwrap_or_default();
        if let Some(message) = parse_yanked_message(&body) {
            bail!(
                "{}: {}",
                crate::error_codes::ATO_ERR_INTEGRITY_FAILURE,
                message
            );
        }
        bail!(
            "Failed to fetch registry manifest for delta install (status={})",
            manifest_status
        );
    }

    let manifest_bytes = manifest_response
        .bytes()
        .await
        .with_context(|| "Failed to read manifest payload for delta install")?
        .to_vec();
    let manifest_toml = String::from_utf8(manifest_bytes)
        .with_context(|| "Remote manifest payload must be UTF-8 TOML")?;
    let manifest: CapsuleManifest = toml::from_str(&manifest_toml)
        .with_context(|| "Invalid remote capsule.toml for delta install")?;
    let manifest_hash = compute_manifest_hash_without_signatures(&manifest)?;
    if normalize_hash_for_compare(&manifest_hash)
        != normalize_hash_for_compare(&target_manifest_hash)
    {
        bail!(
            "Manifest hash mismatch for delta install (expected {}, got {})",
            target_manifest_hash,
            manifest_hash
        );
    }

    let cas_index =
        LocalCasIndex::open_default().with_context(|| "Failed to open local CAS index")?;
    let bloom_wire = cas_index.build_bloom(Some(0.01))?.to_wire();
    let negotiate_request = ManifestNegotiateRequest {
        scoped_id: scoped_ref.scoped_id.clone(),
        target_manifest_hash: target_manifest_hash.clone(),
        have_chunks: Vec::new(),
        have_chunks_bloom: Some(ManifestChunkBloomRequest {
            m_bits: bloom_wire.m_bits,
            k_hashes: bloom_wire.k_hashes,
            seed: bloom_wire.seed,
            bitset_base64: bloom_wire.bitset_base64,
        }),
        reuse_lease_id: None,
        max_bytes: Some(NEGOTIATE_DEFAULT_MAX_BYTES),
    };

    let first_payload = negotiate_manifest(client, base, &negotiate_request, has_token).await?;
    if let Some(id) = first_payload.lease_id.clone() {
        *lease_id = Some(id);
    }

    download_required_chunks(
        client,
        base,
        &cas_index,
        &first_payload.required_chunks,
        lease_id,
        has_token,
    )
    .await?;

    let mut reconstruction = reconstruct_payload_from_local_chunks(&cas_index, &manifest)?;
    if !reconstruction.missing_chunks.is_empty() {
        let reuse_lease = lease_id.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "{}: delta negotiate returned no lease_id; cannot retry exact chunk list.",
                crate::error_codes::ATO_ERR_INTEGRITY_FAILURE
            )
        })?;
        let have_exact = cas_index.available_hashes_for_manifest(
            &manifest_distribution(&manifest)?
                .chunk_list
                .iter()
                .map(|chunk| chunk.chunk_hash.clone())
                .collect::<Vec<_>>(),
        )?;
        let second_request = ManifestNegotiateRequest {
            scoped_id: scoped_ref.scoped_id.clone(),
            target_manifest_hash: target_manifest_hash.clone(),
            have_chunks: have_exact,
            have_chunks_bloom: None,
            reuse_lease_id: Some(reuse_lease),
            max_bytes: Some(NEGOTIATE_DEFAULT_MAX_BYTES),
        };
        let second_payload = negotiate_manifest(client, base, &second_request, has_token).await?;
        if let Some(id) = second_payload.lease_id.clone() {
            *lease_id = Some(id);
        }
        download_required_chunks(
            client,
            base,
            &cas_index,
            &second_payload.required_chunks,
            lease_id,
            has_token,
        )
        .await?;
        reconstruction = reconstruct_payload_from_local_chunks(&cas_index, &manifest)?;
        if !reconstruction.missing_chunks.is_empty() {
            bail!(
                "{}: missing chunks after retry negotiate: {}",
                crate::error_codes::ATO_ERR_INTEGRITY_FAILURE,
                reconstruction.missing_chunks.join(",")
            );
        }
    }

    verify_payload_chunks(&manifest, &reconstruction.payload_tar)?;
    verify_manifest_merkle_root(&manifest)?;

    let payload_tar_zst = {
        let mut encoder = zstd::stream::Encoder::new(Vec::new(), DELTA_RECONSTRUCT_ZSTD_LEVEL)
            .with_context(|| "Failed to create zstd encoder for reconstructed payload")?;
        encoder
            .write_all(&reconstruction.payload_tar)
            .with_context(|| "Failed to encode reconstructed payload.tar.zst")?;
        encoder
            .finish()
            .with_context(|| "Failed to finalize reconstructed payload.tar.zst")?
    };
    let artifact = build_capsule_artifact(Some(&manifest_toml), capsule_lock, &payload_tar_zst)?;
    Ok(DeltaInstallResult::Artifact(artifact))
}

async fn negotiate_manifest(
    client: &reqwest::Client,
    base: &str,
    request: &ManifestNegotiateRequest,
    has_token: bool,
) -> Result<ManifestNegotiateResponse> {
    let endpoint = format!("{}/v1/manifest/negotiate", base);
    let response = with_ato_token(client.post(&endpoint).json(request))
        .send()
        .await
        .with_context(|| "Failed to call manifest negotiate")?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::NOT_IMPLEMENTED {
        bail!("Registry does not support the manifest negotiate API");
    }
    if status == reqwest::StatusCode::UNAUTHORIZED && !has_token {
        bail!(
            "{}: registry requires authentication for manifest read APIs. Run `ato login` or set `ATO_TOKEN=<token>`.",
            crate::error_codes::ATO_ERR_AUTH_REQUIRED
        );
    }
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        if let Some(message) = parse_yanked_message(&body) {
            bail!(
                "{}: {}",
                crate::error_codes::ATO_ERR_INTEGRITY_FAILURE,
                message
            );
        }
        bail!("manifest negotiate failed (status={}): {}", status, body);
    }
    let payload = response
        .json::<ManifestNegotiateResponse>()
        .await
        .with_context(|| "Invalid manifest negotiate response payload")?;
    if payload.yanked.unwrap_or(false) {
        bail!(
            "{}: manifest has been yanked by the publisher.",
            crate::error_codes::ATO_ERR_INTEGRITY_FAILURE
        );
    }
    Ok(payload)
}

async fn download_required_chunks(
    client: &reqwest::Client,
    base: &str,
    cas_index: &LocalCasIndex,
    required_chunks: &[String],
    lease_id: &mut Option<String>,
    has_token: bool,
) -> Result<()> {
    let mut last_refresh = Instant::now();
    for chunk_hash in required_chunks {
        if cas_index.load_chunk_bytes(chunk_hash)?.is_some() {
            continue;
        }
        if lease_id.is_some()
            && last_refresh.elapsed() >= Duration::from_secs(LEASE_REFRESH_INTERVAL_SECS)
        {
            let refreshed =
                refresh_lease(client, base, lease_id.as_deref().unwrap(), has_token).await?;
            *lease_id = Some(refreshed.lease_id);
            last_refresh = Instant::now();
        }
        let endpoint = format!(
            "{}/v1/manifest/chunks/{}",
            base,
            urlencoding::encode(chunk_hash)
        );
        let response = with_ato_token(client.get(&endpoint))
            .send()
            .await
            .with_context(|| format!("Failed to fetch required chunk {}", chunk_hash))?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED && !has_token {
            bail!(
                "{}: registry requires authentication for manifest read APIs. Run `ato login` or set `ATO_TOKEN=<token>`.",
                crate::error_codes::ATO_ERR_AUTH_REQUIRED
            );
        }
        if !response.status().is_success() {
            bail!(
                "{}: failed to fetch chunk {} (status={})",
                crate::error_codes::ATO_ERR_INTEGRITY_FAILURE,
                chunk_hash,
                response.status()
            );
        }
        let bytes = response
            .bytes()
            .await
            .with_context(|| format!("Failed to read required chunk {}", chunk_hash))?;
        cas_index.put_verified_chunk(chunk_hash, &bytes)?;
    }
    Ok(())
}

async fn refresh_lease(
    client: &reqwest::Client,
    base: &str,
    lease_id: &str,
    has_token: bool,
) -> Result<ManifestLeaseRefreshResponse> {
    let endpoint = format!("{}/v1/manifest/leases/refresh", base);
    let response = with_ato_token(client.post(&endpoint).json(&ManifestLeaseRefreshRequest {
        lease_id: lease_id.to_string(),
        ttl_secs: Some(LEASE_REFRESH_INTERVAL_SECS),
    }))
    .send()
    .await
    .with_context(|| "Failed to refresh manifest lease")?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED && !has_token {
        bail!(
            "{}: registry requires authentication for manifest read APIs. Run `ato login` or set `ATO_TOKEN=<token>`.",
            crate::error_codes::ATO_ERR_AUTH_REQUIRED
        );
    }
    if !response.status().is_success() {
        bail!(
            "manifest lease refresh failed (status={})",
            response.status()
        );
    }
    response
        .json::<ManifestLeaseRefreshResponse>()
        .await
        .with_context(|| "Invalid manifest lease refresh response")
}

async fn release_lease_best_effort(
    client: &reqwest::Client,
    registry: &str,
    lease_id: &str,
) -> Result<()> {
    let endpoint = format!(
        "{}/v1/manifest/leases/release",
        registry.trim_end_matches('/')
    );
    let _ = with_ato_token(client.post(&endpoint).json(&ManifestLeaseReleaseRequest {
        lease_id: lease_id.to_string(),
    }))
    .send()
    .await;
    Ok(())
}

#[derive(Debug, Default)]
struct ReconstructResult {
    payload_tar: Vec<u8>,
    missing_chunks: Vec<String>,
}

fn manifest_distribution(
    manifest: &CapsuleManifest,
) -> Result<&capsule_core::types::DistributionInfo> {
    manifest.distribution.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "{}: distribution metadata is missing from capsule.toml",
            crate::error_codes::ATO_ERR_INTEGRITY_FAILURE
        )
    })
}

fn reconstruct_payload_from_local_chunks(
    cas_index: &LocalCasIndex,
    manifest: &CapsuleManifest,
) -> Result<ReconstructResult> {
    let mut payload = Vec::new();
    let mut missing = Vec::new();
    for chunk in &manifest_distribution(manifest)?.chunk_list {
        match cas_index.load_chunk_bytes(&chunk.chunk_hash)? {
            Some(bytes) => {
                if bytes.len() as u64 != chunk.length {
                    bail!(
                        "{}: chunk length mismatch for {} (expected {}, got {})",
                        crate::error_codes::ATO_ERR_INTEGRITY_FAILURE,
                        chunk.chunk_hash,
                        chunk.length,
                        bytes.len()
                    );
                }
                payload.extend_from_slice(&bytes);
            }
            None => {
                missing.push(chunk.chunk_hash.clone());
            }
        }
    }
    Ok(ReconstructResult {
        payload_tar: payload,
        missing_chunks: missing,
    })
}

fn build_capsule_artifact(
    capsule_toml: Option<&str>,
    capsule_lock: Option<&str>,
    payload_tar_zst: &[u8],
) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut out);
        if let Some(manifest_toml) = capsule_toml {
            if !manifest_toml.is_empty() {
                append_capsule_entry(&mut builder, "capsule.toml", manifest_toml.as_bytes())?;
            }
        }
        if let Some(lockfile) = capsule_lock {
            if !lockfile.is_empty() {
                append_capsule_entry(&mut builder, "capsule.lock", lockfile.as_bytes())?;
            }
        }
        append_capsule_entry(&mut builder, "payload.tar.zst", payload_tar_zst)?;
        builder
            .finish()
            .with_context(|| "Failed to finalize reconstructed .capsule archive")?;
    }
    Ok(out)
}

fn append_capsule_entry(
    builder: &mut tar::Builder<&mut Vec<u8>>,
    path: &str,
    bytes: &[u8],
) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_cksum();
    builder
        .append_data(&mut header, path, Cursor::new(bytes))
        .with_context(|| format!("Failed to append {} to reconstructed artifact", path))?;
    Ok(())
}

fn compute_blake3(data: &[u8]) -> String {
    use blake3::Hasher;
    let mut hasher = Hasher::new();
    hasher.update(data);
    let hash = hasher.finalize();
    format!("blake3:{}", hex::encode(hash.as_bytes()))
}

#[cfg(test)]
fn compute_sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

fn equals_hash(expected: &str, got_raw: &str) -> bool {
    let normalized_expected = expected
        .strip_prefix("sha256:")
        .or_else(|| expected.strip_prefix("blake3:"))
        .unwrap_or(expected)
        .to_lowercase();
    let normalized_got = got_raw
        .strip_prefix("sha256:")
        .or_else(|| got_raw.strip_prefix("blake3:"))
        .unwrap_or(got_raw)
        .to_lowercase();
    normalized_expected == normalized_got
}

#[derive(Debug, Deserialize)]
struct YankedResponsePayload {
    #[serde(default)]
    yanked: Option<bool>,
    #[serde(default)]
    message: Option<String>,
}

fn parse_yanked_message(body: &str) -> Option<String> {
    let parsed: YankedResponsePayload = serde_json::from_str(body).ok()?;
    if parsed.yanked.unwrap_or(false) {
        return Some(
            parsed
                .message
                .unwrap_or_else(|| "Manifest has been yanked by the publisher.".to_string()),
        );
    }
    None
}

fn sweep_stale_tmp_capsules(install_dir: &Path) -> Result<()> {
    let entries = match std::fs::read_dir(install_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(anyhow::anyhow!(
                "Failed to read install directory {}: {}",
                install_dir.display(),
                err
            ))
        }
    };
    for entry in entries {
        let entry = entry.with_context(|| {
            format!(
                "Failed to enumerate install directory {}",
                install_dir.display()
            )
        })?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with(".capsule.tmp.") {
            continue;
        }
        let path = entry.path();
        if path.is_file() {
            let _ = std::fs::remove_file(path);
        }
    }
    Ok(())
}

fn write_capsule_atomic(path: &Path, bytes: &[u8], expected_blake3: &str) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        anyhow::anyhow!(
            "Invalid install path without parent directory: {}",
            path.display()
        )
    })?;
    let mut nonce = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut nonce);
    let tmp_path = parent.join(format!(".capsule.tmp.{}", hex::encode(nonce)));

    let result = (|| -> Result<()> {
        let mut file = std::fs::File::create(&tmp_path)
            .with_context(|| format!("Failed to create file: {}", tmp_path.display()))?;
        file.write_all(bytes)
            .with_context(|| format!("Failed to write file: {}", tmp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("Failed to sync file: {}", tmp_path.display()))?;

        let computed = compute_blake3(bytes);
        if !equals_hash(expected_blake3, &computed) {
            bail!(
                "{}: computed artifact hash changed during atomic install write",
                crate::error_codes::ATO_ERR_INTEGRITY_FAILURE
            );
        }

        std::fs::rename(&tmp_path, path).with_context(|| {
            format!(
                "Failed to atomically move {} -> {}",
                tmp_path.display(),
                path.display()
            )
        })?;
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    result
}

async fn verify_manifest_supply_chain(
    client: &reqwest::Client,
    registry: &str,
    scoped_ref: &ScopedCapsuleRef,
    requested_version: Option<&str>,
    artifact_bytes: &[u8],
    allow_unverified: bool,
    allow_downgrade: bool,
) -> Result<()> {
    let base = registry.trim_end_matches('/');
    let endpoint = format!("{}/v1/manifest/epoch/resolve", base);
    let has_token = has_ato_token();
    let resolution = if requested_version.is_some() {
        resolve_manifest_target(
            client,
            base,
            scoped_ref,
            requested_version,
            has_token,
            false,
        )
        .await?
    } else {
        let response = with_ato_token(
            client
                .post(&endpoint)
                .json(&serde_json::json!({ "scoped_id": scoped_ref.scoped_id })),
        )
        .send()
        .await
        .with_context(|| "Failed to fetch manifest epoch pointer")?;
        if !response.status().is_success() {
            if response.status() == reqwest::StatusCode::UNAUTHORIZED && !has_token {
                bail!(
                    "{}: registry requires authentication for manifest read APIs. Run `ato login` or set `ATO_TOKEN=<token>`.",
                    crate::error_codes::ATO_ERR_AUTH_REQUIRED
                );
            }
            if allow_unverified {
                eprintln!(
                    "⚠️  manifest epoch pointer unavailable (status={}): continuing due to --allow-unverified",
                    response.status()
                );
                return Ok(());
            }
            bail!(
                "manifest epoch pointer is required for verified install (status={})",
                response.status()
            );
        }
        let epoch = response
            .json::<ManifestEpochResolveResponse>()
            .await
            .with_context(|| "Invalid manifest epoch response")?;
        verify_epoch_signature(&epoch).with_context(|| "Epoch signature verification failed")?;
        ManifestResolution::Current(epoch)
    };
    let target_manifest_hash = resolution.manifest_hash().to_string();

    let local_manifest_bytes = extract_manifest_toml_from_capsule(artifact_bytes)
        .with_context(|| "capsule.toml is required in artifact")?;
    let local_manifest: CapsuleManifest =
        toml::from_str(&local_manifest_bytes).with_context(|| "Invalid local capsule.toml")?;
    let local_manifest_hash = compute_manifest_hash_without_signatures(&local_manifest)?;
    if normalize_hash_for_compare(&local_manifest_hash)
        != normalize_hash_for_compare(&target_manifest_hash)
    {
        bail!(
            "Artifact manifest hash mismatch against resolved manifest (expected {}, got {})",
            target_manifest_hash,
            local_manifest_hash
        );
    }

    let manifest_endpoint = format!(
        "{}/v1/manifest/documents/{}",
        base,
        urlencoding::encode(&target_manifest_hash)
    );
    let manifest_response = with_ato_token(client.get(&manifest_endpoint))
        .send()
        .await
        .with_context(|| "Failed to fetch manifest payload")?;
    let manifest_status = manifest_response.status();
    if manifest_status.is_success() {
        let remote_manifest_bytes = manifest_response
            .bytes()
            .await
            .with_context(|| "Failed to read remote manifest payload")?;
        let remote_manifest_toml = String::from_utf8(remote_manifest_bytes.to_vec())
            .with_context(|| "Remote manifest payload must be UTF-8 TOML")?;
        let remote_manifest: CapsuleManifest =
            toml::from_str(&remote_manifest_toml).with_context(|| "Invalid remote capsule.toml")?;
        let remote_manifest_hash = compute_manifest_hash_without_signatures(&remote_manifest)?;
        if normalize_hash_for_compare(&remote_manifest_hash)
            != normalize_hash_for_compare(&target_manifest_hash)
        {
            bail!(
                "Remote manifest hash mismatch against resolved manifest (expected {}, got {})",
                target_manifest_hash,
                remote_manifest_hash
            );
        }
    } else if manifest_status == reqwest::StatusCode::UNAUTHORIZED && !has_token {
        bail!(
            "{}: registry requires authentication for manifest read APIs. Run `ato login` or set `ATO_TOKEN=<token>`.",
            crate::error_codes::ATO_ERR_AUTH_REQUIRED
        );
    } else {
        let body = manifest_response.text().await.unwrap_or_default();
        if let Some(message) = parse_yanked_message(&body) {
            bail!(
                "{}: {}",
                crate::error_codes::ATO_ERR_INTEGRITY_FAILURE,
                message
            );
        }
    }
    if !manifest_status.is_success() && !allow_unverified {
        bail!(
            "Failed to fetch registry manifest (status={})",
            manifest_status
        );
    }

    let payload_tar_bytes = extract_payload_tar_from_capsule(artifact_bytes)?;
    verify_payload_chunks(&local_manifest, &payload_tar_bytes)?;
    verify_manifest_merkle_root(&local_manifest)?;

    if let ManifestResolution::Current(epoch) = resolution {
        enforce_epoch_monotonicity(
            &scoped_ref.scoped_id,
            epoch.pointer.epoch,
            &epoch.pointer.manifest_hash,
            allow_downgrade,
        )?;
    }

    Ok(())
}

fn with_ato_token(request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    if let Some(token) = current_ato_token() {
        request.header("authorization", format!("Bearer {}", token))
    } else {
        request
    }
}

fn has_ato_token() -> bool {
    current_ato_token().is_some()
}

fn current_ato_token() -> Option<String> {
    std::env::var("ATO_TOKEN")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn verify_epoch_signature(epoch: &ManifestEpochResolveResponse) -> Result<()> {
    let pub_bytes = BASE64
        .decode(epoch.public_key.as_bytes())
        .with_context(|| "Invalid base64 public key")?;
    if pub_bytes.len() != 32 {
        bail!(
            "Invalid manifest epoch public key length: {}",
            pub_bytes.len()
        );
    }
    let mut pubkey = [0u8; 32];
    pubkey.copy_from_slice(&pub_bytes);
    let did = public_key_to_did(&pubkey);
    if did != epoch.pointer.signer_did {
        bail!(
            "Epoch signer DID mismatch (expected {}, got {})",
            epoch.pointer.signer_did,
            did
        );
    }
    let verifying_key =
        VerifyingKey::from_bytes(&pubkey).with_context(|| "Invalid manifest epoch public key")?;
    let signature_bytes = BASE64
        .decode(epoch.pointer.signature.as_bytes())
        .with_context(|| "Invalid base64 epoch signature")?;
    if signature_bytes.len() != 64 {
        bail!(
            "Invalid manifest epoch signature length: {}",
            signature_bytes.len()
        );
    }
    let mut sig = [0u8; 64];
    sig.copy_from_slice(&signature_bytes);
    let signature = Signature::from_bytes(&sig);
    let unsigned = serde_json::json!({
        "scoped_id": epoch.pointer.scoped_id,
        "epoch": epoch.pointer.epoch,
        "manifest_hash": epoch.pointer.manifest_hash,
        "prev_epoch_hash": epoch.pointer.prev_epoch_hash,
        "issued_at": epoch.pointer.issued_at,
        "signer_did": epoch.pointer.signer_did,
        "key_id": epoch.pointer.key_id,
    });
    let canonical = serde_jcs::to_vec(&unsigned)?;
    verifying_key
        .verify(&canonical, &signature)
        .with_context(|| "ed25519 verification failed")?;
    Ok(())
}

fn extract_manifest_toml_from_capsule(bytes: &[u8]) -> Result<String> {
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let entries = archive
        .entries()
        .context("Failed to read .capsule archive entries")?;
    for entry in entries {
        let mut entry = entry.context("Invalid .capsule entry")?;
        let path = entry.path().context("Failed to read archive entry path")?;
        if path.to_string_lossy() == "capsule.toml" {
            let mut manifest = Vec::new();
            entry
                .read_to_end(&mut manifest)
                .context("Failed to read capsule.toml from artifact")?;
            return String::from_utf8(manifest).with_context(|| "capsule.toml must be UTF-8");
        }
    }
    bail!("Invalid artifact: capsule.toml not found in .capsule archive")
}

fn extract_payload_tar_from_capsule(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let entries = archive
        .entries()
        .context("Failed to read .capsule archive entries")?;
    for entry in entries {
        let mut entry = entry.context("Invalid .capsule entry")?;
        let path = entry.path().context("Failed to read archive entry path")?;
        if path.to_string_lossy() == "payload.tar.zst" {
            let mut payload_zst = Vec::new();
            entry
                .read_to_end(&mut payload_zst)
                .context("Failed to read payload.tar.zst from artifact")?;
            let mut decoder = zstd::stream::Decoder::new(Cursor::new(payload_zst))
                .with_context(|| "Failed to decode payload.tar.zst")?;
            let mut payload_tar = Vec::new();
            decoder
                .read_to_end(&mut payload_tar)
                .context("Failed to read payload.tar bytes")?;
            return Ok(payload_tar);
        }
    }
    bail!("Invalid artifact: payload.tar.zst not found in .capsule archive")
}

fn compute_manifest_hash_without_signatures(manifest: &CapsuleManifest) -> Result<String> {
    manifest_payload::compute_manifest_hash_without_signatures(manifest)
        .map_err(anyhow::Error::from)
}

fn verify_payload_chunks(manifest: &CapsuleManifest, payload_tar: &[u8]) -> Result<()> {
    let distribution = manifest_distribution(manifest)?;
    let mut next_offset = 0u64;
    for chunk in &distribution.chunk_list {
        if chunk.offset != next_offset {
            bail!(
                "manifest chunk_list offset mismatch: expected {}, got {}",
                next_offset,
                chunk.offset
            );
        }
        let start = chunk.offset as usize;
        let end = start.saturating_add(chunk.length as usize);
        if end > payload_tar.len() {
            bail!(
                "manifest chunk range out of bounds: {}..{} (payload={})",
                start,
                end,
                payload_tar.len()
            );
        }
        let actual = format!("blake3:{}", blake3::hash(&payload_tar[start..end]).to_hex());
        if normalize_hash_for_compare(&actual) != normalize_hash_for_compare(&chunk.chunk_hash) {
            bail!(
                "manifest chunk hash mismatch at offset {}: expected {}, got {}",
                chunk.offset,
                chunk.chunk_hash,
                actual
            );
        }
        next_offset = chunk.offset.saturating_add(chunk.length);
    }
    if next_offset != payload_tar.len() as u64 {
        bail!(
            "manifest chunk coverage mismatch: covered {}, payload {}",
            next_offset,
            payload_tar.len()
        );
    }
    Ok(())
}

fn verify_manifest_merkle_root(manifest: &CapsuleManifest) -> Result<()> {
    let distribution = manifest_distribution(manifest)?;
    let mut level: Vec<[u8; 32]> = manifest
        .distribution
        .as_ref()
        .expect("distribution metadata")
        .chunk_list
        .iter()
        .map(|chunk| {
            let normalized = normalize_hash_for_compare(&chunk.chunk_hash);
            let decoded = hex::decode(normalized).unwrap_or_default();
            let mut out = [0u8; 32];
            if decoded.len() == 32 {
                out.copy_from_slice(&decoded);
            }
            out
        })
        .collect();
    let actual_merkle = if level.is_empty() {
        format!("blake3:{}", blake3::hash(b"").to_hex())
    } else {
        while level.len() > 1 {
            let mut next = Vec::with_capacity(level.len().div_ceil(2));
            let mut idx = 0usize;
            while idx < level.len() {
                let left = level[idx];
                let right = if idx + 1 < level.len() {
                    level[idx + 1]
                } else {
                    level[idx]
                };
                let mut hasher = blake3::Hasher::new();
                hasher.update(&left);
                hasher.update(&right);
                let digest = hasher.finalize();
                let mut out = [0u8; 32];
                out.copy_from_slice(digest.as_bytes());
                next.push(out);
                idx += 2;
            }
            level = next;
        }
        format!("blake3:{}", hex::encode(level[0]))
    };
    if normalize_hash_for_compare(&actual_merkle)
        != normalize_hash_for_compare(&distribution.merkle_root)
    {
        bail!(
            "manifest merkle_root mismatch: expected {}, got {}",
            distribution.merkle_root,
            actual_merkle
        );
    }
    Ok(())
}

fn normalize_hash_for_compare(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("sha256:")
        .trim_start_matches("blake3:")
        .to_ascii_lowercase()
}

fn epoch_guard_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".ato")
        .join("state")
        .join("epoch-guard.json")
}

fn enforce_epoch_monotonicity(
    scoped_id: &str,
    epoch: u64,
    manifest_hash: &str,
    allow_downgrade: bool,
) -> Result<()> {
    enforce_epoch_monotonicity_at(
        &epoch_guard_path(),
        scoped_id,
        epoch,
        manifest_hash,
        allow_downgrade,
    )
}

fn enforce_epoch_monotonicity_at(
    state_path: &Path,
    scoped_id: &str,
    epoch: u64,
    manifest_hash: &str,
    allow_downgrade: bool,
) -> Result<()> {
    let mut state = load_epoch_guard_state(state_path)?;
    let manifest_norm = normalize_hash_for_compare(manifest_hash);
    let now = chrono::Utc::now().to_rfc3339();

    if let Some(previous) = state.capsules.get(scoped_id) {
        if epoch == previous.max_epoch
            && normalize_hash_for_compare(&previous.manifest_hash) != manifest_norm
        {
            bail!(
                "Epoch replay mismatch for {} at epoch {}: manifest differs from previously trusted value",
                scoped_id,
                epoch
            );
        }
        if epoch < previous.max_epoch && !allow_downgrade {
            bail!(
                "Downgrade detected for {}: remote epoch {} is older than trusted epoch {}. Re-run with --allow-downgrade to proceed.",
                scoped_id,
                epoch,
                previous.max_epoch
            );
        }
    }

    let mut should_persist = false;
    match state.capsules.get_mut(scoped_id) {
        Some(entry) => {
            if epoch > entry.max_epoch {
                entry.max_epoch = epoch;
                entry.manifest_hash = manifest_hash.to_string();
                entry.updated_at = now;
                should_persist = true;
            }
        }
        None => {
            state.capsules.insert(
                scoped_id.to_string(),
                EpochGuardEntry {
                    max_epoch: epoch,
                    manifest_hash: manifest_hash.to_string(),
                    updated_at: now,
                },
            );
            should_persist = true;
        }
    }

    if should_persist {
        write_epoch_guard_state_atomic(state_path, &state)?;
    }
    Ok(())
}

fn load_epoch_guard_state(path: &Path) -> Result<EpochGuardState> {
    if !path.exists() {
        return Ok(EpochGuardState::default());
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read epoch guard state: {}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(EpochGuardState::default());
    }
    serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse epoch guard state: {}", path.display()))
}

fn write_epoch_guard_state_atomic(path: &Path, state: &EpochGuardState) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "Failed to create epoch guard state directory: {}",
                parent.display()
            )
        })?;
    }

    let payload = serde_json::to_vec_pretty(state).context("Failed to serialize epoch guard")?;
    let mut nonce = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut nonce);
    let tmp_name = format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("epoch-guard"),
        hex::encode(nonce)
    );
    let tmp_path = path.with_file_name(tmp_name);
    {
        let mut file = std::fs::File::create(&tmp_path)
            .with_context(|| format!("Failed to create {}", tmp_path.display()))?;
        file.write_all(&payload)
            .with_context(|| format!("Failed to write {}", tmp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("Failed to flush {}", tmp_path.display()))?;
    }
    std::fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "Failed to atomically replace epoch guard state at {}",
            path.display()
        )
    })?;
    Ok(())
}

pub async fn suggest_scoped_capsules(
    slug: &str,
    registry_url: Option<&str>,
    limit: usize,
) -> Result<Vec<ScopedSuggestion>> {
    let registry = resolve_registry_url(registry_url, false).await?;
    let client = reqwest::Client::new();
    let url = format!(
        "{}/v1/manifest/capsules?q={}&limit={}",
        registry,
        urlencoding::encode(slug),
        limit.clamp(1, 10)
    );
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| "Failed to fetch capsule suggestions")?;
    if !response.status().is_success() {
        return Ok(vec![]);
    }

    let payload = response
        .json::<SuggestionCapsulesResponse>()
        .await
        .with_context(|| "Invalid suggestions response")?;

    let needle = slug.trim().to_lowercase();
    let mut suggestions: Vec<ScopedSuggestion> = payload
        .capsules
        .into_iter()
        .filter_map(|capsule| {
            let scoped_id = capsule.scoped_id.or_else(|| {
                capsule
                    .publisher
                    .as_ref()
                    .map(|publisher| format!("{}/{}", publisher.handle, capsule.slug))
            })?;
            let capsule_slug = capsule.slug.to_lowercase();
            if capsule_slug != needle && !capsule_slug.ends_with(&needle) {
                return None;
            }
            Some(ScopedSuggestion {
                scoped_id,
                downloads: capsule.downloads.unwrap_or(0),
            })
        })
        .collect();
    suggestions.sort_by(|a, b| b.downloads.cmp(&a.downloads));
    suggestions.truncate(3);
    Ok(suggestions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::OnceLock;

    use axum::extract::{Path as AxumPath, State};
    use axum::http::{header::HOST, HeaderMap, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use ed25519_dalek::{Signer as _, SigningKey};
    use tokio::sync::Mutex;

    const TEST_SCOPED_ID: &str = "koh0920/sample";
    const TEST_VERSION: &str = "1.0.0";
    const TEST_LEASE_ID: &str = "lease-test-1";

    fn assert_json_object_has_keys(value: &serde_json::Value, keys: &[&str]) {
        let object = value.as_object().expect("expected JSON object");
        for key in keys {
            assert!(
                object.contains_key(*key),
                "expected key '{}' in JSON object: {object:?}",
                key
            );
        }
    }

    fn test_env_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    async fn acquire_test_env_lock() -> tokio::sync::MutexGuard<'static, ()> {
        test_env_lock().lock().await
    }

    struct EnvVarGuard {
        key: String,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &str, value: Option<&str>) -> Self {
            let previous = std::env::var(key).ok();
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
            Self {
                key: key.to_string(),
                previous,
            }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(value) = &self.previous {
                std::env::set_var(&self.key, value);
            } else {
                std::env::remove_var(&self.key);
            }
        }
    }

    #[test]
    fn native_install_documented_json_contract_fields_are_present() {
        let value = serde_json::to_value(InstallResult {
            capsule_id: "capsule-123".to_string(),
            scoped_id: "koh0920/sample".to_string(),
            publisher: "koh0920".to_string(),
            slug: "sample".to_string(),
            version: "1.0.0".to_string(),
            path: PathBuf::from("/tmp/sample.capsule"),
            content_hash: "blake3:artifact".to_string(),
            install_kind: InstallKind::NativeRequiresLocalDerivation,
            launchable: Some(LaunchableTarget::DerivedApp {
                path: PathBuf::from("/tmp/MyApp.app"),
            }),
            local_derivation: Some(LocalDerivationInfo {
                schema_version: "0.1".to_string(),
                performed: true,
                fetched_dir: PathBuf::from("/tmp/fetch"),
                derived_app_path: Some(PathBuf::from("/tmp/MyApp.app")),
                provenance_path: Some(PathBuf::from("/tmp/local-derivation.json")),
                parent_digest: Some("blake3:parent".to_string()),
                derived_digest: Some("blake3:derived".to_string()),
            }),
            projection: Some(ProjectionInfo {
                performed: true,
                projection_id: Some("projection-123".to_string()),
                projected_path: Some(PathBuf::from("/Applications/MyApp.app")),
                state: Some("ok".to_string()),
                schema_version: Some("0.1".to_string()),
                metadata_path: Some(PathBuf::from("/tmp/projection.json")),
            }),
        })
        .expect("serialize install result");

        assert_json_object_has_keys(
            &value,
            &[
                "install_kind",
                "launchable",
                "local_derivation",
                "projection",
            ],
        );

        assert_json_object_has_keys(
            &value["local_derivation"],
            &[
                "schema_version",
                "provenance_path",
                "parent_digest",
                "derived_digest",
            ],
        );

        assert_json_object_has_keys(
            &value["projection"],
            &["schema_version", "metadata_path", "state"],
        );
    }

    fn test_scoped_ref() -> ScopedCapsuleRef {
        parse_capsule_ref(TEST_SCOPED_ID).expect("valid scoped ref")
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum MockScenario {
        FalsePositiveRecovery,
        FallbackNotImplemented,
        UnauthorizedManifest,
        LeaseReleaseOnFailure,
        YankedNegotiate,
        YankedManifest,
    }

    #[derive(Debug, Clone)]
    struct MockRegistryFixture {
        scoped_id: String,
        publisher: String,
        slug: String,
        version: String,
        manifest_hash: String,
        manifest_toml: String,
        payload_tar: Vec<u8>,
        artifact_bytes: Vec<u8>,
        chunk_hashes: Vec<String>,
        chunk_bytes: HashMap<String, Vec<u8>>,
        lease_id: String,
        epoch_response: serde_json::Value,
    }

    #[derive(Debug, Clone, Default)]
    struct RecordedNegotiateRequest {
        has_bloom: bool,
        have_chunks_len: usize,
        reuse_lease_id: Option<String>,
    }

    #[derive(Debug, Clone, Default)]
    struct MockObservations {
        epoch_calls: usize,
        version_resolve_calls: usize,
        manifest_calls: usize,
        negotiate_calls: Vec<RecordedNegotiateRequest>,
        chunk_calls: Vec<String>,
        distribution_calls: usize,
        artifact_calls: usize,
        release_calls: Vec<String>,
    }

    #[derive(Debug)]
    struct MockRegistryState {
        scenario: MockScenario,
        fixture: MockRegistryFixture,
        observations: MockObservations,
    }

    type SharedMockState = std::sync::Arc<Mutex<MockRegistryState>>;

    struct MockRegistryHandle {
        base_url: String,
        state: SharedMockState,
        task: tokio::task::JoinHandle<()>,
    }

    impl MockRegistryHandle {
        fn base_url(&self) -> &str {
            &self.base_url
        }

        async fn observations(&self) -> MockObservations {
            self.state.lock().await.observations.clone()
        }
    }

    impl Drop for MockRegistryHandle {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    fn compute_merkle_root_for_test(chunk_hashes: &[String]) -> String {
        let mut level: Vec<[u8; 32]> = chunk_hashes
            .iter()
            .map(|chunk_hash| {
                let normalized = normalize_hash_for_compare(chunk_hash);
                let bytes = hex::decode(normalized).expect("hex decode");
                let mut out = [0u8; 32];
                out.copy_from_slice(&bytes);
                out
            })
            .collect();
        if level.is_empty() {
            return format!("blake3:{}", blake3::hash(b"").to_hex());
        }
        while level.len() > 1 {
            let mut next = Vec::with_capacity(level.len().div_ceil(2));
            let mut idx = 0usize;
            while idx < level.len() {
                let left = level[idx];
                let right = if idx + 1 < level.len() {
                    level[idx + 1]
                } else {
                    level[idx]
                };
                let mut hasher = blake3::Hasher::new();
                hasher.update(&left);
                hasher.update(&right);
                next.push(*hasher.finalize().as_bytes());
                idx += 2;
            }
            level = next;
        }
        format!("blake3:{}", hex::encode(level[0]))
    }

    fn build_mock_fixture(
        scoped_id: &str,
        version: &str,
        chunks: Vec<Vec<u8>>,
    ) -> MockRegistryFixture {
        let (publisher, slug) = scoped_id
            .split_once('/')
            .expect("scoped_id must be publisher/slug");

        let mut chunk_hashes = Vec::new();
        let mut chunk_list = Vec::new();
        let mut chunk_bytes = HashMap::new();
        let mut payload_tar = Vec::new();
        let mut offset = 0u64;
        for bytes in chunks {
            let chunk_hash = format!("blake3:{}", blake3::hash(&bytes).to_hex());
            chunk_hashes.push(chunk_hash.clone());
            chunk_bytes.insert(chunk_hash.clone(), bytes.clone());
            chunk_list.push(capsule_core::types::ChunkDescriptor {
                chunk_hash,
                offset,
                length: bytes.len() as u64,
                codec: "fastcdc".to_string(),
                compression: "none".to_string(),
            });
            payload_tar.extend_from_slice(&bytes);
            offset += bytes.len() as u64;
        }
        let merkle_root = compute_merkle_root_for_test(&chunk_hashes);
        let mut manifest = CapsuleManifest::from_toml(
            r#"
schema_version = "1"
name = "sample"
version = "1.0.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
entrypoint = "main.py"
"#,
        )
        .expect("manifest");
        manifest.distribution = Some(capsule_core::types::DistributionInfo {
            manifest_hash: String::new(),
            merkle_root,
            chunk_list,
            signatures: vec![],
        });
        let manifest_hash =
            compute_manifest_hash_without_signatures(&manifest).expect("manifest hash");
        manifest
            .distribution
            .as_mut()
            .expect("distribution")
            .manifest_hash = manifest_hash.clone();
        let manifest_toml = toml::to_string_pretty(&manifest).expect("manifest TOML");

        let payload_tar_zst = {
            let mut encoder = zstd::stream::Encoder::new(Vec::new(), DELTA_RECONSTRUCT_ZSTD_LEVEL)
                .expect("zstd encoder");
            encoder
                .write_all(&payload_tar)
                .expect("write payload tar bytes");
            encoder.finish().expect("finish zstd stream")
        };
        let artifact_bytes = build_capsule_artifact(Some(&manifest_toml), None, &payload_tar_zst)
            .expect("build artifact");

        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let verifying_key = signing_key.verifying_key();
        let signer_did = public_key_to_did(&verifying_key.to_bytes());
        let issued_at = "2026-03-05T00:00:00Z";
        let key_id = "k-main";
        let unsigned_pointer = serde_json::json!({
            "scoped_id": scoped_id,
            "epoch": 1u64,
            "manifest_hash": manifest_hash,
            "prev_epoch_hash": serde_json::Value::Null,
            "issued_at": issued_at,
            "signer_did": signer_did,
            "key_id": key_id,
        });
        let canonical_pointer = serde_jcs::to_vec(&unsigned_pointer).expect("canonical pointer");
        let signature = signing_key.sign(&canonical_pointer);
        let epoch_response = serde_json::json!({
            "pointer": {
                "scoped_id": scoped_id,
                "epoch": 1u64,
                "manifest_hash": manifest_hash,
                "prev_epoch_hash": serde_json::Value::Null,
                "issued_at": issued_at,
                "signer_did": signer_did,
                "key_id": key_id,
                "signature": BASE64.encode(signature.to_bytes()),
            },
            "public_key": BASE64.encode(verifying_key.to_bytes()),
        });

        MockRegistryFixture {
            scoped_id: scoped_id.to_string(),
            publisher: publisher.to_string(),
            slug: slug.to_string(),
            version: version.to_string(),
            manifest_hash,
            manifest_toml,
            payload_tar,
            artifact_bytes,
            chunk_hashes,
            chunk_bytes,
            lease_id: TEST_LEASE_ID.to_string(),
            epoch_response,
        }
    }

    fn build_payload_tar_with_source(path: &str, source: &[u8]) -> Vec<u8> {
        let mut payload = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut payload);
            let mut header = tar::Header::new_gnu();
            header.set_path(path).expect("set payload path");
            header.set_mode(0o644);
            header.set_size(source.len() as u64);
            header.set_mtime(0);
            header.set_cksum();
            builder
                .append_data(&mut header, path, Cursor::new(source))
                .expect("append payload source");
            builder.finish().expect("finish payload tar");
        }
        payload
    }

    async fn spawn_mock_registry(
        scenario: MockScenario,
        fixture: MockRegistryFixture,
    ) -> MockRegistryHandle {
        let state = std::sync::Arc::new(Mutex::new(MockRegistryState {
            scenario,
            fixture,
            observations: MockObservations::default(),
        }));
        let app = Router::new()
            .route("/v1/manifest/epoch/resolve", post(mock_epoch_resolve))
            .route(
                "/v1/manifest/resolve/:publisher/:slug/:version",
                get(mock_version_resolve),
            )
            .route("/v1/manifest/documents/:manifest_hash", get(mock_manifest))
            .route("/v1/manifest/negotiate", post(mock_negotiate))
            .route("/v1/manifest/chunks/:chunk_hash", get(mock_chunk))
            .route("/v1/manifest/leases/refresh", post(mock_lease_refresh))
            .route("/v1/manifest/leases/release", post(mock_lease_release))
            .route(
                "/v1/manifest/capsules/by/:publisher/:slug",
                get(mock_capsule_detail),
            )
            .route(
                "/v1/manifest/capsules/by/:publisher/:slug/distributions",
                get(mock_distribution),
            )
            .route("/mock/artifact.capsule", get(mock_artifact))
            .with_state(state.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock registry");
        let addr = listener.local_addr().expect("mock registry local addr");
        let task = tokio::spawn(async move {
            if let Err(err) = axum::serve(listener, app).await {
                eprintln!("mock registry server error: {}", err);
            }
        });

        MockRegistryHandle {
            base_url: format!("http://{}", addr),
            state,
            task,
        }
    }

    async fn mock_epoch_resolve(State(state): State<SharedMockState>) -> Response {
        let mut guard = state.lock().await;
        guard.observations.epoch_calls += 1;
        match guard.scenario {
            MockScenario::UnauthorizedManifest => StatusCode::UNAUTHORIZED.into_response(),
            MockScenario::FallbackNotImplemented if guard.observations.epoch_calls >= 2 => {
                StatusCode::SERVICE_UNAVAILABLE.into_response()
            }
            _ => Json(guard.fixture.epoch_response.clone()).into_response(),
        }
    }

    async fn mock_version_resolve(
        State(state): State<SharedMockState>,
        AxumPath((publisher, slug, version)): AxumPath<(String, String, String)>,
    ) -> Response {
        let mut guard = state.lock().await;
        guard.observations.version_resolve_calls += 1;
        if publisher != guard.fixture.publisher
            || slug != guard.fixture.slug
            || version != guard.fixture.version
        {
            return StatusCode::NOT_FOUND.into_response();
        }
        Json(serde_json::json!({
            "scoped_id": guard.fixture.scoped_id,
            "version": guard.fixture.version,
            "manifest_hash": guard.fixture.manifest_hash,
            "yanked_at": serde_json::Value::Null,
        }))
        .into_response()
    }

    async fn mock_manifest(
        State(state): State<SharedMockState>,
        AxumPath(manifest_hash): AxumPath<String>,
    ) -> Response {
        let mut guard = state.lock().await;
        guard.observations.manifest_calls += 1;
        if guard.scenario == MockScenario::UnauthorizedManifest {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        if guard.scenario == MockScenario::YankedManifest {
            return (
                StatusCode::GONE,
                Json(serde_json::json!({
                    "error": "manifest_yanked",
                    "message": "Manifest has been yanked by the publisher.",
                    "yanked": true
                })),
            )
                .into_response();
        }
        if normalize_hash_for_compare(&manifest_hash)
            != normalize_hash_for_compare(&guard.fixture.manifest_hash)
        {
            return StatusCode::NOT_FOUND.into_response();
        }
        (StatusCode::OK, guard.fixture.manifest_toml.clone()).into_response()
    }

    async fn mock_negotiate(
        State(state): State<SharedMockState>,
        Json(request): Json<ManifestNegotiateRequest>,
    ) -> Response {
        let mut guard = state.lock().await;
        guard
            .observations
            .negotiate_calls
            .push(RecordedNegotiateRequest {
                has_bloom: request.have_chunks_bloom.is_some(),
                have_chunks_len: request.have_chunks.len(),
                reuse_lease_id: request.reuse_lease_id.clone(),
            });
        let call_index = guard.observations.negotiate_calls.len();
        match guard.scenario {
            MockScenario::FallbackNotImplemented => StatusCode::NOT_IMPLEMENTED.into_response(),
            MockScenario::UnauthorizedManifest => StatusCode::UNAUTHORIZED.into_response(),
            MockScenario::YankedNegotiate => (
                StatusCode::GONE,
                Json(serde_json::json!({
                    "error": "manifest_yanked",
                    "message": "Manifest has been yanked by the publisher.",
                    "yanked": true
                })),
            )
                .into_response(),
            MockScenario::YankedManifest => Json(serde_json::json!({
                "session_id": format!("session-{}", call_index),
                "required_chunks": [],
                "required_manifests": [],
                "lease_id": guard.fixture.lease_id,
                "lease_expires_at": "2026-03-05T00:15:00Z",
            }))
            .into_response(),
            MockScenario::LeaseReleaseOnFailure => Json(serde_json::json!({
                "session_id": format!("session-{}", call_index),
                "required_chunks": [guard.fixture.chunk_hashes[0].clone()],
                "required_manifests": [],
                "lease_id": guard.fixture.lease_id,
                "lease_expires_at": "2026-03-05T00:15:00Z",
            }))
            .into_response(),
            MockScenario::FalsePositiveRecovery => {
                let lease_id = guard.fixture.lease_id.clone();
                if call_index == 1 {
                    Json(serde_json::json!({
                        "session_id": "session-1",
                        "required_chunks": [guard.fixture.chunk_hashes[0].clone()],
                        "required_manifests": [],
                        "lease_id": lease_id,
                        "lease_expires_at": "2026-03-05T00:15:00Z",
                    }))
                    .into_response()
                } else {
                    Json(serde_json::json!({
                        "session_id": "session-2",
                        "required_chunks": [guard.fixture.chunk_hashes[1].clone()],
                        "required_manifests": [],
                        "lease_id": lease_id,
                        "lease_expires_at": "2026-03-05T00:15:00Z",
                    }))
                    .into_response()
                }
            }
        }
    }

    async fn mock_chunk(
        State(state): State<SharedMockState>,
        AxumPath(chunk_hash): AxumPath<String>,
    ) -> Response {
        let mut guard = state.lock().await;
        guard.observations.chunk_calls.push(chunk_hash.clone());
        if guard.scenario == MockScenario::UnauthorizedManifest {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        if guard.scenario == MockScenario::LeaseReleaseOnFailure {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        let bytes = guard.fixture.chunk_bytes.iter().find_map(|(hash, bytes)| {
            if normalize_hash_for_compare(hash) == normalize_hash_for_compare(&chunk_hash) {
                Some(bytes.clone())
            } else {
                None
            }
        });
        match bytes {
            Some(bytes) => (StatusCode::OK, bytes).into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        }
    }

    async fn mock_lease_refresh(
        State(state): State<SharedMockState>,
        Json(payload): Json<serde_json::Value>,
    ) -> Response {
        let guard = state.lock().await;
        let lease_id = payload
            .get("lease_id")
            .and_then(|value| value.as_str())
            .unwrap_or(guard.fixture.lease_id.as_str());
        Json(serde_json::json!({
            "lease_id": lease_id,
            "expires_at": "2026-03-05T00:20:00Z",
            "chunk_count": guard.fixture.chunk_hashes.len(),
        }))
        .into_response()
    }

    async fn mock_lease_release(
        State(state): State<SharedMockState>,
        Json(payload): Json<serde_json::Value>,
    ) -> Response {
        let mut guard = state.lock().await;
        if let Some(lease_id) = payload.get("lease_id").and_then(|value| value.as_str()) {
            guard.observations.release_calls.push(lease_id.to_string());
        }
        StatusCode::OK.into_response()
    }

    async fn mock_capsule_detail(
        State(state): State<SharedMockState>,
        AxumPath((publisher, slug)): AxumPath<(String, String)>,
    ) -> Response {
        let guard = state.lock().await;
        if publisher != guard.fixture.publisher || slug != guard.fixture.slug {
            return StatusCode::NOT_FOUND.into_response();
        }
        Json(serde_json::json!({
            "id": format!("capsule-{}-{}", guard.fixture.publisher, guard.fixture.slug),
            "scoped_id": guard.fixture.scoped_id,
            "slug": guard.fixture.slug,
            "name": "Mock Capsule",
            "description": "mock description",
            "price": 0,
            "currency": "USD",
            "latestVersion": guard.fixture.version,
            "releases": [{
                "version": guard.fixture.version,
                "content_hash": compute_blake3(&guard.fixture.artifact_bytes),
                "signature_status": "verified",
            }],
        }))
        .into_response()
    }

    async fn mock_distribution(
        State(state): State<SharedMockState>,
        AxumPath((publisher, slug)): AxumPath<(String, String)>,
        headers: HeaderMap,
    ) -> Response {
        let mut guard = state.lock().await;
        if publisher != guard.fixture.publisher || slug != guard.fixture.slug {
            return StatusCode::NOT_FOUND.into_response();
        }
        guard.observations.distribution_calls += 1;
        let host = headers
            .get(HOST)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("127.0.0.1");
        Json(serde_json::json!({
            "version": guard.fixture.version,
            "artifact_url": format!("http://{}/mock/artifact.capsule", host),
            "sha256": compute_sha256(&guard.fixture.artifact_bytes),
            "blake3": compute_blake3(&guard.fixture.artifact_bytes),
            "file_name": format!("{}-{}.capsule", guard.fixture.slug, guard.fixture.version),
        }))
        .into_response()
    }

    async fn mock_artifact(State(state): State<SharedMockState>) -> Response {
        let mut guard = state.lock().await;
        guard.observations.artifact_calls += 1;
        (StatusCode::OK, guard.fixture.artifact_bytes.clone()).into_response()
    }

    #[test]
    fn test_compute_blake3() {
        let data = b"hello world";
        let hash = compute_blake3(data);
        assert!(hash.starts_with("blake3:"));
        assert_eq!(hash.len(), 7 + 64);
    }

    #[test]
    fn test_compute_sha256() {
        let data = b"hello world";
        let hash = compute_sha256(data);
        assert_eq!(hash.len(), 64);
    }

    #[test]
    fn test_equals_hash() {
        let value = "b94d27b9934d3e08a52e52d7da7dabfade4f3e9e64c94f4db5d4ef7d6df4f6f6";
        assert!(equals_hash(value, value));
        assert!(equals_hash(&format!("sha256:{}", value), value));
        assert!(equals_hash(&format!("blake3:{}", value), value));
    }

    #[test]
    fn test_normalize_hash_for_compare() {
        let value = "ABCDEF";
        assert_eq!(normalize_hash_for_compare(value), "abcdef");
        assert_eq!(normalize_hash_for_compare("sha256:ABCDEF"), "abcdef");
        assert_eq!(normalize_hash_for_compare("blake3:ABCDEF"), "abcdef");
    }

    #[test]
    fn test_permissions_deserialization_with_aliases() {
        let payload = r#"{
            "network": {
                "egress_allow": ["api.example.com"],
                "connect_allowlist": ["wss://ws.example.com"]
            },
            "isolation": {
                "allow_env": ["OPENAI_API_KEY"]
            },
            "filesystem": {
                "read": ["/opt/data"],
                "write": ["/tmp"]
            }
        }"#;

        let permissions: CapsulePermissions = serde_json::from_str(payload).unwrap();
        let network = permissions.network.unwrap();
        assert_eq!(network.merged_endpoints().len(), 2);
        assert_eq!(
            permissions.isolation.unwrap().allow_env,
            vec!["OPENAI_API_KEY".to_string()]
        );
        let filesystem = permissions.filesystem.unwrap();
        assert_eq!(filesystem.read_only, vec!["/opt/data".to_string()]);
        assert_eq!(filesystem.read_write, vec!["/tmp".to_string()]);
    }

    #[test]
    fn test_permissions_deserialization_missing_fields() {
        let payload = r#"{}"#;
        let permissions: CapsulePermissions = serde_json::from_str(payload).unwrap();
        assert!(permissions.network.is_none());
        assert!(permissions.isolation.is_none());
        assert!(permissions.filesystem.is_none());
    }

    #[test]
    fn test_parse_capsule_ref_accepts_scoped_and_at_scoped() {
        let plain = parse_capsule_ref("koh0920/sample-capsule").unwrap();
        assert_eq!(plain.publisher, "koh0920");
        assert_eq!(plain.slug, "sample-capsule");
        assert_eq!(plain.scoped_id, "koh0920/sample-capsule");

        let with_at = parse_capsule_ref("@koh0920/sample-capsule").unwrap();
        assert_eq!(with_at.scoped_id, "koh0920/sample-capsule");
    }

    #[test]
    fn test_parse_capsule_ref_rejects_slug_only() {
        assert!(parse_capsule_ref("sample-capsule").is_err());
        assert!(is_slug_only_ref("sample-capsule"));
    }

    #[test]
    fn test_parse_capsule_request_extracts_version_suffix() {
        let parsed = parse_capsule_request("koh0920/sample-capsule@1.2.3").unwrap();
        assert_eq!(parsed.scoped_ref.scoped_id, "koh0920/sample-capsule");
        assert_eq!(parsed.version.as_deref(), Some("1.2.3"));
    }

    #[test]
    fn test_normalize_github_repository_accepts_url_host_path_and_owner_repo() {
        assert_eq!(
            normalize_github_repository("https://github.com/Koh0920/ato-cli.git").unwrap(),
            "Koh0920/ato-cli"
        );
        assert_eq!(
            normalize_github_repository("github.com/Koh0920/ato-cli.git").unwrap(),
            "Koh0920/ato-cli"
        );
        assert_eq!(
            normalize_github_repository("www.github.com/Koh0920/ato-cli").unwrap(),
            "Koh0920/ato-cli"
        );
        assert_eq!(
            normalize_github_repository("Koh0920/ato-cli").unwrap(),
            "Koh0920/ato-cli"
        );
    }

    #[test]
    fn test_normalize_install_segment_slugifies_github_owner() {
        assert_eq!(normalize_install_segment("Koh_0920").unwrap(), "koh-0920");
        assert!(normalize_install_segment("___").is_err());
    }

    #[test]
    fn test_github_api_base_url_uses_env_override() {
        let key = "ATO_GITHUB_API_BASE_URL";
        let previous = std::env::var(key).ok();
        std::env::set_var(key, "http://127.0.0.1:3000/");
        assert_eq!(github_api_base_url(), "http://127.0.0.1:3000");
        match previous {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }

    #[test]
    fn test_normalize_github_checkout_dir_renames_to_repo_name() {
        let temp = tempfile::tempdir().expect("tempdir");
        let extracted = temp.path().join("Koh0920-demo-abc123");
        std::fs::create_dir_all(&extracted).expect("create extracted");
        std::fs::write(extracted.join("index.js"), "console.log('hi')").expect("write fixture");
        let normalized =
            normalize_github_checkout_dir(extracted.clone(), "demo").expect("normalize checkout");
        assert_eq!(normalized, temp.path().join("demo"));
        assert!(normalized.join("index.js").exists());
        assert!(!extracted.exists());
    }

    #[test]
    fn test_unpack_github_tarball_rejects_empty_archive() {
        let temp = tempfile::tempdir().expect("tempdir");
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let bytes = encoder.finish().expect("finish gzip");
        let err = unpack_github_tarball(&bytes, temp.path()).expect_err("empty archive must fail");
        assert!(err.to_string().contains("GitHub archive is empty"));
    }

    #[test]
    fn test_unpack_github_tarball_rejects_multiple_top_level_directories() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut archive_bytes = Vec::new();
        {
            let encoder =
                flate2::write::GzEncoder::new(&mut archive_bytes, flate2::Compression::default());
            let mut builder = tar::Builder::new(encoder);

            let mut header = tar::Header::new_gnu();
            header.set_size(1);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "repo-a/index.js", std::io::Cursor::new(b"a"))
                .expect("append repo-a");

            let mut header = tar::Header::new_gnu();
            header.set_size(1);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "repo-b/index.js", std::io::Cursor::new(b"b"))
                .expect("append repo-b");

            builder
                .into_inner()
                .expect("finish tar")
                .finish()
                .expect("finish gzip");
        }

        let err =
            unpack_github_tarball(&archive_bytes, temp.path()).expect_err("must reject archive");
        assert!(err.to_string().contains("multiple top-level directories"));
    }

    #[test]
    fn test_unpack_github_tarball_ignores_global_pax_headers() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut archive_bytes = Vec::new();
        {
            let encoder =
                flate2::write::GzEncoder::new(&mut archive_bytes, flate2::Compression::default());
            let mut builder = tar::Builder::new(encoder);

            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::XGlobalHeader);
            header.set_size(0);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "pax_global_header", std::io::Cursor::new([]))
                .expect("append pax global header");

            let mut header = tar::Header::new_gnu();
            header.set_size(1);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "repo/index.js", std::io::Cursor::new(b"a"))
                .expect("append repo file");

            builder
                .into_inner()
                .expect("finish tar")
                .finish()
                .expect("finish gzip");
        }

        let root = unpack_github_tarball(&archive_bytes, temp.path()).expect("must unpack archive");
        assert_eq!(root, temp.path().join("repo"));
        assert_eq!(
            std::fs::read_to_string(root.join("index.js")).expect("read unpacked file"),
            "a"
        );
    }

    #[test]
    fn test_unpack_github_tarball_rejects_path_traversal_entries() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut tar_bytes = Vec::new();
        {
            let path = b"repo/../evil.txt";
            let content = b"x";
            let mut header = [0u8; 512];
            header[..path.len()].copy_from_slice(path);
            header[100..108].copy_from_slice(b"0000644\0");
            header[108..116].copy_from_slice(b"0000000\0");
            header[116..124].copy_from_slice(b"0000000\0");
            header[124..136].copy_from_slice(b"00000000001\0");
            header[136..148].copy_from_slice(b"00000000000\0");
            header[148..156].fill(b' ');
            header[156] = b'0';
            header[257..263].copy_from_slice(b"ustar\0");
            header[263..265].copy_from_slice(b"00");
            let checksum: u32 = header.iter().map(|byte| *byte as u32).sum();
            let checksum_octal = format!("{checksum:06o}\0 ");
            header[148..156].copy_from_slice(checksum_octal.as_bytes());

            tar_bytes.extend_from_slice(&header);
            tar_bytes.extend_from_slice(content);
            tar_bytes.extend_from_slice(&[0u8; 511][..511 - content.len() + 1]);
            tar_bytes.extend_from_slice(&[0u8; 1024]);
        }
        let mut archive_bytes = Vec::new();
        {
            let mut encoder =
                flate2::write::GzEncoder::new(&mut archive_bytes, flate2::Compression::default());
            use std::io::Write as _;
            encoder.write_all(&tar_bytes).expect("write tar");
            encoder.finish().expect("finish gzip");
        }

        let err =
            unpack_github_tarball(&archive_bytes, temp.path()).expect_err("must reject traversal");
        assert!(err.to_string().contains("unsafe path traversal components"));
    }

    #[test]
    fn test_merge_requested_version_rejects_conflicts() {
        let err = merge_requested_version(Some("1.0.0"), Some("2.0.0")).expect_err("must fail");
        assert!(err.to_string().contains("conflicting_version_request"));
    }

    #[test]
    fn test_epoch_guard_rejects_downgrade_without_flag() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state_path = temp.path().join("epoch-guard.json");
        enforce_epoch_monotonicity_at(&state_path, "koh0920/app", 10, "blake3:aaaa", false)
            .expect("seed epoch");
        let err =
            enforce_epoch_monotonicity_at(&state_path, "koh0920/app", 9, "blake3:bbbb", false)
                .expect_err("downgrade must fail");
        assert!(err.to_string().contains("Downgrade detected"));
    }

    #[test]
    fn test_epoch_guard_allows_downgrade_with_flag() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state_path = temp.path().join("epoch-guard.json");
        enforce_epoch_monotonicity_at(&state_path, "koh0920/app", 10, "blake3:aaaa", false)
            .expect("seed epoch");
        enforce_epoch_monotonicity_at(&state_path, "koh0920/app", 9, "blake3:bbbb", true)
            .expect("downgrade should be allowed with explicit flag");
        let state = load_epoch_guard_state(&state_path).expect("state readable");
        let entry = state.capsules.get("koh0920/app").expect("entry exists");
        assert_eq!(entry.max_epoch, 10);
    }

    #[test]
    fn test_epoch_guard_rejects_same_epoch_conflict() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state_path = temp.path().join("epoch-guard.json");
        enforce_epoch_monotonicity_at(&state_path, "koh0920/app", 7, "blake3:aaaa", false)
            .expect("seed epoch");
        let err = enforce_epoch_monotonicity_at(&state_path, "koh0920/app", 7, "blake3:bbbb", true)
            .expect_err("same epoch conflict must fail");
        assert!(err.to_string().contains("Epoch replay mismatch"));
    }

    #[test]
    fn test_compute_manifest_hash_without_signatures_is_stable() {
        let chunk_hash = format!("blake3:{}", blake3::hash(b"payload").to_hex());
        let mut manifest = CapsuleManifest::from_toml(
            r#"
schema_version = "1"
name = "sample"
version = "1.0.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
entrypoint = "main.py"
"#,
        )
        .expect("manifest");
        manifest.distribution = Some(capsule_core::types::DistributionInfo {
            manifest_hash: String::new(),
            merkle_root: chunk_hash.clone(),
            chunk_list: vec![capsule_core::types::ChunkDescriptor {
                chunk_hash: chunk_hash.clone(),
                offset: 0,
                length: 7,
                codec: "fastcdc".to_string(),
                compression: "none".to_string(),
            }],
            signatures: vec![],
        });
        let hash = compute_manifest_hash_without_signatures(&manifest).expect("hash");
        manifest
            .distribution
            .as_mut()
            .expect("distribution")
            .manifest_hash = hash.clone();
        manifest
            .distribution
            .as_mut()
            .expect("distribution")
            .signatures
            .push(capsule_core::types::SignatureEntry {
                signer_did: "did:key:zabc".to_string(),
                key_id: "k1".to_string(),
                algorithm: "ed25519".to_string(),
                signature: "AAAA".to_string(),
                signed_at: None,
            });
        let hash_with_signature =
            compute_manifest_hash_without_signatures(&manifest).expect("hash");
        assert_eq!(hash, hash_with_signature);
    }

    #[test]
    fn test_verify_payload_chunks_and_merkle_root() {
        let payload = b"payload".to_vec();
        let chunk_hash = format!("blake3:{}", blake3::hash(&payload).to_hex());
        let mut manifest = CapsuleManifest::from_toml(
            r#"
schema_version = "1"
name = "sample"
version = "1.0.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
entrypoint = "main.py"
"#,
        )
        .expect("manifest");
        manifest.distribution = Some(capsule_core::types::DistributionInfo {
            manifest_hash: "blake3:dummy".to_string(),
            merkle_root: chunk_hash.clone(),
            chunk_list: vec![capsule_core::types::ChunkDescriptor {
                chunk_hash,
                offset: 0,
                length: payload.len() as u64,
                codec: "fastcdc".to_string(),
                compression: "none".to_string(),
            }],
            signatures: vec![],
        });
        verify_payload_chunks(&manifest, &payload).expect("chunks");
        verify_manifest_merkle_root(&manifest).expect("merkle");
    }

    #[test]
    fn test_build_capsule_artifact_contains_manifest_and_payload() {
        let manifest = "schema_version = \"1\"\nname = \"sample\"\nversion = \"1.0.0\"\ntype = \"app\"\ndefault_target = \"cli\"\n";
        let payload = b"compressed-payload";
        let artifact = build_capsule_artifact(Some(manifest), None, payload).expect("artifact");
        let mut archive = tar::Archive::new(Cursor::new(artifact));
        let mut has_manifest = false;
        let mut has_payload = false;
        for entry in archive.entries().expect("entries") {
            let mut entry = entry.expect("entry");
            let path = entry.path().expect("path").to_string_lossy().to_string();
            if path == "capsule.toml" {
                has_manifest = true;
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).expect("read manifest");
                assert_eq!(bytes, manifest.as_bytes());
            } else if path == "payload.tar.zst" {
                has_payload = true;
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).expect("read payload");
                assert_eq!(bytes, payload);
            }
        }
        assert!(has_manifest);
        assert!(has_payload);
    }

    #[test]
    fn test_build_capsule_artifact_includes_capsule_toml_when_provided() {
        let payload = b"compressed-payload";
        let capsule_toml = "schema_version = \"0.2\"\nname = \"sample\"\nversion = \"1.0.0\"\ntype = \"app\"\ndefault_target = \"cli\"\n";
        let artifact = build_capsule_artifact(Some(capsule_toml), None, payload).expect("artifact");
        let mut archive = tar::Archive::new(Cursor::new(artifact));
        let mut has_capsule_toml = false;
        for entry in archive.entries().expect("entries") {
            let mut entry = entry.expect("entry");
            let path = entry.path().expect("path").to_string_lossy().to_string();
            if path == "capsule.toml" {
                has_capsule_toml = true;
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).expect("read capsule.toml");
                assert_eq!(bytes, capsule_toml.as_bytes());
            }
        }
        assert!(has_capsule_toml);
    }

    #[test]
    fn test_build_capsule_artifact_includes_capsule_lock_when_provided() {
        let payload = b"compressed-payload";
        let capsule_lock = r#"{"schema_version":"0.1","lock_generated_at":"2026-03-05T00:00:00Z"}"#;
        let artifact = build_capsule_artifact(None, Some(capsule_lock), payload).expect("artifact");
        let mut archive = tar::Archive::new(Cursor::new(artifact));
        let mut has_capsule_lock = false;
        for entry in archive.entries().expect("entries") {
            let mut entry = entry.expect("entry");
            let path = entry.path().expect("path").to_string_lossy().to_string();
            if path == "capsule.lock" {
                has_capsule_lock = true;
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).expect("read capsule.lock");
                assert_eq!(bytes, capsule_lock.as_bytes());
            }
        }
        assert!(has_capsule_lock);
    }

    #[test]
    fn test_reconstruct_payload_reports_missing_chunks() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cas = LocalCasIndex::open(temp.path()).expect("open cas");
        let first = b"chunk-a";
        let second = b"chunk-b";
        let first_hash = format!("blake3:{}", blake3::hash(first).to_hex());
        let second_hash = format!("blake3:{}", blake3::hash(second).to_hex());
        cas.put_verified_chunk(&first_hash, first)
            .expect("put first");

        let mut manifest = CapsuleManifest::from_toml(
            r#"
schema_version = "1"
name = "sample"
version = "1.0.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
entrypoint = "main.py"
"#,
        )
        .expect("manifest");
        manifest.distribution = Some(capsule_core::types::DistributionInfo {
            manifest_hash: "blake3:dummy".to_string(),
            merkle_root: "blake3:dummy".to_string(),
            chunk_list: vec![
                capsule_core::types::ChunkDescriptor {
                    chunk_hash: first_hash.clone(),
                    offset: 0,
                    length: first.len() as u64,
                    codec: "fastcdc".to_string(),
                    compression: "none".to_string(),
                },
                capsule_core::types::ChunkDescriptor {
                    chunk_hash: second_hash.clone(),
                    offset: first.len() as u64,
                    length: second.len() as u64,
                    codec: "fastcdc".to_string(),
                    compression: "none".to_string(),
                },
            ],
            signatures: vec![],
        });

        let reconstructed =
            reconstruct_payload_from_local_chunks(&cas, &manifest).expect("reconstruct");
        assert_eq!(reconstructed.missing_chunks, vec![second_hash]);
        assert_eq!(reconstructed.payload_tar, first);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_delta_install_false_positive_recovers_with_reuse_lease_id() {
        let _env_lock = acquire_test_env_lock().await;
        let cas_root = tempfile::tempdir().expect("cas root");
        let _cas_guard = EnvVarGuard::set(
            "ATO_CAS_ROOT",
            Some(cas_root.path().to_string_lossy().as_ref()),
        );
        let _token_guard = EnvVarGuard::set("ATO_TOKEN", None);

        let fixture = build_mock_fixture(
            TEST_SCOPED_ID,
            TEST_VERSION,
            vec![b"chunk-alpha".to_vec(), b"chunk-beta".to_vec()],
        );
        let server =
            spawn_mock_registry(MockScenario::FalsePositiveRecovery, fixture.clone()).await;
        let client = reqwest::Client::new();
        let scoped_ref = test_scoped_ref();
        let result =
            install_manifest_delta_path(&client, server.base_url(), &scoped_ref, None, None, None)
                .await
                .expect("delta install should succeed after retry");
        let DeltaInstallResult::Artifact(artifact) = result;
        let reconstructed_payload =
            extract_payload_tar_from_capsule(&artifact).expect("extract reconstructed payload");
        assert_eq!(reconstructed_payload, fixture.payload_tar);

        let observations = server.observations().await;
        assert_eq!(observations.negotiate_calls.len(), 2);
        assert!(observations.negotiate_calls[0].has_bloom);
        assert!(!observations.negotiate_calls[1].has_bloom);
        assert_eq!(observations.negotiate_calls[1].have_chunks_len, 1);
        assert_eq!(
            observations.negotiate_calls[1].reuse_lease_id.as_deref(),
            Some(TEST_LEASE_ID)
        );
        assert_eq!(observations.release_calls, vec![TEST_LEASE_ID.to_string()]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_install_app_uses_version_resolve_for_explicit_time_travel() {
        let _env_lock = acquire_test_env_lock().await;
        let cas_root = tempfile::tempdir().expect("cas root");
        let output_root = tempfile::tempdir().expect("output root");
        let runtime_root = tempfile::tempdir().expect("runtime root");
        let _cas_guard = EnvVarGuard::set(
            "ATO_CAS_ROOT",
            Some(cas_root.path().to_string_lossy().as_ref()),
        );
        let _runtime_guard = EnvVarGuard::set(
            "ATO_RUNTIME_ROOT",
            Some(runtime_root.path().to_string_lossy().as_ref()),
        );
        let _token_guard = EnvVarGuard::set("ATO_TOKEN", None);

        let payload_tar = build_payload_tar_with_source("main.py", b"print('time travel')\n");
        let fixture = build_mock_fixture(TEST_SCOPED_ID, TEST_VERSION, vec![payload_tar]);
        let server = spawn_mock_registry(MockScenario::FalsePositiveRecovery, fixture).await;
        let result = install_app(
            "koh0920/sample@1.0.0",
            Some(server.base_url()),
            None,
            Some(output_root.path().to_path_buf()),
            false,
            false,
            ProjectionPreference::Skip,
            false,
            false,
            true,
            false,
        )
        .await
        .expect("explicit version install should succeed");
        assert_eq!(result.version, TEST_VERSION);

        let observations = server.observations().await;
        assert_eq!(observations.version_resolve_calls, 2);
        assert_eq!(observations.epoch_calls, 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_install_app_fails_closed_on_negotiate_501() {
        let _env_lock = acquire_test_env_lock().await;
        let cas_root = tempfile::tempdir().expect("cas root");
        let output_root = tempfile::tempdir().expect("output root");
        let runtime_root = tempfile::tempdir().expect("runtime root");
        let _cas_guard = EnvVarGuard::set(
            "ATO_CAS_ROOT",
            Some(cas_root.path().to_string_lossy().as_ref()),
        );
        let _runtime_guard = EnvVarGuard::set(
            "ATO_RUNTIME_ROOT",
            Some(runtime_root.path().to_string_lossy().as_ref()),
        );
        let _token_guard = EnvVarGuard::set("ATO_TOKEN", None);

        let fixture = build_mock_fixture(TEST_SCOPED_ID, TEST_VERSION, vec![b"payload".to_vec()]);
        let server = spawn_mock_registry(MockScenario::FallbackNotImplemented, fixture).await;
        let err = install_app(
            TEST_SCOPED_ID,
            Some(server.base_url()),
            Some(TEST_VERSION),
            Some(output_root.path().to_path_buf()),
            false,
            false,
            ProjectionPreference::Skip,
            true,
            false,
            true,
            false,
        )
        .await
        .expect_err("install should fail closed when negotiate is unavailable");
        assert!(err
            .to_string()
            .contains("Registry does not support the manifest negotiate API"));

        let observations = server.observations().await;
        assert_eq!(observations.negotiate_calls.len(), 1);
        assert_eq!(observations.distribution_calls, 0);
        assert_eq!(observations.artifact_calls, 0);
        assert!(observations.release_calls.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_install_app_unauthorized_manifest_fails_closed_without_fallback() {
        let _env_lock = acquire_test_env_lock().await;
        let cas_root = tempfile::tempdir().expect("cas root");
        let output_root = tempfile::tempdir().expect("output root");
        let runtime_root = tempfile::tempdir().expect("runtime root");
        let _cas_guard = EnvVarGuard::set(
            "ATO_CAS_ROOT",
            Some(cas_root.path().to_string_lossy().as_ref()),
        );
        let _runtime_guard = EnvVarGuard::set(
            "ATO_RUNTIME_ROOT",
            Some(runtime_root.path().to_string_lossy().as_ref()),
        );
        let _token_guard = EnvVarGuard::set("ATO_TOKEN", None);

        let fixture = build_mock_fixture(TEST_SCOPED_ID, TEST_VERSION, vec![b"payload".to_vec()]);
        let server = spawn_mock_registry(MockScenario::UnauthorizedManifest, fixture).await;
        let err = install_app(
            TEST_SCOPED_ID,
            Some(server.base_url()),
            Some(TEST_VERSION),
            Some(output_root.path().to_path_buf()),
            false,
            false,
            ProjectionPreference::Skip,
            false,
            false,
            true,
            false,
        )
        .await
        .expect_err("install should fail closed on unauthorized manifest read");
        assert!(err
            .to_string()
            .contains(crate::error_codes::ATO_ERR_AUTH_REQUIRED));

        let observations = server.observations().await;
        assert_eq!(observations.distribution_calls, 0);
        assert_eq!(observations.artifact_calls, 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_delta_install_releases_lease_when_chunk_download_fails() {
        let _env_lock = acquire_test_env_lock().await;
        let cas_root = tempfile::tempdir().expect("cas root");
        let _cas_guard = EnvVarGuard::set(
            "ATO_CAS_ROOT",
            Some(cas_root.path().to_string_lossy().as_ref()),
        );
        let _token_guard = EnvVarGuard::set("ATO_TOKEN", None);

        let fixture = build_mock_fixture(TEST_SCOPED_ID, TEST_VERSION, vec![b"chunk".to_vec()]);
        let server = spawn_mock_registry(MockScenario::LeaseReleaseOnFailure, fixture).await;
        let client = reqwest::Client::new();
        let scoped_ref = test_scoped_ref();
        let err =
            install_manifest_delta_path(&client, server.base_url(), &scoped_ref, None, None, None)
                .await
                .expect_err("chunk failure should abort delta install");
        assert!(err
            .to_string()
            .contains(crate::error_codes::ATO_ERR_INTEGRITY_FAILURE));

        let observations = server.observations().await;
        assert_eq!(observations.release_calls, vec![TEST_LEASE_ID.to_string()]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_negotiate_yanked_fails_closed() {
        let _env_lock = acquire_test_env_lock().await;
        let cas_root = tempfile::tempdir().expect("cas root");
        let _cas_guard = EnvVarGuard::set(
            "ATO_CAS_ROOT",
            Some(cas_root.path().to_string_lossy().as_ref()),
        );
        let _token_guard = EnvVarGuard::set("ATO_TOKEN", None);

        let fixture = build_mock_fixture(TEST_SCOPED_ID, TEST_VERSION, vec![b"chunk".to_vec()]);
        let server = spawn_mock_registry(MockScenario::YankedNegotiate, fixture).await;
        let client = reqwest::Client::new();
        let scoped_ref = test_scoped_ref();
        let err =
            install_manifest_delta_path(&client, server.base_url(), &scoped_ref, None, None, None)
                .await
                .expect_err("yanked negotiate must fail closed");
        let message = err.to_string();
        assert!(message.contains(crate::error_codes::ATO_ERR_INTEGRITY_FAILURE));
        assert!(message.to_ascii_lowercase().contains("yanked"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_manifest_yanked_fails_closed_even_with_allow_unverified() {
        let _env_lock = acquire_test_env_lock().await;
        let cas_root = tempfile::tempdir().expect("cas root");
        let output_root = tempfile::tempdir().expect("output root");
        let runtime_root = tempfile::tempdir().expect("runtime root");
        let _cas_guard = EnvVarGuard::set(
            "ATO_CAS_ROOT",
            Some(cas_root.path().to_string_lossy().as_ref()),
        );
        let _runtime_guard = EnvVarGuard::set(
            "ATO_RUNTIME_ROOT",
            Some(runtime_root.path().to_string_lossy().as_ref()),
        );
        let _token_guard = EnvVarGuard::set("ATO_TOKEN", None);

        let fixture = build_mock_fixture(TEST_SCOPED_ID, TEST_VERSION, vec![b"chunk".to_vec()]);
        let server = spawn_mock_registry(MockScenario::YankedManifest, fixture).await;
        let err = install_app(
            TEST_SCOPED_ID,
            Some(server.base_url()),
            Some(TEST_VERSION),
            Some(output_root.path().to_path_buf()),
            false,
            false,
            ProjectionPreference::Skip,
            true,
            false,
            true,
            false,
        )
        .await
        .expect_err("yanked manifest must fail closed");
        let message = err.to_string();
        assert!(message.contains(crate::error_codes::ATO_ERR_INTEGRITY_FAILURE));
        assert!(message.to_ascii_lowercase().contains("yanked"));
    }

    #[test]
    fn test_atomic_install_writes_via_tmp_and_rename() {
        let temp = tempfile::tempdir().expect("tempdir");
        let install_dir = temp.path().join("install");
        std::fs::create_dir_all(&install_dir).expect("mkdir");
        let stale = install_dir.join(".capsule.tmp.stale");
        std::fs::write(&stale, b"stale").expect("write stale");
        sweep_stale_tmp_capsules(&install_dir).expect("sweep stale");
        assert!(!stale.exists());

        let output_path = install_dir.join("sample.capsule");
        let payload = b"atomic-payload".to_vec();
        let expected = compute_blake3(&payload);
        write_capsule_atomic(&output_path, &payload, &expected).expect("atomic write");

        let written = std::fs::read(&output_path).expect("read output");
        assert_eq!(written, payload);
        let leftovers = std::fs::read_dir(&install_dir)
            .expect("read dir")
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".capsule.tmp.")
            })
            .count();
        assert_eq!(leftovers, 0);
    }
}
