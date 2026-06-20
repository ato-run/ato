//! P3 acceptance: end-to-end install lifecycle integration tests.
//!
//! Covers:
//!   - Full lifecycle: install → update → rollback → gc (store-level)
//!   - GC protection rules: current, keep-last, retention, pin, active session
//!   - Error boundaries: corrupt revision_log, corrupt session, corrupt manifest
//!   - delete_revision safety, ipk stability, multiple profiles/apps
//!   - Rollback edge cases, pin/unpin cycle
//!
//! These tests exercise the store layer from `capsule` directly,
//! without depending on network or nacelle.

mod support;

use capsule::foundation::install_lifecycle::{
    self, AppRecord, InstallInstanceStore, LaunchProfile,
    ids::{InstallRevisionId, InstalledAppId, ProfileId},
};
use serial_test::serial;

fn make_store(dir: &tempfile::TempDir) -> InstallInstanceStore {
    InstallInstanceStore::new(dir.path().join("instances")).unwrap()
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

fn scaffold(
    dir: &tempfile::TempDir,
    n_revs: usize,
) -> (
    InstallInstanceStore,
    InstalledAppId,
    ProfileId,
    Vec<InstallRevisionId>,
    install_lifecycle::InstallProfileKey,
) {
    let store = make_store(dir);
    let app_id = InstalledAppId::new("app_acceptance");
    let profile_id = ProfileId::new("default");
    store
        .write_app_record(&make_app_record(&app_id, "acme", "hello", "1.0.0"))
        .unwrap();
    store
        .write_profile(&app_id, &make_default_profile(&profile_id))
        .unwrap();

    let mut revs = Vec::new();
    for i in 0..n_revs {
        let rev = InstallRevisionId::new(format!("rev_{:016x}_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", i));
        store.scaffold_revision(&rev).unwrap();
        store
            .set_current_revision(&app_id, &profile_id, &rev)
            .unwrap();
        revs.push(rev);
    }
    let ipk = install_lifecycle::derive_install_profile_key(&app_id, &profile_id);
    (store, app_id, profile_id, revs, ipk)
}

// ── Full lifecycle ─────────────────────────────────────────────────────────

#[test]
#[serial]
fn full_lifecycle() {
    let _env = support::IsolatedAto::new();
    let dir = tempfile::tempdir().unwrap();
    let (store, app_id, profile_id, revs, ipk) = scaffold(&dir, 4);

    let log = store.list_profile_revisions(&app_id, &profile_id).unwrap();
    assert_eq!(log.len(), 4);
    assert_eq!(log[0].as_str(), revs[0].as_str());
    assert_eq!(log[3].as_str(), revs[3].as_str());

    assert_eq!(
        store
            .current_revision(&app_id, &profile_id)
            .unwrap()
            .as_str(),
        revs[3].as_str()
    );

    store
        .set_current_revision(&app_id, &profile_id, &revs[1])
        .unwrap();
    assert_eq!(
        store
            .current_revision(&app_id, &profile_id)
            .unwrap()
            .as_str(),
        revs[1].as_str()
    );

    let mut protected = std::collections::HashSet::new();
    protected.insert(revs[1].as_str().to_owned());
    let reclaimable = store
        .collect_reclaimable_revisions(&protected, &store.list_all_revisions().unwrap(), 1, 0)
        .unwrap();
    let rec_ids: Vec<&str> = reclaimable.iter().map(|r| r.as_str()).collect();
    assert!(rec_ids.contains(&revs[0].as_str()), "rev0 reclaimable");
    assert!(rec_ids.contains(&revs[2].as_str()), "rev2 reclaimable");
    assert!(
        !rec_ids.contains(&revs[1].as_str()),
        "current rev1 protected"
    );
    assert!(
        !rec_ids.contains(&revs[3].as_str()),
        "keep-last rev3 protected"
    );

    for rev in &reclaimable {
        store.delete_revision(rev).unwrap();
    }
    let surviving = store.list_all_revisions().unwrap();
    let sids: Vec<&str> = surviving.iter().map(|r| r.as_str()).collect();
    assert!(sids.contains(&revs[1].as_str()));
    assert!(sids.contains(&revs[3].as_str()));
    assert!(!sids.contains(&revs[0].as_str()));
    assert!(!sids.contains(&revs[2].as_str()));

    assert_eq!(
        install_lifecycle::derive_install_profile_key(&app_id, &profile_id).as_str(),
        ipk.as_str()
    );
}

// ── GC protection rules ─────────────────────────────────────────────────────

#[test]
fn gc_current_always_protected() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _app_id, _profile_id, revs, _ipk) = scaffold(&dir, 3);
    let mut protected = std::collections::HashSet::new();
    protected.insert(revs[2].as_str().to_owned());
    let reclaimable = store
        .collect_reclaimable_revisions(&protected, &store.list_all_revisions().unwrap(), 0, 0)
        .unwrap();
    let rids: Vec<&str> = reclaimable.iter().map(|r| r.as_str()).collect();
    assert!(
        !rids.contains(&revs[2].as_str()),
        "current must not be reclaimable"
    );
    assert!(rids.contains(&revs[0].as_str()));
    assert!(rids.contains(&revs[1].as_str()));
}

