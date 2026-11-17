//! Stable macOS application wrappers for verified source-built GUI executables.
//! The wrapper binds its plist with an ad-hoc signature, never an upstream
//! publisher identity. It still requires the explicit local unsigned override.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};

use super::artifact::adopt_tree;

fn digest(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    std::io::copy(&mut file, &mut hash)?;
    Ok(format!("sha256:{:x}", hash.finalize()))
}

fn sign_local_app(app: &Path) -> Result<()> {
    let mut child = Command::new(ato_host_facts::CODESIGN_PATH)
        .args(["--force", "--sign", "-"])
        .arg(app)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("bind the local app plist with an ad-hoc signature")?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_wait()? {
            ensure!(
                status.success(),
                "local app ad-hoc signing failed: {status}"
            );
            return Ok(());
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("local app ad-hoc signing timed out");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
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
    let parent = home.join("cache/native-window-app-v2");
    fs::create_dir_all(&parent)?;
    let installed = parent.join(&key);
    let validate = |container: &Path| -> Result<()> {
        let tree = container.join("tree");
        let observed = adopt_tree(&tree)?; // Reject links, special files and oversized trees.
        ensure!(
            observed.file_count == 3,
            "native_window_app_corrupt: unexpected files"
        );
        ensure!(
            fs::read(tree.join(&plist_relative))? == plist.as_bytes(),
            "native_window_app_corrupt: plist changed"
        );
        ensure!(
            fs::read_to_string(container.join("verified-tree"))?
                == format!("{output_digest}\n{}\n", observed.tree_digest),
            "native_window_app_corrupt: signed tree changed"
        );
        ensure!(
            fs::metadata(tree.join(&relative))?.permissions().mode() & 0o777 == 0o755,
            "native_window_app_corrupt: executable mode changed"
        );
        Ok(())
    };
    if installed.exists() {
        validate(&installed)?;
        return Ok((installed.join("tree"), relative));
    }
    let pending = tempfile::Builder::new()
        .prefix("app-")
        .tempdir_in(&parent)?;
    let tree = pending.path().join("tree");
    let target = tree.join(&relative);
    fs::create_dir_all(target.parent().context("GUI executable parent")?)?;
    fs::copy(executable, &target)?;
    fs::set_permissions(&target, fs::Permissions::from_mode(0o755))?;
    ensure!(
        digest(&target)? == output_digest,
        "GUI executable does not match the verified build"
    );
    fs::write(tree.join(&plist_relative), plist.as_bytes())?;
    sign_local_app(&tree.join(format!("{name}.app")))?;
    let signed = adopt_tree(&tree)?;
    fs::write(
        pending.path().join("verified-tree"),
        format!("{output_digest}\n{}\n", signed.tree_digest),
    )?;
    validate(pending.path())?;
    match fs::rename(pending.path(), &installed) {
        Ok(()) => (),
        Err(_) if installed.exists() => validate(&installed)?,
        Err(error) => return Err(error).context("publish local GUI application"),
    }
    Ok((installed.join("tree"), relative))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapper_binds_local_identity_and_rejects_modified_executable_or_plist() {
        let scratch = tempfile::tempdir().unwrap();
        let binary = scratch.path().join("example-cad");
        fs::copy("/bin/echo", &binary).unwrap();
        let expected = digest(&binary).unwrap();
        let home = scratch.path().join("home");
        let (root, entry) = materialize(&binary, &expected, &home).unwrap();
        assert_eq!(
            materialize(&binary, &expected, &home).unwrap(),
            (root.clone(), entry.clone())
        );
        assert_eq!(
            digest(&binary).unwrap(),
            expected,
            "original build remains unchanged"
        );
        let report = super::super::macos::probe_signature(&root.join(&entry));
        assert_eq!(report.codesign_verify, "valid");
        assert_eq!(report.signature_kind, "adhoc");
        assert!(!report.satisfies_require_valid());
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
        fs::copy("/bin/echo", &binary).unwrap();
        let home = scratch.path().join("home");
        assert!(materialize(&binary, &format!("sha256:{}", "0".repeat(64)), &home).is_err());
        let expected = digest(&binary).unwrap();
        let (root, _) = materialize(&binary, &expected, &home).unwrap();
        fs::write(root.join("extra.dylib"), b"unexpected code").unwrap();
        assert!(materialize(&binary, &expected, &home).is_err());
    }
}
