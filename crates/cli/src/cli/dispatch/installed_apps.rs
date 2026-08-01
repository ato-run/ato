use anyhow::{Context, Result};
use capsule::common::paths::ato_path_or_workspace_tmp;
use capsule::foundation::install_lifecycle::{
    InstallInstanceStore, InstalledAppId, ProfileId, derive_install_profile_key,
    ids::InstallRevisionId,
};
use protocol::desktop_library::{
    DESKTOP_LIBRARY_SCHEMA_VERSION, DesktopLibrarySnapshot, InstalledAppSummary,
    InstalledProfileSummary, InstalledRemoveResult, InstalledRevisionSummary,
    InstalledSessionSummary,
};

pub(super) fn execute_installed_command(
    command: crate::InstalledCommands,
    json_mode: bool,
) -> Result<()> {
    match command {
        crate::InstalledCommands::List { json } => output_list(json_mode || json),
        crate::InstalledCommands::Inspect {
            install_profile_key,
            json,
        } => output_inspect(&install_profile_key, json_mode || json),
        crate::InstalledCommands::Remove {
            install_profile_key,
            purge_state,
            json,
        } => output_remove(&install_profile_key, purge_state, json_mode || json),
    }
}

fn open_store() -> Result<InstallInstanceStore> {
    let root = ato_path_or_workspace_tmp("instances");
    InstallInstanceStore::new(&root)
        .with_context(|| format!("open install instance store at {}", root.display()))
}

fn output_list(json: bool) -> Result<()> {
    let snapshot = library_snapshot()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&snapshot)?);
    } else if snapshot.apps.is_empty() {
        println!("No installed apps.");
    } else {
        for app in &snapshot.apps {
            println!("{} ({})", app.capsule_handle, app.version);
            for profile in &app.profiles {
                println!(
                    "  {}  {}  {}",
                    profile.install_profile_key,
                    profile.profile_id,
                    profile.current_revision_id.as_deref().unwrap_or("degraded")
                );
            }
        }
    }
    Ok(())
}

fn output_inspect(install_profile_key: &str, json: bool) -> Result<()> {
    let store = open_store()?;
    let (app_id, _) = find_profile(&store, install_profile_key).with_context(|| {
        format!("install profile key '{install_profile_key}' not found. Run `ato install` first.")
    })?;
    let mut app = build_app_summary(&store, &app_id)?;
    attach_running_sessions(std::slice::from_mut(&mut app))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&app)?);
    } else {
        println!("{} ({})", app.capsule_handle, app.version);
        for profile in app.profiles {
            let marker = if profile.install_profile_key == install_profile_key {
                "*"
            } else {
                " "
            };
            println!(
                "{marker} {} ({})",
                profile.install_profile_key, profile.profile_id
            );
        }
    }
    Ok(())
}

