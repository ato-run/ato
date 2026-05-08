//! Archive unpack: tar.gz / tar.xz / tar.zst / zip / jar+txz.
//!
//! Unpacks into a caller-provided directory. The resolver always
//! points unpack at a temp dir, validates `provides`, then renames
//! atomically into the store. This module never touches the final
//! store path.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, bail, Context};

use super::download::sha256_of_file;
use super::error::ToolArtifactError;
use super::manifest::{ArchiveFormat, ToolArtifactManifest};

/// Unpack `source` (the verified download) into `dest_dir` according
/// to the format declared in `manifest`. `strip_prefix` is honored for
/// tar formats; for zip it is best-effort string-prefix removal.
///
/// All errors are mapped to [`ToolArtifactError::ArtifactUnpackFailed`]
/// so the caller's error surface stays small. The resolver wraps the
/// inner `provides` validation separately.
pub fn unpack_archive(
    manifest: &ToolArtifactManifest,
    source: &Path,
    dest_dir: &Path,
) -> Result<(), ToolArtifactError> {
    let strip = manifest.strip_prefix.as_deref().unwrap_or("");
    do_unpack(manifest, source, dest_dir, strip).map_err(|err| {
        ToolArtifactError::ArtifactUnpackFailed {
            name: manifest.name.clone(),
            reason: format!("{err:#}"),
        }
    })
}

fn do_unpack(
    manifest: &ToolArtifactManifest,
    source: &Path,
    dest_dir: &Path,
    strip_prefix: &str,
) -> Result<(), anyhow::Error> {
    fs::create_dir_all(dest_dir)
        .with_context(|| format!("create unpack dest {}", dest_dir.display()))?;
    match manifest.archive_format {
        ArchiveFormat::TarGz => {
            let file = File::open(source)
                .with_context(|| format!("open {} for tar.gz unpack", source.display()))?;
            let dec = flate2::read::GzDecoder::new(file);
            unpack_tar(dec, dest_dir, strip_prefix).context("tar.gz unpack")?;
        }
        ArchiveFormat::TarXz => {
            let file = File::open(source)
                .with_context(|| format!("open {} for tar.xz unpack", source.display()))?;
            let dec = xz2::read::XzDecoder::new(file);
            unpack_tar(dec, dest_dir, strip_prefix).context("tar.xz unpack")?;
        }
        ArchiveFormat::TarZst => {
            let file = File::open(source)
                .with_context(|| format!("open {} for tar.zst unpack", source.display()))?;
            let dec = zstd::stream::Decoder::new(file).context("zstd decoder")?;
            unpack_tar(dec, dest_dir, strip_prefix).context("tar.zst unpack")?;
        }
        ArchiveFormat::Zip => {
            let file = File::open(source)
                .with_context(|| format!("open {} for zip unpack", source.display()))?;
            unpack_zip(file, dest_dir, strip_prefix).context("zip unpack")?;
        }
        ArchiveFormat::JarTxz => {
            let inner_member = manifest
                .inner_member
                .as_deref()
                .ok_or_else(|| anyhow!("jar+txz manifest missing inner_member"))?;
            let staged = stage_jar_inner_member(source, inner_member, dest_dir.parent())?;
            // Defense in depth: even though the manifest's outer sha256
            // is already verified, also verify the inner archive hash
            // when the manifest provides one. A tampered upstream that
            // preserves the JAR sha256 (e.g. zip metadata trickery) is
            // implausible but cheap to rule out.
            if let Some(expected_inner) = manifest.inner_sha256.as_deref() {
                let got = sha256_of_file(&staged).context("hash inner archive")?;
                if got != expected_inner {
                    bail!(
                        "inner archive sha256 mismatch: expected {}, got {}",
                        expected_inner,
                        got
                    );
                }
            }
            let file = File::open(&staged)
                .with_context(|| format!("open inner {} for unpack", staged.display()))?;
            let dec = xz2::read::XzDecoder::new(file);
            unpack_tar(dec, dest_dir, strip_prefix).context("inner tar.xz unpack")?;
            let _ = fs::remove_file(&staged);
        }
    }
    Ok(())
}

