use anyhow::Result;
use std::sync::Arc;

use crate::adapters::runtime::oci_session_store::{
    OciSessionStore, PodmanMachineStopResult, StopByIdAttempt, apply_stop_result, stop_oci_session,
    stop_oci_session_by_id, stop_podman_machines_if_idle,
};
use crate::reporters::CliReporter;
use crate::runtime::process::{ImportPreviewStopResult, ImportPreviewStopStatus, ProcessManager};
use capsule_core::CapsuleReporter;

pub struct CloseArgs {
    pub id: Option<String>,
    pub name: Option<String>,
    pub all: bool,
    pub force: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImportPreviewStopOutcome {
    Success,
    Failure,
}

pub fn execute(args: CloseArgs, reporter: Arc<CliReporter>) -> Result<()> {
    let pm = ProcessManager::new()?;

    if args.all && args.name.is_none() && args.id.is_none() {
        let processes = pm.list_processes()?;
        let running: Vec<_> = processes.iter().filter(|p| p.status.is_active()).collect();
        let import_previews = pm.list_import_preview_sessions().unwrap_or_default();

        // Check OCI sessions before stopping so we know total activity.
        let oci_running = OciSessionStore::new()
            .ok()
            .and_then(|s| s.list_sessions().ok())
            .map(|sessions| {
                sessions
                    .into_iter()
                    .filter(|s| s.status.is_active())
                    .count()
            })
            .unwrap_or(0);

        if running.is_empty() && import_previews.is_empty() && oci_running == 0 {
            let machine_result = stop_podman_machine_if_idle(&reporter)?;
            report_podman_machine_stop_result(&machine_result, &reporter)?;
            futures::executor::block_on(reporter.notify("No active capsules.".to_string()))?;
            return Ok(());
        }

        // Stop source-runtime (PID-based) processes.
        if !running.is_empty() {
            futures::executor::block_on(
                reporter.notify(format!("Stopping {} active capsule(s)...", running.len())),
            )?;

            let mut stopped = 0;
            for p in &running {
                match pm.stop_process(&p.id, args.force) {
                    Ok(true) => {
                        futures::executor::block_on(
                            reporter.notify(format!("✅ Stopped: {} (PID: {})", p.name, p.pid)),
                        )?;
                        stopped += 1;
                    }
                    Ok(false) => {
                        futures::executor::block_on(
                            reporter.warn(format!("⚠️  Already stopped: {}", p.name)),
                        )?;
                    }
                    Err(err) => {
                        futures::executor::block_on(
                            reporter.warn(format!("❌ Failed to stop {}: {}", p.name, err)),
                        )?;
                    }
                }
            }

            futures::executor::block_on(
                reporter.notify(format!("✅ Stopped {} capsule(s)", stopped)),
            )?;
        }

        stop_all_import_preview_sessions(&pm, args.force, &reporter)?;

        // Stop OCI sessions.
        stop_all_oci_sessions(&args, &reporter)?;
        let machine_result = stop_podman_machine_if_idle(&reporter)?;
        report_podman_machine_stop_result(&machine_result, &reporter)?;

        return Ok(());
    }

    if let Some(id) = &args.id {
        match pm.stop_process(id, args.force) {
            Ok(true) => {
                futures::executor::block_on(
                    reporter.notify(format!("✅ Stopped capsule: {}", id)),
                )?;
            }
            Ok(false) => {
                if let Some(result) = pm.stop_import_preview_session(id, args.force)? {
                    if matches!(
                        report_import_preview_stop_result(&result, &reporter)?,
                        ImportPreviewStopOutcome::Failure
                    ) {
                        anyhow::bail!("{}", import_preview_stop_failure_message(&result));
                    }
                } else if let Some(attempt) = stop_oci_by_id(id, args.force)? {
                    report_oci_stop_attempt(&attempt, &reporter)?;
                    let machine_result = stop_podman_machine_if_idle(&reporter)?;
                    report_podman_machine_stop_result(&machine_result, &reporter)?;
                } else {
                    futures::executor::block_on(
                        reporter.warn(format!("⚠️  Capsule {} is not running", id)),
                    )?;
                }
            }
            Err(err) => {
                anyhow::bail!("Failed to stop capsule {}: {}", id, err);
            }
        }
    } else if let Some(name) = &args.name {
        let processes = pm.find_by_name(name)?;

        if processes.is_empty() {
            anyhow::bail!("No capsule found with name: {}", name);
        }

        let running: Vec<_> = processes.iter().filter(|p| p.status.is_active()).collect();

        if running.is_empty() {
            futures::executor::block_on(
                reporter.warn(format!("⚠️  No running capsule found with name: {}", name)),
            )?;
            return Ok(());
        }

        if running.len() > 1 && !args.all {
            futures::executor::block_on(reporter.warn(format!(
                "⚠️  Multiple capsules found with name '{}'. Use --all to stop all.",
                name
            )))?;
            for p in &running {
                futures::executor::block_on(
                    reporter.notify(format!("  - {} (ID: {}, PID: {})", p.name, p.id, p.pid)),
                )?;
            }
            anyhow::bail!("Multiple matches found. Use --all to stop all.");
        }

        let mut stopped = 0;
        for p in &running {
            match pm.stop_process(&p.id, args.force) {
                Ok(true) => {
                    futures::executor::block_on(reporter.notify(format!(
                        "✅ Stopped: {} (ID: {}, PID: {})",
                        p.name, p.id, p.pid
                    )))?;
                    stopped += 1;
                }
                Ok(false) => {}
                Err(err) => {
                    futures::executor::block_on(
                        reporter.warn(format!("❌ Failed to stop {}: {}", p.name, err)),
                    )?;
                }
            }
        }

        futures::executor::block_on(reporter.notify(format!("✅ Stopped {} capsule(s)", stopped)))?;
    } else {
        anyhow::bail!("Either --id, --name, or --all is required");
    }

    Ok(())
}

fn stop_podman_machine_if_idle(reporter: &Arc<CliReporter>) -> Result<PodmanMachineStopResult> {
    let store = match OciSessionStore::new() {
        Ok(store) => store,
        Err(err) => {
            futures::executor::block_on(reporter.warn(format!(
                "⚠️  Skipped Podman VM stop: could not open OCI session store: {err}"
            )))?;
            return Ok(PodmanMachineStopResult {
                status_before:
                    crate::adapters::runtime::oci_session_store::PodmanMachineStatus::Unknown {
                        reason: "session store unavailable".to_string(),
                    },
                stopped_machines: vec![],
                errors: vec![],
                skipped_reason: Some("could not open OCI session store".to_string()),
            });
        }
    };
    Ok(stop_podman_machines_if_idle(&store))
}

fn stop_all_import_preview_sessions(
    pm: &ProcessManager,
    force: bool,
    reporter: &Arc<CliReporter>,
) -> Result<()> {
    let results = pm.stop_all_import_preview_sessions(force)?;
    if results.is_empty() {
        return Ok(());
    }
    futures::executor::block_on(reporter.notify(format!(
        "Stopping {} import preview session(s)...",
        results.len()
    )))?;
    let mut failures = Vec::new();
    for result in &results {
        if matches!(
            report_import_preview_stop_result(result, reporter)?,
            ImportPreviewStopOutcome::Failure
        ) {
            failures.push(import_preview_stop_failure_message(result));
        }
    }
    if !failures.is_empty() {
        anyhow::bail!(
            "failed to stop import preview session(s): {}",
            failures.join("; ")
        );
    }
    Ok(())
}

fn report_import_preview_stop_result(
    result: &ImportPreviewStopResult,
    reporter: &Arc<CliReporter>,
) -> Result<ImportPreviewStopOutcome> {
    match result.status {
        ImportPreviewStopStatus::Stopped => {
            futures::executor::block_on(reporter.notify(format!(
                "✅ Stopped import preview: {}",
                result.session.run_session_id
            )))?;
            Ok(ImportPreviewStopOutcome::Success)
        }
        ImportPreviewStopStatus::AlreadyGone => {
            futures::executor::block_on(reporter.warn(format!(
                "⚠️  Import preview already gone: {}",
                result.session.run_session_id
            )))?;
            Ok(ImportPreviewStopOutcome::Success)
        }
        ImportPreviewStopStatus::NotAtoOwned | ImportPreviewStopStatus::Failed => {
            futures::executor::block_on(
                reporter.warn(import_preview_stop_failure_message(result)),
            )?;
            Ok(import_preview_stop_outcome(result.status))
        }
    }
}

fn import_preview_stop_failure_message(result: &ImportPreviewStopResult) -> String {
    let detail = result.error.as_deref().unwrap_or("unknown stop failure");
    format!(
        "❌ Failed to stop import preview {}: {}",
        result.session.run_session_id, detail
    )
}

fn import_preview_stop_outcome(status: ImportPreviewStopStatus) -> ImportPreviewStopOutcome {
    match status {
        ImportPreviewStopStatus::Stopped | ImportPreviewStopStatus::AlreadyGone => {
            ImportPreviewStopOutcome::Success
        }
        ImportPreviewStopStatus::NotAtoOwned | ImportPreviewStopStatus::Failed => {
            ImportPreviewStopOutcome::Failure
        }
    }
}

/// Stop all running OCI sessions (containers + networks).
fn stop_all_oci_sessions(args: &CloseArgs, reporter: &Arc<CliReporter>) -> Result<()> {
    let store = match OciSessionStore::new() {
        Ok(s) => s,
        Err(_) => return Ok(()), // No OCI sessions directory yet
    };
    let sessions = store.list_sessions().unwrap_or_default();
    // Retry both Running and StopFailed sessions so that a previous partial
    // failure can be recovered on the next invocation.
    let to_stop: Vec<_> = sessions.iter().filter(|s| s.status.is_active()).collect();

    for session in to_stop {
        futures::executor::block_on(reporter.notify(format!(
            "🐳 Stopping OCI session {} ({}, {} service(s))...",
            session.session_id,
            session.import_kind,
            session.services.len()
        )))?;

        let result = stop_oci_session(session, args.force);
        report_oci_stop_result(&result, &session.network_name, reporter)?;

        // Delete on full success; keep the record (as StopFailed) on partial
        // failure so a later `ato stop --all` can retry.
        apply_stop_result(&store, &session.session_id, &result);
    }

    Ok(())
}

fn stop_oci_by_id(session_id: &str, force: bool) -> Result<Option<StopByIdAttempt>> {
    let store = OciSessionStore::new()?;
    stop_oci_session_by_id(&store, session_id, force)
}

fn report_oci_stop_attempt(attempt: &StopByIdAttempt, reporter: &Arc<CliReporter>) -> Result<()> {
    futures::executor::block_on(reporter.notify(format!(
        "🐳 Stopping OCI session {} ({}, {} service(s))...",
        attempt.record.session_id,
        attempt.record.import_kind,
        attempt.record.services.len()
    )))?;
    report_oci_stop_result(&attempt.result, &attempt.record.network_name, reporter)
}

fn report_oci_stop_result(
    result: &crate::adapters::runtime::oci_session_store::StopResult,
    network_name: &str,
    reporter: &Arc<CliReporter>,
) -> Result<()> {
    for name in &result.stopped_containers {
        futures::executor::block_on(reporter.notify(format!("  ✅ Stopped container: {name}")))?;
    }
    for name in &result.errors {
        futures::executor::block_on(reporter.warn(format!("  ⚠️  {name}")))?;
    }
    if result.network_removed {
        futures::executor::block_on(
            reporter.notify(format!("  🔗 Removed network: {network_name}")),
        )?;
    }
    Ok(())
}

fn report_podman_machine_stop_result(
    result: &PodmanMachineStopResult,
    reporter: &Arc<CliReporter>,
) -> Result<()> {
    if let Some(reason) = &result.skipped_reason {
        futures::executor::block_on(
            reporter.warn(format!("  ⚠️  Skipped Podman VM stop: {reason}")),
        )?;
        return Ok(());
    }

    for name in &result.stopped_machines {
        futures::executor::block_on(reporter.notify(format!("  ✅ Stopped Podman VM: {name}")))?;
    }
    if !result.errors.is_empty() {
        futures::executor::block_on(reporter.warn(format!(
            "  ⚠️  Podman VM before stop: {}",
            result.status_before.display_status()
        )))?;
    }
    for error in &result.errors {
        futures::executor::block_on(reporter.warn(format!("  ⚠️  {error}")))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_close_args_by_id() {
        let args = CloseArgs {
            id: Some("test-123".to_string()),
            name: None,
            all: false,
            force: false,
        };
        assert!(args.id.is_some());
        assert!(args.name.is_none());
        assert!(!args.all);
        assert!(!args.force);
    }

    #[test]
    fn test_close_args_by_name() {
        let args = CloseArgs {
            id: None,
            name: Some("my-capsule".to_string()),
            all: false,
            force: true,
        };
        assert!(args.id.is_none());
        assert!(args.name.is_some());
        assert!(!args.all);
        assert!(args.force);
    }

    #[test]
    fn test_close_args_all() {
        let args = CloseArgs {
            id: None,
            name: None,
            all: true,
            force: false,
        };
        assert!(args.id.is_none());
        assert!(args.name.is_none());
        assert!(args.all);
        assert!(!args.force);
    }

    #[test]
    fn test_close_args_force() {
        let args = CloseArgs {
            id: Some("test-456".to_string()),
            name: None,
            all: false,
            force: true,
        };
        assert!(args.force);
    }

    #[test]
    fn import_preview_stop_failure_message_includes_session_and_detail() {
        let result = ImportPreviewStopResult {
            session: crate::runtime::process::ImportPreviewSession {
                run_session_id: "preview-123".to_string(),
                owner_kind: "desktop".to_string(),
                owner_pid: 1,
                owner_process_start_time_unix_ms: None,
                ato_run_pid: 2,
                ato_run_process_start_time_unix_ms: None,
                process_group_ids: vec![],
                workload_pids: vec![],
                primary_port: None,
                primary_url: None,
                shadow_dir: std::path::PathBuf::from(".tmp/shadow"),
                log_path: std::path::PathBuf::from(".tmp/log"),
                created_at_unix_ms: 0,
                updated_at_unix_ms: 0,
                expires_at_unix_ms: None,
                readiness_state: "ready".to_string(),
                cleanup_policy: "keep_until_explicit_stop".to_string(),
                last_sweep_status: None,
                last_sweep_error: None,
            },
            status: ImportPreviewStopStatus::NotAtoOwned,
            error: Some("ownership could not be verified".to_string()),
        };

        assert_eq!(
            import_preview_stop_failure_message(&result),
            "❌ Failed to stop import preview preview-123: ownership could not be verified"
        );
    }

    #[test]
    fn import_preview_stop_outcome_marks_failed_status_as_failure() {
        assert_eq!(
            import_preview_stop_outcome(ImportPreviewStopStatus::Failed),
            ImportPreviewStopOutcome::Failure
        );
    }
}
