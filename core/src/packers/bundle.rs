use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tar::{Builder, EntryType, Header};

use super::runtime_fetcher::RuntimeFetcher;
use crate::common::paths::workspace_artifacts_dir;
use crate::error::{CapsuleError, Result};
use crate::lockfile::{resolve_existing_lockfile_path, CAPSULE_LOCK_FILE_NAME};
use crate::packers::pack_filter::load_pack_filter_from_path;
use crate::r3_config::resolve_existing_config_path;
use crate::router::CompatProjectInput;
use crate::types::CapsuleManifest;

/// Magic bytes to identify self-extracting v2 bundles.
const BUNDLE_MAGIC: &[u8] = b"NACELLE_V2_BUNDLE";

pub struct PackBundleArgs {
    pub manifest_path: Option<PathBuf>,
    pub workspace_root: PathBuf,
    pub compat_input: Option<CompatProjectInput>,
    pub runtime_path: Option<PathBuf>,
    pub output: Option<PathBuf>,
    pub nacelle_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct SourceTargetHint {
    language: String,
    version: Option<String>,
    entrypoint: Option<String>,
}

#[derive(Debug, Clone)]
struct RuntimeAlias {
    archive_path: String,
    source_path: PathBuf,
}

pub async fn build_bundle(
    args: PackBundleArgs,
    reporter: Arc<dyn crate::reporter::CapsuleReporter + 'static>,
) -> Result<PathBuf> {
    let source_dir = args.workspace_root.canonicalize()?;
    let manifest_path = args
        .manifest_path
        .as_ref()
        .and_then(|path| path.canonicalize().ok());
    let compat_input = args.compat_input.as_ref();
    let manifest = compat_input.map(|input| input.manifest().clone());

    let output_path = args
        .output
        .unwrap_or_else(|| source_dir.join("nacelle-bundle"));

    let source_target_hint = if let Some(manifest) = manifest.as_ref() {
        source_target_hint_from_manifest(manifest)
    } else if let Some(manifest_path) = manifest_path.as_ref() {
        read_manifest_source_target_hint(manifest_path)?
    } else {
        None
    };
    let manifest_entrypoint = if let Some(manifest) = manifest.as_ref() {
        manifest_entrypoint_from_manifest(manifest, source_target_hint.as_ref()).unwrap_or_default()
    } else if let Some(manifest_path) = manifest_path.as_ref() {
        read_manifest_entrypoint(manifest_path, source_target_hint.as_ref())?.unwrap_or_default()
    } else {
        String::new()
    };

    let runtime_to_bundle = decide_runtime_to_bundle(
        &source_dir,
        &manifest_entrypoint,
        source_target_hint.as_ref(),
    )?;

    let mut temp_runtime_dir: Option<PathBuf> = None;

    let runtime_dir = if let Some(runtime) = args.runtime_path {
        runtime
    } else if let Some(spec) = &runtime_to_bundle {
        let fetcher = RuntimeFetcher::new_with_reporter(reporter.clone())?;
        let version = runtime_version_for(spec.language.as_str(), spec.version.as_deref())?;
        match spec.language.as_str() {
            "python" => {
                reporter
                    .notify(format!(
                        "✓ Ensuring Python {} runtime is available...",
                        version
                    ))
                    .await?;
                fetcher.download_python_runtime(&version).await?
            }
            "node" => {
                reporter
                    .notify(format!(
                        "✓ Ensuring Node {} runtime is available...",
                        version
                    ))
                    .await?;
                fetcher.download_node_runtime(&version).await?
            }
            "deno" => {
                reporter
                    .notify(format!(
                        "✓ Ensuring Deno {} runtime is available...",
                        version
                    ))
                    .await?;
                fetcher.download_deno_runtime(&version).await?
            }
            "bun" => {
                reporter
                    .notify(format!(
                        "✓ Ensuring Bun {} runtime is available...",
                        version
                    ))
                    .await?;
                fetcher.download_bun_runtime(&version).await?
            }
            other => {
                return Err(CapsuleError::Pack(format!(
                    "Unsupported runtime language for bundling: {}",
                    other
                )))
            }
        }
    } else {
        let dir =
            std::env::temp_dir().join(format!("capsule-empty-runtime-{}", std::process::id()));
        fs::create_dir_all(&dir)?;
        temp_runtime_dir = Some(dir.clone());
        dir
    };

    if let Some(spec) = &runtime_to_bundle {
        let version = runtime_version_for(spec.language.as_str(), spec.version.as_deref())?;
        reporter
            .notify(format!(
                "✓ Using runtime: {:?} ({} {})",
                runtime_dir, spec.language, version
            ))
            .await?;
    } else {
        if let Some(hint) = &source_target_hint {
            reporter
                .notify(format!(
                    "✓ No runtime bundled (targets.source.language = {}).",
                    hint.language
                ))
                .await?;
        } else {
            reporter
                .notify(format!(
                    "✓ No runtime bundled (entrypoint: {:?})",
                    manifest_entrypoint
                ))
                .await?;
        }
        reporter
            .warn(
                "ℹ️  Note: This bundle will require the entrypoint runtime to be available on the target host."
                    .to_string(),
            )
            .await?;
    }

    let runtime_alias = build_runtime_alias(runtime_to_bundle.as_ref(), &runtime_dir)?;

    reporter
        .notify("✓ Creating bundle archive...".to_string())
        .await?;
    let build_excludes = if let Some(manifest) = manifest.as_ref() {
        build_exclude_patterns_from_manifest(manifest)
    } else if let Some(manifest_path) = manifest_path.as_ref() {
        read_build_exclude_patterns(manifest_path)?
    } else {
        Vec::new()
    };
    let source_ignore = load_capsuleignore(&source_dir, &build_excludes)?;
    let pack_filter = if let Some(manifest) = manifest.as_ref() {
        crate::packers::pack_filter::PackFilter::from_manifest(manifest)?
    } else if let Some(manifest_path) = manifest_path.as_ref() {
        load_pack_filter_from_path(manifest_path)?
    } else {
        return Err(CapsuleError::Pack(
            "bundle creation requires compat manifest bridge or manifest path".to_string(),
        ));
    };
    let _node_modules_guard = NodeModulesGuard::new(&source_dir, source_ignore.as_ref())?;
    let resolved_config_path = resolve_existing_config_path(&source_dir);
    let config_ref = resolved_config_path.as_deref();
    let archive_data = create_bundle_archive(
        &runtime_dir,
        &source_dir,
        source_ignore.as_ref(),
        &pack_filter,
        config_ref,
        runtime_alias.as_ref(),
    )?;
    reporter
        .notify(format!(
            "✓ Archive size: {} MB",
            archive_data.len() / 1_048_576
        ))
        .await?;

    if let Some(dir) = temp_runtime_dir {
        let _ = fs::remove_dir_all(dir);
    }

    reporter
        .notify("✓ Compressing with Zstd Level 19...".to_string())
        .await?;
    let compressed = compress_with_zstd(&archive_data, 19)?;
    reporter
        .notify(format!(
            "✓ Compressed size: {} MB",
            compressed.len() / 1_048_576
        ))
        .await?;
    reporter
        .notify(format!(
            "  Compression ratio: {:.1}%",
            (compressed.len() as f64 / archive_data.len() as f64) * 100.0
        ))
        .await?;

    reporter
        .notify("✓ Creating self-extracting executable...".to_string())
        .await?;

    let nacelle_bin = find_nacelle_binary(args.nacelle_path.as_ref())?;
    reporter
        .notify(format!(
            "✓ Using nacelle binary: {:?} ({} KB)",
            nacelle_bin,
            fs::metadata(&nacelle_bin)?.len() / 1024
        ))
        .await?;

    let mut output = fs::File::create(&output_path)?;

    let nacelle_data = fs::read(&nacelle_bin)?;
    output.write_all(&nacelle_data)?;

    output.write_all(&compressed)?;

    output.write_all(BUNDLE_MAGIC)?;
    let size_bytes = (compressed.len() as u64).to_le_bytes();
    output.write_all(&size_bytes)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&output_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&output_path, perms)?;
    }

    Ok(output_path)
}

