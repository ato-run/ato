//! `ato-launch` system capsule — capsule-launch wizards.
//!
//! Two HTML views:
//!   - `assets/system/ato-launch/consent.html` — pre-flight
//!     consent wizard. Shows the capsule's identity, requested
//!     permissions, and any required env-var inputs. User clicks
//!     "承認して起動" or "キャンセル".
//!   - `assets/system/ato-launch/boot.html` — mid-flight boot
//!     progress. Shows the launch steps (Capsule取得 → 依存解決
//!     → 起動環境 → セキュリティ → データ保護 → プライバシー).
//!
//! Phase 1 ships both views as standalone demonstrable shells —
//! they are openable via MCP for AODD, but are NOT yet hooked into
//! the real `crate::orchestrator::resolve_and_start_guest` capsule
//! launch flow. Phase 2 will (a) gate every CapsuleHandle spawn on
//! a consent decision and (b) drive boot progress from orchestrator
//! events.
//!
//! Phase 1 dispatch handlers close the wizard window on
//! Approve/Cancel and log the outcome. Approve carries the capsule
//! handle so a follow-up iteration can spawn the AppWindow once the
//! consent flow is real.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use gpui::{AnyWindowHandle, App};
use serde::Deserialize;

use crate::state::GuestRoute;
use crate::system_capsule::broker::{BrokerError, Capability};

