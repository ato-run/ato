//! HTTP client for the ato-api `source-imports` endpoints.
//!
//! Auth handoff: Desktop does not own the session token. We spawn
//! `ato desktop-auth-handoff` which prints a JSON envelope of the form
//! `{ session_token, publisher_handle?, site_base_url, api_base_url }`
//! when the user is signed in, or exits non-zero otherwise. This is
//! the same boundary ato-desktop already crosses for the Login flow.
//!
//! Requests use the blocking `ureq` client that ato-desktop already
//! depends on. All calls are intended to be invoked from a background
//! executor task; the dispatch layer is responsible for not blocking
//! the GPUI main thread.

use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ureq;

use crate::orchestrator::resolve_ato_binary;
use crate::source_import_session::{ImportRecipe, ImportRun, ImportSource};

const TIMEOUT_SECS: u64 = 30;

/// Cached credentials for a single import session. Resolved once via
/// `discover()` and reused for the lifetime of the session.
#[derive(Debug, Clone)]
pub(crate) struct ApiCreds {
    pub(crate) session_token: String,
    pub(crate) api_base_url: String,
}

#[derive(Debug, Deserialize)]
struct AuthHandoffResponse {
    session_token: String,
    api_base_url: String,
    #[allow(dead_code)]
    #[serde(default)]
    publisher_handle: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    site_base_url: Option<String>,
}

/// Discover the current user's session token and API base URL by
/// spawning `ato desktop-auth-handoff`. Returns `None` if the user is
/// not signed in (any failure path — missing token, expired session,
/// CLI not found, etc.).
pub(crate) fn discover() -> Option<ApiCreds> {
    let ato_bin = match resolve_ato_binary() {
        Ok(path) => path,
        Err(error) => {
            tracing::debug!(?error, "ato-import api: resolve_ato_binary failed");
            return None;
        }
    };
    let output = Command::new(&ato_bin)
        .arg("desktop-auth-handoff")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .ok()?;
    if !output.status.success() {
        tracing::debug!(
            status = ?output.status,
            stderr = %String::from_utf8_lossy(&output.stderr).lines().next().unwrap_or(""),
            "ato-import api: desktop-auth-handoff exited non-zero (user is signed out)"
        );
        return None;
    }
    let parsed: AuthHandoffResponse = match serde_json::from_slice(&output.stdout) {
        Ok(v) => v,
        Err(error) => {
            tracing::warn!(?error, "ato-import api: desktop-auth-handoff returned unparseable JSON");
            return None;
        }
    };
    if parsed.session_token.is_empty() || parsed.api_base_url.is_empty() {
        return None;
    }
    Some(ApiCreds {
        session_token: parsed.session_token,
        api_base_url: parsed.api_base_url.trim_end_matches('/').to_string(),
    })
}

/// HTTP client for the source-imports endpoints. Holds the resolved
/// credentials; construction does not perform any network I/O.
#[derive(Debug, Clone)]
pub(crate) struct ApiClient {
    creds: ApiCreds,
    agent: ureq::Agent,
}

impl ApiClient {
    pub(crate) fn new(creds: ApiCreds) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(TIMEOUT_SECS))
            .timeout_read(Duration::from_secs(TIMEOUT_SECS))
            .build();
        Self { creds, agent }
    }

    /// POST /v1/source-imports — create or dedup a source import record.
    /// Returns the assigned `source_import_id`.
    pub(crate) fn create_source_import(&self, source: &ImportSource) -> Result<String> {
        let url = format!("{}/v1/source-imports", self.creds.api_base_url);
        let body = json!({
            "source": {
                "repo_namespace": optional(&source.repo_namespace),
                "repo_name": optional(&source.repo_name),
                "source_url_normalized": source.source_url_normalized,
                "revision_id": source.revision_id,
                "source_tree_hash": source.source_tree_hash,
                "subdir": source.subdir,
            },
            "import_source": "desktop",
        });
        let response: CreateImportResponse = self.post_json(&url, &body)?;
        Ok(response.source_import.id)
    }

    /// POST /v1/source-imports/:id/attempt — record a run attempt.
    ///
    /// `import_status` is "inferred" before any run, "verified" on
    /// success, "failed" on failure. Pass `None` for source_import_id
    /// will cause this to bail; the caller should gate on `signed_in`
    /// + a present id before calling.
    pub(crate) fn record_attempt(
        &self,
        source_import_id: &str,
        status: AttemptStatus,
        run: &ImportRun,
    ) -> Result<()> {
        let url = format!(
            "{}/v1/source-imports/{}/attempt",
            self.creds.api_base_url, source_import_id
        );
        let body = json!({
            "import_status": status.as_str(),
            "error_class": run.error_class,
            "error_excerpt": run.error_excerpt,
        });
        // Server returns the updated row; we don't need to read it
        // for PR-3 but we still want to surface a non-2xx as an error.
        let _: AttemptResponse = self.post_json(&url, &body)?;
        Ok(())
    }

    /// POST /v1/source-imports/:id/submit-working-recipe.
    pub(crate) fn submit_working_recipe(
        &self,
        source_import_id: &str,
        recipe: &ImportRecipe,
        editable_recipe_toml: &str,
    ) -> Result<()> {
        let url = format!(
            "{}/v1/source-imports/{}/submit-working-recipe",
            self.creds.api_base_url, source_import_id
        );
        let body = json!({
            "recipe_toml": editable_recipe_toml,
            "origin": recipe.origin,
            "target_label": recipe.target_label,
            "platform_os": optional(&recipe.platform_os),
            "platform_arch": optional(&recipe.platform_arch),
        });
        let _: SubmitResponse = self.post_json(&url, &body)?;
        Ok(())
    }

    fn post_json<T: for<'de> Deserialize<'de>>(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<T> {
        let response = self
            .agent
            .post(url)
            .set("Authorization", &format!("Bearer {}", self.creds.session_token))
            .set("Content-Type", "application/json")
            .set("Accept", "application/json")
            .send_string(&serde_json::to_string(body).context("serialize request body")?);
        match response {
            Ok(resp) => parse_success(resp),
            Err(ureq::Error::Status(code, resp)) => {
                let body = resp.into_string().unwrap_or_default();
                bail!("ato-api {url} returned HTTP {code}: {}", head_lines(&body, 5))
            }
            Err(other) => Err(anyhow!("ato-api {url} request failed: {other}")),
        }
    }
}

