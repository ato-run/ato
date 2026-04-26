use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::internal_api::ExportedArtifact;
use crate::launcher::source::SourceRuntimeConfig;
use crate::launcher::InjectedMount;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OverlayMountSpec {
    pub source: PathBuf,
    pub target: PathBuf,
    #[serde(default)]
    pub readonly: bool,
    #[serde(default)]
    pub mode: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DerivedOutputMountSpec {
    pub host_path: PathBuf,
    pub target: PathBuf,
    #[serde(default = "default_derived_output_kind")]
    pub kind: String,
}

fn default_derived_output_kind() -> String {
    "derived_output".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RuntimeArtifactReference {
    pub name: String,
    pub path: PathBuf,
    #[serde(default)]
    pub env_var: Option<String>,
    #[serde(default)]
    pub add_to_path: bool,
}

#[derive(Debug, Clone)]
pub struct EnvironmentPrepareRequest {
    pub run_id: String,
    pub spec_version: String,
    pub manifest_path: PathBuf,
    pub requested_cwd: Option<String>,
    pub env: Vec<(String, String)>,
    pub ipc_socket_paths: Vec<PathBuf>,
    pub injected_mounts: Vec<InjectedMount>,
    pub overlays: Vec<OverlayMountSpec>,
    pub derived_outputs: Vec<DerivedOutputMountSpec>,
    pub runtime_artifacts: Vec<RuntimeArtifactReference>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupPolicyApplied {
    Preserve,
    DeleteWorkspacePreserveOutputs,
}

impl CleanupPolicyApplied {
    pub fn as_str(self) -> &'static str {
        match self {
            CleanupPolicyApplied::Preserve => "preserve",
            CleanupPolicyApplied::DeleteWorkspacePreserveOutputs => {
                "delete_workspace_preserve_outputs"
            }
        }
    }
}

#[derive(Debug, Clone)]
struct PreparedDerivedOutput {
    host_path: PathBuf,
    kind: String,
    staged_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct EnvironmentWorkspace {
    pub run_id: String,
    pub spec_version: String,
    pub manifest_path: PathBuf,
    pub source_dir: PathBuf,
    pub requested_cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub ipc_socket_paths: Vec<PathBuf>,
    pub injected_mounts: Vec<InjectedMount>,
    pub log_dir: PathBuf,
    pub state_dir: PathBuf,
    cleanup_paths: Vec<PathBuf>,
    cleanup_policy: CleanupPolicyApplied,
    derived_outputs: Vec<PreparedDerivedOutput>,
}

impl EnvironmentWorkspace {
    pub fn for_manifest(
        run_id: String,
        spec_version: String,
        manifest_path: PathBuf,
        requested_cwd: Option<PathBuf>,
        env: Vec<(String, String)>,
        ipc_socket_paths: Vec<PathBuf>,
        injected_mounts: Vec<InjectedMount>,
    ) -> Result<Self> {
        let manifest_path = canonical_manifest_path(&manifest_path)?;
        let source_dir = manifest_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let runtime_dirs = allocate_runtime_dirs(&run_id)?;

        Ok(Self {
            run_id,
            spec_version,
            manifest_path,
            source_dir,
            requested_cwd,
            env,
            ipc_socket_paths,
            injected_mounts,
            log_dir: runtime_dirs.log_dir,
            state_dir: runtime_dirs.state_dir.clone(),
            cleanup_paths: vec![runtime_dirs.state_dir],
            cleanup_policy: CleanupPolicyApplied::Preserve,
            derived_outputs: vec![],
        })
    }

    pub fn runtime_config(&self, dev_mode: bool) -> SourceRuntimeConfig {
        SourceRuntimeConfig {
            dev_mode,
            log_dir: self.log_dir.clone(),
            state_dir: self.state_dir.clone(),
            sidecar_config: None,
        }
    }

    pub fn cleanup_policy(&self) -> CleanupPolicyApplied {
        self.cleanup_policy
    }

    pub fn primary_derived_output_path(&self) -> Option<String> {
        self.derived_outputs
            .first()
            .map(|output| output.host_path.display().to_string())
    }

    pub fn exported_artifacts(&self) -> Result<Vec<ExportedArtifact>> {
        let mut artifacts = Vec::new();
        for output in &self.derived_outputs {
            collect_output_path_artifacts(
                &output.host_path,
                &output.host_path,
                &output.kind,
                &mut artifacts,
            )?;
        }
        Ok(artifacts)
    }

    pub fn sync_derived_outputs(&self) -> Result<()> {
        for output in &self.derived_outputs {
            if let Some(staged_path) = output.staged_path.as_ref() {
                remove_path_if_exists(&output.host_path)?;
                if staged_path.exists() {
                    if staged_path.is_dir() {
                        copy_tree(staged_path, &output.host_path, false)?;
                    } else {
                        copy_file_with_strategy(staged_path, &output.host_path, false)?;
                    }
                }
            }
        }
        Ok(())
    }

    pub fn cleanup(&self) {
        for path in &self.cleanup_paths {
            let _ = remove_path_if_exists(path);
        }
    }
}

pub trait EnvironmentBuilder {
    fn prepare(&self, request: EnvironmentPrepareRequest) -> Result<EnvironmentWorkspace>;
}

pub fn prepare_environment(request: EnvironmentPrepareRequest) -> Result<EnvironmentWorkspace> {
    let builder = PlatformEnvironmentBuilder;
    builder.prepare(request)
}

#[cfg(target_os = "linux")]
struct PlatformEnvironmentBuilder;

#[cfg(target_os = "macos")]
struct PlatformEnvironmentBuilder;

#[cfg(target_os = "windows")]
struct PlatformEnvironmentBuilder;

#[cfg(target_os = "linux")]
impl EnvironmentBuilder for PlatformEnvironmentBuilder {
    fn prepare(&self, request: EnvironmentPrepareRequest) -> Result<EnvironmentWorkspace> {
        let mut workspace = EnvironmentWorkspace::for_manifest(
            request.run_id,
            request.spec_version,
            request.manifest_path,
            request.requested_cwd.map(PathBuf::from),
            apply_runtime_artifact_env(request.env, &request.runtime_artifacts)?,
            request.ipc_socket_paths,
            request.injected_mounts,
        )?;

        for overlay in request.overlays {
            if !overlay.source.exists() {
                anyhow::bail!("overlay source not found: {}", overlay.source.display());
            }
            let relative_target = normalize_workspace_target(&overlay.target)?;
            workspace.injected_mounts.push(InjectedMount {
                source: overlay.source,
                target: guest_workspace_target(&relative_target),
                readonly: overlay.readonly,
            });
        }

        for output in request.derived_outputs {
            let host_path = ensure_derived_output_root(&output.host_path, &workspace.source_dir)?;
            let relative_target = normalize_workspace_target(&output.target)?;
            workspace.injected_mounts.push(InjectedMount {
                source: host_path.clone(),
                target: guest_workspace_target(&relative_target),
                readonly: false,
            });
            workspace.derived_outputs.push(PreparedDerivedOutput {
                host_path,
                kind: output.kind,
                staged_path: None,
            });
        }

        workspace.requested_cwd = workspace
            .requested_cwd
            .take()
            .map(|cwd| normalize_workspace_target(&cwd))
            .transpose()?
            .map(|path| guest_workspace_target(&path));

        Ok(workspace)
    }
}

#[cfg(target_os = "macos")]
impl EnvironmentBuilder for PlatformEnvironmentBuilder {
    fn prepare(&self, request: EnvironmentPrepareRequest) -> Result<EnvironmentWorkspace> {
        let manifest_path = canonical_manifest_path(&request.manifest_path)?;
        let source_dir = manifest_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let manifest_relative = manifest_path
            .strip_prefix(&source_dir)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| PathBuf::from("capsule.toml"));

        let runtime_dirs = allocate_runtime_dirs(&request.run_id)?;
        let workspace_root = runtime_dirs._workspace_root.join("workspace");
        let use_clonefile = is_apfs_filesystem(&source_dir).unwrap_or(false);
        copy_tree(&source_dir, &workspace_root, use_clonefile)?;

        let mut workspace = EnvironmentWorkspace {
            run_id: request.run_id,
            spec_version: request.spec_version,
            manifest_path: workspace_root.join(manifest_relative),
            source_dir: workspace_root.clone(),
            requested_cwd: request
                .requested_cwd
                .as_deref()
                .map(Path::new)
                .map(normalize_workspace_target)
                .transpose()?
                .map(|path| workspace_root.join(path))
                .or_else(|| Some(workspace_root.clone())),
            env: apply_runtime_artifact_env(request.env, &request.runtime_artifacts)?,
            ipc_socket_paths: request.ipc_socket_paths,
            injected_mounts: request.injected_mounts,
            log_dir: runtime_dirs.log_dir,
            state_dir: runtime_dirs.state_dir.clone(),
            cleanup_paths: vec![runtime_dirs._workspace_root, runtime_dirs.state_dir],
            cleanup_policy: CleanupPolicyApplied::DeleteWorkspacePreserveOutputs,
            derived_outputs: vec![],
        };

        for (index, overlay) in request.overlays.into_iter().enumerate() {
            if !overlay.source.exists() {
                anyhow::bail!("overlay source not found: {}", overlay.source.display());
            }

            let relative_target = normalize_workspace_target(&overlay.target)?;
            let target_path = workspace_root.join(&relative_target);
            remove_path_if_exists(&target_path)?;

            if let Some(mode) = overlay.mode {
                if overlay.source.is_dir() {
                    anyhow::bail!("overlay mode override is only supported for files");
                }

                let overlay_cache_dir = workspace_root.join(".nacelle-overlays");
                fs::create_dir_all(&overlay_cache_dir).with_context(|| {
                    format!(
                        "Failed to create overlay cache directory: {}",
                        overlay_cache_dir.display()
                    )
                })?;
                let materialized_overlay = overlay_cache_dir.join(format!("overlay-{index}"));
                copy_file_with_strategy(&overlay.source, &materialized_overlay, use_clonefile)?;
                use std::os::unix::fs::PermissionsExt;
                let mut permissions = fs::metadata(&materialized_overlay)
                    .with_context(|| {
                        format!(
                            "Failed to stat materialized overlay: {}",
                            materialized_overlay.display()
                        )
                    })?
                    .permissions();
                permissions.set_mode(mode);
                fs::set_permissions(&materialized_overlay, permissions).with_context(|| {
                    format!(
                        "Failed to set overlay mode on {}",
                        materialized_overlay.display()
                    )
                })?;
                create_workspace_symlink(&materialized_overlay, &target_path)?;
            } else {
                create_workspace_symlink(&overlay.source, &target_path)?;
            }
        }

        for output in request.derived_outputs {
            let host_path = ensure_derived_output_root(&output.host_path, &source_dir)?;
            let relative_target = normalize_workspace_target(&output.target)?;
            let target_path = workspace_root.join(&relative_target);
            remove_path_if_exists(&target_path)?;
            create_workspace_symlink(&host_path, &target_path)?;
            workspace.derived_outputs.push(PreparedDerivedOutput {
                host_path,
                kind: output.kind,
                staged_path: None,
            });
        }

        set_tree_readonly(&workspace_root)?;
        Ok(workspace)
    }
}

#[cfg(target_os = "windows")]
impl EnvironmentBuilder for PlatformEnvironmentBuilder {
    fn prepare(&self, request: EnvironmentPrepareRequest) -> Result<EnvironmentWorkspace> {
        let manifest_path = canonical_manifest_path(&request.manifest_path)?;
        let source_dir = manifest_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let manifest_relative = manifest_path
            .strip_prefix(&source_dir)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| PathBuf::from("capsule.toml"));

        let runtime_dirs = allocate_runtime_dirs(&request.run_id)?;
        let workspace_root = runtime_dirs._workspace_root.join("workspace");
        copy_tree(&source_dir, &workspace_root, false)?;

        let mut workspace = EnvironmentWorkspace {
            run_id: request.run_id,
            spec_version: request.spec_version,
            manifest_path: workspace_root.join(manifest_relative),
            source_dir: workspace_root.clone(),
            requested_cwd: request
                .requested_cwd
                .as_deref()
                .map(Path::new)
                .map(normalize_workspace_target)
                .transpose()?
                .map(|path| workspace_root.join(path))
                .or_else(|| Some(workspace_root.clone())),
            env: apply_runtime_artifact_env(request.env, &request.runtime_artifacts)?,
            ipc_socket_paths: request.ipc_socket_paths,
            injected_mounts: request.injected_mounts,
            log_dir: runtime_dirs.log_dir,
            state_dir: runtime_dirs.state_dir.clone(),
            cleanup_paths: vec![runtime_dirs._workspace_root, runtime_dirs.state_dir],
            cleanup_policy: CleanupPolicyApplied::DeleteWorkspacePreserveOutputs,
            derived_outputs: vec![],
        };
        workspace
            .env
            .push(("NACELLE_WORKSPACE_WRITABLE".to_string(), "1".to_string()));

        for overlay in request.overlays {
            if !overlay.source.exists() {
                anyhow::bail!("overlay source not found: {}", overlay.source.display());
            }
            let relative_target = normalize_workspace_target(&overlay.target)?;
            let target_path = workspace_root.join(&relative_target);
            remove_path_if_exists(&target_path)?;
            if overlay.source.is_dir() {
                copy_tree(&overlay.source, &target_path, false)?;
            } else {
                copy_file_with_strategy(&overlay.source, &target_path, false)?;
            }
        }

        for output in request.derived_outputs {
            let host_path = ensure_derived_output_root(&output.host_path, &source_dir)?;
            let relative_target = normalize_workspace_target(&output.target)?;
            let staged_path = workspace_root.join(&relative_target);
            fs::create_dir_all(&staged_path).with_context(|| {
                format!(
                    "Failed to create staged output directory: {}",
                    staged_path.display()
                )
            })?;
            workspace.derived_outputs.push(PreparedDerivedOutput {
                host_path,
                kind: output.kind,
                staged_path: Some(staged_path),
            });
        }

        Ok(workspace)
    }
}

struct RuntimeDirs {
    _workspace_root: PathBuf,
    log_dir: PathBuf,
    state_dir: PathBuf,
}

fn allocate_runtime_dirs(run_id: &str) -> Result<RuntimeDirs> {
    let base_root = std::env::temp_dir().join("nacelle-runs").join(run_id);
    let workspace_root = base_root.join(format!(
        "workspace-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let log_dir = base_root.join("logs");
    let state_dir = base_root.join("state");

    fs::create_dir_all(&workspace_root).with_context(|| {
        format!(
            "Failed to create workspace root: {}",
            workspace_root.display()
        )
    })?;
    fs::create_dir_all(&log_dir)
        .with_context(|| format!("Failed to create log dir: {}", log_dir.display()))?;
    fs::create_dir_all(&state_dir)
        .with_context(|| format!("Failed to create state dir: {}", state_dir.display()))?;

    Ok(RuntimeDirs {
        _workspace_root: workspace_root,
        log_dir,
        state_dir,
    })
}

fn canonical_manifest_path(manifest_path: &Path) -> Result<PathBuf> {
    if !manifest_path.exists() {
        anyhow::bail!("manifest not found: {}", manifest_path.display());
    }

    manifest_path.canonicalize().with_context(|| {
        format!(
            "Failed to canonicalize manifest: {}",
            manifest_path.display()
        )
    })
}

fn normalize_workspace_target(path: &Path) -> Result<PathBuf> {
    let relative = if path.is_absolute() {
        path.strip_prefix("/")
            .map_err(|_| anyhow::anyhow!("target must be under workspace root"))?
    } else {
        path
    };

    let mut normalized = PathBuf::new();
    for component in relative.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!("target escapes workspace root: {}", path.display())
            }
        }
    }

    Ok(normalized)
}

