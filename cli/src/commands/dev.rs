use anyhow::{Context, Result};
use nacelle::capsule_types::capsule_v1::CapsuleManifestV1;
use nacelle::engine::socket::{create_socket_manager, SocketConfig};
use nacelle::runtime::source::toolchain::RuntimeFetcher;
use nacelle::verification::sandbox::SandboxPolicy;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::process::Command;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

pub struct DevArgs {
    pub manifest_path: PathBuf,
}

pub async fn execute(args: DevArgs) -> Result<()> {
    let outcome = run_and_wait(RunDevArgs {
        manifest_path: args.manifest_path,
        interactive: true,
        handle_signals: true,
    })
    .await?;

    match outcome.exit_status {
        Some(status) if !status.success() => {
            std::process::exit(status.code().unwrap_or(1));
        }
        _ => Ok(()),
    }
}

pub struct RunDevArgs {
    pub manifest_path: PathBuf,
    pub interactive: bool,
    pub handle_signals: bool,
}

pub struct RunDevOutcome {
    pub pid: u32,
    pub port: u16,
    pub exit_status: Option<ExitStatus>,
}

pub async fn run_non_interactive(manifest_path: PathBuf) -> Result<RunDevOutcome> {
    run_and_wait(RunDevArgs {
        manifest_path,
        interactive: false,
        handle_signals: true,
    })
    .await
}

/// Streaming mode for process-boundary exec.
///
/// Uses stderr for human output and stays in the foreground. Signals are handled.
pub async fn run_streaming(manifest_path: PathBuf) -> Result<RunDevOutcome> {
    run_and_wait(RunDevArgs {
        manifest_path,
        interactive: false,
        handle_signals: true,
    })
    .await
}

macro_rules! human_out {
    ($interactive:expr, $($arg:tt)*) => {
        if $interactive {
            println!($($arg)*);
        } else {
            eprintln!($($arg)*);
        }
    };
}

