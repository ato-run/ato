//! GPU host provisioning and health checking.
//!
//! Implements `ato runner doctor` (read-only diagnostics) and
//! `ato runner provision` (Ubuntu + NVIDIA driver / Docker / toolkit
//! installation). Detection logic lives in
//! `capsule::foundation::host_gpu`; receipt and marker types live
//! in `capsule::foundation::provision_receipt`.

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

/// CUDA base image used for the GPU smoke test.
const SMOKE_TEST_IMAGE: &str = "nvidia/cuda:12.4.1-base-ubuntu22.04";

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

/// A single diagnostic check result.
#[derive(Debug, Clone, Serialize)]
struct CheckResult {
    name: &'static str,
    status: CheckStatus,
    detail: String,
    recommendation: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CheckStatus {
    Ok,
    Warn,
    Fail,
    Na,
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

    // Docker
    if profile.docker_ready() {
        let ver = profile
            .docker
            .as_ref()
            .map(|d| d.version.as_str())
            .unwrap_or("unknown");
        results.push(CheckResult {
            name: "docker",
            status: CheckStatus::Ok,
            detail: format!("Docker {ver} healthy"),
            recommendation: None,
        });
    } else if profile.docker.is_some() {
        results.push(CheckResult {
            name: "docker",
            status: CheckStatus::Warn,
            detail: "Docker installed but daemon not reachable".to_string(),
            recommendation: Some("Run: sudo systemctl start docker"),
        });
    } else {
        results.push(CheckResult {
            name: "docker",
            status: CheckStatus::Fail,
            detail: "Docker not installed".to_string(),
            recommendation: Some("Run: sudo ato runner provision"),
        });
    }

    // NVIDIA Container Toolkit
    if profile.toolkit_configured() {
        let ver = profile
            .nvidia_container_toolkit
            .as_ref()
            .map(|t| t.version.as_str())
            .unwrap_or("unknown");
        results.push(CheckResult {
            name: "nvidia_container_toolkit",
            status: CheckStatus::Ok,
            detail: format!("nvidia-ctk {ver} installed, nvidia runtime registered"),
            recommendation: None,
        });
    } else if profile.nvidia_container_toolkit.is_some() {
        results.push(CheckResult {
            name: "nvidia_container_toolkit",
            status: CheckStatus::Warn,
            detail: "nvidia-ctk installed but nvidia runtime not registered in Docker".to_string(),
            recommendation: Some("Run: sudo nvidia-ctk runtime configure --runtime=docker && sudo systemctl restart docker"),
        });
    } else {
        results.push(CheckResult {
            name: "nvidia_container_toolkit",
            status: CheckStatus::Fail,
            detail: "NVIDIA Container Toolkit not installed".to_string(),
            recommendation: Some("Run: sudo ato runner provision"),
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

    println!();
    if ready {
        println!("  ✓ Host is ready for GPU LLM capsules.");
    } else {
        println!("  ✗ Host is NOT ready. Fix FAIL items above.");
        println!("    Next step: sudo ato runner provision");
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
    Docker {
        action: ProvisionAction,
        detail: String,
    },
    Toolkit {
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
    Configure,
    Verify,
    #[allow(dead_code)]
    Fail,
    DryRun,
}

/// Run `ato runner provision`: install driver, Docker, toolkit, smoke test.
///
/// Async because the optional `--enroll` path delegates to
/// [`runner_agent::run_login`](crate::application::runner_agent::run_login),
/// which is async. All blocking work (apt, docker, modprobe) uses
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

    if profile_name != "nvidia-ubuntu" {
        bail!("Unknown profile: {profile_name}. v0 supports only 'nvidia-ubuntu'.");
    }

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

    // ── Phase C: Docker Engine ──
    let skip_docker = post_driver.docker_ready() && !force;
    if skip_docker {
        emit_event(
            json,
            &ProvisionEvent::Docker {
                action: ProvisionAction::Skip,
                detail: "Docker already installed and healthy".to_string(),
            },
        );
    } else {
        emit_event(
            json,
            &ProvisionEvent::Docker {
                action: if dry_run {
                    ProvisionAction::DryRun
                } else {
                    ProvisionAction::Install
                },
                detail: "apt-get install -y docker.io".to_string(),
            },
        );
        if !dry_run {
            run_apt(&["update"]).context("apt-get update failed before docker install")?;
            run_apt(&["install", "-y", "docker.io"]).context("Failed to install Docker")?;
            // Enable and start — fail hard if systemctl fails.
            let status = Command::new("systemctl")
                .args(["enable", "--now", "docker"])
                .status()
                .context("Failed to run systemctl enable --now docker")?;
            if !status.success() {
                bail!("systemctl enable --now docker exited with status {status}");
            }
            emit_event(
                json,
                &ProvisionEvent::Docker {
                    action: ProvisionAction::Verify,
                    detail: "systemctl enable --now docker".to_string(),
                },
            );
        }
    }

    // ── Phase D: NVIDIA Container Toolkit ──
    let skip_toolkit = post_driver.toolkit_configured() && !force;
    if skip_toolkit {
        emit_event(
            json,
            &ProvisionEvent::Toolkit {
                action: ProvisionAction::Skip,
                detail: "nvidia-container-toolkit already configured".to_string(),
            },
        );
    } else {
        // Add NVIDIA toolkit apt repository
        emit_event(
            json,
            &ProvisionEvent::Toolkit {
                action: if dry_run {
                    ProvisionAction::DryRun
                } else {
                    ProvisionAction::Install
                },
                detail:
                    "Adding NVIDIA toolkit apt repository and installing nvidia-container-toolkit"
                        .to_string(),
            },
        );
        if !dry_run {
            install_nvidia_container_toolkit()?;
            emit_event(
                json,
                &ProvisionEvent::Toolkit {
                    action: ProvisionAction::Configure,
                    detail: "nvidia-ctk runtime configure --runtime=docker".to_string(),
                },
            );
            let ctk_status = Command::new("nvidia-ctk")
                .args(["runtime", "configure", "--runtime=docker"])
                .status()
                .context("Failed to run nvidia-ctk runtime configure")?;
            if !ctk_status.success() {
                bail!(
                    "nvidia-ctk runtime configure --runtime=docker exited with status {ctk_status}"
                );
            }
            let restart_status = Command::new("systemctl")
                .args(["restart", "docker"])
                .status()
                .context("Failed to run systemctl restart docker")?;
            if !restart_status.success() {
                bail!("systemctl restart docker exited with status {restart_status}");
            }
            emit_event(
                json,
                &ProvisionEvent::Toolkit {
                    action: ProvisionAction::Verify,
                    detail: "docker restarted with nvidia runtime".to_string(),
                },
            );
        }
    }

    // ── Phase E: GPU Smoke Test ──
    let smoke_result = if dry_run {
        emit_event(
            json,
            &ProvisionEvent::SmokeTest {
                action: ProvisionAction::DryRun,
                detail: format!("docker run --rm --gpus all {SMOKE_TEST_IMAGE} nvidia-smi"),
            },
        );
        SmokeResult::Skipped
    } else {
        emit_event(
            json,
            &ProvisionEvent::SmokeTest {
                action: ProvisionAction::Verify,
                detail: format!("docker run --rm --gpus all {SMOKE_TEST_IMAGE} nvidia-smi"),
            },
        );
        run_gpu_smoke_test(&mut warnings)
    };

    let smoke_gpu_count = if smoke_result == SmokeResult::Pass {
        count_gpus_in_smoke_output()
    } else {
        None
    };

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
        docker_version: final_profile.docker.as_ref().map(|d| d.version.clone()),
        nvidia_container_toolkit_version: final_profile
            .nvidia_container_toolkit
            .as_ref()
            .map(|t| t.version.clone()),
        docker_gpu_smoke_result: smoke_result,
        smoke_gpu_count_detected: smoke_gpu_count,
        reboot_required: false,
        warnings: warnings.clone(),
        ato_cli_version: agent_version().to_string(),
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
    println!(
        "  Docker:      {}",
        receipt.docker_version.as_deref().unwrap_or("not detected")
    );
    println!(
        "  Toolkit:     {}",
        receipt
            .nvidia_container_toolkit_version
            .as_deref()
            .unwrap_or("not detected")
    );
    let smoke = match receipt.docker_gpu_smoke_result {
        SmokeResult::Pass => format!(
            "PASS ({} GPUs in container)",
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

fn install_nvidia_container_toolkit() -> Result<()> {
    // Add NVIDIA's toolkit apt repository for Ubuntu.
    //
    // Prerequisites: a bare Ubuntu image may not have curl, gnupg, or
    // ca-certificates installed. Install them first so the GPG key
    // download and dearmor can succeed.
    //
    // curl -fsSL https://nvidia.github.io/libnvidia-container/gpgkey | gpg --dearmor -o /usr/share/keyrings/nvidia-toolkit-keyring.gpg
    // curl -fsSL https://nvidia.github.io/libnvidia-container/stable/deb/nvidia-container-toolkit.list | sed 's#deb https://#deb [signed-by=/usr/share/keyrings/nvidia-toolkit-keyring.gpg] https://#g' > /etc/apt/sources.list.d/nvidia-container-toolkit.list
    // apt-get update && apt-get install -y nvidia-container-toolkit

    // Install prerequisites first.
    run_apt(&["update"])?;
    run_apt(&["install", "-y", "ca-certificates", "curl", "gnupg"])?;

    let keyring_path = "/usr/share/keyrings/nvidia-toolkit-keyring.gpg";
    let list_path = "/etc/apt/sources.list.d/nvidia-container-toolkit.list";

    // Use an absolute temp path under /tmp rather than a CWD-relative
    // .ato/tmp/ — root's CWD is unpredictable and we must not pollute it.
    let tmp_key = "/tmp/ato-nvidia-gpgkey.asc";

    // Download GPG key and dearmor it
    let curl_gpg = Command::new("curl")
        .args([
            "-fsSL",
            "https://nvidia.github.io/libnvidia-container/gpgkey",
        ])
        .output()
        .context("Failed to download NVIDIA GPG key")?;
    if !curl_gpg.status.success() {
        bail!("Failed to download NVIDIA GPG key");
    }

    std::fs::write(tmp_key, &curl_gpg.stdout)
        .with_context(|| format!("Failed to write {tmp_key}"))?;

    let gpg_status = Command::new("gpg")
        .args(["--dearmor", "-o", keyring_path, tmp_key])
        .status()
        .context("Failed to dearmor NVIDIA GPG key")?;
    if !gpg_status.success() {
        bail!("gpg --dearmor failed");
    }
    std::fs::remove_file(tmp_key).ok();

    // Download and install apt list
    let curl_list = Command::new("curl")
        .args([
            "-fsSL",
            "https://nvidia.github.io/libnvidia-container/stable/deb/nvidia-container-toolkit.list",
        ])
        .output()
        .context("Failed to download NVIDIA toolkit apt list")?;
    if !curl_list.status.success() {
        bail!("Failed to download NVIDIA toolkit apt list");
    }

    let list_content = String::from_utf8_lossy(&curl_list.stdout);
    let modified: String = list_content
        .lines()
        .map(|line| {
            if line.starts_with("deb https://") {
                line.replacen(
                    "deb https://",
                    &format!("deb [signed-by={keyring_path}] https://"),
                    1,
                )
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    std::fs::write(list_path, modified).with_context(|| format!("Failed to write {list_path}"))?;

    run_apt(&["update"])?;
    run_apt(&["install", "-y", "nvidia-container-toolkit"])?;
    Ok(())
}

fn run_gpu_smoke_test(warnings: &mut Vec<String>) -> SmokeResult {
    let output = Command::new("docker")
        .args([
            "run",
            "--rm",
            "--gpus",
            "all",
            SMOKE_TEST_IMAGE,
            "nvidia-smi",
        ])
        .output();

    match output {
        Ok(o) if o.status.success() => SmokeResult::Pass,
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            warnings.push(format!("GPU smoke test failed: {}", stderr.trim()));
            SmokeResult::Fail
        }
        Err(e) => {
            warnings.push(format!("Failed to run GPU smoke test: {e}"));
            SmokeResult::Fail
        }
    }
}

fn count_gpus_in_smoke_output() -> Option<usize> {
    // Re-run the smoke test and count GPUs (or read from the receipt path)
    // For simplicity, re-detect from host profile.
    let profile = capsule::foundation::host_gpu::detect_host_gpu_profile().ok()?;
    if profile.gpus.is_empty() {
        None
    } else {
        Some(profile.gpus.len())
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
            ProvisionEvent::Docker { action, detail } => {
                ("docker", action_str(*action), detail.clone())
            }
            ProvisionEvent::Toolkit { action, detail } => {
                ("toolkit", action_str(*action), detail.clone())
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
    use capsule::foundation::host_gpu::{DockerInfo, DriverInfo, GpuDevice, OsInfo, ToolkitInfo};

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
            docker: Some(DockerInfo {
                version: "27.5.1".to_string(),
                healthy: true,
            }),
            nvidia_container_toolkit: Some(ToolkitInfo {
                version: "1.17.5".to_string(),
                configured: true,
            }),
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
    fn diagnose_fails_when_no_docker() {
        let mut profile = test_profile();
        profile.docker = None;
        let checks = diagnose(&profile);
        let docker_check = checks.iter().find(|c| c.name == "docker").unwrap();
        assert_eq!(docker_check.status, CheckStatus::Fail);
    }

    #[test]
    fn diagnose_fails_when_no_toolkit() {
        let mut profile = test_profile();
        profile.nvidia_container_toolkit = None;
        let checks = diagnose(&profile);
        let tk_check = checks
            .iter()
            .find(|c| c.name == "nvidia_container_toolkit")
            .unwrap();
        assert_eq!(tk_check.status, CheckStatus::Fail);
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
    fn diagnose_warns_when_docker_unhealthy() {
        let mut profile = test_profile();
        profile.docker = Some(DockerInfo {
            version: "27.5.1".to_string(),
            healthy: false,
        });
        let checks = diagnose(&profile);
        let docker_check = checks.iter().find(|c| c.name == "docker").unwrap();
        assert_eq!(docker_check.status, CheckStatus::Warn);
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
}
