//! Install command implementation
//!
//! Downloads and installs capsules from the Store.
//! Primary path: `/v1/capsules/by/:publisher/:slug/distributions` (.capsule contract)

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use futures::stream::{FuturesUnordered, StreamExt};
use rand::RngCore;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Once, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use tracing::debug;

use capsule_core::packers::payload as manifest_payload;
use capsule_core::resource::cas::LocalCasIndex;
use capsule_core::types::identity::public_key_to_did;
use capsule_core::types::CapsuleManifest;

use crate::artifact_hash::{
    compute_blake3_label as compute_blake3, compute_sha256_hex as compute_sha256, equals_hash,
    normalize_hash_for_compare,
};
use crate::capsule_archive::extract_payload_tar_from_capsule;
use crate::runtime::tree as runtime_tree;

mod github_archive;
mod github_inference;
mod manifest_delta;
mod manifest_integrity;
mod persistence;
pub(crate) mod support;

use github_archive::*;
use github_inference::*;
use manifest_delta::*;
use manifest_integrity::*;
use persistence::*;

const DEFAULT_STORE_DIR: &str = ".ato/store";
const DEFAULT_STORE_API_URL: &str = "https://api.ato.run";
const ENV_STORE_API_URL: &str = "ATO_STORE_API_URL";
const SEGMENT_MAX_LEN: usize = 63;
const LEASE_REFRESH_INTERVAL_SECS: u64 = 300;
const NEGOTIATE_DEFAULT_MAX_BYTES: u64 = 16 * 1024 * 1024;
const DELTA_RECONSTRUCT_ZSTD_LEVEL: i32 = 3;
const DEFAULT_GITHUB_DRAFT_NODE_RUNTIME_VERSION: &str = "20.12.0";
const DEFAULT_GITHUB_DRAFT_PYTHON_RUNTIME_VERSION: &str = "3.11.10";

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
    pub promotion: Option<PromotionInfo>,
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