#[test]
fn gc_keep_last_protection() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _app_id, _profile_id, revs, _ipk) = scaffold(&dir, 5);
    let mut protected = std::collections::HashSet::new();
    protected.insert(revs[4].as_str().to_owned());
    let reclaimable = store
        .collect_reclaimable_revisions(&protected, &store.list_all_revisions().unwrap(), 3, 0)
        .unwrap();
    let rids: Vec<&str> = reclaimable.iter().map(|r| r.as_str()).collect();
    assert!(rids.contains(&revs[0].as_str()));
    assert!(rids.contains(&revs[1].as_str()));
    assert!(
        !rids.contains(&revs[2].as_str()),
        "keep-last 3 protects rev2"
    );
    assert!(!rids.contains(&revs[3].as_str()));
    assert!(!rids.contains(&revs[4].as_str()), "current protected");
}

#[test]
fn gc_respects_pin() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _app_id, _profile_id, revs, _ipk) = scaffold(&dir, 4);
    store.pin_revision(&revs[0]).unwrap();
    let mut protected = std::collections::HashSet::new();
    protected.insert(revs[3].as_str().to_owned());
    let reclaimable = store
        .collect_reclaimable_revisions(&protected, &store.list_all_revisions().unwrap(), 2, 0)
        .unwrap();
    let rids: Vec<&str> = reclaimable.iter().map(|r| r.as_str()).collect();
    assert!(
        !rids.contains(&revs[0].as_str()),
        "pinned rev must be protected"
    );
}

#[test]
fn gc_respects_retention_window() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _app_id, _profile_id, revs, _ipk) = scaffold(&dir, 3);
    let manifest = serde_json::json!({
        "finalized_at": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "artifact_build_id": "build_0000",
    });
    std::fs::write(
        store.revision_artifact_manifest_path(&revs[0]),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let mut protected = std::collections::HashSet::new();
    protected.insert(revs[2].as_str().to_owned());
    let reclaimable = store
        .collect_reclaimable_revisions(&protected, &store.list_all_revisions().unwrap(), 0, 30)
        .unwrap();
    let rids: Vec<&str> = reclaimable.iter().map(|r| r.as_str()).collect();
    assert!(!rids.contains(&revs[0].as_str()), "retention protects rev0");
    assert!(
        rids.contains(&revs[1].as_str()),
        "no manifest → reclaimable"
    );
}

// ── Active session protection (Unix only) ───────────────────────────────────

