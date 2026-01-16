//! v0.2.0 Pack command - Self-extracting binary bundler
//!
//! Creates Tauri-style self-contained executables:
//! 1. Pack runtime + user code + assets into tar.zst
//! 2. Append to nacelle binary with magic bytes
//! 3. Result: Single executable with no external dependencies

use anyhow::{Context, Result};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use nacelle::runtime::source::toolchain::RuntimeFetcher;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tar::Builder;

/// Magic bytes to identify self-extracting bundle
const BUNDLE_MAGIC: &[u8] = nacelle::common::constants::BUNDLE_MAGIC;

/// Arguments for the v0.2.0 pack command
pub struct PackV2Args {
    pub manifest_path: PathBuf,
    pub runtime_path: Option<PathBuf>,
    pub output: Option<PathBuf>,
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

#[allow(dead_code)]
fn is_internal_mode() -> bool {
    std::env::var_os("NACELLE_INTERNAL").is_some()
}

macro_rules! user_out {
    ($($arg:tt)*) => {
        if is_internal_mode() {
            eprintln!($($arg)*);
        } else {
            println!($($arg)*);
        }
    };
}

/// Create a self-extracting bundle
#[allow(dead_code)]
pub async fn execute(args: PackV2Args) -> Result<()> {
    user_out!("📦 Building self-extracting bundle (v0.2.0)...");

    let output_path = build_bundle(args).await?;

    user_out!("✅ Self-extracting bundle created!");
    user_out!("Output: {}", output_path.display());
    user_out!("Size: {} MB", fs::metadata(&output_path)?.len() / 1_048_576);
    user_out!("\n💡 Deploy this single binary - no dependencies needed!");

    Ok(())
}

/// Build a self-extracting bundle and return the output path.
///
/// This helper is intentionally quiet (no stdout), so it can be reused by
/// machine-oriented interfaces like `nacelle internal pack`.
pub async fn build_bundle(args: PackV2Args) -> Result<PathBuf> {
    // 1. Determine paths
    let manifest_path = args.manifest_path.canonicalize()?;
    let source_dir = manifest_path
        .parent()
        .context("Failed to determine source directory")?;

    let output_path = args
        .output
        .unwrap_or_else(|| source_dir.join("nacelle-bundle"));

    // 2. Decide whether we need to bundle a runtime.
    // We use targets.source.{language,version,entrypoint} when present (preferred),
    // and fall back to legacy execution.entrypoint + heuristics.
    let source_target_hint = read_manifest_source_target_hint(&manifest_path)?;
    let manifest_entrypoint =
        read_manifest_entrypoint(&manifest_path, source_target_hint.as_ref())?
            .unwrap_or_else(|| "".to_string());

    let runtime_to_bundle = decide_runtime_to_bundle(
        &manifest_path,
        source_dir,
        &manifest_entrypoint,
        source_target_hint.as_ref(),
    )?;

    // 3. Find/download runtime (Python/Node/Bun/Deno) or create an empty runtime directory.
    let mut temp_runtime_dir: Option<PathBuf> = None;

    let runtime_dir = if let Some(runtime) = args.runtime_path {
        runtime
    } else if let Some(spec) = &runtime_to_bundle {
        // Delegate cache lookup + download behavior to RuntimeFetcher.
        // This keeps cache location consistent across Engine/CLI.
        let fetcher = RuntimeFetcher::new()?;
        let version = runtime_version_for(spec.language.as_str(), spec.version.as_deref());
        match spec.language.as_str() {
            "python" => {
                eprintln!("✓ Ensuring Python {} runtime is available...", version);
                fetcher.download_python_runtime(&version).await?
            }
            "node" => {
                eprintln!("✓ Ensuring Node {} runtime is available...", version);
                fetcher.download_node_runtime(&version).await?
            }
            "deno" => {
                eprintln!("✓ Ensuring Deno {} runtime is available...", version);
                fetcher.download_deno_runtime(&version).await?
            }
            "bun" => {
                eprintln!("✓ Ensuring Bun {} runtime is available...", version);
                fetcher.download_bun_runtime(&version).await?
            }
            other => anyhow::bail!("Unsupported runtime language for bundling: {}", other),
        }
    } else {
        // Non-Python workload: bundle an empty runtime directory.
        let dir =
            std::env::temp_dir().join(format!("nacelle-empty-runtime-{}", std::process::id()));
        fs::create_dir_all(&dir)?;
        temp_runtime_dir = Some(dir.clone());
        dir
    };

    if let Some(spec) = &runtime_to_bundle {
        let version = runtime_version_for(spec.language.as_str(), spec.version.as_deref());
        eprintln!(
            "✓ Using runtime: {:?} ({} {})",
            runtime_dir, spec.language, version
        );
    } else {
        if let Some(hint) = &source_target_hint {
            eprintln!(
                "✓ No runtime bundled (targets.source.language = {}).",
                hint.language
            );
        } else {
            eprintln!(
                "✓ No runtime bundled (entrypoint: {:?})",
                manifest_entrypoint
            );
        }
        eprintln!("ℹ️  Note: This bundle will require the entrypoint runtime to be available on the target host.");
    }

    let runtime_alias = build_runtime_alias(runtime_to_bundle.as_ref(), &runtime_dir)?;

    // 4. Create archive with runtime + source
    eprintln!("✓ Creating bundle archive...");
    let build_excludes = read_build_exclude_patterns(&manifest_path)?;
    let source_ignore = load_capsuleignore(source_dir, &build_excludes)?;
    let config_path = source_dir.join("config.json");
    let config_ref = if config_path.exists() {
        Some(config_path.as_path())
    } else {
        None
    };
    let archive_data = create_bundle_archive(
        &runtime_dir,
        source_dir,
        source_ignore.as_ref(),
        config_ref,
        runtime_alias.as_ref(),
    )?;
    eprintln!("✓ Archive size: {} MB", archive_data.len() / 1_048_576);

    if let Some(dir) = temp_runtime_dir {
        let _ = fs::remove_dir_all(dir);
    }

    // 5. Compress with Zstd (Level 19 for maximum compression)
    eprintln!("✓ Compressing with Zstd Level 19...");
    let compressed = compress_with_zstd(&archive_data, 19)?;
    eprintln!("✓ Compressed size: {} MB", compressed.len() / 1_048_576);
    eprintln!(
        "  Compression ratio: {:.1}%",
        (compressed.len() as f64 / archive_data.len() as f64) * 100.0
    );

    // 6. Find nacelle runtime binary
    eprintln!("✓ Creating self-extracting executable...");

    // Look for nacelle binary in standard locations
    let nacelle_bin = find_nacelle_binary()?;
    eprintln!(
        "✓ Using nacelle binary: {:?} ({} KB)",
        nacelle_bin,
        fs::metadata(&nacelle_bin)?.len() / 1024
    );

    // 7. Create output file with nacelle binary + compressed bundle
    let mut output = fs::File::create(&output_path)?;

    // Copy nacelle binary
    let nacelle_data = fs::read(&nacelle_bin)?;
    output.write_all(&nacelle_data)?;

    // Append compressed bundle
    output.write_all(&compressed)?;

    // Append magic bytes and size
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
    manifest_path: &Path,
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

    let manifest_dir = manifest_path
        .parent()
        .context("Failed to resolve manifest directory")?;
    let entry_path = resolve_entrypoint_path(entrypoint, manifest_dir, source_dir);

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

fn read_manifest_source_target_hint(manifest_path: &Path) -> Result<Option<SourceTargetHint>> {
    let raw = fs::read_to_string(manifest_path)
        .with_context(|| format!("Failed to read manifest: {}", manifest_path.display()))?;

    let manifest: toml::Value = toml::from_str(&raw)
        .with_context(|| format!("Failed to parse manifest TOML: {}", manifest_path.display()))?;

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
        .get("version")
        .and_then(|v| v.as_str())
        .map(|v| v.to_string());

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

    let raw = fs::read_to_string(manifest_path)
        .with_context(|| format!("Failed to read manifest: {}", manifest_path.display()))?;

    let manifest: toml::Value = toml::from_str(&raw)
        .with_context(|| format!("Failed to parse manifest TOML: {}", manifest_path.display()))?;

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

    let source_path = runtime_dir.to_path_buf();
    let version = runtime_version_for(runtime.language.as_str(), runtime.version.as_deref());
    let archive_path = format!("runtime/{}/{}", runtime.language, version);

    Ok(Some(RuntimeAlias {
        archive_path,
        source_path,
    }))
}

fn runtime_version_for(language: &str, version: Option<&str>) -> String {
    if let Some(v) = version {
        return v.to_string();
    }

    match language {
        "python" => "3.11".to_string(),
        "node" => "20".to_string(),
        "deno" => "1.40".to_string(),
        "bun" => "1.1".to_string(),
        _ => "latest".to_string(),
    }
}

fn create_bundle_archive(
    runtime_dir: &Path,
    source_dir: &Path,
    source_ignore: Option<&Gitignore>,
    config_path: Option<&Path>,
    runtime_alias: Option<&RuntimeAlias>,
) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    {
        let mut builder = Builder::new(&mut data);

        if let Some(alias) = runtime_alias {
            append_dir(&mut builder, &alias.source_path, &alias.archive_path, None)?;
        } else {
            append_dir(&mut builder, runtime_dir, "runtime", None)?;
        }

        append_dir(&mut builder, source_dir, "source", source_ignore)?;

        if let Some(config_path) = config_path {
            append_file(&mut builder, config_path, "config.json")?;
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
) -> Result<()> {
    for entry in ignore::WalkBuilder::new(dir)
        .hidden(false)
        .git_ignore(false)
        .git_exclude(false)
        .git_global(false)
        .ignore(false)
        .build()
    {
        let entry = entry?;
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

        let target = if prefix.is_empty() {
            rel.to_path_buf()
        } else {
            PathBuf::from(prefix).join(rel)
        };

        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            builder.append_dir(target, path)?;
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
    let raw = fs::read_to_string(manifest_path)
        .with_context(|| format!("Failed to read manifest: {}", manifest_path.display()))?;

    let manifest: toml::Value = toml::from_str(&raw)
        .with_context(|| format!("Failed to parse manifest TOML: {}", manifest_path.display()))?;

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
        builder.add_line(None, pattern)?;
    }

    let gitignore = builder.build()?;
    Ok(Some(gitignore))
}

fn compress_with_zstd(data: &[u8], level: i32) -> Result<Vec<u8>> {
    zstd::encode_all(data, level).context("Failed to compress with Zstd")
}

#[allow(dead_code)]
fn decompress_bundle(data: &[u8]) -> Result<Vec<u8>> {
    zstd::decode_all(data).context("Failed to decompress bundle")
}

fn find_nacelle_binary() -> Result<PathBuf> {
    // 1) Use NACELLE_PATH if provided
    if let Ok(env_path) = std::env::var("NACELLE_PATH") {
        let p = PathBuf::from(env_path);
        if p.exists() {
            return Ok(p);
        }
    }

    // 2) Look next to current executable
    let exe = std::env::current_exe().context("Failed to resolve current exe path")?;
    if let Some(dir) = exe.parent() {
        let candidate = dir.join("nacelle");
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    // 3) Look in PATH (best-effort)
    if let Ok(path) = which::which("nacelle") {
        return Ok(path);
    }

    // 4) Fall back to current executable if it looks like nacelle
    let current_exe = std::env::current_exe()?;
    if current_exe.is_file() {
        return Ok(current_exe);
    }

    // 5) Try to find in workspace target directory
    if let Some(target_dir) = current_exe.parent().and_then(|p| p.parent()) {
        // Check release first (preferred for smaller size)
        let release_bin = target_dir.join("release").join("nacelle");
        if release_bin.exists() {
            return Ok(release_bin);
        }

        // Fall back to debug
        let debug_bin = target_dir.join("debug").join("nacelle");
        if debug_bin.exists() {
            return Ok(debug_bin);
        }
    }

    // 6) Try which command
    if let Ok(output) = std::process::Command::new("which").arg("nacelle").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Ok(PathBuf::from(path));
            }
        }
    }

    anyhow::bail!(
        "Could not find nacelle binary. Please either:\n\
         1. Set NACELLE_BINARY environment variable\n\
         2. Run 'cargo build --release' in the nacelle directory\n\
         3. Install nacelle to your PATH"
    )
}

/// Extract bundle from a self-contained executable
#[allow(dead_code)]
pub fn extract_bundle(executable_path: &Path) -> Result<Vec<u8>> {
    let file_data = fs::read(executable_path)?;

    // Read magic bytes and size from the end
    let len = file_data.len();
    if len < BUNDLE_MAGIC.len() + 8 {
        anyhow::bail!("File too small to contain a bundle");
    }

    let magic_start = len - BUNDLE_MAGIC.len() - 8;
    let magic = &file_data[magic_start..magic_start + BUNDLE_MAGIC.len()];

    if magic != BUNDLE_MAGIC {
        anyhow::bail!("Not a self-extracting bundle (magic bytes not found)");
    }

    let size_bytes = &file_data[len - 8..len];
    let bundle_size = u64::from_le_bytes(size_bytes.try_into()?) as usize;

    let bundle_start = magic_start - bundle_size;
    let compressed = &file_data[bundle_start..magic_start];

    // Decompress
    zstd::decode_all(compressed).context("Failed to decompress bundle")
}
