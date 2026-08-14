//! Physical realization of `ato.workspace@1` computations.

use std::collections::BTreeMap;
#[cfg(target_os = "linux")]
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::{Arc, RwLock};

use ato_computation::ContentRef;
use ato_semantics_workspace::{
    ProviderError, ToolchainConstraint, WorkspaceOutcome, WorkspaceProvider, WorkspaceResidual,
};

use crate::launcher::source::toolchain::{RuntimeFetcher, ToolchainManager};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::system::sandbox::SandboxPolicy;

pub trait RuntimeResolver: Send + Sync {
    fn resolve(
        &self,
        toolchain: &ToolchainConstraint,
        requested_program: &str,
    ) -> Result<PathBuf, ProviderError>;
}

#[derive(Default)]
pub struct NacelleRuntimeResolver;

impl RuntimeResolver for NacelleRuntimeResolver {
    fn resolve(
        &self,
        toolchain: &ToolchainConstraint,
        requested_program: &str,
    ) -> Result<PathBuf, ProviderError> {
        if let Some(version) = toolchain.version.as_deref() {
            let fetcher =
                RuntimeFetcher::new().map_err(|error| ProviderError::new(error.to_string()))?;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| ProviderError::new(error.to_string()))?;
            let executable = runtime
                .block_on(async {
                    match toolchain.family.as_str() {
                        "python" => fetcher.ensure_python(version).await,
                        "node" => fetcher.ensure_node(version).await,
                        "deno" => fetcher.ensure_deno(version).await,
                        "bun" => fetcher.ensure_bun(version).await,
                        family => anyhow::bail!(
                            "managed runtime resolution is unavailable for {family} {version}"
                        ),
                    }
                })
                .map_err(|error| ProviderError::new(error.to_string()))?;
            return controlled_program(&executable, requested_program);
        }

        let manager = ToolchainManager::new();
        if let Some(toolchain) = manager.find_toolchain(&toolchain.family, None) {
            return controlled_program(&toolchain.path, requested_program);
        }
        which::which(requested_program).map_err(|error| {
            ProviderError::new(format!(
                "unversioned runtime `{requested_program}` is unavailable: {error}"
            ))
        })
    }
}

fn controlled_program(runtime: &Path, requested_program: &str) -> Result<PathBuf, ProviderError> {
    let runtime_name = runtime.file_stem().and_then(|name| name.to_str());
    let requested_stem = Path::new(requested_program)
        .file_stem()
        .and_then(|name| name.to_str());
    if runtime_name == requested_stem
        || matches!(
            (runtime_name, requested_stem),
            (Some("python" | "python3"), Some("python" | "python3"))
        )
    {
        return Ok(runtime.to_path_buf());
    }
    let sibling = runtime
        .parent()
        .ok_or_else(|| ProviderError::new("managed runtime has no binary directory"))?
        .join(requested_program);
    if sibling.is_file() {
        return Ok(sibling);
    }
    #[cfg(windows)]
    {
        let executable = sibling.with_extension("exe");
        if executable.is_file() {
            return Ok(executable);
        }
        let command = sibling.with_extension("cmd");
        if command.is_file() {
            return Ok(command);
        }
    }
    Err(ProviderError::new(format!(
        "program `{requested_program}` is absent from managed runtime {}",
        runtime.display()
    )))
}

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
    runtimes: Arc<dyn RuntimeResolver>,
}

impl Default for NacelleWorkspaceProvider {
    fn default() -> Self {
        Self::new(Arc::new(EmptySecretBackend))
    }
}

impl NacelleWorkspaceProvider {
    pub fn new(secrets: Arc<dyn SecretBackend>) -> Self {
        Self::with_runtime_resolver(secrets, Arc::new(NacelleRuntimeResolver))
    }

