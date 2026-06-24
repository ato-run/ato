//! Engine-abstraction core for the native-inference runtime.
//!
//! A native-inference target lowers to a host-native server process (e.g.
//! `llama-server`). Historically every engine-specific decision — which binary
//! to resolve, which argv to build, which acceleration variant to fetch, which
//! cache key to use — was a string-match on the manifest `engine` field spread
//! across `launch_spec.rs`, `run.rs`, the runtime fetcher, and the doctor.
//!
//! This module collapses all of that into a single [`Engine`] trait dispatched
//! once by [`super::resolve_engine`]. Increment 1 introduces the abstraction and
//! ports today's llama.cpp behavior behind it with ZERO behavior change;
//! additional engines (e.g. SGLang) are added in later increments by
//! implementing this trait and extending [`EngineId`].

use crate::error::{CapsuleError, Result};
use crate::foundation::host_gpu::HostGpuProfile;
use crate::packers::runtime_fetcher::RuntimeFetcher;

/// Canonical identity of a native-inference engine.
///
/// Increment 1 ships llama.cpp only; the enum is the single place new engines
/// are registered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineId {
    LlamaCpp,
}

impl EngineId {
    /// Map a manifest `engine` string to an [`EngineId`].
    ///
    /// `None` = unknown/unsupported engine, so callers emit the canonical
    /// "requires engine_path or engine + engine_version" error (preserving
    /// today's behavior when an engine string isn't recognized).
    pub fn from_manifest(engine: &str) -> Option<Self> {
        match engine.trim().to_ascii_lowercase().as_str() {
            "llama.cpp" | "llamacpp" | "llama-cpp" => Some(Self::LlamaCpp),
            _ => None,
        }
    }

    /// Toolchain-cache key for this engine (the fetcher's canonical key).
    pub fn toolchain_key(self) -> &'static str {
        match self {
            Self::LlamaCpp => "llamacpp",
        }
    }
}

/// Acceleration plan produced by [`Engine::plan_variant`].
///
/// `slug = None` means the engine's default build (CPU on Linux/Windows, Metal
/// on macOS for llama.cpp); `slug = Some(..)` names a specific accelerated build
/// (e.g. `"vulkan"`) that is keyed separately in the toolchain cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantPlan {
    pub slug: Option<String>,
}

impl VariantPlan {
    /// The engine's default build (no accelerated variant slug).
    pub fn default_build() -> Self {
        Self { slug: None }
    }

    /// A named accelerated build (e.g. `"vulkan"`).
    pub fn named(slug: impl Into<String>) -> Self {
        Self {
            slug: Some(slug.into()),
        }
    }

    /// Borrow the slug as `Option<&str>` (the form the fetcher's cache-key
    /// helper consumes).
    pub fn as_deref(&self) -> Option<&str> {
        self.slug.as_deref()
    }
}

/// Host capability snapshot consumed by [`Engine::plan_variant`] and
/// [`Engine::doctor_checks`].
///
/// Wraps the existing [`HostGpuProfile`] predicates so the engine layer never
/// shells out itself — `plan_variant` stays pure and unit-testable.
///
/// Two construction paths, preserving today's launcher-vs-ensure split:
///  * [`HostCapabilities::unprobed`] (`probed = false`) — the launcher/preflight
///    path. The launcher never gates a variant (it only needs a deterministic
///    cache key via [`Engine::cache_variant_plan`]); the readiness-gated
///    `plan_variant` branches return their slug WITHOUT gating here.
///  * [`HostCapabilities::from_profile`] (`probed = true`) — the ensure-step /
///    doctor path that applies the real fail-closed readiness gate. A failed
///    GPU probe (`gpu = None` while `probed = true`) is "not ready" → fail
///    closed, matching the historical ensure-step (which treated a detection
///    error as not-ready).
pub struct HostCapabilities {
    pub os: String,
    pub arch: String,
    pub gpu: Option<HostGpuProfile>,
    /// Whether the GPU was actually probed. `false` = the launcher path (no
    /// readiness gate); `true` = the ensure-step/doctor path (gate on real
    /// readiness, fail closed when the probe found nothing usable).
    pub probed: bool,
}

