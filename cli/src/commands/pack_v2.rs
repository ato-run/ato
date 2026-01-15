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
        match spec.language.as_str() {
            "python" => {
                eprintln!("✓ Ensuring Python {} runtime is available...", spec.version);
                fetcher.download_python_runtime(&spec.version).await?
            }
            "node" => {
                eprintln!("✓ Ensuring Node {} runtime is available...", spec.version);
                fetcher.download_node_runtime(&spec.version).await?
            }
            "deno" => {
                eprintln!("✓ Ensuring Deno {} runtime is available...", spec.version);
                fetcher.download_deno_runtime(&spec.version).await?
            }
            "bun" => {
                eprintln!("✓ Ensuring Bun {} runtime is available...", spec.version);
                fetcher.download_bun_runtime(&spec.version).await?
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
        eprintln!(
            "✓ Using runtime: {:?} ({} {})",
            runtime_dir, spec.language, spec.version
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

    // Copy base binary
    fs::copy(&nacelle_bin, &output_path).context("Failed to copy nacelle binary")?;

    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&output_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&output_path, perms)?;
    }

    // Append bundle with magic bytes
    let mut file = fs::OpenOptions::new().append(true).open(&output_path)?;

    file.write_all(&compressed)?;
    file.write_all(BUNDLE_MAGIC)?;
    file.write_all(&(compressed.len() as u64).to_le_bytes())?;

    Ok(output_path)
}

fn read_manifest_source_target_hint(manifest_path: &Path) -> Result<Option<SourceTargetHint>> {
    let content = fs::read_to_string(manifest_path)
        .with_context(|| format!("Failed to read manifest: {}", manifest_path.display()))?;
    let manifest: toml::Value = toml::from_str(&content)
        .with_context(|| format!("Failed to parse manifest TOML: {}", manifest_path.display()))?;

    let Some(source) = manifest.get("targets").and_then(|t| t.get("source")) else {
        return Ok(None);
    };

    let language = source
        .get("language")
        .and_then(|l| l.as_str())
        .map(|s| s.to_string());

    let language = match language {
        Some(l) => l,
        None => return Ok(None),
    };

    let version = source
        .get("version")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let entrypoint = source
        .get("entrypoint")
        .and_then(|e| e.as_str())
        .map(|s| s.to_string());

    Ok(Some(SourceTargetHint {
        language,
        version,
        entrypoint,
    }))
}

fn read_manifest_entrypoint(
    manifest_path: &Path,
    source_target_hint: Option<&SourceTargetHint>,
) -> Result<Option<String>> {
    // Prefer targets.source.entrypoint when present.
    if let Some(hint) = source_target_hint {
        if let Some(ep) = hint.entrypoint.as_deref() {
            let ep = ep.trim();
            if !ep.is_empty() {
                return Ok(Some(ep.to_string()));
            }
        }
    }

    // Fall back to legacy execution.entrypoint.
    let content = fs::read_to_string(manifest_path)
        .with_context(|| format!("Failed to read manifest: {}", manifest_path.display()))?;
    let manifest: toml::Value = toml::from_str(&content)
        .with_context(|| format!("Failed to parse manifest TOML: {}", manifest_path.display()))?;

    Ok(manifest
        .get("execution")
        .and_then(|e| e.get("entrypoint"))
        .and_then(|e| e.as_str())
        .map(|s| s.to_string()))
}

fn build_runtime_alias(
    runtime_spec: Option<&RuntimeBundleSpec>,
    runtime_dir: &Path,
) -> Result<Option<RuntimeAlias>> {
    let Some(spec) = runtime_spec else {
        return Ok(None);
    };

    let (archive_path, source_path) = match spec.language.as_str() {
        "python" => {
            let python = nacelle::bundle::find_python_binary(runtime_dir)?;
            ("runtime/python/bin/python3".to_string(), python)
        }
        "node" => {
            let node = find_binary_recursive(runtime_dir, &["node", "node.exe"])?;
            ("runtime/node/bin/node".to_string(), node)
        }
        "deno" => {
            let deno = find_binary_recursive(runtime_dir, &["deno", "deno.exe"])?;
            ("runtime/deno/bin/deno".to_string(), deno)
        }
        "bun" => {
            let bun = find_binary_recursive(runtime_dir, &["bun", "bun.exe"])?;
            ("runtime/bun/bin/bun".to_string(), bun)
        }
        _ => return Ok(None),
    };

    if runtime_dir
        .join(archive_path.replace("runtime/", ""))
        .exists()
    {
        return Ok(None);
    }

    Ok(Some(RuntimeAlias {
        archive_path,
        source_path,
    }))
}

