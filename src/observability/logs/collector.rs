use anyhow::{Context, Result};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tracing::{debug, error, warn};

/// LogEntry represents a single log line with metadata
#[derive(Clone, Debug)]
pub struct LogEntry {
    pub timestamp: u64,
    pub stream: LogStreamType,
    pub line: String,
}

/// Type of log stream (stdout/stderr)
#[derive(Clone, Debug, PartialEq)]
pub enum LogStreamType {
    Stdout,
    Stderr,
}

impl std::fmt::Display for LogStreamType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LogStreamType::Stdout => write!(f, "stdout"),
            LogStreamType::Stderr => write!(f, "stderr"),
        }
    }
}

/// LogStream provides access to streaming logs for a capsule
pub struct LogStream {
    receiver: mpsc::Receiver<LogEntry>,
}

impl LogStream {
    /// Receive the next log entry
    pub async fn next(&mut self) -> Option<LogEntry> {
        self.receiver.recv().await
    }

    /// Try to receive the next log entry without blocking
    pub fn try_next(&mut self) -> Option<LogEntry> {
        self.receiver.try_recv().ok()
    }
}

/// LogCollector monitors container log files and provides streaming access
pub struct LogCollector {
    watchers: Arc<Mutex<HashMap<String, Box<dyn Watcher + Send>>>>,
    log_files: Arc<Mutex<HashMap<String, LogFileState>>>,
}

struct LogFileState {
    path: PathBuf,
    position: u64,
    stream_type: LogStreamType,
}

impl LogCollector {
    /// Create a new log collector
    pub fn new() -> Self {
        Self {
            watchers: Arc::new(Mutex::new(HashMap::new())),
            log_files: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Start collecting logs for a capsule
    /// Returns a LogStream that can be used to receive log entries
    pub fn start_collecting(&self, capsule_id: String, log_path: PathBuf) -> Result<LogStream> {
        let (tx, rx) = mpsc::channel(1000);

        // Register log file for stdout (main log file)
        let stdout_key = format!("{}-stdout", capsule_id);
        {
            let mut files = self.log_files.lock().unwrap();
            files.insert(
                stdout_key.clone(),
                LogFileState {
                    path: log_path.clone(),
                    position: 0,
                    stream_type: LogStreamType::Stdout,
                },
            );
        }

        // Read any existing log content
        self.read_existing_logs(&stdout_key, &tx)?;

        // Set up file watcher
        self.setup_watcher(capsule_id.clone(), log_path, tx)?;

        Ok(LogStream { receiver: rx })
    }

    /// Stop collecting logs for a capsule
    pub fn stop_collecting(&self, capsule_id: &str) -> Result<()> {
        // Remove watcher
        {
            let mut watchers = self.watchers.lock().unwrap();
            watchers.remove(capsule_id);
        }

        // Remove log file states
        {
            let mut files = self.log_files.lock().unwrap();
            files.retain(|k, _| !k.starts_with(capsule_id));
        }

        debug!(capsule_id = %capsule_id, "Stopped log collection");
        Ok(())
    }

    /// Get historical logs for a capsule
    pub fn get_logs(&self, log_path: &Path, tail_lines: Option<usize>) -> Result<Vec<LogEntry>> {
        let file = File::open(log_path)
            .context(format!("Failed to open log file: {}", log_path.display()))?;

        let reader = BufReader::new(file);
        let lines: Vec<String> = reader.lines().collect::<Result<Vec<_>, _>>()?;

        let entries: Vec<LogEntry> = if let Some(tail) = tail_lines {
            lines
                .iter()
                .rev()
                .take(tail)
                .rev()
                .map(|line| LogEntry {
                    timestamp: current_timestamp(),
                    stream: LogStreamType::Stdout,
                    line: line.clone(),
                })
                .collect()
        } else {
            lines
                .iter()
                .map(|line| LogEntry {
                    timestamp: current_timestamp(),
                    stream: LogStreamType::Stdout,
                    line: line.clone(),
                })
                .collect()
        };

        Ok(entries)
    }

    fn read_existing_logs(&self, file_key: &str, tx: &mpsc::Sender<LogEntry>) -> Result<()> {
        let (path, stream_type) = {
            let files = self.log_files.lock().unwrap();
            let state = files.get(file_key).context("Log file state not found")?;
            (state.path.clone(), state.stream_type.clone())
        };

        if !path.exists() {
            debug!("Log file does not exist yet: {}", path.display());
            return Ok(());
        }

        let file =
            File::open(&path).context(format!("Failed to open log file: {}", path.display()))?;

        let reader = BufReader::new(file);
        let mut position = 0u64;

        for line_result in reader.lines() {
            let line = line_result?;
            position += (line.len() + 1) as u64; // +1 for newline

            let entry = LogEntry {
                timestamp: current_timestamp(),
                stream: stream_type.clone(),
                line,
            };

            if tx.try_send(entry).is_err() {
                warn!("Log stream buffer full, dropping old entries");
                break;
            }
        }

        // Update position
        {
            let mut files = self.log_files.lock().unwrap();
            if let Some(state) = files.get_mut(file_key) {
                state.position = position;
            }
        }

        Ok(())
    }

    fn setup_watcher(
        &self,
        capsule_id: String,
        log_path: PathBuf,
        tx: mpsc::Sender<LogEntry>,
    ) -> Result<()> {
        let log_files = Arc::clone(&self.log_files);
        let file_key = format!("{}-stdout", capsule_id);

        let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
            match res {
                Ok(event) => {
                    if matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                        // Read new log lines
                        if let Err(e) = Self::read_new_lines(&log_files, &file_key, &tx) {
                            error!("Failed to read new log lines: {}", e);
                        }
                    }
                }
                Err(e) => error!("Watch error: {}", e),
            }
        })?;

