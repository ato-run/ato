/// Read model for the installed-app lifecycle dashboard.
///
/// Provides DTOs and query functions that read from [`InstallInstanceStore`]
/// without mutating state. All errors use [`anyhow::Result`] so callers can
/// decide whether to surface them as user-visible errors or fall back to
/// empty-state views.
///
/// # Fail-closed contract
///
/// A corrupt `revision_log.json`, `artifact_manifest.json`, or unreadable
/// `current_revision` symlink returns `Err` for the whole query.
/// Callers should handle `Err` by rendering an error card rather than
/// silently hiding the broken app.  This matches the GC fail-closed design
/// established in PR #231.
///
/// # Render safety
///
/// All query functions perform filesystem I/O (store + session records).
/// Do NOT call them from a GPUI render path.  Use [`DashboardCache`] to
/// hold the pre-computed snapshot and call [`DashboardCache::refresh`]
/// from a background task or action handler.
use std::sync::Mutex;

use anyhow::{Context, Result};
use capsule_core::common::paths::ato_path_or_workspace_tmp;
use capsule_core::foundation::install_lifecycle::{
    self, ids::InstallRevisionId, InstallInstanceStore, InstalledAppId, ProfileId,
};
use serde::Serialize;

// ── UI Selection State ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum InstalledAppsActionStatus {
    Refreshing,
    Launching { install_profile_key: String },
    Success { message: String },
    Error { message: String },
}

