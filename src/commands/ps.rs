use anyhow::Result;
use std::sync::Arc;

use crate::binding;
use crate::process_manager::{format_duration, get_process_uptime, ProcessManager, ProcessStatus};
use crate::reporters::CliReporter;
use capsule_core::CapsuleReporter;

pub struct PsArgs {
    pub json: bool,
    pub all: bool,
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

                serde_json::json!({
                    "id": p.id,
                    "name": p.name,
                    "pid": p.pid,
                    "workload_pid": p.workload_pid,
                    "status": p.status.to_string(),
                    "runtime": p.runtime,
                    "uptime": uptime,
                    "manifest": p.manifest_path.as_ref().map(|m| m.display().to_string()),
                    "log_path": p.log_path.as_ref().map(|m| m.display().to_string()),
                    "ready_at": p.ready_at,
                    "last_event": p.last_event,
                    "last_error": p.last_error,
                    "exit_code": p.exit_code
                })
            })
            .collect();

        let output = serde_json::to_string_pretty(&json_output)?;
        futures::executor::block_on(reporter.notify(output))?;
    } else {
        futures::executor::block_on(reporter.notify("📋 Listing capsule sessions...".to_string()))?;

        if processes.is_empty() {
            futures::executor::block_on(reporter.notify("No capsules found.".to_string()))?;
            return Ok(());
        }

        futures::executor::block_on(reporter.notify("-".repeat(100)))?;
        futures::executor::block_on(reporter.notify(format!(
            "{:>8} {:>8} {:>12} {:>15} {:>20} {}",
            "PID", "ID", "NAME", "STATUS", "RUNTIME", "UPTIME"
        )))?;
        futures::executor::block_on(reporter.notify("-".repeat(100)))?;

        for p in &processes {
            let uptime = get_process_uptime(p.start_time)
                .map(format_duration)
                .unwrap_or_else(|_| "unknown".to_string());

            let status_str = match p.status {
                ProcessStatus::Starting => "🟡 starting",
                ProcessStatus::Ready => "🟢 ready",
                ProcessStatus::Running => "🟢 running",
                ProcessStatus::Exited => "⚪ exited",
                ProcessStatus::Failed => "🔴 failed",
                ProcessStatus::Stopped => "⚪ stopped",
                ProcessStatus::Unknown => "🟡 unknown",
            };

            let name = if p.name.len() > 12 {
                &p.name[..12]
            } else {
                &p.name
            };

            let id = if p.id.len() > 8 { &p.id[..8] } else { &p.id };

            futures::executor::block_on(reporter.notify(format!(
                "{:>8} {:>8} {:>12} {:>15} {:>20} {}",
                p.pid, id, name, status_str, p.runtime, uptime
            )))?;
        }

        futures::executor::block_on(reporter.notify("-".repeat(100)))?;
        futures::executor::block_on(
            reporter.notify(format!("Total: {} capsule(s)", processes.len())),
        )?;
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
}
