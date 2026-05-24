use anyhow::Result;
use std::sync::Arc;

use crate::adapters::runtime::oci_session_store::{
    OciSessionIngressRecord, OciSessionStatus, OciSessionStore,
};
use crate::binding;
use crate::reporters::CliReporter;
use crate::runtime::process::{format_duration, get_process_uptime, ProcessManager, ProcessStatus};
use capsule_core::CapsuleReporter;

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

pub fn execute(args: PsArgs, reporter: Arc<CliReporter>) -> Result<()> {
    let pm = ProcessManager::new()?;
    let cleaned = pm.cleanup_dead_processes_with_details()?;
    for process in &cleaned {
        let _ = binding::cleanup_service_bindings_for_process_info(process);
    }
    let mut processes = pm.list_processes()?;

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
                    "exit_code": p.exit_code
                })
            })
            .collect();

        // Append OCI sessions to JSON output.
        let oci_sessions = OciSessionStore::new()
            .ok()
            .and_then(|s| s.list_sessions().ok())
            .unwrap_or_default();
        let oci_json: Vec<serde_json::Value> = oci_sessions
            .iter()
            .filter(|s| args.all || s.status == OciSessionStatus::Running)
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
        combined.extend(oci_json);

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
            .filter(|s| args.all || s.status == OciSessionStatus::Running)
            .collect();

        if processes.is_empty() && oci_visible.is_empty() {
            futures::executor::block_on(reporter.notify("No capsules found.".to_string()))?;
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

            if let Some(snapshot) = pm.read_dependency_session_snapshot(&p.id).ok().flatten() {
                if !snapshot.providers.is_empty() {
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
                    futures::executor::block_on(
                        reporter.notify(format!("         deps: {}", deps)),
                    )?;
                }
            }
        }

        // Show OCI sessions as a separate section.
        for s in &oci_visible {
            let status_icon = if s.status == OciSessionStatus::Running {
                "🐳 running"
            } else {
                "⚪ stopped"
            };
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
                status_icon,
                format!("oci/{}", s.import_kind),
                endpoint
            )))?;
        }

        futures::executor::block_on(reporter.notify("-".repeat(100)))?;
        futures::executor::block_on(reporter.notify(format!(
            "Total: {} capsule(s) ({} OCI)",
            processes.len() + oci_visible.len(),
            oci_visible.len()
        )))?;
    }

    Ok(())
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
    fn status_display_keeps_existing_ready_badge() {
        assert_eq!(status_display(ProcessStatus::Ready), "🟢 ready");
    }
}
