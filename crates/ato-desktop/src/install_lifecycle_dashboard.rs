/// Read model for the installed-app lifecycle dashboard.
///
/// Provides DTOs and query functions that read from [`InstallInstanceStore`]
/// without mutating state. All errors use [`anyhow::Result`] so callers can
/// decide whether to surface them as user-visible errors or fall back to
/// empty-state views.
///
/// # Fail-closed contract
///
/// A corrupt `revision_log.json`, `app.json`, or `artifact_manifest.json`
/// for a single app returns `Err` for the whole query.  Callers should
/// handle `Err` by rendering an error card rather than silently hiding the
/// broken app.  This matches the GC fail-closed design established in
/// PR #231.
use anyhow::{Context, Result};
use capsule_core::common::paths::ato_path_or_workspace_tmp;
use capsule_core::foundation::install_lifecycle::{
    self, ids::InstallRevisionId, InstallInstanceStore, InstalledAppId, ProfileId,
};
use serde::Serialize;

// ── DTOs ───────────────────────────────────────────────────────────────────

/// Top-level dashboard item for one installed app.
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
    /// Sessions that carry an `install_revision_id` matching one of this
    /// app's revisions. Populated by the caller when session records are
    /// available; not filled by the pure store queries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub running_sessions_hint: Vec<InstalledAppSessionSummary>,
}

/// Summary of one launch profile within an installed app.
#[derive(Debug, Clone, Serialize)]
pub struct InstalledProfileDashboardItem {
    pub profile_id: String,
    /// Stable key `ipk_<32hex>`; unchanged across revisions.
    pub install_profile_key: String,
    /// `None` when the profile directory exists but no `current_revision`
    /// symlink has been set yet (install in progress).
    pub current_revision_id: Option<String>,
    pub revisions_count: usize,
    /// `finalized_at` of the newest revision in the profile log.
    pub latest_finalized_at: Option<String>,
    /// Filesystem path of the current revision's `output/` directory.
    pub current_output_dir: Option<String>,
}

/// One revision entry, typically shown in a revision list panel.
#[derive(Debug, Clone, Serialize)]
pub struct InstalledRevisionDashboardItem {
    pub revision_id: String,
    pub is_current: bool,
    pub is_pinned: bool,
    pub finalized_at: Option<String>,
    pub output_dir: String,
}

/// Lightweight summary of a running session that belongs to an installed app.
#[derive(Debug, Clone, Serialize)]
pub struct InstalledAppSessionSummary {
    pub session_id: String,
    pub execution_id: Option<String>,
    pub capsule_instance_key: Option<String>,
    pub install_revision_id: Option<String>,
    pub pid: Option<i32>,
    pub status: String,
}

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

    let current_rev = store.current_revision(&app_id, &prof_id).ok();
    let current_str = current_rev.as_ref().map(|r| r.as_str());

    let revision_log = store
        .list_profile_revisions(&app_id, &prof_id)
        .with_context(|| format!("read revision log for {}/{}", installed_app_id, profile_id))?;

    revision_log
        .iter()
        .map(|rev| {
            let is_current = Some(rev.as_str()) == current_str;
            let is_pinned = store.is_pinned(rev);
            let finalized_at = store
                .read_revision_manifest(rev)
                .ok()
                .flatten()
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

// ── Internal builders ───────────────────────────────────────────────────────

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
        if link.exists() {
            store
                .current_revision(app_id, profile_id)
                .ok()
                .map(|r| r.as_str().to_owned())
        } else {
            None
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

    let latest_finalized_at = revisions.iter().rev().find_map(|rev| {
        store
            .read_revision_manifest(rev)
            .ok()
            .flatten()
            .and_then(|v| {
                v.get("finalized_at")
                    .and_then(|s| s.as_str())
                    .map(String::from)
            })
    });

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
    })
}

// ── Launch helper ────────────────────────────────────────────────────────────

/// Launch an installed app by its profile key via `ato launch <ipk> -y`.
///
/// `ato_bin` should be the resolved ato CLI binary path (use
/// `orchestrator::resolve_ato_binary()`).
///
/// Returns the subprocess output stdout string if successful.
pub fn launch_installed_app(
    ato_bin: &std::path::Path,
    install_profile_key: &str,
) -> Result<String> {
    let output = std::process::Command::new(ato_bin)
        .arg("launch")
        .arg(install_profile_key)
        .arg("-y")
        .output()
        .with_context(|| format!("spawn ato launch '{install_profile_key}'"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("ato launch failed: {stderr}");
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

// ── Session attachment ──────────────────────────────────────────────────────

/// Attach running session summaries to each dashboard item by matching
/// `installed_app_id` from ato-session-core records.
///
/// Mutates items in-place. Corrupt session records are skipped with a
/// warn-level log (matching the Desktop fast-path behavior from
/// `read_session_records`).
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
        let rev_id = record.install_revision_id.clone();

        let alive = session_record_is_alive(&record);

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
                status: if alive {
                    "running".into()
                } else {
                    "unknown".into()
                },
            });
            break;
        }
    }
    Ok(())
}

/// Live-process check matching the GC logic in ato-cli/dispatch/gc.rs.
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

    /// Scaffold one app with one profile and N revisions, return app_id and ipk.
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

    // ── Tests ───────────────────────────────────────────────────────────────

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
        // Latest revision in log should be current (scaffold_one sets_current for each).
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
        // Don't scaffold any revision → no current_revision link.

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
        let store = open_store_at(&dir);

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

    fn open_store_at(dir: &tempfile::TempDir) -> InstallInstanceStore {
        InstallInstanceStore::new(&dir.path().join("instances")).unwrap()
    }
}
