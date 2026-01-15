use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::common::constants::BUNDLE_MAGIC;

/// Check if the given executable path contains an embedded bundle.
pub fn is_self_extracting_bundle(exe_path: &Path) -> Result<bool> {
    let file_data = std::fs::read(exe_path)
        .with_context(|| format!("Failed to read executable: {}", exe_path.display()))?;
    Ok(is_self_extracting_bundle_bytes(&file_data))
}

pub fn is_self_extracting_bundle_bytes(file_data: &[u8]) -> bool {
    if file_data.len() < BUNDLE_MAGIC.len() + 8 {
        return false;
    }

    let magic_start = file_data.len() - BUNDLE_MAGIC.len() - 8;
    &file_data[magic_start..magic_start + BUNDLE_MAGIC.len()] == BUNDLE_MAGIC
}

/// Extract the embedded bundle from an executable into the destination directory.
pub fn extract_bundle_to_dir(exe_path: &Path, dest: &Path) -> Result<()> {
    let file_data = std::fs::read(exe_path)
        .with_context(|| format!("Failed to read executable: {}", exe_path.display()))?;
    let decompressed = extract_bundle_bytes(&file_data)?;

    use tar::Archive;
    let mut archive = Archive::new(decompressed.as_slice());
    archive
        .unpack(dest)
        .context("Failed to unpack bundle tar")?;
    Ok(())
}

/// Parse and decompress the embedded bundle bytes from an executable image.
pub fn extract_bundle_bytes(file_data: &[u8]) -> Result<Vec<u8>> {
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

    let bundle_start = magic_start
        .checked_sub(bundle_size)
        .ok_or_else(|| anyhow::anyhow!("Invalid bundle size"))?;

    let compressed = &file_data[bundle_start..magic_start];
    zstd::decode_all(compressed).context("Failed to decompress bundle")
}

/// Read entrypoint from capsule.toml.
///
/// Prefers `targets.source.entrypoint` (UARC V1.1+), falls back to legacy `execution.entrypoint`.
pub fn read_entrypoint_from_manifest(manifest_path: &Path) -> Result<String> {
    let manifest_content = std::fs::read_to_string(manifest_path)
        .with_context(|| format!("Failed to read manifest: {}", manifest_path.display()))?;
    let manifest: toml::Value = toml::from_str(&manifest_content)
        .with_context(|| format!("Failed to parse manifest TOML: {}", manifest_path.display()))?;

    let entrypoint = manifest
        .get("targets")
        .and_then(|t| t.get("source"))
        .and_then(|s| s.get("entrypoint"))
        .and_then(|e| e.as_str())
        .or_else(|| {
            manifest.get("execution").and_then(|e| {
                e.get("release")
                    .and_then(|p| p.get("entrypoint"))
                    .or_else(|| e.get("entrypoint"))
                    .and_then(|e| e.as_str())
            })
        })
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("No entrypoint defined in capsule.toml"))?;

    Ok(entrypoint.to_string())
}

/// Read `targets.source.language` from capsule.toml when present.
pub fn read_source_language_from_manifest(manifest_path: &Path) -> Result<Option<String>> {
    let manifest_content = std::fs::read_to_string(manifest_path)
        .with_context(|| format!("Failed to read manifest: {}", manifest_path.display()))?;
    let manifest: toml::Value = toml::from_str(&manifest_content)
        .with_context(|| format!("Failed to parse manifest TOML: {}", manifest_path.display()))?;

    Ok(manifest
        .get("targets")
        .and_then(|t| t.get("source"))
        .and_then(|s| s.get("language"))
        .and_then(|l| l.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty()))
}

fn normalize_language_alias(lang: &str) -> String {
    let l = lang.trim().to_ascii_lowercase();
    match l.as_str() {
        "python3" => "python".to_string(),
        "nodejs" => "node".to_string(),
        _ => l,
    }
}

fn looks_like_python_file(entrypoint: &str, source_dir: &Path) -> bool {
    let ep = entrypoint.trim();
    if ep.ends_with(".py") {
        return true;
    }

    let candidate = source_dir.join(ep);
    candidate.is_file()
        && candidate
            .extension()
            .and_then(|s| s.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("py"))
            .unwrap_or(false)
}

fn resolve_program(program: &str, source_dir: &Path) -> String {
    if program.starts_with("./") || program.starts_with("../") || program.contains('/') {
        let candidate = source_dir.join(program);
        if candidate.exists() {
            return candidate.to_string_lossy().to_string();
        }
    }
    program.to_string()
}

