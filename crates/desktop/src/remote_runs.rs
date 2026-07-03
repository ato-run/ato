//! Remote runs projection — apps already running on the account's OTHER
//! runners (Connected Runners / Managed Cloud), fetched from the account
//! API so the Shell Icon Bar can show them alongside local windows.
//!
//! Auth reuses the `ato desktop-auth-handoff` boundary
//! ([`crate::source_import_api::discover`]); when the user is signed out
//! (or offline) the snapshot quietly stays empty and the bar shows only
//! local windows. Polling runs on the background executor; only the
//! final snapshot swap touches the GPUI main thread.

use std::time::Duration;

use serde::Deserialize;

use crate::source_import_api::ApiCreds;

const POLL_INTERVAL: Duration = Duration::from_secs(30);
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// One active run on a remote runner, as listed by `GET /v1/runs?status=active`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct RemoteRun {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub capsule_scoped_id: Option<String>,
    pub status: String,
    #[serde(default)]
    pub runner_display_name: Option<String>,
    #[serde(default)]
    pub app_url: Option<String>,
    #[serde(default)]
    pub ready_url: Option<String>,
}

impl RemoteRun {
    /// URL to open when the tab is clicked — the public app URL when the
    /// run exposes one.
    pub fn open_url(&self) -> Option<&str> {
        self.app_url
            .as_deref()
            .or(self.ready_url.as_deref())
            .filter(|url| !url.is_empty())
    }
}

#[derive(Deserialize)]
struct RunsResponse {
    #[serde(default)]
    runs: Vec<RemoteRun>,
}

/// Latest account-wide active-runs snapshot. Empty when signed out,
/// offline, or before the first poll completes.
#[derive(Default)]
pub struct RemoteRunsSnapshot {
    pub runs: Vec<RemoteRun>,
}

impl gpui::Global for RemoteRunsSnapshot {}

fn fetch_active_runs(creds: &ApiCreds) -> anyhow::Result<Vec<RemoteRun>> {
    let url = format!(
        "{}/v1/runs?status=active&limit=50",
        creds.api_base_url.trim_end_matches('/')
    );
    let body = ureq::get(&url)
        .set(
            "Authorization",
            &format!("Bearer {}", creds.session_token),
        )
        .timeout(HTTP_TIMEOUT)
        .call()?
        .into_string()?;
    let parsed: RunsResponse = serde_json::from_str(&body)?;
    Ok(parsed.runs)
}

/// Start the background poller. Installs the (empty) snapshot global and
/// refreshes it every [`POLL_INTERVAL`]; observers of
/// [`RemoteRunsSnapshot`] re-render when the run set changes.
pub fn start_remote_runs_poller(cx: &mut gpui::App) {
    cx.set_global(RemoteRunsSnapshot::default());

    let async_app = cx.to_async();
    let fe = async_app.foreground_executor().clone();
    let be = async_app.background_executor().clone();
    let aa = async_app.clone();
    fe.spawn(async move {
        // Cache the auth handoff across polls (it spawns the ato CLI);
        // drop it when a request fails so an expired session re-discovers.
        let mut creds: Option<ApiCreds> = None;
        loop {
            let attempt_creds = creds.clone();
            let result = be
                .spawn(async move {
                    let creds = match attempt_creds
                        .or_else(crate::source_import_api::discover)
                    {
                        Some(creds) => creds,
                        None => return (None, Vec::new()),
                    };
                    match fetch_active_runs(&creds) {
                        Ok(runs) => (Some(creds), runs),
                        Err(error) => {
                            tracing::debug!(
                                error = %error,
                                "remote runs poll failed; will re-discover next tick"
                            );
                            (None, Vec::new())
                        }
                    }
                })
                .await;
            let (next_creds, runs) = result;
            creds = next_creds;
            aa.update(|cx| {
                let changed = cx.global::<RemoteRunsSnapshot>().runs != runs;
                if changed {
                    tracing::info!(count = runs.len(), "remote runs snapshot updated");
                    cx.set_global(RemoteRunsSnapshot { runs });
                }
            });
            be.timer(POLL_INTERVAL).await;
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_url_prefers_app_url_and_skips_empty() {
        let mut run = RemoteRun {
            id: "run_1".into(),
            label: "hello".into(),
            capsule_scoped_id: None,
            status: "running".into(),
            runner_display_name: Some("oci-a1".into()),
            app_url: Some("https://abc.app.ato.run/".into()),
            ready_url: Some("https://ready.example/".into()),
        };
        assert_eq!(run.open_url(), Some("https://abc.app.ato.run/"));
        run.app_url = None;
        assert_eq!(run.open_url(), Some("https://ready.example/"));
        run.ready_url = Some(String::new());
        assert_eq!(run.open_url(), None);
    }

    #[test]
    fn runs_response_parses_api_shape() {
        let body = r#"{"runs":[{"id":"run_1","label":"hello-capsule",
            "capsule_scoped_id":"community/hello-capsule","status":"running",
            "placement":"external_runner","runner_id":"rd_1",
            "runner_display_name":"oci-a1","ready_url":null,
            "app_url":"https://abc.app.ato.run/","created_at":"2026-07-03",
            "updated_at":"2026-07-03","stopped_at":null,"error":null}]}"#;
        let parsed: RunsResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.runs.len(), 1);
        assert_eq!(parsed.runs[0].label, "hello-capsule");
        assert_eq!(parsed.runs[0].open_url(), Some("https://abc.app.ato.run/"));
    }
}
