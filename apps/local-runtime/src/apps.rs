//! Portable Application Instances, driven through the bundled `ato` CLI.
//!
//! Same discipline as the capsule-execution routes on this server: this module
//! is transport, not execution. The bundle schema, the Instance store, the
//! Contract verification and the worker's own lifecycle all live behind
//! `ato app …` in `ato-portable-application`; what this host adds is only
//! process-boundary concerns — which durable application home the CLI may
//! touch, which binary may run, and which process group the Run belongs to.
//!
//! Why a child `ato` at all rather than linking the library: the durable
//! Instance worker is spawned BY the CLI (`__portable-instance-worker`), which
//! owns claim, activation and the self-stopping save path. Re-implementing any
//! of that here would fork the lifecycle this binary is only supposed to
//! carry across a loopback boundary.
//!
//! Process-group contract: `ato app start --supervised` keeps the worker in
//! THIS process's group. The Desktop shell terminates the runtime by its
//! group, so a supervised worker receives the same stop signal and runs its
//! canonical save-and-release before the group is killed — no orphan Runs,
//! and no second shutdown path that could drift from `ato app stop`.

#![forbid(unsafe_code)]

use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use base64::Engine;

use crate::protocol::AppSourceSnapshotRequest;

/// Path to the `ato` binary the runtime may invoke for portable Apps.
///
/// Supplied by the launching host (Ato Desktop resolves its bundled sidecar);
/// never resolved from PATH here — a PATH hit could be a different build than
/// the one this runtime was assembled with.
pub const ATO_BIN_ENV: &str = "ATO_LOCAL_RUNTIME_ATO_BIN";

/// Durable application home: the `ATO_HOME` under which the Instance store,
/// snapshots and run state live. Supplied by the host so the location is a
/// product decision, not a runtime guess.
pub const APP_HOME_ENV: &str = "ATO_LOCAL_RUNTIME_APP_HOME";

/// Bundles can carry real application payloads; this bound exists only to
/// refuse an obviously malformed request, not to police application size.
/// The product-side cap (`MAX_BUNDLE_BYTES` in the PWA) is the real one.
const MAX_BUNDLE_BYTES: usize = 512 * 1024 * 1024;
const MAX_SOURCE_FILES: usize = 50_000;
const MAX_SOURCE_BYTES: usize = 512 * 1024 * 1024;

/// Stderr is the CLI's diagnostic channel; carry its tail into the error so a
/// caller sees the actual refusal rather than "exit 1".
const STDERR_TAIL: usize = 4096;

/// The configured ability to host portable Applications.
///
/// Absent by default: a runtime launched without both values serves only the
/// capsule-execution routes, and the app routes fail closed.
pub struct AppSupport {
    ato_bin: PathBuf,
    app_home: PathBuf,
}

impl AppSupport {
    /// Read the host-supplied configuration. `None` when neither variable is
    /// set; both-or-neither is enforced so a half-configured runtime fails at
    /// startup instead of at the first request.
    pub fn from_env() -> Result<Option<Self>> {
        let bin = std::env::var_os(ATO_BIN_ENV).map(PathBuf::from);
        let home = std::env::var_os(APP_HOME_ENV).map(PathBuf::from);
        match (bin, home) {
            (None, None) => Ok(None),
            (Some(ato_bin), Some(app_home)) => {
                anyhow::ensure!(
                    ato_bin.is_file(),
                    "{ATO_BIN_ENV} does not point at a file: {}",
                    ato_bin.display()
                );
                std::fs::create_dir_all(&app_home)
                    .with_context(|| format!("creating app home {}", app_home.display()))?;
                Ok(Some(Self { ato_bin, app_home }))
            }
            _ => bail!("{ATO_BIN_ENV} and {APP_HOME_ENV} must be set together"),
        }
    }