fn manifest_needs_python(
    _manifest_path: &Path,
    source_dir: &Path,
    entrypoint: &str,
    source_target_hint: Option<&SourceTargetHint>,
) -> Result<bool> {
    // Canonical hint: targets.source.language is authoritative for bundling decisions.
    if let Some(hint) = source_target_hint {
        if hint.language.eq_ignore_ascii_case("python") {
            return Ok(true);
        }
        // Explicitly non-python: do not bundle python even if heuristics would.
        return Ok(false);
    }

    let ep = entrypoint.trim();
    if ep.is_empty() {
        return Ok(true);
    }

    // Heuristic: direct .py entrypoint file.
    if ep.ends_with(".py") {
        return Ok(true);
    }

    // If it's a path to an existing file under the source dir and looks like python.
    let candidate = source_dir.join(ep);
    if candidate.is_file()
        && candidate
            .extension()
            .and_then(|s| s.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("py"))
            .unwrap_or(false)
    {
        return Ok(true);
    }

    Ok(false)
}

fn normalize_python_version_hint(version: &str) -> Option<String> {
    let mut v = version.trim();
    for prefix in ["^", ">=", "==", "=", "~="] {
        if let Some(rest) = v.strip_prefix(prefix) {
            v = rest.trim();
            break;
        }
    }

    let mut out = String::new();
    for ch in v.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            out.push(ch);
        } else {
            break;
        }
    }

    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

#[derive(Debug, Clone)]
struct RuntimeBundleSpec {
    language: String,
    version: String,
}

fn normalize_language_alias(lang: &str) -> String {
    let l = lang.trim().to_ascii_lowercase();
    match l.as_str() {
        "python3" => "python".to_string(),
        "nodejs" => "node".to_string(),
        _ => l,
    }
}

fn decide_runtime_to_bundle(
    manifest_path: &Path,
    source_dir: &Path,
    manifest_entrypoint: &str,
    source_target_hint: Option<&SourceTargetHint>,
) -> Result<Option<RuntimeBundleSpec>> {
    // Prefer explicit targets.source.language/version when available.
    if let Some(hint) = source_target_hint {
        let language = normalize_language_alias(&hint.language);
        match language.as_str() {
            "python" => {
                let version = hint
                    .version
                    .as_deref()
                    .and_then(normalize_python_version_hint)
                    .unwrap_or_else(|| "3.11".to_string());
                return Ok(Some(RuntimeBundleSpec { language, version }));
            }
            "node" => {
                let version = hint
                    .version
                    .as_deref()
                    .map(|v| v.trim())
                    .filter(|v| !v.is_empty())
                    .unwrap_or("22")
                    .to_string();
                return Ok(Some(RuntimeBundleSpec { language, version }));
            }
            "deno" | "bun" => {
                let version = hint
                    .version
                    .as_deref()
                    .map(|v| v.trim())
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "targets.source.version is required when language = '{}' for bundling",
                            language
                        )
                    })?
                    .to_string();
                return Ok(Some(RuntimeBundleSpec { language, version }));
            }
            _ => {
                // Unknown language: fall back to heuristic.
            }
        }
    }

    // Heuristic fallback for legacy manifests (no targets.source).
    // Python: if entrypoint looks like python file.
    let needs_python = manifest_needs_python(
        manifest_path,
        source_dir,
        manifest_entrypoint,
        source_target_hint,
    )?;
    if needs_python {
        return Ok(Some(RuntimeBundleSpec {
            language: "python".to_string(),
            version: "3.11".to_string(),
        }));
    }

    // Node: entrypoint is a JS/TS file.
    let ep = manifest_entrypoint.trim();
    if ep.ends_with(".js") || ep.ends_with(".mjs") || ep.ends_with(".cjs") || ep.ends_with(".ts") {
        return Ok(Some(RuntimeBundleSpec {
            language: "node".to_string(),
            version: "22".to_string(),
        }));
    }

    Ok(None)
}

fn load_capsuleignore(source_dir: &Path, extra_patterns: &[String]) -> Result<Option<Gitignore>> {
    let ignore_path = source_dir.join(".capsuleignore");
    let mut builder = GitignoreBuilder::new(source_dir);

    let mut has_any = false;
    if ignore_path.exists() {
        builder.add(ignore_path);
        has_any = true;
    }

    for pattern in extra_patterns {
        let p = pattern.trim();
        if p.is_empty() {
            continue;
        }
        // GitignoreBuilder supports adding patterns directly (same syntax as .gitignore).
        builder
            .add_line(None, p)
            .with_context(|| format!("Failed to add exclude pattern: {}", p))?;
        has_any = true;
    }

    if !has_any {
        return Ok(None);
    }

    let gitignore = builder.build().context("Failed to parse .capsuleignore")?;
    Ok(Some(gitignore))
}

