use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use capsule_core::lockfile::{
    manifest_external_capsule_dependencies, CapsuleLock, LockedCapsuleDependency,
};
use capsule_core::router::{ExecutionProfile, ManifestData};
use capsule_core::types::{ExternalCapsuleDependency, ReadinessProbe};
use capsule_core::CapsuleReporter;
use sha2::{Digest, Sha256};

use crate::reporters::CliReporter;
use crate::runtime_tree;

const EXTERNAL_READY_TIMEOUT: Duration = Duration::from_secs(30);
const EXTERNAL_READY_INTERVAL: Duration = Duration::from_millis(250);
const EXTERNAL_CAPSULE_CACHE_DIR_ENV: &str = "ATO_EXTERNAL_CAPSULE_CACHE_DIR";

#[derive(Debug, Clone)]
pub struct ExternalCapsuleOptions {
    pub enforcement: String,
    pub sandbox_mode: bool,
    pub dangerously_skip_permissions: bool,
    pub assume_yes: bool,
}

pub struct ExternalCapsuleGuard {
    caller_env: HashMap<String, String>,
    children: Vec<ExternalCapsuleChild>,
}

impl ExternalCapsuleGuard {
    pub fn caller_env(&self) -> &HashMap<String, String> {
        &self.caller_env
    }

    pub fn shutdown_now(&mut self) {
        for child in &mut self.children {
            child.shutdown();
        }
    }
}

impl Drop for ExternalCapsuleGuard {
    fn drop(&mut self) {
        self.shutdown_now();
    }
}

struct ExternalCapsuleChild {
    child: Child,
    stdout_thread: Option<JoinHandle<std::io::Result<()>>>,
    stderr_thread: Option<JoinHandle<std::io::Result<()>>>,
}

impl ExternalCapsuleChild {
    fn shutdown(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if let Some(handle) = self.stdout_thread.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.stderr_thread.take() {
            let _ = handle.join();
        }
    }
}

pub async fn start_external_capsules(
    plan: &ManifestData,
    lockfile: &CapsuleLock,
    cli_inject_bindings: &[String],
    reporter: std::sync::Arc<CliReporter>,
    options: &ExternalCapsuleOptions,
) -> Result<ExternalCapsuleGuard> {
    let dependencies = manifest_external_capsule_dependencies(&plan.manifest)?;
    if dependencies.is_empty() {
        return Ok(ExternalCapsuleGuard {
            caller_env: HashMap::new(),
            children: Vec::new(),
        });
    }

    let cli_bindings = parse_cli_bindings(cli_inject_bindings)?;
    let mut guard = ExternalCapsuleGuard {
        caller_env: HashMap::new(),
        children: Vec::new(),
    };

    for dependency in dependencies {
        let locked = lockfile
            .capsule_dependencies
            .iter()
            .find(|item| item.name == dependency.alias)
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "{} is missing capsule dependency '{}'",
                    capsule_core::lockfile::CAPSULE_LOCK_FILE_NAME,
                    dependency.alias
                )
            })?;

        let manifest_path = ensure_runtime_tree_for_dependency(&locked).await?;
        let decision =
            capsule_core::router::route_manifest(&manifest_path, ExecutionProfile::Dev, None)?;

        let inject_args = merged_dependency_bindings(&decision.plan, &locked, &cli_bindings);
        let port = decision.plan.execution_port();
        let readiness_probe = decision.plan.selected_target_readiness_probe();

        reporter
            .notify(format!(
                "🔗 Starting external capsule dependency '{}'",
                dependency.alias
            ))
            .await?;

        let mut child =
            spawn_external_capsule_child(&dependency, &manifest_path, &inject_args, options)?;
        wait_for_dependency_readiness(&dependency.alias, &mut child, port, readiness_probe)?;

        if let Some(port) = port {
            guard
                .caller_env
                .extend(connection_env_vars(&dependency.alias, port));
        }
        guard.children.push(child);
    }

    Ok(guard)
}

