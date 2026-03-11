use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
#[cfg(unix)]
use std::{
    collections::BTreeMap,
    io::Write,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::process::CommandExt,
    },
};

use anyhow::{Context, Result};

use capsule_core::execution_plan::canonical::{
    compute_policy_segment_hash, compute_provisioning_policy_hash,
};
use capsule_core::execution_plan::error::AtoExecutionError;
use capsule_core::execution_plan::model::{ExecutionPlan, ExecutionRuntime};
use capsule_core::router::ManifestData;

use crate::common::proxy;
use crate::runtime_manager;
use crate::runtime_overrides;

use super::launch_context::RuntimeLaunchContext;

struct PreparedCommand {
    cmd: Command,
    #[cfg(unix)]
    _secret_fd_guard: Option<std::fs::File>,
}

pub fn execute(
    plan: &ManifestData,
    execution_plan: &ExecutionPlan,
    launch_ctx: &RuntimeLaunchContext,
    dangerously_skip_permissions: bool,
) -> Result<i32> {
    let deno_bin = runtime_manager::ensure_deno_binary(plan)?;

    verify_execution_plan_hashes(execution_plan)?;

    let entrypoint = plan
        .execution_entrypoint()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| {
            AtoExecutionError::policy_violation("source/node target requires entrypoint")
        })?;

    let runtime_dir = resolve_runtime_dir(&plan.manifest_dir, &entrypoint);
    let package_lock = resolve_package_lock_path(&plan.manifest_dir, &runtime_dir);
    let Some(_) = package_lock else {
        return Err(AtoExecutionError::lock_incomplete(
            "package-lock.json is required for source/node Tier1 execution",
            Some("package-lock.json"),
        )
        .into());
    };

    let use_compat_flag = deno_supports_compat_flag(&deno_bin)?;

    run_provisioning(&deno_bin, &runtime_dir, &entrypoint, launch_ctx)?;
    let PreparedCommand {
        mut cmd,
        #[cfg(unix)]
        _secret_fd_guard,
    } = build_runtime_command(
        &deno_bin,
        plan,
        execution_plan,
        &runtime_dir,
        &entrypoint,
        launch_ctx,
        use_compat_flag,
        dangerously_skip_permissions,
    )?;
    let status = cmd
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("Failed to execute deno run for node compat")?;

    Ok(status.code().unwrap_or(1))
}

pub fn spawn(
    plan: &ManifestData,
    execution_plan: &ExecutionPlan,
    launch_ctx: &RuntimeLaunchContext,
    dangerously_skip_permissions: bool,
) -> Result<Child> {
    let deno_bin = runtime_manager::ensure_deno_binary(plan)?;

    verify_execution_plan_hashes(execution_plan)?;

    let entrypoint = plan
        .execution_entrypoint()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| {
            AtoExecutionError::policy_violation("source/node target requires entrypoint")
        })?;

    let runtime_dir = resolve_runtime_dir(&plan.manifest_dir, &entrypoint);
    let package_lock = resolve_package_lock_path(&plan.manifest_dir, &runtime_dir);
    let Some(_) = package_lock else {
        return Err(AtoExecutionError::lock_incomplete(
            "package-lock.json is required for source/node Tier1 execution",
            Some("package-lock.json"),
        )
        .into());
    };

    let use_compat_flag = deno_supports_compat_flag(&deno_bin)?;

    run_provisioning(&deno_bin, &runtime_dir, &entrypoint, launch_ctx)?;
    let PreparedCommand {
        mut cmd,
        #[cfg(unix)]
        _secret_fd_guard,
    } = build_runtime_command(
        &deno_bin,
        plan,
        execution_plan,
        &runtime_dir,
        &entrypoint,
        launch_ctx,
        use_compat_flag,
        dangerously_skip_permissions,
    )?;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.spawn()
        .context("Failed to spawn node compat runtime for orchestration")
}

