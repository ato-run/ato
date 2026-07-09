use anyhow::Result;
use std::sync::Arc;

use crate::adapters::runtime::oci_session_store::{
    OciSessionStatus, OciSessionStore, PodmanMachineStatus, podman_machine_status,
};
use crate::binding;
use crate::reporters::CliReporter;
use crate::runtime::process::{ProcessManager, ProcessStatus, format_duration, get_process_uptime};
use capsule::CapsuleReporter;

pub struct PsArgs {
    pub json: bool,
    pub all: bool,
}

fn status_display(status: ProcessStatus) -> &'static str {
    match status {
        ProcessStatus::Starting => "🟡 starting",
        ProcessStatus::Ready => "🟢 ready",
        ProcessStatus::Running => "🟢 running",
        ProcessStatus::Exited => "⚪ exited",
        ProcessStatus::Failed => "🔴 failed",
        ProcessStatus::Stopped => "⚪ stopped",
        ProcessStatus::Unknown => "🟡 unknown",
    }
}

fn runtime_display(runtime: &str) -> String {
    if let Some(base) = runtime.strip_suffix(" [host-fallback]") {
        return format!("{} ⚠️ (Host Fallback)", base);
    }
    runtime.to_string()
}

/// Whether a Ready-State session's disposable overlay (with its `.fc-session.json`
/// record) is still on disk. `"-"` for non-microVM rows. Shared by JSON + table so
/// they never diverge; pure (no VM) so it is unit-testable.
fn overlay_status(p: &crate::runtime::process::ProcessInfo) -> &'static str {
    match &p.ready_state_overlay_root {
        Some(root) if root.join(".fc-session.json").exists() => "present",
        Some(_) => "missing",
        None => "-",
    }
}

fn oci_session_visible(status: &OciSessionStatus, all: bool) -> bool {
    all || status.is_active()
}

fn oci_status_display(status: &OciSessionStatus) -> &'static str {
    match status {
        OciSessionStatus::Running => "🐳 running",
        OciSessionStatus::Stopped => "⚪ stopped",
        OciSessionStatus::StopFailed => "⚠️ stop_failed",
    }
}