/// Build the command used to execute an extracted bundle.
///
/// For Python entrypoints, we intentionally prefer the embedded runtime under `runtime_dir`.
pub fn build_bundle_command(
    language: Option<&str>,
    entrypoint: &str,
    source_dir: &Path,
    runtime_dir: &Path,
) -> Result<std::process::Command> {
    let language = language.map(normalize_language_alias);

    // Preferred path: manifest declares a source language, so treat entrypoint as a file.
    if let Some(lang) = language.as_deref() {
        match lang {
            "python" => {
                let python_bin = find_python_binary(runtime_dir)?;
                let entrypoint_path = source_dir.join(entrypoint);
                let mut cmd = std::process::Command::new(&python_bin);
                cmd.arg(&entrypoint_path);
                cmd.current_dir(source_dir);
                cmd.env("PYTHONHOME", runtime_dir.join("python"));
                cmd.env("PYTHONPATH", source_dir);
                return Ok(cmd);
            }
            "node" => {
                let node_bin = find_binary_recursive(runtime_dir, &["node", "node.exe"])?;
                let entrypoint_path = source_dir.join(entrypoint);
                let mut cmd = std::process::Command::new(&node_bin);
                cmd.arg(&entrypoint_path);
                cmd.current_dir(source_dir);
                return Ok(cmd);
            }
            "deno" => {
                let deno_bin = find_binary_recursive(runtime_dir, &["deno", "deno.exe"])?;
                let entrypoint_path = source_dir.join(entrypoint);
                let mut cmd = std::process::Command::new(&deno_bin);
                cmd.arg("run");
                cmd.arg(&entrypoint_path);
                cmd.current_dir(source_dir);
                return Ok(cmd);
            }
            "bun" => {
                let bun_bin = find_binary_recursive(runtime_dir, &["bun", "bun.exe"])?;
                let entrypoint_path = source_dir.join(entrypoint);
                let mut cmd = std::process::Command::new(&bun_bin);
                cmd.arg(&entrypoint_path);
                cmd.current_dir(source_dir);
                return Ok(cmd);
            }
            _ => {
                // Unknown language: fall back below.
            }
        }
    }

    // Fallback path: best-effort compatibility for older bundles.
    if looks_like_python_file(entrypoint, source_dir) {
        let python_bin = find_python_binary(runtime_dir)?;
        let entrypoint_path = source_dir.join(entrypoint);

        let mut cmd = std::process::Command::new(&python_bin);
        cmd.arg(&entrypoint_path);
        cmd.current_dir(source_dir);
        cmd.env("PYTHONHOME", runtime_dir.join("python"));
        cmd.env("PYTHONPATH", source_dir);
        return Ok(cmd);
    }

    let parts = shell_words::split(entrypoint).unwrap_or_else(|_| vec![entrypoint.to_string()]);
    let program = parts
        .first()
        .map(|s| s.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("No entrypoint defined in capsule.toml"))?;

    let resolved_program = resolve_program(program, source_dir);

    let mut cmd = std::process::Command::new(resolved_program);
    if parts.len() > 1 {
        cmd.args(&parts[1..]);
    }
    cmd.current_dir(source_dir);
    Ok(cmd)
}

/// Find Python binary in extracted runtime.
///
/// Expected layout is `runtime/python/bin/python3` (from python-build-standalone install_only).
pub fn find_python_binary(runtime_dir: &Path) -> Result<PathBuf> {
    for entry in std::fs::read_dir(runtime_dir)
        .with_context(|| format!("Failed to read runtime dir: {}", runtime_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            let bin_dir = path.join("bin");
            if bin_dir.exists() {
                for name in ["python3", "python"] {
                    let python_path = bin_dir.join(name);
                    if python_path.exists() {
                        return Ok(python_path);
                    }
                }
            }
        }
    }

    anyhow::bail!("Python binary not found in runtime directory")
}

fn find_binary_recursive(runtime_dir: &Path, candidates: &[&str]) -> Result<PathBuf> {
    for candidate in candidates {
        let direct = runtime_dir.join(candidate);
        if direct.is_file() {
            return Ok(direct);
        }
    }

    fn walk(dir: &Path, candidates: &[&str]) -> std::io::Result<Option<PathBuf>> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                if let Some(found) = walk(&path, candidates)? {
                    return Ok(Some(found));
                }
                continue;
            }
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                if candidates.iter().any(|c| c.eq_ignore_ascii_case(name)) {
                    return Ok(Some(path));
                }
            }
        }
        Ok(None)
    }

    match walk(runtime_dir, candidates).context("Failed to search runtime directory")? {
        Some(p) => Ok(p),
        None => anyhow::bail!(
            "Binary not found in runtime directory: {} (candidates={:?})",
            runtime_dir.display(),
            candidates
        ),
    }
}
