//! Boundary-level execution-receipt emission (refs #74, #99).
//!
//! [`emit_receipt_on_result`] wraps a top-level command future
//! (`ato run`, `ato app session start`) and observes its `Result`. On
//! the **happy path** the inner pipeline already wrote a full v2
//! receipt to `~/.ato/executions/<id>/receipt.json` (see
//! `application::execution_receipt_builder::build_prelaunch_receipt_v2`),
//! so the wrapper is a no-op for `Ok(_)`. On the **recoverable-failure
//! / aborted** path the wrapper synthesizes a *partial* receipt with
//! [`ExecutionReceiptV2::partial_failure`] and writes it through the
//! same atomic-write helper that the success path uses.
//!
//! The point of the wrapper is that emission is a side effect of the
//! boundary, not a step inside each phase. Phase code keeps returning
//! `Result`; the wrapper observes the result and emits.
//!
//! ## Classification
//!
//! Errors are classified into [`ReceiptResultClass`] by inspecting the
//! `anyhow::Error` chain:
//!
//! | `AtoErrorPhase`                    | Classification         |
//! |------------------------------------|------------------------|
//! | `Manifest` / `Inference`           | `RecoverableFailure`   |
//! | `Provisioning` / `Execution`       | `RecoverableFailure`   |
//! | `Internal`                         | `Aborted`              |
//! | no `AtoExecutionError` in chain    | (no receipt emitted)   |
//!
//! When the chain has no typed envelope (raw `anyhow::anyhow!` errors,
//! plain strings, etc.) the wrapper deliberately skips emission: a
//! receipt with only `result: aborted` and no diagnostic envelope is
//! worse than no receipt because consumers would assume the runtime
//! produced it. Recovering classification for these cases is a future
//! refactor — until then, the original unwrapped error is the only
//! diagnostic surface.

use std::future::Future;

use anyhow::Result;
use capsule_core::execution_identity::{
    ExecutionReceiptDocument, ExecutionReceiptV2, ReceiptFailureEnvelope, ReceiptFailureKind,
    ReceiptResultClass,
};
use capsule_core::execution_plan::error::AtoExecutionError;

use crate::application::execution_receipts::write_receipt_document_atomic;
#[cfg(test)]
use crate::application::execution_receipts::write_receipt_document_atomic_at;

/// Boundary identification for diagnostic messages.
///
/// In PR-5 the wrapper has no surface that needs to thread graph state
/// through to the partial receipt — by the time the inner pipeline
/// returns `Err`, any graph that was built has been dropped. The
/// struct exists so the boundary signature is stable when future waves
/// add fields (e.g. a writable handle for declared/resolved ids
/// populated mid-pipeline).
#[derive(Debug, Clone, Default)]
pub(crate) struct ReceiptEmissionContext {
    /// Human-readable label for the boundary (e.g. `"ato run"`,
    /// `"ato app session start"`). Surfaces only in the
    /// `ATO-WARN` diagnostic when receipt write fails — never serialized.
    pub(crate) boundary: &'static str,
}

impl ReceiptEmissionContext {
    pub(crate) fn for_boundary(boundary: &'static str) -> Self {
        Self { boundary }
    }
}

/// Wrap a boundary-level future and emit an execution receipt on
/// failure. On success, the inner pipeline already emitted its own
/// happy-path receipt (existing #74-PR4 behavior); the wrapper
/// observes the `Ok(_)` and returns it unchanged.
///
/// Receipt-emission failures are best-effort: a `tracing`-style
/// diagnostic is emitted on stderr but the original error from the
/// inner future is always returned. Hiding the failure under a write
/// error would mask the actual user-visible problem.
pub(crate) async fn emit_receipt_on_result<F, T>(ctx: ReceiptEmissionContext, inner: F) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    let outcome = inner.await;
    if let Err(error) = outcome.as_ref() {
        if let Some(receipt) = partial_receipt_for_error(error) {
            match write_receipt_document_atomic(&ExecutionReceiptDocument::V2(receipt.clone())) {
                Ok(path) => eprintln!(
                    "Execution receipt (v2-experimental, {}): {} ({})",
                    receipt_result_label(receipt.result),
                    receipt.execution_id,
                    path.display()
                ),
                Err(write_err) => {
                    eprintln!(
                        "ATO-WARN failed to write partial execution receipt for {} boundary: {write_err}",
                        ctx.boundary
                    );
                }
            }
        }
        // No envelope means we couldn't classify the error (e.g. plain
        // string, `anyhow::anyhow!`-only). We deliberately do NOT emit
        // a partial receipt in that case: there's no typed envelope to
        // record, and a receipt with only `result: aborted` and no
        // diagnostic is worse than no receipt because consumers would
        // think it was emitted by the runtime rather than by the
        // wrapper.
    }
    outcome
}