fn spawn_external_capsule_child(
    dependency: &ExternalCapsuleDependency,
    manifest_path: &Path,
    inject_args: &[String],
    options: &ExternalCapsuleOptions,
) -> Result<ExternalCapsuleChild> {
    let executable = std::env::current_exe().context("failed to resolve current ato executable")?;
    let mut command = Command::new(executable);
    command
        .arg("open")
        .arg(manifest_path)
        .arg("--enforcement")
        .arg(&options.enforcement)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if options.sandbox_mode {
        command.arg("--sandbox");
    }
    if options.dangerously_skip_permissions {
        command.arg("--dangerously-skip-permissions");
    }
    if options.assume_yes {
        command.arg("--yes");
    }
    for binding in inject_args {
        command.arg("--inject").arg(binding);
    }

    let mut child = command.spawn().with_context(|| {
        format!(
            "failed to start external capsule dependency '{}'",
            dependency.alias
        )
    })?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    Ok(ExternalCapsuleChild {
        child,
        stdout_thread: Some(spawn_prefixed_stream(stdout, &dependency.alias, false)),
        stderr_thread: Some(spawn_prefixed_stream(stderr, &dependency.alias, true)),
    })
}

fn wait_for_dependency_readiness(
    alias: &str,
    child: &mut ExternalCapsuleChild,
    port: Option<u16>,
    readiness_probe: Option<ReadinessProbe>,
) -> Result<()> {
    let deadline = Instant::now() + EXTERNAL_READY_TIMEOUT;
    loop {
        if let Some(status) = child.child.try_wait()? {
            anyhow::bail!(
                "external capsule dependency '{}' exited before becoming ready (exit code: {})",
                alias,
                status.code().unwrap_or(1)
            );
        }

        if let Some(port) = port {
            if let Some(probe) = readiness_probe.as_ref() {
                if readiness_probe_ok(probe, port)? {
                    return Ok(());
                }
            } else if tcp_probe("127.0.0.1", port) {
                return Ok(());
            }
        } else if readiness_probe.is_none() && Instant::now() + EXTERNAL_READY_INTERVAL >= deadline
        {
            return Ok(());
        }

        if Instant::now() >= deadline {
            anyhow::bail!(
                "external capsule dependency '{}' readiness check timed out after {}s",
                alias,
                EXTERNAL_READY_TIMEOUT.as_secs()
            );
        }

        thread::sleep(EXTERNAL_READY_INTERVAL);
    }
}

async fn ensure_runtime_tree_for_dependency(locked: &LockedCapsuleDependency) -> Result<PathBuf> {
    if locked.source_type != "store" {
        anyhow::bail!(
            "external capsule dependency '{}' uses unsupported source_type '{}'",
            locked.name,
            locked.source_type
        );
    }

    let artifact_url = locked.artifact_url.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "{} capsule dependency '{}' is missing artifact_url",
            capsule_core::lockfile::CAPSULE_LOCK_FILE_NAME,
            locked.name
        )
    })?;
    let cache_path = external_capsule_cache_path(locked)?;

    let bytes = if cache_path.exists() {
        let bytes = fs::read(&cache_path)
            .with_context(|| format!("failed to read {}", cache_path.display()))?;
        verify_artifact_bytes(locked, &bytes)?;
        bytes
    } else {
        let bytes = reqwest::Client::new()
            .get(artifact_url)
            .send()
            .await
            .with_context(|| format!("failed to download {}", artifact_url))?
            .error_for_status()
            .with_context(|| format!("failed to download {}", artifact_url))?
            .bytes()
            .await
            .with_context(|| format!("failed to read artifact body {}", artifact_url))?
            .to_vec();
        verify_artifact_bytes(locked, &bytes)?;
        if let Some(parent) = cache_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&cache_path, &bytes)
            .with_context(|| format!("failed to write {}", cache_path.display()))?;
        bytes
    };

    let (publisher, slug) = parse_store_source_identity(&locked.source)?;
    let version = locked.resolved_version.as_deref().unwrap_or("resolved");
    runtime_tree::prepare_runtime_tree(&publisher, &slug, version, &bytes)
}

fn external_capsule_cache_path(locked: &LockedCapsuleDependency) -> Result<PathBuf> {
    let base = if let Ok(path) = std::env::var(EXTERNAL_CAPSULE_CACHE_DIR_ENV) {
        PathBuf::from(path)
    } else {
        dirs::home_dir()
            .context("failed to determine home directory")?
            .join(".ato")
            .join("external-capsules")
    };
    let key = locked
        .sha256
        .as_deref()
        .map(|value| value.trim_start_matches("sha256:"))
        .or_else(|| {
            locked
                .digest
                .as_deref()
                .map(|value| value.trim_start_matches("blake3:"))
        })
        .unwrap_or(locked.name.as_str());
    Ok(base.join(format!("{}.capsule", key)))
}

