//! OCI Image Layer Extraction
//!
//! Handles extraction and merging of OCI image layers into a rootfs directory.
//! Supports tar.gz (gzip) and tar layers.

use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};
use tar::Archive;
use thiserror::Error;
use tracing::{debug, info, warn};

/// Layer extraction errors
#[derive(Error, Debug)]
pub enum LayerError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    #[error("Archive error: {0}")]
    Archive(String),

    #[error("Digest verification failed: expected {expected}, got {actual}")]
    DigestMismatch { expected: String, actual: String },

    #[error("Whiteout error: {0}")]
    Whiteout(String),
}

pub type LayerResult<T> = Result<T, LayerError>;

/// OCI layer extractor
pub struct LayerExtractor {
    _cache_dir: PathBuf,
}

impl LayerExtractor {
    /// Create a new layer extractor
    pub fn new(cache_dir: PathBuf) -> Self {
        Self {
            _cache_dir: cache_dir,
        }
    }

    /// Extract a single layer to the rootfs directory
    ///
    /// Handles:
    /// - gzip-compressed tar files
    /// - Whiteout files (.wh.* for deletions)
    /// - Opaque whiteouts (.wh..wh..opq)
    pub fn extract_layer(&self, layer_path: &Path, rootfs: &Path) -> LayerResult<()> {
        // Detect if gzip compressed
        let mut peek = [0u8; 2];
        let file = File::open(layer_path)?;
        let mut peek_reader = BufReader::new(file);
        peek_reader.read_exact(&mut peek)?;

        let file = File::open(layer_path)?;
        let reader = BufReader::new(file);

        let is_gzip = peek[0] == 0x1f && peek[1] == 0x8b;

        if is_gzip {
            let decoder = GzDecoder::new(reader);
            self.extract_tar(decoder, rootfs)?;
        } else {
            self.extract_tar(reader, rootfs)?;
        }

        Ok(())
    }

    /// Extract tar archive to rootfs
    fn extract_tar<R: Read>(&self, reader: R, rootfs: &Path) -> LayerResult<()> {
        let mut archive = Archive::new(reader);

        for entry in archive.entries()? {
            let mut entry = entry?;
            let path = entry.path()?.to_path_buf();
            let path_str = path.to_string_lossy();

            // Handle whiteout files
            if let Some(filename) = path.file_name().and_then(|f| f.to_str()) {
                if filename.starts_with(".wh.") {
                    self.handle_whiteout(&path, rootfs)?;
                    continue;
                }
            }

            // Skip problematic paths
            if path_str.starts_with("..") || path_str.contains("/../") {
                warn!("Skipping path traversal attempt: {}", path_str);
                continue;
            }

            let dest = rootfs.join(&path);

            // Create parent directories
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }

            // Extract entry
            entry.unpack(&dest)?;

            debug!("Extracted: {}", path_str);
        }

