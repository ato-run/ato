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

use anyhow::{Context, Result};
use serde::Serialize;

use crate::install::fetch_capsule_detail;

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
    let summary = runtime
        .block_on(fetch_capsule_detail(handle, registry))
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