impl HostCapabilities {
    /// Launcher/preflight path: no GPU probe (readiness gating is deferred to
    /// the ensure-step).
    pub fn unprobed() -> Self {
        Self {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            gpu: None,
            probed: false,
        }
    }

    /// Ensure-step/doctor path: a (possibly absent) probed GPU profile.
    pub fn from_profile(gpu: Option<HostGpuProfile>) -> Self {
        Self {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            gpu,
            probed: true,
        }
    }

    /// Whether this host has a usable Vulkan-accelerated native-inference path
    /// (GPU + driver + loader + tool + visible device). `false` when the probe
    /// found nothing usable (or the host was not probed).
    pub fn vulkan_ready(&self) -> bool {
        self.gpu
            .as_ref()
            .map(|p| p.native_inference_vulkan_ready())
            .unwrap_or(false)
    }
}

/// Everything an engine needs from the manifest, resolved once by the router
/// shim ([`super::engine_context`]) so individual [`Engine`] methods don't reach
/// back into [`crate::router::ManifestData`].
///
/// Increment 1 carries only the fields llama.cpp uses; later increments extend
/// this for additional engines.
#[derive(Debug, Clone)]
pub struct EngineContext {
    /// Selected target label (for error messages).
    pub target: String,
    /// Local engine binary override (wins over managed resolution).
    pub engine_path: Option<String>,
    /// Managed engine version / build tag (e.g. llama.cpp `"b4231"`).
    pub engine_version: Option<String>,
    /// The raw manifest `engine_variant` (carried so the ensure-step can run the
    /// fail-closed [`Engine::plan_variant`] gate against a probed host).
    pub variant_raw: Option<String>,
    /// Non-gating cache-key acceleration plan (from
    /// [`Engine::cache_variant_plan`]). Drives the deterministic cache path so
    /// the launcher and fetcher always agree — the platform/readiness
    /// fail-closed is enforced separately by the ensure-step.
    pub variant: VariantPlan,
    /// Local model path override (wins over managed resolution).
    pub model: Option<String>,
    /// Managed single-file model URL (llama.cpp GGUF).
    pub model_url: Option<String>,
    /// Required SHA-256 of the managed model (cache key + integrity check).
    pub model_sha256: Option<String>,
}

/// Status of an engine-side doctor check (capsule layer, no CLI dependency).
/// The CLI maps these to its own `CheckStatus` for rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineCheckStatus {
    Ok,
    Warn,
    Fail,
    Na,
}

/// One engine-side doctor row. The CLI maps this to its `CheckResult` for
/// rendering (`ato doctor native-inference`).
#[derive(Debug, Clone)]
pub struct EngineCheck {
    pub name: &'static str,
    pub status: EngineCheckStatus,
    pub detail: String,
    pub recommendation: Option<&'static str>,
}

/// One native-inference backend.
///
/// Stateless: every method takes the already-resolved [`EngineContext`].
///
/// CONTRACT (mirrored from the existing llama.cpp behavior so the abstraction is
/// behavior-preserving):
///  * [`Engine::resolve_server_command`] and [`Engine::resolve_model_path`] MUST
///    be pure (no network, no filesystem existence check) — receipt/preflight
///    builders call them BEFORE the async ensure-step. The `ensure_*` methods
///    then GUARANTEE those exact paths exist by spawn time
///    (see `launch_spec.rs` resolution comments).
///  * [`Engine::build_server_argv`] MUST NOT include `--port`: the host launcher
///    injects it from the resolved/allocated port so readiness and the app_url
///    agree (`executors/source.rs`).
#[async_trait::async_trait]
pub trait Engine: Send + Sync {
    /// This engine's canonical identity.
    fn id(&self) -> EngineId;

    /// Conventional default port when the manifest omits one
    /// (llama.cpp / `llama-server` = 8080).
    fn default_port(&self) -> u16;

