//! Physical realization of `ato.workspace@1` computations.

use std::collections::BTreeMap;
#[cfg(target_os = "linux")]
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::{Arc, RwLock};

use ato_computation::ContentRef;
use ato_semantics_workspace::{
    ProviderError, WorkspaceOutcome, WorkspaceProvider, WorkspaceResidual,
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::system::sandbox::SandboxPolicy;

pub trait SecretBackend: Send + Sync {
    fn resolve(&self, binding: &str) -> Result<String, ProviderError>;
}

#[derive(Default)]
pub struct EmptySecretBackend;

impl SecretBackend for EmptySecretBackend {
    fn resolve(&self, binding: &str) -> Result<String, ProviderError> {
        Err(ProviderError::new(format!(
            "no secret backend is configured for binding {binding}"
        )))
    }
}

pub struct NacelleWorkspaceProvider {
    sources: RwLock<BTreeMap<ContentRef, PathBuf>>,
    secrets: Arc<dyn SecretBackend>,
}

impl Default for NacelleWorkspaceProvider {
    fn default() -> Self {
        Self::new(Arc::new(EmptySecretBackend))
    }
}

impl NacelleWorkspaceProvider {
    pub fn new(secrets: Arc<dyn SecretBackend>) -> Self {
        Self {
            sources: RwLock::new(BTreeMap::new()),
            secrets,
        }
    }

    /// Register a source directory produced from the matching sealed closure.
    /// Callers must not pass a mutable authoring repository.
    pub fn bind_materialized_source(
        &self,
        source: ContentRef,
        repository_root: impl Into<PathBuf>,
    ) -> Result<(), ProviderError> {
        let root = std::fs::canonicalize(repository_root.into())
            .map_err(|error| ProviderError::new(error.to_string()))?;
        if !root.is_dir() {
            return Err(ProviderError::new(format!(
                "source binding is not a directory: {}",
                root.display()
            )));
        }
        self.sources
            .write()
            .map_err(|error| ProviderError::new(error.to_string()))?
            .insert(source, root);
        Ok(())
    }

    fn source_root(&self, source: &str) -> Result<PathBuf, ProviderError> {
        let source = ContentRef::parse(source.to_owned())
            .map_err(|error| ProviderError::new(error.to_string()))?;
        self.sources
            .read()
            .map_err(|error| ProviderError::new(error.to_string()))?
            .get(&source)
            .cloned()
            .ok_or_else(|| ProviderError::new(format!("source {source} is not materialized")))
    }

    fn command(
        &self,
        workspace: &WorkspaceResidual,
        root: &Path,
        argv: &[String],
    ) -> Result<Command, ProviderError> {
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| ProviderError::new("empty workspace entrypoint"))?;
        let cwd = safe_working_directory(root, &workspace.working_directory)?;
        let executable = which::which(program).map_err(|error| {
            ProviderError::new(format!("runtime `{program}` is unavailable: {error}"))
        })?;
        let mut command = provider_command(executable, workspace, root)?;
        command.args(args).current_dir(cwd);
        for (name, value) in &workspace.environment {
            command.env(name, value);
        }
        for (name, binding) in &workspace.secret_bindings {
            command.env(name, self.secrets.resolve(binding)?);
        }
        configure_sandbox(&mut command, workspace, root)?;
        Ok(command)
    }

    fn prepare_dependencies(
        &self,
        workspace: &WorkspaceResidual,
        root: &Path,
    ) -> Result<(), ProviderError> {
        let argv = match workspace.package_manager.as_deref() {
            Some("npm") if root.join("package-lock.json").is_file() => {
                Some(vec!["npm".to_owned(), "ci".to_owned()])
            }
            Some("npm") => Some(vec!["npm".to_owned(), "install".to_owned()]),
            Some("pnpm") => Some(vec![
                "pnpm".to_owned(),
                "install".to_owned(),
                "--frozen-lockfile".to_owned(),
            ]),
            Some("yarn") => Some(vec![
                "yarn".to_owned(),
                "install".to_owned(),
                "--immutable".to_owned(),
            ]),
            Some("bun") => Some(vec![
                "bun".to_owned(),
                "install".to_owned(),
                "--frozen-lockfile".to_owned(),
            ]),
            Some("pip") if root.join("requirements.txt").is_file() => Some(vec![
                "python".to_owned(),
                "-m".to_owned(),
                "pip".to_owned(),
                "install".to_owned(),
                "-r".to_owned(),
                "requirements.txt".to_owned(),
            ]),
            Some("cargo") if root.join("Cargo.lock").is_file() => Some(vec![
                "cargo".to_owned(),
                "fetch".to_owned(),
                "--locked".to_owned(),
            ]),
            Some("go") => Some(vec![
                "go".to_owned(),
                "mod".to_owned(),
                "download".to_owned(),
            ]),
            _ => None,
        };
        if let Some(argv) = argv {
            let status = self
                .command(workspace, root, &argv)?
                .status()
                .map_err(|error| ProviderError::new(error.to_string()))?;
            require_success("dependency materialization", status)?;
        }
        Ok(())
    }
}

