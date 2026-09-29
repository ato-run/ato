//! The private builder session: the trust boundary of a source build.
//!
//! One session owns, for one job only: a new work root, a fixed-size ext4
//! filesystem (loop mount) holding the daemon data root, BuildKit state, the
//! build context and the saved image; a cgroup v2 subtree with memory, CPU
//! and PID limits that contains the daemon *and* every build step; a network
//! namespace with only `lo`; and the daemon socket. The socket is never taken
//! from the caller. Before any image is loaded the session reads back what
//! the kernel and the daemon report and refuses unless every condition holds
//! ([`super::isolation::check_isolation`]). Release stops the daemon, kills
//! whatever remains in the cgroup, and confirms that no process, mount, loop
//! device or cgroup of the session is left before deleting the work root.
use std::cell::{Cell, RefCell};
use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::isolation::{IsolationExpectation, IsolationFacts, check_isolation, cpu_max};
use super::{
    BuildLimits, BuildOutcome, MAX_BUILD_LOG_BYTES, OciBuilder, Result, SELECTED_DOCKERFILE, err,
    tail,
};

/// Absolute paths of the host tools the session runs; nothing is resolved
/// through the caller's `PATH`.
#[derive(Debug, Clone)]
pub struct SessionTools {
    pub sh: PathBuf,
    pub docker: PathBuf,
    pub dockerd: PathBuf,
    pub unshare: PathBuf,
    pub mkfs_ext4: PathBuf,
    pub mount: PathBuf,
    pub umount: PathBuf,
    pub kill: PathBuf,
}

impl Default for SessionTools {
    fn default() -> Self {
        Self {
            sh: "/bin/sh".into(),
            docker: "/usr/bin/docker".into(),
            dockerd: "/usr/bin/dockerd".into(),
            unshare: "/usr/bin/unshare".into(),
            mkfs_ext4: "/usr/sbin/mkfs.ext4".into(),
            mount: "/usr/bin/mount".into(),
            umount: "/usr/bin/umount".into(),
            kill: "/usr/bin/kill".into(),
        }
    }
}

const TOOL_PATH: &str = "/usr/sbin:/usr/bin:/sbin:/bin";
const CGROUP_ROOT: &str = "/sys/fs/cgroup";
/// Keeps the daemon's socket paths (containerd lives under the exec root)
/// inside the 108-byte `sun_path` limit.
const MAX_WORK_ROOT_BYTES: usize = 48;

pub struct PrivateDockerSession {
    work_root: PathBuf,
    image: PathBuf,
    fs_root: PathBuf,
    cgroup: PathBuf,
    cgroup_name: String,
    socket: PathBuf,
    tools: SessionTools,
    limits: BuildLimits,
    daemon: RefCell<Option<Child>>,
    facts: Option<IsolationFacts>,
    released: Cell<bool>,
    cgroup_made: bool,
}

fn refused(detail: impl Into<String>) -> super::SourceOciError {
    err("source_oci_session_refused", detail)
}
fn io(what: &str) -> impl Fn(std::io::Error) -> super::SourceOciError + '_ {
    move |e| err("source_oci_session_failed", format!("{what}: {e}"))
}

fn run_tool(program: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new(program)
        .env_clear()
        .env("PATH", TOOL_PATH)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(io(&program.display().to_string()))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(err(
            "source_oci_session_failed",
            format!(
                "{} {args:?}: {}",
                program.display(),
                tail(&String::from_utf8_lossy(&output.stderr), 1000)
            ),
        ))
    }
}

fn read_trim(path: &Path) -> String {
    fs::read_to_string(path)
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
}

/// Mount points at or below `root`, deepest first, from this process's view.
fn mounts_under(root: &Path) -> Vec<PathBuf> {
    let text = fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    let mut mounts: Vec<PathBuf> = text
        .lines()
        .filter_map(|line| line.split(' ').nth(4))
        .map(unescape_mount)
        .map(PathBuf::from)
        .filter(|p| p.starts_with(root))
        .collect();
    mounts.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    mounts.dedup();
    mounts
}

