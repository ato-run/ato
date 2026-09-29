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

use super::egress::{EgressAllowance, EgressGate, EgressReport};
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
    /// Needed only for an egress session.
    pub ip: PathBuf,
    pub nsenter: PathBuf,
    pub iptables: PathBuf,
    pub sysctl: PathBuf,
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
            ip: "/usr/sbin/ip".into(),
            nsenter: "/usr/bin/nsenter".into(),
            iptables: "/usr/sbin/iptables".into(),
            sysctl: "/usr/sbin/sysctl".into(),
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
    egress: Option<EgressWiring>,
}

/// The only way out of an egress session: a veth whose host end carries the
/// egress gate. Host INPUT accepts only the gate port from it and FORWARD
/// drops it; inside the namespace DOCKER-USER confines build steps to the
/// gate and there is no default route.
struct EgressWiring {
    allowance: EgressAllowance,
    host_if: String,
    ns_if: String,
    host_ip: std::net::Ipv4Addr,
    ns_ip: std::net::Ipv4Addr,
    bridge: String,
    comment: String,
    gate: RefCell<Option<EgressGate>>,
    port: Cell<u16>,
    host_rules: RefCell<Vec<(String, Vec<String>)>>,
    report: RefCell<Option<EgressReport>>,
}

impl EgressWiring {
    fn proxy(&self) -> String {
        format!("http://{}:{}", self.host_ip, self.port.get())
    }
    fn docker_user_rules(&self) -> Vec<String> {
        let (ip, port) = (self.host_ip, self.port.get());
        vec![
            "-N DOCKER-USER".to_owned(),
            format!("-A DOCKER-USER -d {ip}/32 -p tcp -m tcp --dport {port} -j RETURN"),
            format!("-A DOCKER-USER -s {ip}/32 -p tcp -m tcp --sport {port} -j RETURN"),
            "-A DOCKER-USER -j DROP".to_owned(),
        ]
    }
}

