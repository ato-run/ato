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

use super::gpu_provision::{CheckResult, CheckStatus, print_check_rows};

/// The pinned llama.cpp release the canonical local-LLM capsules use. The
/// platform→artifact mapping is version-independent, so this only labels the
/// probe; each capsule's own `engine_version` still governs its run.
const REFERENCE_ENGINE_VERSION: &str = "b9754";

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

fn diagnose() -> Vec<CheckResult> {
    let mut results = Vec::new();

    // 1. Platform + managed engine availability (reuses the fetcher's mapping).
    let support = llama_cpp_platform_support(REFERENCE_ENGINE_VERSION);
    let os = support
        .as_ref()
        .map(|s| s.os.clone())
        .unwrap_or_else(|_| std::env::consts::OS.to_string());
    match &support {
        Ok(s) if s.default_available => results.push(CheckResult {
            name: "platform",
            status: CheckStatus::Ok,
            detail: format!("{}-{} — managed llama.cpp prebuilt available", s.os, s.arch),
            recommendation: None,
        }),
        Ok(s) => results.push(CheckResult {
            name: "platform",
            status: CheckStatus::Fail,
            detail: format!("{}-{} has no managed llama.cpp prebuilt", s.os, s.arch),
            recommendation: Some(
                "Set an explicit `engine_path`, or run on macOS (arm64/x64) or Linux x64.",
            ),
        }),
        Err(_) => results.push(CheckResult {
            name: "platform",
            status: CheckStatus::Fail,
            detail: format!(
                "unsupported platform: {} {}",
                std::env::consts::OS,
                std::env::consts::ARCH
            ),
            recommendation: Some("native-inference supports macOS (arm64/x64) and Linux x64."),
        }),
    }

    // 2. Model cache writable (the content-addressed store the model verifies into).
    let store = ato_store_dir();
    match writable_dir(&store) {
        Ok(()) => results.push(CheckResult {
            name: "model_cache",
            status: CheckStatus::Ok,
            detail: format!("{} is writable", store.display()),
            recommendation: None,
        }),
        Err(e) => results.push(CheckResult {
            name: "model_cache",
            status: CheckStatus::Fail,
            detail: format!("cannot write the model cache at {}: {e}", store.display()),
            recommendation: Some(
                "Ensure ~/.ato is writable (check permissions and free disk space).",
            ),
        }),
    }

    // 3. Acceleration: Metal on macOS (default build), Vulkan on Linux NVIDIA.
    if os == "macos" {
        results.push(CheckResult {
            name: "acceleration",
            status: CheckStatus::Ok,
            detail: "Metal (Apple GPU) — the default macOS engine build is Metal-accelerated"
                .to_string(),
            recommendation: None,
        });
    } else if os == "linux" {
        match detect_host_gpu_profile() {
            Ok(profile) if profile.native_inference_vulkan_ready() => results.push(CheckResult {
                name: "acceleration",
                status: CheckStatus::Ok,
                detail: "Vulkan GPU ready — the chat-vulkan target can offload to the NVIDIA GPU"
                    .to_string(),
                recommendation: None,
            }),
            Ok(profile) if profile.has_gpu() => results.push(CheckResult {
                name: "acceleration",
                status: CheckStatus::Warn,
                detail: "NVIDIA GPU present but Vulkan is not ready — CPU (chat) works; GPU (chat-vulkan) does not yet"
                    .to_string(),
                recommendation: Some(
                    "Run `ato runner doctor --profile nvidia-ubuntu`, then `sudo ato runner provision`, to enable the chat-vulkan target.",
                ),
            }),
            Ok(_) => results.push(CheckResult {
                name: "acceleration",
                status: CheckStatus::Warn,
                detail: "No NVIDIA GPU detected — the default chat target runs on CPU".to_string(),
                recommendation: Some(
                    "CPU is fine for small models; for GPU use a Linux NVIDIA host with the chat-vulkan target.",
                ),
            }),
            Err(_) => results.push(CheckResult {
                name: "acceleration",
                status: CheckStatus::Warn,
                detail: "Could not probe the GPU — the default chat target runs on CPU".to_string(),
                recommendation: None,
            }),
        }
    } else {
        results.push(CheckResult {
            name: "acceleration",
            status: CheckStatus::Na,
            detail: format!("{os}: CPU only for native-inference"),
            recommendation: None,
        });
    }

    // 4. Recommended target (informational — always OK).
    let vulkan_target = support
        .as_ref()
        .map(|s| s.vulkan_available)
        .unwrap_or(false)
        && detect_host_gpu_profile()
            .map(|p| p.native_inference_vulkan_ready())
            .unwrap_or(false);
    let detail = if vulkan_target {
        "chat (CPU/Metal) — always; chat-vulkan — GPU-accelerated on this host".to_string()
    } else if os == "macos" {
        "chat (Metal-accelerated) — recommended on this host".to_string()
    } else {
        "chat (CPU) — recommended on this host".to_string()
    };
    results.push(CheckResult {
        name: "recommended_target",
        status: CheckStatus::Ok,
        detail,
        recommendation: None,
    });

    results
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
