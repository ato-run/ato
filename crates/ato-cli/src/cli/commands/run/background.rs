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
        for event in event_rx {
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
