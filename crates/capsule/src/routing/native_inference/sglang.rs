//! The SGLang native-inference engine (increment 2).
//!
//! SGLang serves an OpenAI-compatible HTTP API by running the managed Python
//! venv's interpreter as `python -m sglang.launch_server --model-path <dir>
//! --host <host>` (the host launcher injects `--port`, exactly like llama.cpp).
//! It is CUDA-only (Linux + NVIDIA/Ampere): there is a single managed build,
//! gated on real CUDA readiness, and the model is a content-addressed Hugging
//! Face repo (a directory of safetensor shards) rather than a single GGUF file.
//!
//! The pure methods (`resolve_server_command`, `resolve_model_path`,
//! `build_server_argv`, `cache_key`, `plan_variant`) are unit-tested; the async
//! `ensure_engine` / `ensure_model` carry the real venv/pip + HF-download logic
//! (only end-to-end validatable on a CUDA host — host-pending).

use crate::error::{CapsuleError, Result};
use crate::packers::runtime_fetcher::{RuntimeFetcher, sglang_venv_python};

use super::engine::{
    Engine, EngineCheck, EngineCheckStatus, EngineContext, EngineId, HostCapabilities, VariantPlan,
};

/// The conventional SGLang OpenAI-server port when the manifest omits one.
const SGLANG_DEFAULT_PORT: u16 = 30000;

pub(crate) struct SgLangEngine;

#[async_trait::async_trait]
impl Engine for SgLangEngine {
    fn id(&self) -> EngineId {
        EngineId::SgLang
    }

    fn default_port(&self) -> u16 {
        SGLANG_DEFAULT_PORT
    }

    /// PURE, NON-GATING cache-key plan. SGLang has a single managed CUDA build,
    /// so every accepted variant (`None` / `"cuda"`) keys the same cache path:
    /// the default build (slug = None). The platform/readiness fail-closed lives
    /// in `plan_variant` (run at the probed ensure-step), never here — so an
    /// `engine_variant = "cuda"` manifest keys its cache without erroring in the
    /// launcher. Mirrors llama.cpp's split between `cache_variant_plan` and
    /// `plan_variant`.
    fn cache_variant_plan(&self, _variant: Option<&str>) -> VariantPlan {
        VariantPlan::default_build()
    }

    /// GATING. SGLang is Linux + CUDA only with a single build: `None`/`"cuda"`
    /// on Linux → the default build when CUDA-ready; everything else fails
    /// closed with a precise reason (Unsupported-platform vs not-CUDA-ready vs
    /// no-such-variant).
    ///
    /// When probed (`host.probed = true`) the Linux branch gates on real CUDA
    /// readiness (a GPU build must never silently run on a host without a usable
    /// CUDA venv; a failed probe is "not ready"). When unprobed (the launcher
    /// path) it returns the build WITHOUT gating — but the launcher uses
    /// `cache_variant_plan`, so the only callers of this method probe the host.
    fn plan_variant(
        &self,
        variant: Option<&str>,
        host: &HostCapabilities,
    ) -> std::result::Result<VariantPlan, String> {
        let normalized = variant
            .map(|v| v.trim().to_ascii_lowercase())
            .filter(|v| !v.is_empty());
        match (normalized.as_deref(), host.os.as_str()) {
            (None | Some("cuda"), "linux") => {
                if !host.probed || host.cuda_ready() {
                    Ok(VariantPlan::default_build())
                } else {
                    Err(
                        "engine=\"sglang\" needs a CUDA-ready host (NVIDIA GPU + driver + \
                         CUDA runtime + a usable Python/venv), but none was detected. Run \
                         `sudo ato runner provision --profile nvidia-cuda` then \
                         `ato runner doctor --profile nvidia-cuda`, or set an explicit \
                         engine_path."
                            .to_string(),
                    )
                }
            }
            (None | Some("cuda"), other) => Err(format!(
                "engine=\"sglang\" is Linux + CUDA only — no managed build for {other}. \
                 Run it on a Linux NVIDIA host, or set an explicit engine_path."
            )),
            (Some("vulkan"), _) | (Some("metal"), _) | (Some("cpu"), _) => Err(
                "engine=\"sglang\" has no CPU/Vulkan/Metal build — it is CUDA-only. \
                     Omit engine_variant (it defaults to the CUDA build) on a CUDA host."
                    .to_string(),
            ),
            (Some(other), _) => Err(format!(
                "unknown engine_variant {other:?} for sglang (CUDA-only; omit engine_variant)."
            )),
        }
    }

    /// Single CUDA build per wheel version → the cache key is just the wheel
    /// version (no variant slug). Used by BOTH the fetcher (venv dir) and the
    /// launcher (server command path) so they never disagree.
    fn cache_key(&self, version: &str, _variant: &VariantPlan) -> String {
        version.to_string()
    }

