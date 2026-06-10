use super::*;

pub(super) fn process_runtime_label(
    plan: &capsule_core::router::ManifestData,
    dangerous_skip_permissions: bool,
    compatibility_host_mode: CompatibilityHostMode,
) -> String {
    if matches!(compatibility_host_mode, CompatibilityHostMode::Enabled) {
        let runtime = plan
            .execution_runtime()
            .unwrap_or_else(|| "source".to_string());
        let driver = plan.execution_driver();
        return match driver {
            Some(driver) if !driver.trim().is_empty() => {
                format!("{}/{} [host-fallback]", runtime, driver)
            }
            _ => format!("{} [host-fallback]", runtime),
        };
    }
    if dangerous_skip_permissions {
        return "host".to_string();
    }
    "nacelle".to_string()
}

pub(super) fn background_ready_message(
    id: &str,
    compatibility_host_mode: CompatibilityHostMode,
    desktop_open_only: bool,
    is_one_shot: bool,
) -> String {
    if desktop_open_only {
        return format!("🚀 Desktop app launch requested in background (ID: {id})");
    }
    if is_one_shot {
        if matches!(compatibility_host_mode, CompatibilityHostMode::Enabled) {
            return format!("✔ Background command started (Host Fallback, ID: {id})");
        }
        return format!("🚀 Background command started (ID: {id})");
    }
    if matches!(compatibility_host_mode, CompatibilityHostMode::Enabled) {
        return format!("✔ Capsule is ready (Host Fallback, ID: {id})");
    }
    format!("🚀 Capsule started in background and is ready (ID: {id})")
}

/// Message for a background run that launched but emitted no readiness signal
/// (no declared port / no probe). Honest "started, not confirmed ready" — never
/// claims readiness and is NOT the "still starting"/timeout warning (the process
/// launched fine; we simply have nothing to probe).
pub(super) fn background_started_message(
    id: &str,
    compatibility_host_mode: CompatibilityHostMode,
    is_one_shot: bool,
) -> String {
    if is_one_shot {
        if matches!(compatibility_host_mode, CompatibilityHostMode::Enabled) {
            return format!("✔ Background command started (Host Fallback, ID: {id})");
        }
        return format!("🚀 Background command started (ID: {id})");
    }
    if matches!(compatibility_host_mode, CompatibilityHostMode::Enabled) {
        return format!(
            "🚀 Capsule launched in background — no readiness signal, not confirmed ready (Host Fallback, ID: {id}). Use `ato ps --all` to inspect status."
        );
    }
    format!(
        "🚀 Capsule launched in background — no readiness signal, not confirmed ready (ID: {id}). Use `ato ps --all` to inspect status."
    )
}

pub(super) fn background_timeout_message(
    id: &str,
    compatibility_host_mode: CompatibilityHostMode,
    is_one_shot: bool,
) -> String {
    if is_one_shot {
        if matches!(compatibility_host_mode, CompatibilityHostMode::Enabled) {
            return format!(
                "⏳ Background command is still starting in compatibility mode (Host Fallback, ID: {}). Use `ato ps --all` to inspect status.",
                id
            );
        }
        return format!(
            "⏳ Background command is still starting (ID: {}). Use `ato ps --all` to inspect status.",
            id
        );
    }
    if matches!(compatibility_host_mode, CompatibilityHostMode::Enabled) {
        return format!(
            "⏳ Capsule is still starting in compatibility mode (Host Fallback, ID: {}). Use `ato ps --all` to inspect readiness.",
            id
        );
    }
    format!(
        "⏳ Capsule is still starting in background (ID: {}). Use `ato ps --all` to inspect readiness.",
        id
    )
}

pub(super) fn background_completed_message(
    id: &str,
    compatibility_host_mode: CompatibilityHostMode,
) -> String {
    if matches!(compatibility_host_mode, CompatibilityHostMode::Enabled) {
        return format!("✔ Background command completed successfully (Host Fallback, ID: {id})");
    }
    format!("✔ Background command completed successfully (ID: {id})")
}