#[test]
#[serial]
#[cfg(unix)]
fn gc_protects_active_session_revision() {
    use serde_json::json;

    let dir = tempfile::tempdir().unwrap();
    let (store, _app_id, _profile_id, revs, _ipk) = scaffold(&dir, 4);

    let session_root = dir.path().join("desktop_sessions");
    std::fs::create_dir_all(&session_root).unwrap();

    let record = json!({
        "session_id": "gc-accept-session",
        "handle": "publisher/slug",
        "normalized_handle": "publisher/slug",
        "canonical_handle": null,
        "trust_state": "untrusted",
        "source": null,
        "restricted": false,
        "snapshot": null,
        "runtime": {
            "target_label": "main",
            "runtime": null,
            "driver": null,
            "language": null,
            "port": null
        },
        "display_strategy": "guest_webview",
        "pid": std::process::id() as i32,
        "log_path": "/tmp/gc-accept.log",
        "manifest_path": "/tmp/gc-accept.toml",
        "target_label": "main",
        "notes": [],
        "guest": null,
        "web": null,
        "terminal": null,
        "service": null,
        "install_revision_id": revs[0].as_str(),
    });
    std::fs::write(
        session_root.join("gc-accept-session.json"),
        serde_json::to_vec_pretty(&record).unwrap(),
    )
    .unwrap();

    unsafe {
        std::env::set_var("ATO_HOME", dir.path());
    }
    unsafe {
        std::env::set_var("ATO_DESKTOP_SESSION_ROOT", &session_root);
    }

    let mut protected = std::collections::HashSet::new();
    // Collect current_revision of each profile.
    let apps = store.list_installed_apps().unwrap();
    for app in &apps {
        let profiles = store.list_profiles(app).unwrap();
        for profile in &profiles {
            if store.current_revision_link(app, profile).exists() {
                let rev = store.current_revision(app, profile).unwrap();
                protected.insert(rev.as_str().to_owned());
            }
        }
    }
    // Simulate active-session protection by reading the session record.
    let entries = std::fs::read_dir(&session_root).unwrap();
    for entry in entries {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let raw = std::fs::read_to_string(&path).unwrap();
        let record: capsule::state::session::record::StoredSessionInfo =
            serde_json::from_str(&raw).unwrap();
        if let Some(rev_id) = &record.install_revision_id
            && !rev_id.is_empty()
            && let Some(pid) = nix_pid_u32(record.pid)
            && capsule::state::session::process::pid_is_alive(pid)
        {
            protected.insert(rev_id.clone());
        }
    }

    let reclaimable = store
        .collect_reclaimable_revisions(&protected, &store.list_all_revisions().unwrap(), 2, 0)
        .unwrap();
    // revs[0] must survive because the active session references it.
    let rec_ids: Vec<&str> = reclaimable.iter().map(|r| r.as_str()).collect();
    assert!(
        !rec_ids.contains(&revs[0].as_str()),
        "session-referenced rev must be protected"
    );

    unsafe {
        std::env::remove_var("ATO_HOME");
    }
    unsafe {
        std::env::remove_var("ATO_DESKTOP_SESSION_ROOT");
    }
}

#[cfg(unix)]
fn nix_pid_u32(raw: i32) -> Option<u32> {
    if raw <= 0 { None } else { Some(raw as u32) }
}

// ── GC error boundaries ─────────────────────────────────────────────────────

#[test]
fn gc_errs_on_corrupt_revision_log() {
    let dir = tempfile::tempdir().unwrap();
    let (store, app_id, profile_id, _revs, _ipk) = scaffold(&dir, 3);
    let log_path = store
        .profile_dir(&app_id, &profile_id)
        .join("revision_log.json");
    std::fs::write(&log_path, b"garbage {{{").unwrap();

    let result = store.collect_reclaimable_revisions(
        &std::collections::HashSet::new(),
        &store.list_all_revisions().unwrap(),
        2,
        0,
    );
    assert!(result.is_err(), "corrupt log must error");
    let msg = format!("{:#}", result.unwrap_err());
    assert!(
        msg.contains("parse revision log"),
        "expected 'parse revision log': {msg}"
    );
}