    /// PURE, NON-GATING. Map an `engine_variant` to its cache-key acceleration
    /// plan (default/cpu/metal/`""` → default build; anything else → a named
    /// slug verbatim). This is what the launcher and the cache path use — it
    /// NEVER errors on platform/readiness; that fail-closed is `plan_variant`'s
    /// job at the ensure-step. Reproduces the fetcher's `normalize_engine_variant`.
    fn cache_variant_plan(&self, variant: Option<&str>) -> VariantPlan;

    /// GATING. Map an `engine_variant` to an acceleration plan, or a precise
    /// fail-closed error string. This is the ensure-step / doctor gate.
    ///
    /// `host` gates on real readiness only when probed (`host.probed = true`): a
    /// GPU build must never silently fall back to CPU, and a failed probe is
    /// treated as not-ready (fail closed). When unprobed (`host.probed = false`)
    /// the readiness-gated branches return their slug WITHOUT gating — but the
    /// launcher uses [`Engine::cache_variant_plan`] instead, so the only callers
    /// of this method probe the host.
    fn plan_variant(
        &self,
        variant: Option<&str>,
        host: &HostCapabilities,
    ) -> std::result::Result<VariantPlan, String>;

    /// Deterministic toolchain-cache key for a `(version, variant)` pair, used
    /// by BOTH the fetcher and the launcher so the path they agree on never
    /// drifts.
    fn cache_key(&self, version: &str, variant: &VariantPlan) -> String;

    /// PURE. The server command for this target — an `engine_path` override, or
    /// the managed `(version, variant)` → cached binary path.
    fn resolve_server_command(&self, ctx: &EngineContext) -> Result<String>;

    /// PURE. The model argument value (a local path; a GGUF file for llama.cpp).
    fn resolve_model_path(&self, ctx: &EngineContext) -> Result<String>;

    /// PURE. The full server argv given the resolved model path, host, and port.
    /// MUST NOT contain `--port` (the launcher injects it). `port` is provided
    /// for engines that need it inside argv (none today; llama.cpp ignores it).
    fn build_server_argv(&self, model_path: &str, host: &str, port: u16) -> Vec<String>;

    /// ASYNC. Provision the managed engine (binary/venv) into its cache so the
    /// pure paths above exist by spawn time. A local `engine_path` short-circuits.
    /// MUST run the fail-closed [`Engine::plan_variant`] gate against the probed
    /// `host` before fetching (so an unsupported / not-ready accelerated variant
    /// fails closed rather than fetching the wrong build).
    async fn ensure_engine(
        &self,
        ctx: &EngineContext,
        host: &HostCapabilities,
        fetcher: &RuntimeFetcher,
    ) -> Result<()>;

    /// ASYNC. Download + verify the managed model into the content-addressed
    /// cache. The default implementation is today's `model_url` + `model_sha256`
    /// CAS path (llama.cpp); a local `model` short-circuits.
    async fn ensure_model(&self, ctx: &EngineContext) -> Result<()> {
        if ctx
            .model
            .as_deref()
            .map(|m| !m.trim().is_empty())
            .unwrap_or(false)
        {
            return Ok(());
        }
        let Some(url) = ctx
            .model_url
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
        else {
            // No managed model declared: the launcher's resolve_model_path
            // produces the precise "requires model or model_url" error.
            return Ok(());
        };
        let sha_raw = ctx
            .model_sha256
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| CapsuleError::Pack("`model_url` requires `model_sha256`".into()))?;
        let sha = crate::foundation::types::manifest::normalize_model_sha256(sha_raw)
            .ok_or_else(|| {
                CapsuleError::Pack("`model_sha256` must be a 64-char hex SHA-256".into())
            })?;
        crate::resource::model_cache::ensure_model(url, &sha)
            .await
            .map(|_| ())
    }

    /// PURE (no network). Platform/variant availability probe for
    /// `ato doctor native-inference`. Returns engine-named checks the CLI
    /// renders.
    fn doctor_checks(&self, host: &HostCapabilities) -> Vec<EngineCheck>;
}