/// Consent identity sent from the wizard JS on Approve,
/// matching the fields `approve_execution_plan_consent` expects.
#[derive(Debug, Deserialize)]
pub struct ConsentApprovalItem {
    pub scoped_id: String,
    pub version: String,
    pub target_label: String,
    pub policy_segment_hash: String,
    pub provisioning_policy_hash: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LaunchCommand {
    /// User clicked "承認して起動" in the consent wizard.
    /// Carries the preview_id to guard against stale approvals,
    /// any new secret values to persist, non-secret config values,
    /// and execution-plan consent items to record.
    Approve {
        preview_id: String,
        #[serde(default)]
        secrets: HashMap<String, String>,
        #[serde(default)]
        config: HashMap<String, String>,
        #[serde(default)]
        consents: Vec<ConsentApprovalItem>,
    },
    /// User clicked "キャンセル" or dismissed the wizard.
    Cancel,
    /// Boot wizard's "Cancel during launch" affordance.
    AbortBoot,
    /// User pressed "Find candidates" in the GitHub Run wizard.
    /// `repo` is `owner/repo` (already normalized by the React layer).
    GithubFindCandidates { repo: String },
    /// User pressed "CLI 推論を実行" in the CreateTomlScreen.
    /// Reads lightweight metadata from the GitHub repo to infer a
    /// draft `capsule.toml`.
    GithubCliInference { repo: String },
    /// User clicked "起動レビューへ進む" (Proceed to review) in the
    /// candidate detail screen. Normalizes `repo` into a launchable
    /// `github.com/{owner}/{repo}` handle and opens the consent wizard.
    GithubProceedToConsent {
        repo: String,
        title: String,
        manifest_toml: String,
        manifest_source: crate::state::ManifestSource,
        #[serde(default = "default_requested_ref")]
        requested_ref: String,
    },
}

impl LaunchCommand {
    pub fn required_capability(&self) -> Capability {
        match self {
            LaunchCommand::Approve { .. } => Capability::WebviewCreate,
            LaunchCommand::Cancel | LaunchCommand::AbortBoot => Capability::WindowsClose,
            LaunchCommand::GithubFindCandidates { .. } => Capability::WebviewCreate,
            LaunchCommand::GithubCliInference { .. } => Capability::WebviewCreate,
            LaunchCommand::GithubProceedToConsent { .. } => Capability::WebviewCreate,
        }
    }
}

pub fn dispatch(
    cx: &mut App,
    host: AnyWindowHandle,
    command: LaunchCommand,
) -> Result<(), BrokerError> {
    match command {
        LaunchCommand::Approve {
            preview_id,
            secrets,
            config,
            consents,
        } => {
            tracing::info!(preview_id = %preview_id, "ato_launch: user approved");

            // Warn if preview_id doesn't match the active wizard.
            let pending_preview = cx
                .try_global::<crate::window::launch_window::PendingConsentPreview>()
                .and_then(|g| g.0.clone());
            if let Some(ref preview) = pending_preview
                && preview.preview_id != preview_id
            {
                tracing::warn!(
                    expected = %preview_id,
                    current = %preview.preview_id,
                    "ato_launch: preview_id mismatch on approve"
                );
            }
            cx.set_global(crate::window::launch_window::PendingConsentPreview(None));

            let stashed = cx
                .global_mut::<crate::window::launch_window::PendingLaunches>()
                .0
                .remove(&preview_id);

            // PR-D1: an `ato://run` request bypasses the AppWindow boot flow
            // entirely — there is no guest WebView pane to embed. Persist the
            // secrets/consents the wizard collected exactly as any other
            // Approve would (below), but once that is done, spawn `ato run`
            // directly and return without ever calling `open_boot_window`.
            let run_agent_request = stashed.as_ref().and_then(|s| s.run_agent_request.clone());

            let pending_route = stashed.as_ref().map(|s| s.route.clone());
            let requested_client = stashed
                .as_ref()
                .map(|s| s.requested_client)
                .unwrap_or(crate::state::session::SessionClientKind::AtoWindow);

            // Derive route handle for secret grants (must match what
            // AppCapsuleShell uses via secrets_for_capsule).
            let route_handle: Option<String> = match &pending_route {
                Some(GuestRoute::CapsuleHandle { handle, .. }) => Some(handle.clone()),
                Some(GuestRoute::CapsuleUrl { handle, .. }) => Some(handle.clone()),
                Some(GuestRoute::LocalManifest(local)) => Some(local.source_handle.clone()),
                _ => None,
            };

            // Persist new secret values and grant them to this capsule.
            if let Some(ref handle) = route_handle
                && !secrets.is_empty()
            {
                let mut store = crate::config::load_secrets();
                for (key, value) in &secrets {
                    if !value.is_empty() {
                        if let Err(e) = store.add_secret(key.clone(), value.clone()) {
                            return Err(BrokerError::Internal(format!(
                                "Failed to save secret {key}: {e}"
                            )));
                        }
                        if let Err(e) = store.grant_secret(handle, key) {
                            return Err(BrokerError::Internal(format!(
                                "Failed to grant secret {key} to {handle}: {e}"
                            )));
                        }
                    }
                }
            }

            // Record execution-plan consents.
            for consent in &consents {
                if let Err(err) = crate::orchestrator::approve_execution_plan_consent(
                    &consent.scoped_id,
                    &consent.version,
                    &consent.target_label,
                    &consent.policy_segment_hash,
                    &consent.provisioning_policy_hash,
                ) {
                    tracing::error!(
                        error = %err,
                        scoped_id = %consent.scoped_id,
                        "ato_launch: failed to approve consent"
                    );
                }
            }

            // Close the consent wizard so the boot wizard (or, for an
            // `ato://run` request, nothing at all) takes focus.
            let _ = host.update(cx, |_, window, _| window.remove_window());

            // PR-D1: an `ato://run` request never opens a boot wizard / guest
            // WebView pane — approval means "spawn `ato run <source>` on the
            // Desktop Runner now." Secrets + execution-plan consents were
            // already persisted above (identical to any other Approve); the
            // non-secret `config` map is not consumed by this path (the CLI
            // resolves its own env), so it is intentionally dropped here.
            if let Some(run_agent) = run_agent_request {
                let ready_state_enabled = false; // M3: cold-OCI only.
                match crate::desktop_run_agent::launch(
                    &run_agent.source,
                    run_agent.run_id.as_deref(),
                    ready_state_enabled,
                ) {
                    Ok(()) => {
                        tracing::info!(
                            source = %run_agent.source,
                            origin = %run_agent.origin,
                            run_id = ?run_agent.run_id,
                            "ato_launch: run agent launch started after consent approval"
                        );
                    }
                    Err(reason) => {
                        tracing::warn!(
                            source = %run_agent.source,
                            origin = %run_agent.origin,
                            %reason,
                            "ato_launch: run agent launch failed to start after approval"
                        );
                        if let Err(err) =
                            crate::window::launch_blocked_popup::open_launch_blocked_popup(
                                cx,
                                run_agent.source.clone(),
                                reason,
                            )
                        {
                            tracing::error!(error = %err, "ato_launch: failed to show run-agent launch-failed popup");
                        }
                    }
                }
                return Ok(());
            }

            // Store non-secret config so AppCapsuleShell passes it to
            // resolve_and_start_guest.
            let plain_configs: Vec<(String, String)> = config.into_iter().collect();
            cx.set_global(crate::window::launch_window::PendingLaunchConfigs(
                plain_configs.clone(),
            ));

            if let Some(route) = pending_route {
                match crate::window::launch_window::open_boot_window(cx, Some(&route)) {
                    Ok(boot_handle) => {
                        // start_boot_launch owns the background launch
                        // task: it stores the boot handle + abort flag
                        // in BootWindowSlot, drives orchestrator progress
                        // events into the wizard, and on success calls
                        // `open_ready_capsule_window` itself. We must NOT
                        // also call `open_app_window` here — that would
                        // create two concurrent capsule sessions.
                        crate::window::launch_window::start_boot_launch(
                            cx,
                            route.clone(),
                            plain_configs,
                            boot_handle,
                            requested_client,
                        );

                        // Record launch in the start-page history so the
                        // next time the start page opens, this capsule
                        // appears in the "recent capsules" row.
                        let history_item = match &route {
                            GuestRoute::CapsuleHandle { handle, label, .. }
                            | GuestRoute::CapsuleUrl { handle, label, .. } => {
                                Some((handle.as_str(), label.as_str()))
                            }
                            GuestRoute::LocalManifest(local) => {
                                Some((local.source_handle.as_str(), local.label.as_str()))
                            }
                            _ => None,
                        };
                        if let Some((handle, label)) = history_item {
                            let mut store =
                                crate::system_capsule::ato_start::StartPageHistoryStore::load();
                            store.record_open(handle, label);
                            if let Err(err) = store.save() {
                                tracing::warn!(error = %err, "ato_launch: failed to save start history");
                            }
                        }
                    }
                    Err(err) => {
                        tracing::error!(
                            error = %err,
                            "ato_launch: open_boot_window failed after approve"
                        );
                    }
                }
            } else {
                tracing::info!(
                    "ato_launch: approve from MCP/standalone (no pending target) — wizard closed, no AppWindow spawned"
                );
            }
        }
        LaunchCommand::Cancel => {
            // PR-D1: if the cancelled wizard was an `ato://run` consent
            // request, log a clear declined outcome — mirrors this same
            // "tracing-only, no popup" treatment every other Cancel already
            // gets; there is no separate window left open afterward for
            // either case, so silence here is consistent, not a regression.
            let declined_run_agent = cx
                .try_global::<crate::window::launch_window::PendingConsentPreview>()
                .and_then(|p| p.0.as_ref().map(|preview| preview.preview_id.clone()))
                .and_then(|preview_id| {
                    cx.try_global::<crate::window::launch_window::PendingLaunches>()
                        .and_then(|launches| launches.0.get(&preview_id).cloned())
                })
                .and_then(|stashed| stashed.run_agent_request);

            tracing::info!("ato_launch: user cancelled");
            if let Some(run_agent) = declined_run_agent {
                tracing::info!(
                    source = %run_agent.source,
                    origin = %run_agent.origin,
                    "ato_launch: run consent declined"
                );
            }
            cx.set_global(crate::window::launch_window::PendingLaunches::default());
            cx.set_global(crate::window::launch_window::PendingConsentPreview(None));
            let _ = host.update(cx, |_, window, _| window.remove_window());
        }
        LaunchCommand::AbortBoot => {
            tracing::info!("ato_launch: user aborted boot — signalling background task");

            let slot = cx
                .try_global::<crate::window::launch_window::BootWindowSlot>()
                .cloned()
                .unwrap_or_default();
            cx.set_global(crate::window::launch_window::BootWindowSlot::default());

            // Tell the background launch worker to suppress its
            // successful session — otherwise a late success would
            // spawn the AppWindow even after the user cancelled.
            if let Some(flag) = slot.abort_flag.as_ref() {
                flag.store(true, std::sync::atomic::Ordering::Release);
            }
            if let Some(boot) = slot.boot_window {
                let _ = boot.update(cx, |_, window, _| window.remove_window());
            }
        }
        LaunchCommand::GithubFindCandidates { repo } => {
            tracing::info!(repo = %repo, "ato_launch: github_find_candidates requested");

            let shell_weak = cx
                .try_global::<crate::window::launch_window::ActiveGithubRunShell>()
                .and_then(|s| s.0.clone());
            let Some(shell_weak) = shell_weak else {
                tracing::warn!("ato_launch: github_find_candidates — no ActiveGithubRunShell");
                return Ok(());
            };

            let async_app = cx.to_async();
            let fe = cx.foreground_executor().clone();
            let be = cx.background_executor().clone();
            let repo_owned = Arc::new(repo);

            fe.spawn(async move {
                let repo_clone = Arc::clone(&repo_owned);
                let result: serde_json::Value = be
                    .spawn(async move { fetch_github_candidates(&repo_clone) })
                    .await;

                crate::webview_init_guard::wait_until_idle(&be).await;
                async_app.update(|cx| {
                    if let Some(shell) = shell_weak.upgrade() {
                        shell.read(cx).inject_github_candidates(&result);
                    }
                });
            })
            .detach();
        }
        LaunchCommand::GithubCliInference { repo } => {
            tracing::info!(repo = %repo, "ato_launch: github_cli_inference requested");

            let shell_weak = cx
                .try_global::<crate::window::launch_window::ActiveGithubRunShell>()
                .and_then(|s| s.0.clone());
            let Some(shell_weak) = shell_weak else {
                tracing::warn!("ato_launch: github_cli_inference — no ActiveGithubRunShell");
                return Ok(());
            };

            let async_app = cx.to_async();
            let fe = cx.foreground_executor().clone();
            let be = cx.background_executor().clone();
            let repo_owned = Arc::new(repo);

            fe.spawn(async move {
                let repo_clone = Arc::clone(&repo_owned);
                let result: serde_json::Value = be
                    .spawn(async move { infer_capsule_toml(&repo_clone) })
                    .await;

                crate::webview_init_guard::wait_until_idle(&be).await;
                async_app.update(|cx| {
                    if let Some(shell) = shell_weak.upgrade() {
                        shell.read(cx).inject_cli_inference_result(&result);
                    }
                });
            })
            .detach();
        }
        LaunchCommand::GithubProceedToConsent {
            repo,
            title,
            manifest_toml,
            manifest_source,
            requested_ref,
        } => {
            tracing::info!(
                repo = %repo,
                title = %title,
                manifest_source = manifest_source.as_str(),
                requested_ref = %requested_ref,
                "ato_launch: github_proceed_to_consent"
            );

            let shell_weak = cx
                .try_global::<crate::window::launch_window::ActiveGithubRunShell>()
                .and_then(|s| s.0.clone());
            let async_app = cx.to_async();
            let fe = cx.foreground_executor().clone();
            let be = cx.background_executor().clone();
            let request = crate::github_manifest_draft::GithubDraftRequest {
                repo,
                title,
                manifest_toml,
                manifest_source,
                requested_ref,
            };

            fe.spawn(async move {
                let result = be
                    .spawn(async move {
                        crate::github_manifest_draft::prepare_github_manifest_draft(request)
                    })
                    .await;
                crate::webview_init_guard::wait_until_idle(&be).await;
                async_app.update(|cx| match result {
                    Ok(route) => {
                        let _ = host.update(cx, |_, window, _| window.remove_window());
                        cx.set_global(crate::window::launch_window::ActiveGithubRunShell(None));
                        if let Err(err) =
                            crate::window::launch_window::open_consent_window_for_route(cx, route)
                        {
                            tracing::error!(
                                error = %err,
                                "ato_launch: open_consent_window_for_route failed"
                            );
                        }
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "ato_launch: github draft validation failed");
                        if let Some(shell) = shell_weak.and_then(|weak| weak.upgrade()) {
                            let payload = serde_json::json!({
                                "ok": false,
                                "error": format!("{err:#}"),
                            });
                            shell.read(cx).inject_github_proceed_result(&payload);
                        }
                    }
                });
            })
            .detach();
        }
    }
    Ok(())
}