impl InstalledAppsActionStatus {
    pub fn display_text(&self) -> String {
        match self {
            InstalledAppsActionStatus::Refreshing => "Refreshing installed apps...".to_string(),
            InstalledAppsActionStatus::Launching { install_profile_key } => {
                format!("Launching {install_profile_key}...")
            }
            InstalledAppsActionStatus::Success { message } => message.clone(),
            InstalledAppsActionStatus::Error { message } => message.clone(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct InstalledAppsUiState {
    pub selected_installed_app_id: Option<String>,
    pub selected_profile_id: Option<String>,
    pub detail_error: Option<String>,
}

impl InstalledAppsUiState {
    pub fn select_app(&mut self, installed_app_id: String) {
        self.selected_installed_app_id = Some(installed_app_id);
        self.selected_profile_id = Some("default".to_string());
        self.detail_error = None;
        DashboardCache::clear_action_status();
    }

    pub fn select_profile(&mut self, installed_app_id: &str, profile_id: &str) {
        let current = self.selected_installed_app_id.as_deref();
        if current.is_none() || current == Some(installed_app_id) {
            self.selected_installed_app_id = Some(installed_app_id.to_string());
            self.selected_profile_id = Some(profile_id.to_string());
            self.detail_error = None;
        }
    }
}

// ── DTOs ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct InstalledAppDashboardItem {
    pub installed_app_id: String,
    pub publisher: String,
    pub slug: String,
    pub capsule_handle: String,
    pub version: String,
    pub installed_at: String,
    pub updated_at: String,
    pub profiles: Vec<InstalledProfileDashboardItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub running_sessions_hint: Vec<InstalledAppSessionSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InstalledProfileDashboardItem {
    pub profile_id: String,
    pub install_profile_key: String,
    /// `None` only when the `current_revision` symlink does NOT exist on
    /// disk (profile dir created but install not yet completed).
    /// If the symlink exists but cannot be read this column returns `Err`
    /// from the parent query — the caller sees the error state, not a
    /// silently empty `current_revision_id`.
    pub current_revision_id: Option<String>,
    pub revisions_count: usize,
    pub latest_finalized_at: Option<String>,
    pub current_output_dir: Option<String>,
    pub revisions: Vec<InstalledRevisionDashboardItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InstalledRevisionDashboardItem {
    pub revision_id: String,
    pub is_current: bool,
    pub is_pinned: bool,
    pub finalized_at: Option<String>,
    pub output_dir: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct InstalledAppSessionSummary {
    pub session_id: String,
    pub execution_id: Option<String>,
    pub capsule_instance_key: Option<String>,
    pub install_revision_id: Option<String>,
    pub pid: Option<i32>,
    /// `"running"` when the process is confirmed alive; absent otherwise.
    pub status: String,
}

// ── Dashboard cache (Blocker 1 fix) ─────────────────────────────────────────

/// Thread-safe, lazily-refreshed snapshot of the installed-app list.
///
/// Use [`DashboardCache::get()`] from GPUI render paths (no I/O) and
/// [`DashboardCache::refresh()`] from background tasks or action handlers.
pub struct DashboardCache {
    items: Result<Vec<InstalledAppDashboardItem>, String>,
    action_status: Option<InstalledAppsActionStatus>,
}

impl DashboardCache {
    fn empty() -> Self {
        Self {
            items: Ok(Vec::new()),
            action_status: None,
        }
    }

    pub fn get() -> Result<Vec<InstalledAppDashboardItem>, String> {
        let guard = CACHE.lock().unwrap();
        match &guard.items {
            Ok(items) => Ok(items.clone()),
            Err(e) => Err(e.clone()),
        }
    }

    pub fn action_status() -> Option<InstalledAppsActionStatus> {
        CACHE.lock().unwrap().action_status.clone()
    }

    pub fn set_action_status(status: Option<InstalledAppsActionStatus>) {
        CACHE.lock().unwrap().action_status = status;
    }

    pub fn clear_action_status() {
        CACHE.lock().unwrap().action_status = None;
    }

    /// Re-read the store + session records and update the cache.
    /// Safe to call from any thread.  Returns `Ok(())` on success or
    /// `Err(message)` on failure; the cache is always updated either way.
    pub fn refresh() -> Result<(), String> {
        let result = (|| -> Result<_> {
            let mut items = list_installed_apps_dashboard().context("list installed apps")?;
            attach_running_sessions(&mut items).context("attach running sessions")?;
            Ok(items)
        })();

        let mut guard = CACHE.lock().unwrap();
        match result {
            Ok(items) => {
                guard.items = Ok(items);
                Ok(())
            }
            Err(e) => {
                let msg = format!("{:#}", e);
                guard.items = Err(msg.clone());
                Err(msg)
            }
        }
    }

    /// Reset the cache to empty.  Call between tests to avoid
    /// state leaking across `serial_test`-serialized runs.
    #[cfg(test)]
    pub fn reset_for_test() {
        let mut guard = CACHE.lock().unwrap();
        guard.items = Ok(Vec::new());
        guard.action_status = None;
    }
}

static CACHE: std::sync::LazyLock<Mutex<DashboardCache>> =
    std::sync::LazyLock::new(|| Mutex::new(DashboardCache::empty()));

// ── Store helpers ───────────────────────────────────────────────────────────

fn open_store() -> Result<InstallInstanceStore> {
    let root = ato_path_or_workspace_tmp("instances");
    InstallInstanceStore::new(&root)
        .with_context(|| format!("open instance store at {}", root.display()))
}

// ── Public query functions ──────────────────────────────────────────────────

pub fn list_installed_apps_dashboard() -> Result<Vec<InstalledAppDashboardItem>> {
    let store = open_store()?;
    let apps = store.list_installed_apps().context("list installed apps")?;
    let mut items = Vec::with_capacity(apps.len());
    for app_id in &apps {
        let item = build_app_item(&store, app_id)?;
        items.push(item);
    }
    Ok(items)
}

pub fn get_app_detail(installed_app_id: &str) -> Result<InstalledAppDashboardItem> {
    let store = open_store()?;
    let app_id = InstalledAppId::new(installed_app_id);
    build_app_item(&store, &app_id)
}

pub fn list_app_revisions(
    installed_app_id: &str,
    profile_id: &str,
) -> Result<Vec<InstalledRevisionDashboardItem>> {
    let store = open_store()?;
    let app_id = InstalledAppId::new(installed_app_id);
    let prof_id = ProfileId::new(profile_id);

    let current_rev = store.current_revision(&app_id, &prof_id).with_context(|| {
        format!(
            "read current revision for {}/{}",
            installed_app_id, profile_id
        )
    })?;
    let current_str = current_rev.as_str();

    let revision_log = store
        .list_profile_revisions(&app_id, &prof_id)
        .with_context(|| format!("read revision log for {}/{}", installed_app_id, profile_id))?;

    revision_log
        .iter()
        .map(|rev| {
            let is_current = rev.as_str() == current_str;
            let is_pinned = store.is_pinned(rev);
            let finalized_at = store
                .read_revision_manifest(rev)
                .with_context(|| format!("read manifest for revision {}", rev.as_str()))?
                .and_then(|v| {
                    v.get("finalized_at")
                        .and_then(|s| s.as_str())
                        .map(String::from)
                });
            let output_dir = store.revision_output_dir(rev).display().to_string();
            Ok(InstalledRevisionDashboardItem {
                revision_id: rev.as_str().to_owned(),
                is_current,
                is_pinned,
                finalized_at,
                output_dir,
            })
        })
        .collect()
}

// ── Launch (called from GPUI background executor — Blocker 2 fix) ───────────

/// Run `ato launch <ipk> -y` synchronously.  Intended to be called from a
/// GPUI background-executor spawn (not from the render thread or a raw
/// `std::thread::spawn`).
pub fn spawn_launch(ato_bin: &std::path::Path, install_profile_key: &str) -> Result<String> {
    let output = std::process::Command::new(ato_bin)
        .arg("launch")
        .arg(install_profile_key)
        .arg("-y")
        .output()
        .with_context(|| format!("spawn ato launch '{}'", install_profile_key))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("ato launch failed: {stderr}");
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

// ── Session attachment (Blocker 4 fix: only alive sessions) ─────────────────

pub fn attach_running_sessions(items: &mut [InstalledAppDashboardItem]) -> Result<()> {
    let session_root = match ato_session_core::store::session_root() {
        Ok(p) => p,
        Err(_) => return Ok(()),
    };
    if !session_root.exists() {
        return Ok(());
    }

    let entries = std::fs::read_dir(&session_root)
        .with_context(|| format!("read session root {}", session_root.display()))?;
    for entry in entries {
        let entry =
            entry.with_context(|| format!("iterate session root {}", session_root.display()))?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let raw = match std::fs::read_to_string(&path) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    "dashboard: skip unreadable session record {}: {e}",
                    path.display()
                );
                continue;
            }
        };
        let record: ato_session_core::record::StoredSessionInfo = match serde_json::from_str(&raw) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    "dashboard: skip corrupt session record {}: {e}",
                    path.display()
                );
                continue;
            }
        };
        let installed_app_id = match &record.installed_app_id {
            Some(id) if !id.is_empty() => id,
            _ => continue,
        };
        let alive = session_record_is_alive(&record);
        if !alive {
            continue;
        }
        let rev_id = record.install_revision_id.clone();

        for item in items.iter_mut() {
            if item.installed_app_id != *installed_app_id {
                continue;
            }
            item.running_sessions_hint.push(InstalledAppSessionSummary {
                session_id: record.session_id.clone(),
                execution_id: record.execution_id.clone(),
                capsule_instance_key: record.capsule_instance_key.clone(),
                install_revision_id: rev_id.clone(),
                pid: Some(record.pid),
                status: "running".into(),
            });
            break;
        }
    }
    Ok(())
}

fn session_record_is_alive(record: &ato_session_core::record::StoredSessionInfo) -> bool {
    #[cfg(unix)]
    {
        if let Some(pid) = nix_pid(record.pid) {
            if ato_session_core::process::pid_is_alive(pid) {
                return true;
            }
        }
        if let Some(svcs) = &record.orchestration_services {
            for svc in &svcs.services {
                if let Some(pid) = svc.local_pid.and_then(nix_pid) {
                    if ato_session_core::process::pid_is_alive(pid) {
                        return true;
                    }
                }
            }
        }
        false
    }
    #[cfg(not(unix))]
    {
        let _ = record;
        true
    }
}

#[cfg(unix)]
fn nix_pid(raw: i32) -> Option<u32> {
    if raw <= 0 {
        None
    } else {
        Some(raw as u32)
    }
}

// ── Internal builders (Blocker 3 fix: no silent .ok().flatten()) ────────────

/// Read `finalized_at` from a single revision's artifact manifest.
/// Returns `Ok(None)` when the manifest does not contain the field
/// (unfinalized revision).  Returns `Err` when the manifest file is
/// missing, unreadable, or not valid JSON.
fn revision_finalized_at(
    store: &InstallInstanceStore,
    rev: &InstallRevisionId,
) -> Result<Option<String>> {
    store
        .read_revision_manifest(rev)
        .with_context(|| format!("read manifest for revision {}", rev.as_str()))
        .map(|v| {
            v.and_then(|v| {
                v.get("finalized_at")
                    .and_then(|s| s.as_str())
                    .map(String::from)
            })
        })
}

fn build_app_item(
    store: &InstallInstanceStore,
    app_id: &InstalledAppId,
) -> Result<InstalledAppDashboardItem> {
    let record = store
        .read_app_record(app_id)
        .with_context(|| format!("read app record for {}", app_id.as_str()))?;

    let capsule_handle = if record.capsule_handle.is_empty() {
        format!("{}/{}", record.publisher, record.slug)
    } else {
        record.capsule_handle.clone()
    };

    let profiles = store
        .list_profiles(app_id)
        .with_context(|| format!("list profiles for {}", app_id.as_str()))?;

    let mut profile_items = Vec::with_capacity(profiles.len());
    for profile_id in &profiles {
        let item = build_profile_item(store, app_id, profile_id)?;
        profile_items.push(item);
    }

    Ok(InstalledAppDashboardItem {
        installed_app_id: record.installed_app_id.as_str().to_owned(),
        publisher: record.publisher,
        slug: record.slug,
        capsule_handle,
        version: record.version,
        installed_at: record.installed_at,
        updated_at: record.updated_at,
        profiles: profile_items,
        running_sessions_hint: Vec::new(),
    })
}

fn build_profile_item(
    store: &InstallInstanceStore,
    app_id: &InstalledAppId,
    profile_id: &ProfileId,
) -> Result<InstalledProfileDashboardItem> {
    let ipk = install_lifecycle::derive_install_profile_key(app_id, profile_id);

    let current_rev = {
        let link = store.current_revision_link(app_id, profile_id);
        match std::fs::symlink_metadata(&link) {
            Ok(_) => {
                let rev = store
                    .current_revision(app_id, profile_id)
                    .with_context(|| {
                        format!(
                            "read current_revision for {}/{}",
                            app_id.as_str(),
                            profile_id.as_str()
                        )
                    })?;
                Some(rev.as_str().to_owned())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("read current_revision link {}", link.display()));
            }
        }
    };

