//! `ato doctor runner` — read-only capsule-runner host diagnostics.
//!
//! Never mutates host state; prints what was observed, what is missing, and the
//! exact next step (`ato runner setup --fix` for the fixable set, the manual step
//! for BIOS/OS blockers). `--json` emits the same facts machine-readable so
//! provisioning scripts can gate on them.

use anyhow::Result;
use serde::Serialize;

use super::{Check, CheckStatus, ReadinessVerdict, ReadyStateSummary, checks, ready_state_summary};

#[derive(Serialize)]
struct DoctorReport<'a> {
    profile: &'static str,
    checks: &'a [Check],
    ready_state: &'a ReadyStateSummary,
    /// True iff nothing is Missing or Blocked (Warn does not block readiness).
    ready: bool,
}

pub(crate) fn run(json: bool) -> Result<()> {
    let checks = checks::gather();
    let summary = ready_state_summary(&checks);
    let ready = checks
        .iter()
        .all(|c| !matches!(c.status, CheckStatus::Missing | CheckStatus::Blocked));

    if json {
        let report =
            DoctorReport { profile: "capsule-runner", checks: &checks, ready_state: &summary, ready };
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!("Ato Capsule Runner Doctor");
    println!();
    println!("Host:");
    for c in &checks {
        let (mark, status) = match c.status {
            CheckStatus::Ok => ("✓", String::new()),
            CheckStatus::Warn => ("⚠", " — WARN".to_string()),
            CheckStatus::Missing => ("✗", " — MISSING".to_string()),
            CheckStatus::Blocked => ("✗", " — BLOCKED".to_string()),
        };
        println!("  {mark} {}{status}: {}", c.label, c.detail);
        if let Some(fix) = &c.fix
            && c.status != CheckStatus::Ok
        {
            println!("      fix: {fix}");
        }
    }

    println!();
    println!("Ready-State:");
    for (name, verdict) in
        [("build_ready_state", &summary.build_ready_state), ("restore_snapshot", &summary.restore_snapshot)]
    {
        match verdict {
            ReadinessVerdict::Ok => println!("  ✓ {name}: OK"),
            ReadinessVerdict::Blocked(on) => println!("  ✗ {name}: BLOCKED on {}", on.join(", ")),
        }
    }

    println!();
    let blocked: Vec<&Check> =
        checks.iter().filter(|c| c.status == CheckStatus::Blocked).collect();
    let missing = checks.iter().filter(|c| c.status == CheckStatus::Missing).count();
    if !blocked.is_empty() {
        println!("Manual steps required (not fixable from software):");
        for c in &blocked {
            println!("  - {}: {}", c.label, c.detail);
        }
    }
    if missing > 0 {
        println!("Suggested fix ({missing} fixable item(s)):");
        println!("  sudo ato runner setup --fix");
    } else if blocked.is_empty() {
        println!("This machine is ready as a Capsule Runner.");
    }
    Ok(())
}