fn unpack_tar<R: Read>(
    reader: R,
    dest_dir: &Path,
    strip_prefix: &str,
) -> Result<(), anyhow::Error> {
    let mut archive = tar::Archive::new(reader);
    archive.set_preserve_permissions(true);
    archive.set_overwrite(true);
    let entries = archive.entries().context("read tar entries")?;
    for entry in entries {
        let mut entry = entry.context("invalid tar entry")?;
        let raw_path = entry.path().context("read tar entry path")?.into_owned();
        let stripped = match strip_prefix_from(&raw_path, strip_prefix) {
            Some(p) => p,
            None => continue, // entry sits outside strip_prefix, skip
        };
        if stripped.as_os_str().is_empty() {
            continue;
        }
        ensure_safe_relative(&stripped)?;
        let out_path = dest_dir.join(&stripped);
        let header_kind = entry.header().entry_type();
        if header_kind.is_dir() {
            fs::create_dir_all(&out_path)
                .with_context(|| format!("mkdir {}", out_path.display()))?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("mkdir parent {}", parent.display()))?;
        }
        // tar::Entry::unpack handles symlinks, regular files, and
        // permission bits in one call — preserves the executable bit
        // we need on bin/* without a manual chmod pass.
        entry
            .unpack(&out_path)
            .with_context(|| format!("unpack tar entry to {}", out_path.display()))?;
    }
    Ok(())
}

fn unpack_zip(
    file: File,
    dest_dir: &Path,
    strip_prefix: &str,
) -> Result<(), anyhow::Error> {
    let mut archive = zip::ZipArchive::new(file).context("read zip")?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).context("zip entry")?;
        let raw_name = match entry.enclosed_name() {
            Some(n) => n.to_path_buf(),
            None => continue,
        };
        let stripped = match strip_prefix_from(&raw_name, strip_prefix) {
            Some(p) => p,
            None => continue,
        };
        if stripped.as_os_str().is_empty() {
            continue;
        }
        ensure_safe_relative(&stripped)?;
        let out_path = dest_dir.join(&stripped);
        if entry.is_dir() {
            fs::create_dir_all(&out_path)
                .with_context(|| format!("mkdir {}", out_path.display()))?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("mkdir parent {}", parent.display()))?;
        }
        let mut out = File::create(&out_path)
            .with_context(|| format!("create {}", out_path.display()))?;
        std::io::copy(&mut entry, &mut out)
            .with_context(|| format!("write {}", out_path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Some(mode) = entry.unix_mode() {
                let perms = std::fs::Permissions::from_mode(mode);
                let _ = std::fs::set_permissions(&out_path, perms);
            }
        }
    }
    Ok(())
}

/// Extract the named member from a Maven-style JAR (zip) into a temp
/// file alongside the eventual unpack destination. Returns the path of
/// the staged inner file.
fn stage_jar_inner_member(
    source: &Path,
    inner_name: &str,
    sibling_dir: Option<&Path>,
) -> Result<PathBuf, anyhow::Error> {
    let file = File::open(source)
        .with_context(|| format!("open {} for jar+txz unpack", source.display()))?;
    let mut archive = zip::ZipArchive::new(file).context("read jar as zip")?;
    let mut entry = archive
        .by_name(inner_name)
        .with_context(|| format!("inner member '{}' missing from jar", inner_name))?;
    let parent = sibling_dir
        .map(|p| p.to_path_buf())
        .unwrap_or_else(std::env::temp_dir);
    fs::create_dir_all(&parent)
        .with_context(|| format!("mkdir staging parent {}", parent.display()))?;
    let staged = parent.join(format!(
        ".inner-{}-{}",
        std::process::id(),
        sanitize_filename(inner_name)
    ));
    let mut out = File::create(&staged)
        .with_context(|| format!("create staged inner at {}", staged.display()))?;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = entry.read(&mut buf).context("read inner archive")?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).context("write staged inner")?;
    }
    out.flush().ok();
    Ok(staged)
}

fn strip_prefix_from(path: &Path, prefix: &str) -> Option<PathBuf> {
    if prefix.is_empty() {
        return Some(path.to_path_buf());
    }
    let prefix_path = Path::new(prefix);
    path.strip_prefix(prefix_path).ok().map(|p| p.to_path_buf())
}