/// Variant of [`emit_receipt_on_result`] that writes the partial
/// receipt under `root` instead of `~/.ato/executions/`. Used by the
/// crate's own tests to verify the write side without touching the
/// developer's real receipt store.
#[cfg(test)]
pub(crate) async fn emit_receipt_on_result_at<F, T>(
    ctx: ReceiptEmissionContext,
    root: &std::path::Path,
    inner: F,
) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    let outcome = inner.await;
    if let Err(error) = outcome.as_ref() {
        if let Some(receipt) = partial_receipt_for_error(error) {
            if let Err(write_err) =
                write_receipt_document_atomic_at(root, &ExecutionReceiptDocument::V2(receipt))
            {
                eprintln!(
                    "ATO-WARN failed to write partial execution receipt for {} boundary: {write_err}",
                    ctx.boundary
                );
            }
        }
    }
    outcome
}

/// Build a partial v2 receipt for an `anyhow::Error`, or `None` when the
/// error chain has no recognizable typed envelope. Pure function — no
/// I/O — so callers and tests can compose it freely with the write
/// step.
pub(crate) fn partial_receipt_for_error(error: &anyhow::Error) -> Option<ExecutionReceiptV2> {
    let envelope = build_failure_envelope(error)?;
    let result_class = match envelope.kind {
        ReceiptFailureKind::Recoverable => ReceiptResultClass::RecoverableFailure,
        ReceiptFailureKind::Aborted => ReceiptResultClass::Aborted,
    };
    Some(ExecutionReceiptV2::partial_failure(
        chrono::Utc::now().to_rfc3339(),
        result_class,
        envelope,
        None, // declared_execution_id — see ReceiptEmissionContext docs
        None, // resolved_execution_id — see ReceiptEmissionContext docs
        None, // local locator — partial receipts don't surface paths
    ))
}

/// Build a typed [`ReceiptFailureEnvelope`] from an `anyhow::Error` by
/// downcasting to the typed error envelope. Returns `None` when the
/// error chain has no recognizable variant.
pub(crate) fn build_failure_envelope(error: &anyhow::Error) -> Option<ReceiptFailureEnvelope> {
    if let Some(execution_error) = downcast_execution_error(error) {
        return Some(envelope_from_execution_error(execution_error));
    }
    None
}

fn downcast_execution_error(error: &anyhow::Error) -> Option<&AtoExecutionError> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<AtoExecutionError>())
}

fn envelope_from_execution_error(error: &AtoExecutionError) -> ReceiptFailureEnvelope {
    let kind = classify_phase(error.phase);
    // `AtoErrorCode::retryable()` returns true for some codes that
    // map to `Aborted` (e.g. `AtoErrInternal`). An `Aborted` envelope
    // can't be retried by definition — the user can't meaningfully
    // act on it without external intervention — so force
    // `retryable: false` to keep `kind` and `retryable` self-consistent.
    let retryable = match kind {
        ReceiptFailureKind::Aborted => false,
        ReceiptFailureKind::Recoverable => error.retryable,
    };
    ReceiptFailureEnvelope {
        kind,
        code: error.code.to_string(),
        name: error.name.to_string(),
        phase: error.phase.to_string(),
        message: error.message.clone(),
        hint: error.hint.clone(),
        resource: error.resource.clone(),
        target: error.target.clone(),
        retryable,
        interactive_resolution_required: error.interactive_resolution_required.clone(),
        classification: Some(error.classification),
        cleanup_status: error.cleanup_status,
        cleanup_actions: error.cleanup_actions.clone(),
        manifest_suggestion: error.manifest_suggestion.clone(),
        details: error.details.clone(),
    }
}

fn receipt_result_label(result: ReceiptResultClass) -> &'static str {
    match result {
        ReceiptResultClass::Passed => "passed",
        ReceiptResultClass::RecoverableFailure => "recoverable-failure",
        ReceiptResultClass::Aborted => "aborted",
    }
}