fn default_requested_ref() -> String {
    "HEAD".to_string()
}

/// Fetch `capsule.toml` candidates for `owner/repo` from the GitHub
/// contents API. Returns a JSON envelope `{ok:true,candidates:[...]}` or
/// `{ok:false,error:"..."}`. Runs on a background thread — uses `ureq`
/// which is synchronous/blocking.
fn fetch_github_candidates(repo: &str) -> serde_json::Value {
    let url = format!(
        "https://api.github.com/repos/{}/contents/capsule.toml",
        repo
    );
    match ureq::get(&url)
        .set("User-Agent", "ato-desktop")
        .set("Accept", "application/vnd.github+json")
        .call()
    {
        Ok(resp) => {
            match resp.into_json::<serde_json::Value>() {
                Ok(body) => {
                    // GitHub API returns `{content: "<base64>", ...}` for file contents.
                    let content_b64 = body
                        .get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .replace('\n', "");
                    let toml_text = match base64::Engine::decode(
                        &base64::engine::general_purpose::STANDARD,
                        &content_b64,
                    ) {
                        Ok(bytes) => String::from_utf8_lossy(&bytes).to_string(),
                        Err(err) => {
                            return serde_json::json!({
                                "ok": false,
                                "error": format!("base64 decode failed: {err}")
                            });
                        }
                    };

                    // Extract basic metadata from TOML for the candidate row.
                    let name =
                        extract_toml_str(&toml_text, "name").unwrap_or_else(|| repo.to_string());
                    let version = extract_toml_str(&toml_text, "version")
                        .unwrap_or_else(|| "0.0.0".to_string());
                    let description =
                        extract_toml_str(&toml_text, "description").unwrap_or_default();
                    let author = extract_toml_str(&toml_text, "author").unwrap_or_default();

                    serde_json::json!({
                        "ok": true,
                        "candidates": [{
                            "title": name,
                            "version": version,
                            "description": description,
                            "author": author,
                            "status": "community",
                            "source": "github",
                            "manifest_source": "repo",
                            "toml": toml_text,
                            "repo": repo,
                        }]
                    })
                }
                Err(err) => serde_json::json!({
                    "ok": false,
                    "error": format!("failed to parse GitHub response: {err}")
                }),
            }
        }
        Err(ureq::Error::Status(404, _)) => serde_json::json!({
            "ok": true,
            "candidates": [],
            "repo": repo,
        }),
        Err(ureq::Error::Status(code, _)) => serde_json::json!({
            "ok": false,
            "error": format!("GitHub API エラー (HTTP {code})")
        }),
        Err(err) => serde_json::json!({
            "ok": false,
            "error": format!("ネットワークエラー: {err}")
        }),
    }
}

