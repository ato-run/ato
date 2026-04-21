use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CapsuleError, Result};

const OFFICIAL_REGISTRY_DISPLAY_AUTHORITY: &str = "ato.run";
const OFFICIAL_REGISTRY_IDENTITY: &str = "ato-official";
const LOOPBACK_REGISTRY_IDENTITY_PREFIX: &str = "ato-loopback";

// Publisher names reserved for first-party use or routing disambiguation.
// Accepting these as user-registered publishers would create ambiguous URIs
// (e.g. `capsule://ato.run/search/foo` colliding with a search endpoint).
const RESERVED_PUBLISHERS: &[&str] = &[
    "search", "topic", "user", "store", "api", "registry", "help", "docs", "status",
];

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
        return Err(CapsuleError::Config("handle must not be empty".to_string()));
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
        return Err(CapsuleError::Config(format!(
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

    Err(CapsuleError::Config(format!(
        "unsupported handle '{}'",
        input
    )))
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
        .ok_or_else(|| CapsuleError::Config("invalid ato:// host route".to_string()))?;
    let segments = rest
        .split('/')
        .filter(|segment| !segment.trim().is_empty())
        .map(|segment| segment.trim().to_string())
        .collect::<Vec<_>>();
    let Some((namespace, tail)) = segments.split_first() else {
        return Err(CapsuleError::Config(
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
        .ok_or_else(|| CapsuleError::Config("github handle requires owner/repo".to_string()))?;
    let repo = segments
        .next()
        .ok_or_else(|| CapsuleError::Config("github handle requires owner/repo".to_string()))?;
    if segments.next().is_some() {
        return Err(CapsuleError::Config(
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
    let publisher = segments.next().ok_or_else(|| {
        CapsuleError::Config("registry handle requires publisher/slug".to_string())
    })?;
    let slug = segments.next().ok_or_else(|| {
        CapsuleError::Config("registry handle requires publisher/slug".to_string())
    })?;
    if segments.next().is_some() {
        return Err(CapsuleError::Config(
            "registry handle must use publisher/slug".to_string(),
        ));
    }

    if RESERVED_PUBLISHERS.contains(&publisher.as_str()) {
        return Err(CapsuleError::Config(format!(
            "publisher name '{}' is reserved",
            publisher
        )));
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
        CapsuleError::Config(
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
    if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
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
}
