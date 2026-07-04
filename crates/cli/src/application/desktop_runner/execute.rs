//! Desktop Runner gated execution (#838, MacBook M3).
//!
//! The **only** place the Desktop Runner ever runs a workload. It fires solely
//! when the user *explicitly* selects the Desktop Runner provider
//! ([`is_explicitly_selected`]) — the default `ato run` path is untouched. Once
//! selected, it is fail-closed:
//!
//! 1. probe + [`placement::decide`] — proceed only on `local_cold_oci_candidate`;
//!    `suggest_managed_runner` / `ready_state_restore_unsupported_local` return a
//!    clear message and **never** auto-dispatch or cold-start.
//! 2. binding guard — a capsule that requires runtime bindings ([secrets.*] /
//!    [bindings.*] / [external.*], reusing
//!    [`requires_runtime_bindings`](crate::application::ready_state::bindings))
//!    is rejected **before** any container starts.
//! 3. OCI resolution — only `runtime = "oci"` targets run; source capsules are a
//!    clear "unsupported in M3" error, never a faked success.
//!
//! [`plan`] is pure (decision + guard + resolution) and fully unit-tested; the
//! live [`cold_oci::run`] is exercised only by the ignored macOS-26 smoke.

use std::fmt::Write as _;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow};
use capsule::types::CapsuleManifest;

use super::cold_oci::{self, ColdOciRunRequest, DesktopColdOciSession, ExecutionClass};
use super::placement::{self, DesktopPlacementDecision, kind};
use crate::application::ready_state::bindings::requires_runtime_bindings;

/// Env var: explicit opt-in to Desktop Runner local execution (developer-preview).
const EXECUTE_VAR: &str = "ATO_DESKTOP_RUNNER_EXECUTE";
/// Env var: provider selector; `desktop` selects the Desktop Runner.
const PROVIDER_VAR: &str = "ATO_RUN_PROVIDER";

/// Whether the Desktop Runner provider is *explicitly* selected for this run.
/// Default (both unset) is `false` — `ato run` behaves exactly as before.
pub(crate) fn is_explicitly_selected() -> bool {
    selected_from(
        std::env::var(EXECUTE_VAR).ok().as_deref(),
        std::env::var(PROVIDER_VAR).ok().as_deref(),
    )
}

/// Pure selection predicate (testable without mutating process env).
fn selected_from(execute_var: Option<&str>, provider_var: Option<&str>) -> bool {
    let truthy = execute_var
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false);
    let provider = provider_var
        .map(|v| v.trim().eq_ignore_ascii_case("desktop"))
        .unwrap_or(false);
    truthy || provider
}

/// A validated cold-OCI run plan: the decision that authorized it, the guest
/// execution class, and the concrete container request. Producing one performs
/// **no** side effects — it only decides what *would* run.
#[derive(Debug, Clone)]
pub(crate) struct ColdOciRunPlan {
    pub(crate) decision: DesktopPlacementDecision,
    pub(crate) exec_class: ExecutionClass,
    pub(crate) request: ColdOciRunRequest,
}

/// Build a cold-OCI run plan, fail-closed (pure — no execution).
///
/// Order is deliberate: placement gate → binding guard → OCI resolution. Each
/// returns a clear error rather than degrading silently.
pub(crate) fn plan(
    manifest: &CapsuleManifest,
    target: Option<&str>,
    facts: &super::facts::DesktopRunnerFacts,
    host_class: &capsule::foundation::install_lifecycle::RunnerClassFacts,
    ready_state_enabled: bool,
    container_name: String,
) -> Result<ColdOciRunPlan> {
    let decision = placement::decide(facts, host_class, None, ready_state_enabled);

    if decision.placement != kind::LOCAL_COLD_OCI_CANDIDATE {
        // suggest_managed_runner / ready_state_restore_unsupported_local / etc.
        // A recommendation, never an automatic dispatch — we just refuse to run.
        return Err(render_placement_failure(&decision));
    }

    // Binding guard — reject before any container starts. Names only, never values.
    let bindings = requires_runtime_bindings(manifest);
    if bindings.requires_bindings() {
        return Err(anyhow!(
            "Desktop Runner local cold OCI does not support runtime bindings yet. This capsule \
             requires runtime bindings: {}. Use a managed runner or run without Desktop Runner \
             until BindingLease injection is implemented.",
            bindings.summary()
        ));
    }

    let resolved = cold_oci::resolve_oci_target(manifest, target)?;

    // local_cold_oci_candidate guarantees a local backend exists.
    let backend = facts.local_backend().ok_or_else(|| {
        anyhow!("Desktop Runner: internal error — placement is a cold-OCI candidate but no backend")
    })?;
    let exec_class = ExecutionClass::from_backend(backend);
    let request = ColdOciRunRequest::from_target(container_name, &resolved);

    Ok(ColdOciRunPlan {
        decision,
        exec_class,
        request,
    })
}