#[cfg(target_os = "linux")]
fn guest_workspace_target(path: &Path) -> PathBuf {
    if path.as_os_str().is_empty() {
        PathBuf::from("/app")
    } else {
        PathBuf::from("/app").join(path)
    }
}

fn ensure_derived_output_root(output: &Path, lower_source_root: &Path) -> Result<PathBuf> {
    let output_path = if output.is_absolute() {
        output.to_path_buf()
    } else {
        std::env::current_dir()
            .context("Failed to resolve current directory")?
            .join(output)
    };

    if output_path.starts_with(lower_source_root) {
        anyhow::bail!(
            "derived output path must not be inside lower_source: {}",
            output_path.display()
        );
    }

    fs::create_dir_all(&output_path).with_context(|| {
        format!(
            "Failed to create derived output directory: {}",
            output_path.display()
        )
    })?;
    Ok(output_path)
}

fn apply_runtime_artifact_env(
    mut env: Vec<(String, String)>,
    runtime_artifacts: &[RuntimeArtifactReference],
) -> Result<Vec<(String, String)>> {
    let mut path_prefixes = Vec::new();

    for artifact in runtime_artifacts {
        if !artifact.path.exists() {
            anyhow::bail!(
                "runtime artifact not found: {} ({})",
                artifact.name,
                artifact.path.display()
            );
        }

        let env_var = artifact.env_var.clone().unwrap_or_else(|| {
            format!(
                "NACELLE_RUNTIME_ARTIFACT_{}",
                artifact
                    .name
                    .chars()
                    .map(|ch| if ch.is_ascii_alphanumeric() {
                        ch.to_ascii_uppercase()
                    } else {
                        '_'
                    })
                    .collect::<String>()
            )
        });
        env.push((env_var, artifact.path.display().to_string()));

        if artifact.add_to_path {
            let path_entry = if artifact.path.is_dir() {
                artifact.path.clone()
            } else {
                artifact
                    .path
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| artifact.path.clone())
            };
            if !path_prefixes.contains(&path_entry) {
                path_prefixes.push(path_entry);
            }
        }
    }

    if !path_prefixes.is_empty() {
        let existing_path = env
            .iter()
            .rev()
            .find(|(key, _)| key == "PATH")
            .map(|(_, value)| value.clone())
            .unwrap_or_else(|| std::env::var("PATH").unwrap_or_default());

        let mut joined_paths = path_prefixes;
        joined_paths.extend(std::env::split_paths(&existing_path));
        let joined = std::env::join_paths(joined_paths)
            .context("Failed to assemble PATH from runtime artifacts")?
            .to_string_lossy()
            .to_string();

        env.retain(|(key, _)| key != "PATH");
        env.push(("PATH".to_string(), joined));
    }

    Ok(env)
}