pub(super) fn background_failure_prefix(
    id: &str,
    compatibility_host_mode: CompatibilityHostMode,
    is_one_shot: bool,
) -> String {
    if is_one_shot {
        if matches!(compatibility_host_mode, CompatibilityHostMode::Enabled) {
            return format!(
                "Background command failed before start confirmation in compatibility mode (Host Fallback, ID: {id})"
            );
        }
        return format!("Background command failed before start confirmation (ID: {id})");
    }
    if matches!(compatibility_host_mode, CompatibilityHostMode::Enabled) {
        return format!(
            "Background capsule failed before readiness in compatibility mode (Host Fallback, ID: {id})"
        );
    }
    format!("Background capsule failed before readiness (ID: {id})")
}

pub(super) struct BackgroundCompletionOptions {
    pub is_one_shot: bool,
    pub ready_without_events: bool,
    pub desktop_open_only: bool,
    pub compatibility_host_mode: CompatibilityHostMode,
    /// Execution-receipt id to re-stamp from the OBSERVED readiness outcome
    /// (Some only for host-lifecycle paths). `None` leaves the pre-spawn
    /// `launch-passed` gate untouched (e.g. the nacelle-sandbox path, which is
    /// out of scope for host readiness honesty).
    pub execution_id: Option<String>,
}

fn background_process_name(plan: &capsule_core::router::ManifestData) -> String {
    plan.manifest_path
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string()
}

fn background_process_info(
    process: &crate::executors::source::CapsuleProcess,
    plan: &capsule_core::router::ManifestData,
    process_id: &str,
    runtime: String,
    scoped_id: Option<String>,
    ready_without_events: bool,
) -> crate::runtime::process::ProcessInfo {
    let now = SystemTime::now();
    crate::runtime::process::ProcessInfo {
        id: process_id.to_string(),
        name: background_process_name(plan),
        pid: process.child.id() as i32,
        workload_pid: process.workload_pid.map(|value| value as i32),
        status: if ready_without_events {
            crate::runtime::process::ProcessStatus::Ready
        } else {
            crate::runtime::process::ProcessStatus::Starting
        },
        runtime,
        start_time: now,
        os_start_time_unix_ms: ato_session_core::process::process_start_time_unix_ms(
            process.child.id(),
        ),
        workload_os_start_time_unix_ms: process
            .workload_pid
            .and_then(ato_session_core::process::process_start_time_unix_ms),
        manifest_path: Some(plan.manifest_path.clone()),
        scoped_id,
        target_label: Some(plan.selected_target_label().to_string()),
        requested_port: None,
        log_path: process.log_path.clone(),
        ready_at: ready_without_events.then_some(now),
        last_event: Some("spawned".to_string()),
        last_error: None,
        exit_code: None,
    }
}