impl WorkspaceProvider for NacelleWorkspaceProvider {
    fn realize(&self, workspace: &WorkspaceResidual) -> Result<WorkspaceOutcome, ProviderError> {
        let root = self.source_root(&workspace.source)?;
        self.prepare_dependencies(workspace, &root)?;
        let status = self
            .command(workspace, &root, &workspace.entrypoint)?
            .status()
            .map_err(|error| ProviderError::new(error.to_string()))?;
        Ok(WorkspaceOutcome {
            exit_code: status.code().unwrap_or(128),
        })
    }
}

fn safe_working_directory(root: &Path, requested: &str) -> Result<PathBuf, ProviderError> {
    let requested = Path::new(requested);
    if requested.is_absolute()
        || requested.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(ProviderError::new("workspace cwd escapes source root"));
    }
    let joined = root.join(requested);
    let canonical =
        std::fs::canonicalize(&joined).map_err(|error| ProviderError::new(error.to_string()))?;
    if !canonical.starts_with(root) {
        return Err(ProviderError::new("workspace cwd escapes source root"));
    }
    Ok(canonical)
}

fn require_success(operation: &str, status: ExitStatus) -> Result<(), ProviderError> {
    if status.success() {
        Ok(())
    } else {
        Err(ProviderError::new(format!(
            "{operation} exited with {status}"
        )))
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn sandbox_policy(
    workspace: &WorkspaceResidual,
    root: &Path,
) -> Result<SandboxPolicy, ProviderError> {
    let writable = workspace
        .realization
        .writable_paths
        .iter()
        .map(|path| materialize_writable_path(root, path))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SandboxPolicy::for_capsule(root)
        .allow_read_write(writable)
        .with_network(!workspace.realization.network_allow.is_empty()))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn materialize_writable_path(root: &Path, requested: &str) -> Result<PathBuf, ProviderError> {
    let requested = Path::new(requested);
    if requested.as_os_str().is_empty()
        || requested.is_absolute()
        || requested.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(ProviderError::new("writable path escapes source root"));
    }
    let path = root.join(requested);
    std::fs::create_dir_all(&path).map_err(|error| ProviderError::new(error.to_string()))?;
    let canonical =
        std::fs::canonicalize(path).map_err(|error| ProviderError::new(error.to_string()))?;
    if !canonical.starts_with(root) {
        return Err(ProviderError::new("writable path escapes source root"));
    }
    Ok(canonical)
}

#[cfg(target_os = "macos")]
fn provider_command(
    executable: PathBuf,
    workspace: &WorkspaceResidual,
    root: &Path,
) -> Result<Command, ProviderError> {
    if !workspace.realization.sandbox_required {
        return Ok(Command::new(executable));
    }
    let profile =
        crate::system::sandbox::macos::generate_sbpl_profile(&sandbox_policy(workspace, root)?);
    let mut command = Command::new("/usr/bin/sandbox-exec");
    command.args(["-p", &profile]).arg(executable);
    Ok(command)
}

#[cfg(not(target_os = "macos"))]
fn provider_command(
    executable: PathBuf,
    _workspace: &WorkspaceResidual,
    _root: &Path,
) -> Result<Command, ProviderError> {
    Ok(Command::new(executable))
}

#[cfg(target_os = "linux")]
fn configure_sandbox(
    command: &mut Command,
    workspace: &WorkspaceResidual,
    root: &Path,
) -> Result<(), ProviderError> {
    use std::os::unix::process::CommandExt;

    if !workspace.realization.sandbox_required {
        return Ok(());
    }
    let policy = sandbox_policy(workspace, root)?;
    unsafe {
        command.pre_exec(move || {
            let result = crate::system::sandbox::apply_sandbox(&policy)
                .map_err(|error| io::Error::other(error.to_string()))?;
            if !result.fully_enforced {
                return Err(io::Error::other(result.message));
            }
            Ok(())
        });
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn configure_sandbox(
    _command: &mut Command,
    workspace: &WorkspaceResidual,
    _root: &Path,
) -> Result<(), ProviderError> {
    if workspace.realization.sandbox_required && !cfg!(target_os = "macos") {
        return Err(ProviderError::new(
            "required workspace sandbox is unavailable on this platform",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ato_semantics_workspace::{RealizationConstraint, ToolchainConstraint, WorkspacePhase};

    use super::*;

    #[test]
    fn realizes_bound_source_and_returns_process_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        let source = ContentRef::parse(format!("blake3:{}", "ab".repeat(32))).unwrap();
        let provider = NacelleWorkspaceProvider::default();
        provider
            .bind_materialized_source(source.clone(), dir.path())
            .unwrap();
        let workspace = WorkspaceResidual {
            source: source.to_string(),
            toolchain: ToolchainConstraint {
                family: "shell".to_owned(),
                version: None,
            },
            package_manager: None,
            entrypoint: vec!["sh".to_owned(), "-c".to_owned(), "exit 7".to_owned()],
            working_directory: ".".to_owned(),
            environment: BTreeMap::new(),
            secret_bindings: BTreeMap::new(),
            realization: RealizationConstraint {
                network_allow: Vec::new(),
                writable_paths: Vec::new(),
                sandbox_required: false,
            },
            phase: WorkspacePhase::Ready,
        };

        assert_eq!(provider.realize(&workspace).unwrap().exit_code, 7);
    }
}