fn remove_path_if_exists(path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(err).with_context(|| format!("Failed to inspect path: {}", path.display()))
        }
    };

    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)
            .with_context(|| format!("Failed to remove directory: {}", path.display()))?;
    } else {
        fs::remove_file(path)
            .with_context(|| format!("Failed to remove file: {}", path.display()))?;
    }

    Ok(())
}

fn copy_tree(source: &Path, destination: &Path, use_clonefile: bool) -> Result<()> {
    fs::create_dir_all(destination).with_context(|| {
        format!(
            "Failed to create destination directory: {}",
            destination.display()
        )
    })?;

    for entry in fs::read_dir(source)
        .with_context(|| format!("Failed to read directory: {}", source.display()))?
    {
        let entry =
            entry.with_context(|| format!("Failed to read entry in {}", source.display()))?;
        let file_type = entry
            .file_type()
            .with_context(|| format!("Failed to inspect entry type: {}", entry.path().display()))?;
        let target_path = destination.join(entry.file_name());

        if file_type.is_dir() {
            copy_tree(&entry.path(), &target_path, use_clonefile)?;
            continue;
        }

        if file_type.is_file() {
            copy_file_with_strategy(&entry.path(), &target_path, use_clonefile)?;
        }
    }

    Ok(())
}

fn copy_file_with_strategy(source: &Path, destination: &Path, _use_clonefile: bool) -> Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create parent directory: {}", parent.display()))?;
    }

    #[cfg(target_os = "macos")]
    if _use_clonefile && try_clonefile(source, destination).is_ok() {
        return Ok(());
    }

    fs::copy(source, destination).with_context(|| {
        format!(
            "Failed to copy file from {} to {}",
            source.display(),
            destination.display()
        )
    })?;

    let permissions = fs::metadata(source)
        .with_context(|| format!("Failed to stat file: {}", source.display()))?
        .permissions();
    fs::set_permissions(destination, permissions).with_context(|| {
        format!(
            "Failed to preserve file permissions for {}",
            destination.display()
        )
    })?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn is_apfs_filesystem(path: &Path) -> Result<bool> {
    use std::ffi::CStr;
    use std::mem::MaybeUninit;
    use std::os::unix::ffi::OsStrExt;

    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| anyhow::anyhow!("invalid path for APFS detection"))?;
    let mut stat = MaybeUninit::<libc::statfs>::zeroed();
    let result = unsafe { libc::statfs(c_path.as_ptr(), stat.as_mut_ptr()) };
    if result != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("Failed to detect filesystem for {}", path.display()));
    }

    let stat = unsafe { stat.assume_init() };
    let filesystem_name = unsafe { CStr::from_ptr(stat.f_fstypename.as_ptr()) }
        .to_string_lossy()
        .to_ascii_lowercase();
    Ok(filesystem_name == "apfs")
}

