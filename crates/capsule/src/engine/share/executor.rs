//! Share workspace execution via nacelle sandbox.
//!
//! Provides a unified API for running shared workspaces in both CLI (blocking)
//! and Desktop (async PTY streaming) contexts. The executor materializes the
//! share workspace using `ato workspace setup`, then spawns nacelle to run the
//! entry command inside a sandbox.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};

type ResizeTx = Sender<(u16, u16)>;
type ResizeRx = Receiver<(u16, u16)>;

use tracing::{error, info, warn};

use crate::error::{CapsuleError, Result};

use super::types::{
    SHARE_LOCK_FILE, SHARE_SPEC_FILE, SHARE_STATE_FILE, ShareEntrySpec, ShareSpec,
    WorkspaceShareState,
};

// ── Public types ─────────────────────────────────────────────────────────────

/// How nacelle I/O is wired.
pub enum ShareExecutionMode {
    /// CLI: nacelle inherits stdin/stdout/stderr, blocks until exit.
    Inherited,
    /// Desktop: nacelle stdin/stdout piped, returns channels for PTY streaming.
    Piped { cols: u16, rows: u16 },
}

/// Request to execute a shared workspace.
pub struct ShareRunRequest {
    /// Local share path (`share.spec.json` / `share.lock.json`).
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
    /// Override ato binary path (for materialization via `ato workspace setup`).
    pub ato_path: Option<PathBuf>,
    /// When true, bypass nacelle and run the entry command directly on the host.
    /// Mirrors `--compatibility-fallback host` for local `ato run` invocations.
    pub compat_host: bool,
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

/// Execute a shared workspace through nacelle sandbox.
///
/// 1. Materializes the share workspace via `ato workspace setup`
/// 2. Reads the entry from the materialized spec
/// 3. Spawns nacelle with the entry command
pub fn execute_share(request: ShareRunRequest) -> Result<ShareExecutionResult> {
    // Step 0: Content identity — binds the workspace cache to the actual
    // spec/lock content so `ato run ./share.spec.json` from different projects
    // (or an edited file at the same path) never aliases each other.
    let identity = local_share_identity(&request.input)?;

    // Step 1: Create workspace directory
    let workspace = share_workspace_dir(&identity)?;
    std::fs::create_dir_all(&workspace).map_err(|e| {
        CapsuleError::Runtime(format!(
            "failed to create workspace {}: {e}",
            workspace.display()
        ))
    })?;

    // Step 2: Materialize via ato workspace setup
    let ato_bin = resolve_ato_binary(request.ato_path.as_deref())?;
    materialize_into(&ato_bin, &request.input, &workspace, &identity)?;

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

    let env_pairs: Vec<(String, String)> = request.env_overlay.into_iter().collect();

    // Step 5 (compat-host): bypass nacelle and run directly on the host.
    if request.compat_host {
        match request.mode {
            ShareExecutionMode::Inherited => {
                let exit_code = spawn_direct_inherited(&run_command, &run_cwd, &env_pairs)?;
                return Ok(ShareExecutionResult::Completed { exit_code });
            }
            ShareExecutionMode::Piped { .. } => {
                return Err(CapsuleError::Config(
                    "compat_host mode does not support Piped execution".into(),
                ));
            }
        }
    }

    // Step 5: Resolve nacelle
    let nacelle_bin = resolve_nacelle_binary(request.nacelle_path.as_deref())?;

    // Step 6: Build envelope and spawn nacelle
    match request.mode {
        ShareExecutionMode::Inherited => {
            let exit_code =
                spawn_nacelle_inherited(&nacelle_bin, &run_command, &run_cwd, &env_pairs)?;
            // Workspace is intentionally kept for caching — materialize_into
            // reuses it on the next invocation if state.json shows all sources
            // are ok.
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

/// Content-identity marker stored in a materialized workspace. Reuse of a cached
/// workspace is only allowed when the stored marker equals the identity computed
/// from the current input files.
const SHARE_RUN_IDENTITY_FILE: &str = ".ato/share-run.identity";
/// Materialization contract version included in the identity hash. Bump when the
/// semantics of `ato workspace setup --dev` change (e.g. install-step handling).
const SHARE_RUN_IDENTITY_CONTRACT: &str = "ato-share-run-v1";

/// Compute a content-bound identity for a local share input.
///
/// Reads the actual `share.spec.json` / `share.lock.json` pair (whichever file is
/// given plus its sibling), canonicalizes both, and hashes them with SHA-256
/// together with the materialization contract version.
///
/// The raw input string is deliberately **not** part of the identity: two
/// different projects both run as `ato run ./share.spec.json` from different
/// directories, and the same file is edited between runs. Keying a cache on the
/// string alone would execute the wrong code.
fn local_share_identity(input: &str) -> Result<String> {
    let (spec_path, lock_path) = resolve_share_pair(Path::new(input))?;
    let spec_raw = std::fs::read(&spec_path).map_err(|e| {
        CapsuleError::Runtime(format!("failed to read {}: {e}", spec_path.display()))
    })?;
    let lock_raw = std::fs::read(&lock_path).map_err(|e| {
        CapsuleError::Runtime(format!("failed to read {}: {e}", lock_path.display()))
    })?;
    let spec_value: serde_json::Value = serde_json::from_slice(&spec_raw)
        .map_err(|e| CapsuleError::Config(format!("failed to parse share spec: {e}")))?;
    let spec_canon = serde_json::to_vec(&spec_value)
        .map_err(|e| CapsuleError::Config(format!("failed to canonicalize share spec: {e}")))?;
    let lock_value: serde_json::Value = serde_json::from_slice(&lock_raw)
        .map_err(|e| CapsuleError::Config(format!("failed to parse share lock: {e}")))?;
    let lock_canon = serde_json::to_vec(&lock_value)
        .map_err(|e| CapsuleError::Config(format!("failed to canonicalize share lock: {e}")))?;
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(SHARE_RUN_IDENTITY_CONTRACT.as_bytes());
    hasher.update(b"\0");
    hasher.update(&spec_canon);
    hasher.update(b"\0");
    hasher.update(&lock_canon);
    Ok(hex::encode(hasher.finalize()))
}

/// Resolve the spec/lock pair for a local share input (whichever file is given,
/// the sibling is located in the same directory).
fn resolve_share_pair(input: &Path) -> Result<(PathBuf, PathBuf)> {
    let parent = input.parent().unwrap_or_else(|| Path::new("."));
    match input.file_name().and_then(|n| n.to_str()) {
        Some(SHARE_SPEC_FILE) => Ok((input.to_path_buf(), parent.join(SHARE_LOCK_FILE))),
        Some(SHARE_LOCK_FILE) => Ok((parent.join(SHARE_SPEC_FILE), input.to_path_buf())),
        _ => Err(CapsuleError::Config(format!(
            "unsupported share input {}: expected share.spec.json or share.lock.json",
            input.display()
        ))),
    }
}

/// Compute a stable, content-bound workspace directory for a share identity.
///
/// Uses SHA-256 (not `DefaultHasher`) so the directory name is stable and
/// collision-resistant across processes.
fn share_workspace_dir(identity: &str) -> Result<PathBuf> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"ato-share-run-dir\0");
    hasher.update(identity.as_bytes());
    let hash = hex::encode(hasher.finalize());
    Ok(crate::common::paths::ato_path_or_workspace_tmp("apps/share-runs").join(&hash[..32]))
}

/// Run `ato workspace setup <input> --into <workspace> --dev`.
///
/// `--dev` is required so captured `install_steps` run during materialization —
/// the old hidden `ato decap` alias passed `dev: true` internally.
///
/// A cached workspace is reused only when ALL of the following hold:
///   - the stored content identity matches the current input files, and
///   - every source materialized (`sources` non-empty and all `ok`), and
///   - `verification.result == "ok"`, and
///   - every install step succeeded.
///
/// Any other state is cleared and re-materialized from the current input.
fn materialize_into(ato_bin: &Path, input: &str, workspace: &Path, identity: &str) -> Result<()> {
    let state_path = workspace.join(".ato").join("share").join(SHARE_STATE_FILE);
    let identity_path = workspace.join(SHARE_RUN_IDENTITY_FILE);
    if identity_path.exists() && state_path.exists() {
        let stored_identity = std::fs::read_to_string(&identity_path).unwrap_or_default();
        if stored_identity.trim() == identity
            && let Ok(raw) = std::fs::read_to_string(&state_path)
            && let Ok(state) = serde_json::from_str::<WorkspaceShareState>(&raw)
        {
            let sources_ok =
                !state.sources.is_empty() && state.sources.iter().all(|s| s.status == "ok");
            let verification_ok = state.verification.result == "ok";
            let install_ok = state.install_steps.iter().all(|s| s.status == "ok");
            if sources_ok && verification_ok && install_ok {
                info!(input, "reusing verified cached workspace");
                return Ok(());
            }
        }
        // Stale, broken, mismatched identity, or failed install steps — clear and
        // re-materialize from the current input so we never run stale code.
        warn!(input, "clearing stale workspace for re-materialization");
        let _ = std::fs::remove_dir_all(workspace);
        std::fs::create_dir_all(workspace)?;
    }

    info!(input, dest = %workspace.display(), "running ato workspace setup");
    let output = Command::new(ato_bin)
        .args(["workspace", "setup", input, "--into"])
        .arg(workspace)
        .arg("--dev")
        .output()
        .map_err(|e| {
            CapsuleError::Runtime(format!(
                "failed to spawn ato workspace setup for {input}: {e}"
            ))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        error!(input, %stderr, "ato workspace setup failed");
        return Err(CapsuleError::Execution(format!(
            "ato workspace setup failed for {input}: {stderr}"
        )));
    }
    // Record the content identity so a later run only reuses a matching workspace.
    std::fs::write(workspace.join(SHARE_RUN_IDENTITY_FILE), identity)
        .map_err(|e| CapsuleError::Runtime(format!("failed to write share-run identity: {e}")))?;
    info!(input, "ato workspace setup completed");
    Ok(())
}

/// Read the share spec from a materialized workspace.
fn load_materialized_spec(workspace: &Path) -> Result<ShareSpec> {
    let state_path = workspace.join(".ato").join("share").join(SHARE_STATE_FILE);
    let state_raw = std::fs::read_to_string(&state_path).map_err(|e| {
        CapsuleError::Runtime(format!(
            "no state.json in workspace {}: {e}",
            workspace.display()
        ))
    })?;
    let _state: WorkspaceShareState = serde_json::from_str(&state_raw)
        .map_err(|e| CapsuleError::Config(format!("failed to parse state.json: {e}")))?;

    // Try to find share.spec.json
    let spec_path = workspace.join(".ato").join("share").join("share.spec.json");
    if spec_path.exists() {
        let spec_raw = std::fs::read_to_string(&spec_path)?;
        return serde_json::from_str(&spec_raw)
            .map_err(|e| CapsuleError::Config(format!("failed to parse share.spec.json: {e}")));
    }

    // Fallback: try to load from the decap output
    Err(CapsuleError::NotFound(format!(
        "share.spec.json not found in workspace {}",
        workspace.display()
    )))
}

/// Select an entry from the spec.
fn select_entry(spec: &ShareSpec, selector: Option<&str>) -> Result<ShareEntrySpec> {
    let entries = if !spec.entries.is_empty() {
        spec.entries.clone()
    } else {
        return Err(CapsuleError::Config("share has no entries to run".into()));
    };

    if let Some(sel) = selector {
        if let Some(entry) = entries.iter().find(|e| e.id == sel || e.label == sel) {
            return Ok(entry.clone());
        }
        return Err(CapsuleError::NotFound(format!(
            "entry '{sel}' not found in share"
        )));
    }

    // Auto-select: prefer primary
    if let Some(entry) = entries.iter().find(|e| e.primary) {
        return Ok(entry.clone());
    }
    if entries.len() == 1 {
        return Ok(entries[0].clone());
    }
    Err(CapsuleError::Config(format!(
        "share has {} entries but none is primary — specify --entry",
        entries.len()
    )))
}

/// Resolve the ato binary path.
fn resolve_ato_binary(override_path: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = override_path {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        return Err(CapsuleError::NotFound(format!(
            "ato binary not found at {}",
            path.display()
        )));
    }
    if let Some(path) = std::env::var_os("ATO_DESKTOP_ATO_BIN").map(PathBuf::from)
        && path.is_file()
    {
        return Ok(path);
    }
    // Search PATH
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    for entry in std::env::split_paths(&path_var) {
        let candidate = entry.join(platform_binary_name("ato"));
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(CapsuleError::NotFound(
        "ato binary not found on PATH".into(),
    ))
}

/// Resolve the nacelle binary path.
fn resolve_nacelle_binary(override_path: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = override_path {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        return Err(CapsuleError::NotFound(format!(
            "nacelle binary not found at {}",
            path.display()
        )));
    }
    if let Some(path) = std::env::var_os("NACELLE_PATH").map(PathBuf::from)
        && path.is_file()
    {
        return Ok(path);
    }
    // Try capsule engine discovery
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
                let candidate = entry.join(platform_binary_name("nacelle"));
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
            Err(CapsuleError::NotFound(
                "nacelle binary not found — set NACELLE_PATH or install nacelle on PATH".into(),
            ))
        }
    }
}

fn platform_binary_name(name: &str) -> String {
    let suffix = std::env::consts::EXE_SUFFIX;
    if suffix.is_empty() || name.ends_with(suffix) {
        name.to_string()
    } else {
        format!("{name}{suffix}")
    }
}

/// Run the entry command directly on the host (compat-host mode). Returns exit code.
///
/// Used when `--compatibility-fallback host` is set: bypasses nacelle entirely
/// and runs `sh -lc <command>` directly in the entry's working directory.
fn spawn_direct_inherited(
    run_command: &str,
    cwd: &Path,
    env_pairs: &[(String, String)],
) -> Result<i32> {
    info!(cmd = run_command, cwd = %cwd.display(), "spawning directly (compat-host)");
    let status = Command::new("sh")
        .args(["-lc", run_command])
        .current_dir(cwd)
        .envs(env_pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| {
            CapsuleError::Runtime(format!("failed to spawn command (compat-host): {e}"))
        })?;
    Ok(status.code().unwrap_or(1))
}

/// Spawn nacelle with inherited stdio (CLI mode). Returns exit code.
fn spawn_nacelle_inherited(
    nacelle_bin: &Path,
    run_command: &str,
    cwd: &Path,
    env_pairs: &[(String, String)],
) -> Result<i32> {
    let envelope = build_envelope(run_command, cwd, env_pairs, false, 80, 24);
    let envelope_json = serde_json::to_string(&envelope)
        .map_err(|e| CapsuleError::Runtime(format!("failed to serialize envelope: {e}")))?;

    // Write envelope to workspace-local tmp dir.
    let tmp_dir = crate::common::paths::workspace_tmp_dir(cwd);
    std::fs::create_dir_all(&tmp_dir).ok();
    let envelope_path = tmp_dir.join("share-exec.json");
    std::fs::write(&envelope_path, &envelope_json)?;

    info!(cmd = run_command, cwd = %cwd.display(), "spawning nacelle (inherited)");
    let status = Command::new(nacelle_bin)
        .args([
            "internal",
            "--input",
            &envelope_path.to_string_lossy(),
            "exec",
        ])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| {
            CapsuleError::Runtime(format!(
                "failed to spawn nacelle at {}: {e}",
                nacelle_bin.display()
            ))
        })?;

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
    let envelope_json = serde_json::to_string(&envelope)
        .map_err(|e| CapsuleError::Runtime(format!("failed to serialize envelope: {e}")))?;

    // Write envelope to workspace-local tmp dir.
    let tmp_dir = crate::common::paths::workspace_tmp_dir(cwd);
    std::fs::create_dir_all(&tmp_dir).ok();
    let envelope_path = tmp_dir.join("share-exec.json");
    std::fs::write(&envelope_path, &envelope_json)?;

    info!(cmd = run_command, cwd = %cwd.display(), cols, rows, "spawning nacelle (piped)");
    let mut child = Command::new(nacelle_bin)
        .args([
            "internal",
            "--input",
            &envelope_path.to_string_lossy(),
            "exec",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| {
            CapsuleError::Runtime(format!(
                "failed to spawn nacelle at {}: {e}",
                nacelle_bin.display()
            ))
        })?;

    let mut nacelle_stdin = child
        .stdin
        .take()
        .ok_or_else(|| CapsuleError::Runtime("nacelle stdin unavailable".into()))?;
    let nacelle_stdout = child
        .stdout
        .take()
        .ok_or_else(|| CapsuleError::Runtime("nacelle stdout unavailable".into()))?;

    let (input_tx, input_rx): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = channel();
    let (resize_tx, resize_rx): (ResizeTx, ResizeRx) = channel();
    let (output_tx, output_rx): (Sender<String>, Receiver<String>) = channel();

    let session_id = format!("share-{}", child.id());
    let sid = session_id.clone();
    let envelope_cleanup = envelope_path.clone();

    // Thread: nacelle stdout → output_tx
    // Nacelle emits externally-tagged serde JSON: {"TerminalData":{...}} / {"TerminalExited":{...}}
    std::thread::spawn(move || {
        let reader = BufReader::new(nacelle_stdout);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            // Check nacelle's externally-tagged serde format first, then fall back to
            // the legacy flat format in case the binary is older.
            let variant = value
                .as_object()
                .and_then(|o| o.keys().next())
                .map(|k| k.as_str());
            match variant {
                Some("TerminalData") => {
                    if let Some(b64) = value["TerminalData"]
                        .get("data_b64")
                        .and_then(|d| d.as_str())
                        && output_tx.send(b64.to_string()).is_err()
                    {
                        break;
                    }
                }
                Some("TerminalExited") => {
                    let code = value["TerminalExited"]
                        .get("exit_code")
                        .and_then(|c| c.as_i64());
                    info!(session_id = %sid, exit_code = ?code, "share terminal session exited");
                    break;
                }
                // Legacy flat format: {"event":"terminal_data","data_b64":"..."}
                _ => match value.get("event").and_then(|e| e.as_str()) {
                    Some("terminal_data") => {
                        if let Some(b64) = value.get("data_b64").and_then(|d| d.as_str())
                            && output_tx.send(b64.to_string()).is_err()
                        {
                            break;
                        }
                    }
                    Some("terminal_exited") => {
                        let code = value.get("exit_code").and_then(|c| c.as_i64());
                        info!(session_id = %sid, exit_code = ?code, "share terminal session exited (legacy)");
                        break;
                    }
                    _ => {}
                },
            }
        }
        let _ = std::fs::remove_file(&envelope_cleanup);
    });

    let sid2 = session_id.clone();

    // Thread: input_rx + resize_rx → nacelle stdin
    std::thread::spawn(move || {
        use base64::Engine as _;
        use base64::engine::general_purpose::STANDARD;

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
    // Use a bare command name (`sh`) rather than the absolute `/bin/sh`.
    // Rationale: nacelle's source launcher runs non-interactive workloads
    // through `validate_binary`, which rejects absolute paths for
    // portability. Its own error message recommends bare command names.
    // `sh` is guaranteed to be on PATH on every POSIX system, and the
    // Windows branch already used the bare name `cmd`.
    let shell: &str = if cfg!(windows) { "cmd" } else { "sh" };
    let shell_args: &[&str] = if cfg!(windows) { &["/C"] } else { &["-lc"] };
    let mut cmd: Vec<String> = Vec::with_capacity(shell_args.len() + 2);
    cmd.push(shell.to_string());
    for a in shell_args {
        cmd.push((*a).to_string());
    }
    cmd.push(run_command.to_string());

    let mut envelope = serde_json::json!({
        "spec_version": "1.0",
        "workload": {
            "type": "shell",
            "cmd": cmd,
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
    fn share_workspace_dir_is_stable_and_content_bound() {
        // Same identity → same directory.
        let dir1 = share_workspace_dir("identity-aaa").unwrap();
        let dir2 = share_workspace_dir("identity-aaa").unwrap();
        assert_eq!(dir1, dir2);

        // Different identity (different share content) → different directory.
        let dir3 = share_workspace_dir("identity-bbb").unwrap();
        assert_ne!(dir1, dir3);

        // Directory name is a fixed-length SHA-256 hex digest.
        let name = dir1.file_name().unwrap().to_string_lossy().to_string();
        assert_eq!(
            name.len(),
            32,
            "dir name should be a 32-char hex digest: {name}"
        );
        assert!(name.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// Write a spec/lock pair and return the content identity for `input`.
    fn identity_for_pair(dir: &Path, entry_run: &str) -> String {
        let spec = serde_json::json!({
            "schema_version": "2", "name": "demo", "root": "demo",
            "sources": [], "tool_requirements": [], "env_requirements": [],
            "install_steps": [], "entries": [{
                "id": "demo", "label": "Demo", "cwd": ".", "run": entry_run,
                "kind": "command", "primary": true, "depends_on": [],
                "env": {"required": [], "optional": [], "files": []}, "evidence": []
            }],
            "services": [], "notes": {"team_notes": ""},
            "generated_from": {"root_path": "/tmp", "captured_at": "2026-01-01T00:00:00Z", "host_os": "macos"}
        });
        let lock = serde_json::json!({
            "schema_version": "2", "spec_digest": "sha256:test", "generated_guide_digest": "sha256:test",
            "revision": 1, "created_at": "2026-01-01T00:00:00Z",
            "resolved_sources": [], "resolved_tools": []
        });
        std::fs::write(
            dir.join(SHARE_SPEC_FILE),
            serde_json::to_vec(&spec).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.join(SHARE_LOCK_FILE),
            serde_json::to_vec(&lock).unwrap(),
        )
        .unwrap();
        local_share_identity(&dir.join(SHARE_SPEC_FILE).display().to_string()).unwrap()
    }

    #[test]
    fn local_share_identity_is_content_bound_not_path_bound() {
        let temp = tempfile::tempdir().unwrap();
        let dir_a = temp.path().join("a");
        let dir_b = temp.path().join("b");
        std::fs::create_dir_all(&dir_a).unwrap();
        std::fs::create_dir_all(&dir_b).unwrap();

        let identity_a1 = identity_for_pair(&dir_a, "echo A");
        let identity_b = identity_for_pair(&dir_b, "echo B");
        let identity_a2 = identity_for_pair(&dir_a, "echo A");

        // Two different files both named ./share.spec.json (different content).
        assert_ne!(identity_a1, identity_b, "different content must not alias");
        // Re-writing the same content at the same path is stable.
        assert_eq!(identity_a1, identity_a2);
    }

    #[test]
    fn local_share_identity_changes_when_spec_content_changes() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let before = identity_for_pair(dir, "echo A");
        let after = identity_for_pair(dir, "echo B");
        assert_ne!(before, after, "editing the spec must change the identity");
    }

    #[test]
    fn build_envelope_inherited() {
        let env = vec![("FOO".to_string(), "bar".to_string())];
        let envelope = build_envelope(
            "python main.py",
            Path::new("/workspace"),
            &env,
            false,
            80,
            24,
        );
        assert_eq!(envelope["workload"]["type"], "shell");
        // Must use bare command name so nacelle's validate_binary allows it.
        #[cfg(not(windows))]
        assert_eq!(envelope["workload"]["cmd"][0], "sh");
        #[cfg(windows)]
        assert_eq!(envelope["workload"]["cmd"][0], "cmd");
        assert_eq!(envelope["workload"]["cmd"][2], "python main.py");
        assert_eq!(envelope["interactive"], false);
        assert!(envelope.get("terminal").is_none());
    }

    #[test]
    fn build_envelope_piped() {
        let envelope = build_envelope(
            "python main.py",
            Path::new("/workspace"),
            &[],
            true,
            120,
            40,
        );
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
