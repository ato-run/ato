//! The browser verifier's own sandbox.
//!
//! The verifier is verification infrastructure, not a Derivation: it is never
//! lowered as a candidate workload, and it does not share the build sandbox's
//! policy. What it shares are the primitives — bubblewrap, its detection, the
//! system paths a process needs — and one rule: refuse rather than run
//! unconfined.
//!
//! ```text
//! worker (host)
//!   ├─ bwrap  helper sandbox                    ├─ bwrap  browser sandbox (one per launch)
//!   │    /verifier       ro  helper + modules   │    /runtime/chrome  ro  the Chrome directory
//!   │    /runtime/node   ro  the Node install   │    /scratch         rw  the same scratch
//!   │    /scratch        rw  HOME, TMPDIR       │    empty environment, own PID namespace
//!   │    keys on fd 3, none in any environment  │
//!   │    └─ bin/chrome-contained.cjs ──unix socket /scratch/.browser-launcher.sock──▶ worker
//!   └─ both: /usr /lib /bin …, a few /etc read-only; nothing else — no home, no
//!      repository, no credentials, no token or key files
//! ```
//!
//! The network is shared: the helper must reach its judge and agent models,
//! and the browser's traffic is confined to the candidate's origin by the
//! helper's guard proxy (ADR-020), which this does not change.
//!
//! Chrome is never a process of the helper's sandbox. Stagehand starts
//! `bin/chrome-contained.cjs`, which only asks the worker — over a socket in
//! the scratch directory — to start the browser; the worker starts it in a
//! sandbox of its own, a sibling of the helper's rather than nested inside it
//! (nesting needs a user namespace inside a user namespace, which hosts with
//! AppArmor's unprivileged-userns restriction refuse). The browser has an
//! empty environment and its own PID namespace, so a compromised browser can
//! neither read the helper's environment nor see the helper's process. The
//! helper's model keys are not in any process environment to begin with: they
//! arrive on file descriptor 3.
//!
//! Chrome's own sandbox runs inside the browser sandbox when the host allows
//! it; where it cannot (again, the userns restriction), the browser runs with
//! `--no-sandbox` inside ours, and the receipt says so.

use std::io::{BufRead as _, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use ato_formation::browser::VerifierContainment;

use crate::sandbox::containment_available;

pub const GUEST_VERIFIER_ROOT: &str = "/verifier";
pub const GUEST_NODE_ROOT: &str = "/runtime/node";
pub const GUEST_CHROME_ROOT: &str = "/runtime/chrome";
pub const GUEST_SCRATCH: &str = "/scratch";
/// What Stagehand starts as "the browser": a client that asks the worker to
/// start the real one in its own sandbox.
pub const GUEST_CHROME_LAUNCHER: &str = "/verifier/bin/chrome-contained.cjs";
/// The worker's browser launcher, as the helper sees it.
pub const LAUNCHER_SOCKET_NAME: &str = ".browser-launcher.sock";
/// Browsers one verification may start.
const MAX_BROWSER_LAUNCHES: usize = 4;
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

/// Whether Chrome's own sandbox runs inside the browser sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromeSandbox {
    /// Chrome's renderer sandbox is on.
    Enabled,
    /// The host refuses the namespaces Chrome's sandbox needs; the browser
    /// runs with `--no-sandbox`, inside the browser sandbox.
    Disabled,
}

