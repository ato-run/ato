use anyhow::Result;
use std::sync::Arc;

use crate::adapters::runtime::oci_session_store::{
    stop_oci_session, OciSessionStatus, OciSessionStore,
};
use crate::reporters::CliReporter;
use crate::runtime::process::ProcessManager;
use capsule_core::CapsuleReporter;

pub struct CloseArgs {
    pub id: Option<String>,
    pub name: Option<String>,
    pub all: bool,
    pub force: bool,
}

pub fn execute(args: CloseArgs, reporter: Arc<CliReporter>) -> Result<()> {
    let pm = ProcessManager::new()?;

    if args.all && args.name.is_none() && args.id.is_none() {
        let processes = pm.list_processes()?;
        let running: Vec<_> = processes.iter().filter(|p| p.status.is_active()).collect();

        // Check OCI sessions before stopping so we know total activity.
        let oci_running = OciSessionStore::new()
            .ok()
            .and_then(|s| s.list_sessions().ok())
            .map(|sessions| {
                sessions
                    .into_iter()
                    .filter(|s| s.status == OciSessionStatus::Running)
                    .count()
            })
            .unwrap_or(0);

        if running.is_empty() && oci_running == 0 {
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

        // Stop OCI sessions.
        stop_all_oci_sessions(&args, &reporter)?;

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
                futures::executor::block_on(
                    reporter.warn(format!("⚠️  Capsule {} is not running", id)),
                )?;
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

/// Stop all running OCI sessions (containers + networks).
fn stop_all_oci_sessions(args: &CloseArgs, reporter: &Arc<CliReporter>) -> Result<()> {
    let store = match OciSessionStore::new() {
        Ok(s) => s,
        Err(_) => return Ok(()), // No OCI sessions directory yet
    };
    let sessions = store.list_sessions().unwrap_or_default();
    let running: Vec<_> = sessions
        .iter()
        .filter(|s| s.status == OciSessionStatus::Running)
        .collect();

    for session in running {
        futures::executor::block_on(reporter.notify(format!(
            "🐳 Stopping OCI session {} ({}, {} service(s))...",
            session.session_id,
            session.import_kind,
            session.services.len()
        )))?;

        let result = stop_oci_session(session, args.force);

        for name in &result.stopped_containers {
            futures::executor::block_on(
                reporter.notify(format!("  ✅ Stopped container: {name}")),
            )?;
        }
        for name in &result.errors {
            futures::executor::block_on(reporter.warn(format!("  ⚠️  {name}")))?;
        }
        if result.network_removed {
            futures::executor::block_on(
                reporter.notify(format!("  🔗 Removed network: {}", session.network_name)),
            )?;
        }

        // Mark session as stopped (don't delete — user may want to inspect).
        let _ = store.delete_session(&session.session_id);
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
}