pub(super) async fn complete_background_source_process(
    mut process: crate::executors::source::CapsuleProcess,
    plan: &capsule_core::router::ManifestData,
    runtime: String,
    scoped_id: Option<String>,
    options: BackgroundCompletionOptions,
    reporter: &Arc<CliReporter>,
) -> Result<()> {
    let process_id = format!("capsule-{}", process.child.id());
    let info = background_process_info(
        &process,
        plan,
        &process_id,
        runtime,
        scoped_id,
        options.ready_without_events,
    );

    let process_manager = crate::runtime::process::ProcessManager::new()?;
    process_manager.write_pid(&info)?;

    let (startup_outcome, event_rx) = if options.ready_without_events {
        (BackgroundStartupOutcome::Ready, None)
    } else {
        wait_for_background_native_startup(
            &mut process,
            &process_manager,
            &process_id,
            options.is_one_shot,
        )?
    };

    cleanup_process_artifacts(&process.cleanup_paths);

    // Re-stamp the receipt's readiness gate from the OBSERVED outcome (never
    // from spawn): a run that reached readiness is readiness-passed; one that
    // launched without a readiness signal is started-without-readiness. Other
    // outcomes (timeout / failure / one-shot completion) leave the pre-spawn
    // launch-passed gate untouched — honest, since readiness was not observed.
    // Best-effort and V2-only (a no-op on V1 receipts).
    if let Some(execution_id) = options.execution_id.as_deref() {
        let stamp = match startup_outcome {
            BackgroundStartupOutcome::Ready => Some(
                crate::application::execution_receipts::mark_v2_receipt_readiness_passed(
                    execution_id,
                ),
            ),
            BackgroundStartupOutcome::StartedWithoutReadiness => Some(
                crate::application::execution_receipts::mark_v2_receipt_started_without_readiness(
                    execution_id,
                ),
            ),
            BackgroundStartupOutcome::CompletedSuccessfully
            | BackgroundStartupOutcome::TimedOut
            | BackgroundStartupOutcome::FailedBeforeReady => None,
        };
        if let Some(Err(err)) = stamp {
            eprintln!("ATO-WARN failed to mark host execution receipt readiness gate: {err}");
        }
    }

    match startup_outcome {
        BackgroundStartupOutcome::Ready => {
            let _ = process.child;
            let _ = event_rx;
            let _ = process_manager.read_pid(&process_id)?;
            reporter
                .notify(background_ready_message(
                    &process_id,
                    options.compatibility_host_mode,
                    options.desktop_open_only,
                    options.is_one_shot,
                ))
                .await?;
            Ok(())
        }
        BackgroundStartupOutcome::StartedWithoutReadiness => {
            let _ = process.child;
            let _ = event_rx;
            let _ = process_manager.read_pid(&process_id)?;
            reporter
                .notify(background_started_message(
                    &process_id,
                    options.compatibility_host_mode,
                    options.is_one_shot,
                ))
                .await?;
            Ok(())
        }
        BackgroundStartupOutcome::CompletedSuccessfully => {
            let _ = process.child;
            let _ = event_rx;
            let _ = process_manager.read_pid(&process_id)?;
            reporter
                .notify(background_completed_message(
                    &process_id,
                    options.compatibility_host_mode,
                ))
                .await?;
            Ok(())
        }
        BackgroundStartupOutcome::TimedOut => {
            let _ = process.child;
            let _ = event_rx;
            let _ = process_manager.read_pid(&process_id)?;
            reporter
                .warn(background_timeout_message(
                    &process_id,
                    options.compatibility_host_mode,
                    options.is_one_shot,
                ))
                .await?;
            Ok(())
        }
        BackgroundStartupOutcome::FailedBeforeReady => {
            let state = process_manager.read_pid(&process_id).ok();
            let mut message = background_failure_prefix(
                &process_id,
                options.compatibility_host_mode,
                options.is_one_shot,
            );
            if let Some(state) = state {
                if let Some(error) = state.last_error {
                    message.push_str(&format!(": {}", error));
                } else if let Some(code) = state.exit_code {
                    message.push_str(&format!(": exit code {}", code));
                }
                if let Some(log_path) = state.log_path {
                    message.push_str(&format!(". See logs at {}", log_path.display()));
                }
            }
            anyhow::bail!(message);
        }
    }
}

pub(super) async fn complete_foreground_source_process(
    mut process: crate::executors::source::CapsuleProcess,
    reporter: Arc<CliReporter>,
    is_one_shot: bool,
    sandbox_initialized: bool,
    ipc_socket_mapped: bool,
    desktop_open_only: bool,
    use_progressive_ui: bool,
    execution_id: Option<String>,
) -> Result<i32> {
    let (run_label, stop_label) = foreground_run_spinner_labels(desktop_open_only);
    let run_spinner = if use_progressive_ui {
        Some(crate::progressive_ui::start_spinner(run_label))
    } else {
        None
    };
    let readiness_notifier = spawn_foreground_native_event_reporter(
        reporter,
        process.event_rx.take(),
        sandbox_initialized,
        ipc_socket_mapped,
        run_spinner.clone(),
        is_one_shot,
        execution_id,
    )?;
    let exit_code = crate::executors::source::wait_for_exit(&mut process.child).await?;
    if let Some(handle) = readiness_notifier {
        let _ = handle.join();
    }
    if let Some(progress) = run_spinner {
        progress.stop(stop_label);
    }
    cleanup_process_artifacts(&process.cleanup_paths);
    Ok(exit_code)
}

pub(super) fn foreground_run_spinner_labels(
    desktop_open_only: bool,
) -> (&'static str, &'static str) {
    if desktop_open_only {
        ("Opening desktop app...", "Desktop app launch requested.")
    } else {
        ("Running Preview...", "Preview stopped.")
    }
}