/// Build-time egress for one session.
pub struct SessionEgress {
    pub hosts: Vec<String>,
    pub ports: Vec<u16>,
    pub max_transfer_bytes: u64,
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

fn run_tool_output(program: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new(program)
        .env_clear()
        .env("PATH", TOOL_PATH)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(io(&program.display().to_string()))?;
    if !output.status.success() {
        return Err(err(
            "source_oci_session_failed",
            format!(
                "{} {args:?}: {}",
                program.display(),
                tail(&String::from_utf8_lossy(&output.stderr), 1000)
            ),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Names and addresses private to one session: interface names outside
/// Ato's runtime egress bridges (`atoe*`), a /30 in 10.198/16 that no host
/// interface already uses, and a bridge in 10.199/16 inside the namespace.
fn egress_wiring(egress: SessionEgress, seed: u128) -> Result<EgressWiring> {
    let used = fs::read_to_string("/proc/net/fib_trie").unwrap_or_default();
    for attempt in 0..16_u128 {
        let v = (seed.wrapping_add(attempt * 7919) % (1 << 22)) as u32;
        let (a, b) = ((v >> 8) as u8, ((v & 0x3f) as u8) << 2);
        let host_ip = std::net::Ipv4Addr::new(10, 198, a, b + 1);
        let ns_ip = std::net::Ipv4Addr::new(10, 198, a, b + 2);
        if used.contains(&format!("10.198.{a}.{}", b + 1))
            || used.contains(&format!("10.198.{a}.{}", b + 2))
        {
            continue;
        }
        let id = format!("{:06x}", v & 0xff_ffff);
        return Ok(EgressWiring {
            allowance: EgressAllowance {
                hosts: egress.hosts,
                ports: egress.ports,
                max_transfer_bytes: egress.max_transfer_bytes,
            },
            host_if: format!("asoh{id}"),
            ns_if: format!("ason{id}"),
            host_ip,
            ns_ip,
            bridge: format!("10.199.{a}.1/24"),
            comment: format!("ato-source-oci-egress-{id}"),
            gate: RefCell::new(None),
            port: Cell::new(0),
            host_rules: RefCell::new(Vec::new()),
            report: RefCell::new(None),
        });
    }
    Err(refused("no free private address for the egress veth"))
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
    pub fn start(
        work_root: &Path,
        limits: &BuildLimits,
        tools: SessionTools,
        egress: Option<SessionEgress>,
    ) -> Result<Self> {
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
        ]
        .into_iter()
        .chain(
            egress
                .is_some()
                .then_some([&tools.ip, &tools.nsenter, &tools.iptables, &tools.sysctl])
                .into_iter()
                .flatten(),
        ) {
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
        let egress = egress.map(|e| egress_wiring(e, nanos)).transpose()?;
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
            egress,
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
            .args(match &self.egress {
                // Offline: no bridge, no forwarding, no NAT.
                None => vec![
                    "--bridge=none".to_owned(),
                    "--iptables=false".to_owned(),
                    "--ip-forward=false".to_owned(),
                ],
                // Egress: a bridge private to this namespace; NAT stays
                // inside it, and the only way out is the gate veth.
                Some(e) => vec!["--bip".to_owned(), e.bridge.clone()],
            })
            .args([
                "--ip6tables=false",
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
        let mut expected = IsolationExpectation::offline(
            format!("/{}/daemon", self.cgroup_name),
            self.fs_root.join("data").display().to_string(),
        );
        if let Some(e) = &self.egress {
            self.wire_egress()?;
            let mut interfaces = vec!["docker0".to_owned(), "lo".to_owned(), e.ns_if.clone()];
            interfaces.sort();
            expected.interfaces = interfaces;
            expected.routes = 2;
            expected.docker_user_rules = Some(e.docker_user_rules());
        }
        let facts = self.observe();
        check_isolation(&facts, &expected, &self.limits)?;
        self.facts = Some(facts);
        Ok(())
    }

    fn daemon_pid(&self) -> u32 {
        self.daemon.borrow().as_ref().map(Child::id).unwrap_or(0)
    }

    /// Run a tool inside the daemon's network namespace.
    fn in_netns(&self, program: &Path, args: &[&str]) -> Result<String> {
        let netns = format!("--net=/proc/{}/ns/net", self.daemon_pid());
        let mut all = vec![netns.as_str(), "--"];
        let program = program.display().to_string();
        all.push(&program);
        all.extend_from_slice(args);
        run_tool_output(&self.tools.nsenter, &all)
    }

    fn host_rule(&self, chain: &str, spec: &[String]) -> Result<()> {
        let e = self.egress.as_ref().expect("egress");
        let mut args = vec![
            "-w".to_owned(),
            "5".to_owned(),
            "-I".to_owned(),
            chain.to_owned(),
            "1".to_owned(),
        ];
        args.extend(spec.iter().cloned());
        args.extend(["-m", "comment", "--comment", e.comment.as_str()].map(str::to_owned));
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        run_tool(&self.tools.iptables, &refs)?;
        let mut stored = spec.to_vec();
        stored.extend(["-m", "comment", "--comment", e.comment.as_str()].map(str::to_owned));
        e.host_rules.borrow_mut().push((chain.to_owned(), stored));
        Ok(())
    }

    /// veth, gate, host firewall, namespace addresses and DOCKER-USER, in an
    /// order that never leaves a path out of the namespace open.
    fn wire_egress(&self) -> Result<()> {
        let e = self.egress.as_ref().expect("egress");
        let pid = self.daemon_pid().to_string();
        let s = |v: &str| v.to_owned();
        // Host firewall first: nothing from the veth is accepted or
        // forwarded until the gate port is known.
        self.host_rule("FORWARD", &[s("-i"), e.host_if.clone(), s("-j"), s("DROP")])?;
        self.host_rule("FORWARD", &[s("-o"), e.host_if.clone(), s("-j"), s("DROP")])?;
        self.host_rule("INPUT", &[s("-i"), e.host_if.clone(), s("-j"), s("DROP")])?;
        run_tool(
            &self.tools.ip,
            &[
                "link", "add", &e.host_if, "type", "veth", "peer", "name", &e.ns_if,
            ],
        )?;
        run_tool(
            &self.tools.ip,
            &[
                "addr",
                "add",
                &format!("{}/30", e.host_ip),
                "dev",
                &e.host_if,
            ],
        )?;
        run_tool(&self.tools.ip, &["link", "set", &e.host_if, "up"])?;
        let gate = EgressGate::start(
            std::net::SocketAddr::from((e.host_ip, 0)),
            e.allowance.clone(),
        )?;
        e.port.set(gate.address().port());
        *e.gate.borrow_mut() = Some(gate);
        let port = e.port.get().to_string();
        self.host_rule(
            "INPUT",
            &[
                s("-i"),
                e.host_if.clone(),
                s("-p"),
                s("tcp"),
                s("--dport"),
                port.clone(),
                s("-j"),
                s("ACCEPT"),
            ],
        )?;
        run_tool(&self.tools.ip, &["link", "set", &e.ns_if, "netns", &pid])?;
        for key in [
            "net.ipv6.conf.all.disable_ipv6=1",
            "net.ipv6.conf.default.disable_ipv6=1",
        ] {
            self.in_netns(&self.tools.sysctl, &["-q", "-w", key])?;
        }
        self.in_netns(
            &self.tools.ip,
            &["addr", "add", &format!("{}/30", e.ns_ip), "dev", &e.ns_if],
        )?;
        self.in_netns(&self.tools.ip, &["link", "set", &e.ns_if, "up"])?;
        self.in_netns(&self.tools.ip, &["link", "set", "lo", "up"])?;
        let host_ip = format!("{}/32", e.host_ip);
        let ipt = |args: &[&str]| self.in_netns(&self.tools.iptables, args);
        // Build steps (forwarded from docker0) reach only the gate.
        ipt(&["-w", "5", "-I", "DOCKER-USER", "1", "-j", "DROP"])?;
        ipt(&[
            "-w",
            "5",
            "-I",
            "DOCKER-USER",
            "1",
            "-s",
            &host_ip,
            "-p",
            "tcp",
            "--sport",
            &port,
            "-j",
            "RETURN",
        ])?;
        ipt(&[
            "-w",
            "5",
            "-I",
            "DOCKER-USER",
            "1",
            "-d",
            &host_ip,
            "-p",
            "tcp",
            "--dport",
            &port,
            "-j",
            "RETURN",
        ])?;
        // The daemon itself never goes out through the veth except to the gate.
        ipt(&[
            "-w", "5", "-A", "OUTPUT", "-o", &e.ns_if, "-d", &host_ip, "-p", "tcp", "--dport",
            &port, "-j", "ACCEPT",
        ])?;
        ipt(&["-w", "5", "-A", "OUTPUT", "-o", &e.ns_if, "-j", "DROP"])?;
        Ok(())
    }

    fn docker_user_rules(&self) -> Vec<String> {
        self.in_netns(&self.tools.iptables, &["-w", "5", "-S", "DOCKER-USER"])
            .map(|s| s.lines().map(|l| l.trim().to_owned()).collect())
            .unwrap_or_default()
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
        let v4_table = fs::read_to_string(proc.join("net/route")).unwrap_or_default();
        let v4 = v4_table
            .lines()
            .skip(1)
            .filter(|l| l.split_whitespace().next().is_some_and(|i| i != "lo"))
            .count();
        let v6_table = fs::read_to_string(proc.join("net/ipv6_route")).unwrap_or_default();
        // Destination 00000000 with mask 00000000, or ::/0.
        let default_routes = v4_table
            .lines()
            .skip(1)
            .filter(|l| {
                let f: Vec<&str> = l.split_whitespace().collect();
                f.len() > 7 && f[1] == "00000000" && f[7] == "00000000"
            })
            .count()
            + v6_table
                .lines()
                .filter(|l| {
                    let f: Vec<&str> = l.split_whitespace().collect();
                    f.len() > 9
                        && f[0] == "00000000000000000000000000000000"
                        && f[1] == "00"
                        && f[9] != "lo"
                })
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
            default_routes,
            docker_user_rules: if self.egress.is_some() {
                self.docker_user_rules()
            } else {
                vec![]
            },
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
        if let Some(e) = &self.egress
            && let Some(gate) = e.gate.borrow_mut().take()
        {
            *e.report.borrow_mut() = Some(gate.stop());
        }
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
        if let Some(e) = &self.egress {
            if Path::new("/sys/class/net").join(&e.host_if).exists()
                && let Err(error) = run_tool(&self.tools.ip, &["link", "del", &e.host_if])
            {
                problems.push(format!("veth {}: {}", e.host_if, error.detail));
            }
            for (chain, spec) in e.host_rules.borrow().iter().rev() {
                let mut args = vec![
                    "-w".to_owned(),
                    "5".to_owned(),
                    "-D".to_owned(),
                    chain.clone(),
                ];
                args.extend(spec.iter().cloned());
                let refs: Vec<&str> = args.iter().map(String::as_str).collect();
                if let Err(error) = run_tool(&self.tools.iptables, &refs) {
                    problems.push(format!("host rule {chain}: {}", error.detail));
                }
            }
            let rules =
                run_tool_output(&self.tools.iptables, &["-w", "5", "-S"]).unwrap_or_default();
            if rules.contains(&e.comment) {
                problems.push(format!("host rules tagged {} remain", e.comment));
            }
            if Path::new("/sys/class/net").join(&e.host_if).exists() {
                problems.push(format!("interface {} remains", e.host_if));
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
        command.args(["buildx", "build", "--builder", "default"]);
        match &self.session.egress {
            // Offline: RUN steps have no network at all.
            None => {
                command.arg("--network=none");
            }
            // Online: RUN steps use the namespace's private bridge, whose
            // only way out is the gate; the Dockerfile is unchanged and the
            // proxy arrives through BuildKit's predefined proxy build args,
            // which are not recorded in the image history.
            Some(e) => {
                for name in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
                    command
                        .arg("--build-arg")
                        .arg(format!("{name}={}", e.proxy()));
                }
            }
        }
        command.args([
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
    fn preflight_egress(&self, image: &str) -> Result<Value> {
        let e = self.session.egress.as_ref().ok_or_else(|| {
            err(
                "source_oci_network_unauthorized",
                "this session has no egress gate",
            )
        })?;
        let listed = e.allowance.hosts.first().expect("validated allowlist");
        // A public address of a listed host, resolved on the host: dialing
        // it directly must fail just like any other destination.
        let listed_ip = std::net::ToSocketAddrs::to_socket_addrs(&(listed.as_str(), 443))
            .ok()
            .and_then(|mut a| a.find(|a| a.is_ipv4()))
            .map(|a| a.ip().to_string())
            .unwrap_or_else(|| "1.1.1.1".to_owned());
        let gate = e.host_ip.to_string();
        let port = e.port.get().to_string();
        let script = r#"
d() { if nc -w 3 "$1" "$2" </dev/null >/dev/null 2>&1; then echo "OPEN $1:$2"; else echo "BLOCKED $1:$2"; fi; }
p() { printf 'CONNECT %s HTTP/1.1
Host: %s

' "$1" "$1" | nc -w 5 "$GATE" "$PORT" 2>/dev/null | head -n 1; }
command -v nc >/dev/null 2>&1 || { echo "NO_NC"; exit 0; }
d "$LISTED_IP" 443
d 1.1.1.1 443
d "$GATE" 22
echo "UNLISTED $(p example.com:443)"
echo "LISTED $(p "$LISTED":443)"
"#;
        let output = self
            .session
            .command()
            .args([
                "run",
                "--rm",
                "--network",
                "bridge",
                "--read-only",
                "--cap-drop=ALL",
                "--security-opt=no-new-privileges",
                "--user",
                "65534:65534",
                "--pull=never",
                "--entrypoint",
                "/bin/sh",
                "--env",
                &format!("GATE={gate}"),
                "--env",
                &format!("PORT={port}"),
                "--env",
                &format!("LISTED={listed}"),
                "--env",
                &format!("LISTED_IP={listed_ip}"),
                image,
                "-c",
                script,
            ])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| err("source_oci_builder_unavailable", e.to_string()))?;
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        if !output.status.success() || lines.contains(&"NO_NC") {
            return Err(err(
                "source_oci_egress_preflight_unavailable",
                format!(
                    "the probe image cannot run the preflight: {}{}",
                    text.trim(),
                    tail(&String::from_utf8_lossy(&output.stderr), 500)
                ),
            ));
        }
        let direct: Vec<&str> = lines
            .iter()
            .copied()
            .filter(|l| l.starts_with("OPEN ") || l.starts_with("BLOCKED "))
            .collect();
        let unlisted = lines
            .iter()
            .find_map(|l| l.strip_prefix("UNLISTED "))
            .unwrap_or_default();
        let allowed = lines
            .iter()
            .find_map(|l| l.strip_prefix("LISTED "))
            .unwrap_or_default();
        let result = json!({
            "probe_image": image,
            "gate": format!("{gate}:{port}"),
            "direct": direct,
            "unlisted_connect": unlisted,
            "listed_connect": allowed,
            "listed_host": listed,
        });
        if direct.len() != 3
            || direct.iter().any(|l| l.starts_with("OPEN "))
            || !unlisted.contains(" 403")
            || !allowed.contains(" 200")
        {
            return Err(err(
                "source_oci_egress_preflight_failed",
                format!("the gate is bypassable or not effective: {result}"),
            ));
        }
        Ok(result)
    }
    fn egress_report(&self) -> Option<Value> {
        let e = self.session.egress.as_ref()?;
        let report = match e.gate.borrow().as_ref() {
            Some(gate) => gate.report(),
            None => e.report.borrow().clone()?,
        };
        serde_json::to_value(report).ok()
    }
    fn egress_exhausted(&self) -> bool {
        self.session
            .egress
            .as_ref()
            .and_then(|e| e.gate.borrow().as_ref().map(EgressGate::exhausted))
            .unwrap_or(false)
    }
    fn package_inventory(&self, image: &str) -> Result<Option<Vec<String>>> {
        if self.session.egress.is_none() {
            return Ok(None);
        }
        // Offline, read-only, unprivileged: what the package manager of the
        // built image reports, if it has one.
        let output = self
            .session
            .command()
            .args([
                "run",
                "--rm",
                "--network",
                "none",
                "--read-only",
                "--cap-drop=ALL",
                "--security-opt=no-new-privileges",
                "--pull=never",
                "--entrypoint",
                "/bin/sh",
                image,
                "-c",
                "command -v apk >/dev/null 2>&1 && apk info -v 2>/dev/null | sort",
            ])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| err("source_oci_builder_unavailable", e.to_string()))?;
        let packages: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect();
        Ok((output.status.success() && !packages.is_empty()).then_some(packages))
    }
}
