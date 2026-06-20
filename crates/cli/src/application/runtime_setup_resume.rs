//! Runtime Setup reboot-resume marker (#460 PR2).
//!
//! Some Windows substrate remediations (installing WSL, enabling the Virtual
//! Machine Platform) only finish after a reboot. Before such an action, the
//! Desktop writes a small marker so that on the next launch it can resume
//! Runtime Setup from where the user left off — instead of making them
//! re-navigate or, worse, drop to a shell.
//!
//! Layout: `~/.ato/runtime-setup/resume.json`.
//!
//! The marker is advisory and self-healing: a missing, corrupt, or stale marker
//! is treated as "nothing to resume" rather than an error, so a bad file can
//! never wedge Runtime Setup.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use capsule::runtime_setup::{
    RuntimeSetupResumeMarker, RuntimeSetupResumeStep, WindowsSubstrateActionKind,
};

/// Resume markers older than this are ignored (the user likely abandoned the
/// flow or the reboot happened long ago).
pub(crate) const RESUME_MARKER_TTL_MS: u64 = 24 * 60 * 60 * 1000; // 24h

/// Default on-disk path for the resume marker. Never falls back to system tmp.
pub(crate) fn resume_marker_path() -> PathBuf {
    capsule::common::paths::ato_path_or_workspace_tmp("runtime-setup/resume.json")
}

/// Write a resume marker to `path`, creating parent directories as needed.
pub(crate) fn write_resume_marker_at(path: &Path, marker: &RuntimeSetupResumeMarker) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(marker).context("failed to serialize resume marker")?;
    std::fs::write(path, json).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

/// Read a resume marker from `path`. Returns `None` when the file is absent or
/// cannot be parsed (corrupt markers are ignored, never surfaced as errors).
pub(crate) fn read_resume_marker_at(path: &Path) -> Option<RuntimeSetupResumeMarker> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Remove the resume marker at `path`. A missing file is success (idempotent).
pub(crate) fn clear_resume_marker_at(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("failed to remove {}", path.display())),
    }
}

/// Whether `marker` is older than [`RESUME_MARKER_TTL_MS`] relative to
/// `now_unix_ms`. A marker stamped in the future is not stale.
pub(crate) fn is_marker_stale(marker: &RuntimeSetupResumeMarker, now_unix_ms: u64) -> bool {
    now_unix_ms.saturating_sub(marker.created_at_unix_ms) > RESUME_MARKER_TTL_MS
}

// ── default-path convenience wrappers ─────────────────────────────────────────

/// Read the resume marker from the default path (`None` if absent/corrupt).
pub(crate) fn read_resume_marker() -> Option<RuntimeSetupResumeMarker> {
    read_resume_marker_at(&resume_marker_path())
}

/// Clear the resume marker at the default path.
pub(crate) fn clear_resume_marker() -> Result<()> {
    clear_resume_marker_at(&resume_marker_path())
}

/// Outcome of evaluating a resume marker against current substrate state (#460).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResumeOutcome {
    /// No marker present — nothing to resume.
    NothingToResume,
    /// Marker is too old; it has been (or should be) cleared.
    Stale,
    /// Substrate is ready now; clear the marker and proceed with `next`.
    Ready { next: RuntimeSetupResumeStep },
    /// Substrate is not ready yet; keep the marker and re-show the current step.
    StillPending,
}

/// Decide what to do with a resume marker given whether the substrate is now
/// ready. Pure so the policy is unit-testable without disk or a clock. See #460.
pub(crate) fn decide_resume(
    marker: Option<&RuntimeSetupResumeMarker>,
    substrate_ready: bool,
    now_unix_ms: u64,
) -> ResumeOutcome {
    let Some(marker) = marker else {
        return ResumeOutcome::NothingToResume;
    };
    if is_marker_stale(marker, now_unix_ms) {
        return ResumeOutcome::Stale;
    }
    if substrate_ready {
        ResumeOutcome::Ready {
            next: marker.expected_next_step,
        }
    } else {
        ResumeOutcome::StillPending
    }
}

