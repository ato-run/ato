use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use crate::common::paths::manifest_dir;
use crate::config;
use crate::error::{CapsuleError, Result};
use crate::router::CompatProjectInput;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

pub struct EngineRequest {
    pub explicit_path: Option<PathBuf>,
    pub manifest_path: Option<PathBuf>,
    pub compat_input: Option<CompatProjectInput>,
}

pub fn discover_nacelle(req: EngineRequest) -> Result<PathBuf> {
    // 1) Explicit CLI flag
    if let Some(path) = req.explicit_path {
        return validate_engine_path(path);
    }

    // 2) Environment override
    if let Ok(env_path) = std::env::var("NACELLE_PATH") {
        if !env_path.trim().is_empty() {
            return validate_engine_path(PathBuf::from(env_path));
        }
    }

    // 3) Project manifest (capsule.toml)
    if let Some(compat_input) = req.compat_input {
        if let Some(path) = resolve_from_compat_input(&compat_input)? {
            return validate_engine_path(path);
        }
    }

    // 4) Project manifest (capsule.toml)
    if let Some(manifest_path) = req.manifest_path {
        if let Some(path) = resolve_from_manifest(&manifest_path)? {
            return validate_engine_path(path);
        }
    }

    // 5) User registry (~/.ato/config.toml)
    {
        let cfg = config::load_config().map_err(|e| CapsuleError::Config(e.to_string()))?;
        if let Some(default_name) = cfg.default_engine.as_deref() {
            if let Some(entry) = cfg.engines.get(default_name) {
                return validate_engine_path(PathBuf::from(&entry.path)).map_err(|e| {
                    CapsuleError::Config(format!(
                        "Default engine '{}' is not usable (path={}): {}",
                        default_name, entry.path, e
                    ))
                });
            }
        }
    }

    // 6) Portable mode: look next to capsule binary
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("nacelle");
            if candidate.exists() {
                return validate_engine_path(candidate);
            }
        }
    }

    Err(CapsuleError::NotFound(
        "Nacelle engine not found. PATH search is disabled for security.\n\
\
Resolve options:\n\
  - pass --nacelle /absolute/path/to/nacelle\n\
  - set NACELLE_PATH=/absolute/path/to/nacelle\n\
  - register a default engine: ato engine register --name default --path /absolute/path/to/nacelle --default\n\
  - (portable) place nacelle next to the capsule binary"
            .to_string(),
    ))
}

#[cfg(unix)]
fn is_executable(mode: u32) -> bool {
    (mode & 0o111) != 0
}

fn validate_engine_path(path: PathBuf) -> Result<PathBuf> {
    let canonical = path.canonicalize().map_err(|e| {
        CapsuleError::NotFound(format!(
            "Failed to resolve engine path '{}': {}",
            path.display(),
            e
        ))
    })?;

    let meta = std::fs::metadata(&canonical).map_err(|e| CapsuleError::Io(e))?;

    if !meta.is_file() {
        return Err(CapsuleError::Config(format!(
            "Engine path is not a file: {}",
            canonical.display()
        )));
    }

    #[cfg(unix)]
    {
        let mode = meta.permissions().mode();
        if !is_executable(mode) {
            return Err(CapsuleError::Config(format!(
                "Engine is not executable: {}",
                canonical.display()
            )));
        }
    }

    Ok(canonical)
}

