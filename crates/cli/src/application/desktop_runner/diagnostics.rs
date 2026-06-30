//! `ato doctor desktop-runner` — surface the Desktop Runner capability probe
//! ([`super::probe`]) to developers (MacBook M1).
//!
//! Read-only and side-effect free, exactly like the probe: it reports what this
//! host *could* run as a local Ato Runner provider — it never starts the
//! `container` service and never launches a workload. `--json` emits the raw
//! [`DesktopRunnerFacts`] receipt; the default is a human summary.
//!
//! Diagnostics (e.g. "macOS 26+ required") are surfaced **only** here, when the
//! user explicitly invokes this command — never during normal startup.
//!
//! [`human_summary`] is a pure function of the facts so the rendering is
//! unit-tested against constructed hosts without a real Mac.

use std::fmt::Write as _;

use anyhow::Result;

use super::facts::DesktopRunnerFacts;

/// Run `ato doctor desktop-runner`. `json` ⇒ raw facts receipt; else human table.
pub(crate) fn run(json: bool) -> Result<()> {
    let facts = super::probe();
    if json {
        println!("{}", facts.to_receipt_json());
    } else {
        print!("{}", human_summary(&facts));
    }
    Ok(())
}

/// Render the developer-facing human summary (pure).
pub(crate) fn human_summary(facts: &DesktopRunnerFacts) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "ato doctor desktop-runner");
    let _ = writeln!(
        s,
        "  host: {}/{}{}  ·  runtime {}  ·  virtualization {}",
        facts.host_os,
        facts.host_arch,
        facts
            .host_platform_version
            .as_deref()
            .map(|v| format!(" ({v})"))
            .unwrap_or_default(),
        facts.desktop_runtime_version,
        if facts.virtualization_available {
            "available"
        } else {
            "unavailable"
        },
    );
    let _ = writeln!(s);

    // The host is "available" as a Desktop Runner iff it advertises a backend.
    match facts.local_backend() {
        Some(backend) => {
            let _ = writeln!(s, "  Desktop Runner: available");
            let _ = writeln!(s, "  substrate: {}", backend.substrate);
            let _ = writeln!(s, "  isolation: {}", backend.isolation_boundary.as_str());
            let _ = writeln!(s, "  mode: {}", backend.ready_state_kind.as_str());
            let _ = writeln!(s, "  maturity: {}", backend.maturity.as_str());
            // Honest M0 capability lines: everything richer than cold OCI is off.
            let _ = writeln!(
                s,
                "  ready-state restore: {}",
                yes_no(backend.supports_ready_state_restore)
            );
            let _ = writeln!(s, "  CRIU: {}", yes_no(backend.supports_criu_checkpoint));
            let _ = writeln!(s, "  bindings: {}", yes_no(backend.supports_bindings));
        }
        None => {
            let _ = writeln!(s, "  Desktop Runner: unavailable");
            if facts.diagnostics.is_empty() {
                let _ = writeln!(
                    s,
                    "  reason:\n    - no Desktop Runner substrate on this host"
                );
            } else {
                let _ = writeln!(s, "  reason:");
                for d in &facts.diagnostics {
                    let _ = writeln!(s, "    - {d}");
                }
            }
            let _ = writeln!(s, "  fallback:\n    - managed runner");
        }
    }
    s
}

fn yes_no(supported: bool) -> &'static str {
    if supported {
        "supported"
    } else {
        "unsupported"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::desktop_runner::facts::SUBSTRATE_APPLE_CONTAINERIZATION;
    use crate::application::desktop_runner::macos::{MacosProbeInputs, build_macos_facts};

    fn inputs() -> MacosProbeInputs {
        MacosProbeInputs {
            host_arch: "aarch64".into(),
            product_version: Some("26.0".into()),
            is_apple_silicon: true,
            container_path: Some("/usr/local/bin/container".into()),
            container_version: Some("container 0.1.0".into()),
            container_service_running: false,
        }
    }

    #[test]
    fn supported_summary_reports_cold_oci_and_unsupported_advanced_modes() {
        let facts = build_macos_facts(&inputs(), "0.7.0");
        let out = human_summary(&facts);
        assert!(out.contains("Desktop Runner: available"), "{out}");
        assert!(out.contains(SUBSTRATE_APPLE_CONTAINERIZATION), "{out}");
        assert!(out.contains("isolation: vm_wrapped_container"), "{out}");
        assert!(out.contains("mode: cold_oci"), "{out}");
        assert!(out.contains("maturity: experimental"), "{out}");
        assert!(out.contains("ready-state restore: unsupported"), "{out}");
        assert!(out.contains("CRIU: unsupported"), "{out}");
        assert!(out.contains("bindings: unsupported"), "{out}");
    }

    #[test]
    fn unsupported_summary_lists_actionable_reasons_and_fallback() {
        let mut i = inputs();
        i.product_version = Some("15.7.4".into());
        i.container_path = None;
        i.container_version = None;
        let facts = build_macos_facts(&i, "0.7.0");
        let out = human_summary(&facts);
        assert!(out.contains("Desktop Runner: unavailable"), "{out}");
        assert!(out.contains("macOS 26"), "{out}");
        assert!(out.contains("container"), "{out}");
        assert!(
            out.contains("fallback:") && out.contains("managed runner"),
            "{out}"
        );
    }

    #[test]
    fn json_receipt_contains_all_top_level_fields() {
        let facts = build_macos_facts(&inputs(), "0.7.0");
        let json = facts.to_receipt_json();
        for field in [
            "provider_kind",
            "host_os",
            "host_arch",
            "host_platform_version",
            "desktop_runtime_version",
            "virtualization_available",
            "substrates",
            "backends",
        ] {
            assert!(json.contains(field), "json missing {field}: {json}");
        }
    }
}
