//! v2.0 Pack command - Self-extracting binary bundler
//!
//! Creates Tauri-style self-contained executables:
//! 1. Pack runtime + user code + assets into tar.zst
//! 2. Append to capsuled binary with magic bytes
//! 3. Result: Single executable with no external dependencies

use anyhow::{Context, Result};
use nacelle::runtime::source::toolchain::RuntimeFetcher;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tar::Builder;

/// Magic bytes to identify self-extracting bundle
const BUNDLE_MAGIC: &[u8] = b"NACELLE_V2_BUNDLE";

/// Arguments for the v2.0 pack command
pub struct PackV2Args {
    pub manifest_path: PathBuf,
    pub runtime_path: Option<PathBuf>,
    pub output: Option<PathBuf>,
}

/// Create a self-extracting bundle
pub async fn execute(args: PackV2Args) -> Result<()> {
    println!("📦 Building self-extracting bundle (v2.0)...");

    // 1. Determine paths
    let manifest_path = args.manifest_path.canonicalize()?;
    let source_dir = manifest_path
        .parent()
        .context("Failed to determine source directory")?;

    let output_path = args
        .output
        .unwrap_or_else(|| source_dir.join("nacelle-bundle"));

    // 2. Find or download runtime
    let runtime_dir = if let Some(runtime) = args.runtime_path {
        runtime
    } else {
        // Try to find cached runtime, or download if not available
        let cache_dir = dirs::home_dir()
            .context("Failed to get home directory")?
            .join(".nacelle")
            .join("toolchain");

        // For now, use the first available Python runtime
        // TODO: Parse manifest to determine required runtime
        let runtime_path = if cache_dir.exists() {
            let entries = fs::read_dir(&cache_dir)?;
            let mut found = None;
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with("python-") {
                    found = Some(entry.path());
                    break;
                }
            }
            found
        } else {
            None
        };

        if let Some(path) = runtime_path {
            path
        } else {
            // Download Python 3.11 runtime
            println!("✓ No cached runtime found. Downloading Python 3.11...");
            let fetcher = RuntimeFetcher::new()?;
            fetcher.download_python_runtime("3.11").await?
        }
    };

    println!("✓ Using runtime: {:?}", runtime_dir);

    // 3. Create archive with runtime + source
    println!("✓ Creating bundle archive...");
    let archive_data = create_bundle_archive(&runtime_dir, source_dir)?;
    println!("✓ Archive size: {} MB", archive_data.len() / 1_048_576);

    // 4. Compress with Zstd (Level 19 for maximum compression)
    println!("✓ Compressing with Zstd Level 19...");
    let compressed = compress_with_zstd(&archive_data, 19)?;
    println!("✓ Compressed size: {} MB", compressed.len() / 1_048_576);
    println!(
        "  Compression ratio: {:.1}%",
        (compressed.len() as f64 / archive_data.len() as f64) * 100.0
    );

    // 5. Find nacelle runtime binary (not the CLI)
    println!("✓ Creating self-extracting executable...");
    
    // Look for nacelle binary in standard locations
    let nacelle_bin = find_nacelle_binary()?;
    println!("✓ Using nacelle binary: {:?} ({} KB)", nacelle_bin, fs::metadata(&nacelle_bin)?.len() / 1024);

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

    println!("✅ Self-extracting bundle created!");
    println!("Output: {}", output_path.display());
    println!("Size: {} MB", fs::metadata(&output_path)?.len() / 1_048_576);
    println!("\n💡 Deploy this single binary - no dependencies needed!");

    Ok(())
}

/// Create a tar archive containing runtime and source code
fn create_bundle_archive(runtime_dir: &Path, source_dir: &Path) -> Result<Vec<u8>> {
    let mut archive = Builder::new(Vec::new());

    // Add runtime directory
    archive
        .append_dir_all("runtime", runtime_dir)
        .context("Failed to add runtime to archive")?;

    // Add source directory
    archive
        .append_dir_all("source", source_dir)
        .context("Failed to add source to archive")?;

    archive.finish()?;
    Ok(archive.into_inner()?)
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