/// Public entry for `ato internal runtime resume-after-reboot [--json]` (#460
/// PR2). Reads the resume marker, re-checks the (read-only) substrate status,
/// and decides the next step — clearing the marker once the substrate is ready
/// or the marker is stale. Never mutates host runtime state itself.
pub(crate) fn resume_after_reboot(json: bool) -> Result<()> {
    let marker = read_resume_marker();
    let status = crate::application::runtime_setup::collect_setup_status();
    // Non-Windows (or missing substrate diagnostics) → treat as ready: there is
    // no WSL substrate to wait on.
    let substrate_ready = status
        .windows_substrate
        .as_ref()
        .map(|s| s.wsl == capsule::runtime_setup::WslStatus::Ready)
        .unwrap_or(true);
    let now_unix_ms = now_unix_ms();
    let outcome = decide_resume(marker.as_ref(), substrate_ready, now_unix_ms);

    // Clear the marker once it is no longer actionable.
    if matches!(outcome, ResumeOutcome::Ready { .. } | ResumeOutcome::Stale) {
        let _ = clear_resume_marker();
    }

    // #460 PR3: alongside the reboot marker, surface any pending capsule
    // launch-intent so the Desktop can resume the interrupted launch once the
    // substrate is ready. Read-only here (the Desktop consumes it when it acts);
    // the two markers are independent files.
    let launch_intent = crate::application::runtime_setup_launch::read_launch_intent();
    let launch = crate::application::runtime_setup_launch::decide_launch_continuation(
        launch_intent,
        substrate_ready,
        now_unix_ms,
    );

    if json {
        let (outcome_str, next) = match &outcome {
            ResumeOutcome::NothingToResume => ("nothing_to_resume", None),
            ResumeOutcome::Stale => ("stale", None),
            ResumeOutcome::StillPending => ("still_pending", None),
            ResumeOutcome::Ready { next } => ("ready", Some(*next)),
        };
        let launch_payload = match &launch {
            crate::application::runtime_setup_launch::LaunchContinuation::Continue(intent) => {
                serde_json::json!({ "status": "continue", "intent": intent })
            }
            crate::application::runtime_setup_launch::LaunchContinuation::Pending => {
                serde_json::json!({ "status": "pending" })
            }
            crate::application::runtime_setup_launch::LaunchContinuation::Discard => {
                serde_json::json!({ "status": "discard" })
            }
            crate::application::runtime_setup_launch::LaunchContinuation::None => {
                serde_json::Value::Null
            }
        };
        let payload = serde_json::json!({
            "ok": true,
            "resumeOutcome": outcome_str,
            "nextStep": next,
            "runtimeSetupStatus": status,
            "launchContinuation": launch_payload,
        });
        println!("{payload}");
    } else {
        let msg = match &outcome {
            ResumeOutcome::NothingToResume => "No Runtime Setup to resume.".to_string(),
            ResumeOutcome::Stale => "Discarded a stale Runtime Setup resume marker.".to_string(),
            ResumeOutcome::StillPending => {
                "Runtime Setup is still waiting on the Windows substrate.".to_string()
            }
            ResumeOutcome::Ready { next } => {
                format!("Substrate ready; resuming Runtime Setup ({next:?}).")
            }
        };
        println!("{msg}");
    }
    Ok(())
}

/// Current wall-clock as unix milliseconds (0 on a pre-epoch clock).
fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// What executing a substrate remediation entails. Pure so the policy (which
/// command runs, whether a reboot marker is written, whether it delegates to
/// the Podman repair flow) is unit-testable without touching the host. See #460.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SubstratePlan {
    /// `wsl.exe` arguments to run, if any.
    pub wsl_args: Option<Vec<&'static str>>,
    /// Whether to persist a reboot-resume marker before/after running.
    pub writes_reboot_marker: bool,
    /// Whether the action delegates to the Podman machine repair flow.
    pub delegates_to_repair: bool,
    /// Whether the action is guidance-only (nothing to execute from the CLI).
    pub guidance_only: bool,
}