fn output_remove(install_profile_key: &str, purge_state: bool, json: bool) -> Result<()> {
    let store = open_store()?;
    let (app_id, profile_id) = find_profile(&store, install_profile_key).with_context(|| {
        format!("install profile key '{install_profile_key}' not found. Run `ato install` first.")
    })?;
    if live_sessions_for_profile(install_profile_key)?
        .next()
        .is_some()
    {
        anyhow::bail!(
            "installed profile '{install_profile_key}' has a running session; stop it before remove"
        );
    }
    let profiles = store.list_profiles(&app_id)?;
    if purge_state && profiles.len() > 1 {
        anyhow::bail!(
            "cannot purge app state while other profiles remain; remove those profiles first"
        );
    }

    store.remove_profile_registration(&app_id, &profile_id)?;
    if purge_state {
        store.purge_app_state(&app_id)?;
        capsule::installed_state::InstalledStateDb::open_default()
            .context("open installed-state ledger")?
            .purge_install_profile(install_profile_key)
            .context("purge installed-state ledger")?;
    }

    let result = InstalledRemoveResult {
        schema_version: DESKTOP_LIBRARY_SCHEMA_VERSION.to_string(),
        installed_app_id: app_id.as_str().to_string(),
        profile_id: profile_id.as_str().to_string(),
        install_profile_key: install_profile_key.to_string(),
        state_purged: purge_state,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else if purge_state {
        println!("Removed {install_profile_key} and deleted app-owned state.");
    } else {
        println!("Removed {install_profile_key}; persistent state was preserved.");
    }
    Ok(())
}

fn library_snapshot() -> Result<DesktopLibrarySnapshot> {
    let store = open_store()?;
    let app_ids = store.list_installed_apps().context("list installed apps")?;
    let mut apps = app_ids
        .iter()
        .map(|app_id| build_app_summary(&store, app_id))
        .collect::<Result<Vec<_>>>()?;
    apps.sort_by(|a, b| a.capsule_handle.cmp(&b.capsule_handle));
    attach_running_sessions(&mut apps)?;
    Ok(DesktopLibrarySnapshot::new(apps))
}

fn build_app_summary(
    store: &InstallInstanceStore,
    app_id: &InstalledAppId,
) -> Result<InstalledAppSummary> {
    let record = store
        .read_app_record(app_id)
        .with_context(|| format!("read app record for {}", app_id.as_str()))?;
    let mut profiles = store
        .list_profiles(app_id)?
        .iter()
        .map(|profile_id| build_profile_summary(store, app_id, profile_id))
        .collect::<Result<Vec<_>>>()?;
    profiles.sort_by(|a, b| a.profile_id.cmp(&b.profile_id));
    let capsule_handle = if record.capsule_handle.is_empty() {
        format!("{}/{}", record.publisher, record.slug)
    } else {
        record.capsule_handle
    };
    Ok(InstalledAppSummary {
        installed_app_id: record.installed_app_id.as_str().to_string(),
        publisher: record.publisher,
        slug: record.slug,
        capsule_handle,
        version: record.version,
        installed_at: record.installed_at,
        updated_at: record.updated_at,
        profiles,
        running_sessions: Vec::new(),
    })
}

fn build_profile_summary(
    store: &InstallInstanceStore,
    app_id: &InstalledAppId,
    profile_id: &ProfileId,
) -> Result<InstalledProfileSummary> {
    let link = store.current_revision_link(app_id, profile_id);
    let current_revision_id = match std::fs::symlink_metadata(&link) {
        Ok(_) => Some(
            store
                .current_revision(app_id, profile_id)
                .with_context(|| {
                    format!(
                        "read current revision for {}/{}",
                        app_id.as_str(),
                        profile_id.as_str()
                    )
                })?
                .as_str()
                .to_string(),
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).with_context(|| format!("read {}", link.display())),
    };
    let revisions = store
        .list_profile_revisions(app_id, profile_id)
        .with_context(|| {
            format!(
                "read revision log for {}/{}",
                app_id.as_str(),
                profile_id.as_str()
            )
        })?
        .iter()
        .map(|revision| build_revision_summary(store, revision, current_revision_id.as_deref()))
        .collect::<Result<Vec<_>>>()?;
    let current_output_dir = current_revision_id.as_deref().map(|revision| {
        store
            .revision_output_dir(&InstallRevisionId::new(revision))
            .display()
            .to_string()
    });
    Ok(InstalledProfileSummary {
        profile_id: profile_id.as_str().to_string(),
        install_profile_key: derive_install_profile_key(app_id, profile_id)
            .as_str()
            .to_string(),
        current_revision_id,
        current_output_dir,
        revisions,
    })
}

fn build_revision_summary(
    store: &InstallInstanceStore,
    revision: &InstallRevisionId,
    current_revision: Option<&str>,
) -> Result<InstalledRevisionSummary> {
    let finalized_at = store
        .read_revision_manifest(revision)
        .with_context(|| format!("read manifest for revision {}", revision.as_str()))?
        .and_then(|value| {
            value
                .get("finalized_at")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    Ok(InstalledRevisionSummary {
        revision_id: revision.as_str().to_string(),
        is_current: current_revision == Some(revision.as_str()),
        is_pinned: store.is_pinned(revision),
        finalized_at,
        output_dir: store.revision_output_dir(revision).display().to_string(),
    })
}

fn find_profile(
    store: &InstallInstanceStore,
    install_profile_key: &str,
) -> Option<(InstalledAppId, ProfileId)> {
    for app_id in store.list_installed_apps().ok()? {
        for profile_id in store.list_profiles(&app_id).ok()? {
            if derive_install_profile_key(&app_id, &profile_id).as_str() == install_profile_key {
                return Some((app_id, profile_id));
            }
        }
    }
    None
}

fn attach_running_sessions(apps: &mut [InstalledAppSummary]) -> Result<()> {
    for session in stored_sessions()? {
        if !session_is_alive(&session) {
            continue;
        }
        let Some(installed_app_id) = session.installed_app_id.as_deref() else {
            continue;
        };
        if let Some(app) = apps
            .iter_mut()
            .find(|app| app.installed_app_id == installed_app_id)
        {
            app.running_sessions.push(session_summary(&session));
        }
    }
    Ok(())
}

fn live_sessions_for_profile(
    install_profile_key: &str,
) -> Result<impl Iterator<Item = capsule::state::session::record::StoredSessionInfo>> {
    let key = install_profile_key.to_string();
    Ok(stored_sessions()?.into_iter().filter(move |session| {
        session.install_profile_key.as_deref() == Some(key.as_str()) && session_is_alive(session)
    }))
}

fn stored_sessions() -> Result<Vec<capsule::state::session::record::StoredSessionInfo>> {
    let root = match capsule::state::session::store::session_root() {
        Ok(root) if root.exists() => root,
        _ => return Ok(Vec::new()),
    };
    let mut sessions = Vec::new();
    for entry in std::fs::read_dir(&root).with_context(|| format!("read {}", root.display()))? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let raw = match std::fs::read(&path) {
            Ok(raw) => raw,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "skip unreadable session record");
                continue;
            }
        };
        match serde_json::from_slice(&raw) {
            Ok(session) => sessions.push(session),
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "skip corrupt session record");
            }
        }
    }
    Ok(sessions)
}

