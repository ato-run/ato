//! Share URL execution via nacelle sandbox.
//!
//! Provides a unified API for running share URLs in both CLI (blocking) and
//! Desktop (async PTY streaming) contexts. The executor materializes the share
//! workspace using `ato decap`, then spawns nacelle to run the entry command
//! inside a sandbox.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};

use anyhow::{bail, Context, Result};
use tracing::{error, info, warn};

use super::types::{ShareEntrySpec, ShareSpec, WorkspaceShareState, SHARE_STATE_FILE};

// ── Public types ─────────────────────────────────────────────────────────────

/// How nacelle I/O is wired.
pub enum ShareExecutionMode {
    /// CLI: nacelle inherits stdin/stdout/stderr, blocks until exit.
    Inherited,
    /// Desktop: nacelle stdin/stdout piped, returns channels for PTY streaming.
    Piped { cols: u16, rows: u16 },
}

/// Request to execute a share URL.
pub struct ShareRunRequest {
    /// Share URL (`https://ato.run/s/...`) or local share path.
    pub input: String,
    /// Entry selector — `None` auto-selects primary.
    pub entry: Option<String>,
    /// Extra args appended to the entry's run command.
    pub extra_args: Vec<String>,
    /// Environment variable overlay.
    pub env_overlay: BTreeMap<String, String>,
    /// Execution mode.
    pub mode: ShareExecutionMode,
    /// Override nacelle binary path.
    pub nacelle_path: Option<PathBuf>,
    /// Override ato binary path (for decap).
    pub ato_path: Option<PathBuf>,
}

/// A live piped session with channels for PTY I/O.
pub struct SharePipedSession {
    pub session_id: String,
    pub input_tx: Sender<Vec<u8>>,
    pub resize_tx: Sender<(u16, u16)>,
    pub output_rx: Receiver<String>,
}

/// Result of share execution.
pub enum ShareExecutionResult {
    /// Blocking execution completed.
    Completed { exit_code: i32 },
    /// Piped process spawned for async streaming.
    Spawned(SharePipedSession),
}

// ── Main entry point ─────────────────────────────────────────────────────────

/// Execute a share URL through nacelle sandbox.
///
/// 1. Materializes the share workspace via `ato decap`
/// 2. Reads the entry from the materialized spec
/// 3. Spawns nacelle with the entry command
pub fn execute_share(request: ShareRunRequest) -> Result<ShareExecutionResult> {
    // Step 1: Create workspace directory
    let workspace = share_workspace_dir(&request.input)?;
    std::fs::create_dir_all(&workspace)
        .with_context(|| format!("failed to create workspace {}", workspace.display()))?;

    // Step 2: Materialize via ato decap
    let ato_bin = resolve_ato_binary(request.ato_path.as_deref())?;
    decap_into(&ato_bin, &request.input, &workspace)?;

    // Step 3: Read materialized state to find the spec
    let spec = load_materialized_spec(&workspace)?;
    let entry = select_entry(&spec, request.entry.as_deref())?;

    // Step 4: Build run command
    let mut run_command = entry.run.clone();
    if !request.extra_args.is_empty() {
        run_command.push(' ');
        run_command.push_str(&shell_words::join(
            request.extra_args.iter().map(String::as_str),
        ));
    }
    let run_cwd = workspace.join(&entry.cwd);

    // Step 5: Resolve nacelle
    let nacelle_bin = resolve_nacelle_binary(request.nacelle_path.as_deref())?;

    // Step 6: Build envelope and spawn nacelle
    let env_pairs: Vec<(String, String)> = request.env_overlay.into_iter().collect();

    match request.mode {
        ShareExecutionMode::Inherited => {
            let exit_code =
                spawn_nacelle_inherited(&nacelle_bin, &run_command, &run_cwd, &env_pairs)?;
            // Workspace is intentionally kept for caching — decap_into reuses it
            // on the next invocation if state.json shows all sources are ok.
            Ok(ShareExecutionResult::Completed { exit_code })
        }
        ShareExecutionMode::Piped { cols, rows } => {
            // NOTE: workspace cleanup for Piped mode is the caller's responsibility.
            // The workspace must remain alive while the nacelle process runs.
            // The caller should clean up when the terminal session ends.
            let session =
                spawn_nacelle_piped(&nacelle_bin, &run_command, &run_cwd, &env_pairs, cols, rows)?;
            Ok(ShareExecutionResult::Spawned(session))
        }
    }
}

