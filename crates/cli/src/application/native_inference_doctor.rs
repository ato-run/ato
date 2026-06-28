//! `ato doctor native-inference` — one-shot readiness diagnostic for running
//! local-LLM (native-inference) capsules on this host. Reuses the GPU doctor's
//! `CheckResult`/`CheckStatus` rendering (`super::gpu_provision`) and the runtime
//! fetcher's platform→engine mapping (`llama_cpp_platform_support`) so the report
//! can never disagree with what a real `ato run` would actually do.

use anyhow::Result;
use serde::Serialize;

use capsule::common::paths::ato_store_dir;
use capsule::foundation::host_gpu::detect_host_gpu_profile;
use capsule::packers::runtime_fetcher::llama_cpp_platform_support;
use capsule::routing::native_inference::{
    EngineCheck, EngineCheckStatus, EngineId, HostCapabilities, engine_for,
};

use super::gpu_provision::{CheckResult, CheckStatus, print_check_rows};

/// The pinned llama.cpp release the canonical local-LLM capsules use. The
/// platform→artifact mapping is version-independent, so this only labels the
/// probe; each capsule's own `engine_version` still governs its run.
const REFERENCE_ENGINE_VERSION: &str = "b9754";

/// Map a capsule-layer [`EngineCheck`] to the CLI doctor's `CheckResult` (the
/// fields are 1:1). Keeps the engine layer free of the CLI's rendering types.
fn engine_check_to_result(c: EngineCheck) -> CheckResult {
    CheckResult {
        name: c.name,
        status: match c.status {
            EngineCheckStatus::Ok => CheckStatus::Ok,
            EngineCheckStatus::Warn => CheckStatus::Warn,
            EngineCheckStatus::Fail => CheckStatus::Fail,
            EngineCheckStatus::Na => CheckStatus::Na,
        },
        detail: c.detail,
        recommendation: c.recommendation,
    }
}

#[derive(Debug, Serialize)]
struct DoctorOutput {
    ready: bool,
    checks: Vec<CheckResult>,
}

/// Run `ato doctor native-inference`: probe this host and report whether it can
/// run local-LLM capsules. Exits non-zero when a required check FAILs.
pub fn run(json: bool) -> Result<()> {
    let checks = diagnose();
    let ready = checks.iter().all(|c| c.status != CheckStatus::Fail);

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&DoctorOutput { ready, checks })?
        );
    } else {
        print_table(&checks, ready);
    }

    if !ready {
        std::process::exit(1);
    }
    Ok(())
}

/// Whether this host can run native-inference capsules at all — the capability a
/// Connected Runner advertises so the control plane only dispatches
/// native-inference leases here (ato#762). True when no diagnostic check fails
/// (platform supports the managed engine and the model cache is usable); mirrors
/// the readiness rule in [`run`]. Cheap (no network), but heartbeat-path callers
/// should cache the result — the host's capability is stable across a session.
pub fn is_ready() -> bool {
    diagnose().iter().all(|c| c.status != CheckStatus::Fail)
}

fn diagnose() -> Vec<CheckResult> {
    // The platform / acceleration / recommended_target rows are engine-specific
    // and now come from the `LlamaCppEngine` doctor probe (the single source of
    // truth for what an `ato run` would do). The `model_cache` row is the host
    // cache (engine-agnostic) and stays here, inserted between platform and
    // acceleration to preserve the historical row order.
    //
    // Probe the host GPU once — the engine's acceleration/recommended_target
    // rows need it, and on Linux the probe shells out (nvidia-smi/vulkaninfo).
    // macOS never needs it (no Vulkan prebuilt) so skip the probe there. `os` is
    // computed the same way the engine does, from the fetcher's platform mapping.
    let os = llama_cpp_platform_support(REFERENCE_ENGINE_VERSION)
        .map(|s| s.os)
        .unwrap_or_else(|_| std::env::consts::OS.to_string());
    let gpu_profile = if os == "linux" {
        detect_host_gpu_profile().ok()
    } else {
        None
    };

    let host = HostCapabilities::from_profile(gpu_profile);
    let engine_rows = engine_for(EngineId::LlamaCpp).doctor_checks(&host);

    let mut results = Vec::with_capacity(engine_rows.len() + 1);
    for row in engine_rows {
        // Insert the host model-cache row right after the engine's platform row
        // so the output order is platform, model_cache, acceleration,
        // recommended_target (unchanged).
        let is_platform = row.name == "platform";
        results.push(engine_check_to_result(row));
        if is_platform {
            results.push(model_cache_check());
        }
    }

    results
}

/// The host model-cache writability row (engine-agnostic): the
/// content-addressed store every managed model verifies into.
fn model_cache_check() -> CheckResult {
    let store = ato_store_dir();
    match writable_dir(&store) {
        Ok(()) => CheckResult {
            name: "model_cache",
            status: CheckStatus::Ok,
            detail: format!("{} is writable", store.display()),
            recommendation: None,
        },
        Err(e) => CheckResult {
            name: "model_cache",
            status: CheckStatus::Fail,
            detail: format!("cannot write the model cache at {}: {e}", store.display()),
            recommendation: Some(
                "Ensure ~/.ato is writable (check permissions and free disk space).",
            ),
        },
    }
}

/// Confirm `dir` (created if needed) accepts a write, then clean up the probe.
fn writable_dir(dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let probe = dir.join(".ato-doctor-write-probe");
    std::fs::write(&probe, b"ok")?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

fn print_table(checks: &[CheckResult], ready: bool) {
    println!("ato doctor native-inference — local LLM readiness");
    println!();
    print_check_rows(checks);
    println!();
    if ready {
        println!("  ✓ This host can run local-LLM (native-inference) capsules.");
        println!("    Try: ato run github.com/ato-run/local-llm-chat");
    } else {
        println!("  ✗ Not ready — fix the FAIL item(s) above.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Probing the real host must always produce the four core checks and a
    // decisive readiness verdict without panicking. On any supported CI host
    // (macOS/Linux x64/arm64) the platform check passes.
    #[test]
    fn diagnose_reports_core_checks() {
        let checks = diagnose();
        for name in [
            "platform",
            "model_cache",
            "acceleration",
            "recommended_target",
        ] {
            assert!(
                checks.iter().any(|c| c.name == name),
                "missing check: {name}"
            );
        }
        // recommended_target is always informational (never a FAIL).
        let rec = checks
            .iter()
            .find(|c| c.name == "recommended_target")
            .unwrap();
        assert_eq!(rec.status, CheckStatus::Ok);
    }
}
