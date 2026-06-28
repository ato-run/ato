//! The llama.cpp native-inference engine.
//!
//! This is a behavior-preserving relocation of the llama.cpp-specific logic that
//! previously lived as string-matches in `launch_spec.rs`, `run.rs`, the runtime
//! fetcher, and `native_inference_doctor.rs`. Each method below delegates to (or
//! reproduces verbatim) the existing helper so `ato run` resolves, launches, and
//! gates a llama.cpp capsule EXACTLY as before.

use crate::error::{CapsuleError, Result};
use crate::packers::runtime_fetcher::{RuntimeFetcher, llama_cpp_platform_support, llamacpp_cache_key};

use super::engine::{
    Engine, EngineCheck, EngineCheckStatus, EngineContext, EngineId, HostCapabilities, VariantPlan,
};

/// The pinned llama.cpp release the canonical local-LLM capsules use, used to
/// label the doctor's platform probe. The platform→artifact mapping is
/// version-independent, so this only labels the probe; each capsule's own
/// `engine_version` still governs its run. (Was `REFERENCE_ENGINE_VERSION` in
/// `native_inference_doctor.rs`.)
const REFERENCE_ENGINE_VERSION: &str = "b9754";

pub(crate) struct LlamaCppEngine;

#[async_trait::async_trait]
impl Engine for LlamaCppEngine {
    fn id(&self) -> EngineId {
        EngineId::LlamaCpp
    }

    fn default_port(&self) -> u16 {
        // llama.cpp / llama-server conventional port (was launch_spec.rs:154).
        8080
    }

    /// PURE, NON-GATING cache-key plan. Reproduces the fetcher's
    /// `normalize_engine_variant` (default/cpu/metal/`""` → default build;
    /// anything else → the slug verbatim). NEVER errors — the launcher and the
    /// cache path use this; the fail-closed lives in `plan_variant`.
    fn cache_variant_plan(&self, variant: Option<&str>) -> VariantPlan {
        match variant.map(|v| v.trim().to_ascii_lowercase()) {
            None => VariantPlan::default_build(),
            Some(v) => match v.as_str() {
                "" | "default" | "cpu" | "metal" => VariantPlan::default_build(),
                other => VariantPlan::named(other.to_string()),
            },
        }
    }

    /// Reproduces `resolve_engine_variant_action` (was run.rs:4460): variant /
    /// platform dispatch FIRST so a broad GPU-presence gate never masks the real
    /// reason. The Vulkan-on-Linux branch (and only it) gates on full Vulkan
    /// readiness — but only when the host is probed; the unprobed launcher path
    /// returns the slug without gating (only the cache-key path / launcher would
    /// call this unprobed, and it uses `cache_variant_plan` instead).
    fn plan_variant(
        &self,
        variant: Option<&str>,
        host: &HostCapabilities,
    ) -> std::result::Result<VariantPlan, String> {
        let normalized = variant
            .map(|v| v.trim().to_ascii_lowercase())
            .filter(|v| !v.is_empty());
        match normalized.as_deref() {
            // Default CPU/Metal build — no GPU readiness gate.
            None | Some("default") | Some("cpu") | Some("metal") => Ok(VariantPlan::default_build()),
            Some("vulkan") => match host.os.as_str() {
                "linux" => {
                    // A GPU build must never silently fall back to CPU, so when
                    // probed it requires full Vulkan readiness, not mere GPU
                    // presence (a failed probe is "not ready" → fail closed,
                    // matching the historical ensure-step). When unprobed
                    // (launcher), there is no gate.
                    if !host.probed || host.vulkan_ready() {
                        Ok(VariantPlan::named("vulkan"))
                    } else {
                        Err(
                            "engine_variant=\"vulkan\" needs a ready NVIDIA Vulkan host \
                             (GPU + driver + Vulkan device), but none was detected. Run \
                             `ato runner doctor --profile nvidia-ubuntu` / \
                             `ato runner provision --profile nvidia-ubuntu`, or set an explicit \
                             engine_path."
                                .to_string(),
                        )
                    }
                }
                "macos" => Err(
                    "engine_variant=\"vulkan\" is not supported on macOS — omit \
                                engine_variant to use the default Metal-accelerated build."
                        .to_string(),
                ),
                other => Err(format!(
                    "engine_variant=\"vulkan\" has no llama.cpp prebuilt for {other} \
                     (Linux only); set an explicit engine_path."
                )),
            },
            Some("cuda") => Err("engine_variant=\"cuda\" has no managed llama.cpp prebuilt \
                                 (no Linux CUDA release; Windows CUDA is out of scope). Set an \
                                 explicit engine_path, use engine_variant=\"vulkan\" for managed \
                                 GPU acceleration, or wait for a future source-build slice."
                .to_string()),
            Some(other) => Err(format!(
                "unknown engine_variant {other:?} (supported: vulkan; default = CPU/Metal)"
            )),
        }
    }

