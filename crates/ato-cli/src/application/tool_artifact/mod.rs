//! Tool artifact resolver — ato-managed prebuilt binaries for provider
//! tools (Postgres, Redis, …).
//!
//! Surface: [`resolve_tool_artifact`] is the only entry point a caller
//! needs. Given a [`ToolArtifactManifest`] and `$ATO_HOME`, the resolver
//!
//! 1. checks the on-disk cache — return immediately if a valid sidecar
//!    matches the manifest's sha256;
//! 2. downloads the URL through Ato's internal HTTP client into a temp
//!    file, streaming through [`sha2::Sha256`];
//! 3. fails fast if the sha256 does not match
//!    ([`ToolArtifactError::ArtifactChecksumMismatch`]) — never unpacks
//!    untrusted bytes;
//! 4. unpacks into a sibling temp dir (`tar.gz` / `tar.xz` / `tar.zst` /
//!    `zip` / `jar+txz`);
//! 5. validates every `provides` entry exists and is executable
//!    ([`ToolArtifactError::ArtifactMissingProvidedCommand`]);
//! 6. atomically renames the unpacked dir into
//!    `<ato_home>/store/tools/<name>-<platform>-<sha256-prefix>/`.
//!
//! The resolver returns a [`ResolvedToolArtifact`] suitable for the
//! orchestrator's provider-env injection step (see #120) and for the
//! ExecutionGraph / receipt builder.
//!
//! No `curl`/`wget` shell-out. No source-compile. No silent fallback to
//! host package managers. Failure modes are typed in
//! [`ToolArtifactError`].

pub(crate) mod download;
pub(crate) mod error;
pub(crate) mod manifest;
pub(crate) mod registry;
pub(crate) mod store;
pub(crate) mod unpack;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

// Re-exports name the orchestrator-facing surface; until #120 wires
// it in, the compiler sees no consumer and would flag each one.
#[allow(unused_imports)]
pub use download::{Downloader, ReqwestDownloader};
#[allow(unused_imports)]
pub use error::ToolArtifactError;
#[allow(unused_imports)]
pub use manifest::{host_platform, ToolArtifactManifest};
#[allow(unused_imports)]
pub use registry::{known_tool_ids, well_known_tool_artifact};

/// Output of [`resolve_tool_artifact`]. Carries everything the
/// orchestrator needs to wire `ATO_TOOL_*` env vars and everything the
/// receipt builder needs to record what was actually used.
///
/// The orchestrator currently consumes only `root`/`bin_dir`/`lib_dir`/
/// `share_dir`/`provides`. `version`/`platform`/`url`/`sha256` are kept
/// on the struct for the receipt builder (a follow-up to #119) — the
/// allow attributes acknowledge that they have no current consumer
/// without losing them from the public surface.
#[derive(Debug, Clone)]
pub struct ResolvedToolArtifact {
    pub name: String,
    #[allow(dead_code)]
    pub version: String,
    #[allow(dead_code)]
    pub platform: String,
    #[allow(dead_code)]
    pub url: String,
    #[allow(dead_code)]
    pub sha256: String,
    /// Top of the unpacked tree on disk.
    pub root: PathBuf,
    /// `<root>/<layout.bin_dir>` — assignable to `ATO_TOOL_<NAME>_BIN_DIR`.
    pub bin_dir: PathBuf,
    /// `<root>/<layout.lib_dir>` — assignable to `ATO_TOOL_<NAME>_LIB_DIR`.
    pub lib_dir: PathBuf,
    /// `<root>/<layout.share_dir>` — assignable to `ATO_TOOL_<NAME>_SHARE_DIR`.
    pub share_dir: PathBuf,
    /// Map of bare command name → resolved absolute path under `bin_dir`.
    /// Each entry is also assignable as `ATO_TOOL_<COMMAND>` (uppercased).
    pub provides: BTreeMap<String, PathBuf>,
    /// `true` when this resolve hit the on-disk cache without any
    /// network IO. Useful for receipts and observability.
    pub from_cache: bool,
}

