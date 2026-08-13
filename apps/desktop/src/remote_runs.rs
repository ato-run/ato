//! Remote runs projection — apps already running on the account's OTHER
//! runners (Connected Runners / Managed Cloud), fetched from the account
//! API so the Shell Icon Bar can show them alongside local windows.
//!
//! Auth reuses the `ato desktop-auth-handoff` boundary
//! ([`crate::source_import_api::discover`]); when the user is signed out
//! (or offline) the snapshot quietly stays empty and the bar shows only
//! local windows. Polling runs on the background executor; only the
//! final snapshot swap touches the GPUI main thread.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use capsule::common::paths::ato_path_or_workspace_tmp;

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
    /// Store display name, enriched from `GET /v1/capsules/by/...`.
    #[serde(skip)]
    pub display_name: Option<String>,
    /// Local cache file of the capsule's Store icon, enriched + downloaded
    /// in the poller. `None` when the capsule has no icon (letter avatar).
    #[serde(skip)]
    pub icon_path: Option<PathBuf>,
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

/// Store display metadata for one capsule, cached across polls.
#[derive(Clone, Default)]
struct CapsuleDisplay {
    name: Option<String>,
    icon_path: Option<PathBuf>,
}

#[derive(Deserialize)]
struct CapsuleDetail {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    icon: Option<String>,
}

/// Fetch Store display metadata (name + icon) for `scoped_id`
/// ("publisher/slug") and download the icon into the local cache.
/// Public endpoint — no auth. Errors degrade to the letter avatar.
fn fetch_capsule_display(api_base_url: &str, scoped_id: &str) -> CapsuleDisplay {
    let Some((publisher, slug)) = scoped_id.split_once('/') else {
        return CapsuleDisplay::default();
    };
    let url = format!(
        "{}/v1/capsules/by/{}/{}",
        api_base_url.trim_end_matches('/'),
        publisher,
        slug
    );
    let detail: CapsuleDetail = match ureq::get(&url)
        .timeout(HTTP_TIMEOUT)
        .call()
        .and_then(|response| response.into_string().map_err(Into::into))
        .map_err(anyhow::Error::from)
        .and_then(|body| serde_json::from_str(&body).map_err(Into::into))
    {
        Ok(detail) => detail,
        Err(error) => {
            tracing::debug!(%scoped_id, error = %error, "capsule display fetch failed");
            return CapsuleDisplay::default();
        }
    };
    CapsuleDisplay {
        icon_path: detail.icon.as_deref().and_then(download_icon_cached),
        name: detail.name,
    }
}

/// Download `icon_url` into the desktop icon cache (keyed by URL hash) and
/// return the file path. Reuses an existing cache file without refetching.
fn download_icon_cached(icon_url: &str) -> Option<PathBuf> {
    let mut hash: u64 = 14695981039346656037;
    for byte in icon_url.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(1099511628211);
    }
    let ext = icon_url
        .rsplit('.')
        .next()
        .filter(|ext| matches!(*ext, "png" | "jpg" | "jpeg" | "webp" | "gif"))
        .unwrap_or("png");
    let dir = ato_path_or_workspace_tmp("desktop/icon-cache");
    let path = dir.join(format!("{hash:016x}.{ext}"));
    if path.exists() {
        return Some(path);
    }
    if let Err(error) = std::fs::create_dir_all(&dir) {
        tracing::debug!(error = %error, "icon cache dir create failed");
        return None;
    }
    let mut bytes: Vec<u8> = Vec::new();
    let result = ureq::get(icon_url)
        .timeout(HTTP_TIMEOUT)
        .call()
        .map_err(anyhow::Error::from)
        .and_then(|response| {
            use std::io::Read;
            response
                .into_reader()
                .take(5 * 1024 * 1024)
                .read_to_end(&mut bytes)
                .map_err(anyhow::Error::from)
        });
    if let Err(error) = result {
        tracing::debug!(%icon_url, error = %error, "icon download failed");
        return None;
    }
    if bytes.is_empty() {
        return None;
    }
    // Write via temp + rename so a torn write never caches a broken image.
    let tmp = dir.join(format!("{hash:016x}.tmp"));
    if std::fs::write(&tmp, &bytes).is_err() || std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return None;
    }
    Some(path)
}

/// Enrich runs with Store display metadata, reusing `cache` across polls.
fn enrich_runs(
    api_base_url: &str,
    runs: &mut [RemoteRun],
    cache: &mut HashMap<String, CapsuleDisplay>,
) {
    for run in runs.iter_mut() {
        let Some(scoped_id) = run.capsule_scoped_id.clone() else {
            continue;
        };
        let display = cache
            .entry(scoped_id.clone())
            .or_insert_with(|| fetch_capsule_display(api_base_url, &scoped_id))
            .clone();
        run.display_name = display.name;
        run.icon_path = display.icon_path;
    }
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
        let mut display_cache: HashMap<String, CapsuleDisplay> = HashMap::new();
        loop {
            let attempt_creds = creds.clone();
            let mut cache = std::mem::take(&mut display_cache);
            let result = be
                .spawn(async move {
                    let creds = match attempt_creds
                        .or_else(crate::source_import_api::discover)
                    {
                        Some(creds) => creds,
                        None => return (None, Vec::new(), cache),
                    };
                    match fetch_active_runs(&creds) {
                        Ok(mut runs) => {
                            enrich_runs(&creds.api_base_url, &mut runs, &mut cache);
                            (Some(creds), runs, cache)
                        }
                        Err(error) => {
                            tracing::debug!(
                                error = %error,
                                "remote runs poll failed; will re-discover next tick"
                            );
                            (None, Vec::new(), cache)
                        }
                    }
                })
                .await;
            let (next_creds, runs, cache) = result;
            creds = next_creds;
            display_cache = cache;
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
            display_name: None,
            icon_path: None,
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