/// Minimal single-value extractor for a top-level TOML string key.
/// Avoids pulling in the full `toml` crate parse for a hot path.
fn extract_toml_str(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with(key)
            && let Some(rest) = line.strip_prefix(key)
        {
            let rest = rest.trim();
            if let Some(after_eq) = rest.strip_prefix('=') {
                let value = after_eq
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string();
                if !value.is_empty() {
                    return Some(value);
                }
            }
        }
    }
    None
}

fn github_api_get(url: &str) -> Result<serde_json::Value, String> {
    let mut last_err = String::new();
    for attempt in 0..3 {
        if attempt > 0 {
            let delay = Duration::from_millis(1000 * (1u64 << (attempt - 1)));
            std::thread::sleep(delay);
        }

        let resp = ureq::get(url)
            .set("User-Agent", "ato-desktop")
            .set("Accept", "application/vnd.github+json")
            .call()
            .map_err(|e| match e {
                ureq::Error::Status(404, _) => "NOT_FOUND".to_string(),
                ureq::Error::Status(403, resp) => {
                    let body = resp.into_string().unwrap_or_default();
                    if body.contains("rate limit") || body.contains("secondary rate limit") {
                        "RATE_LIMITED".to_string()
                    } else {
                        format!("GITHUB_UNAUTHORIZED: {body}")
                    }
                }
                ureq::Error::Status(code, resp) => {
                    let body = resp.into_string().unwrap_or_default();
                    format!("HTTP_{code}: {body}")
                }
                _ => format!("NETWORK_ERROR: {e}"),
            });

        let resp = match resp {
            Ok(r) => r,
            Err(e) if e == "NOT_FOUND" => return Err(e),
            Err(e) if e.starts_with("RATE_LIMITED") => {
                last_err = e;
                continue;
            }
            Err(e) => return Err(e),
        };

        let body: serde_json::Value = match resp.into_json() {
            Ok(v) => v,
            Err(_) => {
                last_err = "PARSE_ERROR".to_string();
                continue;
            }
        };

        if body.get("content").is_some() || body.is_array() {
            return Ok(body);
        }
        if let Some(msg) = body.get("message").and_then(|v| v.as_str()) {
            if msg.contains("rate limit") || msg.contains("secondary rate limit") {
                last_err = "RATE_LIMITED".to_string();
                continue;
            }
            return Err(format!("GITHUB_API_ERROR: {msg}"));
        }
        return Ok(body);
    }
    Err(last_err)
}