#[cfg(target_os = "macos")]
fn try_clonefile(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let src = CString::new(source.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid source path")
    })?;
    let dst = CString::new(destination.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid destination path")
    })?;

    let result = unsafe { libc::clonefile(src.as_ptr(), dst.as_ptr(), 0) };
    if result == 0 {
        return Ok(());
    }

    Err(std::io::Error::last_os_error())
}

#[cfg(target_os = "macos")]
fn create_workspace_symlink(source: &Path, target: &Path) -> Result<()> {
    use std::os::unix::fs::symlink;

    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create parent directory: {}", parent.display()))?;
    }
    symlink(source, target).with_context(|| {
        format!(
            "Failed to create symlink from {} to {}",
            target.display(),
            source.display()
        )
    })?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn set_tree_readonly(root: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    for entry in fs::read_dir(root)
        .with_context(|| format!("Failed to read directory: {}", root.display()))?
    {
        let entry = entry.with_context(|| format!("Failed to read entry in {}", root.display()))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("Failed to inspect path: {}", path.display()))?;

        if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
            set_tree_readonly(&path)?;
        }

        if metadata.file_type().is_symlink() {
            continue;
        }

        let mut permissions = metadata.permissions();
        let mode = permissions.mode();
        permissions.set_mode(mode & 0o555);
        fs::set_permissions(&path, permissions)
            .with_context(|| format!("Failed to set readonly permissions on {}", path.display()))?;
    }

    let mut root_permissions = fs::metadata(root)
        .with_context(|| format!("Failed to stat workspace root: {}", root.display()))?
        .permissions();
    root_permissions.set_mode(root_permissions.mode() & 0o555);
    fs::set_permissions(root, root_permissions).with_context(|| {
        format!(
            "Failed to set readonly permissions on workspace root: {}",
            root.display()
        )
    })?;
    Ok(())
}