fn resolve_from_manifest(manifest_path: &Path) -> Result<Option<PathBuf>> {
    let manifest_path = normalize_manifest_lookup_path(manifest_path);

    if manifest_path.file_name().and_then(|name| name.to_str()) != Some("capsule.toml") {
        return Ok(None);
    }

    if !manifest_path.exists() {
        return Ok(None);
    }

    let raw = std::fs::read_to_string(&manifest_path).map_err(|e| {
        CapsuleError::Manifest(
            manifest_path.clone(),
            format!("Failed to read manifest: {}", e),
        )
    })?;
    let parsed = toml::from_str::<toml::Value>(&raw).map_err(|e| {
        CapsuleError::Manifest(
            manifest_path.clone(),
            format!("Failed to parse manifest TOML: {}", e),
        )
    })?;

    let engine = parsed.get("engine");
    if engine.is_none() {
        return Ok(None);
    }

    let manifest_dir = manifest_dir(&manifest_path);

    // [engine]
    // nacelle_path = "..."   (path; may be relative to manifest)
    if let Some(path) = engine
        .and_then(|t| t.get("nacelle_path"))
        .and_then(|v| v.as_str())
    {
        let p = PathBuf::from(path);
        return Ok(Some(if p.is_absolute() {
            p
        } else {
            manifest_dir.join(p)
        }));
    }

    // [engine]
    // source = "alias"       (registered engine name)
    if let Some(alias) = engine
        .and_then(|t| t.get("source"))
        .and_then(|v| v.as_str())
    {
        let cfg = config::load_config().map_err(|e| CapsuleError::Config(e.to_string()))?;
        if let Some(entry) = cfg.engines.get(alias) {
            return Ok(Some(PathBuf::from(&entry.path)));
        }
        return Err(CapsuleError::Config(format!(
            "Engine alias '{}' not registered. Run: ato engine register --name {} --path /abs/path/to/nacelle",
            alias, alias
        )));
    }

    Ok(None)
}

fn resolve_from_compat_input(compat_input: &CompatProjectInput) -> Result<Option<PathBuf>> {
    if let Some(path) = compat_input.engine_nacelle_path() {
        return Ok(Some(path));
    }

    if let Some(alias) = compat_input.engine_source_alias() {
        let cfg = config::load_config().map_err(|e| CapsuleError::Config(e.to_string()))?;
        if let Some(entry) = cfg.engines.get(alias) {
            return Ok(Some(PathBuf::from(&entry.path)));
        }
        return Err(CapsuleError::Config(format!(
            "Engine alias '{}' not registered. Run: ato engine register --name {} --path /abs/path/to/nacelle",
            alias, alias
        )));
    }

    Ok(None)
}

fn normalize_manifest_lookup_path(manifest_path: &Path) -> PathBuf {
    if manifest_path.is_dir() {
        return manifest_path.join("capsule.toml");
    }

    manifest_path.to_path_buf()
}

pub fn run_internal(engine: &Path, subcommand: &str, payload: &Value) -> Result<Value> {
    let mut child = Command::new(engine)
        .arg("internal")
        .arg("--input")
        .arg("-")
        .arg(subcommand)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| {
            CapsuleError::ProcessStart(format!(
                "Failed to spawn engine '{}': {}",
                engine.display(),
                e
            ))
        })?;

    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| CapsuleError::ProcessStart("Failed to open engine stdin".to_string()))?;
        let bytes = serde_json::to_vec(payload)
            .map_err(|e| CapsuleError::Runtime(format!("Failed to serialize payload: {}", e)))?;
        stdin.write_all(&bytes).map_err(CapsuleError::Io)?;
    }

    let output = child.wait_with_output().map_err(|e| {
        CapsuleError::Execution(format!(
            "Engine invocation failed (internal {}): {}",
            subcommand, e
        ))
    })?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();

    if stdout.is_empty() {
        return Err(CapsuleError::Execution(format!(
            "Engine returned empty stdout (exit={})",
            output.status.code().unwrap_or(1)
        )));
    }

    let json: Value = serde_json::from_str(&stdout).map_err(|e| {
        CapsuleError::Execution(format!(
            "Failed to parse engine JSON output (exit={}): {}: {}",
            output.status.code().unwrap_or(1),
            e,
            stdout
        ))
    })?;

    // Engine may exit non-zero for workload exit codes; surface JSON either way.
    Ok(json)
}