    fn cache_key(&self, version: &str, variant: &VariantPlan) -> String {
        // Delegate to the fetcher's canonical key so the launcher and fetcher
        // never disagree (was launch_spec.rs:203 / mod.rs:291).
        llamacpp_cache_key(version, variant.as_deref())
    }

    /// Reproduces `resolve_native_inference_engine_command` (was
    /// launch_spec.rs:165). PURE: builds the deterministic cached path WITHOUT an
    /// existence check — the ensure-step guarantees it by spawn time.
    fn resolve_server_command(&self, ctx: &EngineContext) -> Result<String> {
        if let Some(engine_path) = ctx
            .engine_path
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Ok(engine_path.to_string());
        }

        let version = ctx
            .engine_version
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                CapsuleError::Config(format!(
                    "target '{}': engine=\"llama.cpp\" requires `engine_version` \
                     (a build tag, e.g. \"b4231\") — or set an explicit `engine_path`",
                    ctx.target
                ))
            })?;
        // Defense-in-depth: the version is interpolated into the cache path.
        if !crate::foundation::types::manifest::is_safe_engine_version(version) {
            return Err(CapsuleError::Config(format!(
                "target '{}': unsafe `engine_version` {version:?} \
                 (alphanumeric / `.`/`_`/`-` only; no path separators or `..`)",
                ctx.target
            )));
        }
        // The platform-specific fail-closed for an unsupported variant
        // (e.g. `cuda` on Linux, `vulkan` on macOS) is enforced by `plan_variant`
        // / the fetcher at the ensure-step; here we only need the variant-aware
        // cache KEY so this deterministic path matches what the fetcher populates
        // (GPU and CPU builds of a tag never share a directory).
        let key = self.cache_key(version, &ctx.variant);
        // Deterministic cached path: `<cache>/llamacpp-<key>/llama-server`. The
        // fetcher GUARANTEES this canonical path as a post-condition, so we build
        // it WITHOUT an existence check — the receipt/preflight builders call
        // this before the async ensure-step has downloaded the binary, which the
        // ensure-step then guarantees by spawn time.
        let fetcher = RuntimeFetcher::new().map_err(|err| {
            CapsuleError::Config(format!("failed to init toolchain cache: {err}"))
        })?;
        let binary_name = if cfg!(target_os = "windows") {
            "llama-server.exe"
        } else {
            "llama-server"
        };
        let binary = fetcher
            .get_runtime_path(self.id().toolchain_key(), &key)
            .join(binary_name);
        Ok(binary.to_string_lossy().to_string())
    }

    /// Reproduces `resolve_native_inference_model` (was launch_spec.rs:234).
    /// PURE: builds the deterministic content-addressed blob path WITHOUT an
    /// existence check — the ensure-step downloads + verifies it by spawn time.
    fn resolve_model_path(&self, ctx: &EngineContext) -> Result<String> {
        if let Some(model) = ctx
            .model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Ok(model.to_string());
        }

        match ctx
            .model_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(url) => {
                if !crate::foundation::types::manifest::is_safe_model_url(url) {
                    return Err(CapsuleError::Config(format!(
                        "target '{}': `model_url` must be a plain http(s):// URL",
                        ctx.target
                    )));
                }
                let sha_raw = ctx
                    .model_sha256
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        CapsuleError::Config(format!(
                            "target '{}': `model_url` requires `model_sha256`",
                            ctx.target
                        ))
                    })?;
                let sha = crate::foundation::types::manifest::normalize_model_sha256(sha_raw)
                    .ok_or_else(|| {
                        CapsuleError::Config(format!(
                            "target '{}': `model_sha256` must be a 64-char hex SHA-256",
                            ctx.target
                        ))
                    })?;
                // Deterministic content-addressed path — known from the sha256
                // alone, so preflight/receipt builders resolve it before the
                // ensure-step downloads + verifies it (which guarantees it by
                // spawn time).
                let blob = crate::resource::model_cache::model_blob_path(&sha);
                Ok(blob.to_string_lossy().to_string())
            }
            None => Err(CapsuleError::Config(format!(
                "target '{}': runtime=native-inference requires either `model` (a local file) \
                 or `model_url` + `model_sha256` (managed)",
                ctx.target
            ))),
        }
    }

    /// Reproduces the hardcoded `["-m", model, "--host", "127.0.0.1"]` argv (was
    /// launch_spec.rs:138). `--port` is intentionally NOT emitted: the host
    /// launcher injects the resolved port so readiness and app_url agree.
    fn build_server_argv(&self, model_path: &str, host: &str, _port: u16) -> Vec<String> {
        vec![
            "-m".to_string(),
            model_path.to_string(),
            "--host".to_string(),
            host.to_string(),
        ]
    }

    /// Reproduces the llama.cpp arm of `ensure_native_inference_engine` (was
    /// run.rs:4519-4546): variant/platform dispatch via the gating `plan_variant`
    /// against the probed host (fail closed for an unsupported / not-ready
    /// accelerated variant), then fetch the keyed build.
    async fn ensure_engine(
        &self,
        ctx: &EngineContext,
        host: &HostCapabilities,
        fetcher: &RuntimeFetcher,
    ) -> Result<()> {
        if ctx
            .engine_path
            .as_deref()
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
        {
            return Ok(());
        }
        let version = ctx
            .engine_version
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                CapsuleError::Pack(
                    "engine=\"llama.cpp\" requires `engine_version` (a build tag, e.g. \"b4231\")"
                        .to_string(),
                )
            })?;
        // Variant/platform dispatch first; the Vulkan-on-Linux branch (and only
        // it) gates on full Vulkan readiness against the probed host, failing
        // closed rather than falling back to a CPU build. On success the slug is
        // identical to `ctx.variant` (the cache plan).
        let plan = self
            .plan_variant(ctx.variant_raw.as_deref(), host)
            .map_err(CapsuleError::Pack)?;
        fetcher.ensure_llamacpp(version, plan.as_deref()).await?;
        Ok(())
    }

    // `ensure_model` uses the trait default (it IS the llama.cpp CAS path).

    /// Reproduces the engine-derived doctor rows from
    /// `native_inference_doctor::diagnose()` (platform / acceleration /
    /// recommended_target). The host-cache (`model_cache`) row stays in the CLI
    /// doctor — it is engine-agnostic. Output is byte-identical to today.
    fn doctor_checks(&self, host: &HostCapabilities) -> Vec<EngineCheck> {
        let mut results = Vec::new();

        // 1. Platform + managed engine availability (reuses the fetcher mapping).
        let support = llama_cpp_platform_support(REFERENCE_ENGINE_VERSION);
        let os = support
            .as_ref()
            .map(|s| s.os.clone())
            .unwrap_or_else(|_| host.os.clone());
        match &support {
            Ok(s) if s.default_available => results.push(EngineCheck {
                name: "platform",
                status: EngineCheckStatus::Ok,
                detail: format!("{}-{} — managed llama.cpp prebuilt available", s.os, s.arch),
                recommendation: None,
            }),
            Ok(s) => results.push(EngineCheck {
                name: "platform",
                status: EngineCheckStatus::Fail,
                detail: format!("{}-{} has no managed llama.cpp prebuilt", s.os, s.arch),
                recommendation: Some(
                    "Set an explicit `engine_path`, or run on macOS (arm64/x64) or Linux x64.",
                ),
            }),
            Err(_) => results.push(EngineCheck {
                name: "platform",
                status: EngineCheckStatus::Fail,
                detail: format!(
                    "unsupported platform: {} {}",
                    std::env::consts::OS,
                    std::env::consts::ARCH
                ),
                recommendation: Some(
                    "native-inference supports macOS (arm64/x64) and Linux x64.",
                ),
            }),
        }

        // 2. Acceleration: Metal on macOS (default build), Vulkan on Linux NVIDIA.
        //    Uses the already-probed `host.gpu` (the doctor probes once on Linux).
        let gpu_profile = host.gpu.as_ref();
        if os == "macos" {
            results.push(EngineCheck {
                name: "acceleration",
                status: EngineCheckStatus::Ok,
                detail: "Metal (Apple GPU) — the default macOS engine build is Metal-accelerated"
                    .to_string(),
                recommendation: None,
            });
        } else if os == "linux" {
            match gpu_profile {
                Some(profile) if profile.native_inference_vulkan_ready() => {
                    results.push(EngineCheck {
                        name: "acceleration",
                        status: EngineCheckStatus::Ok,
                        detail:
                            "Vulkan GPU ready — the chat-vulkan target can offload to the NVIDIA GPU"
                                .to_string(),
                        recommendation: None,
                    })
                }
                Some(profile) if profile.has_gpu() => results.push(EngineCheck {
                    name: "acceleration",
                    status: EngineCheckStatus::Warn,
                    detail: "NVIDIA GPU present but Vulkan is not ready — CPU (chat) works; GPU (chat-vulkan) does not yet"
                        .to_string(),
                    recommendation: Some(
                        "Run `ato runner doctor --profile nvidia-ubuntu`, then `sudo ato runner provision --profile nvidia-ubuntu`, to enable the chat-vulkan target.",
                    ),
                }),
                Some(_) => results.push(EngineCheck {
                    name: "acceleration",
                    status: EngineCheckStatus::Warn,
                    detail: "No NVIDIA GPU detected — the default chat target runs on CPU"
                        .to_string(),
                    recommendation: Some(
                        "CPU is fine for small models; for GPU use a Linux NVIDIA host with the chat-vulkan target.",
                    ),
                }),
                None => results.push(EngineCheck {
                    name: "acceleration",
                    status: EngineCheckStatus::Warn,
                    detail: "Could not probe the GPU — the default chat target runs on CPU"
                        .to_string(),
                    recommendation: None,
                }),
            }
        } else {
            results.push(EngineCheck {
                name: "acceleration",
                status: EngineCheckStatus::Na,
                detail: format!("{os}: CPU only for native-inference"),
                recommendation: None,
            });
        }

        // 3. Recommended target (informational — always OK).
        let vulkan_target = support
            .as_ref()
            .map(|s| s.vulkan_available)
            .unwrap_or(false)
            && gpu_profile
                .map(|p| p.native_inference_vulkan_ready())
                .unwrap_or(false);
        let detail = if vulkan_target {
            "chat (CPU/Metal) — always; chat-vulkan — GPU-accelerated on this host".to_string()
        } else if os == "macos" {
            "chat (Metal-accelerated) — recommended on this host".to_string()
        } else {
            "chat (CPU) — recommended on this host".to_string()
        };
        results.push(EngineCheck {
            name: "recommended_target",
            status: EngineCheckStatus::Ok,
            detail,
            recommendation: None,
        });

        results
    }
}
