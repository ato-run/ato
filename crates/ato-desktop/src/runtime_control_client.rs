//! Synchronous HTTP client for the Runtime Control API exposed by `ato serve`.
//!
//! The Runtime Control API is bound to loopback (`127.0.0.1`) so no
//! Bearer token is required for loopback callers.  All methods are
//! blocking and must be called from a GPUI background-executor task,
//! never from the render thread.
//!
//! Callers that only need to *read* session state or install-profile lists
//! can call the API directly from JS via the `runtime_base_url` field
//! injected into `StartSnapshot`.  The methods here are for *write*
//! operations (launch / stop) that go through the Desktop IPC bridge.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// HTTP client for the `ato serve` Runtime Control API.
pub(crate) struct RuntimeControlClient {
    base_url: String,
}

impl RuntimeControlClient {
    /// Construct a client pointing at `http://127.0.0.1:<port>`.
    pub(crate) fn new(port: u16) -> Self {
        Self {
            base_url: format!("http://127.0.0.1:{port}"),
        }
    }

    /// The base URL for direct JS fetch calls (e.g. for SSE log streaming).
    pub(crate) fn base_url(&self) -> &str {
        &self.base_url
    }

    /// `POST /v1/runtime/sessions` — launch a session for an installed profile.
    ///
    /// Returns `(session_id, user_visible_url)`.  `user_visible_url` is `None`
    /// when the launched capsule does not expose an HTTP frontend.
    pub(crate) fn launch_session(
        &self,
        install_profile_key: &str,
        target_label: Option<&str>,
    ) -> Result<LaunchSessionResponse> {
        let body = LaunchSessionRequest {
            install_profile_key: install_profile_key.to_string(),
            target_label: target_label.map(|s| s.to_string()),
        };
        let url = format!("{}/v1/runtime/sessions", self.base_url);
        let body_value = serde_json::to_value(&body).context("serialise LaunchSessionRequest")?;
        let response = ureq::post(&url)
            .set("Content-Type", "application/json")
            .send_json(body_value)
            .map_err(|err| match err {
                ureq::Error::Status(status, resp) => {
                    let body = resp.into_string().unwrap_or_default();
                    anyhow::anyhow!(
                        "POST /v1/runtime/sessions returned HTTP {status}: {body}"
                    )
                }
                other => anyhow::Error::new(other).context("POST /v1/runtime/sessions"),
            })?;
        if response.status() != 201 {
            bail!(
                "POST /v1/runtime/sessions returned unexpected HTTP {}",
                response.status()
            );
        }
        let result: LaunchSessionResponse =
            response.into_json().context("parse LaunchSessionResponse")?;
        Ok(result)
    }

    /// `DELETE /v1/runtime/sessions/:id` — stop a running session.
    ///
    /// Returns `Ok(())` on success (204) and on 404 (session already gone).
    pub(crate) fn stop_session(&self, session_id: &str) -> Result<()> {
        let url = format!("{}/v1/runtime/sessions/{session_id}", self.base_url);
        match ureq::delete(&url).call() {
            Ok(_) => Ok(()),
            Err(ureq::Error::Status(404, _)) => {
                tracing::debug!(session_id, "stop_session: session already gone (404)");
                Ok(())
            }
            Err(err) => Err(anyhow::Error::new(err).context("DELETE /v1/runtime/sessions/:id")),
        }
    }
}

#[derive(Serialize)]
struct LaunchSessionRequest {
    install_profile_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_label: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LaunchSessionResponse {
    pub(crate) session_id: String,
    #[serde(default)]
    pub(crate) user_visible_url: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_includes_port() {
        let c = RuntimeControlClient::new(8787);
        assert_eq!(c.base_url(), "http://127.0.0.1:8787");
    }

    #[test]
    fn base_url_uses_configured_port_not_default() {
        let c = RuntimeControlClient::new(9999);
        assert_eq!(c.base_url(), "http://127.0.0.1:9999");
        assert_ne!(c.base_url(), "http://127.0.0.1:8080");
    }

    #[test]
    fn launch_request_serialises_without_target_label() {
        let req = LaunchSessionRequest {
            install_profile_key: "github.com/foo/bar@default".to_string(),
            target_label: None,
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["install_profile_key"], "github.com/foo/bar@default");
        assert!(v.get("target_label").is_none());
    }

    #[test]
    fn launch_request_serialises_with_target_label() {
        let req = LaunchSessionRequest {
            install_profile_key: "github.com/foo/bar@default".to_string(),
            target_label: Some("gpu".to_string()),
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["target_label"], "gpu");
    }
}