    pub fn with_runtime_resolver(
        secrets: Arc<dyn SecretBackend>,
        runtimes: Arc<dyn RuntimeResolver>,
    ) -> Self {
        Self {
            sources: RwLock::new(BTreeMap::new()),
            secrets,
            runtimes,
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
        if !workspace.realization.network_allow.is_empty() {
            return Err(ProviderError::new(
                "exact network allowlist enforcement is unavailable for this provider",
            ));
        }
        let executable = self.runtimes.resolve(&workspace.toolchain, program)?;
        let runtime_directory = executable
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let mut command = provider_command(executable, workspace, root)?;
        let home = root.join(".ato");
        std::fs::create_dir_all(&home).map_err(|error| ProviderError::new(error.to_string()))?;
        command.env_clear();
        command
            .env(
                "PATH",
                std::env::join_paths([runtime_directory])
                    .map_err(|error| ProviderError::new(error.to_string()))?,
            )
            .env("HOME", home)
            .env("LANG", "C.UTF-8")
            .env("LC_ALL", "C.UTF-8");
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
        .with_network(false))
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
    use std::sync::Mutex;

    use ato_semantics_workspace::{RealizationConstraint, ToolchainConstraint, WorkspacePhase};

    use super::*;

    struct FixedRuntime {
        executable: PathBuf,
        requests: Mutex<Vec<(ToolchainConstraint, String)>>,
    }

    impl FixedRuntime {
        fn new(executable: PathBuf) -> Self {
            Self {
                executable,
                requests: Mutex::new(Vec::new()),
            }
        }
    }

    impl RuntimeResolver for FixedRuntime {
        fn resolve(
            &self,
            toolchain: &ToolchainConstraint,
            requested_program: &str,
        ) -> Result<PathBuf, ProviderError> {
            self.requests
                .lock()
                .unwrap()
                .push((toolchain.clone(), requested_program.to_owned()));
            Ok(self.executable.clone())
        }
    }

    fn workspace(source: &ContentRef) -> WorkspaceResidual {
        WorkspaceResidual {
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
        }
    }

    #[test]
    fn realizes_bound_source_and_returns_process_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        let source = ContentRef::parse(format!("blake3:{}", "ab".repeat(32))).unwrap();
        let provider = NacelleWorkspaceProvider::default();
        provider
            .bind_materialized_source(source.clone(), dir.path())
            .unwrap();
        let workspace = workspace(&source);

        assert_eq!(provider.realize(&workspace).unwrap().exit_code, 7);
    }

    #[test]
    fn command_environment_is_a_closed_provider_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let source = ContentRef::parse(format!("blake3:{}", "cd".repeat(32))).unwrap();
        let runtime = Arc::new(FixedRuntime::new(std::env::current_exe().unwrap()));
        let provider =
            NacelleWorkspaceProvider::with_runtime_resolver(Arc::new(EmptySecretBackend), runtime);
        provider
            .bind_materialized_source(source.clone(), dir.path())
            .unwrap();
        let mut workspace = workspace(&source);
        workspace
            .environment
            .insert("DECLARED".to_owned(), "yes".to_owned());

        let root = std::fs::canonicalize(dir.path()).unwrap();
        let command = provider
            .command(&workspace, &root, &workspace.entrypoint)
            .unwrap();
        let environment: BTreeMap<_, _> = command
            .get_envs()
            .filter_map(|(name, value)| value.map(|value| (name.to_owned(), value.to_owned())))
            .collect();
        let keys: Vec<_> = environment
            .keys()
            .map(|key| key.to_string_lossy().into_owned())
            .collect();
        assert_eq!(keys, ["DECLARED", "HOME", "LANG", "LC_ALL", "PATH"]);
    }

    #[test]
    fn exact_network_allowlist_fails_closed_without_enforcement() {
        let dir = tempfile::tempdir().unwrap();
        let source = ContentRef::parse(format!("blake3:{}", "ef".repeat(32))).unwrap();
        let runtime = Arc::new(FixedRuntime::new(std::env::current_exe().unwrap()));
        let provider =
            NacelleWorkspaceProvider::with_runtime_resolver(Arc::new(EmptySecretBackend), runtime);
        provider
            .bind_materialized_source(source.clone(), dir.path())
            .unwrap();
        let mut workspace = workspace(&source);
        workspace.realization.network_allow = vec!["api.openai.com".to_owned()];

        let error = provider.realize(&workspace).unwrap_err();
        assert!(error.to_string().contains("exact network allowlist"));
    }

    #[cfg(unix)]
    #[test]
    fn pinned_runtime_uses_resolved_artifact_not_requested_path_name() {
        let dir = tempfile::tempdir().unwrap();
        let source = ContentRef::parse(format!("blake3:{}", "12".repeat(32))).unwrap();
        let runtime = Arc::new(FixedRuntime::new(PathBuf::from("/bin/sh")));
        let provider = NacelleWorkspaceProvider::with_runtime_resolver(
            Arc::new(EmptySecretBackend),
            runtime.clone(),
        );
        provider
            .bind_materialized_source(source.clone(), dir.path())
            .unwrap();
        let mut workspace = workspace(&source);
        workspace.toolchain = ToolchainConstraint {
            family: "python".to_owned(),
            version: Some("3.11.10".to_owned()),
        };
        workspace.entrypoint = vec![
            "poisoned-python".to_owned(),
            "-c".to_owned(),
            "exit 0".to_owned(),
        ];

        assert_eq!(provider.realize(&workspace).unwrap().exit_code, 0);
        assert_eq!(
            runtime.requests.lock().unwrap().as_slice(),
            [(workspace.toolchain, "poisoned-python".to_owned())]
        );
    }
}
