//! `ato-local-runtime` — a loopback process boundary around Ato's local
//! execution library.
//!
//! It exists so a consumer can reach `ato-local-execution` across a process
//! boundary instead of linking it. Everything about HOW a Computation executes
//! stays in that library; this binary owns transport, authentication, request
//! parsing, work-root resolution and lifecycle, and nothing else.
//!
//! It registers no VM Snapshot materializer. This host has no hypervisor
//! backend, so it realizes only what it can — source/replay — which is the same
//! boundary the CLI expresses from the other side by ADDING VM Snapshot to the
//! shared core set.

#![forbid(unsafe_code)]

mod protocol;
mod server;

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use protocol::Event;

/// Where Capsule projects live. Supplied by the caller; never guessed, and
/// never the process's current directory.
const WORK_ROOT_ENV: &str = "ATO_LOCAL_RUNTIME_WORK_ROOT";
/// Path to a mode-0600 file holding this launch's machine credential.
///
/// A file rather than an environment variable or an argument, so the secret is
/// never present in any process's environment block or in `ps` output.
const CREDENTIAL_FILE_ENV: &str = "ATO_LOCAL_RUNTIME_CREDENTIAL_FILE";

fn main() {
    // `start_durable` supervises a run by spawning `current_exe __worker …`,
    // so every host binary that starts a durable execution must be able to BE
    // that worker. Without this the runtime re-executes itself as a second
    // server and the run never becomes active — which is exactly what happened
    // the first time this was wired up.
    let arguments: Vec<String> = std::env::args().collect();
    if arguments
        .get(1)
        .is_some_and(|argument| argument == "__worker")
    {
        match worker(&arguments[2..]) {
            Ok(()) => return,
            Err(error) => {
                eprintln!("ato-local-runtime worker: {error:#}");
                std::process::exit(1);
            }
        }
    }

    match run() {
        Ok(()) => {}
        Err(error) => {
            emit(&Event::Failed {
                reason: format!("{error:#}"),
            });
            eprintln!("ato-local-runtime: {error:#}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<()> {
    let work_root = PathBuf::from(
        std::env::var(WORK_ROOT_ENV)
            .with_context(|| format!("{WORK_ROOT_ENV} must point at the work root"))?,
    );
    std::fs::create_dir_all(&work_root)
        .with_context(|| format!("creating {}", work_root.display()))?;

    let credential = read_credential()?;

    let server = server::Server::bind(work_root, credential)?;
    let port = server.port();

    // READY only once the listener is bound AND the work root exists: a
    // consumer that connects on this signal must find a runtime that can
    // actually serve, not one that is still preparing.
    emit(&Event::Ready { port });
    eprintln!("ato-local-runtime: ready on 127.0.0.1:{port}");

    server.serve()
}

/// Read the per-launch machine credential.
///
/// Never logged, and never echoed into an error: an error path that prints the
/// value it failed to handle is how a secret reaches a log file.
fn read_credential() -> Result<String> {
    let path = PathBuf::from(
        std::env::var(CREDENTIAL_FILE_ENV)
            .with_context(|| format!("{CREDENTIAL_FILE_ENV} must point at the credential file"))?,
    );
    let credential = std::fs::read_to_string(&path)
        .with_context(|| format!("reading the credential from {}", path.display()))?
        .trim()
        .to_owned();
    if credential.len() < 32 {
        bail!(
            "the credential in {} is too short to be usable",
            path.display()
        );
    }
    Ok(credential)
}

/// Serve one supervised run, delegating entirely to the execution library.
///
/// The argument order is the library's, not this binary's: it is whatever
/// `start_durable` spawns.
fn worker(arguments: &[String]) -> Result<()> {
    let project = arguments
        .first()
        .context("__worker requires a project path")?;
    let branch = arguments.get(1).context("__worker requires a branch")?;
    let head = arguments.get(2).context("__worker requires a head")?;
    let token = arguments.get(3).context("__worker requires a run token")?;
    let descriptor = arguments.get(4);

    let head = ato_computation::ComputationRef::parse(head)?;
    let descriptor = descriptor
        .map(ato_computation::ContentRef::parse)
        .transpose()?;

    ato_local_execution::worker(
        std::path::Path::new(project),
        branch,
        &head,
        token,
        descriptor.as_ref(),
        // This host has no hypervisor backend, so it realizes source/replay
        // only — the same set it advertises by omission.
        &ato_local_execution::core_materializer_registry,
    )
}

fn emit(event: &Event) {
    let line = serde_json::to_string(event).expect("protocol events serialize");
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let _ = writeln!(handle, "{line}");
    let _ = handle.flush();
}