/// The isolation this sandbox provides, as recorded in a receipt.
pub fn contained_evidence(chrome: ChromeSandbox) -> VerifierContainment {
    VerifierContainment {
        containment: "bwrap".to_owned(),
        filesystem: "allowlisted".to_owned(),
        network: "shared-host-net+exact-origin-browser-guard".to_owned(),
        browser_process: match chrome {
            ChromeSandbox::Enabled => "separate-sandbox+empty-environment+chrome-sandbox",
            ChromeSandbox::Disabled => "separate-sandbox+empty-environment+chrome-no-sandbox",
        }
        .to_owned(),
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
            helper_root.join("bin/chrome-contained.cjs").is_file(),
            "the browser verifier has no bin/chrome-contained.cjs launcher client"
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
        self.helper_argv(scratch, settings, &self.entry)
    }

    /// What both sandboxes share: fresh namespaces except the network, the
    /// system read-only, the scratch directory as `/scratch`, nothing else.
    fn base_argv(scratch: &Path) -> Result<Vec<String>> {
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
        argv.extend([
            "--bind".to_owned(),
            utf8(scratch)?,
            GUEST_SCRATCH.to_owned(),
        ]);
        argv.push("--clearenv".to_owned());
        for (name, value) in [
            ("HOME", format!("{GUEST_SCRATCH}/home")),
            ("TMPDIR", format!("{GUEST_SCRATCH}/tmp")),
            ("LANG", "C.UTF-8".to_owned()),
        ] {
            argv.extend(["--setenv".to_owned(), name.to_owned(), value]);
        }
        Ok(argv)
    }

    fn helper_argv(
        &self,
        scratch: &Path,
        settings: &[(String, String)],
        entry: &[String],
    ) -> Result<Vec<String>> {
        let mut argv = Self::base_argv(scratch)?;
        // No Chrome here: the browser runs in a sandbox of its own.
        for (host, guest) in [
            (&self.helper_root, GUEST_VERIFIER_ROOT),
            (&self.node_root, GUEST_NODE_ROOT),
        ] {
            argv.extend(["--ro-bind".to_owned(), utf8(host)?, guest.to_owned()]);
        }
        let mut env: Vec<(String, String)> = vec![
            (
                "PATH".to_owned(),
                format!("{GUEST_NODE_ROOT}/bin:{GUEST_NODE_ROOT}:/usr/bin:/bin"),
            ),
            (
                "ATO_BROWSER_CHROME_PATH".to_owned(),
                GUEST_CHROME_LAUNCHER.to_owned(),
            ),
            (
                "ATO_BROWSER_LAUNCHER_SOCKET".to_owned(),
                format!("{GUEST_SCRATCH}/{LAUNCHER_SOCKET_NAME}"),
            ),
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

    /// The bwrap argv that runs the browser with `args`, in its own sandbox.
    pub fn browser_argv(
        &self,
        scratch: &Path,
        args: &[String],
        chrome: ChromeSandbox,
    ) -> Result<Vec<String>> {
        let mut argv = Self::base_argv(scratch)?;
        argv.extend([
            "--ro-bind".to_owned(),
            utf8(&self.chrome_root)?,
            GUEST_CHROME_ROOT.to_owned(),
        ]);
        argv.extend([
            "--setenv".to_owned(),
            "PATH".to_owned(),
            "/usr/bin:/bin".to_owned(),
        ]);
        argv.extend([
            "--chdir".to_owned(),
            GUEST_SCRATCH.to_owned(),
            "--".to_owned(),
        ]);
        argv.push(self.guest_chrome());
        if chrome == ChromeSandbox::Disabled {
            argv.push("--no-sandbox".to_owned());
        }
        argv.extend(args.iter().cloned());
        Ok(argv)
    }

    /// Can this sandbox actually run here, and with Chrome's own sandbox or
    /// without? Starts both sandboxes once (see [`preflight`](Self::preflight)).
    /// Cached per spec.
    pub fn usable(&self) -> bool {
        self.chrome_sandbox().is_some()
    }

    /// The Chrome sandbox mode this host supports, or `None` when the
    /// verifier sandbox cannot run here at all.
    pub fn chrome_sandbox(&self) -> Option<ChromeSandbox> {
        type Cache = Mutex<Vec<(String, Option<ChromeSandbox>)>>;
        static CACHE: OnceLock<Cache> = OnceLock::new();
        let key = format!("{self:?}");
        let cache = CACHE.get_or_init(Default::default);
        if let Some((_, mode)) = cache.lock().unwrap().iter().find(|(k, _)| *k == key) {
            return *mode;
        }
        let mode = self.preflight().ok();
        cache.lock().unwrap().push((key, mode));
        mode
    }

    /// [`chrome_sandbox`](Self::chrome_sandbox), with the reason when there
    /// is none: the helper sandbox starts and Node finds its launcher client
    /// while the host's home is absent; the browser sandbox starts Chrome far
    /// enough to open its DevTools endpoint — with Chrome's own sandbox if
    /// the host allows it, else without.
    pub fn preflight(&self) -> Result<ChromeSandbox> {
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
             fs.accessSync('{launcher}', fs.constants.R_OK); \
             if (fs.existsSync('{home}')) process.exit(3); \
             process.exit(0)",
            launcher = GUEST_CHROME_LAUNCHER,
            // The host's own home must not exist in here.
            home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_owned()),
        );
        let argv = self.helper_argv(scratch.path(), &[], &["-e".to_owned(), check])?;
        run_to_completion(&argv, Duration::from_secs(20))
            .context("the helper sandbox preflight failed")?;

        let mut failures = Vec::new();
        for mode in [ChromeSandbox::Enabled, ChromeSandbox::Disabled] {
            match self.browser_starts(scratch.path(), mode) {
                Ok(()) => return Ok(mode),
                Err(error) => failures.push(format!("{mode:?}: {error:#}")),
            }
        }
        bail!(
            "the browser sandbox could not start Chrome ({})",
            failures.join("; ")
        )
    }

    /// Start Chrome headless in the browser sandbox until it has opened its
    /// DevTools endpoint, then stop it.
    fn browser_starts(&self, scratch: &Path, mode: ChromeSandbox) -> Result<()> {
        let profile = format!("probe-{mode:?}");
        std::fs::create_dir_all(scratch.join("home"))?;
        std::fs::create_dir_all(scratch.join("tmp"))?;
        let args = [
            "--headless=new",
            "--no-first-run",
            "--disable-gpu",
            "--remote-debugging-port=0",
            &format!("--user-data-dir={GUEST_SCRATCH}/{profile}"),
            "about:blank",
        ]
        .map(str::to_owned);
        let argv = self.browser_argv(scratch, &args, mode)?;
        let mut command = Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        {
            use std::os::unix::process::CommandExt as _;
            command.process_group(0);
        }
        let mut child = command.spawn().context("cannot start bubblewrap")?;
        let ready = scratch.join(&profile).join("DevToolsActivePort");
        let deadline = Instant::now() + Duration::from_secs(20);
        let outcome = loop {
            if ready.is_file() {
                break Ok(());
            }
            if let Some(status) = child.try_wait()? {
                break Err(anyhow::anyhow!("Chrome exited ({status})"));
            }
            if Instant::now() >= deadline {
                break Err(anyhow::anyhow!("Chrome did not open DevTools within 20 s"));
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        kill_group(child.id());
        let _ = child.wait();
        outcome
    }
}

/// Run `argv` to completion within `timeout`; its failure is an error.
fn run_to_completion(argv: &[String], timeout: Duration) -> Result<()> {
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("cannot start bubblewrap")?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("did not finish within {} s", timeout.as_secs());
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
            "{status}: {}",
            stderr.trim().chars().take(400).collect::<String>()
        );
    }
    Ok(())
}