fn github_fetch_file(repo: &str, path: &str) -> Result<Option<String>, String> {
    let url = format!("https://api.github.com/repos/{}/contents/{path}", repo);
    let body = match github_api_get(&url) {
        Ok(v) => v,
        Err(e) if e == "NOT_FOUND" => return Ok(None),
        Err(e) => return Err(e),
    };
    let content_b64 = match body.get("content").and_then(|v| v.as_str()) {
        Some(s) => s.replace('\n', ""),
        None => return Ok(None),
    };
    Ok(
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &content_b64)
            .ok()
            .map(|bytes| String::from_utf8_lossy(&bytes).to_string()),
    )
}

fn infer_capsule_toml(repo: &str) -> serde_json::Value {
    let mut runtime = String::new();
    let mut entry = String::new();
    let mut name = repo
        .split('/')
        .next_back()
        .unwrap_or("my-capsule")
        .to_string();
    let mut port: Option<u16> = None;
    let mut warnings: Vec<String> = Vec::new();
    let mut api_errors: Vec<String> = Vec::new();

    let mut try_fetch = |path: &str| -> Option<String> {
        match github_fetch_file(repo, path) {
            Ok(v) => v,
            Err(e) => {
                api_errors.push(format!("{path}: {e}"));
                None
            }
        }
    };

    let mut package_json = None;
    let mut cargo_toml = false;
    let mut pyproject_toml = false;
    let mut requirements_txt = false;

    if let Some(pkg) = try_fetch("package.json") {
        package_json = Some(pkg);
    }
    if package_json.is_none() {
        cargo_toml = try_fetch("Cargo.toml").is_some();
    }
    if package_json.is_none() && !cargo_toml {
        pyproject_toml = try_fetch("pyproject.toml").is_some();
        requirements_txt = try_fetch("requirements.txt").is_some();
    }

    if let Some(pkg) = package_json {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&pkg) {
            runtime = "node".to_string();
            if let Some(n) = val.get("name").and_then(|v| v.as_str()) {
                name = n.to_string();
            }
            let start_script = val
                .get("scripts")
                .and_then(|s| s.get("start"))
                .and_then(|v| v.as_str());
            let dev_script = val
                .get("scripts")
                .and_then(|s| s.get("dev"))
                .and_then(|v| v.as_str());
            if let Some(s) = start_script {
                entry = s.to_string();
            } else if let Some(d) = dev_script {
                entry = d.to_string();
                port = Some(3000);
            }
            if entry.is_empty() {
                entry = "index.js".to_string();
            }
        }
    } else if cargo_toml {
        runtime = "rust".to_string();
        entry = "cargo run".to_string();
        warnings.push(
            "Rust projects require local clone to build. The inferred entry may need adjustment."
                .to_string(),
        );
    } else if pyproject_toml || requirements_txt {
        runtime = "python".to_string();
        entry = "main.py".to_string();
    }

    if runtime.is_empty() {
        let error_code = if api_errors.iter().any(|e| e.contains("RATE_LIMITED")) {
            "rate_limited"
        } else if !api_errors.is_empty() {
            "github_api_error"
        } else {
            "unsupported_project"
        };
        let message = match error_code {
            "rate_limited" => "GitHub API のレート制限に達しました。少し時間をおいて再試行してください。".to_string(),
            "github_api_error" => format!(
                "GitHub API エラーが発生しました: {}",
                api_errors.join("; ")
            ),
            _ => "このリポジトリのプロジェクト種別を判定できませんでした。手動で capsule.toml を作成してください。".to_string(),
        };
        return serde_json::json!({
            "ok": false,
            "error_code": error_code,
            "message": message,
        });
    }

    let mut toml = format!(
        r#"[capsule]
name = "{name}"
version = "0.1.0"
description = "Auto-inferred capsule.toml for {repo}"

[execution]
runtime = "{runtime}"
entry = "{entry}"
"#
    );
    if let Some(p) = port {
        toml.push_str(&format!("port = {p}\n"));
    }
    if !warnings.is_empty() {
        toml.push_str("\n# Warnings:\n");
        for w in &warnings {
            toml.push_str(&format!("# {w}\n"));
        }
    }

    serde_json::json!({
        "ok": true,
        "toml": toml,
        "repo": repo,
    })
}
