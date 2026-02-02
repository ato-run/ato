use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use crate::config::{RuntimeConfig, ServiceConfig};
use crate::lockfile;
use crate::system::NetworkSandbox;

pub async fn run_services_from_config(
    config: &RuntimeConfig,
    bundle_root: &Path,
    sandbox: Option<&dyn NetworkSandbox>,
) -> Result<(), String> {
    lockfile::enforce_lockfile_allowlist(bundle_root)
        .map_err(|e| format!("capsule.lock allowlist check failed: {}", e))?;
    lockfile::hydrate_bundle(bundle_root)
        .await
        .map_err(|e| format!("capsule.lock hydration failed: {}", e))?;
    if config.services.is_empty() {
        return Err("config.json has no services".to_string());
    }
    if !config.services.contains_key("main") {
        return Err("config.json requires services.main".to_string());
    }

    let order = resolve_dependencies(&config.services)?;
    let mut children: HashMap<String, Child> = HashMap::new();

    for name in &order {
        let svc = config
            .services
            .get(name)
            .ok_or_else(|| format!("Service '{}' missing from config", name))?;

        let mut cmd = build_command(bundle_root, svc)?;
        if let Some(sandbox) = sandbox {
            sandbox
                .apply_to_child(&mut cmd)
                .map_err(|e| format!("Failed to apply sandbox: {e}"))?;
        }
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::inherit());
        cmd.stderr(Stdio::inherit());

        let child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn service '{}': {}", name, e))?;
        children.insert(name.clone(), child);
    }

    supervise_children(children).await
}

async fn supervise_children(mut children: HashMap<String, Child>) -> Result<(), String> {
    loop {
        let mut exited: Option<(String, std::process::ExitStatus)> = None;
        let mut wait_error: Option<(String, String)> = None;

        for (name, child) in children.iter_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    exited = Some((name.clone(), status));
                    break;
                }
                Ok(None) => {}
                Err(e) => {
                    wait_error = Some((name.clone(), e.to_string()));
                    break;
                }
            }
        }

        if let Some((name, err)) = wait_error {
            terminate_all(&mut children, &name);
            return Err(format!("Service '{}' wait failed: {}", name, err));
        }

        if let Some((name, status)) = exited {
            let is_main = name == "main";
            terminate_all(&mut children, &name);

            if is_main {
                if !status.success() {
                    return Err(format!("services.main exited with status {}", status));
                }
                return Ok(());
            }

            return Err(format!("Service '{}' exited (fail-fast): {}", name, status));
        }

        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn terminate_all(children: &mut HashMap<String, Child>, exclude: &str) {
    for (name, child) in children.iter_mut() {
        if name == exclude {
            continue;
        }
        let _ = child.kill();
    }
}

fn build_command(bundle_root: &Path, svc: &ServiceConfig) -> Result<Command, String> {
    let cwd = svc.cwd.as_deref().unwrap_or("source");
    let cwd_path = resolve_path(bundle_root, cwd);

    let executable = resolve_path_with_cwd(bundle_root, cwd_path.clone(), &svc.executable);
    let mut cmd = Command::new(&executable);

    let args: Vec<String> = svc
        .args
        .iter()
        .map(|a| resolve_arg(bundle_root, a))
        .collect();
    cmd.args(&args);

    cmd.current_dir(cwd_path);

    if let Some(envs) = &svc.env {
        for (key, value) in envs {
            cmd.env(key, resolve_env_value(bundle_root, value));
        }
    }
    if let Some(ports) = &svc.ports {
        for (k, v) in ports {
            cmd.env(k, v.to_string());
        }
    }

    Ok(cmd)
}

fn resolve_path_with_cwd(bundle_root: &Path, cwd: PathBuf, path: &str) -> PathBuf {
    let trimmed = path.trim();

    if trimmed.starts_with('/') {
        return PathBuf::from(trimmed);
    }

    if trimmed.starts_with("source/") || trimmed.starts_with("runtime/") {
        return bundle_root.join(trimmed);
    }

    if trimmed.starts_with("./") {
        return cwd.join(trimmed.trim_start_matches("./"));
    }

    if !trimmed.contains('/') {
        let with_cwd = cwd.join(trimmed);
        if with_cwd.exists() {
            return with_cwd;
        }
        if let Ok(found) = which::which(trimmed) {
            return found;
        }
    }

    bundle_root.join(trimmed)
}

fn resolve_path(bundle_root: &Path, path: &str) -> PathBuf {
    let trimmed = path.trim();

    // If the config provides an absolute path, trust it.
    if trimmed.starts_with('/') {
        return PathBuf::from(trimmed);
    }

    // If the config provides a bare executable name (e.g. "bash"), resolve it from PATH
    // so that host-provided runtimes work in bundle mode.
    if !trimmed.contains('/') {
        if let Ok(found) = which::which(trimmed) {
            return found;
        }
    }

    // Otherwise, treat it as a path relative to the extracted bundle root.
    bundle_root.join(trimmed)
}

fn resolve_arg(bundle_root: &Path, arg: &str) -> String {
    let trimmed = arg.trim();
    if trimmed.starts_with("source/") || trimmed.starts_with("runtime/") {
        return resolve_path(bundle_root, trimmed)
            .to_string_lossy()
            .to_string();
    }
    if trimmed.starts_with('/') {
        return trimmed.to_string();
    }
    trimmed.to_string()
}

fn resolve_env_value(bundle_root: &Path, value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.starts_with("source/") || trimmed.starts_with("runtime/") {
        return resolve_path(bundle_root, trimmed)
            .to_string_lossy()
            .to_string();
    }
    trimmed.to_string()
}

fn resolve_dependencies(services: &HashMap<String, ServiceConfig>) -> Result<Vec<String>, String> {
    let mut visited: HashSet<String> = HashSet::new();
    let mut visiting: HashSet<String> = HashSet::new();
    let mut sorted: Vec<String> = Vec::new();

    fn visit(
        name: &str,
        services: &HashMap<String, ServiceConfig>,
        visited: &mut HashSet<String>,
        visiting: &mut HashSet<String>,
        sorted: &mut Vec<String>,
        stack: &mut Vec<String>,
    ) -> Result<(), String> {
        if visited.contains(name) {
            return Ok(());
        }
        if visiting.contains(name) {
            stack.push(name.to_string());
            return Err(format!(
                "Circular dependency detected: {}",
                stack.join(" -> ")
            ));
        }

        let spec = services
            .get(name)
            .ok_or_else(|| format!("Unknown service '{}' (referenced by depends_on)", name))?;

        visiting.insert(name.to_string());
        stack.push(name.to_string());

        if let Some(deps) = &spec.depends_on {
            for dep in deps {
                if !services.contains_key(dep) {
                    return Err(format!(
                        "Service '{}' depends on unknown service '{}'",
                        name, dep
                    ));
                }
                visit(dep, services, visited, visiting, sorted, stack)?;
            }
        }

        stack.pop();
        visiting.remove(name);
        visited.insert(name.to_string());
        sorted.push(name.to_string());
        Ok(())
    }

    let mut names: Vec<&String> = services.keys().collect();
    names.sort();
    for name in names {
        let mut stack = Vec::new();
        visit(
            name,
            services,
            &mut visited,
            &mut visiting,
            &mut sorted,
            &mut stack,
        )?;
    }

    Ok(sorted)
}
