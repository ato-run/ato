//! Desktop launch-intent resolution.
//!
//! The Desktop's normal "open an app" gesture is not a single low-level
//! operation. Depending on what is already known about the target it resolves
//! to one of a few **intents**, and only some of them should ever show the
//! first-run consent wizard:
//!
//! ```text
//! 1. Focus an existing live session            (no launch at all)
//! 2. Launch an installed profile by ipk         (pre-consented; no wizard)
//! 3. Install, then launch                        (first-run; consent applies)
//! 4. Legacy / try open by handle                 (consent applies)
//! ```
//!
//! Keeping this decision in one pure function makes it unit-testable without a
//! running GPUI app and prevents the historical bug where *every* not-running
//! open fell through to `ato run`-style handle launch + consent wizard, so an
//! already-installed app re-showed review on every relaunch.
//!
//! The durable identity of an installed entry is its
//! [`install_profile_key`](capsule::foundation::install_lifecycle::derive_install_profile_key)
//! and the derived stable
//! [`app_url`](capsule::foundation::install_lifecycle::derive_app_url) —
//! never the capsule `handle`, a `.capsule` path, a revision id, or a session's
//! ephemeral `local_url`.

use anyhow::Result;
use capsule::foundation::install_lifecycle::{
    AppRecord, InstallInstanceStore, InstalledAppId, ProfileId, derive_app_url,
    derive_install_profile_key,
};

/// The result of resolving a Desktop "open this app" gesture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopLaunchIntent {
    /// A compatible session is already live — focus it instead of launching.
    FocusSession { session_id: String },
    /// Launch an installed app through its stable profile key. The Desktop
    /// opens `app_url` as the durable identity; the actual session is ensured
    /// via `ato launch <install_profile_key>` (pre-consented, pinned revision).
    /// No consent wizard.
    LaunchInstalledProfile {
        install_profile_key: String,
        app_url: String,
        /// Retained for display / session-record fast-path adapter only.
        handle: String,
    },
    /// A normal open of something that is not installed yet. In a later PR this
    /// drives an install-then-launch flow; for now the caller routes it through
    /// the existing consent/launch path.
    InstallThenLaunch { handle: String },
    /// Explicit legacy / try open by handle (e.g. ambiguous installed match, or
    /// a dev/private run). Routed through the existing consent/launch path.
    LegacyTryOpen { handle: String },
}

/// Outcome of looking a handle up against the installed-app store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstalledMatch {
    /// The handle resolves to exactly one launchable installed profile.
    One {
        install_profile_key: String,
        app_url: String,
    },
    /// No installed app matches this handle.
    None,
    /// More than one installed app matches — do not guess.
    Many,
}

/// Pure inputs for [`resolve_launch_intent`]. The caller performs the
/// (impure) live-session, history, and store lookups and passes the results
/// here so the decision itself stays deterministic and testable.
pub struct IntentInputs {
    /// The capsule handle the user clicked.
    pub handle: String,
    /// `Some(session_id)` if a compatible live session already exists.
    pub live_session_id: Option<String>,
    /// `(install_profile_key, app_url)` recorded in Start history, if the
    /// entry already carries install identity.
    pub history_install: Option<(String, String)>,
    /// Result of matching the handle against the installed-app store.
    pub installed_match: InstalledMatch,
}

/// Resolve a launch gesture to a [`DesktopLaunchIntent`].
///
/// Decision order (first match wins):
/// 1. a live session → [`DesktopLaunchIntent::FocusSession`]
/// 2. Start history already carries install identity → installed launch
/// 3. the handle resolves to exactly one installed profile → installed launch
///    (recovering identity for legacy handle-only history entries)
/// 4. the handle is ambiguous across installed apps → legacy try-open
/// 5. otherwise → install-then-launch
pub fn resolve_launch_intent(inputs: IntentInputs) -> DesktopLaunchIntent {
    let IntentInputs {
        handle,
        live_session_id,
        history_install,
        installed_match,
    } = inputs;

    if let Some(session_id) = live_session_id {
        return DesktopLaunchIntent::FocusSession { session_id };
    }

    if let Some((install_profile_key, app_url)) = history_install {
        return DesktopLaunchIntent::LaunchInstalledProfile {
            install_profile_key,
            app_url,
            handle,
        };
    }

    match installed_match {
        InstalledMatch::One {
            install_profile_key,
            app_url,
        } => DesktopLaunchIntent::LaunchInstalledProfile {
            install_profile_key,
            app_url,
            handle,
        },
        // Ambiguous: never silently pick one installed app for a shared handle.
        InstalledMatch::Many => DesktopLaunchIntent::LegacyTryOpen { handle },
        // Not installed: a normal open that has to install first.
        InstalledMatch::None => DesktopLaunchIntent::InstallThenLaunch { handle },
    }
}

