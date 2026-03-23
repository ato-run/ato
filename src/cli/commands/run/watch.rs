use crate::env::read_env;
use crate::reporters::CliReporter;
use anyhow::{Context, Result};
use capsule_core::CapsuleReporter;
use notify::{event::Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ENV_DEBOUNCE_MS: &str = "CAPSULE_WATCH_DEBOUNCE_MS";

#[derive(Clone)]
enum PatternMatcher {
    Literal(String),
    Regex(Regex),
}

#[derive(Clone, Serialize, Deserialize)]
pub struct WatchConfig {
    pub watch_patterns: Vec<String>,
    pub ignore_patterns: Vec<String>,
    pub debounce_ms: u64,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            watch_patterns: vec![
                "**/*.rs".to_string(),
                "**/*.toml".to_string(),
                "**/*.json".to_string(),
            ],
            ignore_patterns: vec![
                "target/*".to_string(),
                "*.log".to_string(),
                "node_modules/*".to_string(),
                ".git/*".to_string(),
            ],
            debounce_ms: read_env(ENV_DEBOUNCE_MS)
                .and_then(|v| v.parse().ok())
                .unwrap_or(300),
        }
    }
}

pub struct CapsuleHandle {
    pub _target: PathBuf,
    pub _reporter: Arc<CliReporter>,
    pub _restart_lock: Arc<Mutex<bool>>,
    pub process_handle: Arc<Mutex<Option<Child>>>,
}

impl CapsuleHandle {
    pub fn stop(&self) -> Result<()> {
        let mut guard = self.process_handle.lock().unwrap();
        if let Some(mut process) = guard.take() {
            if let Ok(Some(_)) = process.try_wait() {
            } else {
                let _ = process.kill();
                let _ = process.wait();
            }
        }
        Ok(())
    }
}

pub fn watch_directory(
    target: PathBuf,
    config: WatchConfig,
    reporter: Arc<CliReporter>,
) -> Result<(RecommendedWatcher, CapsuleHandle)> {
    futures::executor::block_on(CapsuleReporter::notify(
        &*reporter,
        "👀 Watching for changes...".to_string(),
    ))
    .map_err(|e| anyhow::anyhow!("{:?}", e))?;

    let (tx, rx) = std::sync::mpsc::channel::<notify::Result<Event>>();
    let watcher_reporter = reporter.clone();
    let compiled_watch_patterns = compile_patterns(&config.watch_patterns);
    let compiled_ignore_patterns = compile_patterns(&config.ignore_patterns);
    let target_clone = target.clone();
    let restart_lock = Arc::new(Mutex::new(false));
    let restart_lock_for_handle = restart_lock.clone();
    let process_handle = Arc::new(Mutex::new(None));
    let process_handle_for_watcher = process_handle.clone();

    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<Event>| {
            let _ = tx.send(res);
        },
        notify::Config::default(),
    )
    .with_context(|| "Failed to create file watcher")?;

    watcher
        .watch(&target, RecursiveMode::Recursive)
        .with_context(|| format!("Failed to watch {}", target.display()))?;

    let last_trigger = Arc::new(Mutex::new(Instant::now()));
    let debounce_duration = Duration::from_millis(config.debounce_ms);

    let initial_process = run_capsule(&target_clone, &watcher_reporter)?;
    *process_handle.lock().unwrap() = Some(initial_process);

    std::thread::spawn(move || {
        while let Ok(Ok(event)) = rx.recv() {
            if !is_restart_event_kind(&event.kind) {
                continue;
            }

            let path = event.paths.first().cloned().unwrap_or_else(PathBuf::new);

            if is_symlink_path(&path) || should_ignore_compiled(&path, &compiled_ignore_patterns) {
                continue;
            }

            if !should_watch_compiled(&path, &compiled_watch_patterns) {
                continue;
            }

            let mut last_trigger = last_trigger.lock().unwrap();
            let elapsed = last_trigger.elapsed();

            if elapsed < debounce_duration {
                continue;
            }

            *last_trigger = Instant::now();
            drop(last_trigger);

            let mut lock = restart_lock.lock().unwrap();
            if *lock {
                continue;
            }
            *lock = true;
            drop(lock);

            if let Some(path_str) = path.to_str() {
                let _ = futures::executor::block_on(CapsuleReporter::notify(
                    &*watcher_reporter,
                    format!("📝 Changed: {}", path_str),
                ));
            }

            let _ = futures::executor::block_on(CapsuleReporter::notify(
                &*watcher_reporter,
                "🔄 Stopping capsule...".to_string(),
            ));

            {
                let mut guard = process_handle_for_watcher.lock().unwrap();
                if let Some(mut process) = guard.take() {
                    let _ = process.kill();
                    let _ = process.wait();
                }
            }

            let _ = futures::executor::block_on(CapsuleReporter::notify(
                &*watcher_reporter,
                "🚀 Starting capsule...".to_string(),
            ));

            match run_capsule(&target_clone, &watcher_reporter) {
                Ok(new_process) => {
                    let mut guard = process_handle_for_watcher.lock().unwrap();
                    *guard = Some(new_process);
                    let _ = futures::executor::block_on(CapsuleReporter::notify(
                        &*watcher_reporter,
                        "✅ Capsule restarted".to_string(),
                    ));
                }
                Err(e) => {
                    let _ = futures::executor::block_on(CapsuleReporter::warn(
                        &*watcher_reporter,
                        format!("⚠️  Failed to restart capsule: {}", e),
                    ));
                }
            }

            let mut lock = restart_lock.lock().unwrap();
            *lock = false;
            drop(lock);
        }
    });

    let capsule_handle = CapsuleHandle {
        _target: target.clone(),
        _reporter: reporter.clone(),
        _restart_lock: restart_lock_for_handle,
        process_handle,
    };

    Ok((watcher, capsule_handle))
}