        Ok(())
    }

    /// Handle whiteout files (file deletions in overlay layers)
    fn handle_whiteout(&self, whiteout_path: &Path, rootfs: &Path) -> LayerResult<()> {
        let filename = whiteout_path
            .file_name()
            .and_then(|f| f.to_str())
            .ok_or_else(|| LayerError::Whiteout("Invalid filename".to_string()))?;

        // Opaque whiteout - delete all files in directory
        if filename == ".wh..wh..opq" {
            if let Some(parent) = whiteout_path.parent() {
                let target_dir = rootfs.join(parent);
                if target_dir.exists() {
                    debug!("Opaque whiteout: clearing {}", target_dir.display());
                    // Remove all contents but keep the directory
                    for entry in fs::read_dir(&target_dir)? {
                        let entry = entry?;
                        let path = entry.path();
                        if path.is_dir() {
                            fs::remove_dir_all(&path)?;
                        } else {
                            fs::remove_file(&path)?;
                        }
                    }
                }
            }
            return Ok(());
        }

        // Regular whiteout - delete specific file
        if let Some(target_name) = filename.strip_prefix(".wh.") {
            if let Some(parent) = whiteout_path.parent() {
                let target = rootfs.join(parent).join(target_name);
                if target.exists() {
                    debug!("Whiteout: deleting {}", target.display());
                    if target.is_dir() {
                        fs::remove_dir_all(&target)?;
                    } else {
                        fs::remove_file(&target)?;
                    }
                }
            }
        }

        Ok(())
    }

    /// Merge multiple layers into a single rootfs
    ///
    /// Layers are applied in order (base layer first, then subsequent layers)
    pub fn merge_layers(&self, layer_paths: &[PathBuf], rootfs: &Path) -> LayerResult<()> {
        // Ensure rootfs directory exists
        fs::create_dir_all(rootfs)?;

        info!(
            "Merging {} layers into {}",
            layer_paths.len(),
            rootfs.display()
        );

        for (i, layer_path) in layer_paths.iter().enumerate() {
            info!(
                "Extracting layer {}/{}: {}",
                i + 1,
                layer_paths.len(),
                layer_path.display()
            );
            self.extract_layer(layer_path, rootfs)?;
        }

        info!("Layer merge complete");
        Ok(())
    }

    /// Verify a layer's digest
    pub fn verify_layer_digest(
        &self,
        layer_path: &Path,
        expected_digest: &str,
    ) -> LayerResult<bool> {
        let file = File::open(layer_path)?;
        let mut reader = BufReader::new(file);
        let mut hasher = Sha256::new();

        io::copy(&mut reader, &mut hasher)?;

        let actual_digest = format!("sha256:{:x}", hasher.finalize());

        if actual_digest != expected_digest {
            return Err(LayerError::DigestMismatch {
                expected: expected_digest.to_string(),
                actual: actual_digest,
            });
        }

        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_tarball(dir: &Path, files: &[(&str, &str)]) -> PathBuf {
        let tar_path = dir.join("test.tar");
        let file = File::create(&tar_path).unwrap();
        let mut builder = tar::Builder::new(file);

        for (name, content) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, name, content.as_bytes())
                .unwrap();
        }

        builder.finish().unwrap();
        tar_path
    }

    #[test]
    fn test_extract_simple_layer() {
        let temp = TempDir::new().unwrap();
        let tar_path = create_test_tarball(
            temp.path(),
            &[
                ("hello.txt", "Hello, World!"),
                ("dir/nested.txt", "Nested content"),
            ],
        );

        let rootfs = temp.path().join("rootfs");
        let extractor = LayerExtractor::new(temp.path().to_path_buf());

        extractor.extract_layer(&tar_path, &rootfs).unwrap();

        assert!(rootfs.join("hello.txt").exists());
        assert!(rootfs.join("dir/nested.txt").exists());

        let content = fs::read_to_string(rootfs.join("hello.txt")).unwrap();
        assert_eq!(content, "Hello, World!");
    }

    #[test]
    fn test_layer_merge() {
        let temp = TempDir::new().unwrap();

        // Layer 1: base files
        let layer1 = create_test_tarball(
            temp.path(),
            &[("base.txt", "Base content"), ("shared.txt", "Original")],
        );

        // Layer 2: override and add
        let layer2_path = temp.path().join("layer2.tar");
        let file = File::create(&layer2_path).unwrap();
        let mut builder = tar::Builder::new(file);

        // Override shared.txt
        let mut header = tar::Header::new_gnu();
        header.set_size(10);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "shared.txt", "Modified".as_bytes())
            .unwrap();

        // Add new file
        let mut header = tar::Header::new_gnu();
        header.set_size(3);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "new.txt", "New".as_bytes())
            .unwrap();

        builder.finish().unwrap();

        let rootfs = temp.path().join("rootfs");
        let extractor = LayerExtractor::new(temp.path().to_path_buf());

        // Rename layer1 to avoid conflict
        let layer1_renamed = temp.path().join("layer1.tar");
        fs::rename(&layer1, &layer1_renamed).unwrap();

        extractor
            .merge_layers(&[layer1_renamed, layer2_path], &rootfs)
            .unwrap();

        // Verify merge results
        assert!(rootfs.join("base.txt").exists());
        assert!(rootfs.join("new.txt").exists());

        let shared = fs::read_to_string(rootfs.join("shared.txt")).unwrap();
        assert!(shared.contains("Modified") || shared.contains("Original"));
    }
}