/// Whether an `ato://run` request for a handle should skip the consent
/// wizard because the user already reviewed and installed this exact capsule
/// (PR-D1). Mirrors the bar [`resolve_launch_intent`]'s
/// `LaunchInstalledProfile` arm already uses for handle re-opens: a single,
/// non-degraded installed match is pre-consented. Pure over
/// [`InstalledMatch`] — the caller performs the (impure) install-store
/// lookup via [`installed_match_for_handle`] and passes the result here so
/// the decision itself stays deterministic and testable without a real
/// install store.
///
/// Deliberately narrower than [`resolve_launch_intent`]: `ato://run` targets
/// the Desktop Runner cold-OCI substrate, which has no live-session /
/// AppWindow concept to fast-path through, so only the installed-vs-not
/// question applies here.
pub fn run_intent_is_pre_consented(installed_match: &InstalledMatch) -> bool {
    matches!(installed_match, InstalledMatch::One { .. })
}

/// Look a handle up against the installed-app store and classify the match.
///
/// A handle is considered launchable only when its single matching app has a
/// `default` profile with a resolvable `current_revision` — a half-registered
/// (degraded) install is treated as [`InstalledMatch::None`] so the caller
/// does not present it as a one-click relaunch.
pub fn installed_match_for_handle(
    store: &InstallInstanceStore,
    handle: &str,
) -> Result<InstalledMatch> {
    let apps = store.list_installed_apps()?;
    let mut matched: Vec<InstalledAppId> = Vec::new();
    for app_id in &apps {
        let Ok(record) = store.read_app_record(app_id) else {
            continue;
        };
        if handle_matches_record(handle, &record) {
            matched.push(app_id.clone());
        }
    }

    match matched.as_slice() {
        [] => Ok(InstalledMatch::None),
        [app_id] => {
            // Only the `default` profile is launchable in this PR; non-default
            // profiles are out of scope (see #261 slice 2).
            let profile_id = ProfileId::new("default");
            // Require a resolvable current revision — otherwise the install is
            // degraded / incomplete and must not present as launchable.
            if store.current_revision(app_id, &profile_id).is_err() {
                return Ok(InstalledMatch::None);
            }
            let ipk = derive_install_profile_key(app_id, &profile_id);
            let app_url = derive_app_url(&ipk);
            Ok(InstalledMatch::One {
                install_profile_key: ipk.as_str().to_string(),
                app_url,
            })
        }
        _ => Ok(InstalledMatch::Many),
    }
}

/// A launchable installed target resolved by reverse-looking the install store
/// up by `install_profile_key` — the durable identity behind `ato://app/<ipk>`.
///
/// The `handle` is carried only for route construction / display; it is never
/// the launch identity. Two installs that share a handle are disambiguated by
/// their distinct keys, so resolving by key (unlike resolving by handle) is
/// never ambiguous.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledLaunchTarget {
    pub install_profile_key: String,
    pub handle: String,
    pub app_url: String,
}