fn collect_output_path_artifacts(
    root: &Path,
    path: &Path,
    kind: &str,
    artifacts: &mut Vec<ExportedArtifact>,
) -> Result<()> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("Failed to read output metadata: {}", path.display()))
        }
    };

    if metadata.is_dir() {
        for entry in fs::read_dir(path)
            .with_context(|| format!("Failed to read output directory: {}", path.display()))?
        {
            let entry =
                entry.with_context(|| format!("Failed to read entry in {}", path.display()))?;
            collect_output_path_artifacts(root, &entry.path(), kind, artifacts)?;
        }
        return Ok(());
    }

    let relative_path = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();
    artifacts.push(ExportedArtifact {
        kind: kind.to_string(),
        relative_path,
        size_bytes: metadata.len(),
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_manifest(root: &Path) -> PathBuf {
        let manifest_path = root.join("capsule.toml");
        fs::write(
            &manifest_path,
            r#"name = "env-test"
version = "0.1.0"

[execution]
entrypoint = "python3 main.py"

[isolation]
sandbox = false
"#,
        )
        .unwrap();
        manifest_path
    }

    #[test]
    fn normalize_workspace_target_rejects_parent_escape() {
        let err = normalize_workspace_target(Path::new("../escape")).unwrap_err();
        assert!(err.to_string().contains("target escapes workspace root"));
    }

    #[test]
    fn apply_runtime_artifact_env_adds_env_var_and_path() {
        let temp_dir = TempDir::new().unwrap();
        let artifact_dir = temp_dir.path().join("toolchain");
        let bin_dir = artifact_dir.join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let runtime_binary = bin_dir.join("python3");
        fs::write(&runtime_binary, "#!/usr/bin/env python3\n").unwrap();

        let env = apply_runtime_artifact_env(
            vec![("PATH".to_string(), "/usr/bin".to_string())],
            &[RuntimeArtifactReference {
                name: "python".to_string(),
                path: runtime_binary.clone(),
                env_var: Some("PYTHON_RUNTIME".to_string()),
                add_to_path: true,
            }],
        )
        .unwrap();

        assert!(env.iter().any(|(key, value)| {
            key == "PYTHON_RUNTIME" && value == &runtime_binary.display().to_string()
        }));
        let path_value = env
            .iter()
            .find(|(key, _)| key == "PATH")
            .map(|(_, value)| value.clone())
            .unwrap();
        let joined_paths = std::env::split_paths(&path_value).collect::<Vec<_>>();
        assert_eq!(joined_paths.first().unwrap(), &bin_dir);
    }

    #[test]
    fn environment_workspace_exports_relative_artifacts() {
        let temp_dir = TempDir::new().unwrap();
        let lower_root = temp_dir.path().join("lower");
        fs::create_dir_all(&lower_root).unwrap();
        let manifest_path = write_manifest(&lower_root);
        let derived_root = temp_dir.path().join("derived");
        fs::create_dir_all(derived_root.join("nested")).unwrap();
        fs::write(derived_root.join("nested").join("result.txt"), "ok").unwrap();

        let mut workspace = EnvironmentWorkspace::for_manifest(
            "run-artifacts".to_string(),
            "2.0".to_string(),
            manifest_path,
            None,
            vec![],
            vec![],
            vec![],
        )
        .unwrap();
        workspace.derived_outputs.push(PreparedDerivedOutput {
            host_path: derived_root,
            kind: "artifact".to_string(),
            staged_path: None,
        });

        let exported = workspace.exported_artifacts().unwrap();
        assert_eq!(exported.len(), 1);
        assert_eq!(exported[0].kind, "artifact");
        assert_eq!(exported[0].relative_path, "nested/result.txt");
        assert_eq!(
            workspace.primary_derived_output_path(),
            Some(temp_dir.path().join("derived").display().to_string())
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_prepare_environment_materializes_overlay_and_derived_output() {
        let temp_dir = TempDir::new().unwrap();
        let lower_root = temp_dir.path().join("lower");
        let overlay_root = temp_dir.path().join("overlays");
        let derived_root = temp_dir.path().join("derived");
        fs::create_dir_all(&lower_root).unwrap();
        fs::create_dir_all(&overlay_root).unwrap();
        let manifest_path = write_manifest(&lower_root);
        let overlay_file = overlay_root.join(".env");
        fs::write(&overlay_file, "KEY=value\n").unwrap();

        let workspace = prepare_environment(EnvironmentPrepareRequest {
            run_id: "macos-prepare".to_string(),
            spec_version: "2.0".to_string(),
            manifest_path,
            requested_cwd: Some("src".to_string()),
            env: vec![],
            ipc_socket_paths: vec![],
            injected_mounts: vec![],
            overlays: vec![OverlayMountSpec {
                source: overlay_file.clone(),
                target: PathBuf::from(".env"),
                readonly: true,
                mode: None,
            }],
            derived_outputs: vec![DerivedOutputMountSpec {
                host_path: derived_root.clone(),
                target: PathBuf::from(".derived"),
                kind: "artifact".to_string(),
            }],
            runtime_artifacts: vec![],
        })
        .unwrap();

        assert_eq!(
            workspace.cleanup_policy(),
            CleanupPolicyApplied::DeleteWorkspacePreserveOutputs
        );
        assert!(workspace.source_dir.join(".env").exists());
        assert_eq!(
            fs::read_link(workspace.source_dir.join(".env")).unwrap(),
            overlay_file
        );
        assert_eq!(
            fs::read_link(workspace.source_dir.join(".derived")).unwrap(),
            derived_root
        );
        assert!(workspace
            .requested_cwd
            .as_ref()
            .unwrap()
            .starts_with(&workspace.source_dir));
        workspace.cleanup();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_prepare_environment_maps_overlays_into_guest_paths() {
        let temp_dir = TempDir::new().unwrap();
        let lower_root = temp_dir.path().join("lower");
        let overlay_root = temp_dir.path().join("overlays");
        let derived_root = temp_dir.path().join("derived");
        fs::create_dir_all(&lower_root).unwrap();
        fs::create_dir_all(&overlay_root).unwrap();
        let manifest_path = write_manifest(&lower_root);
        let overlay_file = overlay_root.join("settings.json");
        fs::write(&overlay_file, "{}\n").unwrap();

        let workspace = prepare_environment(EnvironmentPrepareRequest {
            run_id: "linux-prepare".to_string(),
            spec_version: "2.0".to_string(),
            manifest_path,
            requested_cwd: Some("src".to_string()),
            env: vec![],
            ipc_socket_paths: vec![],
            injected_mounts: vec![],
            overlays: vec![OverlayMountSpec {
                source: overlay_file.clone(),
                target: PathBuf::from("config/settings.json"),
                readonly: true,
                mode: None,
            }],
            derived_outputs: vec![DerivedOutputMountSpec {
                host_path: derived_root.clone(),
                target: PathBuf::from(".derived"),
                kind: "artifact".to_string(),
            }],
            runtime_artifacts: vec![],
        })
        .unwrap();

        assert_eq!(workspace.cleanup_policy(), CleanupPolicyApplied::Preserve);
        assert!(workspace
            .injected_mounts
            .iter()
            .any(|mount| mount.source == overlay_file
                && mount.target == Path::new("/app/config/settings.json")));
        assert!(workspace
            .injected_mounts
            .iter()
            .any(|mount| mount.source == derived_root
                && mount.target == Path::new("/app/.derived")
                && !mount.readonly));
        assert_eq!(workspace.requested_cwd, Some(PathBuf::from("/app/src")));
        workspace.cleanup();
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_prepare_environment_stages_outputs_and_marks_workspace_writable() {
        let temp_dir = TempDir::new().unwrap();
        let lower_root = temp_dir.path().join("lower");
        let overlay_root = temp_dir.path().join("overlays");
        let derived_root = temp_dir.path().join("derived");
        fs::create_dir_all(&lower_root).unwrap();
        fs::create_dir_all(&overlay_root).unwrap();
        let manifest_path = write_manifest(&lower_root);
        let overlay_file = overlay_root.join("settings.json");
        fs::write(&overlay_file, "{}\n").unwrap();

        let workspace = prepare_environment(EnvironmentPrepareRequest {
            run_id: "windows-prepare".to_string(),
            spec_version: "2.0".to_string(),
            manifest_path,
            requested_cwd: Some("src".to_string()),
            env: vec![],
            ipc_socket_paths: vec![],
            injected_mounts: vec![],
            overlays: vec![OverlayMountSpec {
                source: overlay_file.clone(),
                target: PathBuf::from("config/settings.json"),
                readonly: true,
                mode: None,
            }],
            derived_outputs: vec![DerivedOutputMountSpec {
                host_path: derived_root.clone(),
                target: PathBuf::from(".derived"),
                kind: "artifact".to_string(),
            }],
            runtime_artifacts: vec![],
        })
        .unwrap();

        assert!(workspace
            .env
            .iter()
            .any(|(key, value)| key == "NACELLE_WORKSPACE_WRITABLE" && value == "1"));
        assert!(workspace
            .source_dir
            .join("config")
            .join("settings.json")
            .exists());
        assert!(workspace.derived_outputs.iter().any(|output| output
            .staged_path
            .as_ref()
            .is_some_and(|path| path.ends_with(".derived"))));
        workspace.cleanup();
    }
}
