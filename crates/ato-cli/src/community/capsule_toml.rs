use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use toml::Value as TomlValue;
use tracing::debug;

const DEFAULT_COMMUNITY_API_URL: &str = "https://api.ato.run";
const ENV_COMMUNITY_API_URL: &str = "ATO_COMMUNITY_API_URL";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CommunityTrustLevel {
    Community,
    Owner,
    Official,
}

impl CommunityTrustLevel {
    pub(crate) fn trust_rank(&self) -> u8 {
        match self {
            CommunityTrustLevel::Community => 0,
            CommunityTrustLevel::Owner => 1,
            CommunityTrustLevel::Official => 2,
        }
    }
}

impl<'de> Deserialize<'de> for CommunityTrustLevel {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.to_lowercase().as_str() {
            "community" => Ok(CommunityTrustLevel::Community),
            "owner" => Ok(CommunityTrustLevel::Owner),
            "official" => Ok(CommunityTrustLevel::Official),
            other => Err(serde::de::Error::custom(format!(
                "unknown trust level: {other}"
            ))),
        }
    }
}

impl Serialize for CommunityTrustLevel {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let s = match self {
            CommunityTrustLevel::Community => "community",
            CommunityTrustLevel::Owner => "owner",
            CommunityTrustLevel::Official => "official",
        };
        serializer.serialize_str(s)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CommunityCapsuleTomlCandidate {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) source: String,
    pub(crate) trust: CommunityTrustLevel,
    pub(crate) stars: u64,
    pub(crate) platforms: Vec<String>,
    pub(crate) last_verified_at: Option<String>,
    pub(crate) permissions_summary: Vec<String>,
    pub(crate) capsule_toml_url: String,
    pub(crate) revision: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCommunityCandidatesResponse {
    candidates: Vec<CommunityCapsuleTomlCandidate>,
}

pub(crate) struct ResolvedCommunityToml {
    pub(crate) candidate: CommunityCapsuleTomlCandidate,
    pub(crate) toml_content: String,
    pub(crate) local_path: Option<PathBuf>,
}

pub(crate) fn resolve_community_api_base_url() -> String {
    std::env::var(ENV_COMMUNITY_API_URL)
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_COMMUNITY_API_URL.to_string())
}

fn community_api_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .with_context(|| "Failed to build community API client")
}

pub(crate) fn platform_display_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(target_os = "linux") {
        "Linux"
    } else if cfg!(target_os = "windows") {
        "Windows"
    } else {
        "Unknown"
    }
}

fn platform_matches(platforms: &[String]) -> bool {
    let current = platform_display_name().to_lowercase();
    platforms.iter().any(|p| p.to_lowercase() == current)
}

fn format_relative_time(verified_at: &str) -> String {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(verified_at) {
        let dt_utc = dt.with_timezone(&chrono::Utc);
        let duration = chrono::Utc::now() - dt_utc;
        if duration.num_days() > 0 {
            return format!("{} days ago", duration.num_days());
        } else if duration.num_hours() > 0 {
            return format!("{} hours ago", duration.num_hours());
        } else if duration.num_minutes() > 0 {
            return format!("{} minutes ago", duration.num_minutes());
        }
        return "just now".to_string();
    }
    "unknown".to_string()
}

pub(crate) fn sort_candidates(candidates: &mut [CommunityCapsuleTomlCandidate]) {
    candidates.sort_by(|a, b| {
        let a_platform_match = platform_matches(&a.platforms);
        let b_platform_match = platform_matches(&b.platforms);
        b_platform_match
            .cmp(&a_platform_match)
            .then_with(|| b.trust.trust_rank().cmp(&a.trust.trust_rank()))
            .then_with(|| b.stars.cmp(&a.stars))
            .then_with(|| {
                let a_time = a
                    .last_verified_at
                    .as_deref()
                    .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok());
                let b_time = b
                    .last_verified_at
                    .as_deref()
                    .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok());
                b_time.cmp(&a_time)
            })
    });
}

pub(crate) async fn fetch_community_capsule_tomls(
    source: &str,
) -> Result<Vec<CommunityCapsuleTomlCandidate>> {
    let client = community_api_client()?;
    let endpoint = format!(
        "{}/v1/capsule-tomls?source={}",
        resolve_community_api_base_url(),
        urlencoding::encode(source)
    );
    debug!(%endpoint, "fetching community capsule.toml candidates");
    let response = client
        .get(&endpoint)
        .header(reqwest::header::USER_AGENT, "ato-cli")
        .send()
        .await
        .with_context(|| format!("Failed to fetch community capsule.tomls for {source}"))?;
    if response.status() == 404 {
        return Ok(Vec::new());
    }
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!(
            "Failed to fetch community capsule.tomls (status={}): {}",
            status,
            body
        );
    }
    let raw: RawCommunityCandidatesResponse = response
        .json()
        .await
        .with_context(|| "Failed to parse community capsule.toml candidates")?;
    Ok(raw.candidates)
}