fn decide_runtime_to_bundle(
    source_dir: &Path,
    entrypoint: &str,
    source_target: Option<&SourceTargetHint>,
) -> Result<Option<SourceTargetHint>> {
    if let Some(target) = source_target {
        let mut resolved = target.clone();
        resolved.entrypoint = resolved.entrypoint.clone().or_else(|| {
            if entrypoint.is_empty() {
                None
            } else {
                Some(entrypoint.to_string())
            }
        });
        return Ok(Some(resolved));
    }

    if entrypoint.trim().is_empty() {
        return Ok(None);
    }

    let entry_path = resolve_entrypoint_path(entrypoint, source_dir, source_dir);

    let ext = entry_path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    if ext == "py" {
        return Ok(Some(SourceTargetHint {
            language: "python".to_string(),
            version: None,
            entrypoint: Some(entrypoint.to_string()),
        }));
    }

    if ext == "js" || ext == "mjs" || ext == "cjs" || ext == "ts" {
        return Ok(Some(SourceTargetHint {
            language: "node".to_string(),
            version: None,
            entrypoint: Some(entrypoint.to_string()),
        }));
    }

    Ok(None)
}

fn source_target_hint_from_manifest(manifest: &CapsuleManifest) -> Option<SourceTargetHint> {
    let target = manifest.targets.as_ref().and_then(|targets| {
        targets
            .source
            .as_ref()
            .map(|source| {
                (
                    source.language.clone(),
                    source.version.clone(),
                    Some(source.entrypoint.clone()),
                )
            })
            .or_else(|| {
                targets
                    .named
                    .get("source")
                    .or_else(|| targets.named.get(&manifest.default_target))
                    .and_then(|target| {
                        if target.runtime.trim() != "source" {
                            return None;
                        }
                        target.language.clone().map(|language| {
                            (
                                language,
                                target.runtime_version.clone(),
                                Some(target.entrypoint.clone()),
                            )
                        })
                    })
            })
    })?;

    Some(SourceTargetHint {
        language: target.0,
        version: target.1,
        entrypoint: target.2,
    })
}