fn run_provisioning(
    deno_bin: &Path,
    runtime_dir: &Path,
    entrypoint: &str,
    launch_ctx: &RuntimeLaunchContext,
) -> Result<()> {
    let mut cmd = Command::new(deno_bin);
    cmd.current_dir(runtime_dir)
        .arg("cache")
        .arg("--node-modules-dir")
        .arg(entrypoint)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    launch_ctx.apply_allowlisted_env(&mut cmd)?;
    if let Some(proxy_env) = proxy::proxy_env_from_env(&[])? {
        proxy::apply_proxy_env(&mut cmd, &proxy_env);
    }

    let status = cmd
        .status()
        .context("Failed to execute deno cache for node compat")?;
    if status.success() {
        Ok(())
    } else {
        Err(AtoExecutionError::lock_incomplete(
            format!(
                "deno cache for source/node Tier1 failed with exit code {}",
                status.code().unwrap_or(1)
            ),
            Some("package-lock.json"),
        )
        .into())
    }
}

#[allow(clippy::too_many_arguments)]
fn build_runtime_command(
    deno_bin: &Path,
    plan: &ManifestData,
    execution_plan: &ExecutionPlan,
    runtime_dir: &Path,
    entrypoint: &str,
    launch_ctx: &RuntimeLaunchContext,
    use_compat_flag: bool,
    dangerously_skip_permissions: bool,
) -> Result<PreparedCommand> {
    let mut cmd = Command::new(deno_bin);
    cmd.current_dir(runtime_dir)
        .arg("run")
        .arg("--node-modules-dir")
        .arg("--no-prompt");
    if !dangerously_skip_permissions {
        cmd.arg("--cached-only");
    }
    if use_compat_flag {
        cmd.arg("--compat");
    }

    let runtime_dir_allow = runtime_dir.to_string_lossy().to_string();

    if dangerously_skip_permissions {
        cmd.arg("-A");
    } else {
        if !execution_plan.runtime.policy.network.allow_hosts.is_empty() {
            cmd.arg(format!(
                "--allow-net={}",
                execution_plan.runtime.policy.network.allow_hosts.join(",")
            ));
        }

        let mut allow_read = execution_plan.runtime.policy.filesystem.read_only.clone();
        allow_read.extend(execution_plan.runtime.policy.filesystem.read_write.clone());
        if !allow_read.iter().any(|path| path == &runtime_dir_allow) {
            allow_read.push(runtime_dir_allow.clone());
        }
        if !allow_read.is_empty() {
            cmd.arg(format!("--allow-read={}", allow_read.join(",")));
        }

        let mut allow_write = execution_plan.runtime.policy.filesystem.read_write.clone();
        if execution_plan.target.runtime == ExecutionRuntime::Web
            && !allow_write.iter().any(|path| path == &runtime_dir_allow)
        {
            allow_write.push(runtime_dir_allow.clone());
        }
        if !allow_write.is_empty() {
            cmd.arg(format!("--allow-write={}", allow_write.join(",")));
        }
    }

    for (key, value) in runtime_overrides::merged_env(plan.execution_env()) {
        cmd.env(key, value);
    }
    if execution_plan.target.runtime == ExecutionRuntime::Web {
        if let Some(port) = runtime_overrides::override_port(plan.execution_port()) {
            cmd.env("PORT", port.to_string());
        }
        if !dangerously_skip_permissions {
            cmd.arg("--allow-env");
            cmd.arg("--allow-sys");
            cmd.arg(format!("--allow-ffi={runtime_dir_allow}"));
        }
    }

    #[cfg(unix)]
    let mut secret_fd_guard: Option<std::fs::File> = None;

    #[cfg(unix)]
    {
        let secrets = collect_runtime_secrets(execution_plan);
        if !secrets.is_empty() {
            secret_fd_guard = Some(inject_secrets_via_fd3(&mut cmd, &secrets)?);
        }
    }

    append_allow_env_permission(&mut cmd, plan, launch_ctx);
    launch_ctx.apply_allowlisted_env(&mut cmd)?;

    if let Some(proxy_env) = proxy::proxy_env_from_env(&[])? {
        proxy::apply_proxy_env(&mut cmd, &proxy_env);
    }

    cmd.arg(entrypoint);
    let args = plan.targets_oci_cmd();
    if !args.is_empty() {
        cmd.args(args);
    }

    Ok(PreparedCommand {
        cmd,
        #[cfg(unix)]
        _secret_fd_guard: secret_fd_guard,
    })
}

