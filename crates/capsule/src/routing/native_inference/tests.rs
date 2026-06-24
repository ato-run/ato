//! Unit tests for the native-inference engine abstraction (increment 1).
//!
//! These assert that `LlamaCppEngine` reproduces today's llama.cpp behavior
//! through the trait. The launcher-level tests (that `derive_launch_spec`
//! produces identical argv/command/port) stay in `launch_spec.rs` and exercise
//! the same code path end to end.

use super::engine::{Engine, EngineCheckStatus, EngineContext, EngineId, HostCapabilities, VariantPlan};
use super::llamacpp::LlamaCppEngine;
use crate::foundation::host_gpu::{
    DriverInfo, GpuDevice, HostGpuProfile, OsInfo, VulkanInfo,
};

// ── host fixtures ─────────────────────────────────────────────────────────

/// Probed Linux host (ensure-step / doctor path: readiness gates apply).
fn linux_caps_probed(gpu: Option<HostGpuProfile>) -> HostCapabilities {
    HostCapabilities {
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
        gpu,
        probed: true,
    }
}

/// Unprobed Linux host (launcher path: readiness gates are skipped).
fn linux_caps_unprobed() -> HostCapabilities {
    HostCapabilities {
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
        gpu: None,
        probed: false,
    }
}

fn macos_caps_probed() -> HostCapabilities {
    HostCapabilities {
        os: "macos".to_string(),
        arch: "aarch64".to_string(),
        gpu: None,
        probed: true,
    }
}