fn env_truthy(key: &str) -> bool {
    match std::env::var(key) {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
}

async fn run_and_wait(args: RunDevArgs) -> Result<RunDevOutcome> {
    let manifest_path = args
        .manifest_path
        .canonicalize()
        .with_context(|| format!("Failed to resolve manifest path: {}", args.manifest_path.display()))?;

    let source_dir = manifest_path
        .parent()
        .context("Failed to determine manifest directory")?
        .to_path_buf();

    let manifest = CapsuleManifestV1::load_from_file(&manifest_path)
        .with_context(|| format!("Failed to load manifest: {}", manifest_path.display()))?;

    let entrypoint = resolve_entrypoint(&manifest);

    let port = manifest.execution.port.unwrap_or(8000);

    human_out!(args.interactive, "🚀 nacelle dev");
    human_out!(args.interactive, "📄 Manifest: {}", manifest_path.display());
    human_out!(args.interactive, "📁 Working dir: {}", source_dir.display());
    human_out!(args.interactive, "▶️  Entrypoint: {}", entrypoint);

    let mut cmd = build_command(&manifest, &source_dir, &entrypoint).await?;

    // Socket activation: bind in parent, pass FD 3 to child.
    // If binding fails (e.g. port already in use), fall back to random free port.
    let socket_manager = match create_socket_manager(SocketConfig {
        port,
        host: "0.0.0.0".to_string(),
        enabled: true,
    }) {
        Ok(sm) => Some(sm),
        Err(e) => {
            eprintln!("⚠️  Socket Activation: Failed to bind port {}: {}", port, e);
            eprintln!("    Retrying with an ephemeral port...");
            match create_socket_manager(SocketConfig {
                port: 0,
                host: "0.0.0.0".to_string(),
                enabled: true,
            }) {
                Ok(sm) => Some(sm),
                Err(e2) => {
                    eprintln!("⚠️  Socket Activation disabled: {}", e2);
                    None
                }
            }
        }
    };

    let effective_port = socket_manager.as_ref().map(|sm| sm.port()).unwrap_or(port);

    cmd.env("PORT", effective_port.to_string());
    cmd.env("CAPSULE_PORT", effective_port.to_string());

    #[cfg(unix)]
    if let Some(sm) = socket_manager.as_ref() {
        sm.prepare_command(&mut cmd)
            .context("Failed to prepare socket activation")?;
    }

    // Phase 3: OS-native sandbox (Landlock / Seatbelt) via pre_exec
    #[cfg(unix)]
    {
        cmd.process_group(0);

        // User-configurable disable switch for dev.
        // (We keep sandbox best-effort by default, but allow explicit opt-out.)
        let sandbox_disabled = env_truthy("NACELLE_DISABLE_SANDBOX");

        if sandbox_disabled {
            human_out!(
                args.interactive,
                "⚠️  Sandbox: Disabled (User Config: NACELLE_DISABLE_SANDBOX=1)"
            );
        } else {
            let sandbox_policy = SandboxPolicy::for_capsule(&source_dir);
            let policy_clone = sandbox_policy.clone();
            let policy_root = source_dir.display().to_string();

            unsafe {
                cmd.pre_exec(move || {
                    match nacelle::verification::sandbox::apply_sandbox(&policy_clone) {
                        Ok(result) => {
                            if result.fully_enforced {
                                eprintln!(
                                    "🔒 Sandbox: Enabled (policy rooted at {})",
                                    policy_root
                                );
                            } else if result.partially_enforced {
                                eprintln!(
                                    "🔒 Sandbox: Enabled (partial; policy rooted at {}) — {}",
                                    policy_root,
                                    result.message
                                );
                            } else {
                                // Not enforced means we're effectively running unsandboxed.
                                eprintln!(
                                    "💔 Sandbox: Failed to initialize (Run implicitly unsafe) — {}",
                                    result.message
                                );
                            }

                            Ok(())
                        }
                        Err(e) => {
                            // In dev, prefer continuing rather than hard-failing.
                            eprintln!(
                                "💔 Sandbox: Failed to initialize (Run implicitly unsafe) — {}",
                                e
                            );
                            Ok(())
                        }
                    }
                });
            }
        }
    }

    human_out!(args.interactive, "🌐 Listening port: {}", effective_port);

    let mut child = cmd.spawn().context("Failed to spawn entrypoint")?;
    let pid = child.id();
    human_out!(args.interactive, "✅ Started (pid={})", pid);
    if args.interactive {
        println!("   Press Ctrl-C to stop");
    }

    let mut wait_task = tokio::task::spawn_blocking(move || child.wait());

    if args.handle_signals {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                eprintln!("\n🛑 Stopping...");
                terminate_process_group(pid);
                // Give the process a moment to exit.
                match tokio::time::timeout(std::time::Duration::from_secs(5), &mut wait_task).await {
                    Ok(status) => {
                        let exit_status = match status {
                            Ok(Ok(s)) => Some(s),
                            Ok(Err(e)) => return Err(e).context("Failed while waiting for child"),
                            Err(e) => return Err(anyhow::anyhow!(e)).context("Wait task failed"),
                        };

                        return Ok(RunDevOutcome {
                            pid,
                            port: effective_port,
                            exit_status,
                        });
                    }
                    Err(_) => {
                        return Ok(RunDevOutcome {
                            pid,
                            port: effective_port,
                            exit_status: None,
                        });
                    }
                }
            }
            status = &mut wait_task => {
                let exit_status = match status {
                    Ok(Ok(s)) => s,
                    Ok(Err(e)) => return Err(e).context("Failed while waiting for child"),
                    Err(e) => return Err(anyhow::anyhow!(e)).context("Wait task failed"),
                };

                if exit_status.success() {
                    human_out!(args.interactive, "✅ Exited successfully");
                }

                return Ok(RunDevOutcome {
                    pid,
                    port: effective_port,
                    exit_status: Some(exit_status),
                });
            }
        }
    }

    let exit_status = match (&mut wait_task).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return Err(e).context("Failed while waiting for child"),
        Err(e) => return Err(anyhow::anyhow!(e)).context("Wait task failed"),
    };

    Ok(RunDevOutcome {
        pid,
        port: effective_port,
        exit_status: Some(exit_status),
    })
}

fn resolve_entrypoint(manifest: &CapsuleManifestV1) -> String {
    if let Some(targets) = &manifest.targets {
        if let Some(source) = &targets.source {
            if !source.entrypoint.is_empty() {
                return source.entrypoint.clone();
            }
        }
    }

    manifest.execution.entrypoint.clone()
}

