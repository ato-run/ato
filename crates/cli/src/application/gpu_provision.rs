//! GPU host provisioning and health checking (Dockerless).
//!
//! Implements `ato runner doctor` (read-only diagnostics) and
//! `ato runner provision` (Ubuntu + NVIDIA driver / Vulkan runtime
//! installation — no Docker / Podman / nvidia-container-toolkit). Detection
//! logic lives in `capsule::foundation::host_gpu`; receipt and marker types
//! live in `capsule::foundation::provision_receipt`.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};
use capsule::common::paths::ato_path_or_workspace_tmp;
use capsule::foundation::host_gpu::HostGpuProfile;
use capsule::foundation::provision_receipt::{
    GpuDeviceSummary, ProvisionMarker, ProvisionPhase, ProvisionReceipt, SmokeResult,
};
use serde::Serialize;

const PROVISION_RECEIPT_RELATIVE: &str = "runner/provision-receipt.json";
const PROVISION_MARKER_RELATIVE: &str = "runner/provision-marker.json";

/// NVIDIA driver branch installed by `provision`. Ubuntu's
/// `nvidia-driver-575` metapackage tracks the current LTS branch.
const NVIDIA_DRIVER_PACKAGE: &str = "nvidia-driver-575";

/// The pinned sglang wheel the `nvidia-cuda` profile provisions into the managed
/// venv. Mirrors the fetcher's pin (the torch cu124 triple + kernels live in
/// `capsule::packers::runtime_fetcher`); the `nvidia-cuda` doctor/provision label
/// this version and the provision drives `RuntimeFetcher::ensure_sglang` with it.
/// The cu124 pin requires an NVIDIA driver new enough to expose CUDA ≥ 12.4 (the
/// `R550`-era branch); the doctor surfaces that as a real gate.
const SGLANG_REFERENCE_WHEEL: &str = "0.4.10.post2";

/// Minimum CUDA driver-API version (the `cu124` pin) the SGLang managed venv's
/// torch wheels require. A host whose driver exposes an older CUDA than this is a
/// real FAIL in the `nvidia-cuda` doctor (the venv would import-fail at runtime).
const SGLANG_MIN_CUDA_MAJOR: u32 = 12;
const SGLANG_MIN_CUDA_MINOR: u32 = 4;

/// `apt` packages the `nvidia-cuda` profile installs for the SGLang managed venv:
/// a system `python3` plus the stdlib `venv` module (split from python3 on
/// Debian/Ubuntu). `uv` (the venv/pip driver) is fetched by the runtime fetcher,
/// not apt.
const CUDA_PYTHON_PACKAGES: &[&str] = &["python3", "python3-venv"];

/// GPU VRAM (bytes) the `nvidia-cuda` doctor treats as comfortable for the
/// AWQ-quantized weights SGLang loads (~18-20GB) plus a KV cache. Below this is a
/// WARNing, never a hard FAIL — smaller models / quantizations still fit.
const SGLANG_RECOMMENDED_VRAM_BYTES: u64 = 24 * 1024 * 1024 * 1024;

// ─────────────────────────────────────────────
// Paths
// ─────────────────────────────────────────────

fn provision_receipt_path() -> PathBuf {
    ato_path_or_workspace_tmp(PROVISION_RECEIPT_RELATIVE)
}

fn provision_marker_path() -> PathBuf {
    ato_path_or_workspace_tmp(PROVISION_MARKER_RELATIVE)
}

// ─────────────────────────────────────────────
// Doctor
// ─────────────────────────────────────────────

/// A single diagnostic check result. Shared by the GPU host doctor and
/// `ato doctor native-inference`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct CheckResult {
    pub(crate) name: &'static str,
    pub(crate) status: CheckStatus,
    pub(crate) detail: String,
    pub(crate) recommendation: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CheckStatus {
    Ok,
    Warn,
    Fail,
    Na,
}

/// Render the `[OK]/[WARN]/[FAIL]/[N/A] name → detail/recommendation` rows shared
/// by every doctor's human-readable table.
pub(crate) fn print_check_rows(checks: &[CheckResult]) {
    for check in checks {
        let label = match check.status {
            CheckStatus::Ok => "OK  ",
            CheckStatus::Warn => "WARN",
            CheckStatus::Fail => "FAIL",
            CheckStatus::Na => "N/A ",
        };
        println!("  [{label}] {}", check.name);
        println!("         {}", check.detail);
        if let Some(rec) = check.recommendation {
            println!("         → {rec}");
        }
    }
}

/// JSON envelope for `ato runner doctor --json`.
#[derive(Debug, Serialize)]
struct DoctorOutput {
    profile: HostGpuProfile,
    checks: Vec<CheckResult>,
    ready: bool,
}

