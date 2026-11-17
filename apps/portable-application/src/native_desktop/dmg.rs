//! Bounded, read-only macOS disk-image materialization. No mounted code runs.
use std::fs;
use std::path::Path;

use super::artifact::{self, ExtractedArtifact, MAX_EXTRACTED_BYTES, MAX_EXTRACTED_ENTRIES};
use anyhow::{Context, Result, ensure};

pub fn extract_file(image: &Path, destination: &Path) -> Result<ExtractedArtifact> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (image, destination);
        anyhow::bail!("native_format_extractor_unavailable: dmg requires macOS");
    }
    #[cfg(target_os = "macos")]
    {
        use std::process::{Command, Stdio};
        ensure!(!destination.exists(), "artifact destination already exists");
        let parent = destination
            .parent()
            .context("artifact destination has no parent")?;
        fs::create_dir_all(parent)?;
        // Keep the directory until detach succeeds, including when copying fails.
        let mount = tempfile::Builder::new()
            .prefix("native-dmg-")
            .tempdir_in(parent)?
            .keep();
        let attach = Command::new("/usr/bin/hdiutil")
            .args([
                "attach",
                "-readonly",
                "-nobrowse",
                "-noautoopen",
                "-plist",
                "-mountpoint",
            ])
            .arg(&mount)
            .arg(image)
            .stdin(Stdio::null())
            .output();
        let attached = attach.as_ref().is_ok_and(|out| out.status.success());
        let result = match attach {
            Ok(out) if out.status.success() => copy_apps(&mount, destination),
            Ok(out) => Err(anyhow::anyhow!(
                "native_dmg_attach_failed: {}",
                String::from_utf8_lossy(&out.stderr)
            )),
            Err(error) => Err(error).context("native_dmg_attach_failed"),
        };
        // A failed attach can still have mounted a partition. Detach first;
        // never recursively delete a potentially mounted directory.
        let detach = Command::new("/usr/bin/hdiutil")
            .arg("detach")
            .arg(&mount)
            .stdin(Stdio::null())
            .output();
        let detached = detach.as_ref().is_ok_and(|out| out.status.success());
        if attached && !detached {
            anyhow::bail!(
                "native_dmg_detach_failed: retained mount at {}",
                mount.display()
            );
        }
        // remove_dir is nonrecursive and fails safely if anything remains.
        let _ = fs::remove_dir(&mount);
        if result.is_err() {
            let _ = fs::remove_dir_all(destination);
        }
        result
    }
}

pub fn extract_bytes(bytes: &[u8], destination: &Path) -> Result<ExtractedArtifact> {
    let parent = destination
        .parent()
        .context("artifact destination has no parent")?;
    fs::create_dir_all(parent)?;
    let mut image = tempfile::NamedTempFile::new_in(parent)?;
    std::io::Write::write_all(&mut image, bytes)?;
    extract_file(image.path(), destination)
}

fn copy_apps(mount: &Path, destination: &Path) -> Result<ExtractedArtifact> {
    ensure!(!destination.exists(), "artifact destination already exists");
    fs::create_dir(destination)?;
    let mut pending = Vec::new();
    for item in fs::read_dir(mount)? {
        let item = item?;
        if item.path().extension().is_some_and(|ext| ext == "app") {
            ensure!(
                item.file_type()?.is_dir(),
                "disk image app is not a real directory"
            );
            pending.push(item.path());
        }
    }
    ensure!(
        !pending.is_empty(),
        "disk image contains no top-level app bundle"
    );
    let mut count = 0_u64;
    let mut bytes = 0_u64;
    while let Some(path) = pending.pop() {
        count += 1;
        ensure!(
            count <= MAX_EXTRACTED_ENTRIES,
            "disk image has too many entries"
        );
        let metadata = fs::symlink_metadata(&path)?;
        let output = destination.join(path.strip_prefix(mount)?);
        if metadata.is_dir() {
            fs::create_dir_all(&output)?;
            for item in fs::read_dir(path)? {
                pending.push(item?.path());
            }
        } else {
            ensure!(
                metadata.is_file(),
                "disk image bundle contains a link or special file: {}",
                path.display()
            );
            bytes = bytes
                .checked_add(metadata.len())
                .context("disk image size overflow")?;
            ensure!(
                bytes <= MAX_EXTRACTED_BYTES,
                "disk image exceeds extraction size limit"
            );
            ensure!(
                fs::copy(&path, &output)? == metadata.len(),
                "disk image changed during copy"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    &output,
                    fs::Permissions::from_mode(metadata.permissions().mode() & 0o777),
                )?;
            }
        }
    }
    artifact::adopt_tree(destination)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn copies_only_apps_and_preserves_content() {
        let root = tempfile::tempdir().unwrap();
        let mount = root.path().join("mount");
        fs::create_dir_all(mount.join("Example.app/Contents")).unwrap();
        fs::write(mount.join("Example.app/Contents/test"), b"verified bytes").unwrap();
        fs::write(mount.join("README"), b"ignored").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/Applications", mount.join("Applications")).unwrap();
        let result = copy_apps(&mount, &root.path().join("out")).unwrap();
        assert_eq!(result.file_count, 1);
        assert_eq!(result.total_bytes, 14);
        assert!(!result.root.join("Applications").exists());
    }
    #[cfg(unix)]
    #[test]
    fn refuses_links_inside_apps_without_following_them() {
        let root = tempfile::tempdir().unwrap();
        let mount = root.path().join("mount");
        fs::create_dir_all(mount.join("Bad.app")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", mount.join("Bad.app/data")).unwrap();
        assert!(copy_apps(&mount, &root.path().join("out")).is_err());
    }
}