/// Render a non-cold-OCI placement decision as a structured, actionable CLI
/// error. Keeps the one-line `(<placement>): <reason>` summary for log greppers,
/// then adds the four fields a user/developer needs to tell "host requirement
/// unmet" apart from "Desktop shell not wired":
///   - `platform`: the host os/arch the decision was made for
///   - `local backend`: `unavailable` (always, at this gate)
///   - `reasons`: the structured [`LocalBackendBlocker`] tags (empty when the
///     cause is not a missing backend, e.g. Ready-State-without-artifact)
///   - `next action`: per-blocker remediation, or a generic managed-runner hint
fn render_placement_failure(decision: &DesktopPlacementDecision) -> anyhow::Error {
    let mut s = String::new();
    let _ = write!(
        s,
        "Desktop Runner will not run this capsule locally ({}): {}",
        decision.placement, decision.reason
    );
    let _ = write!(
        s,
        "\n  platform: {}/{}",
        decision.host_os, decision.host_arch
    );
    let _ = write!(s, "\n  local backend: unavailable");
    if decision.local_backend_blockers.is_empty() {
        let _ = write!(s, "\n  next action: use a managed runner");
    } else {
        let tags: Vec<&str> = decision
            .local_backend_blockers
            .iter()
            .map(|b| b.as_str())
            .collect();
        let _ = write!(s, "\n  reasons: [{}]", tags.join(", "));
        let actions: Vec<&str> = decision
            .local_backend_blockers
            .iter()
            .map(|b| b.next_action())
            .collect();
        let _ = write!(s, "\n  next action: {}", actions.join(" / "));
    }
    anyhow!(s)
}

/// A container name unique per run: `ato-desktop-<sanitized-name>-<pid>-<ms>`.
fn unique_container_name(manifest: &CapsuleManifest) -> String {
    let safe: String = manifest
        .name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let safe: String = safe.trim_matches('-').chars().take(24).collect();
    let pid = std::process::id();
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("ato-desktop-{safe}-{pid}-{millis}")
}

/// Plan and run a capsule on the local Desktop Runner cold-OCI path.
pub(crate) fn run_capsule(
    manifest: &CapsuleManifest,
    target: Option<&str>,
) -> Result<DesktopColdOciSession> {
    let facts = super::probe();
    let host_class = capsule::foundation::install_lifecycle::RunnerClassFacts::from_host();
    let ready_state_enabled = crate::application::ready_state::flags::ready_state_enabled();
    let name = unique_container_name(manifest);

    let plan = plan(
        manifest,
        target,
        &facts,
        &host_class,
        ready_state_enabled,
        name,
    )?;
    eprintln!(
        "DESKTOP-RUNNER: placement selected: {} (guest {}/{})",
        plan.decision.placement, plan.exec_class.guest_os, plan.exec_class.guest_arch
    );
    cold_oci::run(&plan.request, &plan.exec_class)
}