pub(crate) async fn fetch_capsule_toml_by_id(id: &str) -> Result<String> {
    let client = community_api_client()?;
    let endpoint = format!("{}/v1/capsule-tomls/{id}", resolve_community_api_base_url());
    debug!(%endpoint, "fetching capsule.toml by id");
    let response = client
        .get(&endpoint)
        .header(reqwest::header::USER_AGENT, "ato-cli")
        .send()
        .await
        .with_context(|| format!("Failed to fetch capsule.toml by id: {id}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!(
            "Failed to fetch capsule.toml by id (status={}): {}",
            status,
            body
        );
    }
    response
        .text()
        .await
        .with_context(|| "Failed to read capsule.toml response body")
}

pub(crate) async fn fetch_toml_from_url(url: &str) -> Result<String> {
    let client = community_api_client()?;
    debug!(%url, "fetching capsule.toml from URL");
    let response = client
        .get(url)
        .header(reqwest::header::USER_AGENT, "ato-cli")
        .send()
        .await
        .with_context(|| format!("Failed to fetch capsule.toml from {url}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!(
            "Failed to fetch capsule.toml from URL (status={}): {}",
            status,
            body
        );
    }
    response
        .text()
        .await
        .with_context(|| "Failed to read capsule.toml from URL response body")
}

pub(crate) fn validate_capsule_toml_source_matches_run_target(
    toml_content: &str,
    normalized_source: &str,
) -> Result<()> {
    let parsed: TomlValue = toml::from_str(toml_content)
        .with_context(|| "Failed to parse capsule.toml for source validation")?;

    let toml_source = parsed
        .get("source")
        .and_then(|s| s.get("repository"))
        .and_then(|v| v.as_str())
        .or_else(|| {
            parsed
                .get("metadata")
                .and_then(|m| m.get("repository"))
                .and_then(|v| v.as_str())
        });

    match toml_source {
        Some(src) if src == normalized_source => Ok(()),
        Some(src) => bail!(
            "Source identity mismatch: capsule.toml declares source '{}', but run target is '{}'. \
             The TOML must reference the same repository.",
            src,
            normalized_source
        ),
        None => {
            debug!(
                %normalized_source,
                "capsule.toml has no source.repository or metadata.repository; skipping source identity validation"
            );
            Ok(())
        }
    }
}

pub(crate) fn format_candidate_for_display(
    candidate: &CommunityCapsuleTomlCandidate,
    index: usize,
) -> String {
    let trust_label = match candidate.trust {
        CommunityTrustLevel::Community => "community",
        CommunityTrustLevel::Owner => "owner",
        CommunityTrustLevel::Official => "official",
    };
    let platforms = if candidate.platforms.is_empty() {
        "any".to_string()
    } else {
        candidate.platforms.join("/")
    };
    let verified = candidate
        .last_verified_at
        .as_deref()
        .map(|v| format!("verified {}", format_relative_time(v)))
        .unwrap_or_else(|| "not verified".to_string());
    let permissions = if candidate.permissions_summary.is_empty() {
        "none".to_string()
    } else {
        candidate.permissions_summary.join(", ")
    };

    format!(
        "{}. {}\n   {} · ★{} · {} · {}\n   permissions: {}",
        index + 1,
        candidate.title,
        trust_label,
        candidate.stars,
        platforms,
        verified,
        permissions
    )
}

pub(crate) fn prompt_community_candidate_selection(
    source: &str,
    candidates: &[CommunityCapsuleTomlCandidate],
) -> Result<usize> {
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        bail!(
            "TOML_SELECTION_REQUIRED: multiple community capsule.toml candidates exist for '{}'. \
             In non-TTY environments, either use -T/--use-existing-toml to specify which TOML to use, \
             or run with --yes when exactly one candidate exists.",
            source
        );
    }

    eprintln!("\nSelect capsule.toml for {}", source);
    eprintln!();
    for (i, candidate) in candidates.iter().enumerate() {
        eprintln!("{}", format_candidate_for_display(candidate, i));
        eprintln!();
    }
    eprintln!(
        "{}. Infer new capsule.toml (skip community recipes)",
        candidates.len() + 1
    );
    eprintln!(
        "{}. Use local/remote capsule.toml (-T)",
        candidates.len() + 2
    );
    eprintln!();

    loop {
        eprint!("Enter choice (1-{}): ", candidates.len() + 2);
        use std::io::Write;
        let _ = std::io::stderr().flush();

        let mut input = String::new();
        std::io::stdin()
            .read_line(&mut input)
            .context("Failed to read selection")?;

        let choice: usize = match input.trim().parse() {
            Ok(n) if n >= 1 && n <= candidates.len() + 2 => n,
            _ => {
                eprintln!("Invalid choice, try again.");
                continue;
            }
        };

        return Ok(choice);
    }
}

pub(crate) fn is_tty_available() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