fn run_capsule(target: &Path, _reporter: &Arc<CliReporter>) -> Result<Child> {
    let mut cmd = Command::new("capsule");
    cmd.arg("run");
    cmd.arg(target);
    cmd.env("CAPSULE_WATCH_MODE", "1");
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());

    let child = cmd.spawn().with_context(|| "Failed to run capsule")?;

    Ok(child)
}

fn is_restart_event_kind(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
    )
}

fn is_symlink_path(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
}

#[cfg(test)]
fn should_watch(path: &Path, watch_patterns: &[String]) -> bool {
    let compiled = compile_patterns(watch_patterns);
    should_watch_compiled(path, &compiled)
}

#[cfg(test)]
fn should_ignore(path: &Path, ignore_patterns: &[String]) -> bool {
    let compiled = compile_patterns(ignore_patterns);
    should_ignore_compiled(path, &compiled)
}

fn should_watch_compiled(path: &Path, watch_patterns: &[PatternMatcher]) -> bool {
    if watch_patterns.is_empty() {
        return true;
    }
    matches_compiled_patterns(path, watch_patterns)
}

fn should_ignore_compiled(path: &Path, ignore_patterns: &[PatternMatcher]) -> bool {
    matches_compiled_patterns(path, ignore_patterns)
}

fn matches_compiled_patterns(path: &Path, patterns: &[PatternMatcher]) -> bool {
    let path_norm = normalize_path(path);
    patterns.iter().any(|pattern| match pattern {
        PatternMatcher::Literal(literal) => path_norm.contains(literal),
        PatternMatcher::Regex(regex) => regex.is_match(&path_norm),
    })
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase()
}

fn compile_patterns(patterns: &[String]) -> Vec<PatternMatcher> {
    patterns
        .iter()
        .filter_map(|pattern| compile_pattern(pattern))
        .collect()
}

fn compile_pattern(pattern: &str) -> Option<PatternMatcher> {
    let normalized = pattern.replace('\\', "/").to_ascii_lowercase();
    if normalized.is_empty() {
        return None;
    }

    if !normalized.contains('*') && !normalized.contains('?') {
        return Some(PatternMatcher::Literal(normalized));
    }

    let mut regex_src = String::new();
    for ch in normalized.chars() {
        match ch {
            '*' => regex_src.push_str(".*"),
            '?' => regex_src.push('.'),
            _ => regex_src.push_str(&regex::escape(&ch.to_string())),
        }
    }

    match Regex::new(&regex_src) {
        Ok(regex) => Some(PatternMatcher::Regex(regex)),
        Err(_) => Some(PatternMatcher::Literal(normalized.replace('*', ""))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignore_patterns_match_globs() {
        let path = PathBuf::from("/tmp/work/target/debug/build.log");
        let patterns = vec!["target/*".to_string(), "*.log".to_string()];
        assert!(should_ignore(&path, &patterns));
    }

    #[test]
    fn watch_patterns_filter_unmatched_files() {
        let path = PathBuf::from("/tmp/work/source/main.py");
        let patterns = vec!["**/*.rs".to_string(), "**/*.toml".to_string()];
        assert!(!should_watch(&path, &patterns));
    }

    #[test]
    fn watch_patterns_match_expected_files() {
        let path = PathBuf::from("/tmp/work/source/lib.rs");
        let patterns = vec!["**/*.rs".to_string(), "**/*.toml".to_string()];
        assert!(should_watch(&path, &patterns));
    }
}