fn session_summary(
    session: &capsule::state::session::record::StoredSessionInfo,
) -> InstalledSessionSummary {
    InstalledSessionSummary {
        session_id: session.session_id.clone(),
        execution_id: session.execution_id.clone(),
        capsule_instance_key: session.capsule_instance_key.clone(),
        install_profile_key: session.install_profile_key.clone(),
        install_revision_id: session.install_revision_id.clone(),
        pid: Some(session.pid),
        status: "running".to_string(),
    }
}

fn session_is_alive(session: &capsule::state::session::record::StoredSessionInfo) -> bool {
    #[cfg(unix)]
    {
        positive_pid(session.pid).is_some_and(capsule::state::session::process::pid_is_alive)
            || session
                .orchestration_services
                .as_ref()
                .is_some_and(|services| {
                    services.services.iter().any(|service| {
                        service
                            .local_pid
                            .and_then(positive_pid)
                            .is_some_and(capsule::state::session::process::pid_is_alive)
                    })
                })
    }
    #[cfg(not(unix))]
    {
        let _ = session;
        true
    }
}

#[cfg(unix)]
fn positive_pid(pid: i32) -> Option<u32> {
    u32::try_from(pid).ok().filter(|pid| *pid > 0)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use capsule::foundation::install_lifecycle::{AppRecord, LaunchProfile};
    use serial_test::serial;

    use super::*;

    fn seed_app(root: &Path) -> (InstallInstanceStore, InstalledAppId, ProfileId, String) {
        let store = InstallInstanceStore::new(root.join("instances")).unwrap();
        let app_id = InstalledAppId::new("app_desktop_library_test");
        let profile_id = ProfileId::default();
        store
            .write_app_record(&AppRecord {
                installed_app_id: app_id.clone(),
                publisher: "acme".into(),
                slug: "hello".into(),
                capsule_handle: "acme/hello".into(),
                version: "1.2.3".into(),
                installed_at: "2026-01-01T00:00:00Z".into(),
                updated_at: "2026-01-02T00:00:00Z".into(),
            })
            .unwrap();
        store
            .write_profile(
                &app_id,
                &LaunchProfile {
                    profile_id: profile_id.clone(),
                    ..Default::default()
                },
            )
            .unwrap();
        let revision = InstallRevisionId::new("rev_desktop_library_0000000000000001");
        store.scaffold_revision(&revision).unwrap();
        store
            .set_current_revision(&app_id, &profile_id, &revision)
            .unwrap();
        let ipk = derive_install_profile_key(&app_id, &profile_id)
            .as_str()
            .to_string();
        (store, app_id, profile_id, ipk)
    }

    #[test]
    #[serial]
    fn snapshot_uses_install_profile_key_as_stable_card_identity() {
        let dir = tempfile::tempdir().unwrap();
        let (_store, _app, _profile, ipk) = seed_app(dir.path());
        unsafe { std::env::set_var("ATO_HOME", dir.path()) };

        let snapshot = library_snapshot().unwrap();

        unsafe { std::env::remove_var("ATO_HOME") };
        assert_eq!(snapshot.schema_version, "1");
        assert_eq!(snapshot.apps.len(), 1);
        assert_eq!(snapshot.apps[0].profiles[0].install_profile_key, ipk);
        assert_eq!(
            snapshot.apps[0].profiles[0].current_revision_id.as_deref(),
            Some("rev_desktop_library_0000000000000001")
        );
    }

    #[test]
    #[serial]
    fn standard_remove_preserves_app_owned_state() {
        let dir = tempfile::tempdir().unwrap();
        let (store, app, _profile, ipk) = seed_app(dir.path());
        std::fs::write(store.state_dir(&app).join("user-data"), b"keep").unwrap();
        unsafe { std::env::set_var("ATO_HOME", dir.path()) };

        output_remove(&ipk, false, false).unwrap();

        unsafe { std::env::remove_var("ATO_HOME") };
        assert!(store.list_installed_apps().unwrap().is_empty());
        assert_eq!(
            std::fs::read(store.state_dir(&app).join("user-data")).unwrap(),
            b"keep"
        );
    }
}
