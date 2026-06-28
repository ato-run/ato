//! Native-inference engine abstraction.
//!
//! A `runtime = "native-inference"` target lowers to a host-native server
//! process. This module is the single dispatch point that maps the manifest
//! `engine` field to an [`Engine`] implementation, and the single place that
//! reads the `target_engine_*` / `target_model*` manifest accessors into an
//! [`EngineContext`]. Increment 1 ships llama.cpp behind this trait with zero
//! behavior change.

mod engine;
mod llamacpp;
mod sglang;
#[cfg(test)]
mod tests;

pub use engine::{
    Engine, EngineCheck, EngineCheckStatus, EngineContext, EngineId, HostCapabilities, VariantPlan,
};

use crate::router::ManifestData;

/// The ONLY engine-string dispatch site.
///
/// Resolution order (preserving today's `engine_path`-override-wins behavior):
///  1. A recognized `engine` string → that engine.
///  2. Else an explicit local `engine_path` (override) with no/unknown `engine`
///     → llama.cpp's launch shape (the historical default: a local engine
///     binary launched as `<engine_path> -m <model> --host 127.0.0.1`).
///  3. Else `None` — callers treat it as a no-op and let the launcher emit the
///     canonical "requires engine_path or engine + engine_version" error.
pub fn resolve_engine(plan: &ManifestData) -> Option<Box<dyn Engine>> {
    if let Some(id) = plan
        .target_engine()
        .as_deref()
        .and_then(EngineId::from_manifest)
    {
        return Some(engine_for(id));
    }
    // `engine_path` override with no recognized `engine`: the engine half is
    // irrelevant (the path IS the command) and the launch argv is the historical
    // llama.cpp shape, so dispatch to llama.cpp.
    if plan
        .target_engine_path()
        .map(|p| !p.trim().is_empty())
        .unwrap_or(false)
    {
        return Some(engine_for(EngineId::LlamaCpp));
    }
    None
}

/// Construct the [`Engine`] for an [`EngineId`]. Used by host-level probes that
/// have no manifest (e.g. `ato doctor native-inference`, which diagnoses the
/// canonical llama.cpp engine).
pub fn engine_for(id: EngineId) -> Box<dyn Engine> {
    match id {
        EngineId::LlamaCpp => Box::new(llamacpp::LlamaCppEngine),
        EngineId::SgLang => Box::new(sglang::SgLangEngine),
    }
}

/// Build the resolved [`EngineContext`] for `engine` from the manifest.
///
/// This is the one place that reads the native-inference manifest accessors. The
/// variant is resolved with the NON-GATING [`Engine::cache_variant_plan`] (so the
/// launcher and the cache path never error on platform/readiness); the
/// fail-closed gate is `plan_variant`, run by the ensure-step against a probed
/// host. The raw variant string is carried so that gate can run later.
pub fn engine_context(plan: &ManifestData, engine: &dyn Engine) -> EngineContext {
    let variant_raw = plan
        .target_engine_variant()
        .filter(|v| !v.trim().is_empty());
    let variant = engine.cache_variant_plan(variant_raw.as_deref());
    EngineContext {
        target: plan.selected_target_label().to_string(),
        engine_path: plan.target_engine_path().filter(|v| !v.trim().is_empty()),
        engine_version: plan
            .target_engine_version()
            .filter(|v| !v.trim().is_empty()),
        variant_raw,
        variant,
        model: plan.target_model().filter(|v| !v.trim().is_empty()),
        model_url: plan.target_model_url().filter(|v| !v.trim().is_empty()),
        model_sha256: plan.target_model_sha256().filter(|v| !v.trim().is_empty()),
        model_repo: plan.target_model_repo().filter(|v| !v.trim().is_empty()),
        model_revision: plan
            .target_model_revision()
            .filter(|v| !v.trim().is_empty()),
        model_repo_sha256: plan
            .target_model_repo_sha256()
            .filter(|v| !v.trim().is_empty()),
        model_repo_include: plan.target_model_repo_include(),
        model_repo_gated: plan.target_model_repo_gated(),
        server_args: plan.target_server_args(),
    }
}