    /// One `ato` invocation: clean environment, the Instance home as both
    /// ATO_HOME and HOME (toolchain caches the CLI consults belong to the
    /// durable app home, not the operator's shell), and piped output.
    ///
    /// The child deliberately stays in this process's group — that is what
    /// makes the supervised worker reachable by the host's group teardown.
    fn run(&self, args: &[String]) -> Result<Vec<u8>> {
        let output = Command::new(&self.ato_bin)
            .args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.app_home)
            .env("ATO_HOME", &self.app_home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .context("could not run the bundled ato")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let tail = &stderr[stderr.len().saturating_sub(STDERR_TAIL)..];
            bail!("ato {} failed: {}", args.join(" "), tail.trim());
        }
        Ok(output.stdout)
    }

    /// `ato app import` — bytes already on this host become a durable
    /// Instance in the application home.
    ///
    /// The store, not this layer, validates the bundle and selects the
    /// Derivation; a bundle with several routes and no explicit selection is
    /// the CLI's error to return, verbatim.
    pub fn import(
        &self,
        work_root: &Path,
        bundle: &[u8],
        derivation: Option<&str>,
    ) -> Result<serde_json::Value> {
        if bundle.is_empty() {
            bail!("empty portable bundle");
        }
        if bundle.len() > MAX_BUNDLE_BYTES {
            bail!("portable bundle exceeds {} bytes", MAX_BUNDLE_BYTES);
        }
        let incoming = work_root.join("incoming");
        std::fs::create_dir_all(&incoming)
            .with_context(|| format!("creating {}", incoming.display()))?;
        let staged = incoming.join(format!("{}.capsule", unique_name()));
        let staged_arg = staged.display().to_string();
        let mut file = std::fs::File::create(&staged)
            .with_context(|| format!("creating {}", staged.display()))?;
        let outcome = (|| -> Result<serde_json::Value> {
            file.write_all(bundle)
                .with_context(|| format!("writing {}", staged.display()))?;
            drop(file);
            let mut args = vec!["app".into(), "import".into(), staged_arg.clone()];
            if let Some(derivation) = derivation {
                args.push("--derivation".into());
                args.push(derivation.to_owned());
            }
            let stdout = self.run(&args)?;
            let instance: serde_json::Value = serde_json::from_slice(&stdout)
                .context("ato app import returned malformed output")?;
            Ok(instance)
        })();
        let _ = std::fs::remove_file(&staged);
        outcome
    }

    /// Project a Transfer source tree into a regular portable v4 bundle.
    ///
    /// This transport owns only staging and containment. Preset selection,
    /// Contract/Derivation construction and Instance snapshot sealing are all
    /// delegated to the bundled `ato` command, so they cannot drift into a
    /// second Desktop-specific implementation.
    pub fn form_source(
        &self,
        work_root: &Path,
        request: AppSourceSnapshotRequest,
    ) -> Result<serde_json::Value> {
        anyhow::ensure!(
            !request.title.trim().is_empty() && request.title.len() <= 160,
            "source title is invalid"
        );
        anyhow::ensure!(
            !request.files.is_empty() && request.files.len() <= MAX_SOURCE_FILES,
            "source file count is invalid"
        );
        let incoming = work_root.join("incoming");
        std::fs::create_dir_all(&incoming)
            .with_context(|| format!("creating {}", incoming.display()))?;
        let staged = incoming.join(format!("source-{}", unique_name()));
        let source = staged.join("source");
        std::fs::create_dir_all(&source)
            .with_context(|| format!("creating {}", source.display()))?;
        let browser_state = staged.join("browser-state.json");
        let output = staged.join("application.capsule");

        let outcome = (|| -> Result<serde_json::Value> {
            let mut total = 0usize;
            let mut paths = std::collections::BTreeSet::new();
            for entry in request.files {
                let relative = safe_source_path(&entry.path)?;
                anyhow::ensure!(paths.insert(relative.clone()), "duplicate source path");
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(entry.content_base64)
                    .context("source file is not valid base64")?;
                total = total
                    .checked_add(bytes.len())
                    .context("source size overflow")?;
                anyhow::ensure!(total <= MAX_SOURCE_BYTES, "source exceeds byte limit");
                let target = source.join(&relative);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("creating {}", parent.display()))?;
                }
                std::fs::write(&target, bytes)
                    .with_context(|| format!("writing {}", relative.display()))?;
            }
            std::fs::write(&browser_state, serde_json::to_vec(&request.local_storage)?)
                .with_context(|| format!("writing {}", browser_state.display()))?;
            let stdout = self.run(&[
                "app".into(),
                "__pack-source-snapshot".into(),
                source.display().to_string(),
                "--title".into(),
                request.title,
                "--browser-state".into(),
                browser_state.display().to_string(),
                "--output".into(),
                output.display().to_string(),
            ])?;
            let bytes =
                std::fs::read(&output).with_context(|| format!("reading {}", output.display()))?;
            anyhow::ensure!(
                !bytes.is_empty() && bytes.len() <= MAX_BUNDLE_BYTES,
                "formed portable bundle has an invalid size"
            );
            let mut metadata: serde_json::Value = serde_json::from_slice(&stdout)
                .context("ato source pack returned malformed output")?;
            metadata
                .as_object_mut()
                .context("ato source pack returned a non-object")?
                .insert(
                    "bundle_base64".to_owned(),
                    serde_json::Value::String(
                        base64::engine::general_purpose::STANDARD.encode(bytes),
                    ),
                );
            Ok(metadata)
        })();
        let _ = std::fs::remove_dir_all(&staged);
        outcome
    }

    /// `ato app start` — one supervised Run for the Instance.
    ///
    /// `--json --no-open --supervised`: machine-readable result, no browser,
    /// worker inside this process's group (see the module comment for why the
    /// group is the correctness anchor).
    pub fn start(
        &self,
        instance: &str,
        bindings: &std::collections::BTreeMap<String, String>,
    ) -> Result<serde_json::Value> {
        let mut args = vec![
            "app".into(),
            "start".into(),
            instance.to_owned(),
            "--no-open".into(),
            "--json".into(),
            "--supervised".into(),
        ];
        for (name, value) in bindings {
            args.push("--bind".into());
            args.push(format!("{name}={value}"));
        }
        let stdout = self.run(&args)?;
        serde_json::from_slice(&stdout).context("ato app start returned malformed output")
    }

    /// `ato app stop` — the canonical graceful stop: the worker persists
    /// state and releases the Run before this returns.
    pub fn stop(&self, instance: &str) -> Result<()> {
        self.run(&["app".into(), "stop".into(), instance.to_owned()])?;
        Ok(())
    }

    /// `ato app inspect` — the store's own view of the Instance and its
    /// active Run, passed through uninterpreted.
    pub fn status(&self, instance: &str) -> Result<serde_json::Value> {
        let stdout = self.run(&["app".into(), "inspect".into(), instance.to_owned()])?;
        serde_json::from_slice(&stdout).context("ato app inspect returned malformed output")
    }
}

fn safe_source_path(value: &str) -> Result<PathBuf> {
    anyhow::ensure!(
        !value.is_empty() && value.len() <= 1024 && !value.contains('\0'),
        "source path is invalid"
    );
    let path = Path::new(value);
    let mut safe = PathBuf::new();
    let mut depth = 0usize;
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                safe.push(part);
                depth += 1;
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("source path escapes the staging root")
            }
        }
    }
    anyhow::ensure!(
        !safe.as_os_str().is_empty() && depth <= 64,
        "source path is invalid"
    );
    Ok(safe)
}

/// A filesystem-safe throwaway name for the staged bundle: timestamp plus a
/// per-process counter, so concurrent imports never share a path.
fn unique_name() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or_default();
    format!("{nanos}-{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staged_names_do_not_collide() {
        assert_ne!(unique_name(), unique_name());
    }

    #[test]
    fn source_paths_are_confined() {
        assert_eq!(
            safe_source_path("assets/app.js").unwrap(),
            PathBuf::from("assets/app.js")
        );
        assert!(safe_source_path("../secret").is_err());
        assert!(safe_source_path("/etc/passwd").is_err());
        assert!(safe_source_path("").is_err());
    }
}