pub(super) fn spawn_foreground_native_event_reporter(
    reporter: Arc<CliReporter>,
    event_rx: Option<Receiver<LifecycleEvent>>,
    sandbox_initialized: bool,
    ipc_socket_mapped: bool,
    progress: Option<ProgressBar>,
    is_one_shot: bool,
    execution_id: Option<String>,
) -> Result<Option<JoinHandle<()>>> {
    let Some(event_rx) = event_rx else {
        return Ok(None);
    };

    for message in initial_foreground_native_messages(sandbox_initialized, ipc_socket_mapped) {
        if let Some(progress) = progress.as_ref() {
            progress.set_message(message);
        } else {
            futures::executor::block_on(CapsuleReporter::notify(&*reporter, message))?;
        }
    }

    Ok(Some(std::thread::spawn(move || {
        let mut ready_reported = false;
        let mut lifecycle_ready_printed = false;
        let mut readiness_stamped = false;
        for event in event_rx {
            // Re-stamp the receipt's readiness gate from the OBSERVED event
            // (never from spawn): the first Ready is readiness-passed, the first
            // Started (no readiness signal) is started-without-readiness. A run
            // that exits before either leaves the pre-spawn launch-passed gate
            // (never readiness-passed). Best-effort + V2-only (no-op on V1).
            if !readiness_stamped && let Some(execution_id) = execution_id.as_deref() {
                let stamp = match &event {
                    LifecycleEvent::Ready { .. } => {
                        readiness_stamped = true;
                        Some(crate::application::execution_receipts::mark_v2_receipt_readiness_passed(
                            execution_id,
                        ))
                    }
                    LifecycleEvent::Started { .. } => {
                        readiness_stamped = true;
                        Some(
                            crate::application::execution_receipts::mark_v2_receipt_started_without_readiness(
                                execution_id,
                            ),
                        )
                    }
                    LifecycleEvent::Exited { .. } => None,
                };
                if let Some(Err(err)) = stamp {
                    eprintln!(
                        "ATO-WARN failed to mark host execution receipt readiness gate: {err}"
                    );
                }
            }

            for message in foreground_native_event_messages(&event, ready_reported, is_one_shot) {
                match message {
                    ForegroundEventMessage::Notify(message) => {
                        if let Some(progress) = progress.as_ref() {
                            progress.set_message(message);
                        } else {
                            let _ = futures::executor::block_on(CapsuleReporter::notify(
                                &*reporter, message,
                            ));
                        }
                    }
                    ForegroundEventMessage::Warn(message) => {
                        if let Some(progress) = progress.as_ref() {
                            progress.set_message(message);
                        } else {
                            let _ = futures::executor::block_on(CapsuleReporter::warn(
                                &*reporter, message,
                            ));
                        }
                    }
                }
            }

            // Stable machine-readable lifecycle line (companion to "RECEIPT:"):
            // lets non-TTY supervisors (the Connected Runner agent, CI) key on
            // the honest ready signal — and its observed port — without
            // parsing human strings. Printed once, directly to stdout.
            if let LifecycleEvent::Ready { port, .. } = &event
                && !lifecycle_ready_printed
            {
                lifecycle_ready_printed = true;
                println!("{}", lifecycle_ready_line(*port));
            }

            if matches!(event, LifecycleEvent::Ready { .. }) {
                ready_reported = true;
            }
        }
    })))
}