/// Decide how to execute a substrate remediation action. Pure. See #460.
pub(crate) fn plan_substrate_action(kind: WindowsSubstrateActionKind) -> SubstratePlan {
    use WindowsSubstrateActionKind as K;
    match kind {
        // Installing WSL needs elevation (the Desktop elevates) and a reboot, so
        // record a resume marker and request the install.
        K::InstallWsl => SubstratePlan {
            wsl_args: Some(vec!["--install", "--no-distribution"]),
            writes_reboot_marker: true,
            ..SubstratePlan::default()
        },
        // Setting the default version to 2 is non-elevated and non-destructive
        // (it does not convert existing user distros).
        K::EnableWsl2 => SubstratePlan {
            wsl_args: Some(vec!["--set-default-version", "2"]),
            ..SubstratePlan::default()
        },
        // Reboot is pending — just persist the marker so setup resumes after it.
        K::RebootRequired => SubstratePlan {
            writes_reboot_marker: true,
            ..SubstratePlan::default()
        },
        K::RepairPodmanMachine => SubstratePlan {
            delegates_to_repair: true,
            ..SubstratePlan::default()
        },
        K::OpenVirtualizationInstructions | K::None => SubstratePlan {
            guidance_only: true,
            ..SubstratePlan::default()
        },
    }
}

/// Outcome of executing a substrate remediation (`run_substrate_remediation`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SubstrateRemediationResult {
    /// `(success, output)` of the `wsl.exe` step, if one ran.
    pub ran: Option<(bool, String)>,
    /// Whether a reboot-resume marker was persisted.
    pub marker_written: bool,
    /// Whether this was a guidance-only action (nothing executed).
    pub guidance_only: bool,
}

/// Execute a substrate remediation against an injected `wsl.exe` runner and
/// marker path. Pure of process/global state so the failure semantics are
/// unit-testable. See #460 PR2.
///
/// Order matters: the `wsl.exe` step runs **first**; a reboot-resume marker is
/// written **only after** it succeeds. A non-zero `wsl.exe` exit is an **error**
/// and leaves **no** marker behind — so a denied/failed remediation never strands
/// the user in a bogus "resume after reboot" state.
///
/// `RepairPodmanMachine` is handled by the caller (it delegates to the Podman
/// repair flow) and must not be passed here.
fn run_substrate_remediation<F>(
    kind: WindowsSubstrateActionKind,
    source_surface: &str,
    status_summary: &str,
    marker_path: &std::path::Path,
    now_ms: u64,
    run_wsl: F,
) -> Result<SubstrateRemediationResult>
where
    F: Fn(&[&str]) -> std::io::Result<(bool, String)>,
{
    let plan = plan_substrate_action(kind);
    debug_assert!(!plan.delegates_to_repair, "repair is handled by the caller");

    // 1. Run the wsl.exe step (if any). A non-zero exit fails the whole action.
    let ran = if let Some(args) = &plan.wsl_args {
        let (ok, output) =
            run_wsl(args).with_context(|| format!("failed to run wsl.exe {}", args.join(" ")))?;
        if !ok {
            return Err(anyhow::anyhow!(
                "wsl.exe {} failed: {}",
                args.join(" "),
                output.trim()
            ));
        }
        Some((true, output))
    } else {
        None
    };

    // 2. Only after a successful (or absent) command, persist the reboot marker.
    let marker_written = if plan.writes_reboot_marker {
        let marker = RuntimeSetupResumeMarker {
            schema_version: capsule::runtime_setup::RUNTIME_SETUP_RESUME_SCHEMA_VERSION,
            requested_action: kind,
            source_surface: source_surface.to_string(),
            created_at_unix_ms: now_ms,
            requires_reboot: true,
            expected_next_step: RuntimeSetupResumeStep::ContinuePodmanPrepare,
            status_before_reboot: status_summary.to_string(),
        };
        write_resume_marker_at(marker_path, &marker).context("failed to write resume marker")?;
        true
    } else {
        false
    };

    Ok(SubstrateRemediationResult {
        ran,
        marker_written,
        guidance_only: plan.guidance_only,
    })
}