fn manifest_entrypoint_from_manifest(
    manifest: &CapsuleManifest,
    source_target: Option<&SourceTargetHint>,
) -> Option<String> {
    if let Some(target) = source_target {
        if let Some(entrypoint) = &target.entrypoint {
            let trimmed = entrypoint.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }

    let trimmed = manifest.execution.entrypoint.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn build_exclude_patterns_from_manifest(manifest: &CapsuleManifest) -> Vec<String> {
    manifest
        .build
        .as_ref()
        .map(|build| build.exclude_libs.clone())
        .unwrap_or_default()
}

fn read_manifest_source_target_hint(manifest_path: &Path) -> Result<Option<SourceTargetHint>> {
    let raw = fs::read_to_string(manifest_path).map_err(|e| {
        CapsuleError::Pack(format!(
            "Failed to read manifest {}: {}",
            manifest_path.display(),
            e
        ))
    })?;

    let manifest: toml::Value = toml::from_str(&raw).map_err(|e| {
        CapsuleError::Pack(format!(
            "Failed to parse manifest TOML {}: {}",
            manifest_path.display(),
            e
        ))
    })?;

    let target = manifest
        .get("targets")
        .and_then(|t| t.get("source"))
        .and_then(|t| t.as_table());

    let Some(target) = target else {
        return Ok(None);
    };

    let language = target
        .get("language")
        .and_then(|v| v.as_str())
        .map(|v| v.to_string());

    let version = target
        .get("runtime_version")
        .and_then(|v| v.as_str())
        .map(|v| v.to_string())
        .or_else(|| {
            target
                .get("version")
                .and_then(|v| v.as_str())
                .map(|v| v.to_string())
        });

    let entrypoint = target
        .get("entrypoint")
        .and_then(|v| v.as_str())
        .map(|v| v.to_string());

    match language {
        Some(language) => Ok(Some(SourceTargetHint {
            language,
            version,
            entrypoint,
        })),
        None => Ok(None),
    }
}

fn read_manifest_entrypoint(
    manifest_path: &Path,
    source_target: Option<&SourceTargetHint>,
) -> Result<Option<String>> {
    if let Some(target) = source_target {
        if let Some(entrypoint) = &target.entrypoint {
            if !entrypoint.trim().is_empty() {
                return Ok(Some(entrypoint.trim().to_string()));
            }
        }
    }

    let raw = fs::read_to_string(manifest_path).map_err(|e| {
        CapsuleError::Pack(format!(
            "Failed to read manifest {}: {}",
            manifest_path.display(),
            e
        ))
    })?;

    let manifest: toml::Value = toml::from_str(&raw).map_err(|e| {
        CapsuleError::Pack(format!(
            "Failed to parse manifest TOML {}: {}",
            manifest_path.display(),
            e
        ))
    })?;

    let entrypoint = manifest
        .get("execution")
        .and_then(|e| {
            e.get("release")
                .and_then(|p| p.get("entrypoint"))
                .or_else(|| e.get("entrypoint"))
        })
        .and_then(|e| e.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    Ok(entrypoint)
}

fn resolve_entrypoint_path(entrypoint: &str, manifest_dir: &Path, source_dir: &Path) -> PathBuf {
    let trimmed = entrypoint.trim();
    if trimmed.is_empty() {
        return source_dir.to_path_buf();
    }

    let path = PathBuf::from(trimmed);
    if path.is_absolute() {
        return path;
    }

    let manifest_path = manifest_dir.join(&path);
    if manifest_path.exists() {
        return manifest_path;
    }

    source_dir.join(path)
}

fn build_runtime_alias(
    runtime: Option<&SourceTargetHint>,
    runtime_dir: &Path,
) -> Result<Option<RuntimeAlias>> {
    let Some(runtime) = runtime else {
        return Ok(None);
    };

    let source_path = resolve_runtime_root(runtime_dir, runtime.language.as_str())?;
    let archive_path = format!("runtime/{}", runtime.language);

    Ok(Some(RuntimeAlias {
        archive_path,
        source_path,
    }))
}

fn resolve_runtime_root(runtime_dir: &Path, language: &str) -> Result<PathBuf> {
    let direct = runtime_dir.join(language);
    if direct.exists() {
        return Ok(direct);
    }

    if language == "node" {
        let entries = fs::read_dir(runtime_dir)
            .map_err(|e| CapsuleError::Pack(format!("Failed to read runtime dir: {}", e)))?;
        for entry in entries {
            let entry = entry.map_err(|e| CapsuleError::Pack(format!("Walk error: {}", e)))?;
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if path.is_dir() && name.starts_with("node-") {
                return Ok(path);
            }
        }
    }

    Ok(runtime_dir.to_path_buf())
}

fn runtime_version_for(language: &str, version: Option<&str>) -> Result<String> {
    if let Some(v) = version {
        return Ok(v.to_string());
    }
    if matches!(language, "python" | "node" | "deno") {
        return Err(CapsuleError::Config(format!(
            "targets.source.runtime_version is required for language '{}'",
            language
        )));
    }
    Ok("latest".to_string())
}

fn create_bundle_archive(
    runtime_dir: &Path,
    source_dir: &Path,
    source_ignore: Option<&Gitignore>,
    source_filter: &crate::packers::pack_filter::PackFilter,
    config_path: Option<&Path>,
    runtime_alias: Option<&RuntimeAlias>,
) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    {
        let mut builder = Builder::new(&mut data);

        if let Some(alias) = runtime_alias {
            append_dir(
                &mut builder,
                &alias.source_path,
                &alias.archive_path,
                None,
                None,
            )?;
        } else {
            append_dir(&mut builder, runtime_dir, "runtime", None, None)?;
        }

        let source_subdir = source_dir.join("source");
        let (actual_source_dir, source_prefix) = if source_subdir.is_dir() {
            (source_subdir.as_path(), "")
        } else {
            (source_dir, "")
        };

        append_dir(
            &mut builder,
            actual_source_dir,
            source_prefix,
            source_ignore,
            Some(source_filter),
        )?;

        if let Some(config_path) = config_path {
            append_file(&mut builder, config_path, "config.json")?;
        }

        if let Some(capsule_lock) = resolve_existing_lockfile_path(source_dir) {
            append_file(&mut builder, &capsule_lock, CAPSULE_LOCK_FILE_NAME)?;
        }

        let uv_lock = source_dir.join("uv.lock");
        if uv_lock.exists() {
            append_file(&mut builder, &uv_lock, "source/uv.lock")?;
        }

        let locks_dir = source_dir.join("locks");
        if locks_dir.exists() {
            append_dir(&mut builder, &locks_dir, "locks", None, None)?;
        }

        let artifacts_dirs = [
            workspace_artifacts_dir(source_dir),
            source_dir.join("artifacts"),
        ];
        for artifacts_dir in artifacts_dirs {
            if artifacts_dir.exists() {
                append_dir(&mut builder, &artifacts_dir, "artifacts", None, None)?;
                break;
            }
        }

        builder.finish()?;
    }
    Ok(data)
}

fn append_dir(
    builder: &mut Builder<&mut Vec<u8>>,
    dir: &Path,
    prefix: &str,
    ignore: Option<&Gitignore>,
    filter: Option<&crate::packers::pack_filter::PackFilter>,
) -> Result<()> {
    for entry in ignore::WalkBuilder::new(dir)
        .hidden(false)
        .git_ignore(false)
        .git_exclude(false)
        .git_global(false)
        .ignore(false)
        .follow_links(false)
        .build()
    {
        let entry = entry.map_err(|e| CapsuleError::Pack(format!("Walk error: {}", e)))?;
        let path = entry.path();
        let rel = path.strip_prefix(dir).unwrap_or(path);
        if rel.as_os_str().is_empty() {
            continue;
        }

        if let Some(ignore) = ignore {
            if ignore
                .matched_path_or_any_parents(
                    path,
                    entry.file_type().map(|t| t.is_dir()).unwrap_or(false),
                )
                .is_ignore()
            {
                continue;
            }
        }

        if let Some(filter) = filter {
            let is_file = entry.file_type().map(|t| t.is_file()).unwrap_or(false);
            let is_symlink = entry.file_type().map(|t| t.is_symlink()).unwrap_or(false);
            if !(is_file || is_symlink) {
                continue;
            }
            if !filter.should_include_file(rel) {
                continue;
            }
        }

        let target = if prefix.is_empty() {
            rel.to_path_buf()
        } else {
            PathBuf::from(prefix).join(rel)
        };

        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            if filter.is_some() {
                continue;
            }
            builder.append_dir(target, path)?;
        } else if entry.file_type().map(|t| t.is_symlink()).unwrap_or(false) {
            let link_target = fs::read_link(path).map_err(|e| {
                CapsuleError::Pack(format!(
                    "Failed to read symlink target {}: {}",
                    path.display(),
                    e
                ))
            })?;
            let mut header = Header::new_gnu();
            header.set_entry_type(EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            builder.append_link(&mut header, target, link_target)?;
        } else if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            builder.append_path_with_name(path, target)?;
        }
    }

    Ok(())
}

fn append_file(builder: &mut Builder<&mut Vec<u8>>, file: &Path, target: &str) -> Result<()> {
    builder.append_path_with_name(file, target)?;
    Ok(())
}

fn read_build_exclude_patterns(manifest_path: &Path) -> Result<Vec<String>> {
    let raw = fs::read_to_string(manifest_path).map_err(|e| {
        CapsuleError::Pack(format!(
            "Failed to read manifest {}: {}",
            manifest_path.display(),
            e
        ))
    })?;

    let manifest: toml::Value = toml::from_str(&raw).map_err(|e| {
        CapsuleError::Pack(format!(
            "Failed to parse manifest TOML {}: {}",
            manifest_path.display(),
            e
        ))
    })?;

    let patterns = manifest
        .get("build")
        .and_then(|b| b.get("exclude_libs"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(patterns)
}

fn load_capsuleignore(source_dir: &Path, build_excludes: &[String]) -> Result<Option<Gitignore>> {
    let mut builder = GitignoreBuilder::new(source_dir);
    let ignore_path = source_dir.join(".capsuleignore");
    if ignore_path.exists() {
        builder.add(ignore_path);
    }

    for pattern in build_excludes {
        builder
            .add_line(None, pattern)
            .map_err(|e| CapsuleError::Pack(format!("Invalid ignore pattern: {}", e)))?;
    }

    let gitignore = builder
        .build()
        .map_err(|e| CapsuleError::Pack(format!("Failed to build ignore rules: {}", e)))?;
    Ok(Some(gitignore))
}

struct NodeModulesGuard {
    moves: Vec<(PathBuf, PathBuf)>,
}

impl NodeModulesGuard {
    fn new(source_dir: &Path, ignore: Option<&Gitignore>) -> Result<Self> {
        let Some(ignore) = ignore else {
            return Ok(Self { moves: Vec::new() });
        };

        let mut moves = Vec::new();
        let mut it = walkdir::WalkDir::new(source_dir).into_iter();
        while let Some(entry) = it.next() {
            let entry = entry.map_err(|e| CapsuleError::Pack(format!("Walk error: {}", e)))?;
            if !entry.file_type().is_dir() {
                continue;
            }
            if entry.file_name() != "node_modules" {
                continue;
            }

            let path = entry.path();
            if ignore.matched_path_or_any_parents(path, true).is_ignore() {
                let backup = unique_backup_path(path)?;
                fs::rename(path, &backup).map_err(|e| {
                    CapsuleError::Pack(format!("Failed to move {}: {}", path.display(), e))
                })?;
                moves.push((path.to_path_buf(), backup));
                it.skip_current_dir();
            }
        }

        Ok(Self { moves })
    }
}

impl Drop for NodeModulesGuard {
    fn drop(&mut self) {
        for (original, backup) in self.moves.drain(..) {
            if backup.exists() && !original.exists() {
                let _ = fs::rename(&backup, &original);
            }
        }
    }
}

fn unique_backup_path(original: &Path) -> Result<PathBuf> {
    let parent = original.parent().ok_or_else(|| {
        CapsuleError::Pack(format!(
            "Failed to resolve parent for {}",
            original.display()
        ))
    })?;
    let name = original
        .file_name()
        .ok_or_else(|| {
            CapsuleError::Pack(format!("Failed to resolve name for {}", original.display()))
        })?
        .to_string_lossy();
    for idx in 0..100 {
        let suffix = if idx == 0 {
            "capsule-bak".to_string()
        } else {
            format!("capsule-bak-{}", idx)
        };
        let candidate = parent.join(format!("{}.{}", name, suffix));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }

    Err(CapsuleError::Pack(format!(
        "Failed to allocate backup path for {}",
        original.display()
    )))
}

fn compress_with_zstd(data: &[u8], level: i32) -> Result<Vec<u8>> {
    zstd::encode_all(data, level)
        .map_err(|e| CapsuleError::Pack(format!("Failed to compress with Zstd: {}", e)))
}

fn find_nacelle_binary(explicit_path: Option<&PathBuf>) -> Result<PathBuf> {
    if let Some(path) = explicit_path {
        if path.exists() {
            return Ok(path.clone());
        }
    }

    if let Ok(env_path) = std::env::var("NACELLE_PATH") {
        let p = PathBuf::from(env_path);
        if p.exists() {
            return Ok(p);
        }
    }

    let exe = std::env::current_exe()
        .map_err(|e| CapsuleError::Pack(format!("Failed to resolve current exe path: {}", e)))?;
    if let Some(dir) = exe.parent() {
        let candidate = dir.join("nacelle");
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    if let Ok(path) = which::which("nacelle") {
        return Ok(path);
    }

    let current_exe = std::env::current_exe()?;
    if current_exe.is_file() {
        return Ok(current_exe);
    }

    if let Some(target_dir) = current_exe.parent().and_then(|p| p.parent()) {
        let release_bin = target_dir.join("release").join("nacelle");
        if release_bin.exists() {
            return Ok(release_bin);
        }

        let debug_bin = target_dir.join("debug").join("nacelle");
        if debug_bin.exists() {
            return Ok(debug_bin);
        }
    }

    Err(CapsuleError::Pack(
        "Could not find nacelle binary. Please either:\n\
         1. Set NACELLE_PATH environment variable\n\
         2. Run 'cargo build --release' in the nacelle directory\n\
         3. Install nacelle to your PATH"
            .to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::{build_bundle, create_bundle_archive, PackBundleArgs};
    use std::collections::BTreeSet;
    use tar::Archive;

    #[test]
    fn build_bundle_with_stub_nacelle() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();

        let source_dir = root.join("source");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::write(source_dir.join("hello.sh"), "#!/bin/sh\necho ok\n").unwrap();

        let manifest = r#"
schema_version = "0.3"
name = "bundle-test"
version = "0.1.0"
type = "app"

runtime = "source"
run = "hello.sh""#;
        let manifest_path = root.join("capsule.toml");
        std::fs::write(&manifest_path, manifest).unwrap();

        let nacelle_stub = root.join("nacelle");
        std::fs::write(&nacelle_stub, b"nacelle-stub").unwrap();

        let output = root.join("bundle.out");
        let bundle_path = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(build_bundle(
                PackBundleArgs {
                    manifest_path: Some(manifest_path),
                    workspace_root: root.to_path_buf(),
                    compat_input: None,
                    runtime_path: None,
                    output: Some(output.clone()),
                    nacelle_path: Some(nacelle_stub),
                },
                std::sync::Arc::new(crate::reporter::NoOpReporter),
            ))
            .unwrap();

        assert_eq!(bundle_path, output);
        assert!(bundle_path.exists());
        let size = std::fs::metadata(&bundle_path).unwrap().len();
        assert!(size > 0);
    }

    #[test]
    fn create_bundle_archive_places_uv_lock_under_source() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();

        let runtime_dir = root.join("runtime");
        std::fs::create_dir_all(&runtime_dir).unwrap();

        let source_dir = root.join("source");
        std::fs::create_dir_all(&source_dir).unwrap();
        let manifest = r#"
schema_version = "0.3"
name = "bundle-test"
version = "0.1.0"
type = "app"

runtime = "source/python"
runtime_version = "3.11.10"
dependencies = "requirements.txt"
run = "main.py""#;
        let manifest_path = root.join("capsule.toml");
        std::fs::write(&manifest_path, manifest).unwrap();
        std::fs::write(source_dir.join("main.py"), "print('ok')\n").unwrap();
        std::fs::write(source_dir.join("requirements.txt"), "fastapi==0.115.0\n").unwrap();
        std::fs::write(
            source_dir.join("uv.lock"),
            "version = 1\nrevision = 1\nrequires-python = \">=3.11\"\n",
        )
        .unwrap();
        let filter = crate::packers::pack_filter::load_pack_filter_from_path(&manifest_path)
            .expect("filter");

        let archive = create_bundle_archive(&runtime_dir, &source_dir, None, &filter, None, None)
            .expect("archive");

        let mut tar = Archive::new(std::io::Cursor::new(archive));
        let entries = tar
            .entries()
            .expect("entries")
            .map(|entry| {
                entry
                    .expect("entry")
                    .path()
                    .expect("path")
                    .to_string_lossy()
                    .to_string()
            })
            .collect::<BTreeSet<_>>();

        assert!(entries.contains("main.py"), "entries={entries:?}");
        assert!(entries.contains("source/uv.lock"), "entries={entries:?}");
    }
}