/// Run `ato runner doctor`: probe the host and report GPU readiness.
pub fn run_doctor(json: bool) -> Result<()> {
    let profile = capsule::foundation::host_gpu::detect_host_gpu_profile()
        .context("Failed to detect host GPU profile")?;

    let checks = diagnose(&profile);
    let ready = checks.iter().all(|c| c.status != CheckStatus::Fail);

    if json {
        let output = DoctorOutput {
            profile,
            checks,
            ready,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        print_doctor_table(&profile, &checks, ready);
    }

    if !ready {
        std::process::exit(1);
    }
    Ok(())
}

fn diagnose(profile: &HostGpuProfile) -> Vec<CheckResult> {
    let mut results = Vec::new();

    // OS check
    if profile.os.distro.is_empty() {
        results.push(CheckResult {
            name: "os",
            status: CheckStatus::Fail,
            detail: "Could not detect OS from /etc/os-release".to_string(),
            recommendation: Some("This command targets Ubuntu 22.04 or 24.04."),
        });
    } else if profile.os_supported() {
        results.push(CheckResult {
            name: "os",
            status: CheckStatus::Ok,
            detail: format!("{} {}", profile.os.distro, profile.os.version),
            recommendation: None,
        });
    } else {
        results.push(CheckResult {
            name: "os",
            status: CheckStatus::Fail,
            detail: format!(
                "{} {} (supported: ubuntu 22.04, 24.04)",
                profile.os.distro, profile.os.version
            ),
            recommendation: Some("ato runner provision only supports Ubuntu 22.04/24.04."),
        });
    }

    // Secure Boot check
    match profile.secure_boot_enabled {
        Some(true) => results.push(CheckResult {
            name: "secure_boot",
            status: CheckStatus::Warn,
            detail: "Secure Boot is ENABLED".to_string(),
            recommendation: Some("MOK enrollment is required after driver install. Follow the instructions printed by `ato runner provision`."),
        }),
        Some(false) => results.push(CheckResult {
            name: "secure_boot",
            status: CheckStatus::Ok,
            detail: "Disabled".to_string(),
            recommendation: None,
        }),
        None => results.push(CheckResult {
            name: "secure_boot",
            status: CheckStatus::Na,
            detail: "mokutil not installed — cannot determine".to_string(),
            recommendation: None,
        }),
    }

    // GPU presence
    if profile.has_gpu() {
        let gpu_names: Vec<&str> = profile.gpus.iter().map(|g| g.name.as_str()).collect();
        results.push(CheckResult {
            name: "gpu",
            status: CheckStatus::Ok,
            detail: format!("{} GPU(s): {}", profile.gpus.len(), gpu_names.join(", ")),
            recommendation: None,
        });
    } else {
        results.push(CheckResult {
            name: "gpu",
            status: CheckStatus::Fail,
            detail: "No NVIDIA GPUs detected (nvidia-smi not found or no GPUs)".to_string(),
            recommendation: Some("Ensure NVIDIA GPUs are physically installed and powered. Run `ato runner provision` to install the driver."),
        });
    }

    // Driver
    if profile.driver_installed() {
        let ver = profile
            .driver
            .as_ref()
            .map(|d| d.version.as_str())
            .unwrap_or("unknown");
        results.push(CheckResult {
            name: "nvidia_driver",
            status: CheckStatus::Ok,
            detail: format!("Driver {ver} installed, nvidia-smi available"),
            recommendation: None,
        });
    } else {
        results.push(CheckResult {
            name: "nvidia_driver",
            status: CheckStatus::Fail,
            detail: "NVIDIA driver not installed or nvidia-smi not functional".to_string(),
            recommendation: Some("Run: sudo ato runner provision"),
        });
    }

    // CUDA driver API — informational only (kept for a future CUDA source-build
    // path; the Vulkan native-inference path does not require it).
    match profile.cuda.as_ref() {
        Some(cuda) => results.push(CheckResult {
            name: "cuda_driver_api",
            status: CheckStatus::Ok,
            detail: format!("CUDA driver API {} detected", cuda.driver_api_version),
            recommendation: None,
        }),
        None => results.push(CheckResult {
            name: "cuda_driver_api",
            // Informational only — not required for the Vulkan engine path.
            status: CheckStatus::Na,
            detail: "CUDA driver API not detected (not required for the Vulkan engine)".to_string(),
            recommendation: None,
        }),
    }

    // Vulkan loader library (Dockerless GPU path).
    if profile.vulkan_loader_present() {
        results.push(CheckResult {
            name: "vulkan_loader",
            status: CheckStatus::Ok,
            detail: "Vulkan loader (libvulkan) present".to_string(),
            recommendation: None,
        });
    } else {
        results.push(CheckResult {
            name: "vulkan_loader",
            status: CheckStatus::Fail,
            detail: "Vulkan loader (libvulkan) not found".to_string(),
            recommendation: Some("Run: sudo ato runner provision --profile nvidia-ubuntu"),
        });
    }

    // `vulkaninfo` tool (from vulkan-tools) — the readiness/smoke probe. Tracked
    // separately from the loader: a present loader does NOT imply the tool.
    if profile.vulkaninfo_available() {
        results.push(CheckResult {
            name: "vulkaninfo",
            status: CheckStatus::Ok,
            detail: "vulkaninfo tool available".to_string(),
            recommendation: None,
        });
    } else {
        results.push(CheckResult {
            name: "vulkaninfo",
            status: CheckStatus::Fail,
            detail: "vulkaninfo not found (install vulkan-tools)".to_string(),
            recommendation: Some("Run: sudo ato runner provision --profile nvidia-ubuntu"),
        });
    }

    // NVIDIA Vulkan ICD manifest presence (necessary, not sufficient).
    if profile.nvidia_vulkan_icd_present() {
        results.push(CheckResult {
            name: "nvidia_vulkan_icd",
            status: CheckStatus::Ok,
            detail: "NVIDIA Vulkan ICD manifest present".to_string(),
            recommendation: None,
        });
    } else {
        results.push(CheckResult {
            name: "nvidia_vulkan_icd",
            status: CheckStatus::Fail,
            detail: "No NVIDIA Vulkan ICD manifest (nvidia_icd.json) found".to_string(),
            recommendation: Some(
                "Install the NVIDIA driver's Vulkan userspace (e.g. libnvidia-gl-<branch>) on \
                 a bare-metal host, or use a Vulkan-capable image",
            ),
        });
    }

    // Vulkan NVIDIA device visibility (via vulkaninfo).
    if profile.vulkan_nvidia_device_visible() {
        results.push(CheckResult {
            name: "vulkan_nvidia_device",
            status: CheckStatus::Ok,
            detail: "vulkaninfo reports an NVIDIA Vulkan device".to_string(),
            recommendation: None,
        });
    } else {
        results.push(CheckResult {
            name: "vulkan_nvidia_device",
            status: CheckStatus::Fail,
            detail: "No NVIDIA Vulkan device visible".to_string(),
            recommendation: Some(
                "Verify the NVIDIA driver + Vulkan ICD, then `vulkaninfo --summary`",
            ),
        });
    }

    // Overall native-inference (Vulkan) readiness.
    if profile.native_inference_vulkan_ready() {
        results.push(CheckResult {
            name: "native_inference_vulkan_ready",
            status: CheckStatus::Ok,
            detail: "Host can run a Vulkan-accelerated native-inference engine".to_string(),
            recommendation: None,
        });
    } else {
        results.push(CheckResult {
            name: "native_inference_vulkan_ready",
            status: CheckStatus::Fail,
            detail: "Host not ready for Vulkan native-inference (needs GPU + driver + Vulkan \
                     loader + vulkaninfo + NVIDIA device)"
                .to_string(),
            recommendation: Some("Run: sudo ato runner provision --profile nvidia-ubuntu"),
        });
    }

    results
}

fn print_doctor_table(profile: &HostGpuProfile, checks: &[CheckResult], ready: bool) {
    println!("ato runner doctor — GPU host diagnostics");
    println!();
    println!(
        "  OS:       {} {} (kernel {})",
        profile.os.distro, profile.os.version, profile.os.kernel
    );
    if let Some(sb) = profile.secure_boot_enabled {
        println!("  Secure Boot: {}", if sb { "ENABLED" } else { "disabled" });
    }
    println!(
        "  GPUs:     {}",
        if profile.gpus.is_empty() {
            "none detected".into()
        } else {
            format!("{} device(s)", profile.gpus.len())
        }
    );
    for gpu in &profile.gpus {
        let vram_gb = gpu.vram_bytes / (1024 * 1024 * 1024);
        println!("           - {} ({} GB VRAM)", gpu.name, vram_gb);
    }
    println!();

    print_check_rows(checks);

    println!();
    if ready {
        println!("  ✓ Host is ready for GPU LLM capsules.");
    } else {
        println!("  ✗ Host is NOT ready. Fix FAIL items above.");
        println!("    Next step: sudo ato runner provision");
    }
}

// ─────────────────────────────────────────────
// Doctor — profile dispatch + the CUDA (SGLang) profile
// ─────────────────────────────────────────────

/// Route `ato runner doctor --profile <P>` to the right diagnostic. Keeps the
/// historical no-`--profile` invocation (default `nvidia-ubuntu`) working and
/// adds the `nvidia-cuda` (SGLang) profile. An unknown profile is a clean bail
/// (mirrors `run_provision`'s profile guard).
pub fn run_doctor_for_profile(profile_name: &str, json: bool) -> Result<()> {
    match profile_name {
        "nvidia-ubuntu" => run_doctor(json),
        "nvidia-cuda" => run_doctor_cuda(json),
        other => bail!(
            "Unknown doctor profile: {other}. Supported: 'nvidia-ubuntu' (Vulkan / llama.cpp), \
             'nvidia-cuda' (SGLang)."
        ),
    }
}

/// Parse a CUDA driver-API version string (e.g. `"12.4"`) into `(major, minor)`.
/// Returns `None` when empty or unparseable. A bare major (`"12"`) yields minor 0.
fn parse_cuda_version(raw: &str) -> Option<(u32, u32)> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut parts = trimmed.split('.');
    let major = parts.next()?.parse::<u32>().ok()?;
    let minor = parts
        .next()
        .and_then(|m| m.parse::<u32>().ok())
        .unwrap_or(0);
    Some((major, minor))
}

/// `true` when `(major, minor)` is at least the `cu124` floor SGLang's torch
/// wheels need.
fn cuda_meets_floor(major: u32, minor: u32) -> bool {
    (major, minor) >= (SGLANG_MIN_CUDA_MAJOR, SGLANG_MIN_CUDA_MINOR)
}