    let revisions = store
        .list_profile_revisions(app_id, profile_id)
        .with_context(|| {
            format!(
                "read revision log for {}/{}",
                app_id.as_str(),
                profile_id.as_str()
            )
        })?;
    let revisions_count = revisions.len();

    let current_rev_str = current_rev.as_deref().unwrap_or("");
    let mut latest_finalized_at: Option<String> = None;
    let revision_items: Result<Vec<InstalledRevisionDashboardItem>> = revisions
        .iter()
        .map(|rev| {
            let is_current = rev.as_str() == current_rev_str;
            let is_pinned = store.is_pinned(rev);
            let finalized_at = store
                .read_revision_manifest(rev)
                .with_context(|| format!("read manifest for revision {}", rev.as_str()))?
                .and_then(|v| {
                    v.get("finalized_at")
                        .and_then(|s| s.as_str())
                        .map(String::from)
                });
            if finalized_at.is_some() {
                latest_finalized_at = finalized_at.clone();
            }
            let output_dir = store.revision_output_dir(rev).display().to_string();
            Ok(InstalledRevisionDashboardItem {
                revision_id: rev.as_str().to_owned(),
                is_current,
                is_pinned,
                finalized_at,
                output_dir,
            })
        })
        .collect();

    let current_output_dir = current_rev.as_ref().map(|rev_id_str| {
        let rev = InstallRevisionId::new(rev_id_str.as_str());
        store.revision_output_dir(&rev).display().to_string()
    });