/// Run an internal subcommand in streaming mode.
///
/// - stdin: JSON payload
/// - stdout/stderr: inherited (logs stream directly)
/// - returns: exit code of the engine process
///
/// Note: planned API for interactive streaming workflows; not yet wired to a CLI command.
pub fn run_internal_streaming(engine: &Path, subcommand: &str, payload: &Value) -> Result<i32> {
    let mut child = Command::new(engine)
        .arg("internal")
        .arg("--input")
        .arg("-")
        .arg(subcommand)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| {
            CapsuleError::ProcessStart(format!(
                "Failed to spawn engine '{}': {}",
                engine.display(),
                e
            ))
        })?;

    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| CapsuleError::ProcessStart("Failed to open engine stdin".to_string()))?;
        let bytes = serde_json::to_vec(payload)
            .map_err(|e| CapsuleError::Runtime(format!("Failed to serialize payload: {}", e)))?;
        stdin.write_all(&bytes).map_err(CapsuleError::Io)?;
    }

    let child_slot: Arc<Mutex<Option<std::process::Child>>> = Arc::new(Mutex::new(Some(child)));
    #[cfg(unix)]
    let child_slot_for_handler = Arc::clone(&child_slot);

    // Forward Ctrl-C to the engine process.
    // If Ctrl-C fires before we install the handler, the default behavior
    // (SIGINT to the process group) still applies.
    ctrlc::set_handler(move || {
        #[cfg(unix)]
        {
            if let Ok(mut guard) = child_slot_for_handler.lock() {
                if let Some(ref mut c) = *guard {
                    let _ = unsafe { libc::kill(c.id() as i32, libc::SIGINT) };
                }
            }
        }
    })
    .map_err(|e| CapsuleError::Runtime(format!("Failed to set Ctrl-C handler: {}", e)))?;

    let status = {
        let mut guard = child_slot.lock().expect("lock poisoned");
        let mut child = guard.take().expect("child missing");
        child.wait().map_err(|e| {
            CapsuleError::Execution(format!(
                "Engine invocation failed (internal {}): {}",
                subcommand, e
            ))
        })?
    };

    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;

    use tempfile::TempDir;

    struct EnvGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn discover_nacelle_falls_back_to_registered_engine_for_directory_manifest_path() {
        use std::os::unix::fs::PermissionsExt;

        let home = TempDir::new().expect("tempdir");
        let _home = EnvGuard::set("HOME", home.path().to_string_lossy().as_ref());

        let project = TempDir::new().expect("project");
        fs::write(project.path().join("capsule.toml"), "name = \"demo\"\n").expect("manifest");

        let engine_path = home.path().join("nacelle");
        fs::write(&engine_path, "#!/bin/sh\nexit 0\n").expect("engine");
        let mut perms = fs::metadata(&engine_path).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&engine_path, perms).expect("chmod");

        let config_dir = home.path().join(".ato");
        fs::create_dir_all(&config_dir).expect("config dir");
        fs::write(
            config_dir.join("config.toml"),
            format!(
                "default_engine = \"nacelle\"\n\n[engines.nacelle]\npath = \"{}\"\n",
                engine_path.display()
            ),
        )
        .expect("config");

        let discovered = discover_nacelle(EngineRequest {
            explicit_path: None,
            manifest_path: Some(project.path().to_path_buf()),
            compat_input: None,
        })
        .expect("discover nacelle");

        assert_eq!(discovered, engine_path.canonicalize().expect("canonical"));
    }

    #[cfg(unix)]
    #[test]
    fn discover_nacelle_ignores_generated_shadow_manifest_for_engine_lookup() {
        use std::os::unix::fs::PermissionsExt;

        let home = TempDir::new().expect("tempdir");
        let _home = EnvGuard::set("HOME", home.path().to_string_lossy().as_ref());

        let project = TempDir::new().expect("project");
        let shadow_manifest = project.path().join(".ato-nacelle-123.toml");
        fs::write(
            &shadow_manifest,
            "name = \"demo\"\n[execution]\nentrypoint = \"uv\"\n",
        )
        .expect("shadow manifest");

        let engine_path = home.path().join("nacelle");
        fs::write(&engine_path, "#!/bin/sh\nexit 0\n").expect("engine");
        let mut perms = fs::metadata(&engine_path).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&engine_path, perms).expect("chmod");

        let config_dir = home.path().join(".ato");
        fs::create_dir_all(&config_dir).expect("config dir");
        fs::write(
            config_dir.join("config.toml"),
            format!(
                "default_engine = \"nacelle\"\n\n[engines.nacelle]\npath = \"{}\"\n",
                engine_path.display()
            ),
        )
        .expect("config");

        let discovered = discover_nacelle(EngineRequest {
            explicit_path: None,
            manifest_path: Some(shadow_manifest),
            compat_input: None,
        })
        .expect("discover nacelle");

        assert_eq!(discovered, engine_path.canonicalize().expect("canonical"));
    }
}