fn kill_group(group: u32) {
    // SAFETY: signalling a process group this module created; one that is
    // already gone makes `kill` fail harmlessly.
    unsafe {
        libc::kill(-(group as libc::pid_t), libc::SIGKILL);
    }
}

/// The worker's side of `bin/chrome-contained.cjs`: for each request on the
/// socket, one browser in its own sandbox, alive exactly as long as the
/// requesting client and never longer than the verification.
pub struct BrowserLauncher {
    stop: Arc<AtomicBool>,
    groups: Arc<Mutex<Vec<u32>>>,
    threads: Vec<std::thread::JoinHandle<()>>,
    socket: PathBuf,
}

impl BrowserLauncher {
    /// Listen on `<scratch>/.browser-launcher.sock`.
    pub fn start(
        spec: &BrowserVerifierSandboxSpec,
        scratch: &Path,
        chrome: ChromeSandbox,
    ) -> Result<Self> {
        let socket = scratch.join(LAUNCHER_SOCKET_NAME);
        let listener = UnixListener::bind(&socket)
            .with_context(|| "cannot open the browser launcher socket".to_owned())?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let groups = Arc::new(Mutex::new(Vec::new()));
        let launched = Arc::new(AtomicUsize::new(0));
        let accept = {
            let (stop, groups) = (stop.clone(), groups.clone());
            let spec = spec.clone();
            let scratch = scratch.to_path_buf();
            std::thread::spawn(move || {
                let mut handlers = Vec::new();
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let (stop, groups, launched) =
                                (stop.clone(), groups.clone(), launched.clone());
                            let (spec, scratch) = (spec.clone(), scratch.clone());
                            handlers.push(std::thread::spawn(move || {
                                serve_launch(
                                    stream, &spec, &scratch, chrome, &stop, &groups, &launched,
                                )
                            }));
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(50));
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(50)),
                    }
                }
                for handler in handlers {
                    let _ = handler.join();
                }
            })
        };
        Ok(Self {
            stop,
            groups,
            threads: vec![accept],
            socket,
        })
    }

    /// Stop every browser this launcher started, and the launcher.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for group in self.groups.lock().unwrap().drain(..) {
            kill_group(group);
        }
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.socket);
    }
}

