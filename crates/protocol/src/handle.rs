use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Result, WireError};

const OFFICIAL_REGISTRY_DISPLAY_AUTHORITY: &str = "ato.run";
const OFFICIAL_REGISTRY_IDENTITY: &str = "ato-official";
const LOOPBACK_REGISTRY_IDENTITY_PREFIX: &str = "ato-loopback";

// NOTE: Reserved publisher names (e.g. "search", "api") are enforced by
// `apps/ato-store`'s publisher registration validator as part of the
// ato.run authority policy (see `docs/rfcs/accepted/CAPSULE_HANDLE_SPEC.md`
// §4.2). The ato-cli URL parser is authority-agnostic and therefore
// accepts any syntactically valid publisher segment.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputSurface {
    CliRun,
    CliResolve,
    DesktopOmnibar,
    DeepLink,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandleInput {
    pub raw: String,
    pub surface: InputSurface,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandleKind {
    GithubRepo,
    RegistryCapsule,
    LocalPath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryIdentity {
    pub display_authority: String,
    pub registry_identity: String,
    pub registry_endpoint: String,
}

impl RegistryIdentity {
    pub fn ato_official() -> Self {
        Self {
            display_authority: OFFICIAL_REGISTRY_DISPLAY_AUTHORITY.to_string(),
            registry_identity: OFFICIAL_REGISTRY_IDENTITY.to_string(),
            registry_endpoint: "https://api.ato.run".to_string(),
        }
    }

    pub fn loopback(display_authority: &str) -> Self {
        Self {
            display_authority: display_authority.to_string(),
            registry_identity: format!(
                "{LOOPBACK_REGISTRY_IDENTITY_PREFIX}:{}",
                display_authority.to_ascii_lowercase()
            ),
            registry_endpoint: format!("http://{display_authority}"),
        }
    }

    pub fn is_official(&self) -> bool {
        self.registry_identity == OFFICIAL_REGISTRY_IDENTITY
    }

    pub fn is_loopback(&self) -> bool {
        self.registry_identity
            .starts_with(LOOPBACK_REGISTRY_IDENTITY_PREFIX)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CanonicalHandle {
    GithubRepo {
        owner: String,
        repo: String,
    },
    RegistryCapsule {
        registry: RegistryIdentity,
        publisher: String,
        slug: String,
        version: Option<String>,
    },
    LocalPath {
        path: PathBuf,
    },
}

impl CanonicalHandle {
    pub fn kind(&self) -> HandleKind {
        match self {
            Self::GithubRepo { .. } => HandleKind::GithubRepo,
            Self::RegistryCapsule { .. } => HandleKind::RegistryCapsule,
            Self::LocalPath { .. } => HandleKind::LocalPath,
        }
    }

    pub fn display_string(&self) -> String {
        match self {
            Self::GithubRepo { owner, repo } => {
                format!("capsule://github.com/{owner}/{repo}")
            }
            Self::RegistryCapsule {
                registry,
                publisher,
                slug,
                version,
            } => {
                let base = format!(
                    "capsule://{}/{publisher}/{slug}",
                    registry.display_authority
                );
                match version {
                    Some(version) => format!("{base}@{version}"),
                    None => base,
                }
            }
            Self::LocalPath { path } => path.display().to_string(),
        }
    }

    pub fn to_cli_ref(&self) -> Option<String> {
        match self {
            Self::GithubRepo { owner, repo } => Some(format!("github.com/{owner}/{repo}")),
            Self::RegistryCapsule {
                publisher,
                slug,
                version,
                ..
            } => {
                let scoped = format!("{publisher}/{slug}");
                Some(match version {
                    Some(version) => format!("{scoped}@{version}"),
                    None => scoped,
                })
            }
            Self::LocalPath { path } => Some(path.display().to_string()),
        }
    }

    pub fn source_label(&self) -> &'static str {
        match self {
            Self::GithubRepo { .. } => "github",
            Self::RegistryCapsule { .. } => "registry",
            Self::LocalPath { .. } => "local",
        }
    }

    pub fn registry(&self) -> Option<&RegistryIdentity> {
        match self {
            Self::RegistryCapsule { registry, .. } => Some(registry),
            _ => None,
        }
    }

    pub fn registry_url_override(&self) -> Option<&str> {
        self.registry()
            .filter(|registry| !registry.is_official())
            .map(|registry| registry.registry_endpoint.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostRoute {
    pub namespace: String,
    #[serde(default)]
    pub path_segments: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SurfaceInput {
    Capsule { canonical: CanonicalHandle },
    HostRoute { route: HostRoute },
    WebUrl { url: String },
    SearchQuery { query: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResolvedSnapshot {
    GithubRepo {
        commit_sha: String,
        default_branch: Option<String>,
        fetched_at: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        materialized_source: Option<MaterializedSourceIdentity>,
    },
    RegistryRelease {
        version: String,
        release_id: Option<String>,
        content_hash: Option<String>,
        fetched_at: String,
    },
    LocalPath {
        resolved_path: String,
        fetched_at: String,
    },
}

/// Immutable identity of the bytes that a Git-backed launch will actually see.
///
/// `commit_oid` is provenance. The archive and tree hashes are Ato-managed
/// content identities. Observation metadata such as `fetched_at` deliberately
/// does not belong in this type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterializedSourceIdentity {
    pub commit_oid: String,
    pub source_archive_hash: String,
    pub materialized_tree_hash: String,
}

impl MaterializedSourceIdentity {
    pub fn stable_hash(&self) -> String {
        stable_identity_hash(
            "github_source",
            [
                ("commit_oid", self.commit_oid.as_str()),
                ("source_archive_hash", self.source_archive_hash.as_str()),
                (
                    "materialized_tree_hash",
                    self.materialized_tree_hash.as_str(),
                ),
            ],
        )
    }
}

/// Stable, metadata-free projection of a resolved snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SnapshotIdentity {
    GithubSource {
        source: MaterializedSourceIdentity,
    },
    RegistryRelease {
        version: String,
        release_id: Option<String>,
        content_hash: String,
    },
}

impl SnapshotIdentity {
    pub fn stable_hash(&self) -> String {
        match self {
            Self::GithubSource { source } => source.stable_hash(),
            Self::RegistryRelease {
                version,
                release_id,
                content_hash,
            } => stable_identity_hash(
                "registry_release",
                [
                    ("version", version.as_str()),
                    ("release_id", release_id.as_deref().unwrap_or("")),
                    ("content_hash", content_hash.as_str()),
                ],
            ),
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SnapshotIdentityProjectionError {
    #[error("materialized source commit does not match the resolved GitHub commit")]
    CommitMismatch,
}

impl ResolvedSnapshot {
    /// Project cache/provenance-bearing resolution data into immutable identity.
    ///
    /// `None` means the source has not been materialized (or a registry response
    /// did not include a content hash), so claiming a full content identity
    /// would be incorrect.
    pub fn stable_identity(
        &self,
    ) -> std::result::Result<Option<SnapshotIdentity>, SnapshotIdentityProjectionError> {
        match self {
            Self::GithubRepo {
                commit_sha,
                materialized_source: Some(source),
                ..
            } => {
                if source.commit_oid != *commit_sha {
                    return Err(SnapshotIdentityProjectionError::CommitMismatch);
                }
                Ok(Some(SnapshotIdentity::GithubSource {
                    source: source.clone(),
                }))
            }
            Self::GithubRepo { .. } | Self::LocalPath { .. } => Ok(None),
            Self::RegistryRelease {
                version,
                release_id,
                content_hash: Some(content_hash),
                ..
            } => Ok(Some(SnapshotIdentity::RegistryRelease {
                version: version.clone(),
                release_id: release_id.clone(),
                content_hash: content_hash.clone(),
            })),
            Self::RegistryRelease { .. } => Ok(None),
        }
    }
}

fn stable_identity_hash<'a>(
    kind: &str,
    fields: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"ato-snapshot-identity-v1\0");
    update_identity_field(&mut hasher, "kind", kind);
    for (name, value) in fields {
        update_identity_field(&mut hasher, name, value);
    }
    let mut hex = String::with_capacity(64);
    for byte in hasher.finalize() {
        let _ = write!(hex, "{byte:02x}");
    }
    format!("sha256:{hex}")
}

fn update_identity_field(hasher: &mut Sha256, name: &str, value: &str) {
    hasher.update((name.len() as u64).to_be_bytes());
    hasher.update(name.as_bytes());
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustState {
    Unknown,
    Untrusted,
    Trusted,
    Promoted,
    Local,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitialIsolationPolicy {
    pub network: bool,
    pub filesystem_read: bool,
    pub filesystem_write: bool,
    pub secrets: bool,
    pub devices: bool,
}

impl InitialIsolationPolicy {
    pub fn fail_closed() -> Self {
        Self {
            network: false,
            filesystem_read: false,
            filesystem_write: false,
            secrets: false,
            devices: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRequestPolicy {
    pub allow_once: bool,
    pub allow_for_session: bool,
    pub deny: bool,
}

impl PermissionRequestPolicy {
    pub fn jit_default() -> Self {
        Self {
            allow_once: true,
            allow_for_session: true,
            deny: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchIntent {
    pub input: HandleInput,
    pub canonical: CanonicalHandle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedHandle {
    pub input: HandleInput,
    pub canonical: CanonicalHandle,
    pub snapshot: Option<ResolvedSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapsuleDisplayStrategy {
    GuestWebview,
    WebUrl,
    TerminalStream,
    ServiceBackground,
    Unsupported,
}

impl CapsuleDisplayStrategy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::GuestWebview => "guest_webview",
            Self::WebUrl => "web_url",
            Self::TerminalStream => "terminal_stream",
            Self::ServiceBackground => "service_background",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CapsuleRuntimeDescriptor {
    pub target_label: String,
    pub runtime: Option<String>,
    pub driver: Option<String>,
    pub language: Option<String>,
    pub port: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchPlan {
    pub canonical: CanonicalHandle,
    pub snapshot: Option<ResolvedSnapshot>,
    pub trust_state: TrustState,
    pub initial_isolation: InitialIsolationPolicy,
    pub permission_requests: PermissionRequestPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedMetadataCacheEntry {
    pub canonical: CanonicalHandle,
    pub normalized_input: String,
    pub manifest_summary: Option<String>,
    pub snapshot: Option<ResolvedSnapshot>,
    pub fetched_at: String,
    pub ttl_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalTrustDecisionRecord {
    pub canonical: CanonicalHandle,
    pub trust_state: TrustState,
    pub session_scoped: bool,
    pub recorded_at: String,
    pub reason: Option<String>,
}

pub trait HandleResolutionHost {
    fn registry_identity_for_display_authority(&self, authority: &str) -> Option<RegistryIdentity>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct StaticHandleResolutionHost;

impl HandleResolutionHost for StaticHandleResolutionHost {
    fn registry_identity_for_display_authority(&self, authority: &str) -> Option<RegistryIdentity> {
        registry_identity_for_display_authority(authority)
    }
}

pub fn classify_surface_input(input: HandleInput) -> Result<SurfaceInput> {
    let raw = input.raw.trim();
    if raw.is_empty() {
        return Ok(SurfaceInput::SearchQuery {
            query: String::new(),
        });
    }

    if raw.starts_with("http://") || raw.starts_with("https://") {
        return Ok(SurfaceInput::WebUrl {
            url: raw.to_string(),
        });
    }

    if raw.starts_with("ato://") {
        return Ok(SurfaceInput::HostRoute {
            route: parse_host_route(raw)?,
        });
    }

    let expanded_local = expand_local_path(raw);
    if should_treat_input_as_local(raw, &expanded_local) {
        let canonical = expanded_local.canonicalize().unwrap_or(expanded_local);
        return Ok(SurfaceInput::Capsule {
            canonical: CanonicalHandle::LocalPath { path: canonical },
        });
    }

    if looks_like_capsule_or_registry_ref(raw) {
        return Ok(SurfaceInput::Capsule {
            canonical: normalize_capsule_handle(raw)?,
        });
    }

    Ok(SurfaceInput::SearchQuery {
        query: raw.to_string(),
    })
}

pub fn normalize_capsule_handle(raw: &str) -> Result<CanonicalHandle> {
    let input = raw.trim();
    if input.is_empty() {
        return Err(WireError::Config("handle must not be empty".to_string()));
    }

    if let Some(rest) = input.strip_prefix("capsule://github.com/") {
        return parse_github_rest(rest);
    }

    if let Some(rest) = input.strip_prefix("capsule://ato.run/") {
        return parse_registry_rest(rest, RegistryIdentity::ato_official());
    }

    // `capsule://store/` is a deprecated alias for `capsule://ato.run/`.
    // Accept it at parse time and treat it as the official registry.
    if let Some(rest) = input.strip_prefix("capsule://store/") {
        return parse_registry_rest(rest, RegistryIdentity::ato_official());
    }

    if let Some(rest) = input.strip_prefix("capsule://") {
        let (authority, registry_rest) = split_capsule_authority(rest)?;
        if let Some(registry) = registry_identity_for_display_authority(authority) {
            return parse_registry_rest(registry_rest, registry);
        }
        return Err(WireError::Config(format!(
            "unsupported capsule handle '{}': use capsule://ato.run/publisher/slug, capsule://github.com/owner/repo, or capsule://localhost:<port>/publisher/slug",
            input
        )));
    }

    if input.starts_with("github.com/") {
        return parse_github_rest(input.trim_start_matches("github.com/"));
    }

    if looks_like_scoped_registry_ref(input) {
        return parse_registry_rest(input, RegistryIdentity::ato_official());
    }

    Err(WireError::Config(format!("unsupported handle '{}'", input)))
}

pub fn registry_identity_for_display_authority(authority: &str) -> Option<RegistryIdentity> {
    if authority.eq_ignore_ascii_case(OFFICIAL_REGISTRY_DISPLAY_AUTHORITY) {
        return Some(RegistryIdentity::ato_official());
    }
    loopback_registry_identity(authority)
}

pub fn parse_host_route(raw: &str) -> Result<HostRoute> {
    let rest = raw
        .trim()
        .strip_prefix("ato://")
        .ok_or_else(|| WireError::Config("invalid ato:// host route".to_string()))?;
    let segments = rest
        .split('/')
        .filter(|segment| !segment.trim().is_empty())
        .map(|segment| segment.trim().to_string())
        .collect::<Vec<_>>();
    let Some((namespace, tail)) = segments.split_first() else {
        return Err(WireError::Config(
            "ato:// host route requires a namespace".to_string(),
        ));
    };

    Ok(HostRoute {
        namespace: namespace.clone(),
        path_segments: tail.to_vec(),
    })
}

fn parse_github_rest(rest: &str) -> Result<CanonicalHandle> {
    let mut segments = rest
        .split('/')
        .filter(|segment| !segment.trim().is_empty())
        .map(|segment| segment.trim().trim_end_matches(".git").to_string());
    let owner = segments
        .next()
        .ok_or_else(|| WireError::Config("github handle requires owner/repo".to_string()))?;
    let repo = segments
        .next()
        .ok_or_else(|| WireError::Config("github handle requires owner/repo".to_string()))?;
    if segments.next().is_some() {
        return Err(WireError::Config(
            "github handle must use github.com/owner/repo".to_string(),
        ));
    }

    Ok(CanonicalHandle::GithubRepo { owner, repo })
}

fn parse_registry_rest(rest: &str, registry: RegistryIdentity) -> Result<CanonicalHandle> {
    let (path_part, version) = rest
        .rsplit_once('@')
        .map(|(path, version)| (path, Some(version.trim().to_string())))
        .unwrap_or((rest, None));
    let mut segments = path_part
        .split('/')
        .filter(|segment| !segment.trim().is_empty())
        .map(|segment| segment.trim().to_string());
    let publisher = segments
        .next()
        .ok_or_else(|| WireError::Config("registry handle requires publisher/slug".to_string()))?;
    let slug = segments
        .next()
        .ok_or_else(|| WireError::Config("registry handle requires publisher/slug".to_string()))?;
    if segments.next().is_some() {
        return Err(WireError::Config(
            "registry handle must use publisher/slug".to_string(),
        ));
    }

    Ok(CanonicalHandle::RegistryCapsule {
        registry,
        publisher,
        slug,
        version,
    })
}

fn split_capsule_authority(rest: &str) -> Result<(&str, &str)> {
    rest.split_once('/').ok_or_else(|| {
        WireError::Config(
            "capsule handle requires an authority and publisher/slug path".to_string(),
        )
    })
}

fn loopback_registry_identity(authority: &str) -> Option<RegistryIdentity> {
    is_loopback_registry_authority(authority).then(|| RegistryIdentity::loopback(authority))
}

fn is_loopback_registry_authority(authority: &str) -> bool {
    let trimmed = authority.trim();
    matches_loopback_authority(trimmed, "localhost:")
        || matches_loopback_authority(trimmed, "127.0.0.1:")
        || matches_bracketed_loopback_ipv6(trimmed)
}

fn matches_loopback_authority(authority: &str, prefix: &str) -> bool {
    authority.strip_prefix(prefix).is_some_and(has_numeric_port)
}

fn matches_bracketed_loopback_ipv6(authority: &str) -> bool {
    authority
        .strip_prefix("[::1]:")
        .is_some_and(has_numeric_port)
}

fn has_numeric_port(port: &str) -> bool {
    !port.is_empty() && port.chars().all(|ch| ch.is_ascii_digit())
}

fn looks_like_capsule_or_registry_ref(raw: &str) -> bool {
    raw.starts_with("capsule://")
        || raw.starts_with("github.com/")
        || looks_like_scoped_registry_ref(raw)
}

fn looks_like_scoped_registry_ref(raw: &str) -> bool {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.starts_with("ato://") || trimmed.contains(' ') {
        return false;
    }
    let candidate = trimmed
        .split_once('@')
        .map(|(prefix, _)| prefix)
        .unwrap_or(trimmed);
    let mut parts = candidate.split('/');
    let Some(first) = parts.next() else {
        return false;
    };
    let Some(second) = parts.next() else {
        return false;
    };
    parts.next().is_none() && !first.is_empty() && !second.is_empty()
}

fn expand_local_path(raw: &str) -> PathBuf {
    if raw == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(raw));
    }
    if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\"))
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(raw)
}

fn should_treat_input_as_local(raw: &str, expanded_path: &Path) -> bool {
    expanded_path.exists()
        || is_explicit_local_path_input(raw)
        || looks_like_local_capsule_artifact(raw)
}

fn is_explicit_local_path_input(raw: &str) -> bool {
    if raw.is_empty() {
        return false;
    }
    if raw == "." || raw == ".." {
        return true;
    }
    if raw.starts_with("./")
        || raw.starts_with("../")
        || raw.starts_with(".\\")
        || raw.starts_with("..\\")
        || raw.starts_with("~/")
        || raw.starts_with("~\\")
        || raw.starts_with('/')
        || raw.starts_with('\\')
    {
        return true;
    }

    raw.len() >= 3
        && raw.as_bytes()[1] == b':'
        && (raw.as_bytes()[2] == b'/' || raw.as_bytes()[2] == b'\\')
        && raw.as_bytes()[0].is_ascii_alphabetic()
}

fn looks_like_local_capsule_artifact(raw: &str) -> bool {
    let trimmed = raw.trim();
    !trimmed.is_empty() && trimmed.ends_with(".capsule")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_github_shorthand_to_canonical() {
        let canonical = normalize_capsule_handle("github.com/acme/chat").expect("normalize");
        assert_eq!(canonical.display_string(), "capsule://github.com/acme/chat");
        assert_eq!(
            canonical.to_cli_ref().as_deref(),
            Some("github.com/acme/chat")
        );
    }

    #[test]
    fn normalizes_registry_shorthand_to_canonical() {
        let canonical = normalize_capsule_handle("acme/chat").expect("normalize");
        assert_eq!(canonical.display_string(), "capsule://ato.run/acme/chat");
        assert_eq!(canonical.to_cli_ref().as_deref(), Some("acme/chat"));
    }

    #[test]
    fn rejects_registry_handle_without_authority() {
        let error = normalize_capsule_handle("capsule://acme/chat").expect_err("reject");
        assert!(error.to_string().contains("unsupported capsule handle"));
    }

    #[test]
    fn normalizes_loopback_registry_handle_to_canonical() {
        let canonical =
            normalize_capsule_handle("capsule://localhost:8787/acme/chat").expect("normalize");
        assert_eq!(
            canonical.display_string(),
            "capsule://localhost:8787/acme/chat"
        );
        assert_eq!(canonical.to_cli_ref().as_deref(), Some("acme/chat"));
        assert_eq!(
            canonical.registry_url_override(),
            Some("http://localhost:8787")
        );
    }

    #[test]
    fn accepts_ipv4_and_ipv6_loopback_registry_handles() {
        let ipv4 = normalize_capsule_handle("capsule://127.0.0.1:8787/acme/chat").expect("ipv4");
        let ipv6 = normalize_capsule_handle("capsule://[::1]:8787/acme/chat").expect("ipv6");
        assert_eq!(ipv4.display_string(), "capsule://127.0.0.1:8787/acme/chat");
        assert_eq!(ipv6.display_string(), "capsule://[::1]:8787/acme/chat");
    }

    #[test]
    fn rejects_capsule_local_authority() {
        let error = normalize_capsule_handle("capsule://local/path/to/dir").expect_err("reject");
        assert!(error.to_string().contains("unsupported capsule handle"));
    }

    #[test]
    fn parses_host_route_separately_from_capsule_handles() {
        let route = parse_host_route("ato://auth/callback").expect("host route");
        assert_eq!(route.namespace, "auth");
        assert_eq!(route.path_segments, vec!["callback"]);
    }

    #[test]
    fn classifies_desktop_registry_sugar_as_capsule_handle() {
        let surface = classify_surface_input(HandleInput {
            raw: "acme/chat".to_string(),
            surface: InputSurface::DesktopOmnibar,
        })
        .expect("classify");
        match surface {
            SurfaceInput::Capsule { canonical } => {
                assert_eq!(canonical.display_string(), "capsule://ato.run/acme/chat");
            }
            other => panic!("expected capsule surface, got {other:?}"),
        }
    }

    /// Day 6.5 — verify every URL form the desktop omnibar should accept
    /// for the two demo capsules (byok-ai-chat, openclaw-local-llm).
    #[test]
    fn day6_all_omnibar_url_forms_resolve_correctly() {
        struct Case {
            input: &'static str,
            expected_display: &'static str,
        }

        let cases = vec![
            // 1. scoped_id shorthand → ato.run official registry
            Case {
                input: "ato/byok-ai-chat",
                expected_display: "capsule://ato.run/ato/byok-ai-chat",
            },
            Case {
                input: "ato/openclaw-local-llm",
                expected_display: "capsule://ato.run/ato/openclaw-local-llm",
            },
            // 2. capsule:// with explicit ato.run authority
            Case {
                input: "capsule://ato.run/ato/byok-ai-chat",
                expected_display: "capsule://ato.run/ato/byok-ai-chat",
            },
            // 3. capsule:// with localhost loopback
            Case {
                input: "capsule://127.0.0.1:8787/ato/byok-ai-chat",
                expected_display: "capsule://127.0.0.1:8787/ato/byok-ai-chat",
            },
            Case {
                input: "capsule://localhost:8787/ato/openclaw-local-llm",
                expected_display: "capsule://localhost:8787/ato/openclaw-local-llm",
            },
            // 4. github.com shorthand
            Case {
                input: "github.com/user/repo",
                expected_display: "capsule://github.com/user/repo",
            },
            Case {
                input: "capsule://github.com/user/repo",
                expected_display: "capsule://github.com/user/repo",
            },
        ];

        for case in &cases {
            let surface = classify_surface_input(HandleInput {
                raw: case.input.to_string(),
                surface: InputSurface::DesktopOmnibar,
            })
            .unwrap_or_else(|e| panic!("classify '{}' failed: {e}", case.input));
            match surface {
                SurfaceInput::Capsule { canonical } => {
                    assert_eq!(
                        canonical.display_string(),
                        case.expected_display,
                        "input: '{}'",
                        case.input,
                    );
                }
                other => panic!("input '{}': expected Capsule, got {other:?}", case.input,),
            }
        }

        // Local path form — classify_surface_input routes these as
        // LocalPath capsules (not SearchQuery), so verify they're accepted.
        let local = classify_surface_input(HandleInput {
            raw: "/Users/test/samples/byok-ai-chat".to_string(),
            surface: InputSurface::DesktopOmnibar,
        })
        .expect("classify local path");
        match local {
            SurfaceInput::Capsule {
                canonical: CanonicalHandle::LocalPath { .. },
            } => {} // ok
            other => panic!("expected LocalPath capsule, got {other:?}"),
        }

        // https:// URLs → WebUrl (external browser), not capsule
        let web = classify_surface_input(HandleInput {
            raw: "https://ato.run".to_string(),
            surface: InputSurface::DesktopOmnibar,
        })
        .expect("classify web url");
        match web {
            SurfaceInput::WebUrl { url } => {
                assert_eq!(url, "https://ato.run");
            }
            other => panic!("expected WebUrl, got {other:?}"),
        }
    }

    fn github_snapshot(fetched_at: &str) -> ResolvedSnapshot {
        ResolvedSnapshot::GithubRepo {
            commit_sha: "1111111111111111111111111111111111111111".to_string(),
            default_branch: Some("main".to_string()),
            fetched_at: fetched_at.to_string(),
            materialized_source: Some(MaterializedSourceIdentity {
                commit_oid: "1111111111111111111111111111111111111111".to_string(),
                source_archive_hash: format!("sha256:{}", "2".repeat(64)),
                materialized_tree_hash: format!("sha256:{}", "3".repeat(64)),
            }),
        }
    }

    #[test]
    fn snapshot_identity_hash_excludes_fetched_at() {
        let first = github_snapshot("2026-07-14T00:00:00Z")
            .stable_identity()
            .expect("identity projection")
            .expect("materialized identity");
        let later = github_snapshot("2030-01-01T00:00:00Z")
            .stable_identity()
            .expect("identity projection")
            .expect("materialized identity");

        assert_eq!(first, later);
        assert_eq!(first.stable_hash(), later.stable_hash());
    }

    #[test]
    fn materialized_source_identity_hash_tracks_every_content_facet() {
        let base = match github_snapshot("2026-07-14T00:00:00Z")
            .stable_identity()
            .expect("identity projection")
            .expect("materialized identity")
        {
            SnapshotIdentity::GithubSource { source } => source,
            other => panic!("unexpected identity: {other:?}"),
        };
        let base_hash = base.stable_hash();

        for changed in [
            MaterializedSourceIdentity {
                commit_oid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                ..base.clone()
            },
            MaterializedSourceIdentity {
                source_archive_hash: format!("sha256:{}", "b".repeat(64)),
                ..base.clone()
            },
            MaterializedSourceIdentity {
                materialized_tree_hash: format!("sha256:{}", "c".repeat(64)),
                ..base.clone()
            },
        ] {
            assert_ne!(base_hash, changed.stable_hash());
        }
    }

    #[test]
    fn snapshot_identity_rejects_commit_mismatch() {
        let mut snapshot = github_snapshot("2026-07-14T00:00:00Z");
        if let ResolvedSnapshot::GithubRepo { commit_sha, .. } = &mut snapshot {
            *commit_sha = "ffffffffffffffffffffffffffffffffffffffff".to_string();
        }
        assert_eq!(
            snapshot.stable_identity(),
            Err(SnapshotIdentityProjectionError::CommitMismatch)
        );
    }
}