#[test]
fn gc_errs_on_corrupt_artifact_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _app_id, _profile_id, revs, _ipk) = scaffold(&dir, 3);
    std::fs::write(
        store.revision_artifact_manifest_path(&revs[0]),
        b"garbage {{{",
    )
    .unwrap();

    let mut protected = std::collections::HashSet::new();
    protected.insert(revs[2].as_str().to_owned());
    let result = store.collect_reclaimable_revisions(
        &protected,
        &store.list_all_revisions().unwrap(),
        0,
        30,
    );
    assert!(result.is_err(), "corrupt manifest must error");
    let msg = format!("{:?}", result.unwrap_err());
    assert!(
        msg.contains("read manifest") || msg.contains("parse manifest"),
        "expected manifest context: {msg}"
    );
}

// ── delete_revision safety ──────────────────────────────────────────────────

#[test]
fn delete_revision_removes_revision() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _app_id, _profile_id, revs, _ipk) = scaffold(&dir, 3);
    store.delete_revision(&revs[0]).unwrap();
    let surviving = store.list_all_revisions().unwrap();
    let sids: Vec<&str> = surviving.iter().map(|r| r.as_str()).collect();
    assert!(!sids.contains(&revs[0].as_str()));
    assert!(sids.contains(&revs[1].as_str()));
    assert!(sids.contains(&revs[2].as_str()));
}

#[test]
fn delete_revision_refuses_current() {
    let dir = tempfile::tempdir().unwrap();
    let (store, app_id, profile_id, revs, _ipk) = scaffold(&dir, 3);
    let current = store.current_revision(&app_id, &profile_id).unwrap();
    assert_eq!(current.as_str(), revs[2].as_str());

    let result = store.delete_revision(&current);
    assert!(result.is_err());
    assert!(format!("{:#}", result.unwrap_err()).contains("still current"));
    assert!(
        store
            .list_all_revisions()
            .unwrap()
            .iter()
            .any(|r| r.as_str() == current.as_str())
    );
}

// ── Multiple profiles / apps ────────────────────────────────────────────────