fn verify_artifact_bytes(locked: &LockedCapsuleDependency, bytes: &[u8]) -> Result<()> {
    if let Some(expected) = locked.sha256.as_deref() {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let actual = hex::encode(hasher.finalize());
        let expected = expected.trim_start_matches("sha256:");
        if actual != expected {
            anyhow::bail!(
                "artifact sha256 mismatch for '{}': expected {} got {}",
                locked.name,
                expected,
                actual
            );
        }
    }

    if let Some(expected) = locked.digest.as_deref() {
        if let Some(expected) = expected.strip_prefix("blake3:") {
            let actual = blake3::hash(bytes).to_hex().to_string();
            if actual != expected {
                anyhow::bail!(
                    "artifact blake3 mismatch for '{}': expected {} got {}",
                    locked.name,
                    expected,
                    actual
                );
            }
        }
    }

    Ok(())
}

fn parse_store_source_identity(source: &str) -> Result<(String, String)> {
    let raw = source
        .trim()
        .strip_prefix("capsule://store/")
        .ok_or_else(|| anyhow::anyhow!("unsupported store source '{}'", source))?;
    let raw = raw.split_once('?').map(|(path, _)| path).unwrap_or(raw);
    let raw = raw.split_once('@').map(|(path, _)| path).unwrap_or(raw);
    let mut segments = raw.split('/').filter(|segment| !segment.trim().is_empty());
    let publisher = segments
        .next()
        .ok_or_else(|| anyhow::anyhow!("invalid store source '{}'", source))?;
    let slug = segments
        .next()
        .ok_or_else(|| anyhow::anyhow!("invalid store source '{}'", source))?;
    if segments.next().is_some() {
        anyhow::bail!("invalid store source '{}'", source);
    }
    Ok((publisher.to_string(), slug.to_string()))
}

fn parse_cli_bindings(raw_bindings: &[String]) -> Result<BTreeMap<String, String>> {
    let mut bindings = BTreeMap::new();
    for raw_binding in raw_bindings {
        let Some((key, value)) = raw_binding.split_once('=') else {
            anyhow::bail!("--inject must use KEY=VALUE syntax, got '{}'", raw_binding);
        };
        let key = key.trim();
        let value = value.trim();
        if key.is_empty() || value.is_empty() {
            anyhow::bail!(
                "--inject must use non-empty KEY=VALUE syntax, got '{}'",
                raw_binding
            );
        }
        bindings.insert(key.to_string(), value.to_string());
    }
    Ok(bindings)
}

fn merged_dependency_bindings(
    plan: &ManifestData,
    locked: &LockedCapsuleDependency,
    cli_bindings: &BTreeMap<String, String>,
) -> Vec<String> {
    let contract = plan.selected_target_external_injection();
    let mut merged = locked.injection_bindings.clone();
    for key in contract.keys() {
        if let Some(value) = cli_bindings.get(key) {
            merged.insert(key.clone(), value.clone());
        }
    }

    let mut values: Vec<String> = merged
        .into_iter()
        .map(|(key, value)| format!("{}={}", key, value))
        .collect();
    values.sort();
    values
}

fn connection_env_vars(alias: &str, port: u16) -> HashMap<String, String> {
    let mut env = HashMap::new();
    let key = sanitize_alias(alias);
    env.insert(format!("ATO_PKG_{}_HOST", key), "127.0.0.1".to_string());
    env.insert(format!("ATO_PKG_{}_PORT", key), port.to_string());
    env.insert(
        format!("ATO_PKG_{}_URL", key),
        format!("http://127.0.0.1:{}", port),
    );
    env.insert(format!("ATO_SERVICE_{}_HOST", key), "127.0.0.1".to_string());
    env.insert(format!("ATO_SERVICE_{}_PORT", key), port.to_string());
    env
}

fn sanitize_alias(alias: &str) -> String {
    alias
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn spawn_prefixed_stream(
    stream: Option<impl std::io::Read + Send + 'static>,
    alias: &str,
    is_stderr: bool,
) -> JoinHandle<std::io::Result<()>> {
    let prefix = format!("[ext:{}] ", alias);
    thread::spawn(move || {
        let Some(stream) = stream else {
            return Ok(());
        };
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        loop {
            line.clear();
            let bytes = reader.read_line(&mut line)?;
            if bytes == 0 {
                break;
            }
            if is_stderr {
                let mut stderr = std::io::stderr();
                stderr.write_all(prefix.as_bytes())?;
                stderr.write_all(line.as_bytes())?;
                stderr.flush()?;
            } else {
                let mut stdout = std::io::stdout();
                stdout.write_all(prefix.as_bytes())?;
                stdout.write_all(line.as_bytes())?;
                stdout.flush()?;
            }
        }
        Ok(())
    })
}

fn readiness_probe_ok(probe: &ReadinessProbe, port: u16) -> Result<bool> {
    if let Some(path) = probe
        .http_get
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        return Ok(http_probe(path, port));
    }
    if let Some(target) = probe
        .tcp_connect
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        return Ok(matches!(target, "$PORT" | "PORT") && tcp_probe("127.0.0.1", port));
    }
    anyhow::bail!("readiness_probe must define http_get or tcp_connect")
}

