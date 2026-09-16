//! Runner-owned Docker/OCI execution.
//!
//! This is an Adapter, not a Capsule kind. The workload can describe an
//! immutable image and ordinary container arguments, but it never receives a
//! Docker socket, daemon credential, or arbitrary Docker CLI access.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciEndpoint {
    pub host_port: u16,
    pub guest_port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciResourceLimits {
    pub memory_bytes: u64,
    pub cpu_limit_millis: u64,
    pub pids_limit: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciSpec {
    pub id: String,
    pub image: String,
    pub platform: String,
    pub argv: Vec<String>,
    pub working_dir: String,
    pub environment: BTreeMap<String, String>,
    pub endpoints: Vec<OciEndpoint>,
    pub limits: OciResourceLimits,
    pub stop_timeout_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciAdmission {
    pub docker_version: String,
    pub image: String,
    pub platform: String,
}

pub struct DockerOciAdapter {
    docker: PathBuf,
    spec: OciSpec,
    offline_archive: Option<Vec<u8>>,
}

impl DockerOciAdapter {
    pub fn new(spec: OciSpec) -> Result<Self> {
        validate_spec(&spec)?;
        let docker = find_on_path("docker").context(
            "OCI runtime admission failed: Docker CLI is not installed or is not on PATH",
        )?;
        Ok(Self {
            docker,
            spec,
            offline_archive: None,
        })
    }

    /// The caller has already verified the archive's registry manifest,
    /// config, layers, platform, and pinned image digest. Docker may load it,
    /// but must not resolve an absent image through the network.
    pub fn new_offline(spec: OciSpec, archive: Vec<u8>, config_reference: String) -> Result<Self> {
        ensure!(
            config_reference
                .strip_prefix("sha256:")
                .is_some_and(|digest| digest.len() == 64
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())),
            "offline OCI config reference is invalid"
        );
        let mut adapter = Self::new(spec)?;
        adapter.offline_archive = Some(archive);
        Ok(adapter)
    }

    /// Admit the route and acquire its immutable image. Pulling is explicit:
    /// callers can report that this materialization is network-dependent.
    pub fn admit(&self) -> Result<OciAdmission> {
        let version = run_checked(
            &self.docker,
            ["version", "--format", "{{.Server.Version}}"],
            "query Docker Engine version",
        )?;
        if self.offline_archive.is_some() {
            // The verified archive supplied the pinned manifest and all blobs.
            // Docker 29's containerd store identifies the loaded image by the
            // manifest digest, not the config digest. Never pull on this path.
            self.inspect_image()?;
        } else if self.inspect_image().is_err() {
            run_checked(
                &self.docker,
                [
                    "pull",
                    "--platform",
                    self.spec.platform.as_str(),
                    self.spec.image.as_str(),
                ],
                "pull immutable OCI image",
            )?;
        }
        if self.offline_archive.is_none() {
            self.inspect_image()?;
        }
        Ok(OciAdmission {
            docker_version: version.trim().to_owned(),
            image: self.spec.image.clone(),
            platform: self.spec.platform.clone(),
        })
    }

    pub fn spawn(&self, workspace: &Path, runtime_root: &Path) -> Result<OciHandle> {
        ensure!(workspace.is_dir(), "OCI workspace does not exist");
        fs::create_dir_all(runtime_root).context("create OCI runtime directory")?;
        if let Some(archive) = &self.offline_archive {
            let path = runtime_root.join("verified-oci-image.tar");
            fs::write(&path, archive).context("write verified OCI archive")?;
            run_checked(
                &self.docker,
                [
                    "image",
                    "load",
                    "--input",
                    path.to_str().context("OCI archive path is not UTF-8")?,
                ],
                "load verified offline OCI image",
            )?;
        }
        self.admit()?;

        let suffix = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let stem = safe_name(&self.spec.id);
        let container_name = format!("ato-{stem}-{}-{suffix}", std::process::id());
        let network_name = format!("{container_name}-net");
        run_checked(
            &self.docker,
            [
                "network",
                "create",
                "--driver",
                "bridge",
                "--internal",
                network_name.as_str(),
            ],
            "create isolated OCI network",
        )?;

        let env_file = runtime_root.join("environment.list");
        let environment = self
            .spec
            .environment
            .iter()
            .map(|(name, value)| format!("{name}={value}\n"))
            .collect::<String>();
        fs::write(&env_file, environment).context("write OCI environment file")?;

        let argv = docker_run_arguments(
            &self.spec,
            workspace,
            &env_file,
            &container_name,
            &network_name,
        )?;
        let launched = Command::new(&self.docker)
            .args(&argv)
            .output()
            .context("start OCI container")?;
        if !launched.status.success() {
            let _ = remove_network(&self.docker, &network_name);
            bail!("start OCI container failed: {}", bounded_stderr(&launched));
        }
        let container_id = String::from_utf8(launched.stdout)
            .context("Docker returned a non-UTF-8 container id")?
            .trim()
            .to_owned();
        ensure!(
            !container_id.is_empty(),
            "Docker returned an empty container id"
        );
        let container_address = match inspect_container_address(&self.docker, &container_id) {
            Ok(address) => address,
            Err(error) => {
                let _ = Command::new(&self.docker)
                    .args(["rm", "--force", &container_id])
                    .output();
                let _ = remove_network(&self.docker, &network_name);
                return Err(error);
            }
        };
        let mut forwarders = Vec::with_capacity(self.spec.endpoints.len());
        for endpoint in &self.spec.endpoints {
            match PortForwarder::start(endpoint.host_port, container_address, endpoint.guest_port) {
                Ok(forwarder) => forwarders.push(forwarder),
                Err(error) => {
                    drop(forwarders);
                    let _ = Command::new(&self.docker)
                        .args(["rm", "--force", &container_id])
                        .output();
                    let _ = remove_network(&self.docker, &network_name);
                    return Err(error).context("start OCI loopback Port forwarder");
                }
            }
        }
        Ok(OciHandle {
            docker: self.docker.clone(),
            container_id,
            container_name,
            network_name,
            image: self.spec.image.clone(),
            platform: self.spec.platform.clone(),
            forwarders,
            stopped: false,
        })
    }

    fn inspect_image(&self) -> Result<()> {
        let output = Command::new(&self.docker)
            .args([
                "image",
                "inspect",
                "--format",
                "{{json .RepoDigests}}|{{.Os}}/{{.Architecture}}",
                &self.spec.image,
            ])
            .output()
            .context("inspect OCI image")?;
        ensure!(
            output.status.success(),
            "OCI image is not available: {}",
            bounded_stderr(&output)
        );
        let inspected =
            String::from_utf8(output.stdout).context("invalid Docker inspect output")?;
        validate_inspected_image(&self.spec, inspected.trim())
    }
}

fn validate_inspected_image(spec: &OciSpec, inspected: &str) -> Result<()> {
    let (digests, platform) = inspected
        .rsplit_once('|')
        .context("Docker inspect omitted image platform")?;
    let expected_digest = spec
        .image
        .rsplit_once('@')
        .map(|(_, digest)| digest)
        .context("validated image omitted digest")?;
    ensure!(
        digests.contains(expected_digest),
        "local OCI image does not carry declared digest {expected_digest}"
    );
    ensure!(
        platform == spec.platform,
        "OCI platform mismatch: route requires {}, image is {platform}",
        spec.platform
    );
    Ok(())
}

pub struct OciHandle {
    docker: PathBuf,
    container_id: String,
    container_name: String,
    network_name: String,
    image: String,
    platform: String,
    forwarders: Vec<PortForwarder>,
    stopped: bool,
}

impl OciHandle {
    pub fn container_id(&self) -> &str {
        &self.container_id
    }

    pub fn container_name(&self) -> &str {
        &self.container_name
    }

    pub fn image(&self) -> &str {
        &self.image
    }

    pub fn platform(&self) -> &str {
        &self.platform
    }

    pub fn exit_code(&self) -> Result<Option<i32>> {
        let output = Command::new(&self.docker)
            .args([
                "inspect",
                "--format",
                "{{.State.Running}}|{{.State.ExitCode}}",
                &self.container_id,
            ])
            .output()
            .context("inspect OCI container state")?;
        ensure!(
            output.status.success(),
            "inspect OCI container state failed: {}",
            bounded_stderr(&output)
        );
        let value = String::from_utf8(output.stdout).context("invalid container state output")?;
        let (running, code) = value
            .trim()
            .split_once('|')
            .context("Docker inspect omitted container state")?;
        if running == "true" {
            return Ok(None);
        }
        Ok(Some(code.parse().context("invalid OCI exit code")?))
    }

    pub fn stop(mut self) -> Result<()> {
        self.cleanup()
    }

    fn cleanup(&mut self) -> Result<()> {
        if self.stopped {
            return Ok(());
        }
        self.forwarders.clear();
        let container = Command::new(&self.docker)
            .args(["rm", "--force", &self.container_id])
            .output()
            .context("remove OCI container")?;
        let network = remove_network(&self.docker, &self.network_name);
        self.stopped = true;
        ensure!(
            container.status.success(),
            "remove OCI container failed: {}",
            bounded_stderr(&container)
        );
        network
    }
}

struct PortForwarder {
    stop: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl PortForwarder {
    fn start(host_port: u16, container_address: IpAddr, guest_port: u16) -> Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, host_port))
            .with_context(|| format!("bind loopback Port {host_port}"))?;
        listener
            .set_nonblocking(true)
            .context("make OCI Port forwarder non-blocking")?;
        let target = SocketAddr::new(container_address, guest_port);
        let (stop, stopped) = mpsc::channel();
        let thread = thread::Builder::new()
            .name(format!("ato-oci-port-{host_port}"))
            .spawn(move || run_port_forwarder(listener, target, stopped))
            .context("spawn OCI Port forwarder")?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for PortForwarder {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_port_forwarder(listener: TcpListener, target: SocketAddr, stopped: Receiver<()>) {
    loop {
        match stopped.try_recv() {
            Ok(()) | Err(TryRecvError::Disconnected) => return,
            Err(TryRecvError::Empty) => {}
        }
        match listener.accept() {
            Ok((client, _)) => {
                let _ = thread::Builder::new()
                    .name("ato-oci-port-connection".to_owned())
                    .spawn(move || forward_connection(client, target));
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return,
        }
    }
}

fn forward_connection(client: TcpStream, target: SocketAddr) {
    let Ok(upstream) = TcpStream::connect_timeout(&target, Duration::from_secs(2)) else {
        return;
    };
    let Ok(mut client_read) = client.try_clone() else {
        return;
    };
    let Ok(mut upstream_write) = upstream.try_clone() else {
        return;
    };
    let upload = thread::spawn(move || io::copy(&mut client_read, &mut upstream_write));
    let mut upstream_read = upstream;
    let mut client_write = client;
    let _ = io::copy(&mut upstream_read, &mut client_write);
    let _ = upload.join();
}

fn inspect_container_address(docker: &Path, container_id: &str) -> Result<IpAddr> {
    let value = run_checked(
        docker,
        [
            "inspect",
            "--format",
            "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}",
            container_id,
        ],
        "inspect OCI container network address",
    )?;
    value
        .trim()
        .parse()
        .context("Docker returned an invalid container network address")
}

impl Drop for OciHandle {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

fn validate_spec(spec: &OciSpec) -> Result<()> {
    ensure!(!spec.id.trim().is_empty(), "OCI id is empty");
    let (repository, digest) = spec
        .image
        .rsplit_once("@sha256:")
        .context("OCI image must be repository@sha256:<digest>")?;
    ensure!(!repository.is_empty(), "OCI image repository is empty");
    ensure!(
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "OCI image digest is invalid"
    );
    ensure!(
        matches!(spec.platform.as_str(), "linux/amd64" | "linux/arm64"),
        "OCI platform is unsupported"
    );
    ensure!(
        !spec.argv.iter().any(|value| value.contains('\0')),
        "OCI argv contains NUL"
    );
    ensure!(
        spec.working_dir == "/app",
        "OCI working directory must be /app"
    );
    ensure!(
        spec.limits.memory_bytes > 0
            && spec.limits.cpu_limit_millis > 0
            && spec.limits.pids_limit > 0,
        "OCI resource limits must be positive"
    );
    for (name, value) in &spec.environment {
        ensure!(
            !name.is_empty() && !name.contains('=') && !value.contains(['\0', '\n', '\r']),
            "OCI environment is invalid"
        );
    }
    Ok(())
}

fn docker_run_arguments(
    spec: &OciSpec,
    workspace: &Path,
    env_file: &Path,
    container_name: &str,
    network_name: &str,
) -> Result<Vec<String>> {
    let workspace = workspace
        .canonicalize()
        .context("canonicalize OCI workspace")?;
    let env_file = env_file
        .canonicalize()
        .context("canonicalize OCI environment file")?;
    let mut argv = vec![
        "run".to_owned(),
        "--detach".to_owned(),
        "--name".to_owned(),
        container_name.to_owned(),
        "--platform".to_owned(),
        spec.platform.clone(),
        "--network".to_owned(),
        network_name.to_owned(),
        "--read-only".to_owned(),
        "--cap-drop=ALL".to_owned(),
        "--security-opt=no-new-privileges".to_owned(),
        "--pids-limit".to_owned(),
        spec.limits.pids_limit.to_string(),
        "--memory".to_owned(),
        spec.limits.memory_bytes.to_string(),
        "--cpus".to_owned(),
        format!("{:.3}", spec.limits.cpu_limit_millis as f64 / 1000.0),
        "--stop-timeout".to_owned(),
        spec.stop_timeout_seconds.to_string(),
        "--tmpfs".to_owned(),
        "/tmp:rw,noexec,nosuid,size=67108864".to_owned(),
        "--mount".to_owned(),
        format!("type=bind,src={},dst=/app,readonly", workspace.display()),
        "--workdir".to_owned(),
        spec.working_dir.clone(),
        "--env-file".to_owned(),
        env_file.display().to_string(),
        "--label".to_owned(),
        format!("run.ato.dev/id={}", spec.id),
    ];
    argv.push(spec.image.clone());
    argv.extend(spec.argv.iter().cloned());
    Ok(argv)
}

fn remove_network(docker: &Path, network: &str) -> Result<()> {
    let output = Command::new(docker)
        .args(["network", "rm", network])
        .output()
        .context("remove isolated OCI network")?;
    ensure!(
        output.status.success(),
        "remove isolated OCI network failed: {}",
        bounded_stderr(&output)
    );
    Ok(())
}

fn run_checked<'a>(
    executable: &Path,
    arguments: impl IntoIterator<Item = &'a str>,
    operation: &str,
) -> Result<String> {
    let output = Command::new(executable)
        .args(arguments)
        .output()
        .with_context(|| operation.to_owned())?;
    ensure!(
        output.status.success(),
        "{operation} failed: {}",
        bounded_stderr(&output)
    );
    String::from_utf8(output.stdout).with_context(|| format!("{operation} returned non-UTF-8"))
}

fn bounded_stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr)
        .chars()
        .take(2048)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn safe_name(value: &str) -> String {
    let value = value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '-')
        .take(40)
        .collect::<String>();
    if value.is_empty() {
        "run".to_owned()
    } else {
        value
    }
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|search| {
        std::env::split_paths(&search)
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> OciSpec {
        OciSpec {
            id: "run_1".to_owned(),
            image: format!("docker.io/example/app@sha256:{}", "a".repeat(64)),
            platform: "linux/amd64".to_owned(),
            argv: vec!["--serve".to_owned()],
            working_dir: "/app".to_owned(),
            environment: BTreeMap::new(),
            endpoints: vec![OciEndpoint {
                host_port: 49152,
                guest_port: 8000,
            }],
            limits: OciResourceLimits {
                memory_bytes: 256 * 1024 * 1024,
                cpu_limit_millis: 1000,
                pids_limit: 128,
            },
            stop_timeout_seconds: 5,
        }
    }

    #[test]
    fn mutable_image_is_refused() {
        let mut invalid = spec();
        invalid.image = "docker.io/example/app:latest".to_owned();
        assert!(validate_spec(&invalid).is_err());
    }

    #[test]
    fn inspected_image_must_match_both_digest_and_platform() {
        let valid = spec();
        let digest = valid.image.rsplit_once('@').unwrap().1;
        validate_inspected_image(
            &valid,
            &format!("[\"docker.io/example/app@{digest}\"]|linux/amd64"),
        )
        .unwrap();
        assert!(
            validate_inspected_image(
                &valid,
                &format!("[\"docker.io/example/app@{digest}\"]|linux/arm64"),
            )
            .unwrap_err()
            .to_string()
            .contains("platform mismatch")
        );
        assert!(
            validate_inspected_image(
                &valid,
                &format!(
                    "[\"docker.io/example/app@sha256:{}\"]|linux/amd64",
                    "cd".repeat(32)
                ),
            )
            .unwrap_err()
            .to_string()
            .contains("does not carry declared digest")
        );
    }

    #[test]
    fn run_arguments_enforce_isolation_and_limits() {
        let workspace = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let env_file = runtime.path().join("environment.list");
        fs::write(&env_file, "").unwrap();
        let args = docker_run_arguments(
            &spec(),
            workspace.path(),
            &env_file,
            "ato-test",
            "ato-test-net",
        )
        .unwrap();
        let rendered = args.join(" ");
        for required in [
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--pids-limit 128",
            "--memory 268435456",
            "--network ato-test-net",
            "dst=/app,readonly",
        ] {
            assert!(
                rendered.contains(required),
                "missing {required}: {rendered}"
            );
        }
        assert!(!rendered.contains("docker.sock"));
        assert!(!rendered.contains("--privileged"));
        assert!(!rendered.contains("--publish"));
    }
}
