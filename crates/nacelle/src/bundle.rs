use anyhow::{Context, Result};
use std::io::Cursor;
use std::path::{Path, PathBuf};

use crate::common::constants::BUNDLE_MAGIC;

pub struct PreparedExtractionDir {
    path: PathBuf,
    _temp_dir: Option<tempfile::TempDir>,
}

impl PreparedExtractionDir {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn preserved(&self) -> bool {
        self._temp_dir.is_none()
    }
}

pub fn prepare_extraction_dir(keep_extracted: bool) -> Result<PreparedExtractionDir> {
    if keep_extracted {
        let unique_suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "nacelle-bundle-{}-{}",
            std::process::id(),
            unique_suffix
        ));
        std::fs::create_dir_all(&path).with_context(|| {
            format!(
                "Failed to create persistent bundle extraction dir: {}",
                path.display()
            )
        })?;
        return Ok(PreparedExtractionDir {
            path,
            _temp_dir: None,
        });
    }

    let temp_dir = tempfile::Builder::new().prefix("nacelle-").tempdir()?;
    let path = temp_dir.path().to_path_buf();
    Ok(PreparedExtractionDir {
        path,
        _temp_dir: Some(temp_dir),
    })
}

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
    validate_bundle_archive(&decompressed)?;

    use tar::Archive;
    let mut archive = Archive::new(Cursor::new(decompressed));
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

fn validate_bundle_archive(bundle_tar: &[u8]) -> Result<()> {
    let mut archive = tar::Archive::new(Cursor::new(bundle_tar));
    for entry in archive
        .entries()
        .context("Failed to read bundle tar entries")?
    {
        let entry = entry.context("Failed to parse bundle tar entry")?;
        let path = entry.path().context("Failed to read bundle tar path")?;
        validate_bundle_entry_path(path.as_ref())?;

        let entry_type = entry.header().entry_type();
        if !entry_type.is_file() && !entry_type.is_dir() {
            anyhow::bail!(
                "Unsupported bundle entry type for {}",
                path.to_string_lossy()
            );
        }
    }
    Ok(())
}

fn validate_bundle_entry_path(path: &Path) -> Result<()> {
    use std::path::Component;

    if path.is_absolute() {
        anyhow::bail!("Bundle archive contains absolute path: {}", path.display());
    }

    for component in path.components() {
        match component {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!("Bundle archive contains unsafe path: {}", path.display());
            }
            Component::CurDir | Component::Normal(_) => {}
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn make_embedded_bundle(bundle_payload: &[u8]) -> Vec<u8> {
        let compressed = zstd::encode_all(bundle_payload, 0).unwrap();
        let mut image = b"#!/fake/nacelle\n".to_vec();
        image.extend_from_slice(&compressed);
        image.extend_from_slice(BUNDLE_MAGIC);
        image.extend_from_slice(&(compressed.len() as u64).to_le_bytes());
        image
    }

    fn make_tar_payload() -> Vec<u8> {
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);

            let mut header = tar::Header::new_gnu();
            let body = br#"{"services":{"main":{"executable":"python3","args":[]}}}"#;
            header.set_path("config.json").unwrap();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append(&header, &body[..]).unwrap();
            builder.finish().unwrap();
        }
        tar_bytes
    }

    #[test]
    fn detects_self_extracting_bundle_bytes() {
        let image = make_embedded_bundle(&make_tar_payload());
        assert!(is_self_extracting_bundle_bytes(&image));
    }

    #[test]
    fn extracts_valid_bundle_bytes() {
        let payload = make_tar_payload();
        let image = make_embedded_bundle(&payload);

        let extracted = extract_bundle_bytes(&image).unwrap();
        assert_eq!(extracted, payload);
    }

    #[test]
    fn rejects_bundle_with_wrong_magic() {
        let mut image = make_embedded_bundle(&make_tar_payload());
        let magic_start = image.len() - BUNDLE_MAGIC.len() - 8;
        image[magic_start] ^= 0xFF;

        let err = extract_bundle_bytes(&image).unwrap_err();
        assert!(err.to_string().contains("magic bytes not found"));
    }

    #[test]
    fn rejects_bundle_with_invalid_size() {
        let mut image = make_embedded_bundle(&make_tar_payload());
        let len = image.len();
        image[len - 8..].copy_from_slice(&(u64::MAX).to_le_bytes());

        let err = extract_bundle_bytes(&image).unwrap_err();
        assert!(err.to_string().contains("Invalid bundle size"));
    }

    #[test]
    fn rejects_bundle_with_corrupt_payload() {
        let mut image = make_embedded_bundle(&make_tar_payload());
        let magic_start = image.len() - BUNDLE_MAGIC.len() - 8;
        image[magic_start - 1] ^= 0xAA;

        let err = extract_bundle_bytes(&image).unwrap_err();
        assert!(err.to_string().contains("Failed to decompress bundle"));
    }

    #[test]
    fn rejects_bundle_with_parent_dir_entry() {
        let err = validate_bundle_entry_path(Path::new("../escape.txt")).unwrap_err();
        assert!(err.to_string().contains("unsafe path"));
    }

    #[test]
    fn rejects_bundle_with_symlink_entry() {
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            let mut header = tar::Header::new_gnu();
            header.set_path("config.json").unwrap();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_link_name("../outside").unwrap();
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append(&header, Cursor::new(Vec::<u8>::new()))
                .unwrap();
            builder.finish().unwrap();
        }

        let err = validate_bundle_archive(&tar_bytes).unwrap_err();
        assert!(err.to_string().contains("Unsupported bundle entry type"));
    }

    #[test]
    fn temporary_extraction_dir_is_removed_on_drop() {
        let path = {
            let prepared = prepare_extraction_dir(false).unwrap();
            let path = prepared.path().to_path_buf();
            assert!(path.exists());
            path
        };

        assert!(!path.exists());
    }

    #[test]
    fn persistent_extraction_dir_survives_drop() {
        let path = {
            let prepared = prepare_extraction_dir(true).unwrap();
            let path = prepared.path().to_path_buf();
            assert!(prepared.preserved());
            assert!(path.exists());
            path
        };

        assert!(path.exists());
        std::fs::remove_dir_all(path).unwrap();
    }
}
