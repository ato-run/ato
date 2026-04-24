use anyhow::{Context, Result};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::reporters::CliReporter;
use crate::runtime::process::ProcessManager;
use capsule_core::CapsuleReporter;

const LOG_FILE_EXT: &str = ".log";

pub struct LogsArgs {
    pub id: Option<String>,
    pub name: Option<String>,
    pub follow: bool,
    pub tail: Option<usize>,
}

pub fn execute(args: LogsArgs, reporter: Arc<CliReporter>) -> Result<()> {
    let pm = ProcessManager::new()?;

    let (_process_info, log_path) = if let Some(id) = &args.id {
        let info = pm
            .read_pid(id)
            .with_context(|| format!("Failed to read PID file for: {}", id))?;
        let log_path = info.log_path.clone().unwrap_or_else(|| get_log_path(id));
        (info, log_path)
    } else if let Some(name) = &args.name {
        let processes = pm
            .find_by_name(name)
            .with_context(|| format!("Failed to find process by name: {}", name))?;

        if processes.is_empty() {
            anyhow::bail!("No capsule found with name: {}", name);
        }

        if processes.len() > 1 {
            futures::executor::block_on(reporter.warn(format!(
                "⚠️  Multiple capsules found with name '{}'. Using the first one.",
                name
            )))?;
        }

        let info = processes.into_iter().next().unwrap();
        let log_path = info
            .log_path
            .clone()
            .unwrap_or_else(|| get_log_path(&info.id));
        (info, log_path)
    } else {
        anyhow::bail!("Either --id or --name is required");
    };

    if !log_path.exists() {
        anyhow::bail!("Log file not found: {}", log_path.display());
    }

    if args.follow {
        follow_log(&log_path, args.tail, reporter.clone())?;
    } else {
        show_log(&log_path, args.tail)?;
    }

    Ok(())
}

fn get_log_path(id: &str) -> PathBuf {
    let home = capsule_core::common::paths::home_dir_or_workspace_tmp();
    home.join(".ato")
        .join("logs")
        .join(format!("{}{}", id, LOG_FILE_EXT))
}

fn show_log(log_path: &PathBuf, tail: Option<usize>) -> Result<()> {
    let file = File::open(log_path)
        .with_context(|| format!("Failed to open log file: {}", log_path.display()))?;
    let reader = BufReader::new(file);
    let lines: Vec<String> = reader.lines().collect::<Result<_, _>>()?;

    let start = if let Some(n) = tail {
        if n >= lines.len() {
            0
        } else {
            lines.len() - n
        }
    } else {
        0
    };

    for line in &lines[start..] {
        println!("{}", line);
    }

    Ok(())
}

fn follow_log(log_path: &PathBuf, tail: Option<usize>, reporter: Arc<CliReporter>) -> Result<()> {
    futures::executor::block_on(reporter.notify(format!(
        "📜 Following logs: {} (Ctrl+C to stop)",
        log_path.display()
    )))?;

    let file = File::open(log_path)
        .with_context(|| format!("Failed to open log file: {}", log_path.display()))?;

    let metadata = file.metadata()?;
    let initial_size = metadata.len();

    if tail.is_some() {
        let file = File::open(log_path)
            .with_context(|| format!("Failed to open log file: {}", log_path.display()))?;
        let reader = BufReader::new(file);
        let lines: Vec<String> = reader.lines().collect::<Result<_, _>>()?;

        let start = if let Some(n) = tail {
            if n >= lines.len() {
                0
            } else {
                lines.len() - n
            }
        } else {
            0
        };

        for line in &lines[start..] {
            println!("{}", line);
        }
    }

    let mut file = std::fs::OpenOptions::new().read(true).open(log_path)?;

    let mut position = initial_size;

    loop {
        let metadata = file.metadata()?;
        let current_size = metadata.len();

        if current_size > position {
            let mut diff = vec![0u8; (current_size - position) as usize];
            file.read_exact(&mut diff)?;
            position = current_size;

            let output = String::from_utf8_lossy(&diff);
            print!("{}", output);
            std::io::stdout().flush()?;
        }

        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_logs_args_by_id() {
        let args = LogsArgs {
            id: Some("test-123".to_string()),
            name: None,
            follow: false,
            tail: None,
        };
        assert!(args.id.is_some());
        assert!(args.name.is_none());
        assert!(!args.follow);
        assert!(args.tail.is_none());
    }

    #[test]
    fn test_logs_args_by_name() {
        let args = LogsArgs {
            id: None,
            name: Some("my-capsule".to_string()),
            follow: true,
            tail: Some(100),
        };
        assert!(args.id.is_none());
        assert!(args.name.is_some());
        assert!(args.follow);
        assert_eq!(args.tail, Some(100));
    }

    #[test]
    fn test_log_file_ext() {
        assert_eq!(LOG_FILE_EXT, ".log");
    }

    #[test]
    fn test_get_log_path() {
        let path = get_log_path("test-capsule");
        let path_str = path.to_string_lossy();
        assert!(path_str.contains("test-capsule.log"));
        assert!(path_str.contains(".ato/logs"));
    }

    #[test]
    fn test_get_log_path_special_chars() {
        let path = get_log_path("capsule-123-abc");
        let path_str = path.to_string_lossy();
        assert!(path_str.contains("capsule-123-abc.log"));
    }
}