fn deno_supports_compat_flag(deno_bin: &Path) -> Result<bool> {
    let output = Command::new(deno_bin)
        .arg("run")
        .arg("--help")
        .stdin(Stdio::null())
        .output()
        .context("Failed to inspect deno run --help for compat support")?;
    if !output.status.success() {
        return Err(AtoExecutionError::policy_violation(
            "unable to detect deno runtime capabilities for node compat execution",
        )
        .into());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.contains("--compat"))
}

fn resolve_runtime_dir(manifest_dir: &Path, entrypoint: &str) -> PathBuf {
    let source_dir = manifest_dir.join("source");
    if source_dir.is_dir() && source_dir.join(entrypoint).exists() {
        return source_dir;
    }
    manifest_dir.to_path_buf()
}

fn resolve_package_lock_path(manifest_dir: &Path, runtime_dir: &Path) -> Option<PathBuf> {
    let candidates = [
        runtime_dir.join("package-lock.json"),
        manifest_dir.join("package-lock.json"),
        manifest_dir.join("source").join("package-lock.json"),
    ];
    candidates.into_iter().find(|path| path.exists())
}

fn append_allow_env_permission(
    cmd: &mut Command,
    plan: &ManifestData,
    launch_ctx: &RuntimeLaunchContext,
) {
    let has_allow_env = cmd
        .get_args()
        .any(|arg| arg.to_string_lossy().starts_with("--allow-env"));
    if has_allow_env {
        return;
    }

    let mut keys = BTreeSet::new();
    keys.extend(runtime_overrides::merged_env(plan.execution_env()).into_keys());
    keys.extend(launch_ctx.env_permission_keys());
    if keys.is_empty() {
        return;
    }

    cmd.arg(format!(
        "--allow-env={}",
        keys.into_iter().collect::<Vec<_>>().join(",")
    ));
}

#[cfg(test)]
fn map_deno_permission_error(stderr: &[u8]) -> Option<AtoExecutionError> {
    let text = String::from_utf8_lossy(stderr);
    let lower = text.to_ascii_lowercase();

    if !lower.contains("notcapable") && !lower.contains("requires net access") {
        return None;
    }

    let target = extract_deno_net_target(&text);
    let message = if let Some(ref host) = target {
        format!("network policy violation: blocked egress to {}", host)
    } else {
        "network policy violation: blocked egress".to_string()
    };

    Some(AtoExecutionError::new(
        capsule_core::execution_plan::error::AtoErrorCode::AtoErrPolicyViolation,
        message,
        Some("network"),
        target.as_deref(),
        None,
    ))
}

#[cfg(test)]
fn map_node_compat_error(stderr: &[u8]) -> Option<AtoExecutionError> {
    let text = String::from_utf8_lossy(stderr);
    let lower = text.to_ascii_lowercase();

    let unsupported = lower.contains("not implemented")
        || lower.contains("not yet implemented")
        || lower.contains("unsupported")
        || lower.contains("n-api modules are currently not supported");

    if !unsupported {
        return None;
    }

    Some(AtoExecutionError::policy_violation(
        "node compat runtime rejected an unsupported node feature (fail-closed)",
    ))
}