#[derive(Debug, Clone, Serialize)]
pub struct PromotionInfo {
    pub performed: bool,
    pub preview_id: Option<String>,
    pub source_reference: Option<String>,
    pub source_metadata_path: Option<PathBuf>,
    pub source_manifest_path: Option<PathBuf>,
    pub manifest_source: Option<String>,
    pub inference_mode: Option<String>,
    pub resolved_ref: Option<GitHubInstallDraftResolvedRef>,
    pub derived_plan: Option<PromotionDerivedPlanSnapshot>,
    pub promotion_metadata_path: Option<PathBuf>,
    pub content_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PromotionDerivedPlanSnapshot {
    pub runtime: Option<String>,
    pub driver: Option<String>,
    pub resolved_runtime_version: Option<String>,
    pub resolved_port: Option<u16>,
    pub resolved_lock_files: Vec<PathBuf>,
    pub resolved_pack_include: Vec<String>,
    pub warnings: Vec<String>,
    pub deferred_constraints: Vec<String>,
    pub promotion_eligibility: String,
}

#[derive(Debug, Clone)]
pub struct PromotionSourceInfo {
    pub preview_id: String,
    pub source_reference: String,
    pub source_metadata_path: PathBuf,
    pub source_manifest_path: PathBuf,
    pub manifest_source: Option<String>,
    pub inference_mode: Option<String>,
    pub resolved_ref: Option<GitHubInstallDraftResolvedRef>,
    pub derived_plan: PromotionDerivedPlanSnapshot,
}

#[derive(Debug)]
pub struct GitHubCheckout {
    pub repository: String,
    pub publisher: String,
    pub checkout_dir: PathBuf,
    temp_dir: Option<tempfile::TempDir>,
}

impl GitHubCheckout {
    pub fn preserve_for_debugging(&mut self) -> PathBuf {
        if let Some(temp_dir) = self.temp_dir.take() {
            std::mem::forget(temp_dir);
        }
        self.checkout_dir.clone()
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct GitHubInstallDraftResponse {
    pub repo: GitHubInstallDraftRepo,
    #[serde(rename = "capsuleToml")]
    pub capsule_toml: GitHubInstallDraftCapsuleToml,
    #[serde(rename = "repoRef")]
    pub repo_ref: String,
    #[serde(rename = "proposedRunCommand")]
    pub proposed_run_command: Option<String>,
    #[serde(rename = "proposedInstallCommand")]
    pub proposed_install_command: String,
    #[serde(rename = "resolvedRef")]
    pub resolved_ref: GitHubInstallDraftResolvedRef,
    #[serde(rename = "manifestSource")]
    pub manifest_source: String,
    #[serde(rename = "previewToml")]
    pub preview_toml: Option<String>,
    #[serde(rename = "capsuleHint")]
    pub capsule_hint: Option<GitHubInstallDraftHint>,
    #[serde(rename = "inferenceMode")]
    pub inference_mode: Option<String>,
    #[serde(default)]
    pub retryable: bool,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct GitHubInstallDraftRepo {
    pub owner: String,
    pub repo: String,
    #[serde(rename = "fullName")]
    pub full_name: String,
    #[serde(rename = "defaultBranch")]
    pub default_branch: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct GitHubInstallDraftCapsuleToml {
    pub exists: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitHubInstallDraftResolvedRef {
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub sha: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitHubInstallDraftHint {
    pub confidence: String,
    pub warnings: Vec<String>,
    #[serde(default)]
    pub launchability: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct GitHubStoreErrorPayload {
    error: String,
    message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubInstallDraftRetryRequest {
    pub attempt_id: Option<String>,
    pub resolved_ref_sha: String,
    pub previous_toml: String,
    pub smoke_error_class: String,
    pub smoke_error_excerpt: String,
    pub retry_ordinal: u8,
}

impl GitHubInstallDraftResponse {
    pub fn normalize_preview_toml_for_checkout(&self, checkout_dir: &Path) -> Result<Self> {
        let mut normalized = self.clone();
        normalized.preview_toml = self
            .preview_toml
            .as_deref()
            .map(|raw| normalize_github_install_preview_toml(checkout_dir, raw))
            .transpose()?;
        Ok(normalized)
    }
}

pub struct InstallExecutionOptions {
    pub output_dir: Option<PathBuf>,
    pub yes: bool,
    pub projection_preference: ProjectionPreference,
    pub json_output: bool,
    pub can_prompt_interactively: bool,
    pub promotion_source: Option<PromotionSourceInfo>,
    pub keep_progressive_flow_open: bool,
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
pub(crate) struct CapsuleDetail {
    id: String,
    #[serde(default, alias = "scopedId", alias = "scoped_id")]
    scoped_id: Option<String>,
    slug: String,
    name: String,
    description: String,
    price: u64,
    currency: String,
    #[serde(rename = "latestVersion", alias = "latest_version", default)]
    pub(crate) latest_version: Option<String>,
    pub(crate) releases: Vec<ReleaseInfo>,
    #[serde(default)]
    manifest_toml: Option<String>,
    #[serde(default)]
    capsule_lock: Option<String>,
    #[serde(default)]
    permissions: Option<CapsulePermissions>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ReleaseInfo {
    pub(crate) version: String,
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
    DownloadedArtifact { bytes: Vec<u8>, file_name: String },
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

pub fn parse_github_run_ref(input: &str) -> Result<Option<String>> {
    let raw = input.trim();
    if raw.is_empty() {
        return Ok(None);
    }

    if raw.starts_with("github.com/") {
        return normalize_github_repository(raw).map(Some);
    }

    let is_noncanonical_github_ref = raw.starts_with("www.github.com/")
        || raw.starts_with("http://github.com/")
        || raw.starts_with("https://github.com/")
        || raw.starts_with("http://www.github.com/")
        || raw.starts_with("https://www.github.com/");

    if !is_noncanonical_github_ref {
        return Ok(None);
    }

    let normalized = normalize_github_repository(raw).with_context(|| {
        "GitHub repository inputs for `ato run` must use `github.com/owner/repo`"
    })?;
    bail!(
        "GitHub repository inputs for `ato run` must use `github.com/owner/repo`. Re-run with: ato run github.com/{}",
        normalized
    );
}

pub async fn fetch_github_install_draft(repository: &str) -> Result<GitHubInstallDraftResponse> {
    let normalized = normalize_github_repository(repository)?;
    let (owner, repo) = normalized
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("repository must include owner/repo"))?;
    let client = reqwest::Client::new();
    let endpoint = format!(
        "{}/v1/github/repos/{}/{}/install-draft",
        resolve_store_api_base_url(),
        urlencoding::encode(owner),
        urlencoding::encode(repo)
    );
    let response = client
        .get(&endpoint)
        .header(reqwest::header::USER_AGENT, "ato-cli")
        .send()
        .await
        .with_context(|| format!("Failed to fetch GitHub install draft: {normalized}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!(
            "Failed to fetch GitHub install draft (status={}): {}",
            status,
            body
        );
    }

    response
        .json::<GitHubInstallDraftResponse>()
        .await
        .with_context(|| format!("Failed to parse GitHub install draft: {normalized}"))
}

pub async fn retry_github_install_draft(
    repository: &str,
    request: &GitHubInstallDraftRetryRequest,
) -> Result<GitHubInstallDraftResponse> {
    let normalized = normalize_github_repository(repository)?;
    let (owner, repo) = normalized
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("repository must include owner/repo"))?;
    let client = reqwest::Client::new();
    let endpoint = format!(
        "{}/v1/github/repos/{}/{}/install-draft/retry",
        resolve_store_api_base_url(),
        urlencoding::encode(owner),
        urlencoding::encode(repo)
    );
    let response = client
        .post(&endpoint)
        .header(reqwest::header::USER_AGENT, "ato-cli")
        .json(request)
        .send()
        .await
        .with_context(|| format!("Failed to retry GitHub install draft: {normalized}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!(
            "Failed to retry GitHub install draft (status={}): {}",
            status,
            body
        );
    }

    response
        .json::<GitHubInstallDraftResponse>()
        .await
        .with_context(|| format!("Failed to parse retried GitHub install draft: {normalized}"))
}

pub async fn download_github_repository_at_ref(
    repository: &str,
    resolved_ref: Option<&str>,
) -> Result<GitHubCheckout> {
    let normalized = normalize_github_repository(repository)?;
    let (owner, repo) = normalized
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("repository must include owner/repo"))?;
    let publisher = normalize_install_segment(owner)?;
    let client = reqwest::Client::new();
    let archive_url = match resolved_ref.filter(|value| !value.trim().is_empty()) {
        Some(reference) => format!(
            "{}/repos/{owner}/{repo}/tarball/{}",
            github_api_base_url(),
            urlencoding::encode(reference)
        ),
        None => format!("{}/repos/{owner}/{repo}/tarball", github_api_base_url()),
    };
    let response = client
        .get(&archive_url)
        .header(reqwest::header::USER_AGENT, "ato-cli")
        .send()
        .await
        .with_context(|| format!("Failed to fetch GitHub repository archive: {normalized}"))?;
    let session_token = crate::auth::current_session_token();
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        if let Some(token) = session_token.as_deref() {
            let archive_bytes = download_private_github_repository_archive_via_store(
                &client,
                &normalized,
                resolved_ref,
                token,
            )
            .await?;
            let temp_root = github_checkout_root()?;
            let temp_dir = tempfile::Builder::new()
                .prefix("gh-install-")
                .tempdir_in(temp_root)
                .with_context(|| "Failed to create GitHub checkout directory")?;
            let checkout_dir = normalize_github_checkout_dir(
                unpack_github_tarball(&archive_bytes, temp_dir.path())?,
                repo,
            )?;
            return Ok(GitHubCheckout {
                repository: normalized,
                publisher,
                checkout_dir,
                temp_dir: Some(temp_dir),
            });
        }

        bail!(
            "GitHub repository archive returned 404 Not Found for '{}'. If this is a private repository, run `ato login` and ensure the ato GitHub App is installed on the repository owner account.",
            normalized
        );
    }
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
        temp_dir: Some(temp_dir),
    })
}

async fn download_private_github_repository_archive_via_store(
    client: &reqwest::Client,
    normalized_repository: &str,
    resolved_ref: Option<&str>,
    session_token: &str,
) -> Result<Vec<u8>> {
    let (owner, repo) = normalized_repository
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("repository must include owner/repo"))?;
    let endpoint = format!(
        "{}/v1/github/repos/{}/{}/authed/archive",
        resolve_store_api_base_url(),
        urlencoding::encode(owner),
        urlencoding::encode(repo)
    );
    let response = client
        .get(&endpoint)
        .query(&[("ref", resolved_ref.unwrap_or_default())])
        .header(reqwest::header::USER_AGENT, "ato-cli")
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", session_token),
        )
        .send()
        .await
        .with_context(|| {
            format!(
                "Failed to fetch private GitHub repository archive via ato store: {}",
                normalized_repository
            )
        })?;

    if response.status().is_success() {
        return response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .with_context(|| {
                format!(
                    "Failed to read private GitHub repository archive via ato store: {}",
                    normalized_repository
                )
            });
    }

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let payload = serde_json::from_str::<GitHubStoreErrorPayload>(&body).ok();
    let message = match payload.as_ref().map(|value| value.error.as_str()) {
        Some("auth_required") => {
            "Private GitHub repository access requires an ato store session. Run `ato login` and retry.".to_string()
        }
        Some("publisher_required") => {
            "Private GitHub repository access requires a publisher profile. Complete publisher setup, then retry.".to_string()
        }
        Some("github_app_required") => payload
            .as_ref()
            .map(|value| {
                format!(
                    "{} Re-run after installing or reconnecting the ato GitHub App for this owner.",
                    value.message
                )
            })
            .unwrap_or_else(|| {
                "Install or reconnect the ato GitHub App for this repository owner, then retry.".to_string()
            }),
        Some("repo_not_found") => format!(
            "GitHub repository '{}' could not be found, or the connected GitHub App installation cannot access it.",
            normalized_repository
        ),
        Some("github_archive_not_found") => payload
            .as_ref()
            .map(|value| value.message.clone())
            .unwrap_or_else(|| "GitHub archive could not be fetched for the requested ref.".to_string()),
        _ => payload
            .as_ref()
            .map(|value| value.message.clone())
            .unwrap_or(body),
    };

    bail!(
        "Failed to fetch private GitHub repository archive via ato store (status={}): {}",
        status,
        message
    );
}

pub(crate) fn resolve_store_api_base_url() -> String {
    std::env::var(ENV_STORE_API_URL)
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_STORE_API_URL.to_string())
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
    let capsule = fetch_capsule_detail_record(&client, &registry, &scoped_ref).await?;

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

    let target_version_owned = select_requested_or_latest_version(
        requested_version.as_deref(),
        capsule.latest_version.as_deref(),
        &scoped_ref.scoped_id,
        "installable",
    )?;
    let target_version = target_version_owned.as_str();
    ensure_release_exists(&capsule.releases, target_version)?;
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
        DeltaInstallResult::DownloadedArtifact { bytes, file_name } => {
            if !json_output {
                eprintln!(
                    "ℹ️  Registry does not expose manifest delta APIs; falling back to direct artifact download"
                );
            }
            (bytes, file_name)
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
            promotion_source: None,
            keep_progressive_flow_open: false,
        },
        InstallSource::Registry(registry),
    )
    .await
}

pub(crate) async fn fetch_capsule_detail_record(
    client: &reqwest::Client,
    registry: &str,
    scoped_ref: &ScopedCapsuleRef,
) -> Result<CapsuleDetail> {
    let capsule_url = format!(
        "{}/v1/capsules/by/{}/{}",
        registry,
        urlencoding::encode(&scoped_ref.publisher),
        urlencoding::encode(&scoped_ref.slug)
    );
    crate::registry::http::with_ato_token(client.get(&capsule_url))
        .send()
        .await
        .with_context(|| format!("Failed to connect to registry: {}", registry))?
        .json()
        .await
        .with_context(|| format!("Capsule not found: {}", scoped_ref.scoped_id))
}

pub(crate) fn select_requested_or_latest_version(
    requested_version: Option<&str>,
    latest_version: Option<&str>,
    scoped_id: &str,
    availability_label: &str,
) -> Result<String> {
    match requested_version {
        Some(explicit) => Ok(explicit.to_string()),
        None => latest_version
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No {} version available for '{}'. This capsule has no published release version.",
                    availability_label,
                    scoped_id
                )
            }),
    }
}