#[test]
fn gc_protects_all_profile_currents() {
    let dir = tempfile::tempdir().unwrap();
    let store = make_store(&dir);
    let app_id = InstalledAppId::new("app_multi_profile");
    store
        .write_app_record(&make_app_record(&app_id, "acme", "multi", "1.0.0"))
        .unwrap();

    let p_default = ProfileId::new("default");
    let p_staging = ProfileId::new("staging");
    store
        .write_profile(&app_id, &make_default_profile(&p_default))
        .unwrap();
    store
        .write_profile(&app_id, &make_default_profile(&p_staging))
        .unwrap();

    let rd0 = InstallRevisionId::new("rev_d0_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let rd1 = InstallRevisionId::new("rev_d1_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let rs0 = InstallRevisionId::new("rev_s0_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    store.scaffold_revision(&rd0).unwrap();
    store
        .set_current_revision(&app_id, &p_default, &rd0)
        .unwrap();
    store.scaffold_revision(&rd1).unwrap();
    store
        .set_current_revision(&app_id, &p_default, &rd1)
        .unwrap();
    store.scaffold_revision(&rs0).unwrap();
    store
        .set_current_revision(&app_id, &p_staging, &rs0)
        .unwrap();

    let mut protected = std::collections::HashSet::new();
    protected.insert(rd1.as_str().to_owned());
    protected.insert(rs0.as_str().to_owned());
    let reclaimable = store
        .collect_reclaimable_revisions(&protected, &store.list_all_revisions().unwrap(), 0, 0)
        .unwrap();
    let rids: Vec<&str> = reclaimable.iter().map(|r| r.as_str()).collect();
    assert!(rids.contains(&rd0.as_str()));
    assert!(!rids.contains(&rd1.as_str()));
    assert!(!rids.contains(&rs0.as_str()));
}

#[test]
fn gc_keep_last_scoped_per_profile() {
    let dir = tempfile::tempdir().unwrap();
    let store = make_store(&dir);

    let app_a = InstalledAppId::new("app_kl_a");
    let app_b = InstalledAppId::new("app_kl_b");
    let profile = ProfileId::new("default");
    store
        .write_app_record(&make_app_record(&app_a, "acme", "a", "1.0.0"))
        .unwrap();
    store
        .write_app_record(&make_app_record(&app_b, "acme", "b", "1.0.0"))
        .unwrap();
    store
        .write_profile(&app_a, &make_default_profile(&profile))
        .unwrap();
    store
        .write_profile(&app_b, &make_default_profile(&profile))
        .unwrap();

    let mut revs_a = Vec::new();
    for i in 0..5 {
        let rev = InstallRevisionId::new(format!("rev_a{0:01x}_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", i));
        store.scaffold_revision(&rev).unwrap();
        store.set_current_revision(&app_a, &profile, &rev).unwrap();
        revs_a.push(rev);
    }
    let rev_b0 = InstallRevisionId::new("rev_b0_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    store.scaffold_revision(&rev_b0).unwrap();
    store
        .set_current_revision(&app_b, &profile, &rev_b0)
        .unwrap();

    let mut protected = std::collections::HashSet::new();
    protected.insert(revs_a[4].as_str().to_owned());
    protected.insert(rev_b0.as_str().to_owned());
    let reclaimable = store
        .collect_reclaimable_revisions(&protected, &store.list_all_revisions().unwrap(), 2, 0)
        .unwrap();
    let rids: Vec<&str> = reclaimable.iter().map(|r| r.as_str()).collect();
    assert!(rids.contains(&revs_a[0].as_str()));
    assert!(rids.contains(&revs_a[1].as_str()));
    assert!(rids.contains(&revs_a[2].as_str()));
    assert!(!rids.contains(&revs_a[3].as_str()));
    assert!(!rids.contains(&revs_a[4].as_str()));
    assert!(!rids.contains(&rev_b0.as_str()));
}

// ── Rollback edge cases ─────────────────────────────────────────────────────

#[test]
fn rollback_auto_picks_predecessor() {
    let dir = tempfile::tempdir().unwrap();
    let (store, app_id, profile_id, revs, _ipk) = scaffold(&dir, 3);
    let log = store.list_profile_revisions(&app_id, &profile_id).unwrap();
    let current = store.current_revision(&app_id, &profile_id).unwrap();
    let pos = log
        .iter()
        .position(|r| r.as_str() == current.as_str())
        .unwrap();
    let prev = &log[pos - 1];
    store
        .set_current_revision(&app_id, &profile_id, prev)
        .unwrap();
    assert_eq!(
        store
            .current_revision(&app_id, &profile_id)
            .unwrap()
            .as_str(),
        revs[1].as_str()
    );
}

#[test]
fn rollback_noop_same_revision() {
    let dir = tempfile::tempdir().unwrap();
    let (store, app_id, profile_id, revs, _ipk) = scaffold(&dir, 3);
    store
        .set_current_revision(&app_id, &profile_id, &revs[2])
        .unwrap();
    assert_eq!(
        store
            .current_revision(&app_id, &profile_id)
            .unwrap()
            .as_str(),
        revs[2].as_str()
    );
    assert_eq!(
        store
            .list_profile_revisions(&app_id, &profile_id)
            .unwrap()
            .len(),
        3
    );
}

// ── Pin / unpin ─────────────────────────────────────────────────────────────

#[test]
fn pin_unpin_cycle() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _app_id, _profile_id, revs, _ipk) = scaffold(&dir, 3);
    assert!(!store.is_pinned(&revs[0]));
    store.pin_revision(&revs[0]).unwrap();
    assert!(store.is_pinned(&revs[0]));
    store.unpin_revision(&revs[0]).unwrap();
    assert!(!store.is_pinned(&revs[0]));
}

// ── IPK stability ───────────────────────────────────────────────────────────

#[test]
fn ipk_stable_across_recreates() {
    let app_id = InstalledAppId::new("app_ipk_stable");
    let profile_id = ProfileId::new("default");
    let ipk1 = install_lifecycle::derive_install_profile_key(&app_id, &profile_id);
    let ipk2 = install_lifecycle::derive_install_profile_key(&app_id, &profile_id);
    assert_eq!(ipk1.as_str(), ipk2.as_str());

    let app_id2 = InstalledAppId::new("app_ipk_stable_other");
    let ipk3 = install_lifecycle::derive_install_profile_key(&app_id2, &profile_id);
    assert_ne!(ipk1.as_str(), ipk3.as_str());

    let profile_id2 = ProfileId::new("staging");
    let ipk4 = install_lifecycle::derive_install_profile_key(&app_id, &profile_id2);
    assert_ne!(ipk1.as_str(), ipk4.as_str());
}

#[test]
fn ipk_stable_across_revisions() {
    let dir = tempfile::tempdir().unwrap();
    let (store, app_id, profile_id, _revs, _ipk) = scaffold(&dir, 3);
    let ipk1 = install_lifecycle::derive_install_profile_key(&app_id, &profile_id);
    let new_rev = InstallRevisionId::new("rev_extra_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    store.scaffold_revision(&new_rev).unwrap();
    store
        .set_current_revision(&app_id, &profile_id, &new_rev)
        .unwrap();
    let ipk2 = install_lifecycle::derive_install_profile_key(&app_id, &profile_id);
    assert_eq!(ipk1.as_str(), ipk2.as_str());
}

// ── GC: nothing to collect ──────────────────────────────────────────────────

#[test]
fn gc_nothing_to_collect_single_rev() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _app_id, _profile_id, revs, _ipk) = scaffold(&dir, 1);
    let mut protected = std::collections::HashSet::new();
    protected.insert(revs[0].as_str().to_owned());
    let reclaimable = store
        .collect_reclaimable_revisions(&protected, &store.list_all_revisions().unwrap(), 2, 14)
        .unwrap();
    assert!(reclaimable.is_empty());
}

