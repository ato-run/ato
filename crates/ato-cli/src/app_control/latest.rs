//! `ato app latest` — fetch a capsule's latest published version from the
//! registry without consulting the local install cache.
//!
//! The desktop calls this from a worker thread right after a capsule launches
//! (see `ato-desktop/src/webview.rs::apply_launch_session_metadata`). It compares
//! the returned `latest_version` against the running snapshot label and, when
//! the registry has a newer release, surfaces an update banner inside the
//! route-info popover with an "Install update" button.
//!
//! The wrapper is intentionally minimal: `crate::install::fetch_capsule_detail`
//! already does the auth-aware HTTP call (`{registry}/v1/capsules/by/{publisher}/{slug}`)
//! and returns a `CapsuleDetailSummary` with `latest_version` extracted; we
//! just reshape its result into a CCP envelope so the desktop can parse it the
//! same way it parses `resolve` / `session start`.
//!
//! Handle inputs accepted (must reduce to a registry capsule):
//! * canonical: `capsule://ato.run/<pub>/<slug>` (what the desktop sends)
//! * dev registry: `capsule://localhost:<port>/<pub>/<slug>` — registry
//!   override is threaded through to `fetch_capsule_detail` so the dev flow
//!   doesn't fall back to ato.run.
//! * bare scoped: `<pub>/<slug>` and the npm-style `@<pub>/<slug>`.

use anyhow::{Context, Result};
use capsule_core::handle::{normalize_capsule_handle, CanonicalHandle};
use serde::Serialize;

use crate::install::{fetch_capsule_detail, parse_capsule_ref};

const ACTION: &str = "fetch_latest";

#[derive(Debug, Clone, Serialize)]
struct LatestEnvelope<'a> {
    schema_version: &'static str,
    package_id: &'static str,
    action: &'static str,
    result: LatestResult<'a>,
}

#[derive(Debug, Clone, Serialize)]
struct LatestResult<'a> {
    /// The capsule's scoped id as the registry returned it (e.g.
    /// `koh0920/byok-ai-chat`). Useful when the desktop wants to log against
    /// a canonical identifier rather than echo the user-typed handle back.
    scoped_id: String,
    /// The newest version string the registry knows about. `None` is rare —
    /// either the registry serves the capsule but has no published releases,
    /// or `latest_version` was explicitly empty / whitespace.
    latest_version: Option<&'a str>,
}

/// Run the `ato app latest <handle> [--registry URL] [--json]` command.
///
/// `json = true` prints the CCP envelope on stdout. `json = false` prints a
/// human-readable single-line summary to stdout and returns `Ok(())`.
///
/// We use `tokio::runtime::Builder` rather than `#[tokio::main]` so this can
/// stay reachable from the CLI's synchronous dispatcher.
pub fn fetch_latest(handle: &str, registry: Option<&str>, json: bool) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to initialise async runtime for `ato app latest`")?;
    let (cli_ref, registry_override) = canonicalize_for_registry_query(handle, registry)?;
    let summary = runtime
        .block_on(fetch_capsule_detail(&cli_ref, registry_override.as_deref()))
        .with_context(|| format!("failed to fetch capsule detail for {handle}"))?;

    if json {
        let envelope = LatestEnvelope {
            schema_version: super::SCHEMA_VERSION,
            package_id: super::ATO_DESKTOP_PACKAGE_ID,
            action: ACTION,
            result: LatestResult {
                scoped_id: summary.scoped_id.clone(),
                latest_version: summary.latest_version.as_deref(),
            },
        };
        println!("{}", serde_json::to_string_pretty(&envelope)?);
        return Ok(());
    }

    match summary.latest_version.as_deref() {
        Some(version) => println!("{} → latest v{version}", summary.scoped_id),
        None => println!("{} → no published release", summary.scoped_id),
    }
    Ok(())
}

