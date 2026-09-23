//! The browser verifier's own sandbox.
//!
//! The verifier is verification infrastructure, not a Derivation: it is never
//! lowered as a candidate workload, and it does not share the build sandbox's
//! policy. What it shares are the primitives — bubblewrap, its detection, the
//! system paths a process needs — and one rule: refuse rather than run
//! unconfined.
//!
//! ```text
//! host                                   verifier sandbox (bwrap)
//!   helper source + node_modules  --ro-->  /verifier
//!   Node install                  --ro-->  /runtime/node
//!   Chrome directory              --ro-->  /runtime/chrome
//!   per-verification scratch      --rw-->  /scratch        (HOME, TMPDIR, profile)
//!   /usr /lib /bin … , a few /etc --ro-->  same path
//!   nothing else: no HOME, no repository, no credentials, no token files
//! ```
//!
//! The network is shared: the helper must reach its judge and agent models,
//! and the browser's traffic is confined to the candidate's origin by the
//! helper's guard proxy (ADR-020), which this does not change.
//!
//! Chrome does not run beside the helper. `/verifier/bin/chrome-contained`
//! starts it with an empty environment inside a nested PID namespace, so a
//! compromised browser can neither read the helper's environment nor see the
//! helper's process at all. The helper's model keys are not in any process
//! environment to begin with: they arrive on file descriptor 3.

use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use ato_formation::browser::VerifierContainment;

use crate::sandbox::containment_available;

pub const GUEST_VERIFIER_ROOT: &str = "/verifier";
pub const GUEST_NODE_ROOT: &str = "/runtime/node";
pub const GUEST_CHROME_ROOT: &str = "/runtime/chrome";
pub const GUEST_SCRATCH: &str = "/scratch";
/// The launcher that starts Chrome with nothing of the helper's.
pub const GUEST_CHROME_LAUNCHER: &str = "/verifier/bin/chrome-contained";
/// Where the helper finds its model keys: an inherited pipe, read once.
pub const SECRETS_FD: i32 = 3;

/// System paths the helper and Chrome read and execute.
const SYSTEM_READ_ONLY: &[&str] = &["/usr", "/bin", "/sbin", "/lib", "/lib64"];
/// Configuration a process needs to resolve names, trust TLS and render text.
/// Bound one by one: never `/etc` wholesale (no user database, no service
/// configuration).
const SYSTEM_CONFIG_FILES: &[&str] = &[
    "/etc/resolv.conf",
    "/etc/hosts",
    "/etc/nsswitch.conf",
    "/etc/ssl",
    "/etc/ca-certificates",
    "/etc/pki",
    "/etc/fonts",
];

/// The isolation this sandbox provides, as recorded in a receipt.
pub fn contained_evidence() -> VerifierContainment {
    VerifierContainment {
        containment: "bwrap".to_owned(),
        filesystem: "allowlisted".to_owned(),
        network: "shared-host-net+exact-origin-browser-guard".to_owned(),
        browser_process: "separate-pid-namespace+empty-environment".to_owned(),
        secrets: "fd".to_owned(),
    }
}

/// An explicitly uncontained development run, as recorded in a receipt.
pub fn uncontained_evidence() -> VerifierContainment {
    VerifierContainment {
        containment: "none".to_owned(),
        filesystem: "host".to_owned(),
        network: "host+exact-origin-browser-guard".to_owned(),
        browser_process: "helper-child+empty-environment".to_owned(),
        secrets: "fd".to_owned(),
    }
}

/// What the verifier sandbox is built from, resolved on the host.
#[derive(Debug, Clone)]
pub struct BrowserVerifierSandboxSpec {
    helper_root: PathBuf,
    node_root: PathBuf,
    /// The Node binary, relative to `node_root`.
    node_relative: PathBuf,
    chrome_root: PathBuf,
    /// The Chrome binary, relative to `chrome_root`.
    chrome_relative: PathBuf,
    /// Arguments after the Node binary, relative to `/verifier`.
    entry: Vec<String>,
}

impl BrowserVerifierSandboxSpec {
    /// The shipped helper (`apps/formation-browser-verifier`).
    pub fn node_helper(helper_root: &Path, node: &str, chrome: &Path) -> Result<Self> {
        Self::resolve(
            helper_root,
            node,
            chrome,
            vec![
                "--import".to_owned(),
                "tsx".to_owned(),
                "src/main.ts".to_owned(),
            ],
        )
    }