/// Entry from the `ato run` hook: load the capsule manifest from `path` and run it
/// on the Desktop Runner, printing the session receipt. Fail-closed throughout.
pub(crate) fn run_selected(path: &Path, target: Option<&str>) -> Result<()> {
    let manifest_path = if path.is_dir() {
        path.join("capsule.toml")
    } else {
        path.to_path_buf()
    };
    let manifest = CapsuleManifest::load_from_file(&manifest_path).map_err(|e| {
        anyhow!(
            "Desktop Runner: failed to load capsule manifest at {}: {e}",
            manifest_path.display()
        )
    })?;

    let session = run_capsule(&manifest, target)?;
    println!("{}", serde_json::to_string_pretty(&session)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::desktop_runner::macos::{MacosProbeInputs, build_macos_facts};
    use capsule::foundation::install_lifecycle::RunnerClassFacts;

    fn supported_facts() -> super::super::facts::DesktopRunnerFacts {
        build_macos_facts(
            &MacosProbeInputs {
                host_arch: "aarch64".into(),
                product_version: Some("26.0".into()),
                is_apple_silicon: true,
                container_path: Some("/usr/local/bin/container".into()),
                container_version: Some("container 0.1.0".into()),
                container_service_running: true,
            },
            "0.7.0",
        )
    }

    fn no_container_facts() -> super::super::facts::DesktopRunnerFacts {
        let mut i = MacosProbeInputs {
            host_arch: "aarch64".into(),
            product_version: Some("26.0".into()),
            is_apple_silicon: true,
            container_path: Some("/usr/local/bin/container".into()),
            container_version: None,
            container_service_running: false,
        };
        i.container_path = None;
        build_macos_facts(&i, "0.7.0")
    }

    fn old_macos_facts() -> super::super::facts::DesktopRunnerFacts {
        build_macos_facts(
            &MacosProbeInputs {
                host_arch: "aarch64".into(),
                product_version: Some("15.5".into()),
                is_apple_silicon: true,
                container_path: Some("/usr/local/bin/container".into()),
                container_version: Some("container 0.1.0".into()),
                container_service_running: false,
            },
            "0.7.0",
        )
    }

    fn intel_mac_facts() -> super::super::facts::DesktopRunnerFacts {
        build_macos_facts(
            &MacosProbeInputs {
                host_arch: "x86_64".into(),
                product_version: Some("26.0".into()),
                is_apple_silicon: false,
                container_path: Some("/usr/local/bin/container".into()),
                container_version: Some("container 0.1.0".into()),
                container_service_running: false,
            },
            "0.7.0",
        )
    }

    fn manifest(extra: &str) -> CapsuleManifest {
        let base = r#"
schema_version = "0.3"
name = "Demo App"
version = "0.1.0"
type = "app"
default_target = "app"
"#;
        CapsuleManifest::from_toml(&format!("{base}{extra}")).expect("parse")
    }

    fn oci_capsule() -> CapsuleManifest {
        manifest(
            "\n[targets.app]\nruntime = \"oci\"\nimage = \"img:tag\"\ncmd = [\"server\"]\nport = 8080\n",
        )
    }

    // ── selection ───────────────────────────────────────────────────────────

    #[test]
    fn default_is_not_selected() {
        assert!(!selected_from(None, None));
        assert!(!selected_from(Some("0"), Some("managed")));
        assert!(!selected_from(Some(""), None));
    }

    #[test]
    fn explicit_selection_via_either_var() {
        assert!(selected_from(Some("1"), None));
        assert!(selected_from(Some("true"), None));
        assert!(selected_from(None, Some("desktop")));
        assert!(selected_from(None, Some("Desktop")));
    }

    // ── plan (pure, fail-closed) ────────────────────────────────────────────

    #[test]
    fn candidate_no_binding_oci_capsule_plans_a_run() {
        let p = plan(
            &oci_capsule(),
            None,
            &supported_facts(),
            &RunnerClassFacts::from_host(),
            false,
            "ato-desktop-test".into(),
        )
        .unwrap();
        assert_eq!(p.decision.placement, kind::LOCAL_COLD_OCI_CANDIDATE);
        assert_eq!(p.request.image, "img:tag");
        assert_eq!(p.exec_class.guest_os, "linux");
        assert_eq!(p.exec_class.guest_arch, "aarch64");
    }

    #[test]
    fn suggest_managed_runner_refuses_to_run() {
        let err = plan(
            &oci_capsule(),
            None,
            &no_container_facts(),
            &RunnerClassFacts::from_host(),
            false,
            "n".into(),
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("will not run this capsule locally"),
            "{err}"
        );
        assert!(err.to_string().contains("managed runner"), "{err}");
    }

    // ── structured placement diagnostics (4 host patterns) ─────────────────
    //
    // The placement gate must distinguish "host requirement unmet" (one of the
    // three macOS preconditions) from a generic "no local backend" so a user
    // knows exactly what to fix. Each pattern asserts the rendered error names
    // the specific blocker tag + next action, and that the success path does
    // not error.

    #[test]
    fn placement_failure_apple_silicon_macos_too_old_names_upgrade_action() {
        let err = plan(
            &oci_capsule(),
            None,
            &old_macos_facts(),
            &RunnerClassFacts::from_host(),
            false,
            "n".into(),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("macos_too_old"), "missing blocker tag: {msg}");
        assert!(
            msg.contains("upgrade macOS"),
            "missing next-action hint: {msg}"
        );
        assert!(msg.contains("platform: macos/aarch64"), "{msg}");
        assert!(msg.contains("local backend: unavailable"), "{msg}");
    }

    #[test]
    fn placement_failure_apple_silicon_container_missing_names_install_action() {
        let err = plan(
            &oci_capsule(),
            None,
            &no_container_facts(),
            &RunnerClassFacts::from_host(),
            false,
            "n".into(),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("apple_container_missing"), "{msg}");
        assert!(
            msg.contains("install Apple `container`"),
            "missing next-action hint: {msg}"
        );
    }

    #[test]
    fn placement_failure_intel_mac_names_apple_silicon_blocker() {
        let err = plan(
            &oci_capsule(),
            None,
            &intel_mac_facts(),
            &RunnerClassFacts::from_host(),
            false,
            "n".into(),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not_apple_silicon"), "{msg}");
        assert!(
            msg.contains("Apple Containerization requires Apple silicon"),
            "missing next-action hint: {msg}"
        );
        assert!(msg.contains("platform: macos/x86_64"), "{msg}");
    }

    #[test]
    fn placement_success_all_requirements_satisfied_plans_cold_oci() {
        let p = plan(
            &oci_capsule(),
            None,
            &supported_facts(),
            &RunnerClassFacts::from_host(),
            false,
            "ato-desktop-test".into(),
        )
        .unwrap();
        assert_eq!(p.decision.placement, kind::LOCAL_COLD_OCI_CANDIDATE);
        assert!(
            p.decision.local_backend_blockers.is_empty(),
            "a satisfied host has no blockers"
        );
    }

    #[test]
    fn ready_state_enabled_does_not_cold_fallback() {
        let err = plan(
            &oci_capsule(),
            None,
            &supported_facts(),
            &RunnerClassFacts::from_host(),
            true, // Ready-State enabled
            "n".into(),
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("will not run this capsule locally"),
            "{err}"
        );
    }

    #[test]
    fn binding_required_capsule_is_rejected_before_run() {
        let m = manifest(
            "\n[targets.app]\nruntime = \"oci\"\nimage = \"img:tag\"\n\n[secrets.openai]\nenv = \"OPENAI_API_KEY\"\n",
        );
        let err = plan(
            &m,
            None,
            &supported_facts(),
            &RunnerClassFacts::from_host(),
            false,
            "n".into(),
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("does not support runtime bindings"),
            "{err}"
        );
        assert!(
            err.to_string().contains("openai"),
            "names the requirement: {err}"
        );
    }

    #[test]
    fn source_capsule_is_unsupported_not_faked() {
        let m = manifest(
            "\n[targets.app]\nruntime = \"source\"\nrun = \"python app.py\"\nport = 8080\n",
        );
        let err = plan(
            &m,
            None,
            &supported_facts(),
            &RunnerClassFacts::from_host(),
            false,
            "n".into(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("runtime=\"oci\""), "{err}");
    }

    #[test]
    fn unique_container_name_is_sanitized_and_unique_ish() {
        let n = unique_container_name(&oci_capsule());
        assert!(n.starts_with("ato-desktop-demo-app-"), "{n}");
        // No spaces / uppercase leaked from "Demo App".
        assert!(!n.contains(' ') && n == n.to_lowercase(), "{n}");
    }

    #[test]
    fn session_receipt_serializes_guest_class_and_binding_zero() {
        // Build a session receipt directly (no container) to check its shape.
        let facts = supported_facts();
        let class = ExecutionClass::from_backend(facts.local_backend().unwrap());
        let session = DesktopColdOciSession {
            session_id: "s".into(),
            provider_kind: "desktop".into(),
            substrate: class.substrate.clone(),
            host_os: class.host_os.clone(),
            host_arch: class.host_arch.clone(),
            guest_os: class.guest_os.clone(),
            guest_arch: class.guest_arch.clone(),
            isolation_boundary: class.isolation_boundary.clone(),
            ready_state_kind: class.ready_state_kind.clone(),
            image: "img:tag".into(),
            container_name: "s".into(),
            port: Some(8080),
            health_status: "running".into(),
            binding_required: false,
            binding_leases: 0,
            cleanup_ok: true,
        };
        let json = serde_json::to_string(&session).unwrap();
        assert!(json.contains("\"guest_os\":\"linux\""), "{json}");
        assert!(json.contains("\"binding_required\":false"), "{json}");
        assert!(json.contains("\"binding_leases\":0"), "{json}");
    }
}
