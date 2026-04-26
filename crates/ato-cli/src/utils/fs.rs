use anyhow::{Context, Result};
use std::fs;
use std::path::Path;
use walkdir::WalkDir;

pub(crate) fn copy_path_recursive(source: &Path, destination: &Path) -> Result<()> {
    if source.is_dir() {
        fs::create_dir_all(destination).with_context(|| {
            format!(
                "Failed to create destination directory {}",
                destination.display()
            )
        })?;

        for entry in WalkDir::new(source).min_depth(1).into_iter().flatten() {
            let relative = entry.path().strip_prefix(source).unwrap_or(entry.path());
            let target = destination.join(relative);
            if entry.file_type().is_dir() {
                fs::create_dir_all(&target).with_context(|| {
                    format!(
                        "Failed to create destination directory {}",
                        target.display()
                    )
                })?;
            } else if entry.file_type().is_file() {
                copy_file(entry.path(), &target)?;
            }
        }
        return Ok(());
    }

    copy_file(source, destination)
}

fn copy_file(source: &Path, destination: &Path) -> Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create destination parent {}", parent.display()))?;
    }
    fs::copy(source, destination).with_context(|| {
        format!(
            "Failed to copy {} to {}",
            source.display(),
            destination.display()
        )
    })?;
    Ok(())
}
