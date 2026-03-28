use anyhow::{Context, Result};
use bollard::container::InspectContainerOptions;
use bollard::{Docker, API_DEFAULT_VERSION};
use rand::Rng;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tracing::warn;

use crate::reporter::NoOpReporter;
use crate::runtime::oci::OciHandle;
use crate::{SessionRunner, SessionRunnerConfig};

use crate::hardware;
use crate::router::ManifestData;

#[derive(Debug, Clone)]
enum OciEngine {
    Docker,
    Podman,
}

pub fn execute(plan: &ManifestData) -> Result<i32> {
    let engine = detect_engine()?;
    let image = resolve_image(plan)?;
    let mut env = plan.execution_env();
    env.extend(plan.targets_oci_env());

    let mut cmd = Command::new(engine_binary(&engine));
    cmd.arg("run").arg("--rm");

    let name = format!("capsule-{}", rand::thread_rng().gen::<u32>());
    cmd.arg("--name").arg(&name);

    if let Some(port) = plan.execution_port() {
        cmd.arg("-p").arg(format!("{port}:{port}"));
    }

    if let Some(workdir) = plan
        .targets_oci_working_dir()
        .or_else(|| plan.execution_working_dir())
    {
        cmd.arg("-w").arg(workdir);
    }

    if let Some(raw_manifest) = plan
        .compat_manifest()
        .and_then(|bridge| bridge.raw_value().ok())
        .filter(|manifest| hardware::requires_gpu(manifest))
    {
        let _ = raw_manifest;
        if let Some(report) = hardware::detect_nvidia_gpus()? {
            if report.count > 0 {
                cmd.arg("--gpus").arg("all");
            } else {
                warn!("GPU requested but none detected; continuing without --gpus");
            }
        } else {
            warn!("GPU requested but nvidia-smi unavailable; continuing without --gpus");
        }
    }

    for (k, v) in env {
        cmd.arg("--env").arg(format!("{}={}", k, v));
    }

    let image_for_metrics = image.clone();
    cmd.arg(&image);

    let mut args = plan.targets_oci_cmd();
    if args.is_empty() {
        if let Some(entrypoint) = plan.execution_entrypoint() {
            if let Ok(parsed) = shell_words::split(&entrypoint) {
                args = parsed;
            }
        }
    }

    if !args.is_empty() {
        cmd.args(args);
    }

    let mut child = cmd
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("Failed to run OCI engine: {}", engine_binary(&engine)))?;

    let runtime = tokio::runtime::Runtime::new()?;
    let metrics_result = runtime.block_on(run_oci_with_metrics(name, image_for_metrics));

    let status = child
        .wait()
        .with_context(|| format!("Failed waiting for OCI engine: {}", engine_binary(&engine)))?;

    metrics_result?;

    Ok(status.code().unwrap_or(1))
}

async fn run_oci_with_metrics(container_id: String, image_hash: String) -> Result<()> {
    let docker = connect_docker().context("Failed to connect to Docker daemon")?;

    wait_for_container(&docker, &container_id, Duration::from_secs(10)).await?;

    let session_id = format!("oci-{}", rand::thread_rng().gen::<u64>());
    let handle = OciHandle::new(session_id, container_id.clone(), image_hash, docker.clone());
    let reporter = NoOpReporter;
    let config = SessionRunnerConfig {
        sample_interval: Duration::from_secs(5),
        timeout: Some(Duration::from_secs(10)),
        finalize_timeout: Duration::from_secs(5),
    };

    let _metrics = SessionRunner::new(handle, reporter)
        .with_config(config)
        .run()
        .await?;
    Ok(())
}

async fn wait_for_container(docker: &Docker, container_id: &str, timeout: Duration) -> Result<()> {
    let start = Instant::now();
    loop {
        match docker
            .inspect_container(container_id, None::<InspectContainerOptions>)
            .await
        {
            Ok(_) => return Ok(()),
            Err(err) => {
                if start.elapsed() >= timeout {
                    return Err(anyhow::anyhow!(
                        "Container {} not found within {:?}: {}",
                        container_id,
                        timeout,
                        err
                    ));
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

fn connect_docker() -> Result<Docker> {
    if let Some(host) = resolve_docker_host() {
        if let Some(path) = host.strip_prefix("unix://") {
            return Docker::connect_with_local(path, 120, API_DEFAULT_VERSION)
                .context("Failed to connect via unix socket");
        }

        if let Some(path) = host.strip_prefix("npipe://") {
            return Docker::connect_with_local(path, 120, API_DEFAULT_VERSION)
                .context("Failed to connect via named pipe");
        }

        if let Some(addr) = host.strip_prefix("tcp://") {
            let http = format!("http://{}", addr);
            return Docker::connect_with_http(&http, 120, API_DEFAULT_VERSION)
                .context("Failed to connect via http");
        }

        if host.starts_with("http://") {
            return Docker::connect_with_http(&host, 120, API_DEFAULT_VERSION)
                .context("Failed to connect via http");
        }
    }

    Docker::connect_with_local_defaults().context("Failed to connect with local defaults")
}

fn resolve_docker_host() -> Option<String> {
    if let Ok(host) = std::env::var("DOCKER_HOST") {
        let trimmed = host.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    let output = Command::new("docker")
        .args([
            "context",
            "inspect",
            "--format",
            "{{.Endpoints.docker.Host}}",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let host = stdout.trim();
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

fn detect_engine() -> Result<OciEngine> {
    if which::which("docker").is_ok() {
        return Ok(OciEngine::Docker);
    }
    if which::which("podman").is_ok() {
        return Ok(OciEngine::Podman);
    }
    anyhow::bail!("No OCI engine found (docker/podman)");
}

fn engine_binary(engine: &OciEngine) -> &'static str {
    match engine {
        OciEngine::Docker => "docker",
        OciEngine::Podman => "podman",
    }
}

fn resolve_image(plan: &ManifestData) -> Result<String> {
    if let Some(image) = plan.targets_oci_image() {
        return Ok(image);
    }
    if let Some(image) = plan.execution_image() {
        return Ok(image);
    }

    if let Some(runtime) = plan.execution_runtime() {
        if runtime.eq_ignore_ascii_case("oci") || runtime.eq_ignore_ascii_case("docker") {
            if let Some(entrypoint) = plan.execution_entrypoint() {
                return Ok(entrypoint);
            }
        }
    }

    anyhow::bail!("OCI runtime selected but no image specified")
}
