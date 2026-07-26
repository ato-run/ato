//! Driving the Caddy CLI: the [`CaddyControl`] half of ingress activation.
//!
//! Two properties matter more than anything else here.
//!
//! **No shell, ever.** Every invocation is an explicit argv array. A path that
//! contains a space, a quote or a `;` is one argument because it is passed as
//! one argument — not because it was quoted correctly. There is no string for a
//! quoting mistake to live in.
//!
//! **Every call is bounded.** `caddy validate` on a pathological config and
//! `caddy reload` against a wedged admin endpoint can both hang, and an
//! activation that hangs holds the exclusive lock forever — so the next run
//! cannot even recover the transaction. Each operation has its own deadline,
//! and a timeout kills the child AND reaps it, because a zombie holding the
//! pipe would make the next read block in turn.
//!
//! # What `validate` is given
//!
//! The COMPLETE configuration that references the candidate, never the fragment
//! alone. Errors that only exist in composition — two site blocks claiming one
//! hostname, an import that resolves to nothing — are exactly the ones a
//! per-fragment check cannot see, and exactly the ones that break a reload.
//!
//! The live configuration is therefore expected to be a single import of the
//! active generation:
//!
//! ```text
//! import <root>/current/*.caddy
//! ```
//!
//! and validation renders the same shape against the candidate:
//!
//! ```text
//! import <root>/generations/<candidate>/*.caddy
//! ```

#![allow(dead_code)]

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::Result;

use super::ingress_activation::CaddyControl;

/// Which operation an error came from. Kept typed so a caller never has to
/// parse it back out of a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaddyOperation {
    Validate,
    Reload,
}

impl std::fmt::Display for CaddyOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaddyOperation::Validate => write!(f, "caddy validate"),
            CaddyOperation::Reload => write!(f, "caddy reload"),
        }
    }
}

/// Why a Caddy invocation did not succeed.
///
/// The four cases are kept apart because they call for different actions: a
/// spawn failure is an installation problem, a timeout says nothing about the
/// configuration, a signal means something killed Caddy, and only a non-zero
/// exit is Caddy's own verdict on the config.
#[derive(Debug)]
pub(crate) enum CaddyCommandError {
    Spawn {
        operation: CaddyOperation,
        source: std::io::Error,
    },
    Timeout {
        operation: CaddyOperation,
        after: Duration,
    },
    Signalled {
        operation: CaddyOperation,
        signal: i32,
        stderr: String,
    },
    Exited {
        operation: CaddyOperation,
        code: Option<i32>,
        stderr: String,
    },
}

impl std::fmt::Display for CaddyCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaddyCommandError::Spawn { operation, source } => {
                write!(f, "{operation} could not be started: {source}")
            }
            CaddyCommandError::Timeout { operation, after } => write!(
                f,
                "{operation} did not finish within {after:?} and was killed"
            ),
            CaddyCommandError::Signalled {
                operation,
                signal,
                stderr,
            } => write!(f, "{operation} was terminated by signal {signal}: {stderr}"),
            CaddyCommandError::Exited {
                operation,
                code,
                stderr,
            } => match code {
                Some(code) => write!(f, "{operation} exited {code}: {stderr}"),
                None => write!(f, "{operation} exited with no status: {stderr}"),
            },
        }
    }
}

impl std::error::Error for CaddyCommandError {}

/// How much diagnostic output travels in an error. Caddy's config errors are a
/// handful of lines; anything past this is a runaway that would land in a log
/// nobody can read.
const STDERR_BUDGET: usize = 8 * 1024;

/// Environment the child is allowed to inherit.
///
/// `env_clear` plus an allowlist rather than the ambient environment: Caddy
/// genuinely needs a home for its data directory, and a PATH when its own
/// binary shells out, but nothing else about the operator's session should be
/// able to change what a validation decides.
const INHERITED_ENV: &[&str] = &["PATH", "HOME", "XDG_DATA_HOME", "XDG_CONFIG_HOME"];

/// Trim to the budget and drop anything shaped like a credential.
///
/// The generated fragments carry only hostnames and loopback ports, so this is
/// a belt rather than the primary defence — but the error text can quote a
/// LIVE config that an operator hand-added something to, and that path leads
/// straight into a log.
fn sanitize_stderr(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let mut kept = String::new();
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        let sensitive = ["secret", "token", "password", "api_key", "authorization"]
            .iter()
            .any(|needle| lower.contains(needle));
        if sensitive {
            kept.push_str("[redacted line]\n");
        } else {
            kept.push_str(line);
            kept.push('\n');
        }
        if kept.len() >= STDERR_BUDGET {
            kept.truncate(STDERR_BUDGET);
            kept.push_str("\n[truncated]");
            break;
        }
    }
    kept.trim_end().to_string()
}