/// Reverse-resolve an `install_profile_key` to a launchable
/// [`InstalledLaunchTarget`] against an already-open store. Only the `default`
/// profile is considered (#261 slice 2). Returns `Ok(None)` when no installed
/// app derives the requested key, or the single match is degraded (no
/// resolvable current revision) — the same launchability bar as
/// [`installed_match_for_handle`].
pub fn installed_target_for_ipk(
    store: &InstallInstanceStore,
    install_profile_key: &str,
) -> Result<Option<InstalledLaunchTarget>> {
    let profile_id = ProfileId::new("default");
    for app_id in store.list_installed_apps()? {
        let derived = derive_install_profile_key(&app_id, &profile_id);
        if derived.as_str() != install_profile_key {
            continue;
        }
        // Degraded / incomplete install must not present as launchable.
        if store.current_revision(&app_id, &profile_id).is_err() {
            return Ok(None);
        }
        let record = store.read_app_record(&app_id)?;
        let handle = canonical_handle(&record);
        if handle.is_empty() {
            return Ok(None);
        }
        return Ok(Some(InstalledLaunchTarget {
            install_profile_key: derived.as_str().to_string(),
            handle,
            app_url: derive_app_url(&derived),
        }));
    }
    Ok(None)
}

/// Open the install store and resolve an `ato://app/<ipk>` URL (or a bare ipk)
/// to a launchable target. Thin filesystem wrapper around
/// [`installed_target_for_ipk`] for callers (e.g. the `NavigateToUrl` router)
/// that only hold the URL.
pub fn installed_target_for_app_url(app_url: &str) -> Result<Option<InstalledLaunchTarget>> {
    let ipk = app_url
        .strip_prefix("ato://app/")
        .unwrap_or(app_url)
        .trim_end_matches('/')
        .trim();
    if ipk.is_empty() {
        return Ok(None);
    }
    let root = capsule::common::paths::ato_path_or_workspace_tmp("instances");
    let store = InstallInstanceStore::new(&root)?;
    installed_target_for_ipk(&store, ipk)
}

/// Canonical capsule handle for an installed record: prefer the stored
/// `capsule_handle`, fall back to `publisher/slug`. Mirrors the comparison
/// targets of [`handle_matches_record`].
fn canonical_handle(record: &AppRecord) -> String {
    if !record.capsule_handle.is_empty() {
        record.capsule_handle.clone()
    } else if !record.publisher.is_empty() {
        format!("{}/{}", record.publisher, record.slug)
    } else {
        String::new()
    }
}

/// Whether a clicked handle refers to the same capsule as an installed app
/// record. Compares against the record's `capsule_handle` and, as a fallback,
/// its `publisher/slug`, under a light normalization.
fn handle_matches_record(handle: &str, record: &AppRecord) -> bool {
    let target = normalize_handle(handle);
    if target.is_empty() {
        return false;
    }
    if !record.capsule_handle.is_empty() && normalize_handle(&record.capsule_handle) == target {
        return true;
    }
    if !record.publisher.is_empty() {
        let publisher_slug = format!("{}/{}", record.publisher, record.slug);
        if normalize_handle(&publisher_slug) == target {
            return true;
        }
    }
    false
}

