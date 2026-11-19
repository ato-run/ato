//! Bounded, read-only macOS disk-image materialization. No mounted code runs.
use std::fs;
use std::path::{Component, Path};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

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
        ensure!(!destination.exists(), "artifact destination already exists");
        let parent = destination
            .parent()
            .context("artifact destination has no parent")?;
        fs::create_dir_all(parent)?;
        // Outside extraction scratch: its owner's recursive cleanup must never
        // traverse a mount that hdiutil failed to detach.
        let mount = tempfile::Builder::new()
            .prefix("ato-native-dmg-")
            .tempdir()?
            .keep();
        let attach = bounded_command(
            Command::new("/usr/bin/hdiutil")
                .args([
                    "attach",
                    "-readonly",
                    "-nobrowse",
                    "-noautoopen",
                    "-plist",
                    "-mountpoint",
                ])
                .arg(&mount)
                .arg(image),
            Duration::from_secs(180),
        );
        let attached = attach.as_ref().is_ok_and(|(status, _)| status.success());
        let result = match attach {
            Ok((status, _)) if status.success() => copy_apps(&mount, destination),
            Ok((_, stderr)) => Err(anyhow::anyhow!("native_dmg_attach_failed: {stderr}")),
            Err(error) => Err(error).context("native_dmg_attach_failed"),
        };
        // A failed attach can still have mounted a partition. Detach first;
        // never recursively delete a potentially mounted directory.
        let detach = bounded_command(
            Command::new("/usr/bin/hdiutil").arg("detach").arg(&mount),
            Duration::from_secs(30),
        );
        let detached = detach.as_ref().is_ok_and(|(status, _)| status.success());
        // remove_dir never follows or traverses a mounted filesystem. An attach
        // failure can still have mounted a partition; retain/report that too.
        let removed = fs::remove_dir(&mount).is_ok();
        if !detached && (attached || !removed) {
            anyhow::bail!(
                "native_dmg_detach_failed: retained mount at {}",
                mount.display()
            );
        }
        if result.is_err() {
            let _ = fs::remove_dir_all(destination);
        }
        result
    }
}

/// Avoid a stuck image helper holding installation forever or filling a pipe.
fn bounded_command(command: &mut Command, timeout: Duration) -> Result<(ExitStatus, String)> {
    use std::io::{Read, Seek};
    let mut error_output = tempfile::tempfile()?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(error_output.try_clone()?)
        .spawn()?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            result => {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!("disk image helper timed out or could not be observed: {result:?}");
            }
        }
    };
    error_output.rewind()?;
    let mut bytes = Vec::new();
    error_output.take(8192).read_to_end(&mut bytes)?;
    Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
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

/// Permit relative framework links within the real top-level app that owns them.
/// Parent steps must come first: a parent after a symlink component does not
/// necessarily move up one directory in the original lexical path.
pub(super) fn validate_link_name(root: &Path, link: &Path, target: &Path) -> Result<()> {
    let relative = link.strip_prefix(root)?;
    let app = relative.components().next().context("missing app name")?;
    ensure!(
        Path::new(app.as_os_str())
            .extension()
            .is_some_and(|e| e == "app"),
        "link outside an app"
    );
    let mut depth = relative
        .components()
        .count()
        .checked_sub(2)
        .context("app root cannot be a link")?;
    let mut has_name = false;
    for part in target.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir if !has_name && depth > 0 => depth -= 1,
            Component::Normal(_) => has_name = true,
            _ => anyhow::bail!("disk image link target is outside its app or is ambiguous"),
        }
    }
    ensure!(has_name, "disk image link must name an app entry");
    Ok(())
}

pub(super) fn validate_link_destination(root: &Path, link: &Path) -> Result<()> {
    let app = link
        .strip_prefix(root)?
        .components()
        .next()
        .context("missing app name")?;
    let app_root = root.join(app.as_os_str()).canonicalize()?;
    let resolved = link
        .canonicalize()
        .context("dangling or cyclic disk image link")?;
    ensure!(
        resolved.starts_with(&app_root),
        "disk image link resolves outside its app"
    );
    let parent = link
        .parent()
        .context("missing link parent")?
        .canonicalize()?;
    ensure!(
        !parent.starts_with(&resolved),
        "disk image link points to its parent tree"
    );
    Ok(())
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
    #[cfg(unix)]
    let mut links = Vec::new();
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
        #[cfg(unix)]
        if metadata.is_symlink() {
            let target = fs::read_link(&path)?;
            validate_link_name(mount, &path, &target)?;
            // The walk enumerates only real directories on the read-only
            // image. No copied link is used as a destination directory.
            links.push((target, output));
            continue;
        }
        if metadata.is_dir() {
            fs::create_dir(&output)?;
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
            let mut copied = fs::File::create_new(&output)?;
            ensure!(
                std::io::copy(&mut fs::File::open(&path)?, &mut copied)? == metadata.len(),
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
    #[cfg(unix)]
    for (target, output) in links {
        std::os::unix::fs::symlink(target, output)?;
    }
    artifact::inspect_archive_tree(
        destination,
        ato_formation::native_desktop::NativeArtifactFormat::Dmg,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_stuck_helper_is_terminated_at_its_deadline() {
        let started = Instant::now();
        assert!(
            bounded_command(
                Command::new("/bin/sleep").arg("5"),
                Duration::from_millis(25)
            )
            .is_err()
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }
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
    fn framework_links_survive_materialization_and_cache_validation() {
        let root = tempfile::tempdir().unwrap();
        let mount = root.path().join("mount");
        let framework = mount.join("Example.app/Contents/Frameworks/Example.framework");
        fs::create_dir_all(framework.join("Versions/A")).unwrap();
        fs::write(framework.join("Versions/A/Example"), b"signed payload").unwrap();
        std::os::unix::fs::symlink("A", framework.join("Versions/Current")).unwrap();
        std::os::unix::fs::symlink("Versions/Current/Example", framework.join("Example")).unwrap();
        let tree = copy_apps(&mount, &root.path().join("out")).unwrap();
        let entry = tree
            .root
            .join("Example.app/Contents/Frameworks/Example.framework/Example");
        assert_eq!(
            fs::read_link(&entry).unwrap(),
            Path::new("Versions/Current/Example")
        );
        assert_eq!(fs::read(&entry).unwrap(), b"signed payload");
        let format = ato_formation::native_desktop::NativeArtifactFormat::Dmg;
        assert_eq!(
            artifact::inspect_archive_tree(&tree.root, format)
                .unwrap()
                .tree_digest,
            tree.tree_digest
        );
        fs::remove_file(&entry).unwrap();
        std::os::unix::fs::symlink("Versions/A/Example", &entry).unwrap();
        assert_ne!(
            artifact::inspect_archive_tree(&tree.root, format)
                .unwrap()
                .tree_digest,
            tree.tree_digest
        );
        assert!(artifact::adopt_tree(&tree.root).is_err());
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

    #[cfg(unix)]
    #[test]
    fn rejects_dangling_cyclic_and_cross_app_links() {
        for target in ["missing", "data", "../Other.app/real"] {
            let root = tempfile::tempdir().unwrap();
            let mount = root.path().join("mount");
            fs::create_dir_all(mount.join("Bad.app")).unwrap();
            fs::create_dir_all(mount.join("Other.app")).unwrap();
            fs::write(mount.join("Other.app/real"), b"other app").unwrap();
            std::os::unix::fs::symlink(target, mount.join("Bad.app/data")).unwrap();
            assert!(
                copy_apps(&mount, &root.path().join("out")).is_err(),
                "{target}"
            );
        }
    }
}