    /// PURE. The server command: an `engine_path` override (a python that has
    /// sglang), or the managed venv python derived deterministically from the
    /// pinned wheel version. Built WITHOUT an existence check — the ensure-step
    /// guarantees the venv (and `import sglang`) by spawn time.
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
                    "target '{}': engine=\"sglang\" requires `engine_version` \
                     (the pinned sglang wheel, e.g. \"0.4.10.post2\") — or set an \
                     explicit `engine_path`",
                    ctx.target
                ))
            })?;
        if !crate::foundation::types::manifest::is_safe_engine_version(version) {
            return Err(CapsuleError::Config(format!(
                "target '{}': unsafe sglang `engine_version` {version:?} \
                 (alphanumeric / `.`/`_`/`-` only; no path separators or `..`)",
                ctx.target
            )));
        }
        let key = self.cache_key(version, &ctx.variant);
        let fetcher = RuntimeFetcher::new().map_err(|err| {
            CapsuleError::Config(format!("failed to init toolchain cache: {err}"))
        })?;
        let runtime_dir = fetcher.get_runtime_path(self.id().toolchain_key(), &key);
        let python = sglang_venv_python(&runtime_dir);
        Ok(python.to_string_lossy().to_string())
    }

    /// PURE. The `--model-path` value: a local model directory override, or the
    /// content-addressed Hugging Face repo directory derived from the pinned
    /// `model_repo_sha256`. Built WITHOUT an existence check — the ensure-step
    /// downloads + verifies it by spawn time.
    fn resolve_model_path(&self, ctx: &EngineContext) -> Result<String> {
        if let Some(model) = ctx
            .model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Ok(model.to_string());
        }

        // `model_url` (single-file) and `model_repo` (multi-file) are mutually
        // exclusive — SGLang loads a directory, not a single GGUF.
        if ctx
            .model_url
            .as_deref()
            .map(|u| !u.trim().is_empty())
            .unwrap_or(false)
        {
            return Err(CapsuleError::Config(format!(
                "target '{}': engine=\"sglang\" uses `model_repo` (a Hugging Face repo), \
                 not `model_url` (a single file). Remove `model_url`.",
                ctx.target
            )));
        }

        let repo = ctx
            .model_repo
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                CapsuleError::Config(format!(
                    "target '{}': engine=\"sglang\" requires either `model` (a local model \
                     directory) or `model_repo` + `model_revision` + `model_repo_sha256` \
                     (a managed Hugging Face repo)",
                    ctx.target
                ))
            })?;
        if !crate::foundation::types::manifest::is_safe_hf_repo(repo) {
            return Err(CapsuleError::Config(format!(
                "target '{}': `model_repo` must be a `<org>/<name>` Hugging Face id, got {repo:?}",
                ctx.target
            )));
        }
        let sha = ctx
            .model_repo_sha256
            .as_deref()
            .and_then(crate::foundation::types::manifest::normalize_model_sha256)
            .ok_or_else(|| {
                CapsuleError::Config(format!(
                    "target '{}': `model_repo` requires a 64-char hex `model_repo_sha256`",
                    ctx.target
                ))
            })?;
        let dir = crate::resource::model_cache::model_repo_path(&sha);
        Ok(dir.to_string_lossy().to_string())
    }

    /// PURE. The SGLang OpenAI-server argv: `python -m sglang.launch_server
    /// --model-path <dir> --host <host>`. `--port` is intentionally NOT emitted —
    /// the host launcher injects the resolved/allocated port so readiness and the
    /// app_url agree (the same contract as llama.cpp).
    fn build_server_argv(&self, model_path: &str, host: &str, _port: u16) -> Vec<String> {
        vec![
            "-m".to_string(),
            "sglang.launch_server".to_string(),
            "--model-path".to_string(),
            model_path.to_string(),
            "--host".to_string(),
            host.to_string(),
        ]
    }

    /// ASYNC. Provision the managed sglang venv (pinned wheel + torch cu124 +
    /// kernels) so the pure server-command path exists by spawn time. A local
    /// `engine_path` short-circuits. Runs the fail-closed `plan_variant` gate
    /// against the probed host first (so an unsupported / not-CUDA-ready host
    /// fails closed rather than building a venv that can't run).
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
                    "engine=\"sglang\" requires `engine_version` (the pinned sglang wheel, \
                     e.g. \"0.4.10.post2\")"
                        .to_string(),
                )
            })?;
        // CUDA platform/readiness fail-closed before any install.
        self.plan_variant(ctx.variant_raw.as_deref(), host)
            .map_err(CapsuleError::Pack)?;
        fetcher.ensure_sglang(version).await?;
        Ok(())
    }

    /// ASYNC. Download + verify the managed Hugging Face model repo into the
    /// content-addressed cache. A local `model` directory short-circuits.
    /// Reproducible by `model_repo` + `model_revision` (immutable commit) +
    /// `model_repo_sha256` (digest-of-digests over the file set).
    async fn ensure_model(&self, ctx: &EngineContext) -> Result<()> {
        if ctx
            .model
            .as_deref()
            .map(|m| !m.trim().is_empty())
            .unwrap_or(false)
        {
            return Ok(());
        }
        let Some(repo) = ctx
            .model_repo
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty())
        else {
            // No managed repo declared: the launcher's resolve_model_path emits
            // the precise "requires model or model_repo" error.
            return Ok(());
        };
        if !crate::foundation::types::manifest::is_safe_hf_repo(repo) {
            return Err(CapsuleError::Pack(format!(
                "`model_repo` must be a `<org>/<name>` Hugging Face id, got {repo:?}"
            )));
        }
        let revision = ctx
            .model_revision
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .ok_or_else(|| {
                CapsuleError::Pack(
                    "`model_repo` requires `model_revision` (an immutable 40-hex commit)"
                        .to_string(),
                )
            })?;
        if !crate::foundation::types::manifest::is_safe_hf_revision(revision) {
            return Err(CapsuleError::Pack(format!(
                "`model_revision` must be an immutable 40-hex commit (not a branch), got {revision:?}"
            )));
        }
        let repo_sha = ctx
            .model_repo_sha256
            .as_deref()
            .and_then(crate::foundation::types::manifest::normalize_model_sha256)
            .ok_or_else(|| {
                CapsuleError::Pack(
                    "`model_repo` requires a 64-char hex `model_repo_sha256`".to_string(),
                )
            })?;
        let spec = crate::resource::model_cache::HfRepoSpec {
            repo,
            revision,
            repo_sha256: &repo_sha,
            include: &ctx.model_repo_include,
            gated: ctx.model_repo_gated,
        };
        crate::resource::model_cache::ensure_model_repo(&spec)
            .await
            .map(|_| ())
    }

    /// PURE (no network). SGLang/CUDA platform rows for `ato doctor
    /// native-inference`: the NVIDIA GPU, the driver, the CUDA runtime, and the
    /// resulting engine readiness, built from the already-probed `host.gpu`. The
    /// full `nvidia-cuda` provision/doctor command is a separate increment; these
    /// rows are what `ato doctor` aggregates.
    fn doctor_checks(&self, host: &HostCapabilities) -> Vec<EngineCheck> {
        let mut results = Vec::new();

        // 1. Platform.
        if host.os == "linux" {
            results.push(EngineCheck {
                name: "sglang.platform",
                status: EngineCheckStatus::Ok,
                detail: "linux — the SGLang CUDA engine is supported on Linux NVIDIA hosts"
                    .to_string(),
                recommendation: None,
            });
        } else {
            results.push(EngineCheck {
                name: "sglang.platform",
                status: EngineCheckStatus::Fail,
                detail: format!(
                    "{}: engine=\"sglang\" is Linux + CUDA only (no managed build)",
                    host.os
                ),
                recommendation: Some(
                    "Run sglang capsules on a Linux NVIDIA host, or set an explicit engine_path.",
                ),
            });
        }

        // 2. GPU + driver + CUDA runtime + readiness, from the probed profile.
        match host.gpu.as_ref() {
            Some(profile) if profile.native_inference_cuda_ready() => {
                results.push(EngineCheck {
                    name: "sglang.cuda",
                    status: EngineCheckStatus::Ok,
                    detail:
                        "CUDA ready — NVIDIA GPU + driver + CUDA runtime + python/venv detected"
                            .to_string(),
                    recommendation: None,
                });
            }
            Some(profile) if profile.has_gpu() && profile.driver_installed() => {
                results.push(EngineCheck {
                    name: "sglang.cuda",
                    status: EngineCheckStatus::Warn,
                    detail:
                        "NVIDIA GPU + driver present but the CUDA runtime / python venv is not \
                             ready — sglang cannot run yet"
                            .to_string(),
                    recommendation: Some(
                        "Run `sudo ato runner provision --profile nvidia-cuda`, then \
                         `ato runner doctor --profile nvidia-cuda`.",
                    ),
                });
            }
            Some(profile) if profile.has_gpu() => {
                results.push(EngineCheck {
                    name: "sglang.cuda",
                    status: EngineCheckStatus::Warn,
                    detail: "NVIDIA GPU detected but the driver is not installed — sglang cannot run"
                        .to_string(),
                    recommendation: Some(
                        "Run `sudo ato runner provision --profile nvidia-cuda` to install the driver + CUDA runtime.",
                    ),
                });
            }
            Some(_) => results.push(EngineCheck {
                name: "sglang.cuda",
                status: EngineCheckStatus::Fail,
                detail: "No NVIDIA GPU detected — engine=\"sglang\" requires a CUDA GPU"
                    .to_string(),
                recommendation: Some("Use a Linux NVIDIA host for sglang capsules."),
            }),
            None => results.push(EngineCheck {
                name: "sglang.cuda",
                status: EngineCheckStatus::Na,
                detail: "GPU not probed on this host (sglang requires a Linux NVIDIA CUDA host)"
                    .to_string(),
                recommendation: None,
            }),
        }

        results
    }
}