impl Drop for BrowserLauncher {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// One request: `{"args": [...]}` on a line. The worker, not the client,
/// decides the executable, the sandbox and `--no-sandbox`; the client only
/// supplies Chrome's arguments. The answer is `exit <code>` when the browser
/// exits; a client that goes away takes its browser with it.
fn serve_launch(
    stream: UnixStream,
    spec: &BrowserVerifierSandboxSpec,
    scratch: &Path,
    chrome: ChromeSandbox,
    stop: &AtomicBool,
    groups: &Mutex<Vec<u32>>,
    launched: &AtomicUsize,
) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut line = String::new();
    {
        let mut reader = std::io::BufReader::new(std::io::Read::take(&stream, 64 * 1024));
        if reader.read_line(&mut line).is_err() {
            return;
        }
    }
    let args: Vec<String> = match serde_json::from_str::<serde_json::Value>(&line) {
        Ok(value) => match value.get("args").and_then(|args| args.as_array()) {
            Some(args) if args.iter().all(serde_json::Value::is_string) && args.len() <= 256 => {
                args.iter()
                    .filter_map(|arg| arg.as_str().map(str::to_owned))
                    .collect()
            }
            _ => return,
        },
        Err(_) => return,
    };
    let refuse = |mut stream: &UnixStream, why: &str| {
        let _ = writeln!(stream, "refused {why}");
    };
    if launched.fetch_add(1, Ordering::Relaxed) >= MAX_BROWSER_LAUNCHES {
        return refuse(&stream, "too many browsers for one verification");
    }
    let Ok(argv) = spec.browser_argv(scratch, &args, chrome) else {
        return refuse(&stream, "no browser sandbox");
    };
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    let Ok(mut child) = command.spawn() else {
        return refuse(&stream, "cannot start bubblewrap");
    };
    groups.lock().unwrap().push(child.id());
    let _ = writeln!(&stream, "started");
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            let _ = writeln!(&stream, "exit {}", status.code().unwrap_or(1));
            break;
        }
        if stop.load(Ordering::Relaxed) || client_gone(&stream) {
            kill_group(child.id());
            let _ = child.wait();
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    groups.lock().unwrap().retain(|group| *group != child.id());
}

/// Has the client closed its end?
fn client_gone(stream: &UnixStream) -> bool {
    use std::os::fd::AsRawFd as _;
    let mut byte = 0_u8;
    // SAFETY: a non-blocking peek of one byte into a local buffer.
    let read = unsafe {
        libc::recv(
            stream.as_raw_fd(),
            (&mut byte as *mut u8).cast(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    if read == 0 {
        return true;
    }
    if read < 0 {
        let error = std::io::Error::last_os_error();
        return error.kind() != std::io::ErrorKind::WouldBlock;
    }
    false
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