        // Watch the log file
        watcher.watch(&log_path, RecursiveMode::NonRecursive)?;

        // Store watcher
        {
            let mut watchers = self.watchers.lock().unwrap();
            watchers.insert(capsule_id.clone(), Box::new(watcher));
        }

        debug!(
            capsule_id = %capsule_id,
            log_path = %log_path.display(),
            "Started watching log file"
        );

        Ok(())
    }

    fn read_new_lines(
        log_files: &Arc<Mutex<HashMap<String, LogFileState>>>,
        file_key: &str,
        tx: &mpsc::Sender<LogEntry>,
    ) -> Result<()> {
        let (path, stream_type, position) = {
            let files = log_files.lock().unwrap();
            let state = files.get(file_key).context("Log file state not found")?;
            (
                state.path.clone(),
                state.stream_type.clone(),
                state.position,
            )
        };

        let mut file = File::open(&path)?;
        file.seek(SeekFrom::Start(position))?;

        let reader = BufReader::new(file);
        let mut new_position = position;

        for line_result in reader.lines() {
            let line = line_result?;
            new_position += (line.len() + 1) as u64; // +1 for newline

            let entry = LogEntry {
                timestamp: current_timestamp(),
                stream: stream_type.clone(),
                line,
            };

            if tx.try_send(entry).is_err() {
                warn!("Log stream buffer full, dropping new entries");
                break;
            }
        }

        // Update position
        {
            let mut files = log_files.lock().unwrap();
            if let Some(state) = files.get_mut(file_key) {
                state.position = new_position;
            }
        }

        Ok(())
    }
}

impl Default for LogCollector {
    fn default() -> Self {
        Self::new()
    }
}

fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_get_logs_empty_file() {
        let collector = LogCollector::new();
        let temp_file = NamedTempFile::new().unwrap();

        let logs = collector.get_logs(temp_file.path(), None).unwrap();
        assert_eq!(logs.len(), 0);
    }

    #[test]
    fn test_get_logs_with_content() {
        let collector = LogCollector::new();
        let mut temp_file = NamedTempFile::new().unwrap();

        writeln!(temp_file, "Line 1").unwrap();
        writeln!(temp_file, "Line 2").unwrap();
        writeln!(temp_file, "Line 3").unwrap();
        temp_file.flush().unwrap();

        let logs = collector.get_logs(temp_file.path(), None).unwrap();
        assert_eq!(logs.len(), 3);
        assert_eq!(logs[0].line, "Line 1");
        assert_eq!(logs[1].line, "Line 2");
        assert_eq!(logs[2].line, "Line 3");
    }

    #[test]
    fn test_get_logs_tail() {
        let collector = LogCollector::new();
        let mut temp_file = NamedTempFile::new().unwrap();

        for i in 1..=10 {
            writeln!(temp_file, "Line {}", i).unwrap();
        }
        temp_file.flush().unwrap();

        let logs = collector.get_logs(temp_file.path(), Some(3)).unwrap();
        assert_eq!(logs.len(), 3);
        assert_eq!(logs[0].line, "Line 8");
        assert_eq!(logs[1].line, "Line 9");
        assert_eq!(logs[2].line, "Line 10");
    }

    #[test]
    fn test_log_stream_type_to_string() {
        assert_eq!(LogStreamType::Stdout.to_string(), "stdout");
        assert_eq!(LogStreamType::Stderr.to_string(), "stderr");
    }
}