fn ensure_safe_relative(path: &Path) -> Result<(), anyhow::Error> {
    if path.is_absolute() {
        bail!("archive entry has absolute path: {}", path.display());
    }
    for comp in path.components() {
        match comp {
            Component::Normal(_) | Component::CurDir => continue,
            Component::ParentDir => {
                bail!("archive entry escapes destination via '..': {}", path.display())
            }
            Component::RootDir | Component::Prefix(_) => {
                bail!("archive entry has root component: {}", path.display())
            }
        }
    }
    Ok(())
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            other => other,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::tool_artifact::manifest::{
        ArchiveFormat, ArtifactLayout, ToolArtifactManifest,
    };
    use std::io::Cursor;
    use tar::Header;

    fn fixture_manifest(format: ArchiveFormat) -> ToolArtifactManifest {
        ToolArtifactManifest {
            schema_version: "1".into(),
            name: "demo".into(),
            version: "1.0.0".into(),
            platform: "linux-x86_64".into(),
            url: "https://example/x".into(),
            sha256: "0".repeat(64),
            archive_format: format,
            inner_member: None,
            inner_sha256: None,
            strip_prefix: None,
            layout: ArtifactLayout {
                bin_dir: "bin".into(),
                lib_dir: "lib".into(),
                share_dir: "share".into(),
            },
            provides: vec!["demo".into()],
        }
    }

    fn build_synthetic_tar() -> Vec<u8> {
        // bin/demo (executable), lib/.keep, share/.keep
        let buf = Vec::new();
        let mut builder = tar::Builder::new(buf);
        // bin/
        let mut h = Header::new_gnu();
        h.set_size(0);
        h.set_mode(0o755);
        h.set_entry_type(tar::EntryType::Directory);
        h.set_cksum();
        builder
            .append_data(&mut h, "bin/", std::io::empty())
            .unwrap();
        // bin/demo
        let demo = b"#!/bin/sh\necho demo\n";
        let mut h = Header::new_gnu();
        h.set_size(demo.len() as u64);
        h.set_mode(0o755);
        h.set_cksum();
        builder
            .append_data(&mut h, "bin/demo", Cursor::new(demo))
            .unwrap();
        // lib/keep
        let mut h = Header::new_gnu();
        h.set_size(0);
        h.set_mode(0o644);
        h.set_cksum();
        builder
            .append_data(&mut h, "lib/.keep", std::io::empty())
            .unwrap();
        // share/keep
        let mut h = Header::new_gnu();
        h.set_size(0);
        h.set_mode(0o644);
        h.set_cksum();
        builder
            .append_data(&mut h, "share/.keep", std::io::empty())
            .unwrap();
        builder.into_inner().unwrap()
    }

    fn build_tar_gz_fixture() -> Vec<u8> {
        let tar_bytes = build_synthetic_tar();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gz, &tar_bytes).unwrap();
        gz.finish().unwrap()
    }

    #[test]
    fn unpack_tar_gz_preserves_executable_bit() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("demo.tar.gz");
        fs::write(&archive, build_tar_gz_fixture()).unwrap();
        let dest = tmp.path().join("out");
        unpack_archive(&fixture_manifest(ArchiveFormat::TarGz), &archive, &dest)
            .expect("unpack");
        let demo = dest.join("bin/demo");
        assert!(demo.exists(), "bin/demo missing");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&demo).unwrap().permissions().mode();
            assert!(mode & 0o111 != 0, "demo not executable: mode={mode:o}");
        }
        let mut content = String::new();
        File::open(&demo)
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();
        assert!(content.contains("echo demo"));
    }

    #[test]
    fn ensure_safe_relative_rejects_parent_dir() {
        // The `tar` crate already refuses `..` at append time and at
        // unpack time, so a hand-built malicious archive cannot be
        // produced through the safe API. `ensure_safe_relative` is
        // the defense-in-depth layer; test it directly for the cases
        // it is responsible for.
        let bad: &[&str] = &["../escape", "a/b/../../c", "/etc/passwd"];
        for case in bad {
            let err =
                ensure_safe_relative(Path::new(case)).expect_err(&format!("must reject {case}"));
            let msg = format!("{err:#}");
            assert!(
                msg.contains("..") || msg.to_lowercase().contains("absolute"),
                "case={case}, got: {msg}"
            );
        }
    }

    #[test]
    fn ensure_safe_relative_accepts_normal_paths() {
        for ok in &["bin/demo", "lib/postgresql/bloom.dylib", "share/.keep"] {
            ensure_safe_relative(Path::new(ok)).expect("must accept");
        }
    }

    #[test]
    fn unpack_strip_prefix_drops_top_dir() {
        // Build a tar where everything is under "pgsql-16/"
        let mut builder = tar::Builder::new(Vec::new());
        let payload = b"data";
        let mut h = Header::new_gnu();
        h.set_size(payload.len() as u64);
        h.set_mode(0o755);
        h.set_cksum();
        builder
            .append_data(&mut h, "pgsql-16/bin/demo", Cursor::new(payload))
            .unwrap();
        let raw = builder.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gz, &raw).unwrap();
        let bytes = gz.finish().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("a.tar.gz");
        fs::write(&archive, bytes).unwrap();
        let dest = tmp.path().join("out");
        let mut m = fixture_manifest(ArchiveFormat::TarGz);
        m.strip_prefix = Some("pgsql-16".into());
        unpack_archive(&m, &archive, &dest).expect("unpack");
        assert!(dest.join("bin/demo").exists(), "strip_prefix did not drop top dir");
    }
}