pub(crate) fn ensure_release_exists(releases: &[ReleaseInfo], target_version: &str) -> Result<()> {
    releases
        .iter()
        .find(|release| release.version == target_version)
        .with_context(|| format!("Version {} not found", target_version))?;
    Ok(())
}

pub(crate) async fn download_capsule_artifact_bytes(
    client: &reqwest::Client,
    registry: &str,
    scoped_ref: &ScopedCapsuleRef,
    target_version: &str,
) -> Result<Vec<u8>> {
    let download_url = format!(
        "{}/v1/capsules/by/{}/{}/download?version={}",
        registry.trim_end_matches('/'),
        urlencoding::encode(&scoped_ref.publisher),
        urlencoding::encode(&scoped_ref.slug),
        urlencoding::encode(target_version)
    );
    crate::registry::http::with_ato_token(client.get(&download_url))
        .send()
        .await
        .with_context(|| {
            format!(
                "Failed to download artifact for {}@{}",
                scoped_ref.scoped_id, target_version
            )
        })?
        .error_for_status()
        .with_context(|| {
            format!(
                "Artifact download failed for {}@{}",
                scoped_ref.scoped_id, target_version
            )
        })?
        .bytes()
        .await
        .with_context(|| {
            format!(
                "Failed to read artifact body for {}@{}",
                scoped_ref.scoped_id, target_version
            )
        })
        .map(|bytes| bytes.to_vec())
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
        promotion_source,
        keep_progressive_flow_open,
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

    let native_spec =
        crate::build::native_delivery::detect_install_requires_local_derivation(&bytes)?;
    if let Some(_native_spec) = native_spec {
        return complete_native_install_from_bytes(
            capsule_id,
            scoped_ref,
            display_slug,
            version,
            &bytes,
            &normalized_file_name,
            output_dir,
            yes,
            projection_preference,
            json_output,
            can_prompt_interactively,
            promotion_source,
            &source,
            &computed_blake3,
        );
    }

    complete_standard_install_from_bytes(
        capsule_id,
        scoped_ref,
        display_slug,
        version,
        &bytes,
        &normalized_file_name,
        output_dir,
        json_output,
        keep_progressive_flow_open,
        promotion_source,
        &computed_blake3,
    )
}

