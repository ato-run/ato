#![allow(dead_code)]

use anyhow::{Context, Result};
use nacelle::capsule_types::capsule_v1::{CapsuleManifestV1, RuntimeType};
use nacelle::launcher::source::toolchain::RuntimeFetcher;
use nacelle::manager::socket::{create_socket_manager, SocketConfig};
use nacelle::system::sandbox::SandboxPolicy;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::process::ExitStatus;

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

fn looks_like_python_entrypoint(entrypoint: &str, source_dir: &Path) -> bool {
    let ep = entrypoint.trim();
    if ep.ends_with(".py") {
        return true;
    }

    let candidate = source_dir.join(ep);
    candidate.is_file()
        && candidate
            .extension()
            .and_then(|s| s.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("py"))
            .unwrap_or(false)
}

async fn run_and_wait(args: RunDevArgs) -> Result<RunDevOutcome> {
    let manifest_path = args.manifest_path.canonicalize().with_context(|| {
        format!(
            "Failed to resolve manifest path: {}",
            args.manifest_path.display()
        )
    })?;

    let source_dir = manifest_path
        .parent()
        .context("Failed to determine manifest directory")?
        .to_path_buf();

    let manifest = CapsuleManifestV1::load_from_file(&manifest_path)
        .with_context(|| format!("Failed to load manifest: {}", manifest_path.display()))?;

    let entrypoint = manifest.execution.entrypoint.trim();
    if entrypoint.is_empty() {
        anyhow::bail!("No entrypoint configured (execution.entrypoint)");
    }

    let enable_socket_activation = looks_like_python_entrypoint(entrypoint, &source_dir);

    let port: u16 = std::env::var("CAPSULE_PORT")
        .ok()
        .or_else(|| std::env::var("PORT").ok())
        .and_then(|p| p.parse().ok())
        .or_else(|| read_port_from_manifest(&manifest))
        .unwrap_or(8000);

    let socket_manager = if enable_socket_activation {
        match create_socket_manager(SocketConfig {
            port,
            ..SocketConfig::default()
        }) {
            Ok(sm) => {
                human_out!(
                    args.interactive,
                    "🔌 Socket Activation: Bound to port {} (FD {})",
                    port,
                    sm.raw_fd()
                );
                Some(sm)
            }
            Err(e) => {
                human_out!(
                    args.interactive,
                    "⚠️  Socket Activation: Failed to bind port {}: {}",
                    port,
                    e
                );
                human_out!(
                    args.interactive,
                    "    Child process will bind its own socket."
                );
                None
            }
        }
    } else {
        human_out!(
            args.interactive,
            "ℹ️  Socket Activation: Disabled (non-Python entrypoint)"
        );
        None
    };

    let mut cmd = build_command(entrypoint, &source_dir, &manifest).await?;
    cmd.env("PORT", port.to_string());
    cmd.env("CAPSULE_PORT", port.to_string());

    if let Some(ref sm) = socket_manager {
        #[cfg(unix)]
        {
            let socket_fd = sm.raw_fd();
            cmd.env("LISTEN_FDS", "1");
            cmd.env("LISTEN_PID", std::process::id().to_string());

            unsafe {
                cmd.pre_exec(move || {
                    const SD_LISTEN_FDS_START: i32 = 3;

                    if socket_fd != SD_LISTEN_FDS_START
                        && libc::dup2(socket_fd, SD_LISTEN_FDS_START) < 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }

                    let flags = libc::fcntl(SD_LISTEN_FDS_START, libc::F_GETFD);
                    if flags >= 0 {
                        libc::fcntl(
                            SD_LISTEN_FDS_START,
                            libc::F_SETFD,
                            flags & !libc::FD_CLOEXEC,
                        );
                    }

                    Ok(())
                });
            }

            human_out!(
                args.interactive,
                "🔗 Socket Activation: Passing FD {} to child process",
                3
            );
        }
    }

    if args.handle_signals {
        install_signal_handlers();
    }

    let mut child = cmd.spawn().context("Failed to execute entrypoint")?;
    let pid = child.id();

    if args.interactive {
        human_out!(args.interactive, "🚀 Running (PID {}): {}", pid, entrypoint);
    }

    let exit_status = child.wait().ok();

    Ok(RunDevOutcome {
        pid,
        port,
        exit_status,
    })
}

fn read_port_from_manifest(manifest: &CapsuleManifestV1) -> Option<u16> {
    manifest.execution.port
}