pub(super) fn wait_for_background_native_startup(
    process: &mut crate::executors::source::CapsuleProcess,
    process_manager: &crate::runtime::process::ProcessManager,
    process_id: &str,
    is_one_shot: bool,
) -> Result<(BackgroundStartupOutcome, Option<Receiver<LifecycleEvent>>)> {
    let Some(event_rx) = process.event_rx.take() else {
        return Ok((BackgroundStartupOutcome::TimedOut, None));
    };
    let event_rx = Some(event_rx);

    let deadline = Instant::now() + background_ready_wait_timeout();

    loop {
        if let Some(status) = process.child.try_wait()? {
            let exit_code = status.code();
            let _ = process_manager.update_pid(process_id, |info| {
                info.exit_code = exit_code;
                info.last_event = Some("process_exited".to_string());
                if matches!(
                    info.status,
                    crate::runtime::process::ProcessStatus::Starting
                ) {
                    if is_one_shot && status.success() {
                        info.status = crate::runtime::process::ProcessStatus::Exited;
                        info.last_error = None;
                    } else {
                        info.status = crate::runtime::process::ProcessStatus::Failed;
                        if info.last_error.is_none() {
                            info.last_error = Some(if is_one_shot {
                                "command exited before start confirmation".to_string()
                            } else {
                                "process exited before readiness".to_string()
                            });
                        }
                    }
                } else if info.status.is_active() {
                    info.status = crate::runtime::process::ProcessStatus::Exited;
                }
            });
            return Ok((
                if is_one_shot && status.success() {
                    BackgroundStartupOutcome::CompletedSuccessfully
                } else {
                    BackgroundStartupOutcome::FailedBeforeReady
                },
                event_rx,
            ));
        }

        let now = Instant::now();
        if now >= deadline {
            let _ = process_manager.update_pid(process_id, |info| {
                info.last_event = Some("startup_timeout".to_string());
            });
            return Ok((BackgroundStartupOutcome::TimedOut, event_rx));
        }

        let wait_for = std::cmp::min(Duration::from_millis(100), deadline - now);
        match event_rx
            .as_ref()
            .expect("event receiver should still be present during startup wait")
            .recv_timeout(wait_for)
        {
            Ok(event) => match persist_background_native_event(
                process_manager,
                process_id,
                &event,
                is_one_shot,
            )? {
                BackgroundStartupOutcome::Ready => {
                    return Ok((BackgroundStartupOutcome::Ready, event_rx));
                }
                BackgroundStartupOutcome::StartedWithoutReadiness => {
                    // Launched with no readiness signal — terminal for startup
                    // purposes. Return now instead of looping to the deadline.
                    return Ok((BackgroundStartupOutcome::StartedWithoutReadiness, event_rx));
                }
                BackgroundStartupOutcome::CompletedSuccessfully => {
                    return Ok((BackgroundStartupOutcome::CompletedSuccessfully, event_rx));
                }
                BackgroundStartupOutcome::FailedBeforeReady => {
                    return Ok((BackgroundStartupOutcome::FailedBeforeReady, event_rx));
                }
                BackgroundStartupOutcome::TimedOut => {}
            },
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                let _ = process_manager.update_pid(process_id, |info| {
                    if matches!(
                        info.status,
                        crate::runtime::process::ProcessStatus::Starting
                    ) {
                        info.status = crate::runtime::process::ProcessStatus::Unknown;
                        info.last_error = Some(if is_one_shot {
                            "event stream disconnected before start confirmation".to_string()
                        } else {
                            "event stream disconnected before readiness".to_string()
                        });
                    }
                });
                return Ok((BackgroundStartupOutcome::TimedOut, None));
            }
        }
    }
}