#[allow(clippy::too_many_arguments)]
fn complete_native_install_from_bytes(
    capsule_id: String,
    scoped_ref: ScopedCapsuleRef,
    display_slug: String,
    version: String,
    bytes: &[u8],
    normalized_file_name: &str,
    output_dir: Option<PathBuf>,
    yes: bool,
    projection_preference: ProjectionPreference,
    json_output: bool,
    can_prompt_interactively: bool,
    promotion_source: Option<PromotionSourceInfo>,
    source: &InstallSource,
    computed_blake3: &str,
) -> Result<InstallResult> {
    ensure_local_finalize_allowed(yes, can_prompt_interactively, json_output)?;

    let fetch_result = crate::build::native_delivery::materialize_fetch_cache_from_artifact(
        &scoped_ref.scoped_id,
        version.as_str(),
        source.cache_label(),
        bytes,
    )?;

    if !json_output {
        eprintln!("Running local finalize...");
    }
    let finalize_result =
        crate::build::native_delivery::finalize_fetched_artifact(&fetch_result.cache_dir)?;
    let (output_path, promotion) = persist_installed_capsule_with_promotion(
        output_dir,
        &scoped_ref,
        version.as_str(),
        normalized_file_name,
        bytes,
        computed_blake3,
        promotion_source.as_ref(),
    )?;
    let projection = maybe_create_projection(
        &finalize_result.derived_app_path,
        projection_preference,
        yes,
        can_prompt_interactively,
        json_output,
    )?;

    Ok(InstallResult {
        capsule_id,
        scoped_id: scoped_ref.scoped_id.clone(),
        publisher: scoped_ref.publisher,
        slug: display_slug,
        version,
        path: output_path,
        content_hash: computed_blake3.to_string(),
        install_kind: InstallKind::NativeRequiresLocalDerivation,
        launchable: Some(LaunchableTarget::DerivedApp {
            path: finalize_result.derived_app_path.clone(),
        }),
        local_derivation: Some(LocalDerivationInfo {
            schema_version: native_delivery_schema_version(),
            performed: true,
            fetched_dir: fetch_result.cache_dir,
            derived_app_path: Some(finalize_result.derived_app_path),
            provenance_path: Some(finalize_result.provenance_path),
            parent_digest: Some(finalize_result.parent_digest),
            derived_digest: Some(finalize_result.derived_digest),
        }),
        projection: Some(projection),
        promotion,
    })
}