pub fn execute(args: PsArgs, reporter: Arc<CliReporter>) -> Result<()> {
    let pm = ProcessManager::new()?;
    let cleaned = pm.cleanup_dead_processes_with_details()?;
    for process in &cleaned {
        let _ = binding::cleanup_service_bindings_for_process_info(process);
    }
    let mut processes = pm.list_processes()?;
    let import_previews = pm.list_import_preview_sessions().unwrap_or_default();

    if !args.all {
        processes.retain(|p| p.status.is_active());
    }

    if args.json {
        let json_output: Vec<serde_json::Value> = processes
            .iter()
            .map(|p| {
                let uptime = get_process_uptime(p.start_time)
                    .map(format_duration)
                    .unwrap_or_else(|_| "unknown".to_string());

                let dependency_contracts = pm
                    .read_dependency_session_snapshot(&p.id)
                    .ok()
                    .flatten()
                    .map(|snapshot| snapshot.providers)
                    .unwrap_or_default();

                serde_json::json!({
                    "id": p.id,
                    "name": p.name,
                    "pid": p.pid,
                    "workload_pid": p.workload_pid,
                    "status": p.status.to_string(),
                    "status_display": status_display(p.status),
                    "runtime": p.runtime,
                    "runtime_display": runtime_display(&p.runtime),
                    "uptime": uptime,
                    "manifest": p.manifest_path.as_ref().map(|m| m.display().to_string()),
                    "log_path": p.log_path.as_ref().map(|m| m.display().to_string()),
                    "dependency_contracts": dependency_contracts,
                    "ready_at": p.ready_at,
                    "last_event": p.last_event,
                    "last_error": p.last_error,
                    "exit_code": p.exit_code,
                    "port": p.requested_port,
                    // Ready-State (microVM) session metadata (additive; null/absent
                    // for legacy rows). Lets runners/tools identify + query a
                    // long-lived Ready-State session (backend, session key, overlay
                    // for GC, tap for net debugging).
                    "ready_state": p.runtime == "microvm",
                    "ready_state_backend_id": p.ready_state_backend_id,
                    "ready_state_session_id": p.ready_state_session_id,
                    "ready_state_overlay_root": p.ready_state_overlay_root.as_ref().map(|m| m.display().to_string()),
                    "ready_state_tap_dev": p.ready_state_tap_dev,
                    "ready_state_overlay_status": overlay_status(p)
                })
            })
            .collect();

        // Append OCI sessions to JSON output.
        let oci_sessions = OciSessionStore::new()
            .ok()
            .and_then(|s| s.list_sessions().ok())
            .unwrap_or_default();
        let machine_status = podman_machine_status();
        let oci_json: Vec<serde_json::Value> = oci_sessions
            .iter()
            .filter(|s| oci_session_visible(&s.status, args.all))
            .map(|s| {
                let ingress_json = s.ingress.as_ref().map(|i| {
                    serde_json::json!({
                        "mode": i.mode,
                        "router_port": i.router_port,
                        "primary_url": i.primary_url,
                        "routes": i.routes,
                        // Token is included in URLs; include it here for
                        // programmatic access alongside the session record.
                        "token": i.token,
                    })
                });
                serde_json::json!({
                    "kind": "oci",
                    "id": s.session_id,
                    "session_id": s.session_id,
                    "import_kind": s.import_kind,
                    "service_count": s.services.len(),
                    "main_endpoint": s.main_endpoint,
                    "ingress": ingress_json,
                    "status": s.status.to_string(),
                    "source_path": s.source_path,
                    "source_hash": s.source_hash,
                    "created_at": s.created_at,
                })
            })
            .collect();
        let mut combined = json_output;
        let import_preview_json: Vec<serde_json::Value> = import_previews
            .iter()
            .map(|s| {
                serde_json::json!({
                    "kind": "import_preview",
                    "id": s.run_session_id,
                    "run_session_id": s.run_session_id,
                    "pid": s.ato_run_pid,
                    "process_group_ids": s.process_group_ids,
                    "primary_port": s.primary_port,
                    "primary_url": s.primary_url,
                    "shadow_dir": s.shadow_dir.display().to_string(),
                    "log_path": s.log_path.display().to_string(),
                    "readiness_state": s.readiness_state,
                    "cleanup_policy": s.cleanup_policy,
                    "owner_kind": s.owner_kind,
                    "owner_pid": s.owner_pid,
                    "created_at_unix_ms": s.created_at_unix_ms,
                    "updated_at_unix_ms": s.updated_at_unix_ms,
                })
            })
            .collect();
        combined.extend(import_preview_json);
        combined.extend(oci_json);
        if machine_status.is_visible() {
            combined.push(podman_machine_json(&machine_status));
        }

        let output = serde_json::to_string_pretty(&combined)?;
        futures::executor::block_on(reporter.notify(output))?;
    } else {
        futures::executor::block_on(reporter.notify("📋 Listing capsule sessions...".to_string()))?;

        // Load OCI sessions.
        let oci_sessions = OciSessionStore::new()
            .ok()
            .and_then(|s| s.list_sessions().ok())
            .unwrap_or_default();
        let oci_visible: Vec<_> = oci_sessions
            .iter()
            .filter(|s| oci_session_visible(&s.status, args.all))
            .collect();
        let machine_status = podman_machine_status();
        let show_machine_status = machine_status.is_visible();

        if processes.is_empty()
            && import_previews.is_empty()
            && oci_visible.is_empty()
            && !show_machine_status
        {
            futures::executor::block_on(reporter.notify("No capsules found.".to_string()))?;
            return Ok(());
        }

        if processes.is_empty()
            && import_previews.is_empty()
            && oci_visible.is_empty()
            && show_machine_status
        {
            futures::executor::block_on(reporter.notify("Sessions: none".to_string()))?;
            futures::executor::block_on(
                reporter.notify(format!("Podman VM: {}", machine_status.display_status())),
            )?;
            return Ok(());
        }

        futures::executor::block_on(reporter.notify("-".repeat(100)))?;
        futures::executor::block_on(reporter.notify(format!(
            "{:>8} {:>8} {:>12} {:>15} {:>34} {}",
            "PID", "ID", "NAME", "STATUS", "RUNTIME", "UPTIME"
        )))?;
        futures::executor::block_on(reporter.notify("-".repeat(100)))?;

        for p in &processes {
            let uptime = get_process_uptime(p.start_time)
                .map(format_duration)
                .unwrap_or_else(|_| "unknown".to_string());

            let status_str = status_display(p.status);
            let runtime_str = runtime_display(&p.runtime);

            let name = if p.name.len() > 12 {
                &p.name[..12]
            } else {
                &p.name
            };

            let id = if p.id.len() > 8 { &p.id[..8] } else { &p.id };

            futures::executor::block_on(reporter.notify(format!(
                "{:>8} {:>8} {:>12} {:>15} {:>34} {}",
                p.pid, id, name, status_str, runtime_str, uptime
            )))?;

            if p.runtime == "microvm" {
                let f = |o: &Option<String>| o.clone().unwrap_or_else(|| "-".to_string());
                futures::executor::block_on(reporter.notify(format!(
                    "         ready-state: backend={} session={} port={} overlay={} tap={}",
                    f(&p.ready_state_backend_id),
                    f(&p.ready_state_session_id),
                    p.requested_port.map(|v| v.to_string()).unwrap_or_else(|| "-".to_string()),
                    overlay_status(p),
                    f(&p.ready_state_tap_dev),
                )))?;
            }

            if let Some(snapshot) = pm.read_dependency_session_snapshot(&p.id).ok().flatten()
                && !snapshot.providers.is_empty()
            {
                let deps = snapshot
                    .providers
                    .iter()
                    .map(|provider| {
                        let port = provider
                            .allocated_port
                            .map(|port| format!(", port=127.0.0.1:{port}"))
                            .unwrap_or_default();
                        format!("{}(pid={}{})", provider.alias, provider.pid, port)
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                futures::executor::block_on(reporter.notify(format!("         deps: {}", deps)))?;
            }
        }

        for s in &import_previews {
            let id = if s.run_session_id.len() > 8 {
                &s.run_session_id[..8]
            } else {
                &s.run_session_id
            };
            futures::executor::block_on(reporter.notify(format!(
                "{:>8} {:>8} {:>12} {:>15} {:>34} {}",
                s.ato_run_pid,
                id,
                "import",
                "🟢 preview",
                "source/import-preview",
                s.primary_url.as_deref().unwrap_or("-")
            )))?;
        }

        // Show OCI sessions as a separate section.
        for s in &oci_visible {
            let endpoint = s.main_endpoint.as_deref().unwrap_or("-");
            let id = if s.session_id.len() > 8 {
                &s.session_id[..8]
            } else {
                &s.session_id
            };
            futures::executor::block_on(reporter.notify(format!(
                "{:>8} {:>8} {:>12} {:>15} {:>34} {}",
                "—",
                id,
                s.import_kind,
                oci_status_display(&s.status),
                format!("oci/{}", s.import_kind),
                endpoint
            )))?;
        }

        futures::executor::block_on(reporter.notify("-".repeat(100)))?;
        futures::executor::block_on(reporter.notify(format!(
            "Total: {} capsule(s) ({} OCI)",
            processes.len() + import_previews.len() + oci_visible.len(),
            oci_visible.len()
        )))?;
        if show_machine_status {
            futures::executor::block_on(
                reporter.notify(format!("Podman VM: {}", machine_status.display_status())),
            )?;
        }
    }

    Ok(())
}

fn podman_machine_json(status: &PodmanMachineStatus) -> serde_json::Value {
    serde_json::json!({
        "kind": "oci_host",
        "id": "podman-machine",
        "provider": "podman",
        "status": status.status_label(),
        "status_display": status.display_status(),
        "machines": status.machine_names(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ps_args_default() {
        let args = PsArgs {
            json: false,
            all: false,
        };
        assert!(!args.json);
        assert!(!args.all);
    }

    #[test]
    fn test_ps_args_json() {
        let args = PsArgs {
            json: true,
            all: false,
        };
        assert!(args.json);
        assert!(!args.all);
    }

    #[test]
    fn test_ps_args_all() {
        let args = PsArgs {
            json: false,
            all: true,
        };
        assert!(!args.json);
        assert!(args.all);
    }

    #[test]
    fn runtime_display_adds_host_fallback_badge() {
        assert_eq!(
            runtime_display("source/node [host-fallback]"),
            "source/node ⚠️ (Host Fallback)"
        );
    }

    #[test]
    fn runtime_display_shows_microvm_plainly() {
        // Ready-State long-lived sessions render as a clear, ASCII, non-lying label.
        assert_eq!(runtime_display("microvm"), "microvm");
    }

    #[test]
    fn status_display_keeps_existing_ready_badge() {
        assert_eq!(status_display(ProcessStatus::Ready), "🟢 ready");
    }

    #[test]
    fn ps_default_includes_stop_failed_oci_sessions() {
        assert!(oci_session_visible(&OciSessionStatus::Running, false));
        assert!(oci_session_visible(&OciSessionStatus::StopFailed, false));
        assert!(!oci_session_visible(&OciSessionStatus::Stopped, false));
    }

    #[test]
    fn ps_text_renders_stop_failed_as_stop_failed_not_stopped() {
        assert_eq!(
            oci_status_display(&OciSessionStatus::StopFailed),
            "⚠️ stop_failed"
        );
        assert_ne!(
            oci_status_display(&OciSessionStatus::StopFailed),
            "⚪ stopped"
        );
    }
}