// ── Internal helpers ─────────────────────────────────────────────────────────

/// Compute a stable workspace directory for a share URL.
fn share_workspace_dir(input: &str) -> Result<PathBuf> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    input.hash(&mut hasher);
    let hash = hasher.finish();
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| anyhow::anyhow!("HOME not set"))?;
    Ok(home
        .join(".ato")
        .join("apps")
        .join("share-runs")
        .join(format!("{hash:016x}")))
}

/// Run `ato decap <input> --into <workspace>`.
fn decap_into(ato_bin: &Path, input: &str, workspace: &Path) -> Result<()> {
    // Check if already materialized (state.json exists and sources are ok)
    let state_path = workspace.join(".ato").join("share").join(SHARE_STATE_FILE);
    if state_path.exists() {
        if let Ok(raw) = std::fs::read_to_string(&state_path) {
            if let Ok(state) = serde_json::from_str::<WorkspaceShareState>(&raw) {
                let all_ok = state
                    .sources
                    .iter()
                    .all(|s| s.status == "ok");
                if all_ok && !state.sources.is_empty() {
                    info!(input, "reusing cached decap workspace");
                    return Ok(());
                }
            }
        }
        // Stale or broken — clear and re-decap
        warn!(input, "clearing stale workspace for re-decap");
        let _ = std::fs::remove_dir_all(workspace);
        std::fs::create_dir_all(workspace)?;
    }

    info!(input, dest = %workspace.display(), "running ato decap");
    let output = Command::new(ato_bin)
        .args(["decap", input, "--into"])
        .arg(workspace)
        .output()
        .with_context(|| format!("failed to spawn ato decap for {input}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        error!(input, %stderr, "ato decap failed");
        bail!("ato decap failed for {input}: {stderr}");
    }
    info!(input, "ato decap completed");
    Ok(())
}

/// Read the share spec from a materialized workspace.
fn load_materialized_spec(workspace: &Path) -> Result<ShareSpec> {
    let state_path = workspace.join(".ato").join("share").join(SHARE_STATE_FILE);
    let state_raw = std::fs::read_to_string(&state_path)
        .with_context(|| format!("no state.json in workspace {}", workspace.display()))?;
    let _state: WorkspaceShareState = serde_json::from_str(&state_raw)?;

    // Try to find share.spec.json
    let spec_path = workspace.join(".ato").join("share").join("share.spec.json");
    if spec_path.exists() {
        let spec_raw = std::fs::read_to_string(&spec_path)?;
        return serde_json::from_str(&spec_raw)
            .with_context(|| "failed to parse share.spec.json".to_string());
    }

    // Fallback: try to load from the decap output
    bail!(
        "share.spec.json not found in workspace {}",
        workspace.display()
    )
}

/// Select an entry from the spec.
fn select_entry(spec: &ShareSpec, selector: Option<&str>) -> Result<ShareEntrySpec> {
    let entries = if !spec.entries.is_empty() {
        spec.entries.clone()
    } else {
        bail!("share has no entries to run");
    };

    if let Some(sel) = selector {
        if let Some(entry) = entries.iter().find(|e| e.id == sel || e.label == sel) {
            return Ok(entry.clone());
        }
        bail!("entry '{sel}' not found in share");
    }

    // Auto-select: prefer primary
    if let Some(entry) = entries.iter().find(|e| e.primary) {
        return Ok(entry.clone());
    }
    if entries.len() == 1 {
        return Ok(entries[0].clone());
    }
    bail!(
        "share has {} entries but none is primary — specify --entry",
        entries.len()
    );
}

/// Resolve the ato binary path.
fn resolve_ato_binary(override_path: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = override_path {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        bail!("ato binary not found at {}", path.display());
    }
    if let Some(path) = std::env::var_os("ATO_DESKTOP_ATO_BIN").map(PathBuf::from) {
        if path.is_file() {
            return Ok(path);
        }
    }
    // Search PATH
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    for entry in std::env::split_paths(&path_var) {
        let candidate = entry.join("ato");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    bail!("ato binary not found on PATH")
}

/// Resolve the nacelle binary path.
fn resolve_nacelle_binary(override_path: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = override_path {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        bail!("nacelle binary not found at {}", path.display());
    }
    if let Some(path) = std::env::var_os("NACELLE_PATH").map(PathBuf::from) {
        if path.is_file() {
            return Ok(path);
        }
    }
    // Try capsule-core engine discovery
    match crate::engine::discover_nacelle(crate::engine::EngineRequest {
        explicit_path: None,
        manifest_path: None,
        compat_input: None,
    }) {
        Ok(path) => Ok(path),
        Err(_) => {
            // Fallback: search PATH
            let path_var = std::env::var_os("PATH").unwrap_or_default();
            for entry in std::env::split_paths(&path_var) {
                let candidate = entry.join("nacelle");
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
            bail!("nacelle binary not found — set NACELLE_PATH or install nacelle on PATH")
        }
    }
}

/// Spawn nacelle with inherited stdio (CLI mode). Returns exit code.
fn spawn_nacelle_inherited(
    nacelle_bin: &Path,
    run_command: &str,
    cwd: &Path,
    env_pairs: &[(String, String)],
) -> Result<i32> {
    let envelope = build_envelope(run_command, cwd, env_pairs, false, 80, 24);
    let envelope_json = serde_json::to_string(&envelope)?;

    // Write envelope to temp file
    let tmp_dir = cwd.join(".tmp");
    std::fs::create_dir_all(&tmp_dir).ok();
    let envelope_path = tmp_dir.join("share-exec.json");
    std::fs::write(&envelope_path, &envelope_json)?;

    info!(cmd = run_command, cwd = %cwd.display(), "spawning nacelle (inherited)");
    let status = Command::new(nacelle_bin)
        .args(["internal", "--input", &envelope_path.to_string_lossy(), "exec"])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("failed to spawn nacelle at {}", nacelle_bin.display()))?;

    let _ = std::fs::remove_file(&envelope_path);
    Ok(status.code().unwrap_or(1))
}

/// Spawn nacelle with piped stdio (Desktop mode). Returns PTY session.
fn spawn_nacelle_piped(
    nacelle_bin: &Path,
    run_command: &str,
    cwd: &Path,
    env_pairs: &[(String, String)],
    cols: u16,
    rows: u16,
) -> Result<SharePipedSession> {
    let envelope = build_envelope(run_command, cwd, env_pairs, true, cols, rows);
    let envelope_json = serde_json::to_string(&envelope)?;

    // Write envelope to temp file
    let tmp_dir = cwd.join(".tmp");
    std::fs::create_dir_all(&tmp_dir).ok();
    let envelope_path = tmp_dir.join("share-exec.json");
    std::fs::write(&envelope_path, &envelope_json)?;

    info!(cmd = run_command, cwd = %cwd.display(), cols, rows, "spawning nacelle (piped)");
    let mut child = Command::new(nacelle_bin)
        .args(["internal", "--input", &envelope_path.to_string_lossy(), "exec"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("failed to spawn nacelle at {}", nacelle_bin.display()))?;

    let mut nacelle_stdin = child.stdin.take().context("nacelle stdin unavailable")?;
    let nacelle_stdout = child.stdout.take().context("nacelle stdout unavailable")?;

    let (input_tx, input_rx): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = channel();
    let (resize_tx, resize_rx): (Sender<(u16, u16)>, Receiver<(u16, u16)>) = channel();
    let (output_tx, output_rx): (Sender<String>, Receiver<String>) = channel();

    let session_id = format!("share-{}", child.id());
    let sid = session_id.clone();
    let envelope_cleanup = envelope_path.clone();

    // Thread: nacelle stdout → output_tx
    std::thread::spawn(move || {
        let reader = BufReader::new(nacelle_stdout);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            match value.get("event").and_then(|e| e.as_str()) {
                Some("terminal_data") => {
                    if let Some(b64) = value.get("data_b64").and_then(|d| d.as_str()) {
                        if output_tx.send(b64.to_string()).is_err() {
                            break;
                        }
                    }
                }
                Some("terminal_exited") => {
                    let code = value.get("exit_code").and_then(|c| c.as_i64());
                    info!(session_id = %sid, exit_code = ?code, "share terminal session exited");
                    break;
                }
                _ => {}
            }
        }
        let _ = std::fs::remove_file(&envelope_cleanup);
    });

    let sid2 = session_id.clone();

    // Thread: input_rx + resize_rx → nacelle stdin
    std::thread::spawn(move || {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine as _;

        loop {
            while let Ok(data) = input_rx.try_recv() {
                let cmd = serde_json::json!({
                    "type": "terminal_input",
                    "session_id": sid2,
                    "data_b64": STANDARD.encode(&data)
                });
                if writeln!(nacelle_stdin, "{}", cmd).is_err() {
                    return;
                }
                let _ = nacelle_stdin.flush();
            }
            while let Ok((c, r)) = resize_rx.try_recv() {
                let cmd = serde_json::json!({
                    "type": "terminal_resize",
                    "session_id": sid2,
                    "cols": c,
                    "rows": r
                });
                if writeln!(nacelle_stdin, "{}", cmd).is_err() {
                    return;
                }
                let _ = nacelle_stdin.flush();
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    });

    Ok(SharePipedSession {
        session_id,
        input_tx,
        resize_tx,
        output_rx,
    })
}

/// Build a nacelle ExecEnvelope JSON value.
fn build_envelope(
    run_command: &str,
    cwd: &Path,
    env_pairs: &[(String, String)],
    interactive: bool,
    cols: u16,
    rows: u16,
) -> serde_json::Value {
    let mut envelope = serde_json::json!({
        "spec_version": "1.0",
        "workload": {
            "type": "shell",
            "cmd": ["/bin/sh", "-lc", run_command]
        },
        "interactive": interactive,
        "cwd": cwd.display().to_string(),
    });

    if !env_pairs.is_empty() {
        envelope["env"] = serde_json::json!(env_pairs);
    }

    if interactive {
        envelope["terminal"] = serde_json::json!({
            "cols": cols,
            "rows": rows,
            "env_filter": "safe"
        });
    }

    envelope
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_workspace_dir_is_stable() {
        let dir1 = share_workspace_dir("https://ato.run/s/abc123").unwrap();
        let dir2 = share_workspace_dir("https://ato.run/s/abc123").unwrap();
        assert_eq!(dir1, dir2);

        let dir3 = share_workspace_dir("https://ato.run/s/different").unwrap();
        assert_ne!(dir1, dir3);
    }

    #[test]
    fn build_envelope_inherited() {
        let env = vec![("FOO".to_string(), "bar".to_string())];
        let envelope = build_envelope("python main.py", Path::new("/workspace"), &env, false, 80, 24);
        assert_eq!(envelope["workload"]["type"], "shell");
        assert_eq!(envelope["workload"]["cmd"][2], "python main.py");
        assert_eq!(envelope["interactive"], false);
        assert!(envelope.get("terminal").is_none());
    }

    #[test]
    fn build_envelope_piped() {
        let envelope = build_envelope("python main.py", Path::new("/workspace"), &[], true, 120, 40);
        assert_eq!(envelope["interactive"], true);
        assert_eq!(envelope["terminal"]["cols"], 120);
        assert_eq!(envelope["terminal"]["rows"], 40);
    }

    #[test]
    fn select_entry_auto_selects_primary() {
        let spec = ShareSpec {
            schema_version: "2".to_string(),
            name: "test".to_string(),
            root: ".".to_string(),
            sources: vec![],
            tool_requirements: vec![],
            env_requirements: vec![],
            install_steps: vec![],
            entries: vec![
                ShareEntrySpec {
                    id: "secondary".to_string(),
                    label: "Secondary".to_string(),
                    cwd: ".".to_string(),
                    run: "echo secondary".to_string(),
                    kind: "command".to_string(),
                    primary: false,
                    depends_on: vec![],
                    env: Default::default(),
                    evidence: vec![],
                },
                ShareEntrySpec {
                    id: "primary".to_string(),
                    label: "Primary".to_string(),
                    cwd: ".".to_string(),
                    run: "echo primary".to_string(),
                    kind: "command".to_string(),
                    primary: true,
                    depends_on: vec![],
                    env: Default::default(),
                    evidence: vec![],
                },
            ],
            services: vec![],
            notes: Default::default(),
            generated_from: super::super::types::GeneratedFrom {
                root_path: ".".to_string(),
                captured_at: "2026-01-01T00:00:00Z".to_string(),
                host_os: "macos".to_string(),
            },
        };

        let entry = select_entry(&spec, None).unwrap();
        assert_eq!(entry.id, "primary");

        let entry = select_entry(&spec, Some("secondary")).unwrap();
        assert_eq!(entry.id, "secondary");

        assert!(select_entry(&spec, Some("nonexistent")).is_err());
    }
}