fn read_build_exclude_patterns(manifest_path: &Path) -> Result<Vec<String>> {
    let content = fs::read_to_string(manifest_path)
        .with_context(|| format!("Failed to read manifest: {}", manifest_path.display()))?;
    let manifest: toml::Value = toml::from_str(&content)
        .with_context(|| format!("Failed to parse manifest TOML: {}", manifest_path.display()))?;

    let mut patterns: Vec<String> = Vec::new();

    let build = manifest.get("build");
    let gpu = build
        .and_then(|b| b.get("gpu"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if let Some(list) = build
        .and_then(|b| b.get("exclude_libs"))
        .and_then(|v| v.as_array())
    {
        for item in list {
            if let Some(s) = item.as_str() {
                patterns.push(s.to_string());
            }
        }
    }

    // Optional sugar: if gpu=true and exclude_libs is empty, we do NOT add aggressive defaults.
    // Tooling (e.g. `capsule scaffold docker`) can propose defaults safely.
    // Keeping pack deterministic avoids surprising "missing dependency" failures.
    if gpu {
        // reserved for future heuristics
    }

    Ok(patterns)
}

/// Create a tar archive containing runtime and source code
fn create_bundle_archive(
    runtime_dir: &Path,
    source_dir: &Path,
    source_ignore: Option<&Gitignore>,
    config_path: Option<&Path>,
    runtime_alias: Option<&RuntimeAlias>,
) -> Result<Vec<u8>> {
    let mut archive = Builder::new(Vec::new());

    // Add runtime directory
    archive
        .append_dir_all("runtime", runtime_dir)
        .context("Failed to add runtime to archive")?;

    if let Some(alias) = runtime_alias {
        archive
            .append_path_with_name(&alias.source_path, &alias.archive_path)
            .with_context(|| {
                format!(
                    "Failed to add runtime alias {} -> {}",
                    alias.source_path.display(),
                    alias.archive_path
                )
            })?;
    }

    if let Some(config_path) = config_path {
        archive
            .append_path_with_name(config_path, "config.json")
            .with_context(|| {
                format!(
                    "Failed to add config.json to archive: {}",
                    config_path.display()
                )
            })?;
    }

    // Add source directory
    // Default behavior stays "include everything"; if `.capsuleignore` exists, we apply it.
    append_source_dir(&mut archive, source_dir, source_ignore)?;

    archive.finish()?;
    Ok(archive.into_inner()?)
}

fn append_source_dir(
    archive: &mut Builder<Vec<u8>>,
    source_dir: &Path,
    source_ignore: Option<&Gitignore>,
) -> Result<()> {
    for entry in ignore::WalkBuilder::new(source_dir)
        .hidden(false)
        .ignore(false)
        .git_ignore(false)
        .git_exclude(false)
        .parents(false)
        .build()
    {
        let entry = entry?;
        let path = entry.path();

        if path == source_dir {
            continue;
        }

        let relative = path
            .strip_prefix(source_dir)
            .unwrap_or(path)
            .to_string_lossy();

        if relative == "config.json" {
            continue;
        }

        if relative.starts_with(".git/") || relative == ".git" {
            continue;
        }

        if let Some(ignore) = source_ignore {
            let matched = ignore.matched_path_or_any_parents(path, path.is_dir());
            if matched.is_ignore() {
                continue;
            }
        }

        if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            let name_in_archive = format!("source/{}", relative);
            archive
                .append_path_with_name(path, name_in_archive)
                .with_context(|| format!("Failed to add file to archive: {}", path.display()))?;
        }
    }

    Ok(())
}

fn find_binary_recursive(runtime_dir: &Path, candidates: &[&str]) -> Result<PathBuf> {
    for candidate in candidates {
        let direct = runtime_dir.join(candidate);
        if direct.is_file() {
            return Ok(direct);
        }
    }

    let mut stack = vec![runtime_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)
            .with_context(|| format!("Failed to read runtime dir: {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                if candidates.contains(&name) {
                    return Ok(path);
                }
            }
        }
    }

    anyhow::bail!("Runtime binary not found in {:?}", runtime_dir)
}

/// Compress data using Zstd
fn compress_with_zstd(data: &[u8], level: i32) -> Result<Vec<u8>> {
    zstd::encode_all(data, level).context("Failed to compress with Zstd")
}

/// Find the nacelle runtime binary
fn find_nacelle_binary() -> Result<PathBuf> {
    // Priority order:
    // 1. NACELLE_BINARY environment variable
    // 2. Current executable (the running nacelle)
    // 3. Release build in workspace (../target/release/nacelle)
    // 4. Debug build in workspace (../target/debug/nacelle)
    // 5. System PATH

    // Check environment variable
    if let Ok(path) = std::env::var("NACELLE_BINARY") {
        let p = PathBuf::from(path);
        if p.exists() {
            return Ok(p);
        }
    }

    // Prefer the currently running nacelle binary.
    // This avoids embedding a stale release build (behavior mismatch).
    let current_exe = std::env::current_exe()?;
    if current_exe.is_file() {
        return Ok(current_exe);
    }

    // Try to find in workspace target directory
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

    // Try which command
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