    Ok(InstalledProfileDashboardItem {
        profile_id: profile_id.as_str().to_owned(),
        install_profile_key: ipk.as_str().to_owned(),
        current_revision_id: current_rev,
        revisions_count,
        latest_finalized_at,
        current_output_dir,
        revisions: revision_items?,
    })
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use capsule_core::foundation::install_lifecycle::{
        ids::InstallRevisionId, AppRecord, LaunchProfile,
    };
    use serial_test::serial;

    fn make_store(dir: &tempfile::TempDir) -> InstallInstanceStore {
        InstallInstanceStore::new(&dir.path().join("instances")).unwrap()
    }

    fn make_app_record(
        app_id: &InstalledAppId,
        publisher: &str,
        slug: &str,
        version: &str,
    ) -> AppRecord {
        AppRecord {
            installed_app_id: app_id.clone(),
            publisher: publisher.into(),
            slug: slug.into(),
            capsule_handle: format!("{}/{}", publisher, slug),
            version: version.into(),
            installed_at: "2025-01-01T00:00:00Z".into(),
            updated_at: "2025-01-01T00:00:00Z".into(),
        }
    }

    fn make_default_profile(profile_id: &ProfileId) -> LaunchProfile {
        LaunchProfile {
            profile_id: profile_id.clone(),
            port_policy: "auto".into(),
            concurrency_policy: "single".into(),
            isolation: "default".into(),
            ..Default::default()
        }
    }

