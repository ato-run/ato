//! Application-layer construction of bootable Firecracker guest filesystems.
//!
//! A build plan is decoded from an existing Computation and an explicit
//! physical profile. Docker is only a filesystem construction tool: the
//! Workspace Snapshot and every file are re-read from CAS, and the resulting
//! ext4 bytes never define the Computation identity.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};
use ato_adapter_workspace::{WorkspaceSnapshot, restore_workspace};
use ato_computation::{ComputationRef, ContentRef, SemanticsId};
use ato_objects::{ObjectResolver, read_exact_object, resolve_computation};
use guest_agent::supervisor::{StdioMode, SupervisorConfig};
use serde::{Deserialize, Serialize};

const AUTHORING_SEMANTICS_ID: &str = "ato.authoring@1";
const HTTP_ADAPTER_ID: &str = "ato.http@1";
const MAX_AUTHORING_BYTES: u64 = 16 * 1024 * 1024;
const MAX_WORKSPACE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct GuestPhysicalBuildProfile {
    pub base_image: String,
    pub guest_agent: PathBuf,
    pub kernel: PathBuf,
    pub image_size_mib: u64,
    pub network: GuestNetwork,
    pub container_tool: String,
    pub mke2fs: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestNetwork {
    pub guest_ip: String,
    pub host_ip: String,
    pub netmask: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GuestBuildPlan {
    pub target_computation_ref: String,
    pub workspace: GuestWorkspacePlan,
    pub processes: Vec<GuestProcess>,
    pub http_relays: Vec<HttpRelay>,
    pub base_image: String,
    pub guest_agent: String,
    pub kernel: String,
    pub image_size_mib: u64,
    pub network: GuestNetwork,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GuestWorkspacePlan {
    pub snapshot_ref: String,
    pub file_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GuestProcess {
    pub id: String,
    pub command: Vec<String>,
    pub cwd: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HttpRelay {
    pub port_id: String,
    pub guest_port: u16,
    pub target: SocketAddr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GuestImageReceipt {
    pub plan: GuestBuildPlan,
    pub rootfs_path: String,
    pub rootfs_bytes: u64,
    pub filesystem_uuid: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthoringState {
    version: u32,
    config: AuthoringConfig,
    workspace_snapshot: String,
    #[serde(default)]
    semantic_frontier: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthoringConfig {
    schema: u32,
    #[serde(default)]
    process: Vec<ProcessConfig>,
    #[serde(default)]
    adapter: Vec<AdapterConfig>,
    #[serde(default)]
    port: Vec<PortConfig>,
    #[serde(default)]
    connection: Vec<serde_json::Value>,
    #[serde(default)]
    binding: Vec<serde_json::Value>,
    #[serde(default)]
    workspace: serde_json::Value,
    #[serde(default)]
    encap: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessConfig {
    id: String,
    command: Vec<String>,
    #[serde(default = "default_cwd")]
    cwd: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AdapterConfig {
    #[serde(rename = "use")]
    use_adapter: String,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    port: Option<String>,
    #[serde(default)]
    listen: Option<String>,
    #[serde(default)]
    upstream: Option<String>,
    #[serde(default)]
    input: Option<String>,
    #[serde(default)]
    ready_path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PortConfig {
    id: String,
    node: String,
    protocol: String,
    role: String,
    #[serde(default)]
    address: Option<String>,
    #[serde(default)]
    environment: Option<String>,
    #[serde(default)]
    internal: bool,
}

fn default_cwd() -> PathBuf {
    PathBuf::from(".")
}

pub fn derive_guest_build_plan(
    target: &ComputationRef,
    objects: &dyn ObjectResolver,
    profile: &GuestPhysicalBuildProfile,
) -> Result<GuestBuildPlan> {
    validate_profile(profile)?;
    let computation = resolve_computation(objects, target)?;
    ensure!(
        computation.object().semantics == SemanticsId::parse(AUTHORING_SEMANTICS_ID)?,
        "fresh guest construction requires ato.authoring@1"
    );
    let state = load_authoring_state(&computation, objects)?;
    ensure!(state.version == 1, "unsupported authoring state version");
    ensure!(state.config.schema == 1, "unsupported authoring schema");
    ensure!(
        state.config.binding.is_empty(),
        "guest build requires explicit binding support"
    );
    let _ = (
        &state.config.connection,
        &state.config.workspace,
        &state.config.encap,
    );
    let _ = &state.semantic_frontier;

    let snapshot_ref = ContentRef::parse(&state.workspace_snapshot)?;
    let snapshot = load_workspace_snapshot(&snapshot_ref, objects)?;
    verify_workspace_files(&snapshot, objects)?;

    let mut process_ids = BTreeSet::new();
    let mut processes = Vec::new();
    for process in state.config.process {
        ensure!(
            !process.id.is_empty() && !process.command.is_empty(),
            "process requires id and command"
        );
        ensure!(
            process_ids.insert(process.id.clone()),
            "duplicate process id"
        );
        validate_relative_path(&process.cwd)?;
        let cwd = if process.cwd == Path::new(".") {
            "/workspace".to_owned()
        } else {
            format!("/workspace/{}", process.cwd.to_string_lossy())
        };
        processes.push(GuestProcess {
            id: process.id,
            command: process.command,
            cwd,
        });
    }
    ensure!(!processes.is_empty(), "Computation has no process");

    let ports = state
        .config
        .port
        .into_iter()
        .map(|port| (port.id.clone(), port))
        .collect::<BTreeMap<_, _>>();
    let mut relay_ports = BTreeSet::new();
    let mut http_relays = Vec::new();
    for adapter in state
        .config
        .adapter
        .into_iter()
        .filter(|adapter| adapter.use_adapter == HTTP_ADAPTER_ID)
    {
        let port_id = adapter.port.context("HTTP Adapter omitted semantic Port")?;
        let port = ports
            .get(&port_id)
            .context("HTTP Adapter names unknown Port")?;
        ensure!(
            port.protocol == HTTP_ADAPTER_ID,
            "HTTP Adapter Port protocol mismatch"
        );
        ensure!(
            process_ids.contains(&port.node),
            "HTTP Port owner is not a process"
        );
        let _ = (&port.role, &port.address, &port.environment, port.internal);
        if let Some(target_id) = adapter.target.as_deref() {
            ensure!(
                target_id == port.node,
                "HTTP Adapter target/Port owner mismatch"
            );
        }
        let listen: SocketAddr = adapter
            .listen
            .context("HTTP Adapter omitted listen address")?
            .parse()?;
        let upstream: SocketAddr = adapter
            .upstream
            .context("HTTP Adapter omitted upstream address")?
            .parse()?;
        ensure!(
            listen.ip().is_loopback(),
            "host HTTP Adapter listen must be loopback"
        );
        ensure!(
            upstream.ip().is_loopback(),
            "guest application upstream must be loopback"
        );
        ensure!(
            relay_ports.insert(listen.port()),
            "duplicate guest relay port"
        );
        let _ = (&adapter.input, &adapter.ready_path);
        http_relays.push(HttpRelay {
            port_id,
            guest_port: listen.port(),
            target: upstream,
        });
    }
    ensure!(
        !http_relays.is_empty(),
        "Computation has no ato.http@1 Adapter"
    );

    Ok(GuestBuildPlan {
        target_computation_ref: target.to_string(),
        workspace: GuestWorkspacePlan {
            snapshot_ref: snapshot_ref.to_string(),
            file_count: snapshot.files.len(),
        },
        processes,
        http_relays,
        base_image: profile.base_image.clone(),
        guest_agent: profile.guest_agent.display().to_string(),
        kernel: profile.kernel.display().to_string(),
        image_size_mib: profile.image_size_mib,
        network: profile.network.clone(),
    })
}

pub fn build_guest_image(
    target: &ComputationRef,
    objects: &dyn ObjectResolver,
    profile: &GuestPhysicalBuildProfile,
    work_root: &Path,
    output: &Path,
) -> Result<GuestImageReceipt> {
    let plan = derive_guest_build_plan(target, objects, profile)?;
    fs::create_dir_all(work_root)?;
    let build = tempfile::Builder::new()
        .prefix("firecracker-guest-image-")
        .tempdir_in(work_root)?;
    let context = build.path().join("context");
    let workspace = context.join("workspace");
    fs::create_dir_all(&workspace)?;
    restore_workspace(
        &ContentRef::parse(&plan.workspace.snapshot_ref)?,
        &workspace,
        objects,
    )?;
    fs::create_dir_all(context.join("ato"))?;
    fs::copy(&profile.guest_agent, context.join("ato/ato-guest-agent"))?;
    write_file(
        &context.join("ato/supervisor.json"),
        &serde_json::to_vec_pretty(&supervisor_config(&plan)?)?,
    )?;
    write_file(&context.join("ato/init"), init_script(&plan).as_bytes())?;
    write_file(
        &context.join("Dockerfile"),
        dockerfile(&plan.base_image).as_bytes(),
    )?;

    let tag = format!("ato-firecracker-guest:{}", safe_tag(target));
    command_ok(
        Command::new(&profile.container_tool)
            .args(["build", "--pull", "-q", "-t", &tag])
            .arg(&context),
        "build pinned guest image",
    )?;
    let mut image = ImageGuard::new(profile.container_tool.clone(), tag.clone());
    let container_id = command_stdout(
        Command::new(&profile.container_tool).args(["create", &tag]),
        "create guest export container",
    )?;
    let mut container = ContainerGuard::new(profile.container_tool.clone(), container_id);
    let archive = build.path().join("rootfs.tar");
    command_ok(
        Command::new(&profile.container_tool)
            .args(["export", "-o"])
            .arg(&archive)
            .arg(container.id()),
        "export guest filesystem",
    )?;
    container.remove()?;
    let rootfs_dir = build.path().join("rootfs");
    fs::create_dir_all(&rootfs_dir)?;
    command_ok(
        Command::new("tar")
            .args(["-xf"])
            .arg(&archive)
            .args(["-C"])
            .arg(&rootfs_dir),
        "extract guest filesystem",
    )?;
    image.remove()?;

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let rootfs = File::create(output)?;
    rootfs.set_len(profile.image_size_mib * 1024 * 1024)?;
    let filesystem_uuid = filesystem_uuid(&plan);
    command_ok(
        Command::new(&profile.mke2fs)
            .args(["-q", "-F", "-t", "ext4", "-U", &filesystem_uuid, "-d"])
            .arg(&rootfs_dir)
            .arg(output),
        "pack ext4 guest filesystem",
    )?;
    File::open(output)?.sync_all()?;
    let rootfs_bytes = fs::metadata(output)?.len();
    ensure!(rootfs_bytes > 0, "guest rootfs is empty");
    Ok(GuestImageReceipt {
        plan,
        rootfs_path: output.display().to_string(),
        rootfs_bytes,
        filesystem_uuid,
    })
}

fn validate_profile(profile: &GuestPhysicalBuildProfile) -> Result<()> {
    ensure!(
        profile.base_image.contains("@sha256:"),
        "base image must be pinned by immutable sha256 digest"
    );
    ensure!(
        profile.guest_agent.is_file(),
        "guest-agent binary is missing"
    );
    ensure!(profile.kernel.is_file(), "Firecracker kernel is missing");
    ensure!(
        (64..=8192).contains(&profile.image_size_mib),
        "image size is outside bounds"
    );
    ensure!(
        !profile.container_tool.trim().is_empty(),
        "container tool is empty"
    );
    ensure!(profile.mke2fs.is_file(), "mke2fs binary is missing");
    profile.network.guest_ip.parse::<std::net::Ipv4Addr>()?;
    profile.network.host_ip.parse::<std::net::Ipv4Addr>()?;
    profile.network.netmask.parse::<std::net::Ipv4Addr>()?;
    Ok(())
}

fn load_authoring_state(
    computation: &ato_computation::ResolvedComputation,
    objects: &dyn ObjectResolver,
) -> Result<AuthoringState> {
    let reference = &computation.object().residual;
    let metadata = objects.metadata(reference)?;
    let bytes = read_exact_object(objects, reference, metadata.size, MAX_AUTHORING_BYTES)?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    ensure!(
        serde_jcs::to_vec(&value)? == bytes,
        "authoring state is not canonical JCS"
    );
    Ok(serde_json::from_value(value)?)
}

fn load_workspace_snapshot(
    reference: &ContentRef,
    objects: &dyn ObjectResolver,
) -> Result<WorkspaceSnapshot> {
    let metadata = objects.metadata(reference)?;
    let bytes = read_exact_object(objects, reference, metadata.size, MAX_WORKSPACE_BYTES)?;
    let snapshot: WorkspaceSnapshot = serde_json::from_slice(&bytes)?;
    ensure!(
        serde_jcs::to_vec(&snapshot)? == bytes,
        "Workspace Snapshot is not canonical JCS"
    );
    Ok(snapshot)
}

fn verify_workspace_files(
    snapshot: &WorkspaceSnapshot,
    objects: &dyn ObjectResolver,
) -> Result<()> {
    for (path, reference) in &snapshot.files {
        validate_relative_path(Path::new(path))?;
        let reference = ContentRef::parse(reference)?;
        let metadata = objects.metadata(&reference)?;
        let _ = read_exact_object(objects, &reference, metadata.size, MAX_WORKSPACE_BYTES)?;
    }
    Ok(())
}

fn validate_relative_path(path: &Path) -> Result<()> {
    ensure!(
        !path.as_os_str().is_empty() && !path.is_absolute(),
        "workspace path is not relative"
    );
    ensure!(
        path.components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir)),
        "workspace path escapes its boundary"
    );
    Ok(())
}

fn supervisor_config(plan: &GuestBuildPlan) -> Result<SupervisorConfig> {
    ensure!(
        plan.processes.len() == 1,
        "current guest supervisor profile requires one process"
    );
    let process = &plan.processes[0];
    Ok(SupervisorConfig {
        stdio_mode: StdioMode::Log,
        cmd: process.command.clone(),
        cwd: process.cwd.clone(),
        base_env: BTreeMap::new(),
        bindings_env: BTreeMap::new(),
        services: Vec::new(),
        volumes: Vec::new(),
        generated_bindings: Vec::new(),
    })
}

fn dockerfile(base_image: &str) -> String {
    format!(
        "FROM {base_image}\nCOPY workspace/ /workspace/\nCOPY ato/ato-guest-agent /usr/local/bin/ato-guest-agent\nCOPY ato/supervisor.json /etc/ato/supervisor.json\nCOPY ato/init /sbin/init\nRUN chmod 0755 /usr/local/bin/ato-guest-agent /sbin/init\n"
    )
}

fn init_script(plan: &GuestBuildPlan) -> String {
    let relays = plan
        .http_relays
        .iter()
        .map(|relay| {
            format!(
                "/usr/local/bin/ato-guest-agent tcp-relay --listen-guest-port {} --target {} >/dev/console 2>&1 &",
                relay.guest_port, relay.target
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "#!/bin/sh\nexport PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\nexport PYTHONDONTWRITEBYTECODE=1 HOME=/tmp\nmount -t proc proc /proc 2>/dev/null || true\nmount -t sysfs sysfs /sys 2>/dev/null || true\nmount -t devtmpfs devtmpfs /dev 2>/dev/null || true\nmount -t tmpfs tmpfs /tmp 2>/dev/null || true\nmount -t tmpfs tmpfs /run 2>/dev/null || true\nmkdir -p /run/ato/bindings\n{relays}\nexport ATO_GUEST_AGENT_MODE=vsock ATO_GUEST_AGENT_VSOCK_PORT=1025 ATO_BINDINGS_ROOT=/run/ato/bindings\nexec /usr/local/bin/ato-guest-agent >/dev/console 2>&1\n"
    )
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn command_ok(command: &mut Command, stage: &str) -> Result<()> {
    let output = command.output().with_context(|| format!("spawn {stage}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let tail = stderr
        .lines()
        .rev()
        .take(12)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    bail!("{stage} failed: {tail}")
}

fn command_stdout(command: &mut Command, stage: &str) -> Result<String> {
    let output = command.output().with_context(|| format!("spawn {stage}"))?;
    ensure!(output.status.success(), "{stage} failed");
    let value = String::from_utf8(output.stdout)?.trim().to_owned();
    ensure!(!value.is_empty(), "{stage} returned an empty identifier");
    Ok(value)
}

fn safe_tag(target: &ComputationRef) -> String {
    target.content_ref().digest().chars().take(24).collect()
}

fn filesystem_uuid(plan: &GuestBuildPlan) -> String {
    let bytes = blake3::hash(&serde_jcs::to_vec(plan).expect("serializable plan"));
    let hex = bytes.to_hex();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

struct ImageGuard {
    tool: String,
    tag: Option<String>,
}

impl ImageGuard {
    fn new(tool: String, tag: String) -> Self {
        Self {
            tool,
            tag: Some(tag),
        }
    }
    fn remove(&mut self) -> Result<()> {
        if let Some(tag) = self.tag.take() {
            command_ok(
                Command::new(&self.tool).args(["rmi", "-f", &tag]),
                "remove guest build image",
            )?;
        }
        Ok(())
    }
}

impl Drop for ImageGuard {
    fn drop(&mut self) {
        if let Some(tag) = self.tag.take() {
            let _ = Command::new(&self.tool).args(["rmi", "-f", &tag]).status();
        }
    }
}

struct ContainerGuard {
    tool: String,
    id: Option<String>,
}

impl ContainerGuard {
    fn new(tool: String, id: String) -> Self {
        Self { tool, id: Some(id) }
    }
    fn id(&self) -> &str {
        self.id.as_deref().expect("container is owned")
    }
    fn remove(&mut self) -> Result<()> {
        if let Some(id) = self.id.take() {
            command_ok(
                Command::new(&self.tool).args(["rm", "-f", &id]),
                "remove guest export container",
            )?;
        }
        Ok(())
    }
}

impl Drop for ContainerGuard {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            let _ = Command::new(&self.tool).args(["rm", "-f", &id]).status();
        }
    }
}

#[cfg(test)]
mod tests;