/// Reduce a user/desktop-supplied handle to the `(cli_ref, registry_override)`
/// pair that `fetch_capsule_detail` expects. Mirrors the pattern in
/// `app_control::resolve::build_store_resolution` so `app latest` accepts the
/// same handle shapes as `app resolve` and `app session start`.
///
/// The earlier shape passed `handle` straight through; that worked for bare
/// `<pub>/<slug>` but failed for the canonical `capsule://ato.run/...` form
/// that the desktop sends, because `parse_capsule_request` rejects any
/// `split('/')` past two segments.
fn canonicalize_for_registry_query(
    handle: &str,
    registry: Option<&str>,
) -> Result<(String, Option<String>)> {
    if let Ok(canonical) = normalize_capsule_handle(handle) {
        return match &canonical {
            CanonicalHandle::RegistryCapsule { .. } => {
                let cli_ref = canonical
                    .to_cli_ref()
                    .expect("RegistryCapsule always yields a cli_ref");
                let registry_override = registry
                    .map(str::to_string)
                    .or_else(|| canonical.registry_url_override().map(str::to_string));
                Ok((cli_ref, registry_override))
            }
            CanonicalHandle::GithubRepo { .. } => {
                anyhow::bail!(
                    "`ato app latest` requires a registry capsule (got github handle '{handle}')"
                )
            }
            CanonicalHandle::LocalPath { .. } => {
                anyhow::bail!(
                    "`ato app latest` requires a registry capsule (got local path '{handle}')"
                )
            }
        };
    }

    // Fallback: `normalize_capsule_handle` does not recognise the npm-style
    // `@<pub>/<slug>` form, but `parse_capsule_request` does (it strips the
    // leading `@` before splitting). Defer to that parser so the convenience
    // form keeps working from the CLI.
    let scoped = parse_capsule_ref(handle)
        .with_context(|| format!("failed to parse capsule handle '{handle}'"))?;
    Ok((scoped.scoped_id, registry.map(str::to_string)))
}

#[cfg(test)]
mod tests {
    use super::canonicalize_for_registry_query;

    #[test]
    fn accepts_bare_scoped_handle() {
        let (cli_ref, override_) =
            canonicalize_for_registry_query("koh0920/flatnotes", None).expect("bare scoped");
        assert_eq!(cli_ref, "koh0920/flatnotes");
        assert!(override_.is_none());
    }

    #[test]
    fn accepts_npm_style_at_scoped_handle() {
        // `normalize_capsule_handle` does NOT accept `@scope/slug`; this case
        // exercises the `parse_capsule_request` fallback. Regression guard:
        // the original (pre-`app latest`) CLI surface accepted `@scope/slug`
        // and we must keep doing so.
        let (cli_ref, override_) =
            canonicalize_for_registry_query("@koh0920/flatnotes", None).expect("npm-style");
        assert_eq!(cli_ref, "koh0920/flatnotes");
        assert!(override_.is_none());
    }

    #[test]
    fn accepts_canonical_capsule_url_from_desktop() {
        // The exact shape the desktop sends — what was failing before this
        // fix with `invalid_capsule_ref: use publisher/slug ...`.
        let (cli_ref, override_) =
            canonicalize_for_registry_query("capsule://ato.run/koh0920/flatnotes", None)
                .expect("canonical url");
        assert_eq!(cli_ref, "koh0920/flatnotes");
        // ato.run is the official registry, so no override is set.
        assert!(override_.is_none());
    }

    #[test]
    fn loopback_registry_handle_threads_override_through() {
        // Dev registry handles must surface the loopback URL as a registry
        // override, otherwise `fetch_capsule_detail` falls back to ato.run
        // and the dev flow silently misses the local registry.
        let (cli_ref, override_) =
            canonicalize_for_registry_query("capsule://localhost:8787/acme/chat", None)
                .expect("dev registry");
        assert_eq!(cli_ref, "acme/chat");
        let endpoint = override_.expect("loopback registry must yield an override");
        assert!(
            endpoint.contains("localhost:8787"),
            "expected override to point at localhost:8787, got {endpoint}"
        );
    }

    #[test]
    fn explicit_registry_flag_wins_over_loopback_override() {
        // The `--registry` CLI flag is the user's explicit choice; it should
        // shadow whatever the canonical handle would otherwise imply.
        let (_cli_ref, override_) = canonicalize_for_registry_query(
            "capsule://localhost:8787/acme/chat",
            Some("https://registry.example/"),
        )
        .expect("explicit registry override");
        assert_eq!(override_.as_deref(), Some("https://registry.example/"));
    }

    #[test]
    fn rejects_github_handle() {
        let err = canonicalize_for_registry_query("capsule://github.com/acme/app", None)
            .expect_err("github handle must be rejected for app latest");
        let msg = format!("{err:#}");
        assert!(msg.contains("requires a registry capsule"), "got: {msg}");
    }
}
