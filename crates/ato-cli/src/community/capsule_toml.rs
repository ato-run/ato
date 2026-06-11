use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
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
#[allow(dead_code)]
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

    #[serde(default)]
    pub(crate) successful_receipts: Option<u64>,
    #[serde(default)]
    pub(crate) failed_receipts: Option<u64>,
    #[serde(default)]
    pub(crate) current_platform_verified: Option<bool>,
    #[serde(default)]
    pub(crate) source_ref: Option<String>,
    #[serde(default)]
    pub(crate) source_ref_age_days: Option<u64>,
    #[serde(default)]
    pub(crate) risk_score: Option<u8>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCommunityCandidatesResponse {
    candidates: Vec<CommunityCapsuleTomlCandidate>,
}

#[allow(dead_code)]
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

fn community_discovery_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(5))
        .build()
        .with_context(|| "Failed to build community discovery client")
}

fn community_fetch_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .with_context(|| "Failed to build community fetch client")
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
        let a_verified = a.current_platform_verified.unwrap_or(false);
        let b_verified = b.current_platform_verified.unwrap_or(false);
        let a_receipts = a.successful_receipts.unwrap_or(0);
        let b_receipts = b.successful_receipts.unwrap_or(0);
        let a_failed = a.failed_receipts.unwrap_or(0);
        let b_failed = b.failed_receipts.unwrap_or(0);
        let a_risk = a.risk_score.unwrap_or(50);
        let b_risk = b.risk_score.unwrap_or(50);

        b_platform_match
            .cmp(&a_platform_match)
            .then_with(|| b.trust.trust_rank().cmp(&a.trust.trust_rank()))
            .then_with(|| b_verified.cmp(&a_verified))
            .then_with(|| b_receipts.cmp(&a_receipts))
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
            .then_with(|| a_failed.cmp(&b_failed))
            .then_with(|| a_risk.cmp(&b_risk))
    });
}