/// Map an `AtoExecutionError` phase string to a [`ReceiptFailureKind`].
///
/// Phases:
/// - `manifest` / `inference` / `provisioning` / `execution` →
///   `Recoverable` (user can fix and retry).
/// - `internal` → `Aborted` (`AtoErrorPhase::Internal` indicates a
///   bug or precondition violation the user can't meaningfully act on).
/// - anything else → `Recoverable` (default per the brief's
///   "fuzzy → recoverable" instruction).
fn classify_phase(phase: &str) -> ReceiptFailureKind {
    match phase {
        "internal" => ReceiptFailureKind::Aborted,
        _ => ReceiptFailureKind::Recoverable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule_core::engine::execution_plan::error::AtoErrorCode;
    use capsule_core::execution_plan::error::AtoErrorClassification;

    fn execution_error(code: AtoErrorCode) -> AtoExecutionError {
        AtoExecutionError::new(code, "fixture failure", None, None, None)
    }

    #[test]
    fn inference_phase_classified_as_recoverable() {
        let envelope =
            envelope_from_execution_error(&execution_error(AtoErrorCode::AtoErrMissingRequiredEnv));
        assert_eq!(envelope.kind, ReceiptFailureKind::Recoverable);
        assert_eq!(envelope.phase, "inference");
        assert_eq!(envelope.name, "missing_required_env");
    }

    #[test]
    fn provisioning_phase_classified_as_recoverable() {
        let envelope = envelope_from_execution_error(&execution_error(
            AtoErrorCode::AtoErrProvisioningLockIncomplete,
        ));
        assert_eq!(envelope.kind, ReceiptFailureKind::Recoverable);
        assert_eq!(envelope.phase, "provisioning");
    }

    #[test]
    fn execution_phase_classified_as_recoverable() {
        let envelope =
            envelope_from_execution_error(&execution_error(AtoErrorCode::AtoErrRuntimeNotResolved));
        assert_eq!(envelope.kind, ReceiptFailureKind::Recoverable);
        assert_eq!(envelope.phase, "execution");
    }

    #[test]
    fn internal_phase_classified_as_aborted() {
        let envelope =
            envelope_from_execution_error(&execution_error(AtoErrorCode::AtoErrInternal));
        assert_eq!(envelope.kind, ReceiptFailureKind::Aborted);
        assert_eq!(envelope.phase, "internal");
    }

    #[test]
    fn envelope_carries_typed_resolution_and_classification_fields() {
        let error = AtoExecutionError::missing_required_env(
            "missing SECRET_KEY",
            vec!["SECRET_KEY".to_string()],
            Vec::new(),
            Some("app"),
        );
        let envelope = envelope_from_execution_error(&error);

        assert!(
            envelope.interactive_resolution_required.is_some(),
            "partial receipt envelope should preserve the desktop/agent resolution payload"
        );
        assert_eq!(
            envelope.classification,
            Some(AtoErrorClassification::Manifest)
        );
    }

    /// `AtoErrInternal` has `retryable() == true` on the typed error
    /// side, but an `Aborted` envelope is by definition not retryable —
    /// the user can't act on it without external intervention. Pin
    /// the `Aborted` override so envelope `kind` and `retryable` stay
    /// self-consistent.
    #[test]
    fn aborted_envelope_overrides_retryable_to_false() {
        let mut error = execution_error(AtoErrorCode::AtoErrInternal);
        error.retryable = true; // simulate the typed error claiming retryable=true
        assert!(
            error.retryable,
            "fixture sanity: typed error claims retryable=true"
        );

        let envelope = envelope_from_execution_error(&error);
        assert_eq!(envelope.kind, ReceiptFailureKind::Aborted);
        assert!(
            !envelope.retryable,
            "Aborted envelope must override retryable to false regardless of the underlying typed error"
        );
    }

    /// Recoverable envelopes preserve the underlying typed `retryable`
    /// flag — only `Aborted` triggers the override. Pins the inverse
    /// of `aborted_envelope_overrides_retryable_to_false`.
    #[test]
    fn recoverable_envelope_preserves_typed_retryable() {
        let mut error = execution_error(AtoErrorCode::AtoErrRuntimeLaunchFailed);
        error.retryable = true;
        let envelope = envelope_from_execution_error(&error);
        assert_eq!(envelope.kind, ReceiptFailureKind::Recoverable);
        assert!(
            envelope.retryable,
            "Recoverable envelope must pass typed retryable through"
        );
    }

    #[test]
    fn build_failure_envelope_traverses_anyhow_chain() {
        let inner = execution_error(AtoErrorCode::AtoErrMissingRequiredEnv);
        let wrapped: anyhow::Error = anyhow::Error::new(inner).context("outer wrapper");
        let envelope = build_failure_envelope(&wrapped).expect("envelope");
        assert_eq!(envelope.name, "missing_required_env");
        assert_eq!(envelope.phase, "inference");
    }

    #[test]
    fn build_failure_envelope_returns_none_for_plain_anyhow_error() {
        let plain = anyhow::anyhow!("not a typed error");
        assert!(build_failure_envelope(&plain).is_none());
    }

    /// Boundary wrapper observes a typed `Err` and writes a partial
    /// receipt under the test root. Pins the integration shape: file
    /// path, schema version, `result`, and `failure_envelope` content.
    #[tokio::test]
    async fn wrapper_emits_partial_receipt_on_recoverable_failure() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ctx = ReceiptEmissionContext::for_boundary("test boundary");

        let outcome: Result<()> = emit_receipt_on_result_at(ctx, temp.path(), async {
            Err::<(), _>(anyhow::Error::new(execution_error(
                AtoErrorCode::AtoErrMissingRequiredEnv,
            )))
        })
        .await;
        assert!(
            outcome.is_err(),
            "wrapper must propagate the original error"
        );

        // Find the receipt the wrapper wrote — synthetic id starts with `partial:`.
        let entries: Vec<_> = std::fs::read_dir(temp.path())
            .expect("read tempdir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            entries.len(),
            1,
            "wrapper must write exactly one receipt dir"
        );
        assert!(
            entries[0].starts_with("partial_blake3_"),
            "partial receipt dir name must reflect the synthetic id, got {entries:?}"
        );

        let receipt_path = temp.path().join(&entries[0]).join("receipt.json");
        let raw = std::fs::read_to_string(&receipt_path).expect("read receipt");
        let value: serde_json::Value = serde_json::from_str(&raw).expect("parse receipt");
        assert_eq!(
            value
                .get("schema_version")
                .and_then(serde_json::Value::as_u64),
            Some(2)
        );
        assert_eq!(
            value.get("result").and_then(serde_json::Value::as_str),
            Some("recoverable-failure")
        );
        let env = value
            .get("failure_envelope")
            .expect("failure_envelope present");
        assert_eq!(
            env.get("name").and_then(serde_json::Value::as_str),
            Some("missing_required_env")
        );
        assert_eq!(
            env.get("phase").and_then(serde_json::Value::as_str),
            Some("inference")
        );
        assert_eq!(
            env.get("classification")
                .and_then(serde_json::Value::as_str),
            Some("manifest")
        );
        assert_eq!(
            env.get("kind").and_then(serde_json::Value::as_str),
            Some("recoverable")
        );
    }

    /// Internal-phase failures are classified as `Aborted`. The
    /// `result` and `failure_envelope.kind` agree so consumers can
    /// route on either field.
    #[tokio::test]
    async fn wrapper_emits_aborted_receipt_for_internal_phase() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ctx = ReceiptEmissionContext::for_boundary("test boundary");

        let _: Result<()> = emit_receipt_on_result_at(ctx, temp.path(), async {
            Err::<(), _>(anyhow::Error::new(execution_error(
                AtoErrorCode::AtoErrInternal,
            )))
        })
        .await;

        let entry = std::fs::read_dir(temp.path())
            .expect("read tempdir")
            .next()
            .expect("at least one entry")
            .expect("entry");
        let receipt_path = entry.path().join("receipt.json");
        let raw = std::fs::read_to_string(&receipt_path).expect("read receipt");
        let value: serde_json::Value = serde_json::from_str(&raw).expect("parse");
        assert_eq!(
            value.get("result").and_then(serde_json::Value::as_str),
            Some("aborted")
        );
        let env = value.get("failure_envelope").expect("envelope");
        assert_eq!(
            env.get("kind").and_then(serde_json::Value::as_str),
            Some("aborted")
        );
    }

    /// Successful inner futures must NOT cause the wrapper to write a
    /// receipt. The happy-path receipt is the inner pipeline's
    /// responsibility (see `build_prelaunch_receipt_v2`); the wrapper
    /// is failure-only.
    #[tokio::test]
    async fn wrapper_does_not_write_receipt_on_success() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ctx = ReceiptEmissionContext::for_boundary("test boundary");

        let outcome: Result<u32> =
            emit_receipt_on_result_at(ctx, temp.path(), async { Ok(42) }).await;
        assert_eq!(outcome.expect("ok"), 42);

        let entries: Vec<_> = std::fs::read_dir(temp.path())
            .expect("read tempdir")
            .filter_map(|e| e.ok())
            .collect();
        assert!(
            entries.is_empty(),
            "wrapper must not write any receipt on success, found {entries:?}"
        );
    }

    /// Plain `anyhow::anyhow!` errors with no typed envelope produce
    /// no receipt: there's nothing diagnostic to record. This is
    /// intentional — see the rationale in `emit_receipt_on_result`.
    #[tokio::test]
    async fn wrapper_skips_receipt_for_untyped_errors() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ctx = ReceiptEmissionContext::for_boundary("test boundary");

        let _: Result<()> = emit_receipt_on_result_at(ctx, temp.path(), async {
            Err::<(), _>(anyhow::anyhow!("untyped failure"))
        })
        .await;

        let entries: Vec<_> = std::fs::read_dir(temp.path())
            .expect("read tempdir")
            .filter_map(|e| e.ok())
            .collect();
        assert!(
            entries.is_empty(),
            "wrapper must not write a receipt for untyped errors"
        );
    }
}