/// Render each argument on its own line so a logged command can never be
/// mistaken for something that was run through a shell.
fn describe(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| format!("{arg:?}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The production [`CaddyControl`].
pub(crate) struct ProcessCaddyControl {
    caddy_bin: PathBuf,
    /// The configuration Caddy is running — an import of `current/*.caddy`.
    config_path: PathBuf,
    /// The store root, so a candidate-referencing config can be rendered.
    store_root: PathBuf,
    validate_timeout: Duration,
    reload_timeout: Duration,
}

impl ProcessCaddyControl {
    /// Both paths must be absolute: this process's working directory is not
    /// part of the contract, and a relative path would make the same
    /// configuration mean different things to different callers.
    pub(crate) fn new(
        caddy_bin: impl Into<PathBuf>,
        config_path: impl Into<PathBuf>,
        store_root: impl Into<PathBuf>,
        validate_timeout: Duration,
        reload_timeout: Duration,
    ) -> Result<Self> {
        let control = Self {
            caddy_bin: caddy_bin.into(),
            config_path: config_path.into(),
            store_root: store_root.into(),
            validate_timeout,
            reload_timeout,
        };
        for (what, path) in [
            ("caddy binary", &control.caddy_bin),
            ("caddy config", &control.config_path),
            ("store root", &control.store_root),
        ] {
            if !path.is_absolute() {
                anyhow::bail!("{what} path must be absolute (got {})", path.display());
            }
        }
        Ok(control)
    }

    fn run(&self, operation: CaddyOperation, argv: &[String], timeout: Duration) -> Result<()> {
        let mut command = Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.env_clear();
        for key in INHERITED_ENV {
            if let Ok(value) = std::env::var(key) {
                command.env(key, value);
            }
        }

        let mut child = command
            .spawn()
            .map_err(|source| CaddyCommandError::Spawn { operation, source })?;

        // Drain stderr on its own thread, STARTING NOW. Reading it only after
        // the child exits deadlocks the moment the child writes more than the
        // pipe buffer holds: it blocks on the write, we block on the wait, and
        // the only thing that breaks the tie is the deadline — turning a noisy
        // config error into a timeout that says nothing about the config.
        let drain = child.stderr.take().map(|mut stream| {
            std::thread::spawn(move || {
                use std::io::Read;
                // Read to EOF but RETAIN only the budget. Stopping the read at
                // the cap would close the pipe under a still-writing child,
                // which SIGPIPEs it — and a noisy config error would then be
                // reported as "terminated by signal 13" instead of as the exit
                // code Caddy actually chose.
                let cap = STDERR_BUDGET * 2;
                let mut kept = Vec::new();
                let mut chunk = [0u8; 8192];
                loop {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(read) => {
                            if kept.len() < cap {
                                let room = cap - kept.len();
                                kept.extend_from_slice(&chunk[..read.min(room)]);
                            }
                        }
                    }
                }
                kept
            })
        });

        let deadline = Instant::now() + timeout;
        let status = loop {
            match child.try_wait()? {
                Some(status) => break status,
                None if Instant::now() >= deadline => {
                    // Kill AND reap, so no zombie is left behind.
                    let _ = child.kill();
                    let _ = child.wait();
                    // The drain thread is deliberately NOT joined. Killing the
                    // child does not kill ITS children, and a grandchild that
                    // inherited the stderr pipe keeps the write end open — so a
                    // join here would block until that grandchild exits, which
                    // is exactly the hang the deadline exists to bound. The
                    // thread ends on its own when the pipe finally closes, and
                    // a timeout reports no stderr anyway.
                    drop(drain);
                    return Err(CaddyCommandError::Timeout {
                        operation,
                        after: timeout,
                    }
                    .into());
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        };

        let stderr = drain
            .and_then(|handle| handle.join().ok())
            .map(|bytes| sanitize_stderr(&bytes))
            .unwrap_or_default();
        if status.success() {
            return Ok(());
        }
        Err(classify(operation, status, stderr).into())
    }

    /// The complete configuration that references `digest`, written beside the
    /// store so it shares a filesystem with what it imports.
    fn candidate_config(&self, digest: &str) -> Result<TempConfig> {
        let path = self.store_root.join(format!(
            ".tmp-validate-{}-{}.caddy",
            std::process::id(),
            digest
        ));
        let body = format!(
            "# generated for `caddy validate` only — never served.\nimport {}\n",
            self.store_root
                .join("generations")
                .join(digest)
                .join("*.caddy")
                .display()
        );
        std::fs::write(&path, body)?;
        Ok(TempConfig { path })
    }
}

/// Removes itself, so a failed validation does not leave a config lying beside
/// the ones Caddy imports by glob.
struct TempConfig {
    path: PathBuf,
}

impl Drop for TempConfig {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(unix)]
fn classify(
    operation: CaddyOperation,
    status: std::process::ExitStatus,
    stderr: String,
) -> CaddyCommandError {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(code), _) => CaddyCommandError::Exited {
            operation,
            code: Some(code),
            stderr,
        },
        (None, Some(signal)) => CaddyCommandError::Signalled {
            operation,
            signal,
            stderr,
        },
        (None, None) => CaddyCommandError::Exited {
            operation,
            code: None,
            stderr,
        },
    }
}

#[cfg(not(unix))]
fn classify(
    operation: CaddyOperation,
    status: std::process::ExitStatus,
    stderr: String,
) -> CaddyCommandError {
    CaddyCommandError::Exited {
        operation,
        code: status.code(),
        stderr,
    }
}

impl CaddyControl for ProcessCaddyControl {
    fn validate(&mut self, digest: &str) -> Result<()> {
        let config = self.candidate_config(digest)?;
        let argv = vec![
            self.caddy_bin.display().to_string(),
            "validate".to_string(),
            "--adapter".to_string(),
            "caddyfile".to_string(),
            "--config".to_string(),
            config.path.display().to_string(),
        ];
        eprintln!("[runner] {}", describe(&argv));
        self.run(CaddyOperation::Validate, &argv, self.validate_timeout)
    }

    fn reload(&mut self) -> Result<()> {
        let argv = vec![
            self.caddy_bin.display().to_string(),
            "reload".to_string(),
            "--adapter".to_string(),
            "caddyfile".to_string(),
            "--config".to_string(),
            self.config_path.display().to_string(),
        ];
        eprintln!("[runner] {}", describe(&argv));
        self.run(CaddyOperation::Reload, &argv, self.reload_timeout)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// A stand-in for the caddy binary. It is a script only so the test can
    /// choose its behaviour; the production path still passes an explicit argv.
    fn fake_caddy(dir: &std::path::Path, body: &str) -> PathBuf {
        let path = dir.join("fake-caddy");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    fn control(dir: &std::path::Path, body: &str) -> ProcessCaddyControl {
        let root = dir.join("store");
        std::fs::create_dir_all(root.join("generations").join("gen-a")).expect("mkdir");
        std::fs::write(root.join("live.caddy"), "# live\n").expect("live");
        ProcessCaddyControl::new(
            fake_caddy(dir, body),
            root.join("live.caddy"),
            &root,
            Duration::from_secs(5),
            Duration::from_secs(5),
        )
        .expect("control")
    }

    #[test]
    fn validate_and_reload_succeed_on_exit_zero() {
        let dir = tempfile::tempdir().unwrap();
        let mut caddy = control(dir.path(), "exit 0");
        caddy.validate("gen-a").expect("validate");
        caddy.reload().expect("reload");
    }

    #[test]
    fn a_non_zero_exit_is_caddys_own_verdict_and_carries_its_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let mut caddy = control(dir.path(), "echo 'duplicate site block' >&2; exit 3");

        let error = caddy.validate("gen-a").expect_err("must fail");
        let error = error.downcast::<CaddyCommandError>().expect("typed");
        match error {
            CaddyCommandError::Exited {
                operation,
                code,
                stderr,
            } => {
                assert_eq!(operation, CaddyOperation::Validate);
                assert_eq!(code, Some(3));
                assert!(stderr.contains("duplicate site block"), "{stderr}");
            }
            other => panic!("expected Exited, got {other:?}"),
        }
    }

    /// A hang must not hold the activation lock forever — the next run could
    /// not even recover the transaction.
    #[test]
    fn a_hanging_invocation_times_out_and_the_child_is_reaped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("store");
        std::fs::create_dir_all(root.join("generations").join("gen-a")).unwrap();
        std::fs::write(root.join("live.caddy"), "# live\n").unwrap();
        let mut caddy = ProcessCaddyControl::new(
            fake_caddy(dir.path(), "sleep 30"),
            root.join("live.caddy"),
            &root,
            Duration::from_millis(150),
            Duration::from_millis(150),
        )
        .unwrap();

        let started = Instant::now();
        let error = caddy.reload().expect_err("must time out");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the deadline must bound the call"
        );
        match error.downcast::<CaddyCommandError>().expect("typed") {
            CaddyCommandError::Timeout { operation, .. } => {
                assert_eq!(operation, CaddyOperation::Reload);
            }
            other => panic!("expected Timeout, got {other:?}"),
        }
    }

    #[test]
    fn a_signalled_child_is_distinguished_from_a_non_zero_exit() {
        let dir = tempfile::tempdir().unwrap();
        // Kill ourselves with SIGKILL: no exit code, only a signal.
        let mut caddy = control(dir.path(), "kill -9 $$");
        let error = caddy.reload().expect_err("must fail");
        match error.downcast::<CaddyCommandError>().expect("typed") {
            CaddyCommandError::Signalled { signal, .. } => assert_eq!(signal, 9),
            other => panic!("expected Signalled, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_binary_is_a_spawn_failure_not_a_verdict() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("store");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("live.caddy"), "# live\n").unwrap();
        let mut caddy = ProcessCaddyControl::new(
            dir.path().join("does-not-exist"),
            root.join("live.caddy"),
            &root,
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .unwrap();
        match caddy.reload().unwrap_err().downcast::<CaddyCommandError>() {
            Ok(CaddyCommandError::Spawn { .. }) => {}
            other => panic!("expected Spawn, got {other:?}"),
        }
    }

    /// A path full of shell metacharacters is ONE argument, because it is
    /// passed as one argument.
    #[test]
    fn a_path_with_shell_metacharacters_is_a_single_argument() {
        let dir = tempfile::tempdir().unwrap();
        let nasty = dir.path().join("a b;rm -rf /$(whoami)'\"");
        std::fs::create_dir_all(nasty.join("generations").join("gen-a")).unwrap();
        std::fs::write(nasty.join("live.caddy"), "# live\n").unwrap();
        // Echo the argument count and the last argument, so the assertion is
        // about what the child actually received.
        let mut caddy = ProcessCaddyControl::new(
            fake_caddy(dir.path(), "echo \"$#:$5\" >&2; exit 7"),
            nasty.join("live.caddy"),
            &nasty,
            Duration::from_secs(5),
            Duration::from_secs(5),
        )
        .unwrap();

        let error = caddy.reload().expect_err("exits 7");
        match error.downcast::<CaddyCommandError>().unwrap() {
            CaddyCommandError::Exited { stderr, .. } => {
                assert!(stderr.starts_with("5:"), "argv count: {stderr}");
                assert!(
                    stderr.contains("a b;rm -rf /$(whoami)"),
                    "the path arrived intact: {stderr}"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    /// The child sees an allowlist, not the operator's session.
    #[test]
    fn the_child_environment_is_minimized() {
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: single-threaded test process section; the variable exists only
        // to prove it does NOT reach the child.
        unsafe { std::env::set_var("ATO_TEST_LEAKY_VALUE", "must-not-appear") };
        let mut caddy = control(dir.path(), "env >&2; exit 5");
        let error = caddy.reload().expect_err("exits 5");
        match error.downcast::<CaddyCommandError>().unwrap() {
            CaddyCommandError::Exited { stderr, .. } => {
                assert!(
                    !stderr.contains("ATO_TEST_LEAKY_VALUE"),
                    "the ambient environment leaked: {stderr}"
                );
            }
            other => panic!("{other:?}"),
        }
        unsafe { std::env::remove_var("ATO_TEST_LEAKY_VALUE") };
    }

    #[test]
    fn stderr_is_capped_and_credential_shaped_lines_are_redacted() {
        let dir = tempfile::tempdir().unwrap();
        let mut caddy = control(
            dir.path(),
            "echo 'api_key = hunter2' >&2; for i in $(seq 1 4000); do echo 'noise-line-padding' >&2; done; exit 1",
        );
        let error = caddy.validate("gen-a").expect_err("exits 1");
        match error.downcast::<CaddyCommandError>().unwrap() {
            CaddyCommandError::Exited { stderr, .. } => {
                assert!(!stderr.contains("hunter2"), "a credential survived");
                assert!(stderr.contains("[redacted line]"));
                assert!(
                    stderr.len() <= STDERR_BUDGET + 32,
                    "stderr was not capped: {} bytes",
                    stderr.len()
                );
            }
            other => panic!("{other:?}"),
        }
    }

    /// The temporary validation config never outlives the call — it sits beside
    /// files Caddy imports by glob.
    #[test]
    fn the_validation_config_is_removed_afterwards() {
        let dir = tempfile::tempdir().unwrap();
        let mut caddy = control(dir.path(), "exit 0");
        caddy.validate("gen-a").expect("validate");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path().join("store"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".tmp-validate-")
            })
            .collect();
        assert!(leftovers.is_empty(), "a validation config was left behind");
    }

    #[test]
    fn relative_paths_are_refused() {
        assert!(
            ProcessCaddyControl::new(
                "caddy",
                "/etc/caddy/Caddyfile",
                "/var/lib/ato",
                Duration::from_secs(1),
                Duration::from_secs(1),
            )
            .is_err(),
            "a relative binary path makes the call depend on the working directory"
        );
    }
}