/// Resolve a tool artifact — see module docs for the full pipeline.
///
/// Pass [`ReqwestDownloader::default`] in production. Tests can pass a
/// custom [`Downloader`] (e.g. `LocalFileDownloader`) to drive the
/// pipeline without a live HTTP listener.
pub fn resolve_tool_artifact(
    manifest: &ToolArtifactManifest,
    ato_home: &Path,
    downloader: &dyn Downloader,
) -> Result<ResolvedToolArtifact, ToolArtifactError> {
    manifest.validate()?;
    enforce_platform(manifest)?;

    if let Some(meta) = store::read_cache_meta(ato_home, manifest) {
        let root = store::cache_dir(ato_home, manifest);
        let provides = store::validate_provides(manifest, &root)?;
        return Ok(build_resolved(manifest, &meta.url, root, provides, true));
    }

    let store_parent = store::store_root(ato_home);
    fs::create_dir_all(&store_parent).map_err(|e| ToolArtifactError::StoreError {
        name: manifest.name.clone(),
        reason: format!("create store {}: {}", store_parent.display(), e),
    })?;

    // Stage download + unpack inside the store dir so the final
    // rename is same-filesystem and atomic.
    let staging =
        tempfile::Builder::new()
            .prefix(".staging-")
            .tempdir_in(&store_parent)
            .map_err(|e| ToolArtifactError::StoreError {
                name: manifest.name.clone(),
                reason: format!("create staging dir: {e}"),
            })?;
    let download_path = staging.path().join("download.bin");
    let unpack_dir = staging.path().join("unpack");
    fs::create_dir_all(&unpack_dir).map_err(|e| ToolArtifactError::StoreError {
        name: manifest.name.clone(),
        reason: format!("create unpack dir: {e}"),
    })?;

    download::fetch_and_verify(downloader, manifest, &download_path)?;
    unpack::unpack_archive(manifest, &download_path, &unpack_dir)?;
    let _ = fs::remove_file(&download_path);
    let provides = store::validate_provides(manifest, &unpack_dir)?;

    // Persist the unpack dir into the store and consume the staging
    // TempDir so its Drop does not try to remove the now-renamed
    // tree. We move out of the TempDir using `into_path` and then
    // clean up the (now-empty) parent ourselves.
    let staging_path = staging.keep();
    let final_root = store::install_atomic(ato_home, manifest, &unpack_dir)?;
    let _ = fs::remove_dir_all(&staging_path);
    let provides_resolved = store::validate_provides(manifest, &final_root)?;
    debug_assert_eq!(provides.len(), provides_resolved.len());
    Ok(build_resolved(
        manifest,
        &manifest.url,
        final_root,
        provides_resolved,
        false,
    ))
}

/// Resolve every tool ID in `tool_ids` against the built-in registry,
/// install each via [`resolve_tool_artifact`], and return the
/// `ATO_TOOL_*` env map that the orchestrator should merge into the
/// provider's spawn env.
///
/// Env keys produced per resolved artifact (all uppercased):
///
/// - `ATO_TOOL_<NAME>_ROOT`        — top of the unpacked tree
/// - `ATO_TOOL_<NAME>_BIN_DIR`     — `<root>/<layout.bin_dir>`
/// - `ATO_TOOL_<NAME>_LIB_DIR`     — `<root>/<layout.lib_dir>`
/// - `ATO_TOOL_<NAME>_SHARE_DIR`   — `<root>/<layout.share_dir>`
/// - `ATO_TOOL_<COMMAND>`          — one per `provides` entry, absolute path
///
/// `<NAME>` is the artifact name uppercased with hyphens replaced by
/// underscores; `<COMMAND>` is each `provides` entry uppercased the
/// same way. So `name = "postgresql"` and `provides = ["initdb",
/// "postgres", "pg_ctl"]` produces `ATO_TOOL_POSTGRESQL_*` plus
/// `ATO_TOOL_INITDB`, `ATO_TOOL_POSTGRES`, `ATO_TOOL_PG_CTL`.
///
/// The function does not set `DYLD_LIBRARY_PATH` / `LD_LIBRARY_PATH`.
/// Artifacts are expected to be relocatable via `@loader_path` /
/// `$ORIGIN`; injecting a library-path env would defeat that and
/// might also be stripped under macOS hardened runtime. Per-platform
/// artifact validation owns the relocatability check.
///
/// Errors propagate from [`resolve_tool_artifact`]. An unknown tool
/// ID becomes a [`ToolArtifactError::InvalidArtifactManifest`] with a
/// reason explaining which IDs are supported on this host.
pub fn resolve_target_tool_env(
    tool_ids: &[String],
    ato_home: &Path,
    downloader: &dyn Downloader,
) -> Result<std::collections::BTreeMap<String, String>, ToolArtifactError> {
    let mut out = std::collections::BTreeMap::new();
    for id in tool_ids {
        let manifest = registry::well_known_tool_artifact(id).ok_or_else(|| {
            let supported = registry::known_tool_ids().join(", ");
            ToolArtifactError::InvalidArtifactManifest {
                name: id.clone(),
                reason: format!(
                    "tool '{id}' has no pinned artifact for this host platform '{}'; known tool ids: [{}]",
                    host_platform().unwrap_or("unknown"),
                    supported
                ),
            }
        })?;
        let resolved = resolve_tool_artifact(&manifest, ato_home, downloader)?;
        merge_resolved_into_env(&resolved, &mut out);
    }
    Ok(out)
}

