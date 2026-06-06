//! Foreground host-runtime *prepare* (Podman).
//!
//! Streams `ato internal runtime prepare --tools podman --emit-json` progress
//! into the active Runtime Setup surface (onboarding or settings). Unlike
//! [`super::install`] (which provisions Ato-managed language toolchains), this
//! is the only desktop path that may mutate host/runtime state for a container
//! engine: install Podman when supported, create/start the Ato-managed
//! `ato-podman` machine, and verify readiness.
//!
//! The streaming machinery (single-job guard, `started`/`progress`/`complete`
//! events, status refresh) is shared with `install` via
//! [`super::install::spawn_runtime_job`]; only validation and the CLI
//! subcommand differ. The dedicated [`Capability::RuntimeSetupPrepare`] gate
//! (checked by the broker before this runs) is what keeps prepare distinct from
//! install.

use anyhow::{Result as AnyhowResult, anyhow, bail};
use capsule_core::runtime_setup::ToolKind;
use gpui::App;

use super::ensure_install_global;
use super::install::{RuntimeJobKind, push_runtime_setup_error, spawn_runtime_job};

/// Kick off a foreground prepare of the requested host runtimes (Podman).
pub(crate) fn start_runtime_prepare(cx: &mut App, request_id: Option<String>, tools: Vec<String>) {
    let tools = match parse_prepareable_tools(&tools) {
        Ok(tools) => tools,
        Err(err) => {
            ensure_install_global(cx);
            push_runtime_setup_error(cx, request_id, &format!("{err:#}"));
            return;
        }
    };
    spawn_runtime_job(cx, request_id, tools, RuntimeJobKind::Prepare);
}

/// Kick off a repair of the Ato-managed Podman machine (#460 PR2). Streams the
/// same progress/terminal events as prepare and refreshes status on completion.
pub(crate) fn start_runtime_repair(cx: &mut App, request_id: Option<String>) {
    spawn_runtime_job(
        cx,
        request_id,
        Vec::new(),
        RuntimeJobKind::RepairHostRuntime,
    );
}

/// Kick off a Windows substrate remediation (#460 PR2). `action` is a
/// `WindowsSubstrateActionKind` token; the CLI rejects unknown/`none` actions.
pub(crate) fn start_windows_substrate(
    cx: &mut App,
    request_id: Option<String>,
    action: String,
    source_surface: Option<String>,
) {
    if action.trim().is_empty() {
        ensure_install_global(cx);
        push_runtime_setup_error(cx, request_id, "no substrate action specified");
        return;
    }
    spawn_runtime_job(
        cx,
        request_id,
        Vec::new(),
        RuntimeJobKind::PrepareWindowsSubstrate {
            action,
            source_surface: source_surface.unwrap_or_else(|| "settings".to_string()),
        },
    );
}

/// Validate and normalise a `--tools` prepare request. Only *host runtimes*
/// (Podman today) may be prepared through this path; managed toolchains go
/// through [`super::install`] and detection-only/bundled tools are rejected
/// wholesale so a bad request cannot half-prepare.
pub(crate) fn parse_prepareable_tools(tools: &[String]) -> AnyhowResult<Vec<String>> {
    if tools.is_empty() {
        bail!("no runtime tools selected to prepare");
    }
    let mut parsed = Vec::with_capacity(tools.len());
    for tool in tools {
        let kind =
            ToolKind::parse_tool(tool).ok_or_else(|| anyhow!("unknown runtime tool: {tool}"))?;
        if !kind.is_host_runtime_prepareable() {
            bail!("{} is not a host runtime Ato can prepare", kind.as_str());
        }
        let token = kind.as_str().to_string();
        if !parsed.contains(&token) {
            parsed.push(token);
        }
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_prepareable_tools_allows_podman() {
        assert_eq!(
            parse_prepareable_tools(&["podman".to_string()]).unwrap(),
            vec!["podman".to_string()]
        );
    }

    #[test]
    fn parse_prepareable_tools_rejects_managed_toolchains() {
        let err = parse_prepareable_tools(&["node".to_string()])
            .unwrap_err()
            .to_string();
        assert!(err.contains("not a host runtime"), "got: {err}");
    }

    #[test]
    fn parse_prepareable_tools_rejects_detection_only() {
        let err = parse_prepareable_tools(&["docker".to_string()])
            .unwrap_err()
            .to_string();
        assert!(err.contains("not a host runtime"), "got: {err}");
    }

    #[test]
    fn parse_prepareable_tools_rejects_empty() {
        assert!(parse_prepareable_tools(&[]).is_err());
    }

    #[test]
    fn parse_prepareable_tools_dedupes() {
        assert_eq!(
            parse_prepareable_tools(&["podman".to_string(), "podman".to_string()]).unwrap(),
            vec!["podman".to_string()]
        );
    }
}