#[test]
fn gc_does_not_delete_protected() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _app_id, _profile_id, revs, _ipk) = scaffold(&dir, 3);
    let mut protected = std::collections::HashSet::new();
    protected.insert(revs[2].as_str().to_owned());
    protected.insert(revs[1].as_str().to_owned());
    let reclaimable = store
        .collect_reclaimable_revisions(&protected, &store.list_all_revisions().unwrap(), 2, 0)
        .unwrap();
    for rev in &reclaimable {
        assert_ne!(rev.as_str(), revs[1].as_str());
        assert_ne!(rev.as_str(), revs[2].as_str());
    }
}

// ── Current revision not in log is still protected in GC ────────────────────

#[test]
fn gc_protects_current_even_when_not_in_log() {
    let dir = tempfile::tempdir().unwrap();
    let (store, app_id, profile_id, _revs, _ipk) = scaffold(&dir, 3);
    let ghost = InstallRevisionId::new("rev_ghost_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    store.scaffold_revision(&ghost).unwrap();
    #[cfg(unix)]
    {
        let link = store.current_revision_link(&app_id, &profile_id);
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(store.revision_dir(&ghost), &link).unwrap();
    }

    let mut protected = std::collections::HashSet::new();
    protected.insert(ghost.as_str().to_owned());
    let reclaimable = store
        .collect_reclaimable_revisions(&protected, &store.list_all_revisions().unwrap(), 0, 0)
        .unwrap();
    let rids: Vec<&str> = reclaimable.iter().map(|r| r.as_str()).collect();
    assert!(
        !rids.contains(&ghost.as_str()),
        "current (ghost) must be protected"
    );
}
