use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;

use anyhow::{Context, Result};

use capsule_core::router::ManifestData;
use capsule_core::runtime::oci::{
    BollardOciRuntimeClient, OciContainerRequest, OciLogChunk, OciPortSpec, OciRuntimeClient,
};
use capsule_core::CapsuleReporter;

use super::launch_context::RuntimeLaunchContext;
use crate::reporters::CliReporter;

const OCI_STOP_TIMEOUT_SECS: i64 = 5;

pub async fn execute(
    plan: &ManifestData,
    reporter: Arc<CliReporter>,
    launch_ctx: &RuntimeLaunchContext,
) -> Result<i32> {
    let client = BollardOciRuntimeClient::connect_default()
        .context("Failed to connect to OCI engine via Docker-compatible API")?;
    execute_with_client(plan, reporter, launch_ctx, &client).await
}

pub async fn execute_with_client<C: OciRuntimeClient>(
    plan: &ManifestData,
    reporter: Arc<CliReporter>,
    launch_ctx: &RuntimeLaunchContext,
    client: &C,
) -> Result<i32> {
    let image = plan
        .targets_oci_image()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("OCI runtime selected but no image specified"))?;

    let manifest_name = plan
        .manifest_name()
        .unwrap_or_else(|| "capsule".to_string());
    let session_id = session_id(&manifest_name);
    let container_name = format!(
        "ato-{}-{}-main",
        sanitize_name(&manifest_name),
        session_suffix(&session_id)
    );

    let mut labels = base_labels(&session_id, &manifest_name, "main");
    labels.insert(
        "io.ato.target".to_string(),
        plan.selected_target_label().to_string(),
    );

    let ports = plan
        .execution_port()
        .map(|port| {
            vec![OciPortSpec {
                container_port: port,
                host_port: Some(port),
                protocol: "tcp".to_string(),
                host_ip: Some("127.0.0.1".to_string()),
            }]
        })
        .unwrap_or_default();

    let mut env = plan.targets_oci_env();
    env.extend(launch_ctx.merged_env());
    let mut cmd = plan.targets_oci_cmd();
    if cmd.is_empty() {
        if let Some(entrypoint) = plan
            .execution_entrypoint()
            .or_else(|| plan.execution_run_command())
        {
            cmd = shell_words::split(&entrypoint).unwrap_or_else(|_| vec![entrypoint]);
        }
    }

    client.pull_image(&image).await?;
    let container_id = client
        .create_container(&OciContainerRequest {
            name: container_name,
            image,
            cmd,
            env,
            working_dir: plan.targets_oci_working_dir(),
            labels,
            mounts: launch_ctx
                .injected_mounts()
                .iter()
                .map(|mount| capsule_core::runtime::oci::OciMountSpec {
                    source: mount.source.to_string_lossy().to_string(),
                    target: mount.target.clone(),
                    readonly: mount.readonly,
                })
                .collect(),
            ports,
            network: None,
            aliases: Vec::new(),
        })
        .await?;
    client.start_container(&container_id).await?;

    if let Some(port) = plan.execution_port() {
        reporter
            .notify(format!(
                "🌐 OCI target '{}' is available at http://127.0.0.1:{}/",
                plan.selected_target_label(),
                port
            ))
            .await?;
    }

    let mut logs = client.logs(&container_id, true).await?;
    let log_task = tokio::spawn(async move {
        while let Some(chunk) = logs.recv().await {
            match chunk {
                Ok(chunk) => {
                    let _ = print_log_chunk("main", &chunk);
                }
                Err(err) => {
                    let _ = writeln!(std::io::stderr(), "[main] log error: {}", err);
                    break;
                }
            }
        }
    });

    let exit_code = tokio::select! {
        result = client.wait_container(&container_id) => result?,
        _ = tokio::signal::ctrl_c() => {
            let _ = client.stop_container(&container_id, OCI_STOP_TIMEOUT_SECS).await;
            130
        }
    };

    let _ = client.remove_container(&container_id, true).await;
    let _ = log_task.await;

    Ok(exit_code as i32)
}

fn print_log_chunk(service_name: &str, chunk: &OciLogChunk) -> Result<()> {
    let prefix = format!("[{}] ", service_name);
    if chunk.stderr {
        let mut writer = std::io::stderr();
        writer.write_all(prefix.as_bytes())?;
        writer.write_all(&chunk.message)?;
        writer.flush()?;
    } else {
        let mut writer = std::io::stdout();
        writer.write_all(prefix.as_bytes())?;
        writer.write_all(&chunk.message)?;
        writer.flush()?;
    }
    Ok(())
}

fn base_labels(
    session_id: &str,
    manifest_name: &str,
    service_name: &str,
) -> HashMap<String, String> {
    HashMap::from([
        ("io.ato.session".to_string(), session_id.to_string()),
        ("io.ato.service".to_string(), service_name.to_string()),
        ("io.ato.manifest".to_string(), manifest_name.to_string()),
    ])
}

fn session_id(manifest_name: &str) -> String {
    format!(
        "{}-{}-{}",
        sanitize_name(manifest_name),
        session_suffix(manifest_name),
        std::process::id()
    )
}

fn session_suffix(value: &str) -> String {
    let hash = blake3::hash(value.as_bytes()).to_hex().to_string();
    hash.chars().take(8).collect()
}

fn sanitize_name(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    sanitized.trim_matches('-').to_string()
}