fn unescape_mount(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 4 <= bytes.len() {
            let octal = std::str::from_utf8(&bytes[i + 1..i + 4]).unwrap_or("");
            if let Ok(v) = u8::from_str_radix(octal, 8) {
                out.push(v);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn loop_devices_backed_by(image: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir("/sys/block") else {
        return vec![];
    };
    entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("loop"))
        .filter(|e| Path::new(&read_trim(&e.path().join("loop/backing_file"))) == image)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect()
}

/// The listening unix socket bound at `socket` in `pid`'s network namespace
/// is held open by `pid` itself: its inode appears among the process's file
/// descriptors. A path that is an alias of another daemon's socket fails.
fn socket_listener(pid: u32, socket: &Path) -> Option<u32> {
    let table = fs::read_to_string(format!("/proc/{pid}/net/unix")).ok()?;
    let wanted = socket.display().to_string();
    let inodes: Vec<String> = table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            // Num RefCount Protocol Flags Type St Inode Path; Flags 00010000 = listening.
            (fields.len() >= 8 && fields[3] == "00010000" && fields[7] == wanted)
                .then(|| fields[6].to_owned())
        })
        .collect();
    let [inode] = inodes.as_slice() else {
        return None;
    };
    let target = format!("socket:[{inode}]");
    fs::read_dir(format!("/proc/{pid}/fd"))
        .ok()?
        .flatten()
        .any(|fd| fs::read_link(fd.path()).is_ok_and(|l| l.display().to_string() == target))
        .then_some(pid)
}

/// Effective UID from /proc (no libc call; this crate forbids unsafe code).
fn effective_uid() -> Option<u32> {
    fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("Uid:"))
        .and_then(|ids| ids.split_whitespace().nth(1))
        .and_then(|id| id.parse().ok())
}

impl PrivateDockerSession {
    /// Create and verify a new session under `work_root`, which must not
    /// exist. On any failure everything already created is released; a
    /// release failure is kept next to the original error.
    pub fn start(work_root: &Path, limits: &BuildLimits, tools: SessionTools) -> Result<Self> {
        if effective_uid() != Some(0) {
            return Err(refused(
                "a private builder session must own a network namespace, a cgroup and a filesystem (root required)",
            ));
        }
        let text = work_root
            .to_str()
            .ok_or_else(|| refused("work root is not UTF-8"))?;
        if !work_root.is_absolute()
            || text.len() > MAX_WORK_ROOT_BYTES
            || work_root
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        {
            return Err(refused(format!(
                "work root must be a normalized absolute path of at most {MAX_WORK_ROOT_BYTES} bytes"
            )));
        }
        if fs::symlink_metadata(work_root).is_ok() {
            return Err(refused(
                "work root already exists; sessions are never reused",
            ));
        }
        let parent = work_root
            .parent()
            .filter(|p| fs::symlink_metadata(p).is_ok_and(|m| m.is_dir()))
            .ok_or_else(|| refused("work root parent must be an existing directory"))?;
        let _ = parent;
        for tool in [
            &tools.sh,
            &tools.docker,
            &tools.dockerd,
            &tools.unshare,
            &tools.mkfs_ext4,
            &tools.mount,
            &tools.umount,
            &tools.kill,
        ] {
            if !tool.is_absolute() || !tool.is_file() {
                return Err(refused(format!(
                    "tool {} is not an absolute file",
                    tool.display()
                )));
            }
        }
        let controllers = read_trim(&Path::new(CGROUP_ROOT).join("cgroup.subtree_control"));
        if !["cpu", "memory", "pids"]
            .iter()
            .all(|c| controllers.split(' ').any(|x| x == *c))
        {
            return Err(refused(format!(
                "cgroup v2 root does not delegate cpu, memory and pids ({controllers:?})"
            )));
        }
        fs::DirBuilder::new()
            .mode(0o700)
            .create(work_root)
            .map_err(io("create work root"))?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let cgroup_name = format!("ato-source-oci-{}-{nanos}", std::process::id());
        let fs_root = work_root.join("fs");
        let mut session = Self {
            work_root: work_root.to_path_buf(),
            image: work_root.join("fs.img"),
            socket: fs_root.join("docker.sock"),
            fs_root,
            cgroup: Path::new(CGROUP_ROOT).join(&cgroup_name),
            cgroup_name,
            tools,
            limits: limits.clone(),
            daemon: RefCell::new(None),
            facts: None,
            released: Cell::new(false),
            cgroup_made: false,
        };
        match session.setup() {
            Ok(()) => Ok(session),
            Err(mut e) => {
                if let Err(cleanup) = session.release() {
                    e.cleanup = Some(Box::new(cleanup));
                }
                Err(e)
            }
        }
    }