fn merge_resolved_into_env(
    resolved: &ResolvedToolArtifact,
    out: &mut std::collections::BTreeMap<String, String>,
) {
    let prefix = format!("ATO_TOOL_{}", normalize_env_name(&resolved.name));
    out.insert(format!("{prefix}_ROOT"), resolved.root.display().to_string());
    out.insert(
        format!("{prefix}_BIN_DIR"),
        resolved.bin_dir.display().to_string(),
    );
    out.insert(
        format!("{prefix}_LIB_DIR"),
        resolved.lib_dir.display().to_string(),
    );
    out.insert(
        format!("{prefix}_SHARE_DIR"),
        resolved.share_dir.display().to_string(),
    );
    for (cmd, path) in &resolved.provides {
        let key = format!("ATO_TOOL_{}", normalize_env_name(cmd));
        out.insert(key, path.display().to_string());
    }
}

fn normalize_env_name(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' => c.to_ascii_uppercase(),
            'A'..='Z' | '0'..='9' | '_' => c,
            '-' | '.' => '_',
            other => other,
        })
        .collect()
}

fn enforce_platform(manifest: &ToolArtifactManifest) -> Result<(), ToolArtifactError> {
    let host = host_platform().unwrap_or("unknown");
    if manifest.platform != host {
        return Err(ToolArtifactError::UnsupportedArtifactPlatform {
            name: manifest.name.clone(),
            platform: manifest.platform.clone(),
            host: host.to_string(),
        });
    }
    Ok(())
}