/// Public entry for `ato internal runtime prepare-windows-substrate --action …`
/// (#460 PR2). Executes the substrate remediation the Desktop offered: runs the
/// relevant `wsl.exe` step (non-destructive; never converts user distros),
/// persists a reboot-resume marker **only after** the step succeeds, or delegates
/// to the Podman machine repair flow. The Desktop supplies elevation for actions
/// whose plan `requires_admin`. A failed `wsl.exe` step returns an error (and in
/// JSON mode, `ok:false`) and leaves no marker.
pub(crate) fn prepare_windows_substrate(
    kind: WindowsSubstrateActionKind,
    source_surface: String,
    json: bool,
) -> Result<()> {
    // `none` is not a runnable remediation — reject it rather than report a
    // misleading guidance-only success.
    if kind == WindowsSubstrateActionKind::None {
        return Err(anyhow::anyhow!("no remediation to run for action 'none'"));
    }

    if plan_substrate_action(kind).delegates_to_repair {
        return crate::application::runtime_prepare::repair_host_runtime(json);
    }

    let status_summary = capsule::runtime_setup::WindowsSubstrateAction::for_kind(kind).label;
    let result = run_substrate_remediation(
        kind,
        &source_surface,
        &status_summary,
        &resume_marker_path(),
        now_unix_ms(),
        |args| {
            let output = std::process::Command::new("wsl.exe").args(args).output()?;
            let mut combined = output.stdout.clone();
            combined.extend_from_slice(&output.stderr);
            Ok((
                output.status.success(),
                crate::application::runtime_setup::decode_wsl_output(&combined),
            ))
        },
    );

    match result {
        Ok(result) => {
            if json {
                let payload = serde_json::json!({
                    "ok": true,
                    "action": format!("{kind:?}"),
                    "sourceSurface": source_surface,
                    "guidanceOnly": result.guidance_only,
                    "rebootMarkerWritten": result.marker_written,
                    "ran": result.ran.as_ref().map(|(ok, out)| serde_json::json!({ "ok": ok, "output": out })),
                });
                println!("{payload}");
            } else if result.guidance_only {
                println!("No automatic action; follow the on-screen guidance for {kind:?}.");
            } else if result.marker_written {
                println!(
                    "Started {kind:?}; restart to continue — Ato will resume setup afterward."
                );
            } else {
                println!("Ran {kind:?}: ok");
            }
            Ok(())
        }
        Err(err) => {
            if json {
                let payload = serde_json::json!({
                    "ok": false,
                    "action": format!("{kind:?}"),
                    "sourceSurface": source_surface,
                    "error": { "message": format!("{err:#}") },
                });
                println!("{payload}");
            }
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule::runtime_setup::{RuntimeSetupResumeStep, WindowsSubstrateActionKind};

    fn sample_marker(created_at_unix_ms: u64) -> RuntimeSetupResumeMarker {
        RuntimeSetupResumeMarker {
            schema_version: capsule::runtime_setup::RUNTIME_SETUP_RESUME_SCHEMA_VERSION,
            requested_action: WindowsSubstrateActionKind::InstallWsl,
            source_surface: "onboarding".to_string(),
            created_at_unix_ms,
            requires_reboot: true,
            expected_next_step: RuntimeSetupResumeStep::ContinuePodmanPrepare,
            status_before_reboot: "wsl missing".to_string(),
        }
    }

    #[test]
    fn write_then_read_roundtrips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("runtime-setup/resume.json");
        let marker = sample_marker(1_000);
        write_resume_marker_at(&path, &marker).expect("write");
        assert_eq!(read_resume_marker_at(&path), Some(marker));
    }

    #[test]
    fn read_absent_is_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("does-not-exist.json");
        assert_eq!(read_resume_marker_at(&path), None);
    }

    #[test]
    fn read_corrupt_is_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("resume.json");
        std::fs::write(&path, "{ not valid json ]").expect("write");
        assert_eq!(read_resume_marker_at(&path), None);
    }

    #[test]
    fn clear_removes_marker_and_is_idempotent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("resume.json");
        write_resume_marker_at(&path, &sample_marker(1_000)).expect("write");
        assert!(path.exists());
        clear_resume_marker_at(&path).expect("clear");
        assert!(!path.exists());
        // Second clear on an already-absent file is still Ok.
        clear_resume_marker_at(&path).expect("clear idempotent");
    }

    #[test]
    fn staleness_uses_ttl() {
        let marker = sample_marker(1_000);
        assert!(!is_marker_stale(&marker, 1_000)); // same instant
        assert!(!is_marker_stale(&marker, 1_000 + RESUME_MARKER_TTL_MS)); // exactly TTL
        assert!(is_marker_stale(&marker, 1_000 + RESUME_MARKER_TTL_MS + 1)); // past TTL
        // A marker stamped in the future (clock skew) is not stale.
        assert!(!is_marker_stale(&marker, 500));
    }

    #[test]
    fn decide_resume_no_marker_is_nothing() {
        assert_eq!(
            decide_resume(None, false, 2_000),
            ResumeOutcome::NothingToResume
        );
        assert_eq!(
            decide_resume(None, true, 2_000),
            ResumeOutcome::NothingToResume
        );
    }

    #[test]
    fn decide_resume_stale_marker_is_stale_even_if_ready() {
        let marker = sample_marker(1_000);
        let now = 1_000 + RESUME_MARKER_TTL_MS + 1;
        assert_eq!(
            decide_resume(Some(&marker), true, now),
            ResumeOutcome::Stale
        );
    }

    #[test]
    fn decide_resume_ready_clears_and_continues() {
        let marker = sample_marker(1_000);
        assert_eq!(
            decide_resume(Some(&marker), true, 2_000),
            ResumeOutcome::Ready {
                next: RuntimeSetupResumeStep::ContinuePodmanPrepare
            }
        );
    }

    #[test]
    fn decide_resume_not_ready_is_still_pending() {
        let marker = sample_marker(1_000);
        assert_eq!(
            decide_resume(Some(&marker), false, 2_000),
            ResumeOutcome::StillPending
        );
    }

    // ── substrate remediation plan ────────────────────────────────────────────

    #[test]
    fn plan_install_wsl_runs_install_and_writes_marker() {
        let plan = plan_substrate_action(WindowsSubstrateActionKind::InstallWsl);
        assert_eq!(plan.wsl_args, Some(vec!["--install", "--no-distribution"]));
        assert!(plan.writes_reboot_marker);
        assert!(!plan.delegates_to_repair);
    }

    #[test]
    fn plan_enable_wsl2_sets_default_version_no_marker() {
        let plan = plan_substrate_action(WindowsSubstrateActionKind::EnableWsl2);
        assert_eq!(plan.wsl_args, Some(vec!["--set-default-version", "2"]));
        assert!(!plan.writes_reboot_marker);
    }

    #[test]
    fn plan_reboot_required_only_writes_marker() {
        let plan = plan_substrate_action(WindowsSubstrateActionKind::RebootRequired);
        assert!(plan.writes_reboot_marker);
        assert_eq!(plan.wsl_args, None);
    }

    #[test]
    fn plan_repair_delegates() {
        let plan = plan_substrate_action(WindowsSubstrateActionKind::RepairPodmanMachine);
        assert!(plan.delegates_to_repair);
        assert_eq!(plan.wsl_args, None);
        assert!(!plan.writes_reboot_marker);
    }

    #[test]
    fn plan_virtualization_and_none_are_guidance_only() {
        for kind in [
            WindowsSubstrateActionKind::OpenVirtualizationInstructions,
            WindowsSubstrateActionKind::None,
        ] {
            let plan = plan_substrate_action(kind);
            assert!(plan.guidance_only);
            assert_eq!(plan.wsl_args, None);
            assert!(!plan.writes_reboot_marker);
            assert!(!plan.delegates_to_repair);
        }
    }

    // ── run_substrate_remediation: failure semantics (#460 PR2 review) ─────────

    use std::cell::RefCell;

    /// A fake `wsl.exe` runner that records calls and returns a fixed result.
    fn fake_wsl<'a>(
        calls: &'a RefCell<Vec<Vec<String>>>,
        success: bool,
        output: &'a str,
    ) -> impl Fn(&[&str]) -> std::io::Result<(bool, String)> + 'a {
        move |args: &[&str]| {
            calls
                .borrow_mut()
                .push(args.iter().map(|s| s.to_string()).collect());
            Ok((success, output.to_string()))
        }
    }

    #[test]
    fn remediation_install_wsl_success_runs_then_writes_marker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("resume.json");
        let calls = RefCell::new(Vec::new());
        let result = run_substrate_remediation(
            WindowsSubstrateActionKind::InstallWsl,
            "onboarding",
            "WSL missing",
            &path,
            1_000,
            fake_wsl(&calls, true, "ok"),
        )
        .expect("install ok");
        assert_eq!(result.ran, Some((true, "ok".to_string())));
        assert!(result.marker_written);
        assert!(path.exists(), "marker must exist after success");
        assert_eq!(calls.borrow().len(), 1);
    }

    #[test]
    fn remediation_install_wsl_failure_errs_and_leaves_no_marker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("resume.json");
        let calls = RefCell::new(Vec::new());
        let err = run_substrate_remediation(
            WindowsSubstrateActionKind::InstallWsl,
            "onboarding",
            "WSL missing",
            &path,
            1_000,
            fake_wsl(&calls, false, "access denied"),
        )
        .expect_err("non-zero wsl exit must fail");
        assert!(err.to_string().contains("failed"), "{err}");
        assert!(
            !path.exists(),
            "a failed remediation must NOT leave a resume marker"
        );
    }

    #[test]
    fn remediation_enable_wsl2_success_no_marker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("resume.json");
        let calls = RefCell::new(Vec::new());
        let result = run_substrate_remediation(
            WindowsSubstrateActionKind::EnableWsl2,
            "settings",
            "WSL2 unavailable",
            &path,
            1_000,
            fake_wsl(&calls, true, ""),
        )
        .expect("enable ok");
        assert!(result.ran.is_some());
        assert!(!result.marker_written);
        assert!(!path.exists());
        assert_eq!(calls.borrow()[0], vec!["--set-default-version", "2"]);
    }

    #[test]
    fn remediation_enable_wsl2_failure_errs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("resume.json");
        let calls = RefCell::new(Vec::new());
        let err = run_substrate_remediation(
            WindowsSubstrateActionKind::EnableWsl2,
            "settings",
            "WSL2 unavailable",
            &path,
            1_000,
            fake_wsl(&calls, false, "boom"),
        )
        .expect_err("non-zero wsl exit must fail");
        assert!(err.to_string().contains("failed"), "{err}");
        assert!(!path.exists());
    }

    #[test]
    fn remediation_reboot_required_writes_marker_without_wsl() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("resume.json");
        let calls = RefCell::new(Vec::new());
        let result = run_substrate_remediation(
            WindowsSubstrateActionKind::RebootRequired,
            "onboarding",
            "Reboot required",
            &path,
            1_000,
            fake_wsl(&calls, true, "unused"),
        )
        .expect("reboot marker");
        assert_eq!(result.ran, None, "no wsl.exe runs for reboot-required");
        assert!(result.marker_written);
        assert!(path.exists());
        assert!(calls.borrow().is_empty(), "must not invoke wsl.exe");
    }
}
