use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use capsule::common::paths::ato_path;
use serde::{Deserialize, Serialize};

use super::registry::{
    self, SOURCE_EXCLUDED_DIRS, SYSTEM_WORKSPACE_FILES, SeedCopyMode, SystemCapsuleLayout,
    resolve_serving_root,
};

const SYSTEM_CAPSULES_HOME: &str = "apps/ato-desktop/system-capsules";
const CURRENT_RECORD_FILE: &str = "current.json";
const DEGRADATION_SUFFIX: &str = "degraded.json";
const STATE_FILE: &str = ".ato-system-capsule.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedSystemCapsule {
    pub capsule: String,
    pub seed_hash: String,
    pub lockfile_hash: Option<String>,
    pub materialized_dir: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemCapsuleBootstrapReport {
    pub materialized: Vec<MaterializedSystemCapsule>,
    pub reused: Vec<MaterializedSystemCapsule>,
    pub degraded: Vec<DegradedSystemCapsule>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemCapsuleRoot {
    pub slug: String,
    pub root: PathBuf,
    pub serving_root: PathBuf,
    pub seed_hash: String,
    pub lockfile_hash: Option<String>,
    pub layout: SystemCapsuleLayout,
    pub degraded: Option<DegradedSystemCapsule>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemCapsuleLookup {
    Ready(SystemCapsuleRoot),
    Unavailable(SystemCapsuleUnavailable),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemCapsuleUnavailable {
    pub slug: String,
    pub degraded: Option<DegradedSystemCapsule>,
    pub kind: SystemCapsuleUnavailableKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemCapsuleUnavailableKind {
    UnknownCapsule,
    MissingCurrentRecord,
    MaterializedRootMissing {
        root: PathBuf,
    },
    ServingRootMissing {
        root: PathBuf,
        serving_root: PathBuf,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DegradedSystemCapsule {
    pub capsule: String,
    pub error: String,
    pub seed_hash: Option<String>,
    pub lockfile_hash: Option<String>,
    pub recorded_at_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MaterializedSeedState {
    capsule: String,
    seed_hash: String,
    lockfile_hash: Option<String>,
    materialized_dir: String,
    updated_at_unix_ms: u64,
}

pub fn bootstrap_from_assets(assets_dir: &Path) -> Result<SystemCapsuleBootstrapReport> {
    let system_dir = assets_dir.join("system");
    if !system_dir.is_dir() {
        anyhow::bail!(
            "system capsule assets directory does not exist: {}",
            system_dir.display()
        );
    }

    let home = system_capsules_home()?;
    fs::create_dir_all(&home)
        .with_context(|| format!("failed to create system capsule home {}", home.display()))?;

    let mut report = SystemCapsuleBootstrapReport::default();
    for entry in registry::all() {
        bootstrap_seed(&system_dir, &home, entry, &mut report);
    }
    super::static_resolver::clear_lookup_cache();
    Ok(report)
}

pub fn current_materialized_root(capsule: &str) -> Result<Option<PathBuf>> {
    match lookup_system_capsule(capsule)? {
        SystemCapsuleLookup::Ready(root) => Ok(Some(root.root)),
        SystemCapsuleLookup::Unavailable(_) => Ok(None),
    }
}

pub fn resolve_system_capsule_root(capsule: &str) -> Result<SystemCapsuleRoot> {
    match lookup_system_capsule(capsule)? {
        SystemCapsuleLookup::Ready(root) => Ok(root),
        SystemCapsuleLookup::Unavailable(unavailable) => anyhow::bail!(
            "system capsule {} is unavailable: {:?}",
            unavailable.slug,
            unavailable.kind
        ),
    }
}

pub fn lookup_system_capsule(capsule: &str) -> Result<SystemCapsuleLookup> {
    let Some(entry) = registry::lookup(capsule) else {
        return Ok(SystemCapsuleLookup::Unavailable(SystemCapsuleUnavailable {
            slug: capsule.to_string(),
            degraded: None,
            kind: SystemCapsuleUnavailableKind::UnknownCapsule,
        }));
    };

    let home = system_capsules_home()?;
    let degraded = read_degraded_system_capsule(entry.slug)?;
    let record_path = capsule_record_path(&home, entry.slug);
    if !record_path.is_file() {
        return Ok(SystemCapsuleLookup::Unavailable(SystemCapsuleUnavailable {
            slug: entry.slug.to_string(),
            degraded,
            kind: SystemCapsuleUnavailableKind::MissingCurrentRecord,
        }));
    }

    let record = read_materialized_seed_state(&record_path)?;
    let root = PathBuf::from(&record.materialized_dir);
    if !root.is_dir() {
        return Ok(SystemCapsuleLookup::Unavailable(SystemCapsuleUnavailable {
            slug: entry.slug.to_string(),
            degraded,
            kind: SystemCapsuleUnavailableKind::MaterializedRootMissing { root },
        }));
    }

    let serving_root = resolve_serving_root(entry, &root)?;
    if !serving_root.is_dir() {
        return Ok(SystemCapsuleLookup::Unavailable(SystemCapsuleUnavailable {
            slug: entry.slug.to_string(),
            degraded,
            kind: SystemCapsuleUnavailableKind::ServingRootMissing { root, serving_root },
        }));
    }

    Ok(SystemCapsuleLookup::Ready(SystemCapsuleRoot {
        slug: entry.slug.to_string(),
        root,
        serving_root,
        seed_hash: record.seed_hash,
        lockfile_hash: record.lockfile_hash,
        layout: entry.layout,
        degraded,
    }))
}

pub fn read_degraded_system_capsule(capsule: &str) -> Result<Option<DegradedSystemCapsule>> {
    if registry::lookup(capsule).is_none() {
        return Ok(None);
    }

    let path = degraded_record_path(&system_capsules_home()?, capsule);
    if !path.is_file() {
        return Ok(None);
    }

    let bytes = fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let degraded = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse degraded record {}", path.display()))?;
    Ok(Some(degraded))
}

fn bootstrap_seed(
    system_dir: &Path,
    home: &Path,
    entry: &registry::SystemCapsuleRegistryEntry,
    report: &mut SystemCapsuleBootstrapReport,
) {
    let capsule_source = system_dir.join(entry.slug);
    if !capsule_source.exists() {
        return;
    }

    let capsule_home = home.join(entry.slug);
    if let Err(error) = fs::create_dir_all(&capsule_home) {
        report_degraded(home, entry.slug, error.into(), None, None, report);
        return;
    }

    let staging_dir = capsule_home.join(staging_dir_name());
    if staging_dir.exists() {
        let _ = fs::remove_dir_all(&staging_dir);
    }
    if let Err(error) = fs::create_dir_all(&staging_dir) {
        report_degraded(home, entry.slug, error.into(), None, None, report);
        return;
    }

    let stage_result = match entry.copy_mode {
        SeedCopyMode::FileSet(files) => copy_seed_files(&capsule_source, &staging_dir, files),
        SeedCopyMode::SourceTree => {
            copy_filtered_tree(&capsule_source, &staging_dir, SOURCE_EXCLUDED_DIRS)
        }
        SeedCopyMode::SourceTreeWithDist => copy_filtered_tree(
            &capsule_source,
            &staging_dir,
            &["node_modules", ".vite", ".astro", ".next", "target"],
        ),
        SeedCopyMode::StaticDist => copy_static_dist(&capsule_source, &staging_dir),
        SeedCopyMode::WorkspaceMembers(members) => {
            copy_workspace_seed(system_dir, &staging_dir, members)
        }
    };
    if let Err(error) = stage_result {
        let _ = fs::remove_dir_all(&staging_dir);
        report_degraded(home, entry.slug, error, None, None, report);
        return;
    }

    let seed_hash = match hash_tree(&staging_dir) {
        Ok(hash) => hash,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging_dir);
            report_degraded(home, entry.slug, error, None, None, report);
            return;
        }
    };
    let lockfile_hash = match resolve_lockfile_hash(&staging_dir) {
        Ok(hash) => hash,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging_dir);
            report_degraded(home, entry.slug, error, Some(seed_hash), None, report);
            return;
        }
    };
    let target_dir = capsule_home.join(seed_hash_dir_name(&seed_hash));

    if target_dir.exists() {
        let _ = fs::remove_dir_all(&staging_dir);
        if let Err(error) = write_materialized_seed_state(
            entry.slug,
            &capsule_home,
            &target_dir,
            &seed_hash,
            lockfile_hash.as_deref(),
        ) {
            report_degraded(
                home,
                entry.slug,
                error,
                Some(seed_hash),
                lockfile_hash,
                report,
            );
            return;
        }
        clear_degraded_state(home, entry.slug);
        report.reused.push(MaterializedSystemCapsule {
            capsule: entry.slug.to_string(),
            seed_hash,
            lockfile_hash,
            materialized_dir: target_dir,
        });
        return;
    }

    if let Err(error) = fs::rename(&staging_dir, &target_dir) {
        let _ = fs::remove_dir_all(&staging_dir);
        report_degraded(
            home,
            entry.slug,
            error.into(),
            Some(seed_hash),
            lockfile_hash,
            report,
        );
        return;
    }

    if let Err(error) = write_materialized_seed_state(
        entry.slug,
        &capsule_home,
        &target_dir,
        &seed_hash,
        lockfile_hash.as_deref(),
    ) {
        report_degraded(
            home,
            entry.slug,
            error,
            Some(seed_hash),
            lockfile_hash,
            report,
        );
        return;
    }
    clear_degraded_state(home, entry.slug);
    report.materialized.push(MaterializedSystemCapsule {
        capsule: entry.slug.to_string(),
        seed_hash,
        lockfile_hash,
        materialized_dir: target_dir,
    });
}

fn system_capsules_home() -> Result<PathBuf> {
    ato_path(SYSTEM_CAPSULES_HOME).context("failed to resolve system capsule home")
}

fn copy_seed_files(from: &Path, to: &Path, files: &[&str]) -> Result<()> {
    for file in files {
        copy_optional_file(&from.join(file), &to.join(file))?;
    }
    Ok(())
}

fn copy_workspace_seed(system_dir: &Path, staging_dir: &Path, members: &[&str]) -> Result<()> {
    for file in SYSTEM_WORKSPACE_FILES {
        copy_optional_file(&system_dir.join(file), &staging_dir.join(file))?;
    }
    for member in members {
        copy_filtered_tree(
            &system_dir.join(member),
            &staging_dir.join(member),
            SOURCE_EXCLUDED_DIRS,
        )?;
    }
    Ok(())
}

fn copy_static_dist(from: &Path, to: &Path) -> Result<()> {
    copy_filtered_tree(&from.join("dist"), &to.join("dist"), &[])
}

fn copy_filtered_tree(from: &Path, to: &Path, excluded_dirs: &[&str]) -> Result<()> {
    if !from.exists() {
        return Ok(());
    }
    if !from.is_dir() {
        anyhow::bail!(
            "expected directory but found non-directory entry: {}",
            from.display()
        );
    }

    fs::create_dir_all(to).with_context(|| format!("failed to create {}", to.display()))?;
    for entry in fs::read_dir(from).with_context(|| format!("failed to read {}", from.display()))? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() && excluded_dirs.iter().any(|excluded| *excluded == name) {
            continue;
        }

        let destination = to.join(entry.file_name());
        if path.is_dir() {
            copy_filtered_tree(&path, &destination, excluded_dirs)?;
        } else {
            copy_optional_file(&path, &destination)?;
        }
    }

    Ok(())
}

fn copy_optional_file(from: &Path, to: &Path) -> Result<()> {
    if !from.exists() {
        return Ok(());
    }
    if !from.is_file() {
        anyhow::bail!("expected file but found non-file entry: {}", from.display());
    }

    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::copy(from, to)
        .with_context(|| format!("failed to copy {} to {}", from.display(), to.display()))?;
    Ok(())
}

fn hash_tree(root: &Path) -> Result<String> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));

    let mut hasher = blake3::Hasher::new();
    for (relative, path) in files {
        hasher.update(relative.as_bytes());
        hasher.update(&[0]);
        let bytes = fs::read(&path)
            .with_context(|| format!("failed to read {} while hashing seed", path.display()))?;
        hasher.update(&bytes);
        hasher.update(&[0xff]);
    }
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

