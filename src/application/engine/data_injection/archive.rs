use std::fs;
use std::path::Path;

use anyhow::Result;
use walkdir::WalkDir;

pub(super) fn extract_archive_if_needed(archive_path: &Path, dest: &Path) -> Result<()> {
    let name = archive_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if name.ends_with(".zip") {
        let file = fs::File::open(archive_path)?;
        let mut zip = zip::ZipArchive::new(file)?;
        for index in 0..zip.len() {
            let mut entry = zip.by_index(index)?;
            let Some(safe_name) = entry.enclosed_name().map(|value| value.to_path_buf()) else {
                continue;
            };
            let out_path = dest.join(safe_name);
            if entry.name().ends_with('/') {
                fs::create_dir_all(&out_path)?;
                continue;
            }
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out = fs::File::create(&out_path)?;
            std::io::copy(&mut entry, &mut out)?;
        }
        return Ok(());
    }
    if name.ends_with(".tar") || name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        let file = fs::File::open(archive_path)?;
        if name.ends_with(".tar") {
            let mut archive = tar::Archive::new(file);
            archive.unpack(dest)?;
        } else {
            let decoder = flate2::read::GzDecoder::new(file);
            let mut archive = tar::Archive::new(decoder);
            archive.unpack(dest)?;
        }
        return Ok(());
    }
    anyhow::bail!(
        "unsupported directory injection archive: {}",
        archive_path.display()
    )
}

pub(super) fn set_read_only_recursive(path: &Path) -> Result<()> {
    if path.is_dir() {
        for entry in WalkDir::new(path).into_iter().flatten() {
            let metadata = entry.metadata()?;
            let mut permissions = metadata.permissions();
            permissions.set_readonly(true);
            fs::set_permissions(entry.path(), permissions)?;
        }
    } else if path.exists() {
        let metadata = fs::metadata(path)?;
        let mut permissions = metadata.permissions();
        permissions.set_readonly(true);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}