#[cfg(test)]
fn extract_deno_net_target(stderr: &str) -> Option<String> {
    let marker = "Requires net access to \"";
    let start = stderr.find(marker)? + marker.len();
    let tail = &stderr[start..];
    let end = tail.find('"')?;
    let host_port = &tail[..end];
    let host = host_port.split(':').next().unwrap_or(host_port).trim();
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

#[cfg(unix)]
fn collect_runtime_secrets(execution_plan: &ExecutionPlan) -> BTreeMap<String, String> {
    let mut keys = BTreeSet::new();

    for key in &execution_plan.runtime.policy.secrets.allow_secret_ids {
        if !key.trim().is_empty() {
            keys.insert(key.trim().to_string());
        }
    }

    if std::env::var_os("OPENAI_API_KEY").is_some() {
        keys.insert("OPENAI_API_KEY".to_string());
    }

    let mut secrets = BTreeMap::new();
    for key in keys {
        if let Ok(value) = std::env::var(&key) {
            if !value.is_empty() {
                secrets.insert(key, value);
            }
        }
    }

    secrets
}

#[cfg(unix)]
fn inject_secrets_via_fd3(
    cmd: &mut Command,
    secrets: &BTreeMap<String, String>,
) -> Result<std::fs::File> {
    let mut fds = [0; 2];
    let pipe_result = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if pipe_result != 0 {
        return Err(anyhow::anyhow!("failed to create secret pipe"));
    }

    let read_fd = fds[0];
    let write_fd = fds[1];

    let mut writer = unsafe { std::fs::File::from_raw_fd(write_fd) };
    let payload = serde_json::to_vec(secrets)
        .context("failed to serialize secret payload for fd injection")?;
    writer
        .write_all(&payload)
        .context("failed to write secret payload into fd pipe")?;
    drop(writer);

    let reader = unsafe { std::fs::File::from_raw_fd(read_fd) };
    let dup_from_fd = reader.as_raw_fd();

    unsafe {
        cmd.pre_exec(move || {
            if libc::dup2(dup_from_fd, 3) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if dup_from_fd != 3 {
                libc::close(dup_from_fd);
            }
            Ok(())
        });
    }

    cmd.env("ATO_SECRET_FD", "3");
    for key in secrets.keys() {
        cmd.env(format!("ATO_SECRET_FD_{key}"), "3");
        cmd.env_remove(key);
    }

    Ok(reader)
}

fn verify_execution_plan_hashes(execution_plan: &ExecutionPlan) -> Result<()> {
    let expected_policy_hash = compute_policy_segment_hash(
        &execution_plan.runtime,
        &execution_plan.consent.mount_set_algo_id,
        execution_plan.consent.mount_set_algo_version,
    )?;
    if expected_policy_hash != execution_plan.consent.policy_segment_hash {
        return Err(AtoExecutionError::lockfile_tampered(
            "policy_segment_hash mismatch detected before node compat runtime",
            Some("policy_segment_hash"),
        )
        .into());
    }

    let expected_provisioning_hash =
        compute_provisioning_policy_hash(&execution_plan.provisioning)?;
    if expected_provisioning_hash != execution_plan.consent.provisioning_policy_hash {
        return Err(AtoExecutionError::lockfile_tampered(
            "provisioning_policy_hash mismatch detected before node compat runtime",
            Some("provisioning_policy_hash"),
        )
        .into());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        map_deno_permission_error, map_node_compat_error, resolve_package_lock_path,
        resolve_runtime_dir,
    };

    #[test]
    fn runtime_dir_uses_source_when_entrypoint_exists_only_there() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        std::fs::create_dir_all(tmp.path().join("source")).expect("create source dir");
        std::fs::write(
            tmp.path().join("source").join("main.js"),
            "console.log('ok');",
        )
        .expect("write source entrypoint");

        let runtime_dir = resolve_runtime_dir(tmp.path(), "main.js");
        assert_eq!(runtime_dir, tmp.path().join("source"));
    }

    #[test]
    fn package_lock_path_falls_back_to_source_dir() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        std::fs::create_dir_all(tmp.path().join("source")).expect("create source dir");
        std::fs::write(tmp.path().join("source").join("package-lock.json"), "{}")
            .expect("write source package-lock");

        let runtime_dir = resolve_runtime_dir(tmp.path(), "main.js");
        let lock_path = resolve_package_lock_path(tmp.path(), &runtime_dir)
            .expect("must resolve package-lock.json");
        assert_eq!(
            lock_path,
            tmp.path().join("source").join("package-lock.json")
        );
    }

    #[test]
    fn map_permission_error_returns_policy_violation() {
        let stderr = b"error: Uncaught (in promise) PermissionDenied: Requires net access to \"api.example.com:443\"";
        let err = map_deno_permission_error(stderr).expect("must map");
        assert_eq!(err.code, "ATO_ERR_POLICY_VIOLATION");
        assert!(err.message.contains("blocked egress"));
    }

    #[test]
    fn map_node_compat_error_returns_policy_violation() {
        let stderr = b"error: This API is not implemented in Deno";
        let err = map_node_compat_error(stderr).expect("must map");
        assert_eq!(err.code, "ATO_ERR_POLICY_VIOLATION");
    }
}