/// A probed profile with full Vulkan readiness.
fn vulkan_ready_profile() -> HostGpuProfile {
    HostGpuProfile {
        os: OsInfo {
            distro: "ubuntu".to_string(),
            version: "22.04".to_string(),
            kernel: "5.15.0".to_string(),
        },
        secure_boot_enabled: None,
        gpus: vec![GpuDevice {
            index: 0,
            name: "NVIDIA".to_string(),
            uuid: None,
            vram_bytes: 0,
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
    }
}

/// A probed profile with a GPU but NOT Vulkan-ready (no visible device).
fn gpu_not_vulkan_ready_profile() -> HostGpuProfile {
    let mut p = vulkan_ready_profile();
    p.vulkan = Some(VulkanInfo {
        loader_present: true,
        vulkaninfo_available: true,
        nvidia_icd_present: false,
        nvidia_device_visible: false,
    });
    p
}

// ── EngineId ──────────────────────────────────────────────────────────────

#[test]
fn engine_id_maps_llamacpp_aliases() {
    for s in ["llama.cpp", "llamacpp", "llama-cpp", "  LLAMA.CPP  "] {
        assert_eq!(EngineId::from_manifest(s), Some(EngineId::LlamaCpp), "{s:?}");
    }
    assert_eq!(EngineId::from_manifest("sglang"), None);
    assert_eq!(EngineId::from_manifest(""), None);
    assert_eq!(EngineId::LlamaCpp.toolchain_key(), "llamacpp");
}

// ── plan_variant (was resolve_engine_variant_action) ──────────────────────

#[test]
fn plan_variant_default_returns_default_build_without_gating() {
    // The default / cpu / metal variants never gate on the GPU — even a probed
    // host with no GPU resolves them (matches the old `never_ready` cases).
    let caps = linux_caps_probed(None);
    for v in [None, Some("default"), Some("cpu"), Some("metal"), Some("")] {
        assert_eq!(
            LlamaCppEngine.plan_variant(v, &caps),
            Ok(VariantPlan::default_build()),
            "variant {v:?} should be the default build"
        );
    }
}

#[test]
fn plan_variant_vulkan_on_macos_errors_with_metal_hint_not_gpu() {
    let err = LlamaCppEngine
        .plan_variant(Some("vulkan"), &macos_caps_probed())
        .expect_err("macOS vulkan must fail");
    assert!(err.contains("macOS") && err.contains("Metal"), "{err}");
    assert!(!err.contains("NVIDIA Vulkan host") && !err.contains("runner doctor"));
}

#[test]
fn plan_variant_cuda_fails_closed_not_gpu_presence() {
    let err = LlamaCppEngine
        .plan_variant(Some("cuda"), &linux_caps_probed(None))
        .expect_err("cuda must fail closed");
    assert!(
        err.contains("cuda") && err.contains("no managed llama.cpp prebuilt"),
        "{err}"
    );
    assert!(!err.contains("NVIDIA Vulkan host") && !err.contains("ubuntu-x64"));
}

#[test]
fn plan_variant_unknown_errors_as_unknown() {
    let err = LlamaCppEngine
        .plan_variant(Some("rocm"), &linux_caps_probed(None))
        .expect_err("unknown variant must fail");
    assert!(err.contains("unknown engine_variant"), "{err}");
}

#[test]
fn plan_variant_vulkan_linux_probed_not_ready_fails_closed() {
    let err = LlamaCppEngine
        .plan_variant(
            Some("vulkan"),
            &linux_caps_probed(Some(gpu_not_vulkan_ready_profile())),
        )
        .expect_err("probed-but-not-ready vulkan must fail closed");
    assert!(
        err.contains("runner doctor") || err.contains("runner provision"),
        "{err}"
    );
}

#[test]
fn plan_variant_vulkan_linux_probed_failed_probe_fails_closed() {
    // A probed host whose probe found NO GPU (gpu=None, probed=true) is the
    // ensure-step detection-failure case: it must fail closed, NOT skip the gate.
    let err = LlamaCppEngine
        .plan_variant(Some("vulkan"), &linux_caps_probed(None))
        .expect_err("probed-with-failed-detection vulkan must fail closed");
    assert!(
        err.contains("runner doctor") || err.contains("runner provision"),
        "{err}"
    );
}

#[test]
fn plan_variant_vulkan_linux_probed_ready_returns_vulkan() {
    let action = LlamaCppEngine
        .plan_variant(Some("vulkan"), &linux_caps_probed(Some(vulkan_ready_profile())));
    assert_eq!(action, Ok(VariantPlan::named("vulkan")));
}

#[test]
fn plan_variant_vulkan_linux_unprobed_returns_vulkan_without_gating() {
    // The launcher path (unprobed) returns the slug WITHOUT gating so the cache
    // key is deterministic; the ensure-step's probed host is the real gate.
    let action = LlamaCppEngine.plan_variant(Some("vulkan"), &linux_caps_unprobed());
    assert_eq!(action, Ok(VariantPlan::named("vulkan")));
}

// ── cache_variant_plan (non-gating; launcher / cache-key path) ────────────

#[test]
fn cache_variant_plan_never_gates() {
    // default family → default build.
    for v in [None, Some("default"), Some("cpu"), Some("metal"), Some("")] {
        assert_eq!(
            LlamaCppEngine.cache_variant_plan(v),
            VariantPlan::default_build(),
            "{v:?}"
        );
    }
    // Everything else → the slug verbatim, NEVER an error — even cuda / macOS-
    // unsupported / unknown (the launcher keys the cache; the fetcher/ensure-step
    // is what fails closed).
    assert_eq!(
        LlamaCppEngine.cache_variant_plan(Some("vulkan")),
        VariantPlan::named("vulkan")
    );
    assert_eq!(
        LlamaCppEngine.cache_variant_plan(Some("cuda")),
        VariantPlan::named("cuda")
    );
    assert_eq!(
        LlamaCppEngine.cache_variant_plan(Some("  VULKAN ")),
        VariantPlan::named("vulkan")
    );
}

// ── cache_key ─────────────────────────────────────────────────────────────

#[test]
fn cache_key_separates_variants() {
    assert_eq!(LlamaCppEngine.cache_key("b9754", &VariantPlan::default_build()), "b9754");
    assert_eq!(
        LlamaCppEngine.cache_key("b9754", &VariantPlan::named("vulkan")),
        "b9754@vulkan"
    );
}

// ── build_server_argv ─────────────────────────────────────────────────────

#[test]
fn build_server_argv_is_hardcoded_and_omits_port() {
    let argv = LlamaCppEngine.build_server_argv("/m.gguf", "127.0.0.1", 9001);
    assert_eq!(argv, vec!["-m", "/m.gguf", "--host", "127.0.0.1"]);
    // `--port` must NEVER be in argv — the host launcher injects it.
    assert!(!argv.iter().any(|a| a == "--port"));
}

// ── resolve_server_command / resolve_model_path (pure) ────────────────────

fn ctx(engine_path: Option<&str>, engine_version: Option<&str>, variant: VariantPlan) -> EngineContext {
    EngineContext {
        target: "app".to_string(),
        engine_path: engine_path.map(String::from),
        engine_version: engine_version.map(String::from),
        variant_raw: variant.slug.clone(),
        variant,
        model: None,
        model_url: None,
        model_sha256: None,
    }
}

#[test]
fn resolve_server_command_prefers_engine_path() {
    let c = ctx(Some("./llama-server"), None, VariantPlan::default_build());
    assert_eq!(LlamaCppEngine.resolve_server_command(&c).unwrap(), "./llama-server");
}

#[test]
fn resolve_server_command_managed_requires_version() {
    let c = ctx(None, None, VariantPlan::default_build());
    let err = LlamaCppEngine
        .resolve_server_command(&c)
        .expect_err("managed engine needs engine_version");
    assert!(err.to_string().contains("engine_version"), "{err}");
}

#[test]
fn resolve_server_command_managed_resolves_cached_path() {
    let c = ctx(None, Some("b4231"), VariantPlan::default_build());
    let cmd = LlamaCppEngine.resolve_server_command(&c).expect("resolves");
    let suffix = if cfg!(target_os = "windows") {
        "llamacpp-b4231\\llama-server.exe"
    } else {
        "llamacpp-b4231/llama-server"
    };
    assert!(cmd.ends_with(suffix), "got: {cmd}");
}

#[test]
fn resolve_server_command_variant_keys_cache_path() {
    let c = ctx(None, Some("b9754"), VariantPlan::named("vulkan"));
    let cmd = LlamaCppEngine.resolve_server_command(&c).expect("resolves");
    let suffix = if cfg!(target_os = "windows") {
        "llamacpp-b9754@vulkan\\llama-server.exe"
    } else {
        "llamacpp-b9754@vulkan/llama-server"
    };
    assert!(cmd.ends_with(suffix), "got: {cmd}");
}

#[test]
fn resolve_model_path_prefers_local_model() {
    let mut c = ctx(Some("./llama-server"), None, VariantPlan::default_build());
    c.model = Some("./model.gguf".to_string());
    assert_eq!(LlamaCppEngine.resolve_model_path(&c).unwrap(), "./model.gguf");
}

#[test]
fn resolve_model_path_managed_resolves_blob_path() {
    let hex = "a".repeat(64);
    let mut c = ctx(Some("./llama-server"), None, VariantPlan::default_build());
    c.model_url = Some("https://example.com/m.gguf".to_string());
    c.model_sha256 = Some(hex.clone());
    let path = LlamaCppEngine.resolve_model_path(&c).expect("resolves");
    assert!(path.ends_with(&format!("sha256-{hex}")), "got: {path}");
}

#[test]
fn resolve_model_path_model_url_requires_sha256() {
    let mut c = ctx(Some("./llama-server"), None, VariantPlan::default_build());
    c.model_url = Some("https://example.com/m.gguf".to_string());
    let err = LlamaCppEngine
        .resolve_model_path(&c)
        .expect_err("model_url needs sha256");
    assert!(err.to_string().contains("model_sha256"), "{err}");
}

#[test]
fn resolve_model_path_invalid_sha256_errors() {
    let mut c = ctx(Some("./llama-server"), None, VariantPlan::default_build());
    c.model_url = Some("https://example.com/m.gguf".to_string());
    c.model_sha256 = Some("not-a-real-hash".to_string());
    let err = LlamaCppEngine
        .resolve_model_path(&c)
        .expect_err("invalid sha must error");
    assert!(err.to_string().contains("SHA-256"), "{err}");
}

#[test]
fn resolve_model_path_requires_model_or_url() {
    let c = ctx(Some("./llama-server"), None, VariantPlan::default_build());
    let err = LlamaCppEngine
        .resolve_model_path(&c)
        .expect_err("needs model or model_url");
    let msg = err.to_string();
    assert!(msg.contains("model") && msg.contains("model_url"), "{msg}");
}

// ── doctor_checks ─────────────────────────────────────────────────────────

#[test]
fn doctor_checks_include_engine_rows() {
    // The engine owns platform / acceleration / recommended_target; model_cache
    // is the CLI doctor's host-cache row (not engine-specific).
    let caps = HostCapabilities::from_profile(
        capsule_detect_or_none(),
    );
    let checks = LlamaCppEngine.doctor_checks(&caps);
    for name in ["platform", "acceleration", "recommended_target"] {
        assert!(checks.iter().any(|c| c.name == name), "missing {name}");
    }
    let rec = checks
        .iter()
        .find(|c| c.name == "recommended_target")
        .unwrap();
    assert_eq!(rec.status, EngineCheckStatus::Ok);
}

fn capsule_detect_or_none() -> Option<HostGpuProfile> {
    if std::env::consts::OS == "linux" {
        crate::foundation::host_gpu::detect_host_gpu_profile().ok()
    } else {
        None
    }
}
