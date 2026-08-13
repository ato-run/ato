//! Desktop-side client for the ato-api community `capsule-tomls`
//! discovery endpoint.
//!
//! This is the read-only, **unauthenticated** counterpart to the
//! `ato-cli` community discovery path
//! (`crates/cli/src/community/capsule_toml.rs`). The Featured Apps
//! Community Import surface queries `GET /v1/capsule-tomls?source=<owner/repo>`
//! to list the published community recipes for a source so the user can
//! review and explicitly pick one — rather than letting the CLI silently
//! infer a recipe during `session start`.
//!
//! The endpoint requires no session token (it is the same one the CLI
//! hits with a bare `User-Agent`), so unlike `source_import_api` there is
//! no auth handoff here. Calls are blocking (`ureq`) and MUST run on a
//! background executor — never the GPUI main thread.

use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};

const DEFAULT_COMMUNITY_API_URL: &str = "https://api.ato.run";
const ENV_COMMUNITY_API_URL: &str = "ATO_COMMUNITY_API_URL";
const TIMEOUT_SECS: u64 = 8;

/// Resolve the community API base URL, honouring the `ATO_COMMUNITY_API_URL`
/// override so staging / local API environments work without code changes.
/// Mirrors `ato-cli`'s `resolve_community_api_base_url` exactly.
pub fn resolve_community_api_base_url() -> String {
    std::env::var(ENV_COMMUNITY_API_URL)
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_COMMUNITY_API_URL.to_string())
}

/// Normalize a source string to the API-stored form (`owner/repo`).
///
/// Accepts any of:
///   - `github.com/owner/repo`
///   - `capsule://github.com/owner/repo`
///   - `https://github.com/owner/repo`
///   - `owner/repo` (already normalized)
///
/// Strips the scheme/host prefixes and trailing slashes. Mirrors the
/// CLI's `normalize_expected_source` so a Desktop lookup hits the same
/// registry row the CLI would.
pub fn normalize_source(source: &str) -> String {
    let trimmed = source.trim();
    let without_scheme = trimmed
        .strip_prefix("capsule://")
        .or_else(|| trimmed.strip_prefix("https://"))
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    let without_host = without_scheme
        .strip_prefix("github.com/")
        .unwrap_or(without_scheme);
    without_host.trim_end_matches('/').to_string()
}

/// A single community capsule.toml candidate, projected to the fields the
/// Community Import review surface displays. The `id` (`ctoml_xxx`) is the
/// pre-selected recipe identity threaded into `session start
/// --community-toml-id`.
///
/// Field set is a subset of the CLI's `CommunityCapsuleTomlCandidate`; the
/// API returns camelCase keys.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CommunityCandidate {
    pub id: String,
    pub title: String,
    pub source: String,
    /// `community` | `owner` | `official`. Kept as a raw string for the
    /// review UI; the surface does not need the typed trust ranking the
    /// CLI uses for auto-selection.
    #[serde(default)]
    pub trust: String,
    #[serde(default)]
    pub stars: u64,
    #[serde(default)]
    pub platforms: Vec<String>,
    #[serde(default)]
    pub permissions_summary: Vec<String>,
    #[serde(default)]
    pub last_verified_at: Option<String>,
    #[serde(default)]
    pub successful_receipts: Option<u64>,
    #[serde(default)]
    pub failed_receipts: Option<u64>,
    #[serde(default)]
    pub current_platform_verified: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCandidatesResponse {
    #[serde(default)]
    candidates: Vec<CommunityCandidate>,
}

/// Fetch the published community recipes for `source` (any handle form).
///
/// Returns an empty vector when the registry has no candidates (HTTP 404
/// or an empty `candidates` array) — the caller renders an explicit
/// "no community recipe" state rather than falling back to inference.
///
/// Blocking; call from a background executor.
pub fn fetch_candidates(source: &str) -> Result<Vec<CommunityCandidate>> {
    let normalized = normalize_source(source);
    if normalized.is_empty() {
        bail!("community discovery requires a non-empty source");
    }
    let endpoint = format!(
        "{}/v1/capsule-tomls?source={}",
        resolve_community_api_base_url(),
        urlencode(&normalized)
    );
    tracing::debug!(%endpoint, "community_api: fetching candidates");

    match ureq::get(&endpoint)
        .timeout(Duration::from_secs(TIMEOUT_SECS))
        .set("User-Agent", "ato-desktop")
        .set("Accept", "application/json")
        .call()
    {
        Ok(resp) => {
            let parsed: RawCandidatesResponse = resp
                .into_json()
                .map_err(|e| anyhow!("community_api: invalid JSON from {endpoint}: {e}"))?;
            Ok(parsed.candidates)
        }
        // 404 = no candidates registered for this source. Not an error.
        Err(ureq::Error::Status(404, _)) => Ok(Vec::new()),
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            bail!(
                "community_api: {endpoint} returned HTTP {code}: {}",
                body.lines().take(3).collect::<Vec<_>>().join(" ")
            )
        }
        Err(other) => Err(anyhow!(
            "community_api: request to {endpoint} failed: {other}"
        )),
    }
}