fn http_probe(path: &str, port: u16) -> bool {
    if path.starts_with("http://") || path.starts_with("https://") {
        return false;
    }
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{}", path)
    };

    let Ok(mut stream) = std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_secs(1),
    ) else {
        return false;
    };
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        path
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader.read_line(&mut status_line).is_ok()
        && (status_line.contains(" 200 ")
            || status_line.contains(" 201 ")
            || status_line.contains(" 204 "))
}

fn tcp_probe(host: &str, port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &format!("{}:{}", host, port)
            .parse()
            .unwrap_or(std::net::SocketAddr::from(([127, 0, 0, 1], port))),
        Duration::from_secs(1),
    )
    .is_ok()
}

#[cfg(test)]
mod tests {
    use super::{connection_env_vars, merged_dependency_bindings, sanitize_alias};
    use capsule_core::router::{ExecutionProfile, ManifestData};
    use std::collections::{BTreeMap, HashMap};
    use std::path::PathBuf;

    #[test]
    fn builds_parent_connection_env() {
        let env = connection_env_vars("auth-svc", 8080);
        assert_eq!(env["ATO_PKG_AUTH_SVC_HOST"], "127.0.0.1");
        assert_eq!(env["ATO_PKG_AUTH_SVC_PORT"], "8080");
        assert_eq!(env["ATO_PKG_AUTH_SVC_URL"], "http://127.0.0.1:8080");
    }

    #[test]
    fn sanitize_alias_normalizes_non_alnum() {
        assert_eq!(sanitize_alias("api-gateway/v1"), "API_GATEWAY_V1");
    }

    #[test]
    fn cli_bindings_override_locked_dependency_bindings() {
        let mut target = toml::map::Map::new();
        target.insert(
            "runtime".to_string(),
            toml::Value::String("source".to_string()),
        );
        target.insert(
            "driver".to_string(),
            toml::Value::String("native".to_string()),
        );
        target.insert(
            "entrypoint".to_string(),
            toml::Value::String("main.py".to_string()),
        );
        target.insert(
            "external_injection".to_string(),
            toml::Value::Table(toml::map::Map::from_iter([(
                "MODEL_DIR".to_string(),
                toml::Value::Table(toml::map::Map::from_iter([(
                    "type".to_string(),
                    toml::Value::String("directory".to_string()),
                )])),
            )])),
        );
        let manifest = toml::Value::Table(toml::map::Map::from_iter([
            ("name".to_string(), toml::Value::String("demo".to_string())),
            (
                "default_target".to_string(),
                toml::Value::String("default".to_string()),
            ),
            (
                "targets".to_string(),
                toml::Value::Table(toml::map::Map::from_iter([(
                    "default".to_string(),
                    toml::Value::Table(target),
                )])),
            ),
        ]));
        let plan = ManifestData {
            manifest,
            manifest_path: PathBuf::from("capsule.toml"),
            manifest_dir: PathBuf::from("."),
            profile: ExecutionProfile::Dev,
            selected_target: "default".to_string(),
            state_source_overrides: HashMap::new(),
        };
        let locked = capsule_core::lockfile::LockedCapsuleDependency {
            name: "worker".to_string(),
            source: "capsule://store/acme/worker".to_string(),
            source_type: "store".to_string(),
            injection_bindings: BTreeMap::from([(
                "MODEL_DIR".to_string(),
                "https://data.tld/default.zip".to_string(),
            )]),
            resolved_version: Some("1.0.0".to_string()),
            digest: None,
            sha256: None,
            artifact_url: None,
        };
        let cli = BTreeMap::from([("MODEL_DIR".to_string(), "file://./local-model".to_string())]);

        let bindings = merged_dependency_bindings(&plan, &locked, &cli);
        assert_eq!(bindings, vec!["MODEL_DIR=file://./local-model".to_string()]);
    }
}