/// Run `ato runner doctor --profile nvidia-cuda`: probe the host and report
/// SGLang (CUDA) readiness. Mirrors [`run_doctor`] for the Vulkan profile.
pub fn run_doctor_cuda(json: bool) -> Result<()> {
    let profile = capsule::foundation::host_gpu::detect_host_gpu_profile()
        .context("Failed to detect host GPU profile")?;

    let checks = diagnose_cuda(&profile);
    let ready = checks.iter().all(|c| c.status != CheckStatus::Fail);

    if json {
        let output = DoctorOutput {
            profile,
            checks,
            ready,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        print_doctor_table_cuda(&profile, &checks, ready);
    }

    if !ready {
        std::process::exit(1);
    }
    Ok(())
}

/// The `nvidia-cuda` (SGLang) check list. REUSES the Inc2 CUDA predicates on
/// [`HostGpuProfile`] (`native_inference_cuda_ready`, `max_gpu_vram_bytes`, the
/// `cuda`/`cuda_runtime` fields) so the doctor can never disagree with what a
/// real `ato run` of an sglang capsule would do. NO docker / nvidia-container-
/// toolkit checks (Dockerless). The deep `import sglang` smoke is run live (the
/// venv may not exist yet), so the sglang-venv row degrades to WARN/N/A rather
/// than lying — it is the provision + a real run that prove the import.
fn diagnose_cuda(profile: &HostGpuProfile) -> Vec<CheckResult> {
    let mut results = Vec::new();

    // 1. OS (same supported set as the Vulkan profile).
    if profile.os.distro.is_empty() {
        results.push(CheckResult {
            name: "os",
            status: CheckStatus::Fail,
            detail: "Could not detect OS from /etc/os-release".to_string(),
            recommendation: Some("This command targets Ubuntu 22.04 or 24.04."),
        });
    } else if profile.os_supported() {
        results.push(CheckResult {
            name: "os",
            status: CheckStatus::Ok,
            detail: format!("{} {}", profile.os.distro, profile.os.version),
            recommendation: None,
        });
    } else {
        results.push(CheckResult {
            name: "os",
            status: CheckStatus::Fail,
            detail: format!(
                "{} {} (supported: ubuntu 22.04, 24.04)",
                profile.os.distro, profile.os.version
            ),
            recommendation: Some(
                "ato runner provision --profile nvidia-cuda only supports Ubuntu 22.04/24.04.",
            ),
        });
    }

    // 2. Secure Boot (same MOK policy as Vulkan — never auto-disabled).
    match profile.secure_boot_enabled {
        Some(true) => results.push(CheckResult {
            name: "secure_boot",
            status: CheckStatus::Warn,
            detail: "Secure Boot is ENABLED".to_string(),
            recommendation: Some("MOK enrollment is required after driver install. Follow the instructions printed by `ato runner provision --profile nvidia-cuda`."),
        }),
        Some(false) => results.push(CheckResult {
            name: "secure_boot",
            status: CheckStatus::Ok,
            detail: "Disabled".to_string(),
            recommendation: None,
        }),
        None => results.push(CheckResult {
            name: "secure_boot",
            status: CheckStatus::Na,
            detail: "mokutil not installed — cannot determine".to_string(),
            recommendation: None,
        }),
    }

    // 3. GPU presence.
    if profile.has_gpu() {
        let gpu_names: Vec<&str> = profile.gpus.iter().map(|g| g.name.as_str()).collect();
        results.push(CheckResult {
            name: "gpu",
            status: CheckStatus::Ok,
            detail: format!("{} GPU(s): {}", profile.gpus.len(), gpu_names.join(", ")),
            recommendation: None,
        });
    } else {
        results.push(CheckResult {
            name: "gpu",
            status: CheckStatus::Fail,
            detail: "No NVIDIA GPUs detected (nvidia-smi not found or no GPUs)".to_string(),
            recommendation: Some("Ensure NVIDIA GPUs are physically installed and powered. Run `sudo ato runner provision --profile nvidia-cuda` to install the driver."),
        });
    }

    // 4. NVIDIA driver + version (nvidia-smi present and functional).
    if profile.driver_installed() {
        let ver = profile
            .driver
            .as_ref()
            .map(|d| d.version.as_str())
            .unwrap_or("unknown");
        results.push(CheckResult {
            name: "nvidia_driver",
            status: CheckStatus::Ok,
            detail: format!("Driver {ver} installed, nvidia-smi available"),
            recommendation: None,
        });
    } else {
        results.push(CheckResult {
            name: "nvidia_driver",
            status: CheckStatus::Fail,
            detail: "NVIDIA driver not installed or nvidia-smi not functional".to_string(),
            recommendation: Some("Run: sudo ato runner provision --profile nvidia-cuda"),
        });
    }

    // 5. CUDA runtime / toolkit availability — a REAL gate for SGLang (its torch
    //    wheels are pinned to cu124, so the driver must expose CUDA ≥ 12.4).
    match profile.cuda.as_ref() {
        Some(cuda) => match parse_cuda_version(&cuda.driver_api_version) {
            Some((major, minor)) if cuda_meets_floor(major, minor) => results.push(CheckResult {
                name: "cuda_runtime",
                status: CheckStatus::Ok,
                detail: format!(
                    "CUDA driver API {major}.{minor} (≥ {SGLANG_MIN_CUDA_MAJOR}.{SGLANG_MIN_CUDA_MINOR}, satisfies the cu124 sglang pin)"
                ),
                recommendation: None,
            }),
            Some((major, minor)) => results.push(CheckResult {
                name: "cuda_runtime",
                status: CheckStatus::Fail,
                detail: format!(
                    "CUDA driver API {major}.{minor} is older than the cu124 sglang pin (needs ≥ {SGLANG_MIN_CUDA_MAJOR}.{SGLANG_MIN_CUDA_MINOR})"
                ),
                recommendation: Some(
                    "Upgrade the NVIDIA driver to an R550-era (CUDA 12.4+) branch: sudo ato runner provision --profile nvidia-cuda --force",
                ),
            }),
            None => results.push(CheckResult {
                name: "cuda_runtime",
                status: CheckStatus::Fail,
                detail: "CUDA driver API version could not be parsed from nvidia-smi".to_string(),
                recommendation: Some("Run: sudo ato runner provision --profile nvidia-cuda"),
            }),
        },
        None => results.push(CheckResult {
            name: "cuda_runtime",
            status: CheckStatus::Fail,
            detail: "CUDA runtime not detected (nvidia-smi reported no CUDA driver API)".to_string(),
            recommendation: Some("Run: sudo ato runner provision --profile nvidia-cuda"),
        }),
    }

    // 6. python3 — the host interpreter the managed sglang venv is built from.
    let python3_ok = profile
        .cuda_runtime
        .as_ref()
        .map(|c| c.python3_ok)
        .unwrap_or(false);
    if python3_ok {
        results.push(CheckResult {
            name: "python3",
            status: CheckStatus::Ok,
            detail: "python3 interpreter present on PATH".to_string(),
            recommendation: None,
        });
    } else {
        results.push(CheckResult {
            name: "python3",
            status: CheckStatus::Fail,
            detail: "python3 not found on PATH".to_string(),
            recommendation: Some("Run: sudo ato runner provision --profile nvidia-cuda (installs python3 + python3-venv)"),
        });
    }

    // 7. python venv module — the stdlib `venv` (split from python3 on Ubuntu).
    let venv_ok = profile
        .cuda_runtime
        .as_ref()
        .map(|c| c.venv_module_ok)
        .unwrap_or(false);
    if venv_ok {
        results.push(CheckResult {
            name: "python_venv",
            status: CheckStatus::Ok,
            detail: "python3 -m venv is available (sglang installs into a managed venv)".to_string(),
            recommendation: None,
        });
    } else {
        results.push(CheckResult {
            name: "python_venv",
            status: CheckStatus::Fail,
            detail: "the python3 `venv` module is not importable".to_string(),
            recommendation: Some("Run: sudo ato runner provision --profile nvidia-cuda (installs python3-venv)"),
        });
    }

    // 8. sglang venv / `import sglang` — proves a usable managed engine. The venv
    //    is only present AFTER provision (and `import sglang` only passes on a
    //    real CUDA host). Probe the canonical venv python live, but degrade
    //    honestly to WARN (host CUDA-ready, venv just not built yet) rather than
    //    FAIL on a host that simply has not been provisioned.
    match probe_sglang_venv(SGLANG_REFERENCE_WHEEL) {
        SglangVenvProbe::ImportOk => results.push(CheckResult {
            name: "sglang_venv",
            status: CheckStatus::Ok,
            detail: format!(
                "managed sglang {SGLANG_REFERENCE_WHEEL} venv present and `import sglang` succeeds"
            ),
            recommendation: None,
        }),
        SglangVenvProbe::ImportFailed(detail) => results.push(CheckResult {
            name: "sglang_venv",
            status: CheckStatus::Fail,
            detail: format!(
                "managed sglang {SGLANG_REFERENCE_WHEEL} venv present but `import sglang` failed: {detail}"
            ),
            recommendation: Some(
                "Rebuild the venv: sudo ato runner provision --profile nvidia-cuda --force (the cu124 kernels must load on this GPU).",
            ),
        }),
        SglangVenvProbe::Missing => results.push(CheckResult {
            name: "sglang_venv",
            status: CheckStatus::Warn,
            detail: format!(
                "managed sglang {SGLANG_REFERENCE_WHEEL} venv not built yet — it is created on first provision/run"
            ),
            recommendation: Some(
                "Run: sudo ato runner provision --profile nvidia-cuda (creates the venv + runs the import smoke).",
            ),
        }),
    }

    // 9. GPU VRAM headroom — a hint, not a gate (WARN below the recommended
    //    floor; smaller models / quantizations still fit).
    if profile.has_gpu() {
        let vram = profile.max_gpu_vram_bytes();
        let vram_gb = vram / (1024 * 1024 * 1024);
        if profile.gpu_vram_meets(SGLANG_RECOMMENDED_VRAM_BYTES) {
            results.push(CheckResult {
                name: "gpu_vram",
                status: CheckStatus::Ok,
                detail: format!("{vram_gb} GB VRAM on the largest GPU (comfortable for AWQ weights)"),
                recommendation: None,
            });
        } else {
            results.push(CheckResult {
                name: "gpu_vram",
                status: CheckStatus::Warn,
                detail: format!(
                    "{vram_gb} GB VRAM on the largest GPU — below the ~{} GB recommended for AWQ weights",
                    SGLANG_RECOMMENDED_VRAM_BYTES / (1024 * 1024 * 1024)
                ),
                recommendation: Some(
                    "Use a smaller / more-quantized model, or a higher-VRAM GPU, if weights do not fit.",
                ),
            });
        }
    }

    // 10. Overall SGLang (CUDA) readiness floor — reuses the shared predicate.
    if profile.native_inference_cuda_ready() {
        results.push(CheckResult {
            name: "native_inference_cuda_ready",
            status: CheckStatus::Ok,
            detail: "Host can run the SGLang (CUDA) native-inference engine".to_string(),
            recommendation: None,
        });
    } else {
        results.push(CheckResult {
            name: "native_inference_cuda_ready",
            status: CheckStatus::Fail,
            detail: "Host not ready for SGLang/CUDA native-inference (needs GPU + driver + CUDA \
                     runtime + python3/venv)"
                .to_string(),
            recommendation: Some("Run: sudo ato runner provision --profile nvidia-cuda"),
        });
    }

    results
}

/// Result of probing the managed sglang venv for `ato runner doctor`.
enum SglangVenvProbe {
    /// The venv python exists and `import sglang` succeeded.
    ImportOk,
    /// The venv python exists but `import sglang` failed (stderr tail).
    ImportFailed(String),
    /// No venv python at the canonical path (not built yet).
    Missing,
}

/// Probe the managed sglang venv at the canonical cache path for `version`:
/// does its python exist, and does `import sglang` succeed? Read-only — never
/// builds anything (that is provision's job). Reuses the fetcher's canonical
/// venv-python path so the doctor and the launcher never disagree.
fn probe_sglang_venv(version: &str) -> SglangVenvProbe {
    let python = match capsule::packers::runtime_fetcher::RuntimeFetcher::new() {
        Ok(fetcher) => {
            let runtime_dir = fetcher.get_runtime_path("sglang", version);
            capsule::packers::runtime_fetcher::sglang_venv_python(&runtime_dir)
        }
        Err(_) => return SglangVenvProbe::Missing,
    };
    if !python.is_file() {
        return SglangVenvProbe::Missing;
    }
    match Command::new(&python).args(["-c", "import sglang"]).output() {
        Ok(o) if o.status.success() => SglangVenvProbe::ImportOk,
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            SglangVenvProbe::ImportFailed(stderr.trim().lines().last().unwrap_or("").to_string())
        }
        Err(e) => SglangVenvProbe::ImportFailed(e.to_string()),
    }
}

fn print_doctor_table_cuda(profile: &HostGpuProfile, checks: &[CheckResult], ready: bool) {
    println!("ato runner doctor — SGLang (CUDA) host diagnostics");
    println!();
    println!(
        "  OS:       {} {} (kernel {})",
        profile.os.distro, profile.os.version, profile.os.kernel
    );
    if let Some(sb) = profile.secure_boot_enabled {
        println!("  Secure Boot: {}", if sb { "ENABLED" } else { "disabled" });
    }
    if let Some(cuda) = profile.cuda.as_ref() {
        println!("  CUDA API: {}", cuda.driver_api_version);
    }
    println!(
        "  GPUs:     {}",
        if profile.gpus.is_empty() {
            "none detected".into()
        } else {
            format!("{} device(s)", profile.gpus.len())
        }
    );
    for gpu in &profile.gpus {
        let vram_gb = gpu.vram_bytes / (1024 * 1024 * 1024);
        println!("           - {} ({} GB VRAM)", gpu.name, vram_gb);
    }
    println!();

    print_check_rows(checks);

    println!();
    if ready {
        println!("  ✓ Host is ready for SGLang (CUDA) LLM capsules.");
    } else {
        println!("  ✗ Host is NOT ready. Fix FAIL items above.");
        println!("    Next step: sudo ato runner provision --profile nvidia-cuda");
    }
}