fn parse_success<T: for<'de> Deserialize<'de>>(resp: ureq::Response) -> Result<T> {
    let url = resp.get_url().to_string();
    let text = resp.into_string().context("read response body")?;
    serde_json::from_str::<T>(&text).with_context(|| {
        format!(
            "ato-api {url} returned invalid JSON (first 200 chars): {}",
            text.chars().take(200).collect::<String>()
        )
    })
}

fn optional(s: &str) -> Option<&str> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn head_lines(text: &str, n: usize) -> String {
    text.lines().take(n).collect::<Vec<_>>().join("\n")
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum AttemptStatus {
    Inferred,
    Running,
    Verified,
    Failed,
}

impl AttemptStatus {
    fn as_str(self) -> &'static str {
        match self {
            AttemptStatus::Inferred => "inferred",
            AttemptStatus::Running => "running",
            AttemptStatus::Verified => "verified",
            AttemptStatus::Failed => "failed",
        }
    }
}

// Response mirror types — we only deserialize fields we actually use.

#[derive(Debug, Deserialize)]
struct CreateImportResponse {
    source_import: SourceImportRow,
}

#[derive(Debug, Deserialize)]
struct AttemptResponse {
    #[serde(default)]
    #[allow(dead_code)]
    source_import: Option<SourceImportRow>,
}

#[derive(Debug, Deserialize)]
struct SubmitResponse {
    #[serde(default)]
    #[allow(dead_code)]
    source_import: Option<SourceImportRow>,
}

#[derive(Debug, Deserialize, Serialize)]
struct SourceImportRow {
    id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attempt_status_strings() {
        assert_eq!(AttemptStatus::Inferred.as_str(), "inferred");
        assert_eq!(AttemptStatus::Running.as_str(), "running");
        assert_eq!(AttemptStatus::Verified.as_str(), "verified");
        assert_eq!(AttemptStatus::Failed.as_str(), "failed");
    }

    #[test]
    fn create_response_parses_minimal_shape() {
        let json = r#"{
            "source_import": {"id": "si_abc123"},
            "source_snapshot": {"id": "ss_xyz", "anything": true},
            "deduped": false
        }"#;
        let parsed: CreateImportResponse = serde_json::from_str(json).expect("parses");
        assert_eq!(parsed.source_import.id, "si_abc123");
    }

    #[test]
    fn attempt_response_parses_with_or_without_row() {
        let with_row: AttemptResponse = serde_json::from_str(
            r#"{"source_import": {"id": "si_abc"}}"#,
        )
        .expect("with row");
        assert!(with_row.source_import.is_some());

        let empty: AttemptResponse = serde_json::from_str(r#"{}"#).expect("empty");
        assert!(empty.source_import.is_none());
    }

    #[test]
    fn optional_returns_none_for_empty() {
        assert_eq!(optional(""), None);
        assert_eq!(optional("x"), Some("x"));
    }

    #[test]
    fn auth_handoff_response_parses_real_shape() {
        let json = r#"{
            "session_token": "abc.def.ghi",
            "publisher_handle": "alice",
            "site_base_url": "https://ato.run",
            "api_base_url": "https://api.ato.run"
        }"#;
        let parsed: AuthHandoffResponse = serde_json::from_str(json).expect("parses");
        assert_eq!(parsed.session_token, "abc.def.ghi");
        assert_eq!(parsed.api_base_url, "https://api.ato.run");
    }

    #[test]
    fn auth_handoff_response_parses_without_optional_fields() {
        let json = r#"{
            "session_token": "abc.def.ghi",
            "api_base_url": "https://api.ato.run"
        }"#;
        let parsed: AuthHandoffResponse = serde_json::from_str(json).expect("parses");
        assert!(parsed.publisher_handle.is_none());
        assert!(parsed.site_base_url.is_none());
    }
}