pub(crate) fn should_prompt_community_sharing() -> bool {
    is_tty_available()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_level_deserializes_community() {
        let json = r#""community""#;
        let level: CommunityTrustLevel = serde_json::from_str(json).expect("deserialize");
        assert_eq!(level, CommunityTrustLevel::Community);
    }

    #[test]
    fn trust_level_deserializes_owner() {
        let json = r#""owner""#;
        let level: CommunityTrustLevel = serde_json::from_str(json).expect("deserialize");
        assert_eq!(level, CommunityTrustLevel::Owner);
    }

    #[test]
    fn trust_level_deserializes_official() {
        let json = r#""official""#;
        let level: CommunityTrustLevel = serde_json::from_str(json).expect("deserialize");
        assert_eq!(level, CommunityTrustLevel::Official);
    }

    #[test]
    fn trust_level_rank_order() {
        assert!(
            CommunityTrustLevel::Community.trust_rank() < CommunityTrustLevel::Owner.trust_rank()
        );
        assert!(
            CommunityTrustLevel::Owner.trust_rank() < CommunityTrustLevel::Official.trust_rank()
        );
    }

    #[test]
    fn candidate_sorting_prefers_platform_match() {
        let mut candidates = vec![
            CommunityCapsuleTomlCandidate {
                id: "1".into(),
                title: "A".into(),
                source: "github.com/a/b".into(),
                trust: CommunityTrustLevel::Community,
                stars: 10,
                platforms: vec![],
                last_verified_at: None,
                permissions_summary: vec![],
                capsule_toml_url: "https://a.toml".into(),
                revision: None,
            },
            CommunityCapsuleTomlCandidate {
                id: "2".into(),
                title: "B".into(),
                source: "github.com/a/b".into(),
                trust: CommunityTrustLevel::Community,
                stars: 5,
                platforms: vec![platform_display_name().to_string()],
                last_verified_at: None,
                permissions_summary: vec![],
                capsule_toml_url: "https://b.toml".into(),
                revision: None,
            },
        ];

        sort_candidates(&mut candidates);
        assert_eq!(candidates[0].id, "2");
    }

    #[test]
    fn candidate_sorting_prefers_higher_trust() {
        let mut candidates = vec![
            CommunityCapsuleTomlCandidate {
                id: "1".into(),
                title: "A".into(),
                source: "github.com/a/b".into(),
                trust: CommunityTrustLevel::Owner,
                stars: 5,
                platforms: vec![],
                last_verified_at: None,
                permissions_summary: vec![],
                capsule_toml_url: "https://a.toml".into(),
                revision: None,
            },
            CommunityCapsuleTomlCandidate {
                id: "2".into(),
                title: "B".into(),
                source: "github.com/a/b".into(),
                trust: CommunityTrustLevel::Official,
                stars: 1,
                platforms: vec![],
                last_verified_at: None,
                permissions_summary: vec![],
                capsule_toml_url: "https://b.toml".into(),
                revision: None,
            },
        ];

        sort_candidates(&mut candidates);
        assert_eq!(candidates[0].id, "2");
    }

    #[test]
    fn candidate_sorting_prefers_more_stars() {
        let mut candidates = vec![
            CommunityCapsuleTomlCandidate {
                id: "1".into(),
                title: "A".into(),
                source: "github.com/a/b".into(),
                trust: CommunityTrustLevel::Community,
                stars: 5,
                platforms: vec![],
                last_verified_at: None,
                permissions_summary: vec![],
                capsule_toml_url: "https://a.toml".into(),
                revision: None,
            },
            CommunityCapsuleTomlCandidate {
                id: "2".into(),
                title: "B".into(),
                source: "github.com/a/b".into(),
                trust: CommunityTrustLevel::Community,
                stars: 100,
                platforms: vec![],
                last_verified_at: None,
                permissions_summary: vec![],
                capsule_toml_url: "https://b.toml".into(),
                revision: None,
            },
        ];

        sort_candidates(&mut candidates);
        assert_eq!(candidates[0].id, "2");
    }

    #[test]
    fn source_identity_match_succeeds() {
        let toml = r#"
            name = "test"
            version = "1.0.0"
            [source]
            repository = "github.com/owner/repo"
        "#;
        validate_capsule_toml_source_matches_run_target(toml, "github.com/owner/repo")
            .expect("should match");
    }

    #[test]
    fn source_identity_mismatch_fails() {
        let toml = r#"
            name = "test"
            version = "1.0.0"
            [source]
            repository = "github.com/other/repo"
        "#;
        let result = validate_capsule_toml_source_matches_run_target(toml, "github.com/owner/repo");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Source identity mismatch"));
    }

    #[test]
    fn source_identity_metadata_fallback() {
        let toml = r#"
            name = "test"
            version = "1.0.0"
            [metadata]
            repository = "github.com/owner/repo"
        "#;
        validate_capsule_toml_source_matches_run_target(toml, "github.com/owner/repo")
            .expect("should match via metadata");
    }

    #[test]
    fn resolve_community_api_base_url_defaults() {
        let url = resolve_community_api_base_url();
        assert_eq!(url, "https://api.ato.run");
    }
}
