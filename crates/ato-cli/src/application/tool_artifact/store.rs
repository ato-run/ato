//! On-disk store layout and atomic install.
//!
//! Layout: `<ato_home>/store/tools/<name>-<platform>-<sha256-prefix>/`.
//! Cache hit is detected by presence of a `.ato-tool-artifact.json`
//! sidecar inside the resolved root. The sidecar carries the full
//! sha256, source url, and the manifest version — enough for the
//! receipt builder to reproduce the resolve later.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use super::error::ToolArtifactError;
use super::manifest::ToolArtifactManifest;

pub(crate) const META_FILENAME: &str = ".ato-tool-artifact.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StoredArtifactMeta {
    pub schema_version: String,
    pub name: String,
    pub version: String,
    pub platform: String,
    pub url: String,
    pub sha256: String,
    pub archive_format: String,
    pub provides: Vec<String>,
    pub bin_dir: String,
    pub lib_dir: String,
    pub share_dir: String,
}

pub(crate) fn store_root(ato_home: &Path) -> PathBuf {
    ato_home.join("store").join("tools")
}

pub(crate) fn cache_key(manifest: &ToolArtifactManifest) -> String {
    let prefix: String = manifest.sha256.chars().take(12).collect();
    format!("{}-{}-{}", manifest.name, manifest.platform, prefix)
}

pub(crate) fn cache_dir(ato_home: &Path, manifest: &ToolArtifactManifest) -> PathBuf {
    store_root(ato_home).join(cache_key(manifest))
}

/// Detect a usable cache entry. Returns `Some(meta)` only when the
/// resolved dir exists, the sidecar parses, **and** the recorded
/// sha256 matches the manifest. A divergent sha256 means an out-of-
/// band rewrite happened — fall through to a fresh install rather
/// than trust the dir.
pub(crate) fn read_cache_meta(
    ato_home: &Path,
    manifest: &ToolArtifactManifest,
) -> Option<StoredArtifactMeta> {
    let dir = cache_dir(ato_home, manifest);
    let meta_path = dir.join(META_FILENAME);
    let bytes = fs::read(&meta_path).ok()?;
    let meta: StoredArtifactMeta = serde_json::from_slice(&bytes).ok()?;
    if meta.sha256 != manifest.sha256 {
        return None;
    }
    Some(meta)
}

/// Atomically install `staged_dir` (already-unpacked, already-validated)
/// at the cache path for `manifest`. The staging dir must live on the
/// same filesystem as the store root for `rename` to be atomic.
///
/// On collision (another process won the race) the freshly staged copy
/// is removed and the existing store entry wins.
pub(crate) fn install_atomic(
    ato_home: &Path,
    manifest: &ToolArtifactManifest,
    staged_dir: &Path,
) -> Result<PathBuf, ToolArtifactError> {
    let final_dir = cache_dir(ato_home, manifest);
    if let Some(parent) = final_dir.parent() {
        fs::create_dir_all(parent).map_err(|e| ToolArtifactError::StoreError {
            name: manifest.name.clone(),
            reason: format!("create store parent {}: {}", parent.display(), e),
        })?;
    }
    write_meta_sidecar(staged_dir, manifest).map_err(|e| ToolArtifactError::StoreError {
        name: manifest.name.clone(),
        reason: format!("write metadata sidecar: {e:#}"),
    })?;

    // Rename target into place. If the destination already exists, a
    // concurrent installer beat us — drop the staged copy and use
    // theirs. fs::rename behavior on existing-non-empty targets varies
    // across platforms (POSIX replaces a file but errors on a
    // non-empty dir; on macOS the platform-specific renamex_np could
    // be exclusive but rename returns ENOTEMPTY for existing dirs).
    // We handle that explicitly.
    if final_dir.exists() {
        let _ = fs::remove_dir_all(staged_dir);
        return Ok(final_dir);
    }
    match fs::rename(staged_dir, &final_dir) {
        Ok(_) => Ok(final_dir),
        Err(err) => {
            // Last-chance race: someone created the dir between our
            // exists() check and rename(). Treat it as a cache hit.
            if final_dir.exists() {
                let _ = fs::remove_dir_all(staged_dir);
                Ok(final_dir)
            } else {
                Err(ToolArtifactError::StoreError {
                    name: manifest.name.clone(),
                    reason: format!(
                        "atomic rename {} -> {} failed: {}",
                        staged_dir.display(),
                        final_dir.display(),
                        err
                    ),
                })
            }
        }
    }
}

fn write_meta_sidecar(
    staged_dir: &Path,
    manifest: &ToolArtifactManifest,
) -> Result<(), anyhow::Error> {
    let meta = StoredArtifactMeta {
        schema_version: "1".into(),
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        platform: manifest.platform.clone(),
        url: manifest.url.clone(),
        sha256: manifest.sha256.clone(),
        archive_format: archive_format_name(&manifest.archive_format),
        provides: manifest.provides.clone(),
        bin_dir: manifest.layout.bin_dir.clone(),
        lib_dir: manifest.layout.lib_dir.clone(),
        share_dir: manifest.layout.share_dir.clone(),
    };
    let bytes = serde_json::to_vec_pretty(&meta).context("serialize sidecar")?;
    fs::write(staged_dir.join(META_FILENAME), bytes).context("write sidecar")?;
    Ok(())
}

fn archive_format_name(fmt: &super::manifest::ArchiveFormat) -> String {
    use super::manifest::ArchiveFormat::*;
    match fmt {
        TarGz => "tar.gz",
        TarXz => "tar.xz",
        TarZst => "tar.zst",
        Zip => "zip",
        JarTxz => "jar+txz",
    }
    .to_string()
}