// ─────────────────────────────────────────────
// Provision
// ─────────────────────────────────────────────

/// JSON event emitted during `--json` provision progress.
#[derive(Debug, Serialize)]
#[serde(tag = "phase")]
#[serde(rename_all = "snake_case")]
enum ProvisionEvent {
    Preflight {
        os: String,
        version: String,
        secure_boot: Option<bool>,
    },
    Driver {
        action: ProvisionAction,
        detail: String,
    },
    Vulkan {
        action: ProvisionAction,
        detail: String,
    },
    /// nvidia-cuda: the CUDA runtime check (driver-exposed CUDA driver-API).
    CudaRuntime {
        action: ProvisionAction,
        detail: String,
    },
    /// nvidia-cuda: the python3 + venv install (apt) for the managed sglang venv.
    Python {
        action: ProvisionAction,
        detail: String,
    },
    /// nvidia-cuda: building the managed sglang venv (uv venv + cu124 pip install).
    SglangVenv {
        action: ProvisionAction,
        detail: String,
    },
    SmokeTest {
        action: ProvisionAction,
        detail: String,
    },
    Receipt {
        action: ProvisionAction,
        detail: String,
    },
    RebootRequired {
        message: String,
    },
    Done {
        reboot_required: bool,
    },
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum ProvisionAction {
    Skip,
    Install,
    #[allow(dead_code)]
    Configure,
    Verify,
    #[allow(dead_code)]
    Fail,
    DryRun,
}

/// Run `ato runner provision`: install the NVIDIA driver + Vulkan runtime, then
/// a `vulkaninfo` smoke test (Dockerless — no Docker/Podman/toolkit).
///
/// Async because the optional `--enroll` path delegates to
/// [`runner_agent::run_login`](crate::application::runner_agent::run_login),
/// which is async. All blocking work (apt, modprobe) uses
/// `std::process::Command` directly — the caller drives this via
/// `tokio::runtime::Runtime::block_on` from the dispatch layer.
pub async fn run_provision(
    profile_name: &str,
    force: bool,
    resume: bool,
    enroll: Option<Option<String>>,
    json: bool,
    dry_run: bool,
) -> Result<()> {
    // ── Phase A: Preflight ──
    let pre = capsule::foundation::host_gpu::detect_host_gpu_profile()
        .context("Failed to detect host GPU profile")?;

    emit_event(
        json,
        &ProvisionEvent::Preflight {
            os: pre.os.distro.clone(),
            version: pre.os.version.clone(),
            secure_boot: pre.secure_boot_enabled,
        },
    );

    if !pre.os_supported() {
        bail!(
            "Unsupported OS: {} {}. ato runner provision supports Ubuntu 22.04 or 24.04 only.",
            pre.os.distro,
            pre.os.version
        );
    }

    let is_cuda = match profile_name {
        "nvidia-ubuntu" => false,
        "nvidia-cuda" => true,
        other => bail!(
            "Unknown profile: {other}. Supported: 'nvidia-ubuntu' (Vulkan / llama.cpp), \
             'nvidia-cuda' (SGLang)."
        ),
    };

    // Root check (skip only for --dry-run so users can preview without sudo).
    // --resume still needs root because it runs apt / systemctl / docker.
    if !dry_run && !is_root() {
        bail!("ato runner provision requires root. Run with: sudo ato runner provision");
    }

    // ── Resume: read marker and skip completed phases ──
    let marker = if resume {
        read_marker().context("No provision marker found — cannot resume. Run without --resume.")?
    } else {
        None
    };

    let mut warnings = Vec::new();
    let secure_boot = pre.secure_boot_enabled.unwrap_or(false);

    // ── Phase B: NVIDIA Driver ──
    let driver_was_installed = pre.driver_installed();
    let skip_driver = marker.is_some() || (driver_was_installed && !force);

    if skip_driver && !force {
        let ver = pre
            .driver
            .as_ref()
            .map(|d| d.version.as_str())
            .unwrap_or("unknown");
        emit_event(
            json,
            &ProvisionEvent::Driver {
                action: ProvisionAction::Skip,
                detail: format!("Driver already installed ({ver})"),
            },
        );
    } else {
        emit_event(
            json,
            &ProvisionEvent::Driver {
                action: if dry_run {
                    ProvisionAction::DryRun
                } else {
                    ProvisionAction::Install
                },
                detail: format!("apt-get install -y {NVIDIA_DRIVER_PACKAGE}"),
            },
        );

        if !dry_run {
            // Refresh package index before installing the driver metapackage.
            // On a bare Ubuntu the local apt cache may be stale or empty.
            run_apt(&["update"]).context("apt-get update failed before driver install")?;
            run_apt(&["install", "-y", NVIDIA_DRIVER_PACKAGE])
                .context("Failed to install NVIDIA driver")?;
        }

        // Secure Boot: warn about MOK enrollment and exit
        if secure_boot {
            let msg = "NVIDIA driver installed. Secure Boot is ON.\n\
                \n\
                A reboot is required. During boot, the MOK Manager will appear:\n\
                1. Select 'Enroll MOK'\n\
                2. Select 'Continue'\n\
                3. Select 'Yes' to enroll\n\
                4. Enter the password you set during install\n\
                5. Reboot\n\
                \n\
                After enrollment, run: sudo ato runner provision --resume";
            emit_event(
                json,
                &ProvisionEvent::RebootRequired {
                    message: msg.to_string(),
                },
            );
            if !dry_run {
                write_marker(&ProvisionMarker {
                    profile: profile_name.to_string(),
                    phase: ProvisionPhase::PostDriverInstall,
                    secure_boot_enabled: true,
                    timestamp_unix: now_unix(),
                })?;
            }
            if !json {
                println!("\n{msg}\n");
            }
            return Ok(());
        }

        // No Secure Boot: try to load the module without reboot
        if !dry_run {
            let modprobe = Command::new("modprobe").arg("nvidia").output();
            match modprobe {
                Ok(o) if o.status.success() => {
                    emit_event(
                        json,
                        &ProvisionEvent::Driver {
                            action: ProvisionAction::Verify,
                            detail: "nvidia module loaded without reboot".to_string(),
                        },
                    );
                }
                _ => {
                    // modprobe failed — reboot needed
                    let msg = "NVIDIA driver installed. A reboot is required for the kernel module to load.\n\
                        \n\
                        After reboot, run: sudo ato runner provision --resume";
                    emit_event(
                        json,
                        &ProvisionEvent::RebootRequired {
                            message: msg.to_string(),
                        },
                    );
                    write_marker(&ProvisionMarker {
                        profile: profile_name.to_string(),
                        phase: ProvisionPhase::RebootRequired,
                        secure_boot_enabled: false,
                        timestamp_unix: now_unix(),
                    })?;
                    if !json {
                        println!("\n{msg}\n");
                    }
                    return Ok(());
                }
            }
        }
    }

    // ── Re-detect after driver install (or resume) ──
    let post_driver = capsule::foundation::host_gpu::detect_host_gpu_profile()
        .context("Failed to re-detect host GPU profile after driver install")?;

    if !post_driver.driver_installed() && !dry_run {
        bail!(
            "NVIDIA driver does not appear to be loaded after install. \
             Try rebooting and running: sudo ato runner provision --resume"
        );
    }

    // ── nvidia-cuda profile: CUDA runtime + python/venv + sglang venv ──
    // The driver phase above (incl. the Secure-Boot/MOK handling) is shared with
    // the Vulkan profile verbatim; from here the CUDA profile installs python3 +
    // the managed sglang venv instead of the Vulkan loader/tools, then writes a
    // CUDA receipt and (optionally) enrolls. Returns early — the Vulkan phases
    // below are nvidia-ubuntu-only.
    if is_cuda {
        return provision_cuda_phases(
            profile_name,
            force,
            enroll,
            json,
            dry_run,
            secure_boot,
            &mut warnings,
        )
        .await;
    }

    // ── Phase C: Vulkan runtime (Dockerless GPU path) ──
    // No Docker, no nvidia-container-toolkit. We need BOTH the loader library
    // (`libvulkan1`) AND the `vulkaninfo` tool (`vulkan-tools`, used by the smoke).
    // Skipping purely on loader presence previously left `vulkaninfo` missing, so
    // the smoke failed with "No such file or directory". Skip only when both the
    // loader and the tool are already present.
    let skip_vulkan =
        post_driver.vulkan_loader_present() && post_driver.vulkaninfo_available() && !force;
    if skip_vulkan {
        emit_event(
            json,
            &ProvisionEvent::Vulkan {
                action: ProvisionAction::Skip,
                detail: "Vulkan loader + vulkaninfo already present".to_string(),
            },
        );
    } else {
        // Install only what is missing. `vulkan-tools` provides `vulkaninfo`;
        // `libvulkan1` provides the loader.
        let mut pkgs: Vec<&str> = Vec::new();
        if force || !post_driver.vulkaninfo_available() {
            pkgs.push("vulkan-tools");
        }
        if force || !post_driver.vulkan_loader_present() {
            pkgs.push("libvulkan1");
        }
        emit_event(
            json,
            &ProvisionEvent::Vulkan {
                action: if dry_run {
                    ProvisionAction::DryRun
                } else {
                    ProvisionAction::Install
                },
                detail: format!("apt-get install -y {}", pkgs.join(" ")),
            },
        );
        if !dry_run {
            run_apt(&["update"]).context("apt-get update failed before Vulkan install")?;
            let mut args = vec!["install", "-y"];
            args.extend_from_slice(&pkgs);
            run_apt(&args).context("Failed to install the Vulkan loader/tools")?;
            emit_event(
                json,
                &ProvisionEvent::Vulkan {
                    action: ProvisionAction::Verify,
                    detail: format!("{} installed", pkgs.join(" + ")),
                },
            );
        }
    }

    // ── Phase D: GPU smoke (Dockerless: vulkaninfo must see an NVIDIA device) ──
    let smoke_result = if dry_run {
        emit_event(
            json,
            &ProvisionEvent::SmokeTest {
                action: ProvisionAction::DryRun,
                detail: "vulkaninfo --summary (expect an NVIDIA device)".to_string(),
            },
        );
        SmokeResult::Skipped
    } else {
        emit_event(
            json,
            &ProvisionEvent::SmokeTest {
                action: ProvisionAction::Verify,
                detail: "vulkaninfo --summary".to_string(),
            },
        );
        run_vulkan_smoke_test(&mut warnings)
    };

    let smoke_gpu_count = if smoke_result == SmokeResult::Pass {
        // Re-probe the device count from nvidia-smi (the GPUs the engine will see).
        capsule::foundation::host_gpu::detect_host_gpu_profile()
            .ok()
            .map(|p| p.gpus.len())
    } else {
        None
    };

    // If the smoke did not pass and the host has a GPU + driver but no NVIDIA
    // Vulkan ICD manifest, surface a specific, actionable warning. We do NOT
    // mutate the driver here: the NVIDIA userspace libs are commonly bind-mounted
    // read-only (containers), so a full host driver install / Vulkan-capable image
    // is the correct fix rather than blind driver surgery.
    if !dry_run
        && smoke_result != SmokeResult::Pass
        && let Ok(p) = capsule::foundation::host_gpu::detect_host_gpu_profile()
        && p.has_gpu()
        && p.driver_installed()
        && !p.nvidia_vulkan_icd_present()
    {
        warnings.push(
            "NVIDIA Vulkan ICD manifest (nvidia_icd.json) not found: GPU + driver are \
                     present but the Vulkan ICD is missing. Install the NVIDIA driver's Vulkan \
                     userspace (e.g. libnvidia-gl-<branch>) on a bare-metal host, or use a \
                     Vulkan-capable image. Provision does not mutate driver libs (often \
                     bind-mounted read-only in containers)."
                .to_string(),
        );
    }

    // ── Dry-run: stop here without writing any state ──
    if dry_run {
        emit_event(
            json,
            &ProvisionEvent::Done {
                reboot_required: false,
            },
        );
        if !json {
            println!();
            println!("  Dry run complete — no changes were made.");
            println!("  Receipt and marker were not written.");
        }
        return Ok(());
    }

    // ── Phase F: Receipt ──
    let final_profile = capsule::foundation::host_gpu::detect_host_gpu_profile()
        .context("Failed to detect final host GPU profile")?;

    let receipt = ProvisionReceipt {
        timestamp_unix: now_unix(),
        profile: profile_name.to_string(),
        os: final_profile.os.clone(),
        kernel_version: final_profile.os.kernel.clone(),
        secure_boot_enabled: secure_boot,
        driver_version: final_profile.driver.as_ref().map(|d| d.version.clone()),
        cuda_driver_api_version: final_profile
            .cuda
            .as_ref()
            .map(|c| c.driver_api_version.clone()),
        gpu_count: final_profile.gpus.len(),
        gpu_devices: final_profile
            .gpus
            .iter()
            .map(GpuDeviceSummary::from)
            .collect(),
        vulkan_loader_present: final_profile.vulkan_loader_present(),
        vulkaninfo_available: final_profile.vulkaninfo_available(),
        nvidia_vulkan_icd_present: final_profile.nvidia_vulkan_icd_present(),
        vulkan_nvidia_device_visible: final_profile.vulkan_nvidia_device_visible(),
        gpu_smoke_result: smoke_result,
        smoke_gpu_count_detected: smoke_gpu_count,
        reboot_required: false,
        warnings: warnings.clone(),
        ato_cli_version: agent_version().to_string(),
        // CUDA / SGLang fields are not part of the Vulkan (nvidia-ubuntu) path.
        cuda_runtime_present: None,
        python3_version: None,
        sglang_version: None,
        sglang_import_ok: None,
        max_gpu_vram_bytes: None,
    };

    write_receipt(&receipt)?;
    clear_marker()?;

    emit_event(
        json,
        &ProvisionEvent::Receipt {
            action: ProvisionAction::Verify,
            detail: format!("Receipt written to {}", provision_receipt_path().display()),
        },
    );

    // ── Smoke test failure = non-zero exit (before enrollment) ──
    // The receipt is still written (for diagnostics), but the command
    // exits non-zero so scripts and CI can detect the incomplete state.
    // This check runs BEFORE enrollment so we never register an
    // incomplete GPU runner with the control plane.
    if smoke_result == SmokeResult::Fail {
        if !json {
            print_provision_summary(&receipt);
            println!();
            println!("  ✗ GPU smoke test FAILED — host is not fully ready.");
            println!("    Check the warnings above and re-run after fixing.");
        }
        emit_event(
            json,
            &ProvisionEvent::Done {
                reboot_required: false,
            },
        );
        std::process::exit(1);
    }

    // ── Phase G: Enrollment (optional) ──
    // Only reached when the smoke test passed (or was skipped in dry-run,
    // but dry-run returns early before this point).
    if let Some(enroll_display_name) = enroll {
        if !json {
            print_provision_summary(&receipt);
        }
        // Delegate to runner login
        crate::application::runner_agent::run_login(
            None,
            None,
            enroll_display_name,
            None,
            false,
            None,
        )
        .await
        .context("Enrollment (ato runner login) failed")?;
    } else {
        if !json {
            print_provision_summary(&receipt);
        }
    }

    emit_event(
        json,
        &ProvisionEvent::Done {
            reboot_required: false,
        },
    );

    Ok(())
}

/// The `nvidia-cuda` (SGLang) provision phases, run AFTER the shared driver phase
/// (so the driver + Secure-Boot/MOK handling in [`run_provision`] is reused
/// verbatim). Installs python3 + python3-venv (apt) and the managed sglang venv
/// (via the runtime fetcher's `ensure_sglang`, which does `uv venv` + the cu124
/// pip install + an `import sglang` smoke), then writes a CUDA receipt and
/// optionally enrolls. The venv install + import smoke are REAL commands that
/// only fully succeed on a real NVIDIA Ubuntu host (host-pending) — never stubbed.
#[allow(clippy::too_many_arguments)]
async fn provision_cuda_phases(
    profile_name: &str,
    force: bool,
    enroll: Option<Option<String>>,
    json: bool,
    dry_run: bool,
    secure_boot: bool,
    warnings: &mut Vec<String>,
) -> Result<()> {
    // ── Phase C: CUDA runtime check (the driver must expose CUDA ≥ cu124) ──
    // SGLang's torch wheels are pinned to cu124; an older CUDA driver-API means
    // the venv would import-fail. We do not (and cannot) install a newer driver
    // mid-run — surface it as a hard failure with the remediation.
    let cuda_profile = capsule::foundation::host_gpu::detect_host_gpu_profile()
        .context("Failed to detect host GPU profile for the CUDA runtime check")?;
    let cuda_version = cuda_profile
        .cuda
        .as_ref()
        .map(|c| c.driver_api_version.clone())
        .unwrap_or_default();
    let cuda_ok = parse_cuda_version(&cuda_version)
        .map(|(major, minor)| cuda_meets_floor(major, minor))
        .unwrap_or(false);
    emit_event(
        json,
        &ProvisionEvent::CudaRuntime {
            action: if dry_run {
                ProvisionAction::DryRun
            } else {
                ProvisionAction::Verify
            },
            detail: if cuda_version.is_empty() {
                "nvidia-smi reports no CUDA driver API".to_string()
            } else {
                format!("CUDA driver API {cuda_version}")
            },
        },
    );
    if !dry_run && !cuda_ok {
        bail!(
            "CUDA driver API {} does not satisfy the cu124 sglang pin (needs ≥ {}.{}). \
             Upgrade the NVIDIA driver to an R550-era (CUDA 12.4+) branch, then re-run \
             with --force.",
            if cuda_version.is_empty() {
                "<none>"
            } else {
                &cuda_version
            },
            SGLANG_MIN_CUDA_MAJOR,
            SGLANG_MIN_CUDA_MINOR
        );
    }

    // ── Phase D: python3 + python3-venv (the managed sglang venv host) ──
    let skip_python = cuda_profile
        .cuda_runtime
        .as_ref()
        .map(|c| c.python3_ok && c.venv_module_ok)
        .unwrap_or(false)
        && !force;
    if skip_python {
        emit_event(
            json,
            &ProvisionEvent::Python {
                action: ProvisionAction::Skip,
                detail: "python3 + venv already present".to_string(),
            },
        );
    } else {
        emit_event(
            json,
            &ProvisionEvent::Python {
                action: if dry_run {
                    ProvisionAction::DryRun
                } else {
                    ProvisionAction::Install
                },
                detail: format!("apt-get install -y {}", CUDA_PYTHON_PACKAGES.join(" ")),
            },
        );
        if !dry_run {
            run_apt(&["update"]).context("apt-get update failed before python install")?;
            let mut args = vec!["install", "-y"];
            args.extend_from_slice(CUDA_PYTHON_PACKAGES);
            run_apt(&args).context("Failed to install python3 / python3-venv")?;
            emit_event(
                json,
                &ProvisionEvent::Python {
                    action: ProvisionAction::Verify,
                    detail: format!("{} installed", CUDA_PYTHON_PACKAGES.join(" + ")),
                },
            );
        }
    }

    // ── Phase E: the managed sglang venv (uv venv + cu124 pip + import smoke) ──
    // `ensure_sglang` is the real fetch: it creates the venv, pip-installs the
    // pinned torch(cu124)/sglang/kernels, and runs `import sglang` as a
    // post-condition. The import only passes when the CUDA kernels load on a real
    // GPU host — so this is the CUDA "GPU smoke" for the SGLang path.
    let mut sglang_import_ok = false;
    let smoke_result = if dry_run {
        emit_event(
            json,
            &ProvisionEvent::SglangVenv {
                action: ProvisionAction::DryRun,
                detail: format!(
                    "uv venv + pip install (cu124) sglang=={SGLANG_REFERENCE_WHEEL}, then `import sglang`"
                ),
            },
        );
        SmokeResult::Skipped
    } else {
        emit_event(
            json,
            &ProvisionEvent::SglangVenv {
                action: ProvisionAction::Install,
                detail: format!("building managed sglang {SGLANG_REFERENCE_WHEEL} venv (cu124)"),
            },
        );
        let fetcher =
            capsule::packers::runtime_fetcher::RuntimeFetcher::new().map_err(|err| {
                anyhow::anyhow!("failed to init the toolchain cache for the sglang venv: {err}")
            })?;
        match fetcher.ensure_sglang(SGLANG_REFERENCE_WHEEL).await {
            Ok(python) => {
                sglang_import_ok = true;
                emit_event(
                    json,
                    &ProvisionEvent::SglangVenv {
                        action: ProvisionAction::Verify,
                        detail: format!("sglang venv ready at {} (`import sglang` ok)", python.display()),
                    },
                );
                emit_event(
                    json,
                    &ProvisionEvent::SmokeTest {
                        action: ProvisionAction::Verify,
                        detail: "import sglang succeeded in the managed venv".to_string(),
                    },
                );
                SmokeResult::Pass
            }
            Err(err) => {
                // Honest failure: the venv built but the CUDA import failed, or
                // the install itself failed. Record it as a smoke FAIL + warning;
                // the receipt is still written for diagnostics (mirrors Vulkan).
                warnings.push(format!("sglang venv / `import sglang` failed: {err}"));
                emit_event(
                    json,
                    &ProvisionEvent::SmokeTest {
                        action: ProvisionAction::Fail,
                        detail: format!("import sglang failed: {err}"),
                    },
                );
                SmokeResult::Fail
            }
        }
    };

    // ── Dry-run: stop here without writing any state (mirrors run_provision) ──
    if dry_run {
        emit_event(
            json,
            &ProvisionEvent::Done {
                reboot_required: false,
            },
        );
        if !json {
            println!();
            println!("  Dry run complete — no changes were made.");
            println!("  Receipt and marker were not written.");
        }
        return Ok(());
    }

    // ── Phase F: Receipt (CUDA fields populated; Vulkan fields reflect probe) ──
    let final_profile = capsule::foundation::host_gpu::detect_host_gpu_profile()
        .context("Failed to detect final host GPU profile")?;

    let smoke_gpu_count = if smoke_result == SmokeResult::Pass {
        Some(final_profile.gpus.len())
    } else {
        None
    };

    let receipt = ProvisionReceipt {
        timestamp_unix: now_unix(),
        profile: profile_name.to_string(),
        os: final_profile.os.clone(),
        kernel_version: final_profile.os.kernel.clone(),
        secure_boot_enabled: secure_boot,
        driver_version: final_profile.driver.as_ref().map(|d| d.version.clone()),
        cuda_driver_api_version: final_profile
            .cuda
            .as_ref()
            .map(|c| c.driver_api_version.clone()),
        gpu_count: final_profile.gpus.len(),
        gpu_devices: final_profile
            .gpus
            .iter()
            .map(GpuDeviceSummary::from)
            .collect(),
        // Vulkan fields: not part of the CUDA path — record the probe as-is
        // (false unless the host happens to also have Vulkan installed).
        vulkan_loader_present: final_profile.vulkan_loader_present(),
        vulkaninfo_available: final_profile.vulkaninfo_available(),
        nvidia_vulkan_icd_present: final_profile.nvidia_vulkan_icd_present(),
        vulkan_nvidia_device_visible: final_profile.vulkan_nvidia_device_visible(),
        gpu_smoke_result: smoke_result,
        smoke_gpu_count_detected: smoke_gpu_count,
        reboot_required: false,
        warnings: warnings.clone(),
        ato_cli_version: agent_version().to_string(),
        // ── CUDA / SGLang receipt fields ──
        cuda_runtime_present: Some(
            final_profile
                .cuda
                .as_ref()
                .map(|c| !c.driver_api_version.trim().is_empty())
                .unwrap_or(false),
        ),
        python3_version: detect_python3_version(),
        sglang_version: Some(SGLANG_REFERENCE_WHEEL.to_string()),
        sglang_import_ok: Some(sglang_import_ok),
        max_gpu_vram_bytes: Some(final_profile.max_gpu_vram_bytes()),
    };

    write_receipt(&receipt)?;
    clear_marker()?;

    emit_event(
        json,
        &ProvisionEvent::Receipt {
            action: ProvisionAction::Verify,
            detail: format!("Receipt written to {}", provision_receipt_path().display()),
        },
    );

    // ── Smoke failure = non-zero exit BEFORE enrollment (mirrors Vulkan) ──
    if smoke_result == SmokeResult::Fail {
        if !json {
            print_provision_summary(&receipt);
            println!();
            println!("  ✗ SGLang venv / `import sglang` FAILED — host is not fully ready.");
            println!("    Check the warnings above and re-run after fixing.");
        }
        emit_event(
            json,
            &ProvisionEvent::Done {
                reboot_required: false,
            },
        );
        std::process::exit(1);
    }

    // ── Phase G: Enrollment (optional) ──
    if let Some(enroll_display_name) = enroll {
        if !json {
            print_provision_summary(&receipt);
        }
        crate::application::runner_agent::run_login(
            None,
            None,
            enroll_display_name,
            None,
            false,
            None,
        )
        .await
        .context("Enrollment (ato runner login) failed")?;
    } else if !json {
        print_provision_summary(&receipt);
    }

    emit_event(
        json,
        &ProvisionEvent::Done {
            reboot_required: false,
        },
    );

    Ok(())
}

fn print_provision_summary(receipt: &ProvisionReceipt) {
    println!();
    println!("ato runner provision — complete");
    println!();
    println!(
        "  OS:          {} {} (kernel {})",
        receipt.os.distro, receipt.os.version, receipt.kernel_version
    );
    println!(
        "  Driver:      {}",
        receipt.driver_version.as_deref().unwrap_or("not detected")
    );
    println!(
        "  CUDA API:    {}",
        receipt
            .cuda_driver_api_version
            .as_deref()
            .unwrap_or("not detected")
    );
    println!("  GPUs:        {} device(s)", receipt.gpu_count);
    for gpu in &receipt.gpu_devices {
        let gb = gpu.vram_bytes / (1024 * 1024 * 1024);
        println!("               - {} ({} GB)", gpu.name, gb);
    }
    // CUDA (sglang) receipts carry `sglang_version`; print their rows instead of
    // the Vulkan runtime summary so the output never claims the wrong path.
    let is_cuda_receipt = receipt.sglang_version.is_some();
    if is_cuda_receipt {
        println!(
            "  Python:      {}",
            receipt.python3_version.as_deref().unwrap_or("not detected")
        );
        println!(
            "  SGLang:      {} (import {})",
            receipt.sglang_version.as_deref().unwrap_or("?"),
            match receipt.sglang_import_ok {
                Some(true) => "ok",
                Some(false) => "FAILED",
                None => "unknown",
            }
        );
    } else {
        println!(
            "  Vulkan:      loader {}, vulkaninfo {}, NVIDIA ICD {}, device {}",
            if receipt.vulkan_loader_present {
                "present"
            } else {
                "missing"
            },
            if receipt.vulkaninfo_available {
                "present"
            } else {
                "missing"
            },
            if receipt.nvidia_vulkan_icd_present {
                "present"
            } else {
                "missing"
            },
            if receipt.vulkan_nvidia_device_visible {
                "visible"
            } else {
                "not visible"
            }
        );
    }
    let smoke = match receipt.gpu_smoke_result {
        SmokeResult::Pass if is_cuda_receipt => "PASS (import sglang in the managed venv)".to_string(),
        SmokeResult::Pass => format!(
            "PASS ({} GPUs via vulkaninfo/nvidia-smi)",
            receipt.smoke_gpu_count_detected.unwrap_or(0)
        ),
        SmokeResult::Fail => "FAIL".to_string(),
        SmokeResult::Skipped => "SKIPPED".to_string(),
    };
    println!("  Smoke test:  {smoke}");
    if !receipt.warnings.is_empty() {
        println!();
        println!("  Warnings:");
        for w in &receipt.warnings {
            println!("    - {w}");
        }
    }
    println!();
    println!("  Receipt: {}", provision_receipt_path().display());
    println!();
    println!("  Next: enroll as a Connected Runner with: ato runner login");
    println!("        or start serving:                   ato runner serve");
}

// ─────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────

fn is_root() -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn agent_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn run_apt(args: &[&str]) -> Result<()> {
    let mut cmd = Command::new("apt-get");
    cmd.args(args);
    let status = cmd
        .status()
        .with_context(|| format!("Failed to execute apt-get {:?}", args))?;
    if !status.success() {
        bail!("apt-get {:?} exited with status {}", args, status);
    }
    Ok(())
}

/// Capture `python3 --version` (e.g. `"Python 3.12.3"`) for the CUDA receipt.
/// Returns `None` when python3 is absent or the call fails. python prints the
/// version to stdout (3.4+); older builds used stderr, so fall back to it.
fn detect_python3_version() -> Option<String> {
    let output = Command::new("python3").arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = if stdout.trim().is_empty() {
        stderr.trim()
    } else {
        stdout.trim()
    };
    if combined.is_empty() {
        None
    } else {
        Some(combined.to_string())
    }
}

fn run_vulkan_smoke_test(warnings: &mut Vec<String>) -> SmokeResult {
    let output = Command::new("vulkaninfo").arg("--summary").output();
    match output {
        Ok(o)
            if o.status.success()
                && String::from_utf8_lossy(&o.stdout)
                    .to_lowercase()
                    .contains("nvidia") =>
        {
            SmokeResult::Pass
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            warnings.push(format!(
                "Vulkan smoke test found no NVIDIA device: {}",
                stderr.trim()
            ));
            SmokeResult::Fail
        }
        Err(e) => {
            warnings.push(format!("Failed to run vulkaninfo: {e}"));
            SmokeResult::Fail
        }
    }
}

fn emit_event(json: bool, event: &ProvisionEvent) {
    if json {
        if let Ok(line) = serde_json::to_string(event) {
            println!("{line}");
        }
    } else {
        // Human-readable progress
        let (phase, action, detail) = match event {
            ProvisionEvent::Preflight { os, version, .. } => {
                ("preflight", "check", format!("{os} {version}"))
            }
            ProvisionEvent::Driver { action, detail } => {
                ("driver", action_str(*action), detail.clone())
            }
            ProvisionEvent::Vulkan { action, detail } => {
                ("vulkan", action_str(*action), detail.clone())
            }
            ProvisionEvent::CudaRuntime { action, detail } => {
                ("cuda_runtime", action_str(*action), detail.clone())
            }
            ProvisionEvent::Python { action, detail } => {
                ("python", action_str(*action), detail.clone())
            }
            ProvisionEvent::SglangVenv { action, detail } => {
                ("sglang_venv", action_str(*action), detail.clone())
            }
            ProvisionEvent::SmokeTest { action, detail } => {
                ("smoke", action_str(*action), detail.clone())
            }
            ProvisionEvent::Receipt { action, detail } => {
                ("receipt", action_str(*action), detail.clone())
            }
            ProvisionEvent::RebootRequired { message } => ("reboot", "required", message.clone()),
            ProvisionEvent::Done { .. } => ("done", "complete", String::new()),
        };
        if !detail.is_empty() {
            println!("  [{phase}/{action}] {detail}");
        } else {
            println!("  [{phase}/{action}]");
        }
    }
}

fn action_str(a: ProvisionAction) -> &'static str {
    match a {
        ProvisionAction::Skip => "skip",
        ProvisionAction::Install => "install",
        ProvisionAction::Configure => "configure",
        ProvisionAction::Verify => "verify",
        ProvisionAction::Fail => "fail",
        ProvisionAction::DryRun => "dry-run",
    }
}