pub(crate) async fn fetch_community_capsule_tomls(
    source: &str,
) -> Result<Vec<CommunityCapsuleTomlCandidate>> {
    let client = community_discovery_client()?;
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
    let client = community_fetch_client()?;
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
    let client = community_fetch_client()?;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SourceValidationOutcome {
    Match,
    MissingSource,
    Mismatch {
        toml_source: String,
        expected_source: String,
    },
}

pub(crate) fn extract_toml_source(toml_content: &str) -> Option<String> {
    let parsed: TomlValue = toml::from_str(toml_content).ok()?;
    parsed
        .get("source")
        .and_then(|s| s.get("repository"))
        .and_then(|v| v.as_str().map(str::to_string))
        .or_else(|| {
            parsed
                .get("metadata")
                .and_then(|m| m.get("repository"))
                .and_then(|v| v.as_str().map(str::to_string))
        })
}

pub(crate) fn validate_capsule_toml_source_matches_run_target(
    toml_content: &str,
    normalized_source: &str,
) -> SourceValidationOutcome {
    match extract_toml_source(toml_content) {
        Some(src) if src == normalized_source => SourceValidationOutcome::Match,
        Some(src) => SourceValidationOutcome::Mismatch {
            toml_source: src,
            expected_source: normalized_source.to_string(),
        },
        None => SourceValidationOutcome::MissingSource,
    }
}

pub(crate) fn validate_capsule_toml_source_with_provenance(
    toml_content: &str,
    normalized_source: &str,
    provenance_source: &str,
) -> SourceValidationOutcome {
    if provenance_source != normalized_source {
        return SourceValidationOutcome::Mismatch {
            toml_source: provenance_source.to_string(),
            expected_source: normalized_source.to_string(),
        };
    }

    match extract_toml_source(toml_content) {
        Some(toml_src) if toml_src == normalized_source => SourceValidationOutcome::Match,
        Some(toml_src) => SourceValidationOutcome::Mismatch {
            toml_source: toml_src,
            expected_source: normalized_source.to_string(),
        },
        None => {
            debug!(
                %normalized_source,
                %provenance_source,
                "capsule.toml has no source.repository; using community candidate.source as provenance"
            );
            SourceValidationOutcome::Match
        }
    }
}

pub(crate) fn validate_candidate_source_matches_run_target(
    candidate_source: &str,
    normalized_source: &str,
) -> Result<()> {
    if candidate_source == normalized_source {
        Ok(())
    } else {
        bail!(
            "Community candidate source mismatch: candidate declares source '{}', \
             but run target is '{}'.",
            candidate_source,
            normalized_source
        )
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

    let mut meta_parts = vec![
        trust_label.to_string(),
        format!("★{}", candidate.stars),
        platforms,
        verified,
    ];

    if let Some(receipts) = candidate.successful_receipts {
        meta_parts.push(format!("{} successful receipts", receipts));
    }
    if let Some(verified) = candidate.current_platform_verified
        && verified
    {
        meta_parts.push("platform-verified".to_string());
    }

    let mut detail_parts = Vec::new();
    if let Some(source_ref) = &candidate.source_ref {
        detail_parts.push(format!("source: {}", source_ref));
    }
    if let Some(risk) = candidate.risk_score {
        let label = match risk {
            0..=33 => "low",
            34..=66 => "medium",
            _ => "high",
        };
        detail_parts.push(format!("risk: {}", label));
    }

    let permissions = if candidate.permissions_summary.is_empty() {
        "none".to_string()
    } else {
        candidate.permissions_summary.join(", ")
    };

    let mut lines = vec![format!(
        "{}. {}\n   {}",
        index + 1,
        candidate.title,
        meta_parts.join(" · "),
    )];
    if !detail_parts.is_empty() {
        lines.push(format!("   {}", detail_parts.join(" · ")));
    }
    lines.push(format!("   permissions: {}", permissions));

    lines.join("\n")
}

/// Called when the community API returns zero candidates for `source`.
/// In a TTY, offers the user a choice: continue with inference or cancel.
/// In non-TTY / JSON environments, proceeds to inference silently.
/// Returns `true` to continue with inference, `false` to cancel.
pub(crate) fn prompt_no_candidates_flow(source: &str) -> Result<bool> {
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return Ok(true);
    }

    eprintln!("\nNo community recipes found for {}.", source);
    eprintln!();
    eprintln!("1. Infer new capsule.toml from project structure");
    eprintln!("2. Cancel");
    eprintln!("   (To use an existing capsule.toml, re-run with -T ./path/to/capsule.toml)");
    eprintln!();

    loop {
        eprint!("Enter choice (1-2): ");
        use std::io::Write;
        let _ = std::io::stderr().flush();

        let mut input = String::new();
        std::io::stdin()
            .read_line(&mut input)
            .context("Failed to read selection")?;

        match input.trim() {
            "1" => return Ok(true),
            "2" => return Ok(false),
            _ => eprintln!("Invalid choice, try again."),
        }
    }
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
    eprintln!();

    loop {
        eprint!("Enter choice (1-{}): ", candidates.len() + 1);
        use std::io::Write;
        let _ = std::io::stderr().flush();

        let mut input = String::new();
        std::io::stdin()
            .read_line(&mut input)
            .context("Failed to read selection")?;

        let choice: usize = match input.trim().parse() {
            Ok(n) if n >= 1 && n <= candidates.len() + 1 => n,
            _ => {
                eprintln!("Invalid choice, try again.");
                continue;
            }
        };

        return Ok(choice);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Serialises access to ATO_COMMUNITY_API_URL across all tests in this
    // module (sync and async) so they don't race against each other. An
    // async-aware mutex so async tests can hold it across `.await` points
    // (clippy::await_holding_lock); sync tests use `blocking_lock()`.
    pub(super) static COMMUNITY_URL_MUTEX: tokio::sync::Mutex<()> =
        tokio::sync::Mutex::const_new(());

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
            {
                let mut c = minimal_candidate("1");
                c.stars = 10;
                c
            },
            {
                let mut c = minimal_candidate("2");
                c.stars = 5;
                c.platforms = vec![platform_display_name().to_string()];
                c
            },
        ];

        sort_candidates(&mut candidates);
        assert_eq!(candidates[0].id, "2");
    }

    #[test]
    fn candidate_sorting_prefers_higher_trust() {
        let mut candidates = vec![
            {
                let mut c = minimal_candidate("1");
                c.trust = CommunityTrustLevel::Owner;
                c.stars = 5;
                c
            },
            {
                let mut c = minimal_candidate("2");
                c.trust = CommunityTrustLevel::Official;
                c.stars = 1;
                c
            },
        ];

        sort_candidates(&mut candidates);
        assert_eq!(candidates[0].id, "2");
    }

    #[test]
    fn candidate_sorting_prefers_more_stars() {
        let mut candidates = vec![
            {
                let mut c = minimal_candidate("1");
                c.stars = 5;
                c
            },
            {
                let mut c = minimal_candidate("2");
                c.stars = 100;
                c
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
        assert_eq!(
            validate_capsule_toml_source_matches_run_target(toml, "github.com/owner/repo"),
            SourceValidationOutcome::Match
        );
    }

    #[test]
    fn source_identity_mismatch_fails() {
        let toml = r#"
            name = "test"
            version = "1.0.0"
            [source]
            repository = "github.com/other/repo"
        "#;
        assert_eq!(
            validate_capsule_toml_source_matches_run_target(toml, "github.com/owner/repo"),
            SourceValidationOutcome::Mismatch {
                toml_source: "github.com/other/repo".into(),
                expected_source: "github.com/owner/repo".into(),
            }
        );
    }

    #[test]
    fn source_identity_metadata_fallback() {
        let toml = r#"
            name = "test"
            version = "1.0.0"
            [metadata]
            repository = "github.com/owner/repo"
        "#;
        assert_eq!(
            validate_capsule_toml_source_matches_run_target(toml, "github.com/owner/repo"),
            SourceValidationOutcome::Match
        );
    }

    #[test]
    fn source_identity_missing_source_returns_missing() {
        let toml = r#"
            name = "test"
            version = "1.0.0"
        "#;
        assert_eq!(
            validate_capsule_toml_source_matches_run_target(toml, "github.com/owner/repo"),
            SourceValidationOutcome::MissingSource
        );
    }

    #[test]
    fn validate_with_provenance_uses_provenance_when_toml_missing() {
        let toml = r#"
            name = "test"
            version = "1.0.0"
        "#;
        assert_eq!(
            validate_capsule_toml_source_with_provenance(
                toml,
                "github.com/owner/repo",
                "github.com/owner/repo"
            ),
            SourceValidationOutcome::Match
        );
    }

    #[test]
    fn validate_with_provenance_rejects_provenance_mismatch() {
        let toml = r#"
            name = "test"
            version = "1.0.0"
        "#;
        assert_eq!(
            validate_capsule_toml_source_with_provenance(
                toml,
                "github.com/owner/repo",
                "github.com/wrong/repo"
            ),
            SourceValidationOutcome::Mismatch {
                toml_source: "github.com/wrong/repo".into(),
                expected_source: "github.com/owner/repo".into(),
            }
        );
    }

    #[test]
    fn validate_with_provenance_toml_source_overrides_provenance_on_mismatch() {
        let toml = r#"
            name = "test"
            version = "1.0.0"
            [source]
            repository = "github.com/different/repo"
        "#;
        assert_eq!(
            validate_capsule_toml_source_with_provenance(
                toml,
                "github.com/owner/repo",
                "github.com/owner/repo"
            ),
            SourceValidationOutcome::Mismatch {
                toml_source: "github.com/different/repo".into(),
                expected_source: "github.com/owner/repo".into(),
            }
        );
    }

    #[test]
    fn candidate_source_matches_run_target() {
        assert!(
            validate_candidate_source_matches_run_target(
                "github.com/owner/repo",
                "github.com/owner/repo"
            )
            .is_ok()
        );
    }

    #[test]
    fn candidate_source_mismatch_run_target_fails() {
        let result = validate_candidate_source_matches_run_target(
            "github.com/wrong/repo",
            "github.com/owner/repo",
        );
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Community candidate source mismatch")
        );
    }

    #[test]
    fn resolve_community_api_base_url_defaults() {
        let _g = COMMUNITY_URL_MUTEX.blocking_lock();
        let url = resolve_community_api_base_url();
        assert_eq!(url, "https://api.ato.run");
    }

    fn minimal_candidate(id: &str) -> CommunityCapsuleTomlCandidate {
        CommunityCapsuleTomlCandidate {
            id: id.to_string(),
            title: format!("Candidate {}", id),
            source: "github.com/a/b".to_string(),
            trust: CommunityTrustLevel::Community,
            stars: 0,
            platforms: vec![],
            last_verified_at: None,
            permissions_summary: vec![],
            capsule_toml_url: format!("https://{}.toml", id),
            revision: None,
            successful_receipts: None,
            failed_receipts: None,
            current_platform_verified: None,
            source_ref: None,
            source_ref_age_days: None,
            risk_score: None,
        }
    }

    #[test]
    fn candidate_without_new_fields_deserializes() {
        let json = r#"{
            "id": "abc123",
            "title": "Test Capsule",
            "source": "github.com/owner/repo",
            "trust": "community",
            "stars": 42,
            "platforms": ["macOS"],
            "lastVerifiedAt": "2025-01-15T10:30:00Z",
            "permissionsSummary": ["network"],
            "capsuleTomlUrl": "https://example.com/toml",
            "revision": "v1"
        }"#;
        let c: CommunityCapsuleTomlCandidate = serde_json::from_str(json).expect("deserialize");
        assert_eq!(c.id, "abc123");
        assert_eq!(c.successful_receipts, None);
        assert_eq!(c.failed_receipts, None);
        assert_eq!(c.current_platform_verified, None);
        assert_eq!(c.source_ref, None);
        assert_eq!(c.source_ref_age_days, None);
        assert_eq!(c.risk_score, None);
    }

    #[test]
    fn candidate_with_new_fields_deserializes() {
        let json = r#"{
            "id": "abc123",
            "title": "Test Capsule",
            "source": "github.com/owner/repo",
            "trust": "official",
            "stars": 124,
            "platforms": ["macOS", "Linux", "Windows"],
            "lastVerifiedAt": "2025-06-01T12:00:00Z",
            "permissionsSummary": ["network", "persistent state"],
            "capsuleTomlUrl": "https://example.com/toml",
            "revision": "v2",
            "successfulReceipts": 18,
            "failedReceipts": 1,
            "currentPlatformVerified": true,
            "sourceRef": "a1b2c3d",
            "sourceRefAgeDays": 14,
            "riskScore": 10
        }"#;
        let c: CommunityCapsuleTomlCandidate = serde_json::from_str(json).expect("deserialize");
        assert_eq!(c.successful_receipts, Some(18));
        assert_eq!(c.failed_receipts, Some(1));
        assert_eq!(c.current_platform_verified, Some(true));
        assert_eq!(c.source_ref, Some("a1b2c3d".to_string()));
        assert_eq!(c.source_ref_age_days, Some(14));
        assert_eq!(c.risk_score, Some(10));
    }

    #[test]
    fn sorting_prefers_current_platform_verified() {
        let mut candidates = vec![
            {
                let mut c = minimal_candidate("1");
                c.trust = CommunityTrustLevel::Official;
                c.stars = 100;
                c.current_platform_verified = Some(false);
                c
            },
            {
                let mut c = minimal_candidate("2");
                c.trust = CommunityTrustLevel::Official;
                c.stars = 100;
                c.current_platform_verified = Some(true);
                c
            },
        ];
        sort_candidates(&mut candidates);
        assert_eq!(candidates[0].id, "2");
    }

    #[test]
    fn sorting_penalizes_failed_receipts() {
        let mut candidates = vec![
            {
                let mut c = minimal_candidate("1");
                c.successful_receipts = Some(10);
                c.failed_receipts = Some(5);
                c
            },
            {
                let mut c = minimal_candidate("2");
                c.successful_receipts = Some(10);
                c.failed_receipts = Some(0);
                c
            },
        ];
        sort_candidates(&mut candidates);
        assert_eq!(candidates[0].id, "2");
    }

    #[test]
    fn sorting_penalizes_higher_risk_score() {
        let mut candidates = vec![
            {
                let mut c = minimal_candidate("1");
                c.successful_receipts = Some(10);
                c.failed_receipts = Some(0);
                c.risk_score = Some(80);
                c
            },
            {
                let mut c = minimal_candidate("2");
                c.successful_receipts = Some(10);
                c.failed_receipts = Some(0);
                c.risk_score = Some(10);
                c
            },
        ];
        sort_candidates(&mut candidates);
        assert_eq!(candidates[0].id, "2");
    }

    #[test]
    fn display_omits_unknown_fields() {
        let c = minimal_candidate("x");
        let output = format_candidate_for_display(&c, 0);
        assert!(!output.contains("source:"));
        assert!(!output.contains("risk:"));
        assert!(!output.contains("receipts"));
        assert!(!output.contains("platform-verified"));
    }

    #[test]
    fn display_includes_receipt_risk_and_source_ref() {
        let mut c = minimal_candidate("x");
        c.successful_receipts = Some(18);
        c.source_ref = Some("a1b2c3d".to_string());
        c.risk_score = Some(10);
        c.current_platform_verified = Some(true);
        let output = format_candidate_for_display(&c, 0);
        assert!(output.contains("18 successful receipts"));
        assert!(output.contains("source: a1b2c3d"));
        assert!(output.contains("risk: low"));
        assert!(output.contains("platform-verified"));
    }

    #[test]
    fn display_risk_labels() {
        let mut low = minimal_candidate("low");
        low.risk_score = Some(20);
        assert!(format_candidate_for_display(&low, 0).contains("risk: low"));

        let mut med = minimal_candidate("med");
        med.risk_score = Some(50);
        assert!(format_candidate_for_display(&med, 0).contains("risk: medium"));

        let mut high = minimal_candidate("high");
        high.risk_score = Some(90);
        assert!(format_candidate_for_display(&high, 0).contains("risk: high"));
    }

    #[test]
    fn sorting_none_risk_below_low_risk() {
        let mut candidates = vec![
            {
                let mut c = minimal_candidate("none");
                c.risk_score = None;
                c
            },
            {
                let mut c = minimal_candidate("low");
                c.risk_score = Some(10);
                c
            },
        ];
        sort_candidates(&mut candidates);
        assert_eq!(candidates[0].id, "low");
    }

    #[test]
    fn sorting_none_risk_above_high_risk() {
        let mut candidates = vec![
            {
                let mut c = minimal_candidate("high");
                c.risk_score = Some(80);
                c
            },
            {
                let mut c = minimal_candidate("none");
                c.risk_score = None;
                c
            },
        ];
        sort_candidates(&mut candidates);
        assert_eq!(candidates[0].id, "none");
    }

    // ── additional -T / source-identity regression coverage ─────────────────

    #[test]
    fn extract_toml_source_returns_none_for_empty_toml() {
        assert_eq!(extract_toml_source(""), None);
    }

    #[test]
    fn extract_toml_source_returns_none_when_no_repository_key() {
        let toml = r#"name = "foo"
[source]
branch = "main"
"#;
        assert_eq!(extract_toml_source(toml), None);
    }

    #[test]
    fn validate_local_t_source_match_ok() {
        let toml = "[source]\nrepository = \"github.com/o/r\"\n";
        assert_eq!(
            validate_capsule_toml_source_matches_run_target(toml, "github.com/o/r"),
            SourceValidationOutcome::Match
        );
    }

    #[test]
    fn validate_local_t_source_mismatch_is_mismatch() {
        let toml = "[source]\nrepository = \"github.com/o/r\"\n";
        assert!(matches!(
            validate_capsule_toml_source_matches_run_target(toml, "github.com/other/repo"),
            SourceValidationOutcome::Mismatch { .. }
        ));
    }

    #[test]
    fn validate_remote_t_missing_source_is_missing() {
        let toml = "name = \"foo\"\n";
        assert_eq!(
            validate_capsule_toml_source_matches_run_target(toml, "github.com/o/r"),
            SourceValidationOutcome::MissingSource
        );
    }

    #[test]
    fn validate_candidate_source_mismatch_error_message_contains_both_sources() {
        let err = validate_candidate_source_matches_run_target("github.com/a/b", "github.com/c/d")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("github.com/a/b"),
            "expected old source in error"
        );
        assert!(
            err.contains("github.com/c/d"),
            "expected run target in error"
        );
    }

    #[test]
    fn no_candidates_flow_returns_true_in_non_tty() {
        // In a non-TTY test environment stdin/stderr are not terminals,
        // so prompt_no_candidates_flow must return Ok(true) without prompting.
        let result = prompt_no_candidates_flow("github.com/a/b").expect("should not fail");
        assert!(result, "non-TTY must proceed to inference");
    }

    // ── async / mock-HTTP tests ──────────────────────────────────────────────
    mod async_tests {
        use super::super::*;
        use super::COMMUNITY_URL_MUTEX;

        /// Starts a minimal inline HTTP server that serves exactly one response
        /// and returns its base URL.  Uses only tokio primitives — no extra deps.
        async fn mock_http(status: u16, body: impl Into<String> + Send + 'static) -> String {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            use tokio::net::TcpListener;

            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let body = body.into();
            tokio::spawn(async move {
                if let Ok((mut stream, _)) = listener.accept().await {
                    let mut buf = vec![0u8; 4096];
                    let _ = stream.read(&mut buf).await;
                    let reason = match status {
                        200 => "OK",
                        404 => "Not Found",
                        500 => "Internal Server Error",
                        _ => "Unknown",
                    };
                    let resp = format!(
                        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(resp.as_bytes()).await;
                }
            });
            format!("http://127.0.0.1:{port}")
        }

        #[tokio::test]
        async fn fetch_candidates_parses_multiple_candidates() {
            {
                let _g = COMMUNITY_URL_MUTEX.lock().await;
                let body = r#"{"candidates":[
                {"id":"c1","title":"Recipe One","source":"github.com/a/b","trust":"community","stars":10,"platforms":["macOS"],"lastVerifiedAt":null,"permissionsSummary":["network"],"capsuleTomlUrl":"http://x/c1","revision":null},
                {"id":"c2","title":"Recipe Two","source":"github.com/a/b","trust":"owner","stars":5,"platforms":[],"lastVerifiedAt":null,"permissionsSummary":[],"capsuleTomlUrl":"http://x/c2","revision":null}
            ]}"#;
                let base = mock_http(200, body).await;
                unsafe {
                    std::env::set_var("ATO_COMMUNITY_API_URL", &base);
                }
                let result = fetch_community_capsule_tomls("github.com/a/b").await;
                unsafe {
                    std::env::remove_var("ATO_COMMUNITY_API_URL");
                }
                let candidates = result.expect("fetch should succeed");
                assert_eq!(candidates.len(), 2);
                assert_eq!(candidates[0].id, "c1");
                assert_eq!(candidates[1].id, "c2");
                assert_eq!(candidates[0].permissions_summary, vec!["network"]);
            }
        }

        #[tokio::test]
        async fn fetch_candidates_404_returns_empty_vec() {
            {
                let _g = COMMUNITY_URL_MUTEX.lock().await;
                let base = mock_http(404, r#"{"error":"not found"}"#).await;
                unsafe {
                    std::env::set_var("ATO_COMMUNITY_API_URL", &base);
                }
                let result = fetch_community_capsule_tomls("github.com/unknown/repo").await;
                unsafe {
                    std::env::remove_var("ATO_COMMUNITY_API_URL");
                }
                let candidates = result.expect("404 must return empty vec, not error");
                assert!(candidates.is_empty());
            }
        }

        #[tokio::test]
        async fn fetch_candidates_empty_array_returns_empty_vec() {
            {
                let _g = COMMUNITY_URL_MUTEX.lock().await;
                let base = mock_http(200, r#"{"candidates":[]}"#).await;
                unsafe {
                    std::env::set_var("ATO_COMMUNITY_API_URL", &base);
                }
                let result = fetch_community_capsule_tomls("github.com/a/b").await;
                unsafe {
                    std::env::remove_var("ATO_COMMUNITY_API_URL");
                }
                let candidates = result.expect("empty candidates list is ok");
                assert!(candidates.is_empty());
            }
        }

        #[tokio::test]
        async fn fetch_candidates_500_returns_error() {
            {
                let _g = COMMUNITY_URL_MUTEX.lock().await;
                let base = mock_http(500, r#"{"error":"internal"}"#).await;
                unsafe {
                    std::env::set_var("ATO_COMMUNITY_API_URL", &base);
                }
                let result = fetch_community_capsule_tomls("github.com/a/b").await;
                unsafe {
                    std::env::remove_var("ATO_COMMUNITY_API_URL");
                }
                assert!(result.is_err(), "non-2xx (excluding 404) must return error");
                let msg = result.unwrap_err().to_string();
                assert!(
                    msg.contains("Failed to fetch community capsule.tomls")
                        || msg.contains("status=500"),
                    "unexpected error message: {msg}"
                );
            }
        }

        #[tokio::test]
        async fn fetch_candidates_invalid_json_returns_error() {
            {
                let _g = COMMUNITY_URL_MUTEX.lock().await;
                let base = mock_http(200, r#"not valid json at all"#).await;
                unsafe {
                    std::env::set_var("ATO_COMMUNITY_API_URL", &base);
                }
                let result = fetch_community_capsule_tomls("github.com/a/b").await;
                unsafe {
                    std::env::remove_var("ATO_COMMUNITY_API_URL");
                }
                assert!(result.is_err(), "invalid JSON must return error");
            }
        }

        #[tokio::test]
        async fn fetch_capsule_toml_by_id_returns_content() {
            {
                let _g = COMMUNITY_URL_MUTEX.lock().await;
                let toml_body = "[source]\nrepository = \"github.com/a/b\"\n";
                let base = mock_http(200, toml_body).await;
                unsafe {
                    std::env::set_var("ATO_COMMUNITY_API_URL", &base);
                }
                let result = fetch_capsule_toml_by_id("some-id").await;
                unsafe {
                    std::env::remove_var("ATO_COMMUNITY_API_URL");
                }
                let content = result.expect("fetch by id should succeed");
                assert!(content.contains("[source]"));
                assert!(content.contains("github.com/a/b"));
            }
        }

        #[tokio::test]
        async fn fetch_capsule_toml_by_id_non_2xx_returns_error() {
            {
                let _g = COMMUNITY_URL_MUTEX.lock().await;
                let base = mock_http(404, r#"not found"#).await;
                unsafe {
                    std::env::set_var("ATO_COMMUNITY_API_URL", &base);
                }
                let result = fetch_capsule_toml_by_id("missing-id").await;
                unsafe {
                    std::env::remove_var("ATO_COMMUNITY_API_URL");
                }
                assert!(
                    result.is_err(),
                    "non-2xx from capsule-tomls/<id> must error"
                );
            }
        }

        #[tokio::test]
        async fn fetch_toml_from_url_returns_content() {
            // fetch_toml_from_url uses a direct URL, not the community API base URL,
            // so no ATO_COMMUNITY_API_URL override needed.
            let toml_body = "[source]\nrepository = \"github.com/a/b\"\n";
            let base = mock_http(200, toml_body).await;
            let url = format!("{}/capsule.toml", base);
            let result = fetch_toml_from_url(&url).await;
            let content = result.expect("fetch_toml_from_url should succeed");
            assert!(content.contains("[source]"));
        }

        #[tokio::test]
        async fn fetch_toml_from_url_non_2xx_returns_error() {
            let base = mock_http(403, r#"forbidden"#).await;
            let url = format!("{}/capsule.toml", base);
            let result = fetch_toml_from_url(&url).await;
            assert!(result.is_err());
            let msg = result.unwrap_err().to_string();
            assert!(
                msg.contains("Failed to fetch capsule.toml from URL") || msg.contains("status=403"),
                "unexpected message: {msg}"
            );
        }

        #[tokio::test]
        async fn community_api_url_env_override_is_respected() {
            {
                let _g = COMMUNITY_URL_MUTEX.lock().await;
                // Verify that ATO_COMMUNITY_API_URL override actually routes requests
                // to the specified server.  If the override is ignored the request would
                // go to https://api.ato.run and fail / timeout in CI.
                let base = mock_http(200, r#"{"candidates":[]}"#).await;
                unsafe {
                    std::env::set_var("ATO_COMMUNITY_API_URL", &base);
                }
                let resolved = resolve_community_api_base_url();
                unsafe {
                    std::env::remove_var("ATO_COMMUNITY_API_URL");
                }
                assert_eq!(resolved, base);
            }
        }
    } // end mod async_tests
} // end mod tests