    fn scaffold_one(
        dir: &tempfile::TempDir,
        n_revs: usize,
    ) -> (
        InstalledAppId,
        ProfileId,
        install_lifecycle::InstallProfileKey,
    ) {
        let store = make_store(dir);
        let app_id = InstalledAppId::new("app_dash_test");
        let profile_id = ProfileId::new("default");
        store
            .write_app_record(&make_app_record(&app_id, "acme", "hello", "1.0.0"))
            .unwrap();
        store
            .write_profile(&app_id, &make_default_profile(&profile_id))
            .unwrap();
        for i in 0..n_revs {
            let rev =
                InstallRevisionId::new(&format!("rev_dash_{:016x}_aaaaaaaaaaaaaaaaaaaaaaaaaa", i));
            store.scaffold_revision(&rev).unwrap();
            store
                .set_current_revision(&app_id, &profile_id, &rev)
                .unwrap();
        }
        let ipk = install_lifecycle::derive_install_profile_key(&app_id, &profile_id);
        (app_id, profile_id, ipk)
    }

    #[test]
    #[serial]
    fn empty_store_returns_empty_vec() {
        let dir = tempfile::tempdir().unwrap();
        let _store = make_store(&dir);
        std::env::set_var("ATO_HOME", dir.path());
        let apps = list_installed_apps_dashboard().unwrap();
        assert!(apps.is_empty());
        std::env::remove_var("ATO_HOME");
    }

    #[test]
    #[serial]
    fn single_app_returns_correct_item() {
        let dir = tempfile::tempdir().unwrap();
        let (app_id, _profile_id, ipk) = scaffold_one(&dir, 2);
        std::env::set_var("ATO_HOME", dir.path());
        let apps = list_installed_apps_dashboard().unwrap();
        std::env::remove_var("ATO_HOME");

        assert_eq!(apps.len(), 1);
        let app = &apps[0];
        assert_eq!(app.installed_app_id, app_id.as_str());
        assert_eq!(app.publisher, "acme");
        assert_eq!(app.slug, "hello");
        assert_eq!(app.capsule_handle, "acme/hello");
        assert_eq!(app.version, "1.0.0");
        assert_eq!(app.profiles.len(), 1);

        let profile = &app.profiles[0];
        assert_eq!(profile.profile_id, "default");
        assert_eq!(profile.install_profile_key, ipk.as_str());
        assert!(profile.current_revision_id.is_some());
        assert_eq!(profile.revisions_count, 2);
    }

    #[test]
    #[serial]
    fn get_app_detail_returns_full_detail() {
        let dir = tempfile::tempdir().unwrap();
        let (app_id, _profile_id, _ipk) = scaffold_one(&dir, 3);
        std::env::set_var("ATO_HOME", dir.path());
        let detail = get_app_detail(app_id.as_str()).unwrap();
        std::env::remove_var("ATO_HOME");

        assert_eq!(detail.installed_app_id, app_id.as_str());
        assert_eq!(detail.profiles.len(), 1);
        assert_eq!(detail.profiles[0].revisions_count, 3);
    }

    #[test]
    #[serial]
    fn list_revisions_returns_correct_count_and_current_marker() {
        let dir = tempfile::tempdir().unwrap();
        let (app_id, profile_id, _ipk) = scaffold_one(&dir, 2);
        std::env::set_var("ATO_HOME", dir.path());
        let revisions = list_app_revisions(app_id.as_str(), profile_id.as_str()).unwrap();
        std::env::remove_var("ATO_HOME");

        assert_eq!(revisions.len(), 2);
        assert!(!revisions[0].is_current);
        assert!(revisions[1].is_current);
    }

    #[test]
    #[serial]
    fn current_revision_none_when_link_missing() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(&dir);
        let app_id = InstalledAppId::new("app_no_link");
        let profile_id = ProfileId::new("default");
        store
            .write_app_record(&make_app_record(&app_id, "acme", "nolink", "1.0.0"))
            .unwrap();
        store
            .write_profile(&app_id, &make_default_profile(&profile_id))
            .unwrap();

        std::env::set_var("ATO_HOME", dir.path());
        let apps = list_installed_apps_dashboard().unwrap();
        std::env::remove_var("ATO_HOME");

