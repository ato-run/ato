use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CapsuleError, Result};

const OFFICIAL_REGISTRY_DISPLAY_AUTHORITY: &str = "ato.run";
const OFFICIAL_REGISTRY_IDENTITY: &str = "ato-official";

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
    Capsule {
        canonical: CanonicalHandle,
    },
    HostRoute {
        route: HostRoute,
    },
    WebUrl {
        url: String,
    },
    SearchQuery {
        query: String,
    },
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

    if input.starts_with("capsule://") {
        return Err(CapsuleError::Config(format!(
            "unsupported capsule handle '{}': use capsule://ato.run/publisher/slug or capsule://github.com/owner/repo",
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

    Ok(CanonicalHandle::RegistryCapsule {
        registry,
        publisher,
        slug,
        version,
    })
}

fn looks_like_capsule_or_registry_ref(raw: &str) -> bool {
    raw.starts_with("capsule://") || raw.starts_with("github.com/") || looks_like_scoped_registry_ref(raw)
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
    expanded_path.exists() || is_explicit_local_path_input(raw) || looks_like_local_capsule_artifact(raw)
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
        assert_eq!(
            canonical.display_string(),
            "capsule://github.com/acme/chat"
        );
        assert_eq!(canonical.to_cli_ref().as_deref(), Some("github.com/acme/chat"));
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
