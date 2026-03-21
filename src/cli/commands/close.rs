use anyhow::Result;
use std::sync::Arc;

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

        if running.is_empty() {
            futures::executor::block_on(reporter.notify("No active capsules.".to_string()))?;
            return Ok(());
        }

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

        futures::executor::block_on(reporter.notify(format!("✅ Stopped {} capsule(s)", stopped)))?;
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