fn collect_files(root: &Path, current: &Path, files: &mut Vec<(String, PathBuf)>) -> Result<()> {
    for entry in
        fs::read_dir(current).with_context(|| format!("failed to read {}", current.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(root, &path, files)?;
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .with_context(|| {
                format!(
                    "failed to relativize {} against {}",
                    path.display(),
                    root.display()
                )
            })?
            .to_string_lossy()
            .replace('\\', "/");
        files.push((relative, path));
    }
    Ok(())
}

fn resolve_lockfile_hash(staged_dir: &Path) -> Result<Option<String>> {
    for candidate in [
        "package-lock.json",
        "pnpm-lock.yaml",
        "yarn.lock",
        "Cargo.lock",
    ] {
        let path = staged_dir.join(candidate);
        if path.is_file() {
            return Ok(Some(hash_file(&path)?));
        }
    }
    Ok(None)
}

fn hash_file(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(&bytes);
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

fn write_materialized_seed_state(
    capsule: &str,
    capsule_home: &Path,
    materialized_dir: &Path,
    seed_hash: &str,
    lockfile_hash: Option<&str>,
) -> Result<()> {
    let now = now_unix_ms();
    let state = MaterializedSeedState {
        capsule: capsule.to_string(),
        seed_hash: seed_hash.to_string(),
        lockfile_hash: lockfile_hash.map(str::to_string),
        materialized_dir: materialized_dir.display().to_string(),
        updated_at_unix_ms: now,
    };
    let json =
        serde_json::to_vec_pretty(&state).context("failed to serialize materialized seed state")?;

    fs::write(materialized_dir.join(STATE_FILE), &json).with_context(|| {
        format!(
            "failed to write {}",
            materialized_dir.join(STATE_FILE).display()
        )
    })?;
    fs::write(
        capsule_record_path(capsule_home.parent().unwrap_or(capsule_home), capsule),
        json,
    )
    .with_context(|| format!("failed to write current record for {capsule}"))?;
    Ok(())
}

fn read_materialized_seed_state(path: &Path) -> Result<MaterializedSeedState> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse materialized seed state {}", path.display()))
}

fn report_degraded(
    home: &Path,
    capsule: &str,
    error: anyhow::Error,
    seed_hash: Option<String>,
    lockfile_hash: Option<String>,
    report: &mut SystemCapsuleBootstrapReport,
) {
    let degraded = DegradedSystemCapsule {
        capsule: capsule.to_string(),
        error: error.to_string(),
        seed_hash,
        lockfile_hash,
        recorded_at_unix_ms: now_unix_ms(),
    };

    if let Ok(json) = serde_json::to_vec_pretty(&degraded) {
        let _ = fs::write(degraded_record_path(home, capsule), json);
    }
    report.degraded.push(degraded);
}

fn clear_degraded_state(home: &Path, capsule: &str) {
    let _ = fs::remove_file(degraded_record_path(home, capsule));
}

fn capsule_record_path(home: &Path, capsule: &str) -> PathBuf {
    home.join(capsule).join(CURRENT_RECORD_FILE)
}

fn degraded_record_path(home: &Path, capsule: &str) -> PathBuf {
    home.join(format!("{capsule}.{DEGRADATION_SUFFIX}"))
}

fn staging_dir_name() -> String {
    format!(".staging-{}-{}", std::process::id(), now_unix_ms())
}

fn seed_hash_dir_name(seed_hash: &str) -> String {
    seed_hash.replace(':', "-")
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::{
        SystemCapsuleLayout, SystemCapsuleLookup, SystemCapsuleUnavailableKind,
        bootstrap_from_assets, current_materialized_root, lookup_system_capsule,
        resolve_system_capsule_root,
    };
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    struct AtoHomeGuard {
        previous: Option<std::ffi::OsString>,
    }

    impl AtoHomeGuard {
        fn new(path: &Path) -> Self {
            let previous = std::env::var_os("ATO_HOME");
            unsafe {
                std::env::set_var("ATO_HOME", path);
            }
            Self { previous }
        }
    }

    impl Drop for AtoHomeGuard {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.as_ref() {
                unsafe {
                    std::env::set_var("ATO_HOME", previous);
                }
            } else {
                unsafe {
                    std::env::remove_var("ATO_HOME");
                }
            }
        }
    }

    #[test]
    fn bootstrap_system_capsules_materializes_source_seed_into_ato_home() {
        let home = TempDir::new().expect("temp home should exist");
        let assets = TempDir::new().expect("temp assets should exist");
        let _guard = AtoHomeGuard::new(home.path());

        fs::create_dir_all(assets.path().join("system").join("ato-dock").join("src"))
            .expect("dock src should exist");
        fs::create_dir_all(
            assets
                .path()
                .join("system")
                .join("ato-dock")
                .join("node_modules"),
        )
        .expect("dock node_modules should exist");
        fs::write(
            assets
                .path()
                .join("system")
                .join("ato-dock")
                .join("package.json"),
            b"{}",
        )
        .expect("dock package should exist");
        fs::write(
            assets
                .path()
                .join("system")
                .join("ato-dock")
                .join("src")
                .join("main.jsx"),
            b"export default null;",
        )
        .expect("dock source should exist");
        fs::write(
            assets
                .path()
                .join("system")
                .join("ato-dock")
                .join("node_modules")
                .join("ignored.txt"),
            b"ignored",
        )
        .expect("ignored dependency should exist");

        let report = bootstrap_from_assets(assets.path()).expect("bootstrap should succeed");

        assert_eq!(report.materialized.len(), 1);
        assert!(report.reused.is_empty());
        assert!(report.degraded.is_empty());

        let root = current_materialized_root("ato-dock")
            .expect("current materialized root should resolve")
            .expect("current materialized root should exist");
        assert!(root.join("package.json").is_file());
        assert!(root.join("src").join("main.jsx").is_file());
        assert!(!root.join("node_modules").exists());
        assert!(root.join(".ato-system-capsule.json").is_file());
    }

    #[test]
    fn resolve_system_capsule_root_returns_layout_and_serving_root_for_dist_seed() {
        let home = TempDir::new().expect("temp home should exist");
        let assets = TempDir::new().expect("temp assets should exist");
        let _guard = AtoHomeGuard::new(home.path());

        fs::create_dir_all(
            assets
                .path()
                .join("system")
                .join("ato-onboarding")
                .join("dist"),
        )
        .expect("onboarding dist should exist");
        fs::write(
            assets
                .path()
                .join("system")
                .join("ato-onboarding")
                .join("dist")
                .join("index.html"),
            b"<html>onboarding</html>",
        )
        .expect("onboarding index should exist");

        bootstrap_from_assets(assets.path()).expect("bootstrap should succeed");

        let root =
            resolve_system_capsule_root("ato-onboarding").expect("onboarding root should resolve");
        assert_eq!(root.layout, SystemCapsuleLayout::DistSeed);
        assert_eq!(root.serving_root, root.root.join("dist"));
        assert!(root.serving_root.join("index.html").is_file());
    }

    #[test]
    fn lookup_system_capsule_reports_missing_current_record() {
        let home = TempDir::new().expect("temp home should exist");
        let _guard = AtoHomeGuard::new(home.path());

        let lookup = lookup_system_capsule("ato-store").expect("lookup should not fail");
        match lookup {
            SystemCapsuleLookup::Unavailable(unavailable) => {
                assert_eq!(unavailable.slug, "ato-store");
                assert_eq!(
                    unavailable.kind,
                    SystemCapsuleUnavailableKind::MissingCurrentRecord
                );
            }
            other => panic!("expected unavailable state, got {other:?}"),
        }
    }

    #[test]
    fn lookup_system_capsule_surfaces_degraded_record_on_ready_root() {
        let home = TempDir::new().expect("temp home should exist");
        let assets = TempDir::new().expect("temp assets should exist");
        let _guard = AtoHomeGuard::new(home.path());

        fs::create_dir_all(assets.path().join("system").join("ato-store").join("dist"))
            .expect("store dist should exist");
        fs::write(
            assets
                .path()
                .join("system")
                .join("ato-store")
                .join("dist")
                .join("index.html"),
            b"store",
        )
        .expect("store dist should exist");

        bootstrap_from_assets(assets.path()).expect("bootstrap should succeed");
        fs::write(
            home.path()
                .join("apps")
                .join("ato-desktop")
                .join("system-capsules")
                .join("ato-store.degraded.json"),
            r#"{"capsule":"ato-store","error":"seed failed","seed_hash":null,"lockfile_hash":null,"recorded_at_unix_ms":1}"#,
        )
        .expect("degraded record should write");

        let root = resolve_system_capsule_root("ato-store").expect("store root should resolve");
        assert_eq!(
            root.degraded.expect("degraded record should exist").error,
            "seed failed"
        );
    }

    #[test]
    fn lookup_system_capsule_reports_missing_serving_root_as_corruption() {
        let home = TempDir::new().expect("temp home should exist");
        let assets = TempDir::new().expect("temp assets should exist");
        let _guard = AtoHomeGuard::new(home.path());

        fs::create_dir_all(assets.path().join("system").join("ato-store").join("dist"))
            .expect("store dist should exist");
        fs::write(
            assets
                .path()
                .join("system")
                .join("ato-store")
                .join("dist")
                .join("index.html"),
            b"store",
        )
        .expect("store dist should exist");

        bootstrap_from_assets(assets.path()).expect("bootstrap should succeed");
        let root = current_materialized_root("ato-store")
            .expect("current root should resolve")
            .expect("current root should exist");
        fs::remove_dir_all(root.join("dist")).expect("dist dir should be removable");

        let lookup = lookup_system_capsule("ato-store").expect("lookup should not fail");
        match lookup {
            SystemCapsuleLookup::Unavailable(unavailable) => match unavailable.kind {
                SystemCapsuleUnavailableKind::ServingRootMissing { .. } => {}
                other => panic!("expected serving root corruption, got {other:?}"),
            },
            other => panic!("expected unavailable state, got {other:?}"),
        }
    }

    #[test]
    fn bootstrap_system_capsules_reuses_existing_materialization_when_seed_is_unchanged() {
        let home = TempDir::new().expect("temp home should exist");
        let assets = TempDir::new().expect("temp assets should exist");
        let _guard = AtoHomeGuard::new(home.path());

        fs::create_dir_all(assets.path().join("system").join("ato-store").join("dist"))
            .expect("store dist should exist");
        fs::write(
            assets
                .path()
                .join("system")
                .join("ato-store")
                .join("dist")
                .join("index.html"),
            b"store",
        )
        .expect("store dist file should exist");

        let first = bootstrap_from_assets(assets.path()).expect("first bootstrap should succeed");
        let second = bootstrap_from_assets(assets.path()).expect("second bootstrap should succeed");

        assert_eq!(first.materialized.len(), 1);
        assert!(second.materialized.is_empty());
        assert_eq!(second.reused.len(), 1);
        assert!(second.degraded.is_empty());
    }

    #[test]
    fn bootstrap_system_capsules_degrades_one_capsule_without_blocking_others() {
        let home = TempDir::new().expect("temp home should exist");
        let assets = TempDir::new().expect("temp assets should exist");
        let _guard = AtoHomeGuard::new(home.path());

        fs::create_dir_all(assets.path().join("system").join("ato-dock").join("src"))
            .expect("dock src should exist");
        fs::write(
            assets
                .path()
                .join("system")
                .join("ato-dock")
                .join("package.json"),
            b"{}",
        )
        .expect("dock package should exist");
        fs::write(
            assets
                .path()
                .join("system")
                .join("ato-dock")
                .join("src")
                .join("main.jsx"),
            b"export default null;",
        )
        .expect("dock source should exist");
        fs::create_dir_all(assets.path().join("system").join("ato-store").join("dist"))
            .expect("store dist should exist");
        fs::write(
            assets
                .path()
                .join("system")
                .join("ato-store")
                .join("dist")
                .join("index.html"),
            b"store",
        )
        .expect("store dist should exist");

        let blocked_capsule_home = home
            .path()
            .join("apps")
            .join("ato-desktop")
            .join("system-capsules")
            .join("ato-dock");
        fs::create_dir_all(
            blocked_capsule_home
                .parent()
                .expect("blocked capsule should have parent"),
        )
        .expect("parent should exist");
        fs::write(&blocked_capsule_home, b"blocker")
            .expect("blocked capsule path should become a file");

        let report = bootstrap_from_assets(assets.path())
            .expect("bootstrap should complete even with degradation");

        assert_eq!(report.degraded.len(), 1);
        assert_eq!(report.degraded[0].capsule, "ato-dock");
        assert_eq!(report.materialized.len(), 1);
        assert_eq!(report.materialized[0].capsule, "ato-store");
        assert!(
            home.path()
                .join("apps")
                .join("ato-desktop")
                .join("system-capsules")
                .join("ato-dock.degraded.json")
                .is_file()
        );
    }
}