// ─────────────────────────────────────────────
// Receipt / Marker I/O
// ─────────────────────────────────────────────

fn write_receipt(receipt: &ProvisionReceipt) -> Result<()> {
    let path = provision_receipt_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
    }
    let json = serde_json::to_string_pretty(receipt)?;
    std::fs::write(&path, json).with_context(|| format!("failed to write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to set 0600 on {}", path.display()))?;
    }
    Ok(())
}

fn write_marker(marker: &ProvisionMarker) -> Result<()> {
    let path = provision_marker_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(marker)?;
    std::fs::write(&path, json).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn read_marker() -> Result<Option<ProvisionMarker>> {
    let path = provision_marker_path();
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let marker: ProvisionMarker = serde_json::from_str(&raw)
        .with_context(|| format!("invalid marker at {}", path.display()))?;
    Ok(Some(marker))
}

fn clear_marker() -> Result<()> {
    let path = provision_marker_path();
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

// ─────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use capsule::foundation::host_gpu::{
        CudaInfo, CudaRuntimeInfo, DriverInfo, GpuDevice, OsInfo, VulkanInfo,
    };

    fn test_profile() -> HostGpuProfile {
        HostGpuProfile {
            os: OsInfo {
                distro: "ubuntu".to_string(),
                version: "22.04".to_string(),
                kernel: "5.15.0-91-generic".to_string(),
            },
            secure_boot_enabled: Some(false),
            gpus: vec![GpuDevice {
                index: 0,
                name: "NVIDIA GeForce RTX 3060".to_string(),
                uuid: Some("GPU-1234".to_string()),
                vram_bytes: 12 * 1024 * 1024 * 1024,
                pcie_bus_id: None,
            }],
            driver: Some(DriverInfo {
                version: "575.57.08".to_string(),
                nvidia_smi_available: true,
            }),
            cuda: None,
            vulkan: Some(VulkanInfo {
                loader_present: true,
                vulkaninfo_available: true,
                nvidia_icd_present: true,
                nvidia_device_visible: true,
            }),
            cuda_runtime: None,
        }
    }

    #[test]
    fn diagnose_returns_all_ok_for_fully_provisioned_host() {
        let profile = test_profile();
        let checks = diagnose(&profile);
        assert!(
            checks
                .iter()
                .all(|c| c.status == CheckStatus::Ok || c.status == CheckStatus::Na)
        );
        // secure_boot is Some(false) → OK, not NA
        let sb = checks.iter().find(|c| c.name == "secure_boot").unwrap();
        assert_eq!(sb.status, CheckStatus::Ok);
    }

    #[test]
    fn diagnose_fails_when_os_unsupported() {
        let mut profile = test_profile();
        profile.os.distro = "debian".to_string();
        let checks = diagnose(&profile);
        let os_check = checks.iter().find(|c| c.name == "os").unwrap();
        assert_eq!(os_check.status, CheckStatus::Fail);
    }

    #[test]
    fn diagnose_fails_when_no_gpu() {
        let mut profile = test_profile();
        profile.gpus.clear();
        let checks = diagnose(&profile);
        let gpu_check = checks.iter().find(|c| c.name == "gpu").unwrap();
        assert_eq!(gpu_check.status, CheckStatus::Fail);
    }

    #[test]
    fn diagnose_fails_when_no_driver() {
        let mut profile = test_profile();
        profile.driver = None;
        let checks = diagnose(&profile);
        let drv_check = checks.iter().find(|c| c.name == "nvidia_driver").unwrap();
        assert_eq!(drv_check.status, CheckStatus::Fail);
    }

    #[test]
    fn diagnose_fails_when_no_vulkan_loader() {
        let mut profile = test_profile();
        profile.vulkan = None;
        let checks = diagnose(&profile);
        let vk = checks.iter().find(|c| c.name == "vulkan_loader").unwrap();
        assert_eq!(vk.status, CheckStatus::Fail);
        // And the overall readiness gate fails closed.
        let ready = checks
            .iter()
            .find(|c| c.name == "native_inference_vulkan_ready")
            .unwrap();
        assert_eq!(ready.status, CheckStatus::Fail);
    }

    #[test]
    fn diagnose_fails_when_no_vulkan_device() {
        let mut profile = test_profile();
        profile.vulkan = Some(VulkanInfo {
            loader_present: true,
            vulkaninfo_available: true,
            nvidia_icd_present: true,
            nvidia_device_visible: false,
        });
        let checks = diagnose(&profile);
        let dev = checks
            .iter()
            .find(|c| c.name == "vulkan_nvidia_device")
            .unwrap();
        assert_eq!(dev.status, CheckStatus::Fail);
    }

    #[test]
    fn diagnose_emits_vulkaninfo_and_icd_checks() {
        let checks = diagnose(&test_profile());
        for name in ["vulkaninfo", "nvidia_vulkan_icd"] {
            assert!(
                checks.iter().any(|c| c.name == name),
                "doctor must emit the `{name}` check"
            );
        }
    }

    #[test]
    fn diagnose_loader_present_but_vulkaninfo_missing_is_not_ready() {
        // The original bug: libvulkan present (loader ok) but `vulkaninfo` (the
        // smoke tool) missing must NOT report ready.
        let mut profile = test_profile();
        profile.vulkan = Some(VulkanInfo {
            loader_present: true,
            vulkaninfo_available: false,
            nvidia_icd_present: true,
            nvidia_device_visible: false,
        });
        let checks = diagnose(&profile);
        let vi = checks.iter().find(|c| c.name == "vulkaninfo").unwrap();
        assert_eq!(vi.status, CheckStatus::Fail);
        let ready = checks
            .iter()
            .find(|c| c.name == "native_inference_vulkan_ready")
            .unwrap();
        assert_eq!(ready.status, CheckStatus::Fail);
    }

    #[test]
    fn diagnose_fails_when_nvidia_icd_missing() {
        let mut profile = test_profile();
        profile.vulkan = Some(VulkanInfo {
            loader_present: true,
            vulkaninfo_available: true,
            nvidia_icd_present: false,
            nvidia_device_visible: false,
        });
        let checks = diagnose(&profile);
        let icd = checks
            .iter()
            .find(|c| c.name == "nvidia_vulkan_icd")
            .unwrap();
        assert_eq!(icd.status, CheckStatus::Fail);
    }

    #[test]
    fn diagnose_has_no_docker_or_toolkit_checks() {
        let checks = diagnose(&test_profile());
        assert!(
            !checks
                .iter()
                .any(|c| c.name == "docker" || c.name == "nvidia_container_toolkit"),
            "Dockerless doctor must not emit docker/toolkit checks"
        );
    }

    #[test]
    fn diagnose_warns_when_secure_boot_on() {
        let mut profile = test_profile();
        profile.secure_boot_enabled = Some(true);
        let checks = diagnose(&profile);
        let sb = checks.iter().find(|c| c.name == "secure_boot").unwrap();
        assert_eq!(sb.status, CheckStatus::Warn);
        assert!(sb.recommendation.is_some());
    }

    #[test]
    fn diagnose_ready_when_gpu_driver_and_vulkan_device_present() {
        let checks = diagnose(&test_profile());
        let ready = checks
            .iter()
            .find(|c| c.name == "native_inference_vulkan_ready")
            .unwrap();
        assert_eq!(ready.status, CheckStatus::Ok);
    }

    #[test]
    fn provision_event_serializes_with_phase_tag() {
        let event = ProvisionEvent::Driver {
            action: ProvisionAction::Install,
            detail: "apt-get install -y nvidia-driver-575".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"phase\":\"driver\""));
        assert!(json.contains("\"action\":\"install\""));
    }

    #[test]
    fn check_status_serializes_as_snake_case() {
        assert_eq!(serde_json::to_string(&CheckStatus::Ok).unwrap(), "\"ok\"");
        assert_eq!(
            serde_json::to_string(&CheckStatus::Fail).unwrap(),
            "\"fail\""
        );
    }

    // ── nvidia-cuda (SGLang) doctor ──

    /// A host that satisfies the CUDA host-readiness floor: NVIDIA GPU + driver +
    /// CUDA driver-API 12.4 + python3/venv. (`sglang_venv` is probed live, so the
    /// "fully ready" verdict depends on whether a managed venv exists — these
    /// fixtures only assert the host-floor rows, never `native_inference_cuda_ready`
    /// being the *only* OK row.)
    fn cuda_profile() -> HostGpuProfile {
        HostGpuProfile {
            os: OsInfo {
                distro: "ubuntu".to_string(),
                version: "24.04".to_string(),
                kernel: "6.8.0-31-generic".to_string(),
            },
            secure_boot_enabled: Some(false),
            gpus: vec![GpuDevice {
                index: 0,
                name: "NVIDIA RTX A6000".to_string(),
                uuid: Some("GPU-a6000".to_string()),
                vram_bytes: 48 * 1024 * 1024 * 1024,
                pcie_bus_id: None,
            }],
            driver: Some(DriverInfo {
                version: "570.124.06".to_string(),
                nvidia_smi_available: true,
            }),
            cuda: Some(CudaInfo {
                driver_api_version: "12.4".to_string(),
                toolkit_version: None,
            }),
            vulkan: None,
            cuda_runtime: Some(CudaRuntimeInfo {
                cuda_runtime_present: true,
                python3_ok: true,
                venv_module_ok: true,
                max_gpu_vram_bytes: 48 * 1024 * 1024 * 1024,
            }),
        }
    }

    fn cuda_check<'a>(checks: &'a [CheckResult], name: &str) -> &'a CheckResult {
        checks
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("nvidia-cuda doctor must emit the `{name}` check"))
    }

    #[test]
    fn parse_cuda_version_handles_major_minor_and_bare_major() {
        assert_eq!(parse_cuda_version("12.4"), Some((12, 4)));
        assert_eq!(parse_cuda_version(" 12.6 "), Some((12, 6)));
        assert_eq!(parse_cuda_version("13"), Some((13, 0)));
        assert_eq!(parse_cuda_version(""), None);
        assert_eq!(parse_cuda_version("n/a"), None);
    }

    #[test]
    fn cuda_meets_floor_enforces_cu124() {
        assert!(cuda_meets_floor(12, 4));
        assert!(cuda_meets_floor(12, 6));
        assert!(cuda_meets_floor(13, 0));
        assert!(!cuda_meets_floor(12, 3));
        assert!(!cuda_meets_floor(11, 8));
    }

    #[test]
    fn diagnose_cuda_emits_the_core_cuda_rows() {
        let checks = diagnose_cuda(&cuda_profile());
        for name in [
            "os",
            "secure_boot",
            "gpu",
            "nvidia_driver",
            "cuda_runtime",
            "python3",
            "python_venv",
            "sglang_venv",
            "gpu_vram",
            "native_inference_cuda_ready",
        ] {
            cuda_check(&checks, name);
        }
    }

    #[test]
    fn diagnose_cuda_has_no_docker_toolkit_or_vulkan_rows() {
        let checks = diagnose_cuda(&cuda_profile());
        for banned in [
            "docker",
            "nvidia_container_toolkit",
            "vulkan_loader",
            "vulkaninfo",
            "nvidia_vulkan_icd",
        ] {
            assert!(
                !checks.iter().any(|c| c.name == banned),
                "Dockerless CUDA doctor must not emit the `{banned}` row"
            );
        }
    }

    #[test]
    fn diagnose_cuda_ok_host_passes_the_floor_rows() {
        let checks = diagnose_cuda(&cuda_profile());
        for name in ["gpu", "nvidia_driver", "cuda_runtime", "python3", "python_venv"] {
            assert_eq!(
                cuda_check(&checks, name).status,
                CheckStatus::Ok,
                "`{name}` should be OK on a CUDA-ready host"
            );
        }
        // The host-readiness floor predicate is satisfied.
        assert_eq!(
            cuda_check(&checks, "native_inference_cuda_ready").status,
            CheckStatus::Ok
        );
    }

    #[test]
    fn diagnose_cuda_fails_when_no_gpu() {
        let mut profile = cuda_profile();
        profile.gpus.clear();
        let checks = diagnose_cuda(&profile);
        assert_eq!(cuda_check(&checks, "gpu").status, CheckStatus::Fail);
        assert_eq!(
            cuda_check(&checks, "native_inference_cuda_ready").status,
            CheckStatus::Fail
        );
    }

    #[test]
    fn diagnose_cuda_fails_when_no_driver() {
        let mut profile = cuda_profile();
        profile.driver = None;
        let checks = diagnose_cuda(&profile);
        assert_eq!(cuda_check(&checks, "nvidia_driver").status, CheckStatus::Fail);
    }

    #[test]
    fn diagnose_cuda_fails_when_cuda_older_than_cu124() {
        let mut profile = cuda_profile();
        profile.cuda = Some(CudaInfo {
            driver_api_version: "12.2".to_string(),
            toolkit_version: None,
        });
        let checks = diagnose_cuda(&profile);
        let cuda = cuda_check(&checks, "cuda_runtime");
        assert_eq!(cuda.status, CheckStatus::Fail);
        assert!(cuda.detail.contains("12.2"));
    }

    #[test]
    fn diagnose_cuda_fails_when_no_cuda_runtime() {
        let mut profile = cuda_profile();
        profile.cuda = None;
        let checks = diagnose_cuda(&profile);
        assert_eq!(cuda_check(&checks, "cuda_runtime").status, CheckStatus::Fail);
    }

    #[test]
    fn diagnose_cuda_fails_when_python_or_venv_missing() {
        let mut profile = cuda_profile();
        profile.cuda_runtime = Some(CudaRuntimeInfo {
            cuda_runtime_present: true,
            python3_ok: false,
            venv_module_ok: false,
            max_gpu_vram_bytes: 48 * 1024 * 1024 * 1024,
        });
        let checks = diagnose_cuda(&profile);
        assert_eq!(cuda_check(&checks, "python3").status, CheckStatus::Fail);
        assert_eq!(cuda_check(&checks, "python_venv").status, CheckStatus::Fail);
        assert_eq!(
            cuda_check(&checks, "native_inference_cuda_ready").status,
            CheckStatus::Fail
        );
    }

    #[test]
    fn diagnose_cuda_warns_on_low_vram() {
        let mut profile = cuda_profile();
        profile.gpus = vec![GpuDevice {
            index: 0,
            name: "NVIDIA GeForce RTX 3060".to_string(),
            uuid: None,
            vram_bytes: 12 * 1024 * 1024 * 1024,
            pcie_bus_id: None,
        }];
        let checks = diagnose_cuda(&profile);
        // Low VRAM is a WARN, never a hard FAIL.
        assert_eq!(cuda_check(&checks, "gpu_vram").status, CheckStatus::Warn);
    }

    #[test]
    fn diagnose_cuda_warns_when_secure_boot_on() {
        let mut profile = cuda_profile();
        profile.secure_boot_enabled = Some(true);
        let checks = diagnose_cuda(&profile);
        let sb = cuda_check(&checks, "secure_boot");
        assert_eq!(sb.status, CheckStatus::Warn);
        assert!(sb.recommendation.is_some());
    }

    #[test]
    fn run_doctor_for_profile_rejects_unknown_profile() {
        let err = run_doctor_for_profile("nvidia-amd", false).unwrap_err();
        assert!(err.to_string().contains("Unknown doctor profile"));
    }

    #[test]
    fn cuda_provision_events_serialize_with_phase_tags() {
        for (event, expected_phase) in [
            (
                ProvisionEvent::CudaRuntime {
                    action: ProvisionAction::Verify,
                    detail: "CUDA driver API 12.4".to_string(),
                },
                "cuda_runtime",
            ),
            (
                ProvisionEvent::Python {
                    action: ProvisionAction::Install,
                    detail: "apt-get install -y python3 python3-venv".to_string(),
                },
                "python",
            ),
            (
                ProvisionEvent::SglangVenv {
                    action: ProvisionAction::Install,
                    detail: "building managed sglang 0.4.10.post2 venv".to_string(),
                },
                "sglang_venv",
            ),
        ] {
            let json = serde_json::to_string(&event).unwrap();
            assert!(
                json.contains(&format!("\"phase\":\"{expected_phase}\"")),
                "expected phase {expected_phase} in {json}"
            );
        }
    }
}