fn normalize_python_version(version: Option<&str>) -> String {
    let default_version = "3.11";
    let Some(raw) = version else {
        return default_version.to_string();
    };

    let raw = raw.trim();
    if raw.is_empty() {
        return default_version.to_string();
    }

    let raw = raw.strip_prefix('^').unwrap_or(raw);
    // Accept full versions like 3.11.10, but also normalize ^3.11 -> 3.11
    let parts: Vec<&str> = raw.split('.').collect();
    if parts.len() >= 2 {
        return format!("{}.{}", parts[0], parts[1]);
    }

    raw.to_string()
}

async fn build_command(
    manifest: &CapsuleManifestV1,
    source_dir: &Path,
    entrypoint: &str,
) -> Result<Command> {
    let language = manifest
        .targets
        .as_ref()
        .and_then(|t| t.source.as_ref())
        .map(|s| s.language.to_ascii_lowercase());

    let entrypoint = entrypoint.trim();

    // Some manifests (e.g. older `capsule init` output) encode entrypoint as a full command
    // string like `sh -c '...'`. In that case, executing `<source_dir>/<entrypoint>` will
    // fail with ENOENT. Detect and construct the process properly.
    let looks_like_command = entrypoint.contains(' ') || entrypoint.contains('\t');

    let entrypoint_path = source_dir.join(entrypoint);

    // Keep this minimal: support Python source capsules first (matches existing sample).
    // If not source/python, try to exec the entrypoint directly.
    let mut cmd = if !matches!(language.as_deref(), Some("python")) && looks_like_command {
        build_command_from_entrypoint_string(source_dir, entrypoint)?
    } else if matches!(language.as_deref(), Some("python")) {
        let python = match which::which("python3").or_else(|_| which::which("python")) {
            Ok(p) => p,
            Err(_) => {
                // JIT provisioning fallback: download a standalone Python runtime.
                // IMPORTANT: internal mode must not write to stdout; RuntimeFetcher handles that.
                let version = normalize_python_version(
                    manifest
                        .targets
                        .as_ref()
                        .and_then(|t| t.source.as_ref())
                        .and_then(|s| s.version.as_deref()),
                );

                eprintln!("⚠️  Host python not found. JIT provisioning Python {}...", version);

                let fetcher = RuntimeFetcher::new()
                    .context("Failed to initialize runtime fetcher for JIT python")?;
                fetcher
                    .ensure_python(&version)
                    .await
                    .context("Failed to download Python runtime")?
            }
        };

        let mut c = Command::new(python);
        c.arg(&entrypoint_path);
        c
    } else {
        Command::new(&entrypoint_path)
    };

    cmd.current_dir(source_dir);

    for (k, v) in &manifest.execution.env {
        cmd.env(k, v);
    }

    Ok(cmd)
}

fn build_command_from_entrypoint_string(source_dir: &Path, entrypoint: &str) -> Result<Command> {
    let ep = entrypoint.trim();

    // Explicit shell form.
    for shell in ["sh", "/bin/sh", "bash", "/bin/bash"] {
        let prefix = format!("{} -c ", shell);
        if let Some(rest) = ep.strip_prefix(&prefix) {
            let mut c = Command::new(shell);
            c.arg("-c").arg(rest.trim());
            c.current_dir(source_dir);
            return Ok(c);
        }
    }

    // Fallback: minimal argv splitting (no quote handling). Prefer absolute/relative paths.
    let mut parts = ep.split_whitespace();
    let program = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("entrypoint is empty"))?;

    let program_path = if program.contains('/') {
        let p = PathBuf::from(program);
        if p.is_absolute() {
            p
        } else {
            source_dir.join(p)
        }
    } else {
        PathBuf::from(program)
    };

    let mut c = Command::new(program_path);
    for arg in parts {
        c.arg(arg);
    }
    c.current_dir(source_dir);
    Ok(c)
}

#[cfg(unix)]
fn terminate_process_group(pid: u32) {
    unsafe {
        // Negative PID targets the process group.
        // We set process_group(0), so child becomes leader of its own group.
        libc::kill(-(pid as i32), libc::SIGTERM);
    }
}

#[cfg(not(unix))]
fn terminate_process_group(_pid: u32) {
    // Best-effort: nothing to do.
}