    fn setup(&mut self) -> Result<()> {
        // Fixed-size filesystem: disk use of the daemon, BuildKit, context
        // and save is bounded by the kernel, not by after-the-fact checks.
        let image = File::create(&self.image).map_err(io("create session disk"))?;
        image
            .set_len(self.limits.disk_bytes)
            .map_err(io("size session disk"))?;
        drop(image);
        let image = self.image.display().to_string();
        run_tool(&self.tools.mkfs_ext4, &["-q", "-F", "-m", "0", &image])?;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&self.fs_root)
            .map_err(io("create session mount point"))?;
        run_tool(
            &self.tools.mount,
            &[
                "-o",
                "loop,nodev,nosuid",
                &image,
                &self.fs_root.display().to_string(),
            ],
        )?;
        // One cgroup for the daemon and (through --cgroup-parent) every
        // build step; the limits apply to their sum.
        fs::create_dir(&self.cgroup).map_err(io("create session cgroup"))?;
        self.cgroup_made = true;
        let write =
            |file: &str, value: &str| fs::write(self.cgroup.join(file), value).map_err(io(file));
        write("memory.max", &self.limits.memory_bytes.to_string())?;
        if self.cgroup.join("memory.swap.max").exists() {
            write("memory.swap.max", "0")?;
        }
        write("pids.max", &self.limits.pids_limit.to_string())?;
        write("cpu.max", &cpu_max(&self.limits))?;
        write("cgroup.subtree_control", "+cpu +memory +pids")?;
        for child in ["daemon", "builds"] {
            fs::create_dir(self.cgroup.join(child)).map_err(io("create session cgroup child"))?;
        }
        for dir in ["data", "exec", "cli", "job"] {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(self.fs_root.join(dir))
                .map_err(io("create session layout"))?;
        }
        // An empty daemon config: the host's daemon.json (mirrors, proxies,
        // registries, data root) never applies to a session.
        fs::write(self.fs_root.join("daemon.json"), "{}").map_err(io("daemon config"))?;
        let log = File::create(self.fs_root.join("dockerd.log")).map_err(io("daemon log"))?;
        let path = |p: &Path| p.display().to_string();
        let child = Command::new(&self.tools.sh)
            .env_clear()
            .env("PATH", TOOL_PATH)
            .arg("-c")
            // Enter the session cgroup before exec, so no daemon process can
            // start outside it; then a new network namespace for the daemon.
            .arg("echo $$ > \"$1\" && shift && exec \"$@\"")
            .arg("sh")
            .arg(self.cgroup.join("daemon/cgroup.procs"))
            .arg(&self.tools.unshare)
            .args(["--net", "--"])
            .arg(&self.tools.dockerd)
            .args(["--config-file", &path(&self.fs_root.join("daemon.json"))])
            .args(["--host", &format!("unix://{}", path(&self.socket))])
            .args(["--data-root", &path(&self.fs_root.join("data"))])
            .args(["--exec-root", &path(&self.fs_root.join("exec"))])
            .args(["--pidfile", &path(&self.fs_root.join("dockerd.pid"))])
            .args(["--feature", "containerd-snapshotter=false"])
            .args(["--exec-opt", "native.cgroupdriver=cgroupfs"])
            .args(["--cgroup-parent", &format!("/{}/builds", self.cgroup_name)])
            .args([
                "--bridge=none",
                "--iptables=false",
                "--ip6tables=false",
                "--ip-forward=false",
                "--userland-proxy=false",
                "--log-level",
                "warn",
            ])
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(io("daemon log"))?)
            .stderr(log)
            .spawn()
            .map_err(io("start private daemon"))?;
        *self.daemon.borrow_mut() = Some(child);
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let exited = self
                .daemon
                .borrow_mut()
                .as_mut()
                .and_then(|c| c.try_wait().ok().flatten());
            if let Some(status) = exited {
                let log = fs::read_to_string(self.fs_root.join("dockerd.log")).unwrap_or_default();
                return Err(err(
                    "source_oci_session_failed",
                    format!("private daemon exited ({status}): {}", tail(&log, 1500)),
                ));
            }
            if fs::symlink_metadata(&self.socket).is_ok() && self.docker(&["version"]).is_ok() {
                break;
            }
            if Instant::now() >= deadline {
                return Err(err(
                    "source_oci_session_failed",
                    "private daemon did not become ready",
                ));
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        let facts = self.observe();
        check_isolation(
            &facts,
            &IsolationExpectation {
                daemon_cgroup: format!("/{}/daemon", self.cgroup_name),
                root_dir: self.fs_root.join("data").display().to_string(),
            },
            &self.limits,
        )?;
        self.facts = Some(facts);
        Ok(())
    }

    fn observe(&self) -> IsolationFacts {
        let pid = self.daemon.borrow().as_ref().map(Child::id).unwrap_or(0);
        let proc = PathBuf::from(format!("/proc/{pid}"));
        let link = |p: &Path| {
            fs::read_link(p)
                .map(|l| l.display().to_string())
                .unwrap_or_default()
        };
        let interfaces = fs::read_to_string(proc.join("net/dev"))
            .unwrap_or_default()
            .lines()
            .skip(2)
            .filter_map(|l| l.split(':').next().map(|i| i.trim().to_owned()))
            .collect();
        let v4 = fs::read_to_string(proc.join("net/route"))
            .unwrap_or_default()
            .lines()
            .skip(1)
            .filter(|l| l.split_whitespace().next().is_some_and(|i| i != "lo"))
            .count();
        let v6 = fs::read_to_string(proc.join("net/ipv6_route"))
            .unwrap_or_default()
            .lines()
            .filter(|l| l.split_whitespace().last().is_some_and(|i| i != "lo"))
            .count();
        let daemon_cgroup = fs::read_to_string(proc.join("cgroup"))
            .unwrap_or_default()
            .lines()
            .find_map(|l| l.strip_prefix("0::").map(str::to_owned))
            .unwrap_or_default();
        let info = self
            .docker(&["info", "--format", "{{.DockerRootDir}}\n{{.CgroupDriver}}"])
            .unwrap_or_default();
        let mut info = info.lines();
        IsolationFacts {
            daemon_pid: pid,
            daemon_comm: read_trim(&proc.join("comm")),
            daemon_netns: link(&proc.join("ns/net")),
            host_netns: link(Path::new("/proc/1/ns/net")),
            caller_netns: link(Path::new("/proc/self/ns/net")),
            interfaces,
            non_loopback_routes: v4 + v6,
            daemon_cgroup,
            memory_max: read_trim(&self.cgroup.join("memory.max")),
            pids_max: read_trim(&self.cgroup.join("pids.max")),
            cpu_max: read_trim(&self.cgroup.join("cpu.max")),
            socket_is_socket: fs::symlink_metadata(&self.socket)
                .is_ok_and(|m| m.file_type().is_socket()),
            socket_listener_pid: socket_listener(pid, &self.socket),
            reported_root_dir: info.next().unwrap_or_default().trim().to_owned(),
            reported_cgroup_driver: info.next().unwrap_or_default().trim().to_owned(),
        }
    }

    fn command(&self) -> Command {
        let cli = self.fs_root.join("cli");
        let mut command = Command::new(&self.tools.docker);
        // Only the session daemon; no inherited proxies, contexts or
        // credentials (DOCKER_CONFIG is an empty session directory).
        command
            .env_clear()
            .env("PATH", TOOL_PATH)
            .env("DOCKER_HOST", format!("unix://{}", self.socket.display()))
            .env("DOCKER_CONFIG", &cli)
            .env("HOME", &cli)
            .env("DOCKER_BUILDKIT", "1");
        command
    }

    fn docker(&self, args: &[&str]) -> Result<String> {
        let output = self
            .command()
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| err("source_oci_builder_unavailable", e.to_string()))?;
        if !output.status.success() {
            return Err(err(
                "source_oci_builder_failed",
                tail(&String::from_utf8_lossy(&output.stderr), 2000),
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Idempotent. Stops the daemon, kills everything left in the session
    /// cgroup (including build steps a lost client left behind), unmounts,
    /// and removes the work root only when nothing of the session remains.
    pub fn release(&self) -> Result<()> {
        if self.released.replace(true) {
            return Ok(());
        }
        let mut problems = Vec::new();
        if let Some(mut child) = self.daemon.borrow_mut().take() {
            // Graceful stop first so the daemon unmounts its own layers.
            let _ = run_tool(&self.tools.kill, &["-TERM", &child.id().to_string()]);
            let deadline = Instant::now() + Duration::from_secs(20);
            while Instant::now() < deadline && matches!(child.try_wait(), Ok(None)) {
                std::thread::sleep(Duration::from_millis(100));
            }
            if matches!(child.try_wait(), Ok(None)) {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
        if self.cgroup_made {
            let _ = fs::write(self.cgroup.join("cgroup.kill"), "1");
            let deadline = Instant::now() + Duration::from_secs(15);
            let populated = || {
                read_trim(&self.cgroup.join("cgroup.events"))
                    .lines()
                    .any(|l| l == "populated 1")
            };
            while populated() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
            if populated() {
                problems.push("processes remain in the session cgroup".to_owned());
            }
        }
        for mount in mounts_under(&self.work_root) {
            if let Err(e) = run_tool(&self.tools.umount, &[&mount.display().to_string()]) {
                problems.push(format!("umount {}: {}", mount.display(), e.detail));
            }
        }
        let left = mounts_under(&self.work_root);
        if !left.is_empty() {
            problems.push(format!("mounts remain: {left:?}"));
        }
        if self.cgroup_made && self.cgroup.exists() {
            let mut dirs = vec![];
            let mut stack = vec![self.cgroup.clone()];
            while let Some(dir) = stack.pop() {
                if let Ok(entries) = fs::read_dir(&dir) {
                    stack.extend(entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
                }
                dirs.push(dir);
            }
            dirs.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
            for dir in dirs {
                if let Err(e) = fs::remove_dir(&dir) {
                    problems.push(format!("cgroup {}: {e}", dir.display()));
                }
            }
        }
        let loops = loop_devices_backed_by(&self.image);
        if !loops.is_empty() {
            problems.push(format!("loop devices remain: {loops:?}"));
        }
        if problems.is_empty() {
            if let Err(e) = fs::remove_dir_all(&self.work_root) {
                problems.push(format!("remove work root: {e}"));
            }
        } else {
            // The work root is never reused; mark why it was kept.
            let _ = fs::write(
                self.work_root.join("CLEANUP-UNCONFIRMED"),
                problems.join("\n"),
            );
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(err("source_oci_cleanup_unconfirmed", problems.join("; ")))
        }
    }
}

impl Drop for PrivateDockerSession {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

/// BuildKit (docker driver) over one [`PrivateDockerSession`].
pub struct DockerCliBuilder {
    session: PrivateDockerSession,
    scratch: PathBuf,
}

impl DockerCliBuilder {
    pub fn new(session: PrivateDockerSession) -> Self {
        let scratch = session.fs_root.join("job");
        Self { session, scratch }
    }
}

impl OciBuilder for DockerCliBuilder {
    fn scratch(&self) -> &Path {
        &self.scratch
    }
    fn identity(&self) -> Result<Value> {
        let server = self
            .session
            .docker(&["version", "--format", "{{.Server.Version}}"])?;
        let buildx = self.session.docker(&["buildx", "version"])?;
        Ok(json!({
            "docker_server": server.trim(),
            "buildx": buildx.trim(),
            "driver": "docker (private daemon, classic image store)",
            "session": {
                "limits": self.session.limits,
                "cgroup_parent_for_build_steps": format!("/{}/builds", self.session.cgroup_name),
                "isolation": self.session.facts,
                "daemon_config": "empty (host daemon.json not used)",
                "cache": "session-local; deleted with the session",
            },
        }))
    }
    fn image_ids(&self) -> Result<Vec<String>> {
        Ok(self
            .session
            .docker(&["image", "ls", "--all", "--quiet", "--no-trunc"])?
            .lines()
            .map(str::to_owned)
            .collect())
    }
    fn load(&self, archive: &Path) -> Result<()> {
        self.session
            .docker(&["load", "--quiet", "--input", &archive.display().to_string()])
            .map(|_| ())
    }
    fn tag(&self, image: &str, tag: &str) -> Result<()> {
        self.session.docker(&["tag", image, tag]).map(|_| ())
    }
    fn build(
        &self,
        context: &Path,
        platform: &str,
        named_contexts: &[(String, String)],
        tag: &str,
        timeout: Duration,
    ) -> Result<BuildOutcome> {
        let mut command = self.session.command();
        command.args([
            "buildx",
            "build",
            "--builder",
            "default",
            "--network=none",
            "--pull=false",
            "--provenance=false",
            "--sbom=false",
            "--progress=plain",
            "--platform",
            platform,
            "--file",
        ]);
        command.arg(context.join(SELECTED_DOCKERFILE));
        for (name, target) in named_contexts {
            command
                .arg("--build-context")
                .arg(format!("{name}={target}"));
        }
        command.args(["--tag", tag, "--load"]).arg(context);
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| err("source_oci_builder_unavailable", e.to_string()))?;
        // Progress output is read as it arrives and bounded: only a tail is
        // kept, and exceeding the bound aborts the build.
        let mut stderr = child.stderr.take().expect("piped");
        let exceeded = Arc::new(AtomicBool::new(false));
        let flag = exceeded.clone();
        let reader = std::thread::spawn(move || {
            let mut kept: Vec<u8> = Vec::new();
            let mut total = 0_u64;
            let mut buffer = [0_u8; 8192];
            while let Ok(n) = stderr.read(&mut buffer) {
                if n == 0 {
                    break;
                }
                total += n as u64;
                kept.extend_from_slice(&buffer[..n]);
                if kept.len() > 16 * 1024 {
                    kept.drain(..kept.len() - 8 * 1024);
                }
                if total > MAX_BUILD_LOG_BYTES {
                    flag.store(true, Ordering::SeqCst);
                }
            }
            String::from_utf8_lossy(&kept).into_owned()
        });
        let deadline = Instant::now() + timeout;
        let result = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok(Some(status)),
                Ok(None) if Instant::now() >= deadline => break Ok(None),
                Ok(None) if exceeded.load(Ordering::SeqCst) => break Ok(None),
                Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                Err(e) => break Err(e),
            }
        };
        let status = match result {
            Ok(status) => status,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(err("source_oci_builder_unavailable", e.to_string()));
            }
        };
        if status.is_none() {
            // The daemon-side build is stopped by the session release, which
            // confirms that nothing remains in the session cgroup.
            let _ = child.kill();
            let _ = child.wait();
        }
        let log = reader.join().unwrap_or_default();
        Ok(match status {
            _ if exceeded.load(Ordering::SeqCst) => BuildOutcome::LogExceeded,
            None => BuildOutcome::TimedOut,
            Some(s) if s.success() => BuildOutcome::Built,
            Some(_) => BuildOutcome::Failed {
                log_tail: tail(&log, 4000),
            },
        })
    }
    fn image_id(&self, reference: &str) -> Result<String> {
        Ok(self
            .session
            .docker(&["image", "inspect", "--format", "{{.Id}}", reference])?
            .trim()
            .to_owned())
    }
    fn image_size(&self, image_id: &str) -> Result<u64> {
        self.session
            .docker(&["image", "inspect", "--format", "{{.Size}}", image_id])?
            .trim()
            .parse()
            .map_err(|_| err("source_oci_builder_failed", "unreadable image size"))
    }
    fn save(&self, image_id: &str, output: &Path) -> Result<()> {
        // By image ID: the archive carries no repository tags.
        self.session
            .docker(&["save", "--output", &output.display().to_string(), image_id])
            .map(|_| ())
    }
    fn release(&self) -> Result<()> {
        self.session.release()
    }
}