fn background_ready_wait_timeout() -> Duration {
    std::env::var(BACKGROUND_READY_WAIT_TIMEOUT_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .filter(|duration| !duration.is_zero())
        .unwrap_or(BACKGROUND_READY_WAIT_TIMEOUT)
}

fn persist_background_native_event(
    process_manager: &crate::runtime::process::ProcessManager,
    process_id: &str,
    event: &LifecycleEvent,
    is_one_shot: bool,
) -> Result<BackgroundStartupOutcome> {
    let now = SystemTime::now();
    let updated = process_manager.update_pid(process_id, |info| match event {
        LifecycleEvent::Ready { .. } => {
            info.status = crate::runtime::process::ProcessStatus::Ready;
            info.ready_at = Some(now);
            info.last_event = Some("ready".to_string());
            info.last_error = None;
        }
        LifecycleEvent::Started { .. } => {
            // Launched without a readiness signal: the process is Running but
            // NOT confirmed ready. Do not set Ready/ready_at — that would be a
            // false-ready. (Background readiness then resolves via the normal
            // wait/timeout path; a follow-up may add a dedicated outcome.)
            if matches!(
                info.status,
                crate::runtime::process::ProcessStatus::Starting
            ) {
                info.status = crate::runtime::process::ProcessStatus::Running;
            }
            info.last_event = Some("started".to_string());
            info.last_error = None;
        }
        LifecycleEvent::Exited { service, exit_code } => {
            info.exit_code = *exit_code;
            info.last_event = Some("exited".to_string());
            if matches!(
                info.status,
                crate::runtime::process::ProcessStatus::Starting
            ) {
                if is_one_shot && exit_code == &Some(0) {
                    info.status = crate::runtime::process::ProcessStatus::Exited;
                    info.last_error = None;
                } else {
                    info.status = crate::runtime::process::ProcessStatus::Failed;
                    info.last_error = Some(if is_one_shot {
                        format!("command '{}' exited before start confirmation", service)
                    } else {
                        format!("service '{}' exited before readiness", service)
                    });
                }
            } else if info.status.is_active() {
                info.status = crate::runtime::process::ProcessStatus::Exited;
            }
        }
    })?;

    // A Started event is the honest "launched, no readiness signal" terminal
    // signal: surface it directly as StartedWithoutReadiness so the startup
    // wait returns promptly instead of stalling to the ready-wait deadline.
    // Gate on the event variant (not the resulting status) — `Running` is too
    // generic to key on, and only the Started event should short-circuit here.
    if matches!(event, LifecycleEvent::Started { .. }) {
        return Ok(BackgroundStartupOutcome::StartedWithoutReadiness);
    }

    Ok(match updated.status {
        crate::runtime::process::ProcessStatus::Ready => BackgroundStartupOutcome::Ready,
        crate::runtime::process::ProcessStatus::Exited
            if is_one_shot && updated.exit_code == Some(0) =>
        {
            BackgroundStartupOutcome::CompletedSuccessfully
        }
        crate::runtime::process::ProcessStatus::Failed => {
            BackgroundStartupOutcome::FailedBeforeReady
        }
        _ => BackgroundStartupOutcome::TimedOut,
    })
}

pub(super) fn cleanup_process_artifacts(paths: &[PathBuf]) {
    for path in paths {
        if path.exists() {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub(super) async fn cleanup_existing_scoped_processes_before_run(
    scoped_id: &str,
    reporter: &Arc<CliReporter>,
) -> Result<()> {
    let process_manager = crate::runtime::process::ProcessManager::new()?;
    let cleaned = process_manager.cleanup_scoped_processes(scoped_id, true)?;
    if cleaned > 0 {
        reporter
            .warn(format!(
                "🧹 Cleaned up {} existing process record(s) for {} before run",
                cleaned, scoped_id
            ))
            .await?;
    }
    Ok(())
}

pub(super) fn initial_foreground_native_messages(
    sandbox_initialized: bool,
    ipc_socket_mapped: bool,
) -> Vec<String> {
    let mut messages = Vec::new();
    if sandbox_initialized {
        messages.push("[✓] Sandbox initialized".to_string());
    }
    if ipc_socket_mapped {
        messages.push("[✓] IPC socket mapped".to_string());
    }
    messages
}

/// Machine-readable ready line: `LIFECYCLE: ready` or `LIFECYCLE: ready port=N`.
pub(super) fn lifecycle_ready_line(port: Option<u16>) -> String {
    match port {
        Some(port) => format!("LIFECYCLE: ready port={port}"),
        None => "LIFECYCLE: ready".to_string(),
    }
}

pub(super) fn foreground_native_event_messages(
    event: &LifecycleEvent,
    ready_reported: bool,
    is_one_shot: bool,
) -> Vec<ForegroundEventMessage> {
    match event {
        LifecycleEvent::Ready { service, .. } if !ready_reported => {
            let ready_message = if is_one_shot {
                if service == "main" {
                    "[✓] Command started (ready event received)".to_string()
                } else {
                    format!("[✓] Command '{service}' started (ready event received)")
                }
            } else if service == "main" {
                "[✓] Service is ready (ready event received)".to_string()
            } else {
                format!("[✓] Service '{service}' is ready (ready event received)")
            };
            vec![
                ForegroundEventMessage::Notify(ready_message),
                ForegroundEventMessage::Notify("    Streaming logs...".to_string()),
            ]
        }
        LifecycleEvent::Exited { service, exit_code } if !ready_reported => {
            let exit_code = exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "unknown".to_string());
            if is_one_shot && exit_code == "0" {
                return vec![ForegroundEventMessage::Notify(if service == "main" {
                    "[✓] Command completed successfully (exit code: 0)".to_string()
                } else {
                    format!("[✓] Command '{service}' completed successfully (exit code: 0)")
                })];
            }
            let message = if is_one_shot {
                if service == "main" {
                    format!("❌ Command exited before start confirmation (exit code: {exit_code})")
                } else {
                    format!(
                        "❌ Command '{service}' exited before start confirmation (exit code: {exit_code})"
                    )
                }
            } else {
                format!("❌ Service '{service}' exited before readiness (exit code: {exit_code})")
            };
            vec![ForegroundEventMessage::Warn(message)]
        }
        LifecycleEvent::Started { service, .. } if !ready_reported => {
            // Launched, but NO readiness signal (no probe / no declared port).
            // Honest "started, not ready" — never printed as ready. A later
            // Exited still surfaces "exited before readiness" (ready_reported
            // stays false).
            let message = if is_one_shot {
                format!("[•] Command '{service}' started (no readiness signal)")
            } else if service == "main" {
                "[•] Service launched — no readiness signal, not confirmed ready".to_string()
            } else {
                format!(
                    "[•] Service '{service}' launched — no readiness signal, not confirmed ready"
                )
            };
            vec![
                ForegroundEventMessage::Notify(message),
                ForegroundEventMessage::Notify("    Streaming logs...".to_string()),
            ]
        }
        _ => Vec::new(),
    }
}

pub(super) async fn notify_web_endpoint(
    plan: &capsule_core::router::ManifestData,
    reporter: &Arc<CliReporter>,
) -> Result<()> {
    let port = runtime_overrides::override_port(plan.execution_port()).ok_or_else(|| {
        anyhow::anyhow!(
            "runtime=web target '{}' requires targets.<label>.port",
            plan.selected_target_label()
        )
    })?;

    reporter
        .notify(format!(
            "🌐 Web target '{}' is available at http://127.0.0.1:{}/",
            plan.selected_target_label(),
            port
        ))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process_manager_with_starting_record(
        id: &str,
    ) -> (crate::runtime::process::ProcessManager, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let manager = crate::runtime::process::ProcessManager::with_run_dir_for_test(
            dir.path().to_path_buf(),
        );
        let info = crate::runtime::process::ProcessInfo {
            id: id.to_string(),
            name: "test".to_string(),
            pid: std::process::id() as i32,
            workload_pid: None,
            status: crate::runtime::process::ProcessStatus::Starting,
            runtime: "host".to_string(),
            start_time: SystemTime::now(),
            os_start_time_unix_ms: None,
            workload_os_start_time_unix_ms: None,
            manifest_path: None,
            scoped_id: None,
            target_label: None,
            requested_port: None,
            log_path: None,
            ready_at: None,
            last_event: None,
            last_error: None,
            exit_code: None,
        };
        manager.write_pid(&info).expect("write pid record");
        (manager, dir)
    }

    #[test]
    fn lifecycle_ready_line_is_stable_and_machine_readable() {
        assert_eq!(
            lifecycle_ready_line(Some(8000)),
            "LIFECYCLE: ready port=8000"
        );
        assert_eq!(lifecycle_ready_line(None), "LIFECYCLE: ready");
    }

    #[test]
    fn started_event_maps_to_started_without_readiness_not_timeout() {
        // Regression guard for the host-path timeout defect: a Started event (no
        // readiness signal) must resolve PROMPTLY to StartedWithoutReadiness,
        // never Ready and never the TimedOut catch-all (which would stall a
        // healthy no-port background run to the ready-wait deadline).
        let id = "capsule-started-test";
        let (manager, _dir) = process_manager_with_starting_record(id);

        let outcome = persist_background_native_event(
            &manager,
            id,
            &LifecycleEvent::Started {
                service: "main".to_string(),
                endpoint: None,
                port: None,
            },
            false,
        )
        .expect("persist started event");

        assert_eq!(outcome, BackgroundStartupOutcome::StartedWithoutReadiness);

        // The persisted record is Running (launched) — never Ready, no ready_at.
        let persisted = manager.read_pid(id).expect("read pid record");
        assert_eq!(
            persisted.status,
            crate::runtime::process::ProcessStatus::Running
        );
        assert_eq!(persisted.last_event.as_deref(), Some("started"));
        assert!(
            persisted.ready_at.is_none(),
            "a Started event must never set ready_at"
        );
    }

    #[test]
    fn ready_event_still_maps_to_ready() {
        // Sanity: a genuine Ready event still resolves to Ready (the port-branch
        // honest path is unchanged by this fix).
        let id = "capsule-ready-test";
        let (manager, _dir) = process_manager_with_starting_record(id);
        let outcome = persist_background_native_event(
            &manager,
            id,
            &LifecycleEvent::Ready {
                service: "main".to_string(),
                endpoint: None,
                port: Some(8080),
            },
            false,
        )
        .expect("persist ready event");
        assert_eq!(outcome, BackgroundStartupOutcome::Ready);
    }
}
