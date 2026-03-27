use std::path::{Path, PathBuf};

use crate::ato_lock::{
    self, AtoLock, AtoLockValidationError, ValidationMode as CanonicalValidationMode,
};
use crate::error::{CapsuleError, Result};
use crate::lockfile::{self, CapsuleLock};
use crate::manifest::{self, LoadedManifest};
use crate::types::ValidationMode as ManifestValidationMode;

pub const ATO_LOCK_FILE_NAME: &str = "ato.lock.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExplicitInputKind {
    Directory,
    CanonicalLock,
    CompatibilityManifest,
    SingleScript,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SingleScriptLanguage {
    Python,
    TypeScript,
    JavaScript,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSingleScript {
    pub path: PathBuf,
    pub language: SingleScriptLanguage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedInputKind {
    CanonicalLock,
    CompatibilityProject,
    SourceOnly,
}

impl ResolvedInputKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CanonicalLock => "canonical_lock",
            Self::CompatibilityProject => "compatibility_project",
            Self::SourceOnly => "source_only",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolverAdvisoryCode {
    CanonicalCoexistsWithCompatibility,
    CompatibilityIgnoredByCanonical,
    SourceOnlyBootstrap,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolverAdvisory {
    pub code: ResolverAdvisoryCode,
    pub message: String,
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredArtifacts {
    pub canonical_lock_path: Option<PathBuf>,
    pub compatibility_manifest_path: Option<PathBuf>,
    pub compatibility_lock_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputProvenance {
    pub requested_path: PathBuf,
    pub explicit_input_kind: ExplicitInputKind,
    pub project_root: PathBuf,
    pub discovered: DiscoveredArtifacts,
    pub selected_kind: ResolvedInputKind,
    pub authoritative_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ResolvedCanonicalLock {
    pub path: PathBuf,
    pub project_root: PathBuf,
    pub lock: AtoLock,
}

#[derive(Debug, Clone)]
pub struct ResolvedCompatibilityProject {
    pub manifest: LoadedManifest,
    pub legacy_lock: Option<ResolvedCompatibilityLock>,
    pub project_root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ResolvedCompatibilityLock {
    pub path: PathBuf,
    pub lock: CapsuleLock,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSourceOnly {
    pub project_root: PathBuf,
    pub single_script: Option<ResolvedSingleScript>,
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum ResolvedInput {
    CanonicalLock {
        canonical: ResolvedCanonicalLock,
        provenance: InputProvenance,
        advisories: Vec<ResolverAdvisory>,
    },
    CompatibilityProject {
        project: ResolvedCompatibilityProject,
        provenance: InputProvenance,
        advisories: Vec<ResolverAdvisory>,
    },
    SourceOnly {
        source: ResolvedSourceOnly,
        provenance: InputProvenance,
        advisories: Vec<ResolverAdvisory>,
    },
}

impl ResolvedInput {
    pub fn kind(&self) -> ResolvedInputKind {
        match self {
            Self::CanonicalLock { .. } => ResolvedInputKind::CanonicalLock,
            Self::CompatibilityProject { .. } => ResolvedInputKind::CompatibilityProject,
            Self::SourceOnly { .. } => ResolvedInputKind::SourceOnly,
        }
    }

    pub fn provenance(&self) -> &InputProvenance {
        match self {
            Self::CanonicalLock { provenance, .. }
            | Self::CompatibilityProject { provenance, .. }
            | Self::SourceOnly { provenance, .. } => provenance,
        }
    }

    pub fn advisories(&self) -> &[ResolverAdvisory] {
        match self {
            Self::CanonicalLock { advisories, .. }
            | Self::CompatibilityProject { advisories, .. }
            | Self::SourceOnly { advisories, .. } => advisories,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ResolveInputOptions {
    pub manifest_validation_mode: ManifestValidationMode,
    pub canonical_validation_mode: CanonicalValidationMode,
}

impl Default for ResolveInputOptions {
    fn default() -> Self {
        Self {
            manifest_validation_mode: ManifestValidationMode::Strict,
            canonical_validation_mode: CanonicalValidationMode::Strict,
        }
    }
}

#[derive(Debug, Clone)]
struct InputDiscovery {
    requested_path: PathBuf,
    explicit_input_kind: ExplicitInputKind,
    project_root: PathBuf,
    discovered: DiscoveredArtifacts,
}

#[derive(Debug, Clone)]
enum ResolutionSelection {
    CanonicalLock,
    CompatibilityProject,
    SourceOnly,
}

pub fn resolve_authoritative_input(
    path: &Path,
    options: ResolveInputOptions,
) -> Result<ResolvedInput> {
    let discovery = discover_input(path)?;
    let (selection, advisories) = classify_discovery(&discovery)?;
    materialize_resolution(discovery, selection, advisories, options)
}

fn discover_input(path: &Path) -> Result<InputDiscovery> {
    if !path.exists() {
        return Err(CapsuleError::Config(format!(
            "Input path does not exist: {}",
            path.display()
        )));
    }

    let requested_path = path.canonicalize().map_err(|err| {
        CapsuleError::Config(format!(
            "Failed to resolve input path {}: {err}",
            path.display()
        ))
    })?;

    let (project_root, explicit_input_kind) = if requested_path.is_dir() {
        (requested_path.clone(), ExplicitInputKind::Directory)
    } else {
        let file_name = requested_path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                CapsuleError::Config(format!(
                    "Unsupported input path: {}",
                    requested_path.display()
                ))
            })?;
        let project_root = requested_path.parent().ok_or_else(|| {
            CapsuleError::Config(format!(
                "Input path has no parent directory: {}",
                requested_path.display()
            ))
        })?;

        match file_name {
            ATO_LOCK_FILE_NAME => (project_root.to_path_buf(), ExplicitInputKind::CanonicalLock),
            "capsule.toml" => (
                project_root.to_path_buf(),
                ExplicitInputKind::CompatibilityManifest,
            ),
            _ if resolve_single_script_language(&requested_path).is_some() => {
                (project_root.to_path_buf(), ExplicitInputKind::SingleScript)
            }
            lockfile::CAPSULE_LOCK_FILE_NAME | lockfile::LEGACY_CAPSULE_LOCK_FILE_NAME => {
                return Err(legacy_lock_without_manifest_error(&requested_path));
            }
            _ => {
                return Err(CapsuleError::Config(format!(
                    "Unsupported authoritative input path: {}",
                    requested_path.display()
                )));
            }
        }
    };

    let discovered = DiscoveredArtifacts {
        canonical_lock_path: project_root
            .join(ATO_LOCK_FILE_NAME)
            .exists()
            .then(|| project_root.join(ATO_LOCK_FILE_NAME)),
        compatibility_manifest_path: project_root
            .join("capsule.toml")
            .exists()
            .then(|| project_root.join("capsule.toml")),
        compatibility_lock_path: lockfile::resolve_existing_lockfile_path(&project_root),
    };

    Ok(InputDiscovery {
        requested_path,
        explicit_input_kind,
        project_root,
        discovered,
    })
}

fn classify_discovery(
    discovery: &InputDiscovery,
) -> Result<(ResolutionSelection, Vec<ResolverAdvisory>)> {
    let mut advisories = Vec::new();

    if discovery.discovered.canonical_lock_path.is_some() {
        if let Some(manifest_path) = discovery.discovered.compatibility_manifest_path.as_ref() {
            advisories.push(ResolverAdvisory {
                code: ResolverAdvisoryCode::CanonicalCoexistsWithCompatibility,
                message: format!(
                    "{} coexists with compatibility inputs; canonical lock remains authoritative.",
                    ATO_LOCK_FILE_NAME
                ),
                paths: vec![manifest_path.clone()],
            });
        }
        if let Some(lock_path) = discovery.discovered.compatibility_lock_path.as_ref() {
            advisories.push(ResolverAdvisory {
                code: ResolverAdvisoryCode::CompatibilityIgnoredByCanonical,
                message: format!(
                    "Compatibility input {} is advisory only because {} is present.",
                    lock_path
                        .file_name()
                        .and_then(|value| value.to_str())
                        .unwrap_or(lockfile::CAPSULE_LOCK_FILE_NAME),
                    ATO_LOCK_FILE_NAME
                ),
                paths: vec![lock_path.clone()],
            });
        }

        return Ok((ResolutionSelection::CanonicalLock, advisories));
    }

    if discovery.discovered.compatibility_manifest_path.is_some() {
        return Ok((ResolutionSelection::CompatibilityProject, advisories));
    }

    if let Some(lock_path) = discovery.discovered.compatibility_lock_path.as_ref() {
        return Err(legacy_lock_without_manifest_error(lock_path));
    }

    advisories.push(ResolverAdvisory {
        code: ResolverAdvisoryCode::SourceOnlyBootstrap,
        message: "No canonical or compatibility project input was found; caller should use source-only/bootstrap flow.".to_string(),
        paths: vec![discovery.project_root.clone()],
    });
    Ok((ResolutionSelection::SourceOnly, advisories))
}

fn materialize_resolution(
    discovery: InputDiscovery,
    selection: ResolutionSelection,
    advisories: Vec<ResolverAdvisory>,
    options: ResolveInputOptions,
) -> Result<ResolvedInput> {
    match selection {
        ResolutionSelection::CanonicalLock => {
            let path = discovery
                .discovered
                .canonical_lock_path
                .clone()
                .expect("canonical lock path must exist");
            let lock = ato_lock::load_unvalidated_from_path(&path)?;
            ato_lock::validate_persisted(&lock, options.canonical_validation_mode).map_err(|errors| {
                CapsuleError::Config(format!(
                    "{} is present but invalid at {}: {}. Compatibility inputs will not be used as fallback.",
                    ATO_LOCK_FILE_NAME,
                    path.display(),
                    format_ato_lock_validation_errors(&errors)
                ))
            })?;

            let provenance = InputProvenance {
                requested_path: discovery.requested_path,
                explicit_input_kind: discovery.explicit_input_kind,
                project_root: discovery.project_root.clone(),
                discovered: discovery.discovered,
                selected_kind: ResolvedInputKind::CanonicalLock,
                authoritative_path: Some(path.clone()),
            };

            Ok(ResolvedInput::CanonicalLock {
                canonical: ResolvedCanonicalLock {
                    path,
                    project_root: discovery.project_root,
                    lock,
                },
                provenance,
                advisories,
            })
        }
        ResolutionSelection::CompatibilityProject => {
            let manifest_path = discovery
                .discovered
                .compatibility_manifest_path
                .clone()
                .expect("compatibility manifest path must exist");
            let manifest = manifest::load_manifest_with_validation_mode(
                &manifest_path,
                options.manifest_validation_mode,
            )?;
            let legacy_lock = if let Some(lock_path) =
                discovery.discovered.compatibility_lock_path.as_ref()
            {
                let raw = std::fs::read_to_string(lock_path).map_err(|err| {
                    CapsuleError::Config(format!("Failed to read {}: {err}", lock_path.display()))
                })?;
                Some(ResolvedCompatibilityLock {
                    path: lock_path.clone(),
                    lock: lockfile::parse_lockfile_text(&raw, lock_path)?,
                })
            } else {
                None
            };

            let provenance = InputProvenance {
                requested_path: discovery.requested_path,
                explicit_input_kind: discovery.explicit_input_kind,
                project_root: discovery.project_root.clone(),
                discovered: discovery.discovered,
                selected_kind: ResolvedInputKind::CompatibilityProject,
                authoritative_path: Some(manifest_path),
            };

            Ok(ResolvedInput::CompatibilityProject {
                project: ResolvedCompatibilityProject {
                    manifest,
                    legacy_lock,
                    project_root: discovery.project_root,
                },
                provenance,
                advisories,
            })
        }
        ResolutionSelection::SourceOnly => {
            let single_script = single_script_from_discovery(&discovery);
            let provenance = InputProvenance {
                requested_path: discovery.requested_path,
                explicit_input_kind: discovery.explicit_input_kind,
                project_root: discovery.project_root.clone(),
                discovered: discovery.discovered,
                selected_kind: ResolvedInputKind::SourceOnly,
                authoritative_path: None,
            };

            Ok(ResolvedInput::SourceOnly {
                source: ResolvedSourceOnly {
                    project_root: discovery.project_root,
                    single_script,
                },
                provenance,
                advisories,
            })
        }
    }
}

fn legacy_lock_without_manifest_error(path: &Path) -> CapsuleError {
    CapsuleError::Config(format!(
        "{} is not an authoritative command-entry input without capsule.toml: {}",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or(lockfile::CAPSULE_LOCK_FILE_NAME),
        path.display()
    ))
}

fn resolve_single_script_language(path: &Path) -> Option<SingleScriptLanguage> {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("py") => Some(SingleScriptLanguage::Python),
        Some("ts") | Some("tsx") => Some(SingleScriptLanguage::TypeScript),
        Some("js") | Some("jsx") => Some(SingleScriptLanguage::JavaScript),
        _ => None,
    }
}

fn single_script_from_discovery(discovery: &InputDiscovery) -> Option<ResolvedSingleScript> {
    if discovery.explicit_input_kind != ExplicitInputKind::SingleScript {
        return None;
    }

    Some(ResolvedSingleScript {
        path: discovery.requested_path.clone(),
        language: resolve_single_script_language(&discovery.requested_path)
            .expect("single script language must be known"),
    })
}

fn format_ato_lock_validation_errors(errors: &[AtoLockValidationError]) -> String {
    errors
        .iter()
        .map(|error| error.to_string())
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::tempdir;

    use super::{
        resolve_authoritative_input, ResolveInputOptions, ResolvedInput, ResolvedInputKind,
        ResolverAdvisoryCode, SingleScriptLanguage, ATO_LOCK_FILE_NAME,
    };
    use crate::ato_lock::{recompute_lock_id, AtoLock};

    fn write_manifest(dir: &Path, name: &str) {
        fs::write(
            dir.join("capsule.toml"),
            format!(
                r#"schema_version = "0.2"
name = "{name}"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "web"
driver = "static"
entrypoint = "dist"
port = 4173
"#
            ),
        )
        .expect("write manifest");
    }

    fn write_legacy_lock(dir: &Path) {
        fs::write(
            dir.join("capsule.lock.json"),
            r#"{
  "version": "1",
  "meta": {
    "created_at": "2026-03-25T00:00:00Z",
    "manifest_hash": "sha256:demo"
  }
}"#,
        )
        .expect("write legacy lock");
    }

    fn write_canonical_lock(dir: &Path) {
        let mut lock = AtoLock::default();
        lock.resolution
            .entries
            .insert("runtime".to_string(), serde_json::json!({"kind": "deno"}));
        lock.contract.entries.insert(
            "process".to_string(),
            serde_json::json!({"driver": "deno", "entrypoint": "main.ts"}),
        );
        recompute_lock_id(&mut lock).expect("recompute lock id");
        let raw = crate::ato_lock::to_pretty_json(&lock).expect("serialize lock");
        fs::write(dir.join(ATO_LOCK_FILE_NAME), raw).expect("write canonical lock");
    }

    #[test]
    fn canonical_lock_wins_over_compatibility_inputs() {
        let dir = tempdir().expect("tempdir");
        write_manifest(dir.path(), "demo");
        write_legacy_lock(dir.path());
        write_canonical_lock(dir.path());

        let resolved = resolve_authoritative_input(dir.path(), ResolveInputOptions::default())
            .expect("resolve authoritative input");

        assert_eq!(resolved.kind(), ResolvedInputKind::CanonicalLock);
        assert_eq!(
            resolved.provenance().selected_kind,
            ResolvedInputKind::CanonicalLock
        );
        assert!(resolved
            .advisories()
            .iter()
            .any(|advisory| advisory.code
                == ResolverAdvisoryCode::CanonicalCoexistsWithCompatibility));
        assert!(
            resolved
                .advisories()
                .iter()
                .any(|advisory| advisory.code
                    == ResolverAdvisoryCode::CompatibilityIgnoredByCanonical)
        );
    }

    #[test]
    fn invalid_canonical_lock_does_not_fallback_to_manifest() {
        let dir = tempdir().expect("tempdir");
        write_manifest(dir.path(), "demo");
        fs::write(
            dir.path().join(ATO_LOCK_FILE_NAME),
            r#"{"schema_version":1}"#,
        )
        .expect("write invalid lock");

        let err = resolve_authoritative_input(dir.path(), ResolveInputOptions::default())
            .expect_err("invalid canonical lock must fail");
        let message = err.to_string();
        assert!(message.contains("Compatibility inputs will not be used as fallback"));
        assert!(message.contains(ATO_LOCK_FILE_NAME));
    }

    #[test]
    fn manifest_only_returns_compatibility_project() {
        let dir = tempdir().expect("tempdir");
        write_manifest(dir.path(), "demo");

        let resolved = resolve_authoritative_input(dir.path(), ResolveInputOptions::default())
            .expect("resolve compatibility project");

        match resolved {
            ResolvedInput::CompatibilityProject { project, .. } => {
                assert_eq!(project.manifest.model.name, "demo");
                assert!(project.legacy_lock.is_none());
            }
            other => panic!("unexpected resolved input: {other:?}"),
        }
    }

    #[test]
    fn source_only_returns_bootstrap_state() {
        let dir = tempdir().expect("tempdir");

        let resolved = resolve_authoritative_input(dir.path(), ResolveInputOptions::default())
            .expect("resolve source only");

        assert_eq!(resolved.kind(), ResolvedInputKind::SourceOnly);
        assert!(resolved
            .advisories()
            .iter()
            .any(|advisory| advisory.code == ResolverAdvisoryCode::SourceOnlyBootstrap));
    }

    #[test]
    fn legacy_lock_without_manifest_is_fail_closed() {
        let dir = tempdir().expect("tempdir");
        write_legacy_lock(dir.path());

        let err = resolve_authoritative_input(dir.path(), ResolveInputOptions::default())
            .expect_err("legacy lock without manifest must fail");
        assert!(err
            .to_string()
            .contains("is not an authoritative command-entry input without capsule.toml"));
    }

    #[test]
    fn explicit_path_semantics_follow_authoritative_precedence() {
        let dir = tempdir().expect("tempdir");
        write_manifest(dir.path(), "demo");
        write_canonical_lock(dir.path());
        write_legacy_lock(dir.path());

        let manifest_path = dir.path().join("capsule.toml");
        let canonical_path = dir.path().join(ATO_LOCK_FILE_NAME);
        let legacy_path = dir.path().join("capsule.lock.json");

        let from_manifest =
            resolve_authoritative_input(&manifest_path, ResolveInputOptions::default())
                .expect("resolve from manifest path");
        assert_eq!(from_manifest.kind(), ResolvedInputKind::CanonicalLock);

        let from_lock =
            resolve_authoritative_input(&canonical_path, ResolveInputOptions::default())
                .expect("resolve from canonical lock path");
        assert_eq!(from_lock.kind(), ResolvedInputKind::CanonicalLock);

        let err = resolve_authoritative_input(&legacy_path, ResolveInputOptions::default())
            .expect_err("legacy lock path must fail");
        assert!(err
            .to_string()
            .contains("is not an authoritative command-entry input without capsule.toml"));
    }

    #[test]
    fn single_python_script_resolves_as_source_only() {
        let dir = tempdir().expect("tempdir");
        let script_path = dir.path().join("hello.py");
        fs::write(&script_path, "print('hello')\n").expect("write script");

        let resolved = resolve_authoritative_input(&script_path, ResolveInputOptions::default())
            .expect("resolve single script");

        match resolved {
            ResolvedInput::SourceOnly {
                source, provenance, ..
            } => {
                assert_eq!(
                    provenance.explicit_input_kind,
                    super::ExplicitInputKind::SingleScript
                );
                let script = source.single_script.expect("single script metadata");
                assert_eq!(script.path, script_path.canonicalize().expect("canonical"));
                assert_eq!(script.language, SingleScriptLanguage::Python);
            }
            other => panic!("unexpected resolved input: {other:?}"),
        }
    }

    #[test]
    fn single_typescript_script_resolves_as_source_only() {
        let dir = tempdir().expect("tempdir");
        let script_path = dir.path().join("hello.ts");
        fs::write(&script_path, "console.log('hello');\n").expect("write script");

        let resolved = resolve_authoritative_input(&script_path, ResolveInputOptions::default())
            .expect("resolve single script");

        match resolved {
            ResolvedInput::SourceOnly {
                source, provenance, ..
            } => {
                assert_eq!(
                    provenance.explicit_input_kind,
                    super::ExplicitInputKind::SingleScript
                );
                let script = source.single_script.expect("single script metadata");
                assert_eq!(script.path, script_path.canonicalize().expect("canonical"));
                assert_eq!(script.language, SingleScriptLanguage::TypeScript);
            }
            other => panic!("unexpected resolved input: {other:?}"),
        }
    }

    #[test]
    fn single_tsx_script_resolves_as_source_only() {
        let dir = tempdir().expect("tempdir");
        let script_path = dir.path().join("hello.tsx");
        fs::write(&script_path, "export const App = <div>hello</div>;\n").expect("write script");

        let resolved = resolve_authoritative_input(&script_path, ResolveInputOptions::default())
            .expect("resolve single script");

        match resolved {
            ResolvedInput::SourceOnly {
                source, provenance, ..
            } => {
                assert_eq!(
                    provenance.explicit_input_kind,
                    super::ExplicitInputKind::SingleScript
                );
                let script = source.single_script.expect("single script metadata");
                assert_eq!(script.path, script_path.canonicalize().expect("canonical"));
                assert_eq!(script.language, SingleScriptLanguage::TypeScript);
            }
            other => panic!("unexpected resolved input: {other:?}"),
        }
    }

    #[test]
    fn single_javascript_script_resolves_as_source_only() {
        let dir = tempdir().expect("tempdir");
        let script_path = dir.path().join("hello.js");
        fs::write(&script_path, "console.log('hello');\n").expect("write script");

        let resolved = resolve_authoritative_input(&script_path, ResolveInputOptions::default())
            .expect("resolve single script");

        match resolved {
            ResolvedInput::SourceOnly {
                source, provenance, ..
            } => {
                assert_eq!(
                    provenance.explicit_input_kind,
                    super::ExplicitInputKind::SingleScript
                );
                let script = source.single_script.expect("single script metadata");
                assert_eq!(script.path, script_path.canonicalize().expect("canonical"));
                assert_eq!(script.language, SingleScriptLanguage::JavaScript);
            }
            other => panic!("unexpected resolved input: {other:?}"),
        }
    }

    #[test]
    fn single_jsx_script_resolves_as_source_only() {
        let dir = tempdir().expect("tempdir");
        let script_path = dir.path().join("hello.jsx");
        fs::write(&script_path, "export const App = <div>hello</div>;\n").expect("write script");

        let resolved = resolve_authoritative_input(&script_path, ResolveInputOptions::default())
            .expect("resolve single script");

        match resolved {
            ResolvedInput::SourceOnly {
                source, provenance, ..
            } => {
                assert_eq!(
                    provenance.explicit_input_kind,
                    super::ExplicitInputKind::SingleScript
                );
                let script = source.single_script.expect("single script metadata");
                assert_eq!(script.path, script_path.canonicalize().expect("canonical"));
                assert_eq!(script.language, SingleScriptLanguage::JavaScript);
            }
            other => panic!("unexpected resolved input: {other:?}"),
        }
    }
}