async fn build_command(
    entrypoint: &str,
    source_dir: &Path,
    manifest: &CapsuleManifestV1,
) -> Result<Command> {
    let runtime_type = manifest.execution.runtime.clone();

    #[allow(deprecated)]
    let cmd = match runtime_type {
        RuntimeType::Source | RuntimeType::Native => {
            build_source_command(entrypoint, source_dir, manifest).await?
        }
        RuntimeType::Oci | RuntimeType::Docker | RuntimeType::Youki => {
            build_oci_command(entrypoint, source_dir)?
        }
        RuntimeType::Wasm => build_wasm_command(entrypoint, source_dir)?,
    };

    // Apply sandbox policy if configured
    if env_truthy("NACELLE_DISABLE_SANDBOX") {
        human_out!(true, "⚠️  Sandbox disabled via NACELLE_DISABLE_SANDBOX");
    } else {
        let policy = SandboxPolicy::for_capsule(source_dir.to_path_buf());
        if let Err(err) = nacelle::system::sandbox::apply_sandbox(&policy) {
            human_out!(true, "⚠️  Sandbox apply failed: {}", err);
        }
    }

    Ok(cmd)
}

async fn build_source_command(
    entrypoint: &str,
    source_dir: &Path,
    manifest: &CapsuleManifestV1,
) -> Result<Command> {
    let runtime = detect_runtime(entrypoint, source_dir, manifest).await?;

    let mut cmd = Command::new(&runtime.program);
    cmd.args(&runtime.args);
    cmd.current_dir(source_dir);

    Ok(cmd)
}

fn build_oci_command(entrypoint: &str, source_dir: &Path) -> Result<Command> {
    let docker = which::which("docker").context("docker not found in PATH")?;
    let mut cmd = Command::new(docker);

    cmd.args(["run", "--rm"]);
    cmd.arg(entrypoint);

    if source_dir.exists() {
        cmd.arg("-v");
        cmd.arg(format!("{}:/app", source_dir.display()));
    }

    Ok(cmd)
}

fn build_wasm_command(entrypoint: &str, source_dir: &Path) -> Result<Command> {
    let wasm_path = source_dir.join(entrypoint);
    let wasmtime = which::which("wasmtime").context("wasmtime not found in PATH")?;

    let mut cmd = Command::new(wasmtime);
    cmd.args(["run", wasm_path.to_string_lossy().as_ref()]);
    Ok(cmd)
}

async fn detect_runtime(
    entrypoint: &str,
    source_dir: &Path,
    manifest: &CapsuleManifestV1,
) -> Result<ResolvedRuntime> {
    if entrypoint.ends_with(".py")
        || source_dir
            .join(entrypoint)
            .extension()
            .and_then(|s| s.to_str())
            == Some("py")
    {
        return resolve_python_runtime(manifest).await;
    }

    if entrypoint.ends_with(".js") || entrypoint.ends_with(".ts") {
        return resolve_node_runtime(manifest).await;
    }

    if entrypoint.ends_with(".sh") {
        return Ok(ResolvedRuntime {
            program: "bash".to_string(),
            args: vec![entrypoint.to_string()],
        });
    }

    Ok(ResolvedRuntime {
        program: entrypoint.to_string(),
        args: Vec::new(),
    })
}

#[derive(Debug)]
struct ResolvedRuntime {
    program: String,
    args: Vec<String>,
}

async fn resolve_python_runtime(_manifest: &CapsuleManifestV1) -> Result<ResolvedRuntime> {
    let version = "3.11".to_string();

    let fetcher = RuntimeFetcher::new()?;
    let runtime_dir = fetcher.download_python_runtime(&version).await?;

    let python = runtime_dir.join("python/bin/python3");

    Ok(ResolvedRuntime {
        program: python.to_string_lossy().to_string(),
        args: Vec::new(),
    })
}

async fn resolve_node_runtime(_manifest: &CapsuleManifestV1) -> Result<ResolvedRuntime> {
    let version = "20".to_string();

    let fetcher = RuntimeFetcher::new()?;
    let runtime_dir = fetcher.download_node_runtime(&version).await?;

    let node = runtime_dir.join("node/bin/node");

    Ok(ResolvedRuntime {
        program: node.to_string_lossy().to_string(),
        args: Vec::new(),
    })
}

#[cfg(unix)]
fn install_signal_handlers() {
    let _ = ctrlc::set_handler(move || {
        eprintln!("\n🛑 Received interrupt, terminating...");
    });
}

#[cfg(not(unix))]
fn install_signal_handlers() {}