        assert_eq!(apps.len(), 1);
        let profile = &apps[0].profiles[0];
        assert!(profile.current_revision_id.is_none());
        assert_eq!(profile.revisions_count, 0);
    }

    #[test]
    #[serial]
    fn corrupt_revision_log_returns_err() {
        let dir = tempfile::tempdir().unwrap();
        let (app_id, profile_id, _ipk) = scaffold_one(&dir, 1);
        let store = InstallInstanceStore::new(&dir.path().join("instances")).unwrap();

        let log_path = store
            .profile_dir(&app_id, &profile_id)
            .join("revision_log.json");
        std::fs::write(&log_path, b"garbage {{{").unwrap();

        std::env::set_var("ATO_HOME", dir.path());
        let result = list_app_revisions(app_id.as_str(), profile_id.as_str());
        std::env::remove_var("ATO_HOME");
        assert!(result.is_err());
        let msg = format!("{:#}", result.unwrap_err());
        assert!(
            msg.contains("parse revision log"),
            "expected 'parse revision log': {msg}"
        );
    }

    #[test]
    #[serial]
    fn current_revision_read_failure_is_err() {
        let dir = tempfile::tempdir().unwrap();
        let (app_id, profile_id, _ipk) = scaffold_one(&dir, 1);
        let store = InstallInstanceStore::new(&dir.path().join("instances")).unwrap();

        #[cfg(unix)]
        {
            // Replace current_revision symlink → target="/".
            // `Path::new("/").file_name()` returns `None` on Unix,
            // which causes `current_revision()` to fail with
            // "extract revision id from symlink target".
            let link = store.current_revision_link(&app_id, &profile_id);
            std::fs::remove_file(&link).unwrap();
            std::os::unix::fs::symlink(std::path::Path::new("/"), &link).unwrap();
        }

        std::env::set_var("ATO_HOME", dir.path());
        let result = list_installed_apps_dashboard();
        std::env::remove_var("ATO_HOME");

        #[cfg(unix)]
        {
            assert!(
                result.is_err(),
                "current_revision symlink to / must produce Err, got Ok"
            );
            let msg = format!("{:#}", result.unwrap_err());
            assert!(
                msg.contains("revision id from symlink") || msg.contains("current_revision"),
                "expected 'revision id' or 'current_revision' context in error: {msg}"
            );
        }
        #[cfg(not(unix))]
        {
            assert!(result.is_ok());
        }
    }

    #[test]
    #[serial]
    fn cache_refresh_populates_items() {
        DashboardCache::reset_for_test();
        let dir = tempfile::tempdir().unwrap();
        let (_app_id, _profile_id, _ipk) = scaffold_one(&dir, 2);
        std::env::set_var("ATO_HOME", dir.path());

        let initial = DashboardCache::get().unwrap();
        assert!(initial.is_empty());

        let _ = DashboardCache::refresh();
        let after = DashboardCache::get().unwrap();
        assert_eq!(after.len(), 1);

        DashboardCache::reset_for_test();
        std::env::remove_var("ATO_HOME");
    }

    #[test]
    #[serial]
    fn running_badge_only_for_alive_sessions() {
        DashboardCache::reset_for_test();
        let dir = tempfile::tempdir().unwrap();
        let (_app_id, _profile_id, _ipk) = scaffold_one(&dir, 1);
        std::env::set_var("ATO_HOME", dir.path());

        let mut items = list_installed_apps_dashboard().unwrap();
        assert_eq!(items.len(), 1);
        assert!(items[0].running_sessions_hint.is_empty());

        let _ = attach_running_sessions(&mut items);

        DashboardCache::reset_for_test();
        std::env::remove_var("ATO_HOME");
    }

    #[test]
    fn installed_apps_ui_select_profile_when_app_id_is_none() {
        let mut ui = super::InstalledAppsUiState::default();
        assert!(ui.selected_installed_app_id.is_none());
        assert!(ui.selected_profile_id.is_none());

        ui.select_profile("app_aaa", "prod");

        assert_eq!(ui.selected_installed_app_id.as_deref(), Some("app_aaa"));
        assert_eq!(ui.selected_profile_id.as_deref(), Some("prod"));
    }

    #[test]
    fn installed_apps_ui_select_profile_ignores_mismatched_app() {
        let mut ui = super::InstalledAppsUiState::default();
        ui.select_app("app_bbb".into());
        assert_eq!(ui.selected_installed_app_id.as_deref(), Some("app_bbb"));
        assert_eq!(ui.selected_profile_id.as_deref(), Some("default"));

        ui.select_profile("app_aaa", "prod");

        assert_eq!(
            ui.selected_installed_app_id.as_deref(),
            Some("app_bbb"),
            "selected app must not change on mismatch"
        );
        assert_eq!(
            ui.selected_profile_id.as_deref(),
            Some("default"),
            "profile must not change on mismatch"
        );
    }

    #[test]
    fn installed_apps_ui_select_profile_matching_app_succeeds() {
        let mut ui = super::InstalledAppsUiState::default();
        ui.select_app("app_bbb".into());

        ui.select_profile("app_bbb", "prod");

        assert_eq!(ui.selected_installed_app_id.as_deref(), Some("app_bbb"));
        assert_eq!(ui.selected_profile_id.as_deref(), Some("prod"));
    }

    #[test]
    fn installed_apps_ui_select_app_clears_error() {
        let mut ui = super::InstalledAppsUiState {
            detail_error: Some("corrupt".into()),
            ..Default::default()
        };

        ui.select_app("app_aaa".into());

        assert!(ui.detail_error.is_none());
    }

    #[test]
    #[serial]
    fn dashboard_cache_refresh_returns_ok_on_success() {
        DashboardCache::reset_for_test();
        let dir = tempfile::tempdir().unwrap();
        let (_app_id, _profile_id, _ipk) = scaffold_one(&dir, 1);
        std::env::set_var("ATO_HOME", dir.path());

        let result = DashboardCache::refresh();
        assert!(result.is_ok());

        DashboardCache::reset_for_test();
        std::env::remove_var("ATO_HOME");
    }

    #[test]
    #[serial]
    fn dashboard_cache_refresh_returns_err_on_corrupt_store() {
        DashboardCache::reset_for_test();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("ATO_HOME", dir.path());

        // Create a file where instances/ should be, so list_installed_apps fails
        std::fs::write(dir.path().join("instances"), "not a directory").unwrap();

        let result = DashboardCache::refresh();
        assert!(result.is_err(), "expected Err, got Ok: {result:?}");

        DashboardCache::reset_for_test();
        std::env::remove_var("ATO_HOME");
    }

    #[test]
    #[serial]
    fn dashboard_cache_action_status_reflects_refresh_error() {
        DashboardCache::reset_for_test();
        DashboardCache::clear_action_status();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("ATO_HOME", dir.path());

        // Create a file where instances/ should be, so list_installed_apps fails
        std::fs::write(dir.path().join("instances"), "not a directory").unwrap();

        let _ = DashboardCache::refresh();
        let status = DashboardCache::action_status();
        assert!(status.is_none(), "action_status is not set by refresh alone");

        // Simulate what the button handler does
        DashboardCache::set_action_status(Some(super::InstalledAppsActionStatus::Refreshing));
        let result = DashboardCache::refresh();
        DashboardCache::set_action_status(Some(match result {
            Ok(()) => super::InstalledAppsActionStatus::Success {
                message: "ok".to_string(),
            },
            Err(e) => super::InstalledAppsActionStatus::Error {
                message: format!("Refresh failed: {e}"),
            },
        }));

        match DashboardCache::action_status() {
            Some(super::InstalledAppsActionStatus::Error { message }) => {
                assert!(
                    message.contains("Refresh failed"),
                    "expected error message, got: {message}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }

        DashboardCache::reset_for_test();
        std::env::remove_var("ATO_HOME");
    }

    #[test]
    #[serial]
    fn dashboard_cache_action_status_clear_by_select_app() {
        DashboardCache::reset_for_test();
        DashboardCache::clear_action_status();
        let dir = tempfile::tempdir().unwrap();
        let (_app_id, _profile_id, _ipk) = scaffold_one(&dir, 1);
        std::env::set_var("ATO_HOME", dir.path());

        // Set a pending status
        DashboardCache::set_action_status(Some(super::InstalledAppsActionStatus::Refreshing));
        assert!(DashboardCache::action_status().is_some());

        // select_app clears it via DashboardCache::clear_action_status()
        let mut ui = super::InstalledAppsUiState::default();
        ui.select_app("test-app".to_string());
        assert!(
            DashboardCache::action_status().is_none(),
            "select_app must clear DashboardCache action_status"
        );

        DashboardCache::reset_for_test();
        std::env::remove_var("ATO_HOME");
    }
}