#[allow(clippy::too_many_arguments)]
fn complete_standard_install_from_bytes(
    capsule_id: String,
    scoped_ref: ScopedCapsuleRef,
    display_slug: String,
    version: String,
    bytes: &[u8],
    normalized_file_name: &str,
    output_dir: Option<PathBuf>,
    json_output: bool,
    keep_progressive_flow_open: bool,
    promotion_source: Option<PromotionSourceInfo>,
    computed_blake3: &str,
) -> Result<InstallResult> {
    let (output_path, promotion) = persist_installed_capsule_with_promotion(
        output_dir,
        &scoped_ref,
        version.as_str(),
        normalized_file_name,
        bytes,
        computed_blake3,
        promotion_source.as_ref(),
    )?;
    emit_standard_install_success(
        &scoped_ref.scoped_id,
        &output_path,
        json_output,
        keep_progressive_flow_open,
    )?;

    Ok(InstallResult {
        capsule_id,
        scoped_id: scoped_ref.scoped_id.clone(),
        publisher: scoped_ref.publisher,
        slug: display_slug,
        version,
        path: output_path.clone(),
        content_hash: computed_blake3.to_string(),
        install_kind: InstallKind::Standard,
        launchable: Some(LaunchableTarget::CapsuleArchive {
            path: output_path.clone(),
        }),
        local_derivation: None,
        projection: None,
        promotion,
    })
}