/// Fetch the raw `capsule.toml` for a single community recipe by its
/// `ctoml_…` id. The detail endpoint (`GET /v1/capsule-tomls/{id}`) returns
/// the TOML body directly (not a JSON envelope), so callers can show the user
/// exactly what they're about to launch — the only reliable way to tell two
/// same-titled community recipes apart.
///
/// Blocking; call from a background executor.
pub fn fetch_candidate_toml(id: &str) -> Result<String> {
    // The id is interpolated into the URL path, so require the canonical
    // `ctoml_<alnum>` shape (prefix + ASCII alphanumerics/underscore only)
    // before building the request — this both matches the documented format
    // and blocks path traversal / injection.
    let trimmed = id.trim();
    if !trimmed.starts_with("ctoml_")
        || !trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        bail!("invalid community recipe id");
    }
    let endpoint = format!(
        "{}/v1/capsule-tomls/{}",
        resolve_community_api_base_url(),
        trimmed
    );
    tracing::debug!(%endpoint, "community_api: fetching recipe toml");

    match ureq::get(&endpoint)
        .timeout(Duration::from_secs(TIMEOUT_SECS))
        .set("User-Agent", "ato-desktop")
        .set("Accept", "text/plain, application/toml, */*")
        .call()
    {
        Ok(resp) => resp
            .into_string()
            .map_err(|e| anyhow!("community_api: could not read recipe body from {endpoint}: {e}")),
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            bail!(
                "community_api: {endpoint} returned HTTP {code}: {}",
                body.lines().take(3).collect::<Vec<_>>().join(" ")
            )
        }
        Err(other) => Err(anyhow!(
            "community_api: request to {endpoint} failed: {other}"
        )),
    }
}

/// Percent-encode a query-parameter value. The `source` is `owner/repo`,
/// so only `/` and a handful of reserved characters need escaping; the
/// `percent-encoding` crate (already a dependency) handles the rest.
fn urlencode(value: &str) -> String {
    use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_capsule_scheme_and_host() {
        assert_eq!(
            normalize_source("capsule://github.com/excalidraw/excalidraw"),
            "excalidraw/excalidraw"
        );
    }

    #[test]
    fn normalize_strips_github_host() {
        assert_eq!(
            normalize_source("github.com/toeverything/AFFiNE"),
            "toeverything/AFFiNE"
        );
    }

    #[test]
    fn normalize_strips_https_scheme_and_trailing_slash() {
        assert_eq!(
            normalize_source("https://github.com/open-webui/open-webui/"),
            "open-webui/open-webui"
        );
    }

    #[test]
    fn normalize_passes_through_already_normalized() {
        assert_eq!(
            normalize_source("excalidraw/excalidraw"),
            "excalidraw/excalidraw"
        );
    }

    #[test]
    fn urlencode_escapes_slash() {
        assert_eq!(urlencode("owner/repo"), "owner%2Frepo");
    }

    #[test]
    fn fetch_candidate_toml_rejects_path_injection_ids() {
        // Anything outside `[A-Za-z0-9_]` must be refused before a request is
        // built, so a crafted id can't traverse or escape the path.
        for bad in [
            "",
            "  ",
            "ctoml/../admin",
            "ctoml_abc/extra",
            "ctoml abc",
            "session_01k", // wrong prefix
            "abc123",      // no ctoml_ prefix
        ] {
            assert!(
                fetch_candidate_toml(bad).is_err(),
                "expected rejection for {bad:?}"
            );
        }
    }

    #[test]
    #[serial_test::serial]
    fn base_url_honours_env_override() {
        // Mutating a process-global env var: snapshot and restore so this
        // test can't pollute others. `#[serial]` keeps it off other tests
        // that touch the same var.
        let original = std::env::var(ENV_COMMUNITY_API_URL).ok();

        // Default when unset.
        unsafe {
            std::env::remove_var(ENV_COMMUNITY_API_URL);
        }
        assert_eq!(resolve_community_api_base_url(), DEFAULT_COMMUNITY_API_URL);

        // Honours the override, trimming whitespace and trailing slashes.
        unsafe {
            std::env::set_var(ENV_COMMUNITY_API_URL, "  https://staging.example/api/  ");
        }
        assert_eq!(
            resolve_community_api_base_url(),
            "https://staging.example/api"
        );

        // A blank override falls back to the default.
        unsafe {
            std::env::set_var(ENV_COMMUNITY_API_URL, "   ");
        }
        assert_eq!(resolve_community_api_base_url(), DEFAULT_COMMUNITY_API_URL);

        // Restore the pre-test environment.
        unsafe {
            match original {
                Some(v) => std::env::set_var(ENV_COMMUNITY_API_URL, v),
                None => std::env::remove_var(ENV_COMMUNITY_API_URL),
            }
        }
    }

    #[test]
    fn candidate_parses_camelcase_api_shape() {
        let json = r#"{
            "candidates": [{
                "id": "ctoml_01ksza4np2yrs1mqe7jz10ep1g",
                "title": "Excalidraw",
                "source": "excalidraw/excalidraw",
                "trust": "official",
                "stars": 1600,
                "platforms": ["linux/amd64"],
                "permissionsSummary": ["network: outbound"],
                "lastVerifiedAt": "2026-06-01T00:00:00Z",
                "successfulReceipts": 12,
                "currentPlatformVerified": true
            }]
        }"#;
        let parsed: RawCandidatesResponse = serde_json::from_str(json).expect("parses");
        assert_eq!(parsed.candidates.len(), 1);
        let c = &parsed.candidates[0];
        assert_eq!(c.id, "ctoml_01ksza4np2yrs1mqe7jz10ep1g");
        assert_eq!(c.title, "Excalidraw");
        assert_eq!(c.trust, "official");
        assert_eq!(c.successful_receipts, Some(12));
        assert_eq!(c.current_platform_verified, Some(true));
    }

    #[test]
    fn empty_candidates_parse_to_empty_vec() {
        let parsed: RawCandidatesResponse =
            serde_json::from_str(r#"{"candidates": []}"#).expect("parses");
        assert!(parsed.candidates.is_empty());
    }
}