    /// Any Node program under `helper_root`, run the same way. `entry` is
    /// what follows the Node binary, with paths relative to `/verifier`.
    pub fn resolve(
        helper_root: &Path,
        node: &str,
        chrome: &Path,
        entry: Vec<String>,
    ) -> Result<Self> {
        let helper_root = helper_root.canonicalize().with_context(|| {
            format!(
                "cannot read the browser verifier at {}",
                helper_root.display()
            )
        })?;
        let node = resolve_program(node)?;
        let chrome = chrome
            .canonicalize()
            .with_context(|| format!("cannot read the browser at {}", chrome.display()))?;
        ensure!(node.is_file(), "the Node binary is not a file");
        ensure!(chrome.is_file(), "the browser binary is not a file");
        ensure!(
            helper_root.join("bin/chrome-contained").is_file(),
            "the browser verifier has no bin/chrome-contained launcher"
        );

        // A Node install is bound whole (`<root>/bin/node`): its standard
        // library lives beside the binary. A lone binary binds its directory.
        let node_dir = node.parent().context("the Node binary has no directory")?;
        let (node_root, node_relative) = if node_dir.file_name().is_some_and(|name| name == "bin") {
            let root = node_dir.parent().context("the Node install has no root")?;
            (
                root.to_path_buf(),
                Path::new("bin").join(node.file_name().unwrap()),
            )
        } else {
            (
                node_dir.to_path_buf(),
                PathBuf::from(node.file_name().unwrap()),
            )
        };
        let chrome_root = chrome
            .parent()
            .context("the browser has no directory")?
            .to_path_buf();
        let chrome_relative = PathBuf::from(chrome.file_name().unwrap());

        for (what, path) in [
            ("browser verifier", &helper_root),
            ("Node install", &node_root),
            ("browser directory", &chrome_root),
        ] {
            refuse_broad_bind(what, path)?;
        }
        Ok(Self {
            helper_root,
            node_root,
            node_relative,
            chrome_root,
            chrome_relative,
            entry,
        })
    }

    fn guest_node(&self) -> String {
        format!("{GUEST_NODE_ROOT}/{}", self.node_relative.display())
    }

    fn guest_chrome(&self) -> String {
        format!("{GUEST_CHROME_ROOT}/{}", self.chrome_relative.display())
    }

    /// The bwrap argv that runs the helper with `scratch` as `/scratch`.
    /// `settings` are non-secret variables passed through (model names).
    pub fn argv(&self, scratch: &Path, settings: &[(String, String)]) -> Result<Vec<String>> {
        self.argv_running(scratch, settings, &self.entry)
    }

    fn argv_running(
        &self,
        scratch: &Path,
        settings: &[(String, String)],
        entry: &[String],
    ) -> Result<Vec<String>> {
        let mut argv: Vec<String> = [
            "bwrap",
            "--unshare-all",
            // The helper talks to its models; the browser's own traffic is
            // held to the candidate's origin by the guard proxy.
            "--share-net",
            "--die-with-parent",
            "--new-session",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--tmpfs",
            "/dev/shm",
            "--tmpfs",
            "/tmp",
        ]
        .map(str::to_owned)
        .to_vec();
        for path in SYSTEM_READ_ONLY.iter().chain(SYSTEM_CONFIG_FILES) {
            argv.extend([
                "--ro-bind-try".to_owned(),
                (*path).to_owned(),
                (*path).to_owned(),
            ]);
        }
        for (host, guest) in [
            (&self.helper_root, GUEST_VERIFIER_ROOT),
            (&self.node_root, GUEST_NODE_ROOT),
            (&self.chrome_root, GUEST_CHROME_ROOT),
        ] {
            argv.extend(["--ro-bind".to_owned(), utf8(host)?, guest.to_owned()]);
        }
        argv.extend([
            "--bind".to_owned(),
            utf8(scratch)?,
            GUEST_SCRATCH.to_owned(),
        ]);

        argv.push("--clearenv".to_owned());
        let mut env: Vec<(String, String)> = vec![
            ("HOME".to_owned(), format!("{GUEST_SCRATCH}/home")),
            ("TMPDIR".to_owned(), format!("{GUEST_SCRATCH}/tmp")),
            (
                "PATH".to_owned(),
                format!("{GUEST_NODE_ROOT}/bin:/usr/bin:/bin"),
            ),
            ("LANG".to_owned(), "C.UTF-8".to_owned()),
            (
                "ATO_BROWSER_CHROME_PATH".to_owned(),
                GUEST_CHROME_LAUNCHER.to_owned(),
            ),
            ("ATO_BROWSER_CHROME_REAL".to_owned(), self.guest_chrome()),
            ("ATO_VERIFIER_SECRETS_FD".to_owned(), SECRETS_FD.to_string()),
        ];
        env.extend(settings.iter().cloned());
        for (name, value) in env {
            argv.extend(["--setenv".to_owned(), name, value]);
        }
        argv.extend([
            "--chdir".to_owned(),
            GUEST_VERIFIER_ROOT.to_owned(),
            "--".to_owned(),
        ]);
        argv.push(self.guest_node());
        argv.extend(entry.iter().cloned());
        Ok(argv)
    }