fn normalize_handle(handle: &str) -> String {
    handle
        .trim()
        .trim_start_matches("capsule://")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule::foundation::install_lifecycle::{InstallRevisionId, LaunchProfile};

    fn inputs(handle: &str) -> IntentInputs {
        IntentInputs {
            handle: handle.to_string(),
            live_session_id: None,
            history_install: None,
            installed_match: InstalledMatch::None,
        }
    }

    #[test]
    fn live_session_focuses_even_when_installed() {
        let i = IntentInputs {
            live_session_id: Some("sess-1".into()),
            history_install: Some(("ipk_abc".into(), "ato://app/ipk_abc".into())),
            ..inputs("acme/hello")
        };
        assert_eq!(
            resolve_launch_intent(i),
            DesktopLaunchIntent::FocusSession {
                session_id: "sess-1".into()
            }
        );
    }

    #[test]
    fn history_install_chooses_installed_launch() {
        let i = IntentInputs {
            history_install: Some(("ipk_abc".into(), "ato://app/ipk_abc".into())),
            ..inputs("acme/hello")
        };
        assert_eq!(
            resolve_launch_intent(i),
            DesktopLaunchIntent::LaunchInstalledProfile {
                install_profile_key: "ipk_abc".into(),
                app_url: "ato://app/ipk_abc".into(),
                handle: "acme/hello".into(),
            }
        );
    }

    #[test]
    fn legacy_handle_recovers_installed_profile() {
        let i = IntentInputs {
            installed_match: InstalledMatch::One {
                install_profile_key: "ipk_xyz".into(),
                app_url: "ato://app/ipk_xyz".into(),
            },
            ..inputs("acme/hello")
        };
        assert_eq!(
            resolve_launch_intent(i),
            DesktopLaunchIntent::LaunchInstalledProfile {
                install_profile_key: "ipk_xyz".into(),
                app_url: "ato://app/ipk_xyz".into(),
                handle: "acme/hello".into(),
            }
        );
    }

    #[test]
    fn unknown_handle_is_install_then_launch() {
        assert_eq!(
            resolve_launch_intent(inputs("github.com/owner/repo")),
            DesktopLaunchIntent::InstallThenLaunch {
                handle: "github.com/owner/repo".into()
            }
        );
    }

    #[test]
    fn ambiguous_handle_falls_back_to_legacy() {
        let i = IntentInputs {
            installed_match: InstalledMatch::Many,
            ..inputs("acme/hello")
        };
        assert_eq!(
            resolve_launch_intent(i),
            DesktopLaunchIntent::LegacyTryOpen {
                handle: "acme/hello".into()
            }
        );
    }

    // ── run_intent_is_pre_consented (PR-D1) ─────────────────────────────────

    #[test]
    fn run_intent_pre_consented_only_for_a_single_installed_match() {
        assert!(run_intent_is_pre_consented(&InstalledMatch::One {
            install_profile_key: "ipk_abc".into(),
            app_url: "ato://app/ipk_abc".into(),
        }));
        assert!(!run_intent_is_pre_consented(&InstalledMatch::None));
        assert!(!run_intent_is_pre_consented(&InstalledMatch::Many));
    }

    // ── installed_match_for_handle (store-backed) ───────────────────────────

    fn store_in(dir: &tempfile::TempDir) -> InstallInstanceStore {
        InstallInstanceStore::new(dir.path().join("instances")).unwrap()
    }

    fn install_app(
        store: &InstallInstanceStore,
        app_id: &str,
        publisher: &str,
        slug: &str,
        with_current_revision: bool,
    ) -> InstalledAppId {
        let app_id = InstalledAppId::new(app_id);
        let profile_id = ProfileId::new("default");
        store
            .write_app_record(&AppRecord {
                installed_app_id: app_id.clone(),
                publisher: publisher.into(),
                slug: slug.into(),
                capsule_handle: format!("{publisher}/{slug}"),
                version: "1.0.0".into(),
                installed_at: "2025-01-01T00:00:00Z".into(),
                updated_at: "2025-01-01T00:00:00Z".into(),
            })
            .unwrap();
        store
            .write_profile(
                &app_id,
                &LaunchProfile {
                    profile_id: profile_id.clone(),
                    port_policy: "auto".into(),
                    concurrency_policy: "single".into(),
                    isolation: "default".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        if with_current_revision {
            let rev = InstallRevisionId::new("rev_match_test_000000000000000000");
            store.scaffold_revision(&rev).unwrap();
            store
                .set_current_revision(&app_id, &profile_id, &rev)
                .unwrap();
        }
        app_id
    }

    #[test]
    fn match_one_installed_app() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(&dir);
        let app_id = install_app(
            &store,
            "app_one0000000000000000000000000",
            "acme",
            "hello",
            true,
        );

        let m = installed_match_for_handle(&store, "acme/hello").unwrap();
        let expected_ipk = derive_install_profile_key(&app_id, &ProfileId::new("default"));
        match m {
            InstalledMatch::One {
                install_profile_key,
                app_url,
            } => {
                assert_eq!(install_profile_key, expected_ipk.as_str());
                assert_eq!(app_url, format!("ato://app/{}", expected_ipk.as_str()));
            }
            other => panic!("expected One, got {other:?}"),
        }
    }

    #[test]
    fn no_match_for_unknown_handle() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(&dir);
        install_app(
            &store,
            "app_one0000000000000000000000000",
            "acme",
            "hello",
            true,
        );
        assert_eq!(
            installed_match_for_handle(&store, "github.com/other/thing").unwrap(),
            InstalledMatch::None
        );
    }

    #[test]
    fn degraded_install_without_current_revision_is_not_launchable() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(&dir);
        install_app(
            &store,
            "app_deg0000000000000000000000000",
            "acme",
            "hello",
            false,
        );
        assert_eq!(
            installed_match_for_handle(&store, "acme/hello").unwrap(),
            InstalledMatch::None,
            "an install without a current revision must not present as launchable"
        );
    }

    #[test]
    fn duplicate_handle_is_ambiguous() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(&dir);
        install_app(
            &store,
            "app_dup1000000000000000000000000",
            "acme",
            "hello",
            true,
        );
        install_app(
            &store,
            "app_dup2000000000000000000000000",
            "acme",
            "hello",
            true,
        );
        assert_eq!(
            installed_match_for_handle(&store, "acme/hello").unwrap(),
            InstalledMatch::Many
        );
    }

    #[test]
    fn handle_match_is_normalized() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(&dir);
        install_app(
            &store,
            "app_norm000000000000000000000000",
            "acme",
            "hello",
            true,
        );
        // capsule:// prefix + trailing slash + case differences all normalize.
        assert!(matches!(
            installed_match_for_handle(&store, "capsule://acme/Hello/").unwrap(),
            InstalledMatch::One { .. }
        ));
    }

    // ── installed_target_for_ipk (ato://app/<ipk> reverse lookup) ───────────

    #[test]
    fn ipk_lookup_returns_target_with_canonical_handle() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(&dir);
        let app_id = install_app(
            &store,
            "app_ipk0000000000000000000000000",
            "acme",
            "hello",
            true,
        );
        let ipk = derive_install_profile_key(&app_id, &ProfileId::new("default"));

        let target = installed_target_for_ipk(&store, ipk.as_str())
            .unwrap()
            .expect("known ipk must resolve to a launchable target");
        assert_eq!(target.install_profile_key, ipk.as_str());
        assert_eq!(target.handle, "acme/hello");
        assert_eq!(target.app_url, format!("ato://app/{}", ipk.as_str()));
    }

    #[test]
    fn ipk_lookup_unknown_key_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(&dir);
        install_app(
            &store,
            "app_ipk0000000000000000000000000",
            "acme",
            "hello",
            true,
        );
        assert_eq!(
            installed_target_for_ipk(&store, "ipk_does_not_exist00000000000000").unwrap(),
            None
        );
    }

    #[test]
    fn ipk_lookup_degraded_install_is_none() {
        // Matching key but no current revision → not launchable, same bar as
        // installed_match_for_handle.
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(&dir);
        let app_id = install_app(
            &store,
            "app_deg0000000000000000000000000",
            "acme",
            "hello",
            false,
        );
        let ipk = derive_install_profile_key(&app_id, &ProfileId::new("default"));
        assert_eq!(
            installed_target_for_ipk(&store, ipk.as_str()).unwrap(),
            None
        );
    }

    #[test]
    fn ipk_lookup_disambiguates_shared_handle() {
        // Two installs sharing a handle are Ambiguous by handle, but each ipk
        // resolves to exactly one target — the whole point of ato://app/<ipk>.
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(&dir);
        let a = install_app(
            &store,
            "app_dup1000000000000000000000000",
            "acme",
            "hello",
            true,
        );
        let b = install_app(
            &store,
            "app_dup2000000000000000000000000",
            "acme",
            "hello",
            true,
        );
        assert_eq!(
            installed_match_for_handle(&store, "acme/hello").unwrap(),
            InstalledMatch::Many,
            "handle resolution is ambiguous"
        );
        let ipk_a = derive_install_profile_key(&a, &ProfileId::new("default"));
        let ipk_b = derive_install_profile_key(&b, &ProfileId::new("default"));
        assert_eq!(
            installed_target_for_ipk(&store, ipk_a.as_str())
                .unwrap()
                .map(|t| t.install_profile_key),
            Some(ipk_a.as_str().to_string())
        );
        assert_eq!(
            installed_target_for_ipk(&store, ipk_b.as_str())
                .unwrap()
                .map(|t| t.install_profile_key),
            Some(ipk_b.as_str().to_string())
        );
    }
}