/// Validate that every entry in `manifest.provides` exists under
/// `<root>/<bin_dir>/` and is executable on Unix. Returns a map from
/// the bare command name to the absolute path so the resolver can
/// hand it directly to the `ATO_TOOL_*` env injection step.
pub(crate) fn validate_provides(
    manifest: &ToolArtifactManifest,
    root: &Path,
) -> Result<BTreeMap<String, PathBuf>, ToolArtifactError> {
    let bin = root.join(&manifest.layout.bin_dir);
    let mut out = BTreeMap::new();
    for cmd in &manifest.provides {
        let p = bin.join(cmd);
        let metadata = match fs::metadata(&p) {
            Ok(m) => m,
            Err(_) => {
                return Err(ToolArtifactError::ArtifactMissingProvidedCommand {
                    name: manifest.name.clone(),
                    command: cmd.clone(),
                    bin_dir: bin.clone(),
                });
            }
        };
        if !metadata.is_file() {
            return Err(ToolArtifactError::ArtifactMissingProvidedCommand {
                name: manifest.name.clone(),
                command: cmd.clone(),
                bin_dir: bin.clone(),
            });
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 {
                return Err(ToolArtifactError::ArtifactMissingProvidedCommand {
                    name: manifest.name.clone(),
                    command: cmd.clone(),
                    bin_dir: bin.clone(),
                });
            }
        }
        out.insert(cmd.clone(), p);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::tool_artifact::manifest::{
        ArchiveFormat, ArtifactLayout, ToolArtifactManifest,
    };

    fn fixture_manifest() -> ToolArtifactManifest {
        ToolArtifactManifest {
            schema_version: "1".into(),
            name: "demo".into(),
            version: "1.0.0".into(),
            platform: "linux-x86_64".into(),
            url: "https://example/x.tar.gz".into(),
            sha256: "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789".into(),
            archive_format: ArchiveFormat::TarGz,
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

    #[test]
    fn cache_key_format_is_stable() {
        let m = fixture_manifest();
        assert_eq!(cache_key(&m), "demo-linux-x86_64-abcdef012345");
    }

    #[test]
    fn validate_provides_reports_missing_command() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("bin")).unwrap();
        // bin/ exists but bin/demo does not
        let err = validate_provides(&fixture_manifest(), root).unwrap_err();
        match err {
            ToolArtifactError::ArtifactMissingProvidedCommand { command, .. } => {
                assert_eq!(command, "demo");
            }
            other => panic!("unexpected: {other}"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn validate_provides_reports_non_executable_command() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let demo = bin.join("demo");
        fs::write(&demo, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&demo, fs::Permissions::from_mode(0o644)).unwrap();
        let err = validate_provides(&fixture_manifest(), tmp.path()).unwrap_err();
        match err {
            ToolArtifactError::ArtifactMissingProvidedCommand { .. } => {}
            other => panic!("unexpected: {other}"),
        }
    }

    #[test]
    fn validate_provides_resolves_paths_when_executable() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let demo = bin.join("demo");
        fs::write(&demo, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&demo, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let map = validate_provides(&fixture_manifest(), tmp.path()).unwrap();
        assert_eq!(map.len(), 1);
        assert_eq!(map["demo"], demo);
    }

    #[test]
    fn install_atomic_round_trips_through_cache_meta() {
        let ato_home = tempfile::tempdir().unwrap();
        let staged = tempfile::tempdir().unwrap();
        // Pretend the staged dir is unpack output: bin/demo + lib/.keep.
        let bin = staged.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("demo"), b"#!/bin/sh\n").unwrap();
        let m = fixture_manifest();
        // We cannot rename the tempdir directly because it would be
        // dropped twice; mimic the resolver by using an explicit child
        // of the store_root parent.
        let stage_inside_store = ato_home.path().join("store").join("tools");
        fs::create_dir_all(&stage_inside_store).unwrap();
        let real_stage = stage_inside_store.join(".staging-demo");
        fs::rename(staged.path(), &real_stage).unwrap();
        let final_dir = install_atomic(ato_home.path(), &m, &real_stage).expect("install");
        assert_eq!(final_dir, cache_dir(ato_home.path(), &m));
        assert!(final_dir.join("bin/demo").exists(), "binary moved");
        assert!(final_dir.join(META_FILENAME).exists(), "sidecar present");
        let meta = read_cache_meta(ato_home.path(), &m).expect("meta readable");
        assert_eq!(meta.sha256, m.sha256);
        assert_eq!(meta.name, m.name);
        // staging dir was renamed away
        assert!(!real_stage.exists(), "staged dir should be gone");
    }

    #[test]
    fn read_cache_meta_returns_none_when_sha_diverges() {
        let ato_home = tempfile::tempdir().unwrap();
        let m = fixture_manifest();
        let dir = cache_dir(ato_home.path(), &m);
        fs::create_dir_all(&dir).unwrap();
        // Write a sidecar with a different sha256 — simulates a
        // tampered cache. read_cache_meta must reject.
        let mut altered_meta = StoredArtifactMeta {
            schema_version: "1".into(),
            name: m.name.clone(),
            version: m.version.clone(),
            platform: m.platform.clone(),
            url: m.url.clone(),
            sha256: "0".repeat(64),
            archive_format: archive_format_name(&m.archive_format),
            provides: m.provides.clone(),
            bin_dir: m.layout.bin_dir.clone(),
            lib_dir: m.layout.lib_dir.clone(),
            share_dir: m.layout.share_dir.clone(),
        };
        altered_meta.sha256 = "0".repeat(64);
        fs::write(
            dir.join(META_FILENAME),
            serde_json::to_vec(&altered_meta).unwrap(),
        )
        .unwrap();
        assert!(read_cache_meta(ato_home.path(), &m).is_none());
    }
}
