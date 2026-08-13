//! Repository authoring adapter.
//!
//! Files, ecosystem metadata, and optional `capsule.toml` syntax are evidence
//! used to compile one canonical `ato.workspace@1` computation. They are not
//! part of the semantic core and are compiled away here.

#![forbid(unsafe_code)]

mod resolution;

pub use resolution::*;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use ato_computation::{Boundary, ComputationObject, ContentRef, SemanticsId};
use ato_objects::{ObjectError, ObjectStore};
use ato_semantics_workspace::{
    RealizationConstraint, SourceClosure, SourceEntry, SourceEntryKind, ToolchainConstraint,
    WORKSPACE_SEMANTICS_ID, WorkspacePhase, WorkspaceResidual, encode_workspace_residual,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use walkdir::WalkDir;

const MAX_SOURCE_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SOURCE_CLOSURE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryOptions {
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub secret_bindings: BTreeMap<String, String>,
    pub network_allow: Vec<String>,
    pub writable_paths: Vec<String>,
    pub sandbox_required: bool,
}

impl Default for RepositoryOptions {
    fn default() -> Self {
        Self {
            arguments: Vec::new(),
            environment: BTreeMap::new(),
            secret_bindings: BTreeMap::new(),
            network_allow: Vec::new(),
            writable_paths: vec![".ato".to_owned()],
            sandbox_required: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledRepository {
    pub computation: ComputationObject,
    pub source: ContentRef,
    pub evidence: InferenceEvidence,
    pub repository_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InferenceEvidence {
    pub observed_files: Vec<String>,
    pub selected_toolchain: String,
    pub selected_entrypoint: Vec<String>,
    pub authoring_manifest_used: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryInference {
    pub toolchain: ToolchainConstraint,
    pub entrypoint: Vec<String>,
    pub package_manager: Option<String>,
    pub candidate_id: String,
    pub authoring_manifest_used: bool,
}

/// Inspect repository authoring evidence without producing an execution plan.
/// The returned candidate is the same concrete workspace choice used by
/// [`compile_repository`].
pub fn inspect_repository(root: &Path) -> Result<RepositoryInference, RepositoryError> {
    if !root.is_dir() {
        return Err(RepositoryError::NotDirectory(root.to_path_buf()));
    }
    let authoring = load_authoring(root)?;
    let inferred = infer_workspace(root, authoring.as_ref())?;
    #[derive(Serialize)]
    struct CandidateIdentity<'a> {
        toolchain: &'a ToolchainConstraint,
        entrypoint: &'a [String],
        package_manager: &'a Option<String>,
        working_directory: &'a str,
    }
    let canonical = serde_jcs::to_vec(&CandidateIdentity {
        toolchain: &inferred.toolchain,
        entrypoint: &inferred.entrypoint,
        package_manager: &inferred.package_manager,
        working_directory: &inferred.working_directory,
    })?;
    Ok(RepositoryInference {
        toolchain: inferred.toolchain,
        entrypoint: inferred.entrypoint,
        package_manager: inferred.package_manager,
        candidate_id: format!("blake3:{}", blake3::hash(&canonical).to_hex()),
        authoring_manifest_used: authoring.is_some(),
    })
}

#[derive(Debug, Error)]
pub enum RepositoryError {
    #[error("repository path is not a directory: {0}")]
    NotDirectory(PathBuf),
    #[error("repository I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("repository walk failed: {0}")]
    Walk(#[from] walkdir::Error),
    #[error("repository object persistence failed: {0}")]
    Objects(#[from] ObjectError),
    #[error("repository authoring input is invalid: {0}")]
    Authoring(String),
    #[error("repository source file exceeds 64 MiB: {0}")]
    SourceFileTooLarge(PathBuf),
    #[error("repository source closure exceeds 512 MiB")]
    SourceClosureTooLarge,
    #[error("repository symlink escapes the source boundary: {0}")]
    EscapingSymlink(PathBuf),
    #[error("repository type is ambiguous or unsupported; add [workspace] to capsule.toml")]
    Unsupported,
    #[error("workspace residual encoding failed: {0}")]
    Workspace(String),
    #[error("source closure encoding failed: {0}")]
    Closure(#[from] serde_json::Error),
    #[error("git source fetch failed: {0}")]
    Git(String),
}

pub fn compile_repository(
    root: &Path,
    objects: &dyn ObjectStore,
    mut options: RepositoryOptions,
) -> Result<CompiledRepository, RepositoryError> {
    if !root.is_dir() {
        return Err(RepositoryError::NotDirectory(root.to_path_buf()));
    }
    options.network_allow.sort();
    options.network_allow.dedup();
    options.writable_paths.sort();
    options.writable_paths.dedup();

    let (source, observed_files) = seal_source(root, objects)?;
    let authoring = load_authoring(root)?;
    let inferred = infer_workspace(root, authoring.as_ref())?;
    let mut entrypoint = inferred.entrypoint.clone();
    entrypoint.extend(options.arguments);
    let residual = WorkspaceResidual {
        source: source.as_str().to_owned(),
        toolchain: inferred.toolchain.clone(),
        package_manager: inferred.package_manager,
        entrypoint,
        working_directory: inferred.working_directory,
        environment: options.environment,
        secret_bindings: options.secret_bindings,
        realization: RealizationConstraint {
            network_allow: options.network_allow,
            writable_paths: options.writable_paths,
            sandbox_required: options.sandbox_required,
        },
        phase: WorkspacePhase::Ready,
    };
    let residual_bytes = encode_workspace_residual(&residual)
        .map_err(|error| RepositoryError::Workspace(error.to_string()))?;
    let residual = objects.put(&residual_bytes)?;
    Ok(CompiledRepository {
        computation: ComputationObject {
            semantics: SemanticsId::parse(WORKSPACE_SEMANTICS_ID)
                .expect("static workspace semantics id is valid"),
            boundary: Boundary::new(),
            residual,
        },
        source,
        evidence: InferenceEvidence {
            observed_files,
            selected_toolchain: inferred.toolchain.family,
            selected_entrypoint: inferred.entrypoint,
            authoring_manifest_used: authoring.is_some(),
        },
        repository_root: root.to_path_buf(),
    })
}

pub fn fetch_git_repository(source: &str, destination: &Path) -> Result<(), RepositoryError> {
    if destination.exists() {
        return Err(RepositoryError::Git(format!(
            "destination already exists: {}",
            destination.display()
        )));
    }
    let status = Command::new("git")
        .args(["clone", "--depth", "1", "--", source])
        .arg(destination)
        .status()?;
    if !status.success() {
        return Err(RepositoryError::Git(format!(
            "git clone exited with {status}"
        )));
    }
    Ok(())
}

fn seal_source(
    root: &Path,
    objects: &dyn ObjectStore,
) -> Result<(ContentRef, Vec<String>), RepositoryError> {
    let mut entries = Vec::new();
    let mut observed = Vec::new();
    let mut total = 0_u64;
    for item in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| !ignored(entry.path(), root))
    {
        let item = item?;
        if item.file_type().is_dir() {
            continue;
        }
        let relative = item.path().strip_prefix(root).expect("walk root prefix");
        let path = portable_path(relative)?;
        let metadata = fs::symlink_metadata(item.path())?;
        let bytes = if metadata.file_type().is_symlink() {
            let target = fs::read_link(item.path())?;
            validate_symlink(&target, item.path())?;
            target.to_string_lossy().as_bytes().to_vec()
        } else {
            if metadata.len() > MAX_SOURCE_FILE_BYTES {
                return Err(RepositoryError::SourceFileTooLarge(
                    item.path().to_path_buf(),
                ));
            }
            fs::read(item.path())?
        };
        total = total.saturating_add(bytes.len() as u64);
        if total > MAX_SOURCE_CLOSURE_BYTES {
            return Err(RepositoryError::SourceClosureTooLarge);
        }
        let content = objects.put(&bytes)?;
        observed.push(path.clone());
        entries.push(SourceEntry {
            path,
            content: content.as_str().to_owned(),
            executable: executable(&metadata),
            kind: if metadata.file_type().is_symlink() {
                SourceEntryKind::Symlink
            } else {
                SourceEntryKind::File
            },
        });
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    observed.sort();
    let closure = serde_jcs::to_vec(&SourceClosure { entries })?;
    let source = objects.put(&closure)?;
    Ok((source, observed))
}

pub fn materialize_source(
    source: &ContentRef,
    objects: &dyn ato_objects::ObjectResolver,
    destination: &Path,
) -> Result<(), RepositoryError> {
    if destination.exists() {
        return Err(RepositoryError::Authoring(format!(
            "materialization destination already exists: {}",
            destination.display()
        )));
    }
    let metadata = objects.metadata(source)?;
    let bytes =
        ato_objects::read_exact_object(objects, source, metadata.size, MAX_SOURCE_FILE_BYTES)?;
    let closure: SourceClosure = serde_json::from_slice(&bytes)?;
    fs::create_dir_all(destination)?;
    for entry in closure.entries {
        let relative = Path::new(&entry.path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(RepositoryError::Authoring(format!(
                "source closure path escapes destination: {}",
                entry.path
            )));
        }
        let reference = ContentRef::parse(entry.content)
            .map_err(|error| RepositoryError::Authoring(error.to_string()))?;
        let metadata = objects.metadata(&reference)?;
        let content = ato_objects::read_exact_object(
            objects,
            &reference,
            metadata.size,
            MAX_SOURCE_FILE_BYTES,
        )?;
        let output = destination.join(relative);
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        match entry.kind {
            SourceEntryKind::File => {
                fs::write(&output, content)?;
                set_executable(&output, entry.executable)?;
            }
            SourceEntryKind::Symlink => materialize_symlink(&output, &content)?,
        }
    }
    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path, executable: bool) -> Result<(), RepositoryError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if executable { 0o755 } else { 0o644 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path, _executable: bool) -> Result<(), RepositoryError> {
    Ok(())
}

#[cfg(unix)]
fn materialize_symlink(path: &Path, content: &[u8]) -> Result<(), RepositoryError> {
    use std::os::unix::fs::symlink;
    let target = std::str::from_utf8(content)
        .map_err(|_| RepositoryError::Authoring("non-UTF-8 symlink target".to_owned()))?;
    let target = Path::new(target);
    validate_symlink(target, path)?;
    symlink(target, path)?;
    Ok(())
}

#[cfg(not(unix))]
fn materialize_symlink(_path: &Path, _content: &[u8]) -> Result<(), RepositoryError> {
    Err(RepositoryError::Authoring(
        "symlink source entries are unavailable on this platform".to_owned(),
    ))
}

fn ignored(path: &Path, root: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return true;
    };
    matches!(
        relative.components().next(),
        Some(Component::Normal(name))
            if matches!(name.to_str(), Some(".git" | ".ato" | "target" | "node_modules" | ".venv"))
    )
}

fn portable_path(path: &Path) -> Result<String, RepositoryError> {
    let mut segments = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(segment) => {
                segments.push(segment.to_str().ok_or_else(|| {
                    RepositoryError::Authoring("non-UTF-8 source path".to_owned())
                })?)
            }
            _ => {
                return Err(RepositoryError::Authoring(format!(
                    "non-relative source path: {}",
                    path.display()
                )));
            }
        }
    }
    Ok(segments.join("/"))
}

fn validate_symlink(target: &Path, path: &Path) -> Result<(), RepositoryError> {
    if target.is_absolute()
        || target
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
    {
        return Err(RepositoryError::EscapingSymlink(path.to_path_buf()));
    }
    Ok(())
}

#[cfg(unix)]
fn executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable(_metadata: &fs::Metadata) -> bool {
    false
}

#[derive(Debug, Deserialize)]
struct AuthoringFile {
    workspace: AuthoringWorkspace,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthoringWorkspace {
    toolchain: String,
    version: Option<String>,
    entrypoint: Vec<String>,
    package_manager: Option<String>,
    #[serde(default = "default_working_directory")]
    working_directory: String,
}

fn default_working_directory() -> String {
    ".".to_owned()
}

fn load_authoring(root: &Path) -> Result<Option<AuthoringWorkspace>, RepositoryError> {
    let path = root.join("capsule.toml");
    if !path.is_file() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path)?;
    let parsed: Result<AuthoringFile, _> = toml::from_str(&raw);
    match parsed {
        Ok(file) => Ok(Some(file.workspace)),
        Err(_) => Ok(None),
    }
}

struct InferredWorkspace {
    toolchain: ToolchainConstraint,
    package_manager: Option<String>,
    entrypoint: Vec<String>,
    working_directory: String,
}

fn infer_workspace(
    root: &Path,
    authoring: Option<&AuthoringWorkspace>,
) -> Result<InferredWorkspace, RepositoryError> {
    if let Some(authoring) = authoring {
        if authoring.entrypoint.is_empty() {
            return Err(RepositoryError::Authoring(
                "workspace.entrypoint must not be empty".to_owned(),
            ));
        }
        return Ok(InferredWorkspace {
            toolchain: ToolchainConstraint {
                family: authoring.toolchain.clone(),
                version: authoring.version.clone(),
            },
            package_manager: authoring.package_manager.clone(),
            entrypoint: authoring.entrypoint.clone(),
            working_directory: authoring.working_directory.clone(),
        });
    }

    if root.join("deno.json").is_file() || root.join("deno.jsonc").is_file() {
        return Ok(inferred(
            "deno",
            version_file(root, ".deno-version"),
            None,
            ["deno", "task", "start"],
        ));
    }
    if root.join("package.json").is_file() {
        let package_manager = node_package_manager(root);
        let script = node_start_script(root)?;
        return Ok(inferred(
            "node",
            version_file(root, ".node-version").or_else(|| version_file(root, ".nvmrc")),
            Some(package_manager.clone()),
            [package_manager.as_str(), "run", script.as_str()],
        ));
    }
    if root.join("pyproject.toml").is_file()
        || root.join("requirements.txt").is_file()
        || root.join("main.py").is_file()
        || root.join("app.py").is_file()
    {
        let main = if root.join("main.py").is_file() {
            "main.py"
        } else {
            "app.py"
        };
        return Ok(inferred(
            "python",
            version_file(root, ".python-version"),
            Some("pip".to_owned()),
            ["python3", main],
        ));
    }
    if root.join("Cargo.toml").is_file() {
        let args = if root.join("Cargo.lock").is_file() {
            vec!["cargo".to_owned(), "run".to_owned(), "--locked".to_owned()]
        } else {
            vec!["cargo".to_owned(), "run".to_owned()]
        };
        return Ok(InferredWorkspace {
            toolchain: ToolchainConstraint {
                family: "rust".to_owned(),
                version: version_file(root, "rust-toolchain"),
            },
            package_manager: Some("cargo".to_owned()),
            entrypoint: args,
            working_directory: ".".to_owned(),
        });
    }
    if root.join("go.mod").is_file() {
        return Ok(inferred(
            "go",
            None,
            Some("go".to_owned()),
            ["go", "run", "."],
        ));
    }
    Err(RepositoryError::Unsupported)
}

fn inferred<const N: usize>(
    family: &str,
    version: Option<String>,
    package_manager: Option<String>,
    entrypoint: [&str; N],
) -> InferredWorkspace {
    InferredWorkspace {
        toolchain: ToolchainConstraint {
            family: family.to_owned(),
            version,
        },
        package_manager,
        entrypoint: entrypoint.into_iter().map(str::to_owned).collect(),
        working_directory: ".".to_owned(),
    }
}

fn version_file(root: &Path, name: &str) -> Option<String> {
    fs::read_to_string(root.join(name))
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn node_package_manager(root: &Path) -> String {
    if root.join("pnpm-lock.yaml").is_file() {
        "pnpm"
    } else if root.join("yarn.lock").is_file() {
        "yarn"
    } else if root.join("bun.lockb").is_file() || root.join("bun.lock").is_file() {
        "bun"
    } else {
        "npm"
    }
    .to_owned()
}

fn node_start_script(root: &Path) -> Result<String, RepositoryError> {
    let bytes = fs::read(root.join("package.json"))?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    let scripts = value.get("scripts").and_then(serde_json::Value::as_object);
    for candidate in ["start", "dev"] {
        if scripts.and_then(|scripts| scripts.get(candidate)).is_some() {
            return Ok(candidate.to_owned());
        }
    }
    Err(RepositoryError::Unsupported)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ato_objects::{MemoryObjectStore, ObjectResolver};

    use super::*;

    #[test]
    fn compiles_node_repository_to_workspace_computation() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"scripts":{"start":"node index.js"}}"#,
        )
        .unwrap();
        fs::write(dir.path().join("package-lock.json"), "{}").unwrap();
        fs::write(dir.path().join("index.js"), "console.log('hello')").unwrap();
        let objects = Arc::new(MemoryObjectStore::default());

        let compiled =
            compile_repository(dir.path(), objects.as_ref(), RepositoryOptions::default()).unwrap();

        assert_eq!(
            compiled.computation.semantics.as_str(),
            WORKSPACE_SEMANTICS_ID
        );
        assert_eq!(compiled.evidence.selected_toolchain, "node");
        assert_eq!(
            compiled.evidence.selected_entrypoint,
            ["npm", "run", "start"]
        );
        assert!(objects.metadata(&compiled.source).is_ok());
    }

    #[test]
    fn source_identity_changes_when_source_changes() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("main.py"), "print('Alice')").unwrap();
        let objects = MemoryObjectStore::default();
        let alice = compile_repository(dir.path(), &objects, RepositoryOptions::default()).unwrap();
        fs::write(dir.path().join("main.py"), "print('Bob')").unwrap();
        let bob = compile_repository(dir.path(), &objects, RepositoryOptions::default()).unwrap();

        assert_ne!(alice.source, bob.source);
        assert_ne!(alice.computation.residual, bob.computation.residual);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_source_symlink_that_can_escape_repository() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("main.py"), "print('safe')").unwrap();
        symlink("../secret", dir.path().join("escape")).unwrap();
        let objects = MemoryObjectStore::default();

        assert!(matches!(
            compile_repository(dir.path(), &objects, RepositoryOptions::default()),
            Err(RepositoryError::EscapingSymlink(_))
        ));
    }
}
