//! v2.0 Pack command - Self-extracting binary bundler
//!
//! Creates Tauri-style self-contained executables:
//! 1. Pack runtime + user code + assets into tar.zst
//! 2. Append to capsuled binary with magic bytes
//! 3. Result: Single executable with no external dependencies

use anyhow::{Context, Result};
use nacelle::runtime::source::toolchain::RuntimeFetcher;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tar::Builder;

/// Magic bytes to identify self-extracting bundle
const BUNDLE_MAGIC: &[u8] = nacelle::bundle::BUNDLE_MAGIC;

/// Arguments for the v2.0 pack command
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
pub async fn execute(args: PackV2Args) -> Result<()> {
    user_out!("📦 Building self-extracting bundle (v2.0)...");

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
    let manifest_entrypoint = read_manifest_entrypoint(&manifest_path, source_target_hint.as_ref())?
        .unwrap_or_else(|| "".to_string());

    let needs_python = manifest_needs_python(
        &manifest_path,
        source_dir,
        &manifest_entrypoint,
        source_target_hint.as_ref(),
    )?;

    let python_version = if needs_python {
        source_target_hint
            .as_ref()
            .and_then(|h| {
                if h.language.eq_ignore_ascii_case("python") {
                    h.version.as_deref().and_then(normalize_python_version_hint)
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "3.11".to_string())
    } else {
        "3.11".to_string()
    };

    // 3. Find/download runtime (Python) or create an empty runtime directory.
    let mut temp_runtime_dir: Option<PathBuf> = None;

    let runtime_dir = if let Some(runtime) = args.runtime_path {
        runtime
    } else if needs_python {
        // Delegate cache lookup + download behavior to RuntimeFetcher.
        // This keeps cache location consistent across Engine/CLI.
        eprintln!("✓ Ensuring Python {} runtime is available...", python_version);
        let fetcher = RuntimeFetcher::new()?;
        fetcher.download_python_runtime(&python_version).await?
    } else {
        // Non-Python workload: bundle an empty runtime directory.
        let dir = std::env::temp_dir().join(format!(
            "nacelle-empty-runtime-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir)?;
        temp_runtime_dir = Some(dir.clone());
        dir
    };

    if needs_python {
        eprintln!("✓ Using runtime: {:?}", runtime_dir);
    } else {
        if let Some(hint) = &source_target_hint {
            eprintln!(
                "✓ No runtime bundled (targets.source.language = {}).",
                hint.language
            );
        } else {
            eprintln!("✓ No runtime bundled (non-Python entrypoint: {:?})", manifest_entrypoint);
        }
        eprintln!(
            "ℹ️  Note: This bundle will require the entrypoint program (e.g. bun/node) to be available on the target host."
        );
    }

    // 4. Create archive with runtime + source
    eprintln!("✓ Creating bundle archive...");
    let source_ignore = load_capsuleignore(source_dir)?;
    let archive_data = create_bundle_archive(&runtime_dir, source_dir, source_ignore.as_ref())?;
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

    let Some(source) = manifest
        .get("targets")
        .and_then(|t| t.get("source"))
    else {
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
    if candidate.is_file() {
        if candidate
            .extension()
            .and_then(|s| s.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("py"))
            .unwrap_or(false)
        {
            return Ok(true);
        }
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

fn load_capsuleignore(source_dir: &Path) -> Result<Option<Gitignore>> {
    let ignore_path = source_dir.join(".capsuleignore");
    if !ignore_path.exists() {
        return Ok(None);
    }

    let mut builder = GitignoreBuilder::new(source_dir);
    builder.add(ignore_path);
    let gitignore = builder
        .build()
        .context("Failed to parse .capsuleignore")?;
    Ok(Some(gitignore))
}

/// Create a tar archive containing runtime and source code
fn create_bundle_archive(
    runtime_dir: &Path,
    source_dir: &Path,
    source_ignore: Option<&Gitignore>,
) -> Result<Vec<u8>> {
    let mut archive = Builder::new(Vec::new());

    // Add runtime directory
    archive
        .append_dir_all("runtime", runtime_dir)
        .context("Failed to add runtime to archive")?;

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

/// Compress data using Zstd
fn compress_with_zstd(data: &[u8], level: i32) -> Result<Vec<u8>> {
    zstd::encode_all(data, level).context("Failed to compress with Zstd")
}

/// Find the nacelle runtime binary
fn find_nacelle_binary() -> Result<PathBuf> {
    // Priority order:
    // 1. NACELLE_BINARY environment variable
    // 2. Release build in workspace (../target/release/nacelle)
    // 3. Debug build in workspace (../target/debug/nacelle)
    // 4. System PATH

    // Check environment variable
    if let Ok(path) = std::env::var("NACELLE_BINARY") {
        let p = PathBuf::from(path);
        if p.exists() {
            return Ok(p);
        }
    }

    // Try to find in workspace target directory
    let current_exe = std::env::current_exe()?;
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
         2. Run 'cargo build --release' in the capsuled directory\n\
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
