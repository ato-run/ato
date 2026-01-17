use anyhow::{Context, Result};
use std::path::Path;

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