fn ensure_local_finalize_allowed(
    yes: bool,
    can_prompt_interactively: bool,
    json_output: bool,
) -> Result<()> {
    if !crate::build::native_delivery::host_supports_finalize() {
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

    Ok(())
}

fn persist_installed_capsule_with_promotion(
    output_dir: Option<PathBuf>,
    scoped_ref: &ScopedCapsuleRef,
    target_version: &str,
    normalized_file_name: &str,
    bytes: &[u8],
    computed_blake3: &str,
    promotion_source: Option<&PromotionSourceInfo>,
) -> Result<(PathBuf, Option<PromotionInfo>)> {
    let output_path = persist_installed_artifact(
        output_dir,
        &scoped_ref.publisher,
        &scoped_ref.slug,
        target_version,
        normalized_file_name,
        bytes,
        computed_blake3,
    )?;
    let promotion = persist_promotion_info(&output_path, promotion_source, computed_blake3)?;
    if promotion.is_some() {
        let _ = runtime_tree::prepare_promoted_runtime_for_capsule(&output_path)?;
    }
    Ok((output_path, promotion))
}

fn emit_standard_install_success(
    scoped_id: &str,
    output_path: &Path,
    json_output: bool,
    keep_progressive_flow_open: bool,
) -> Result<()> {
    if json_output {
        return Ok(());
    }

    if crate::progressive_ui::can_use_progressive_ui(false) {
        crate::progressive_ui::show_note(
            "Installed 1 capsule",
            format!(
                "{}\nSaved to    :\n{}\nRun with    :\n  ato run {}",
                scoped_id,
                crate::progressive_ui::format_path_for_note(output_path),
                output_path.display()
            ),
        )?;
        if keep_progressive_flow_open && crate::progressive_ui::is_flow_active() {
            crate::progressive_ui::show_step(format!(
                "Installed and linked: {}",
                output_path.display()
            ))?;
        } else {
            crate::progressive_ui::show_outro(format!(
                "Done! Run persistently with: ato run {}",
                output_path.display()
            ))?;
        }
    } else {
        eprintln!("✅ Installed to: {}", output_path.display());
        eprintln!("   To run: ato run {}", output_path.display());
    }

    Ok(())
}

fn native_delivery_schema_version() -> String {
    crate::build::native_delivery::delivery_schema_version().to_string()
}

fn skipped_projection_info(json_output: bool) -> ProjectionInfo {
    if !json_output {
        eprintln!("Launcher projection skipped.");
    }
    ProjectionInfo {
        performed: false,
        projection_id: None,
        projected_path: None,
        state: Some("skipped".to_string()),
        schema_version: Some(native_delivery_schema_version()),
        metadata_path: None,
    }
}

fn failed_projection_info(
    derived_app_path: &Path,
    err: &anyhow::Error,
    json_output: bool,
) -> ProjectionInfo {
    if !json_output {
        eprintln!("Launcher projection failed: {err}");
        eprintln!(
            "Run `ato project {}` to try again later.",
            derived_app_path.display()
        );
    }
    ProjectionInfo {
        performed: false,
        projection_id: None,
        projected_path: None,
        state: Some("failed".to_string()),
        schema_version: Some(native_delivery_schema_version()),
        metadata_path: None,
    }
}

fn successful_projection_info(
    result: crate::build::native_delivery::ProjectResult,
) -> ProjectionInfo {
    ProjectionInfo {
        performed: true,
        projection_id: Some(result.projection_id),
        projected_path: Some(result.projected_path),
        state: Some(result.state),
        schema_version: Some(native_delivery_schema_version()),
        metadata_path: Some(result.metadata_path),
    }
}

fn run_projection_best_effort(derived_app_path: &Path, json_output: bool) -> ProjectionInfo {
    match crate::build::native_delivery::execute_project(derived_app_path, None) {
        Ok(result) => successful_projection_info(result),
        Err(err) => failed_projection_info(derived_app_path, &err, json_output),
    }
}

fn maybe_create_projection(
    derived_app_path: &Path,
    projection_preference: ProjectionPreference,
    yes: bool,
    can_prompt_interactively: bool,
    json_output: bool,
) -> Result<ProjectionInfo> {
    match projection_preference {
        ProjectionPreference::Skip => Ok(skipped_projection_info(json_output)),
        ProjectionPreference::Force => {
            Ok(run_projection_best_effort(derived_app_path, json_output))
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
                Ok(run_projection_best_effort(derived_app_path, json_output))
            } else {
                Ok(skipped_projection_info(json_output))
            }
        }
    }
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

pub async fn fetch_capsule_detail(
    capsule_ref: &str,
    registry_url: Option<&str>,
) -> Result<CapsuleDetailSummary> {
    let scoped_ref = parse_capsule_ref(capsule_ref)?;
    let registry = resolve_registry_url(registry_url, false).await?;
    let client = reqwest::Client::new();
    let capsule = fetch_capsule_detail_record(&client, &registry, &scoped_ref).await?;

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
        "{}/v1/capsules/by/{}/{}",
        registry,
        urlencoding::encode(&scoped_ref.publisher),
        urlencoding::encode(&scoped_ref.slug)
    );
    let response = crate::registry::http::with_ato_token(client.get(&capsule_url))
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
    let manifest_response = crate::registry::http::with_ato_token(client.get(&manifest_endpoint))
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
    let response = crate::registry::http::with_ato_token(client.post(&endpoint).json(request))
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
        let response = crate::registry::http::with_ato_token(client.get(&endpoint))
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
    let response = crate::registry::http::with_ato_token(client.post(&endpoint).json(
        &ManifestLeaseRefreshRequest {
            lease_id: lease_id.to_string(),
            ttl_secs: Some(LEASE_REFRESH_INTERVAL_SECS),
        },
    ))
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
    let _ = crate::registry::http::with_ato_token(client.post(&endpoint).json(
        &ManifestLeaseReleaseRequest {
            lease_id: lease_id.to_string(),
        },
    ))
    .send()
    .await;
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
mod tests;
