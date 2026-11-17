//! Stable macOS application wrappers for verified source-built GUI executables.
//! The wrapper never signs or changes the executable and never claims the
//! upstream publisher's bundle identifier.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};

use super::artifact::adopt_tree;

fn digest(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    std::io::copy(&mut file, &mut hash)?;
    Ok(format!("sha256:{:x}", hash.finalize()))
}

pub fn materialize(
    executable: &Path,
    output_digest: &str,
    home: &Path,
) -> Result<(PathBuf, String)> {
    let name = executable
        .file_name()
        .and_then(|name| name.to_str())
        .context("GUI executable name")?;
    ensure!(
        !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
        "invalid GUI executable name"
    );
    let hex = output_digest
        .strip_prefix("sha256:")
        .context("GUI output digest")?;
    ensure!(
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "invalid GUI output digest"
    );
    let key = format!("{:x}", Sha256::digest(format!("{output_digest}\0{name}")));
    let relative = format!("{name}.app/Contents/MacOS/{name}");
    let plist_relative = format!("{name}.app/Contents/Info.plist");
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>{name}</string>
<key>CFBundleIdentifier</key><string>run.ato.local.{key}</string>
<key>CFBundleName</key><string>{name}</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleVersion</key><string>1</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
"#
    );
    let parent = home.join("cache/native-window-app-v1");
    fs::create_dir_all(&parent)?;
    let installed = parent.join(&key);
    let validate = |tree: &Path| -> Result<()> {
        let observed = adopt_tree(tree)?; // Reject links, special files and oversized trees.
        ensure!(
            observed.file_count == 2,
            "native_window_app_corrupt: unexpected files"
        );
        ensure!(
            fs::read(tree.join(&plist_relative))? == plist.as_bytes(),
            "native_window_app_corrupt: plist changed"
        );
        ensure!(
            digest(&tree.join(&relative))? == output_digest,
            "native_window_app_corrupt: executable changed"
        );
        ensure!(
            fs::metadata(tree.join(&relative))?.permissions().mode() & 0o777 == 0o755,
            "native_window_app_corrupt: executable mode changed"
        );
        Ok(())
    };
    if installed.exists() {
        validate(&installed)?;
        return Ok((installed, relative));
    }
    let pending = tempfile::Builder::new()
        .prefix("app-")
        .tempdir_in(&parent)?;
    let target = pending.path().join(&relative);
    fs::create_dir_all(target.parent().context("GUI executable parent")?)?;
    fs::copy(executable, &target)?;
    fs::set_permissions(&target, fs::Permissions::from_mode(0o755))?;
    fs::write(pending.path().join(&plist_relative), plist.as_bytes())?;
    validate(pending.path())?;
    match fs::rename(pending.path(), &installed) {
        Ok(()) => (),
        Err(_) if installed.exists() => validate(&installed)?,
        Err(error) => return Err(error).context("publish local GUI application"),
    }
    Ok((installed, relative))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapper_reuses_unchanged_bytes_and_rejects_modified_executable_or_plist() {
        let scratch = tempfile::tempdir().unwrap();
        let binary = scratch.path().join("example-cad");
        fs::write(&binary, b"verified binary").unwrap();
        let expected = digest(&binary).unwrap();
        let home = scratch.path().join("home");
        let (root, entry) = materialize(&binary, &expected, &home).unwrap();
        assert_eq!(
            materialize(&binary, &expected, &home).unwrap(),
            (root.clone(), entry.clone())
        );
        assert_eq!(fs::read(root.join(&entry)).unwrap(), b"verified binary");
        let plist = root.join("example-cad.app/Contents/Info.plist");
        let original_plist = fs::read(&plist).unwrap();
        fs::write(&plist, b"other bundle identity").unwrap();
        assert!(materialize(&binary, &expected, &home).is_err());
        fs::write(&plist, original_plist).unwrap();
        fs::write(root.join(&entry), b"modified binary").unwrap();
        assert!(materialize(&binary, &expected, &home).is_err());
    }

    #[test]
    fn wrapper_refuses_wrong_digest_and_extra_files() {
        let scratch = tempfile::tempdir().unwrap();
        let binary = scratch.path().join("cad");
        fs::write(&binary, b"verified binary").unwrap();
        let home = scratch.path().join("home");
        assert!(materialize(&binary, &format!("sha256:{}", "0".repeat(64)), &home).is_err());
        let expected = digest(&binary).unwrap();
        let (root, _) = materialize(&binary, &expected, &home).unwrap();
        fs::write(root.join("extra.dylib"), b"unexpected code").unwrap();
        assert!(materialize(&binary, &expected, &home).is_err());
    }
}