    /// Can this sandbox actually run here? Starts it once, with Node checking
    /// from the inside that the browser, its launcher and bubblewrap (for the
    /// browser's own PID namespace) are present. Cached per spec.
    pub fn usable(&self) -> bool {
        static CACHE: OnceLock<std::sync::Mutex<Vec<(String, bool)>>> = OnceLock::new();
        let key = format!("{self:?}");
        let cache = CACHE.get_or_init(Default::default);
        if let Some((_, ok)) = cache.lock().unwrap().iter().find(|(k, _)| *k == key) {
            return *ok;
        }
        let ok = self.preflight().is_ok();
        cache.lock().unwrap().push((key, ok));
        ok
    }

    /// [`usable`](Self::usable), with the reason when it is not.
    pub fn preflight(&self) -> Result<()> {
        ensure!(
            containment_available(),
            "bubblewrap is unavailable or the host refuses its namespaces"
        );
        let scratch = tempfile::Builder::new()
            .prefix("ato-browser-preflight-")
            .tempdir()
            .context("no scratch directory")?;
        let check = format!(
            "const fs = require('fs'); \
             for (const p of ['{chrome}', '{launcher}', '/usr/bin/bwrap']) fs.accessSync(p, fs.constants.X_OK); \
             for (const p of ['{home}']) if (fs.existsSync(p)) process.exit(3); \
             process.exit(0)",
            chrome = self.guest_chrome(),
            launcher = GUEST_CHROME_LAUNCHER,
            // The host's own home must not exist in here.
            home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_owned()),
        );
        let argv = self.argv_running(scratch.path(), &[], &["-e".to_owned(), check])?;
        let mut child = Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("cannot start bubblewrap")?;
        let deadline = Instant::now() + Duration::from_secs(20);
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                bail!("the verifier sandbox did not start within 20 s");
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        if !status.success() {
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                use std::io::Read as _;
                let _ = pipe.read_to_string(&mut stderr);
            }
            bail!(
                "the verifier sandbox preflight failed ({status}): {}",
                stderr.trim().chars().take(400).collect::<String>()
            );
        }
        Ok(())
    }
}

/// A program name resolved the way a shell would, then canonicalized.
fn resolve_program(program: &str) -> Result<PathBuf> {
    let path = Path::new(program);
    if path.components().count() > 1 {
        return path
            .canonicalize()
            .with_context(|| format!("cannot read {program}"));
    }
    let search = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&search)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
        .with_context(|| format!("{program} is not on PATH"))?
        .canonicalize()
        .with_context(|| format!("cannot read {program}"))
}

/// A bind that would expose more than the thing itself is refused: the
/// filesystem root, the user's home or anything above it, or anything that
/// is, contains or sits inside a credential directory.
fn refuse_broad_bind(what: &str, path: &Path) -> Result<()> {
    ensure!(
        path.components().any(|c| matches!(c, Component::Normal(_))),
        "the {what} resolves to the filesystem root; refusing to expose it"
    );
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        let home = home.canonicalize().unwrap_or(home);
        ensure!(
            !home.starts_with(path),
            "the {what} ({}) is the home directory or contains it; refusing to expose it",
            path.display()
        );
    }
    let mut sensitive = ato_sandbox::sensitive_paths();
    // Per-user configuration, including the file the worker reads its model
    // keys from.
    if let Some(home) = std::env::var_os("HOME") {
        sensitive.push(PathBuf::from(home).join(".config"));
    }
    for sensitive in sensitive {
        let sensitive = sensitive.canonicalize().unwrap_or(sensitive);
        ensure!(
            !path.starts_with(&sensitive) && !sensitive.starts_with(path),
            "the {what} overlaps a credential directory; refusing to expose it"
        );
    }
    Ok(())
}

fn utf8(path: &Path) -> Result<String> {
    path.to_str()
        .map(ToOwned::to_owned)
        .context("a verifier path is not valid UTF-8")
}