fn build_resolved(
    manifest: &ToolArtifactManifest,
    url: &str,
    root: PathBuf,
    provides: BTreeMap<String, PathBuf>,
    from_cache: bool,
) -> ResolvedToolArtifact {
    ResolvedToolArtifact {
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        platform: manifest.platform.clone(),
        url: url.to_string(),
        sha256: manifest.sha256.clone(),
        bin_dir: root.join(&manifest.layout.bin_dir),
        lib_dir: root.join(&manifest.layout.lib_dir),
        share_dir: root.join(&manifest.layout.share_dir),
        provides,
        root,
        from_cache,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::tool_artifact::download::test_support::LocalFileDownloader;
    use crate::application::tool_artifact::manifest::{ArchiveFormat, ArtifactLayout};
    use std::io::{Cursor, Write};
    use tar::Header;

    fn build_synthetic_tar_gz() -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        let mut h = Header::new_gnu();
        h.set_size(0);
        h.set_mode(0o755);
        h.set_entry_type(tar::EntryType::Directory);
        h.set_cksum();
        builder
            .append_data(&mut h, "bin/", std::io::empty())
            .unwrap();
        let demo = b"#!/bin/sh\necho demo\n";
        let mut h = Header::new_gnu();
        h.set_size(demo.len() as u64);
        h.set_mode(0o755);
        h.set_cksum();
        builder
            .append_data(&mut h, "bin/demo", Cursor::new(demo))
            .unwrap();
        let mut h = Header::new_gnu();
        h.set_size(0);
        h.set_mode(0o755);
        h.set_entry_type(tar::EntryType::Directory);
        h.set_cksum();
        builder
            .append_data(&mut h, "lib/", std::io::empty())
            .unwrap();
        let mut h = Header::new_gnu();
        h.set_size(0);
        h.set_mode(0o755);
        h.set_entry_type(tar::EntryType::Directory);
        h.set_cksum();
        builder
            .append_data(&mut h, "share/", std::io::empty())
            .unwrap();
        let raw = builder.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&raw).unwrap();
        gz.finish().unwrap()
    }

    fn make_manifest(url: String, sha256: String) -> ToolArtifactManifest {
        ToolArtifactManifest {
            schema_version: "1".into(),
            name: "demo".into(),
            version: "1.0.0".into(),
            platform: host_platform().unwrap_or("linux-x86_64").to_string(),
            url,
            sha256,
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

    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(bytes);
        hex::encode(h.finalize())
    }

    #[test]
    fn resolve_end_to_end_downloads_unpacks_validates_and_caches() {
        let tmp_src = tempfile::tempdir().unwrap();
        let archive_bytes = build_synthetic_tar_gz();
        let archive_path = tmp_src.path().join("demo.tar.gz");
        std::fs::write(&archive_path, &archive_bytes).unwrap();
        let url = format!("test-local://{}", archive_path.display());
        let sha = sha256_hex(&archive_bytes);
        let manifest = make_manifest(url, sha);

        let ato_home = tempfile::tempdir().unwrap();

        let resolved =
            resolve_tool_artifact(&manifest, ato_home.path(), &LocalFileDownloader)
                .expect("first resolve must succeed");
        assert!(!resolved.from_cache, "first resolve is a fresh install");
        assert_eq!(resolved.name, "demo");
        assert!(
            resolved.bin_dir.ends_with("bin"),
            "bin_dir = {}",
            resolved.bin_dir.display()
        );
        let demo = resolved
            .provides
            .get("demo")
            .expect("demo provided")
            .clone();
        assert!(demo.is_file());
        // Receipt-grade: root is under store/tools/<key>
        assert!(
            resolved
                .root
                .to_string_lossy()
                .contains("store/tools/demo-"),
            "root = {}",
            resolved.root.display()
        );

        // Second call hits the cache and skips download.
        let again =
            resolve_tool_artifact(&manifest, ato_home.path(), &FailingDownloader)
                .expect("cache hit must avoid downloader");
        assert!(again.from_cache);
        assert_eq!(again.root, resolved.root);
        assert_eq!(again.provides.get("demo"), Some(&demo));
    }

    #[test]
    fn resolve_rejects_checksum_mismatch_before_unpack() {
        let tmp_src = tempfile::tempdir().unwrap();
        let archive_bytes = build_synthetic_tar_gz();
        let archive_path = tmp_src.path().join("demo.tar.gz");
        std::fs::write(&archive_path, &archive_bytes).unwrap();
        let url = format!("test-local://{}", archive_path.display());
        let manifest = make_manifest(
            url,
            "0000000000000000000000000000000000000000000000000000000000000000".into(),
        );
        let ato_home = tempfile::tempdir().unwrap();
        let err = resolve_tool_artifact(&manifest, ato_home.path(), &LocalFileDownloader)
            .expect_err("must reject");
        match err {
            ToolArtifactError::ArtifactChecksumMismatch { .. } => {}
            other => panic!("unexpected: {other}"),
        }
        // The store dir must not be left containing a partial unpack.
        let key_dir = store::cache_dir(ato_home.path(), &manifest);
        assert!(
            !key_dir.exists(),
            "checksum mismatch must not leave a store entry: {}",
            key_dir.display()
        );
    }

    #[test]
    fn resolve_rejects_unsupported_platform() {
        let tmp_src = tempfile::tempdir().unwrap();
        let archive_bytes = build_synthetic_tar_gz();
        let archive_path = tmp_src.path().join("demo.tar.gz");
        std::fs::write(&archive_path, &archive_bytes).unwrap();
        let url = format!("test-local://{}", archive_path.display());
        let mut manifest = make_manifest(url, sha256_hex(&archive_bytes));
        manifest.platform = "imaginary-os-vax".into();
        let ato_home = tempfile::tempdir().unwrap();
        let err = resolve_tool_artifact(&manifest, ato_home.path(), &LocalFileDownloader)
            .expect_err("must reject");
        match err {
            ToolArtifactError::UnsupportedArtifactPlatform { platform, .. } => {
                assert_eq!(platform, "imaginary-os-vax");
            }
            other => panic!("unexpected: {other}"),
        }
    }

    #[test]
    fn resolve_rejects_missing_provides_after_unpack() {
        let tmp_src = tempfile::tempdir().unwrap();
        let archive_bytes = build_synthetic_tar_gz();
        let archive_path = tmp_src.path().join("demo.tar.gz");
        std::fs::write(&archive_path, &archive_bytes).unwrap();
        let url = format!("test-local://{}", archive_path.display());
        let mut manifest = make_manifest(url, sha256_hex(&archive_bytes));
        // The synthetic archive provides `demo`. Asking for `missing`
        // forces the post-unpack validation to fail.
        manifest.provides = vec!["missing".into()];
        let ato_home = tempfile::tempdir().unwrap();
        let err = resolve_tool_artifact(&manifest, ato_home.path(), &LocalFileDownloader)
            .expect_err("must reject");
        match err {
            ToolArtifactError::ArtifactMissingProvidedCommand { command, .. } => {
                assert_eq!(command, "missing");
            }
            other => panic!("unexpected: {other}"),
        }
        // Critical: the unpack dir must not have been promoted into
        // the store. A failed `provides` validation must leave the
        // cache untouched so the next run re-downloads.
        let key_dir = store::cache_dir(ato_home.path(), &manifest);
        assert!(
            !key_dir.exists(),
            "missing provides must not leave a store entry"
        );
    }

    /// Test downloader that errors on every call. The cache-hit path
    /// must avoid hitting it; if the cache lookup is wrong, this
    /// downloader fails the second resolve loud and clear.
    struct FailingDownloader;
    impl Downloader for FailingDownloader {
        fn fetch_to(
            &self,
            _url: &str,
            _dest: &Path,
        ) -> Result<download::DownloadOutcome, anyhow::Error> {
            Err(anyhow::anyhow!(
                "downloader must not be called on cache hit"
            ))
        }
    }

    #[test]
    fn resolve_target_tool_env_emits_expected_keys() {
        let tmp_src = tempfile::tempdir().unwrap();
        let archive_bytes = build_synthetic_tar_gz();
        let archive_path = tmp_src.path().join("demo.tar.gz");
        std::fs::write(&archive_path, &archive_bytes).unwrap();
        let url = format!("test-local://{}", archive_path.display());
        let manifest = make_manifest(url, sha256_hex(&archive_bytes));
        // Resolve once via the public API to populate the cache, then
        // hand-merge to verify the env-key shape (resolve_target_tool_env
        // only knows registry-tools so it can't accept this synthetic
        // manifest directly — we test merge_resolved_into_env instead).
        let ato_home = tempfile::tempdir().unwrap();
        let resolved = resolve_tool_artifact(&manifest, ato_home.path(), &LocalFileDownloader)
            .expect("resolve");
        let mut env = std::collections::BTreeMap::new();
        merge_resolved_into_env(&resolved, &mut env);
        assert_eq!(env.get("ATO_TOOL_DEMO_ROOT"), Some(&resolved.root.display().to_string()));
        assert_eq!(env.get("ATO_TOOL_DEMO_BIN_DIR"), Some(&resolved.bin_dir.display().to_string()));
        assert_eq!(env.get("ATO_TOOL_DEMO_LIB_DIR"), Some(&resolved.lib_dir.display().to_string()));
        assert_eq!(env.get("ATO_TOOL_DEMO_SHARE_DIR"), Some(&resolved.share_dir.display().to_string()));
        assert!(env.contains_key("ATO_TOOL_DEMO"));
        // No DYLD_LIBRARY_PATH / LD_LIBRARY_PATH leaks from this layer.
        assert!(!env.contains_key("DYLD_LIBRARY_PATH"));
        assert!(!env.contains_key("LD_LIBRARY_PATH"));
    }

    #[test]
    fn resolve_target_tool_env_rejects_unknown_tool_id() {
        let ato_home = tempfile::tempdir().unwrap();
        let err = resolve_target_tool_env(
            &["does-not-exist".to_string()],
            ato_home.path(),
            &LocalFileDownloader,
        )
        .expect_err("must reject unknown tool id");
        match err {
            ToolArtifactError::InvalidArtifactManifest { name, reason } => {
                assert_eq!(name, "does-not-exist");
                assert!(reason.contains("known tool ids"));
            }
            other => panic!("unexpected: {other}"),
        }
    }

    #[test]
    fn normalize_env_name_uppercases_and_sanitizes() {
        assert_eq!(normalize_env_name("postgresql"), "POSTGRESQL");
        assert_eq!(normalize_env_name("pg_ctl"), "PG_CTL");
        assert_eq!(normalize_env_name("foo-bar.baz"), "FOO_BAR_BAZ");
    }

    /// Real-world AODD anchor: drives the production [`ReqwestDownloader`]
    /// against the pinned upstream Postgres 16.9.0 JAR from Maven
    /// Central, verifies its sha256, unpacks the JAR-wrapping-txz, and
    /// confirms the `provides` set matches what zonky actually ships.
    ///
    /// `#[ignore]` because it requires network and is darwin-arm64 only.
    /// Run manually when bumping the manifest pin:
    ///
    /// ```bash
    /// cargo test -p ato-cli --lib \
    ///     application::tool_artifact::tests::resolve_real_zonky_postgres_16_9_0 \
    ///     -- --ignored --nocapture
    /// ```
    ///
    /// The hashes here are the same values posted to issue #120 from
    /// Phase 1 of the AODD investigation.
    #[test]
    #[ignore = "network + darwin-arm64 only; run manually after manifest bumps"]
    fn resolve_real_zonky_postgres_16_9_0() {
        if host_platform() != Some("darwin-aarch64") {
            eprintln!("skipping: zonky darwin-arm64v8 manifest pinned to darwin-aarch64");
            return;
        }
        let manifest = ToolArtifactManifest {
            schema_version: "1".into(),
            name: "postgresql".into(),
            version: "16.9.0".into(),
            platform: "darwin-aarch64".into(),
            url: "https://repo1.maven.org/maven2/io/zonky/test/postgres/embedded-postgres-binaries-darwin-arm64v8/16.9.0/embedded-postgres-binaries-darwin-arm64v8-16.9.0.jar".into(),
            sha256: "53b2672c602e16e4c94fb56b9aa68cc26a0bbb0df851f256f41a2cdbeccc9cb6".into(),
            archive_format: ArchiveFormat::JarTxz,
            inner_member: Some("postgres-darwin-arm_64.txz".into()),
            inner_sha256: Some(
                "090e91773217f8d3d222699a6da2bf5533ffab8c6b65b14df63cba3b1b63ea5a".into(),
            ),
            strip_prefix: None,
            layout: ArtifactLayout {
                bin_dir: "bin".into(),
                lib_dir: "lib".into(),
                share_dir: "share".into(),
            },
            // The artifact deliberately does NOT ship pg_isready (see
            // Phase 1 finding). The orchestrator gets readiness from a
            // native postgres probe, not a binary.
            provides: vec!["initdb".into(), "postgres".into(), "pg_ctl".into()],
        };
        let ato_home = tempfile::tempdir().expect("ato_home");
        let resolved = resolve_tool_artifact(&manifest, ato_home.path(), &ReqwestDownloader::default())
            .expect("resolve must succeed against real upstream");
        assert_eq!(resolved.name, "postgresql");
        assert_eq!(resolved.version, "16.9.0");
        assert!(!resolved.from_cache);
        for cmd in ["initdb", "postgres", "pg_ctl"] {
            let p = resolved.provides.get(cmd).expect("provides entry");
            assert!(p.is_file(), "{cmd} not a file: {}", p.display());
        }
        // pg_isready was deliberately not in `provides` and must not
        // appear in the resolved map.
        assert!(!resolved.provides.contains_key("pg_isready"));

        // Round-trip cache: second resolve must hit cache without
        // touching the network (FailingDownloader would fail).
        let cached = resolve_tool_artifact(&manifest, ato_home.path(), &FailingDownloader)
            .expect("cache hit");
        assert!(cached.from_cache);
        assert_eq!(cached.root, resolved.root);
    }
}
