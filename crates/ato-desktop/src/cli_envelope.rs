//! Wire-format DTOs and parser for `ato-cli` JSON error events.
//!
//! When `ato-cli` is invoked with `--json` and an `AtoExecutionError`
//! escapes its `main_entry`, the error is serialized as a single line
//! of JSON to **stderr** by `apps/ato-cli/src/utils/error.rs::
//! emit_ato_error_jsonl`. The shape is FLAT (no `schema_version` /
//! `error` wrapper) and discriminated by `level: "fatal"`. Example:
//!
//! ```jsonc
//! {"level":"fatal","code":"ATO_ERR_MISSING_REQUIRED_ENV",
//!  "name":"missing_required_env",
//!  "phase":"inference","classification":"manifest",
//!  "message":"missing required environment variables...",
//!  "retryable":false,"interactive_resolution":true,
//!  "resource":"environment","target":"main",
//!  "hint":"...",
//!  "details":{
//!    "missing_keys":["OPENAI_API_KEY"],
//!    "missing_schema":[{"name":"OPENAI_API_KEY","kind":"secret","label":"OpenAI API Key"}],
//!    "target":"main"}}
//! ```
//!
//! Note: prior drafts spelled the code as `E103`. The on-the-wire form
//! is now `ATO_ERR_MISSING_REQUIRED_ENV` (single-sourced from
//! `capsule_core::execution_plan::error::AtoErrorCode`); consumers that
//! need to discriminate should match on the stable `name` field
//! (`missing_required_env`) rather than `code`.
//!
//! # Wire-shape source of truth
//!
//! As of M5, `ConfigField` / `ConfigKind` are the canonical wire shape
//! for both the CLI emitter and the Desktop consumer. As of N3 they
//! live in the dedicated [`capsule_wire`] crate so the Desktop links
//! only the IPC surface and not capsule-core's runtime stack. The
//! contract test in `crates/ato-cli/src/adapters/output/diagnostics/
//! tests.rs::maps_missing_required_env_error_to_e103_with_schema`
//! pins the JSON shape on the CLI side; the Desktop test suite below
//! exercises the same shape against the same types.

use capsule_wire::config::ConfigField;
use capsule_wire::consent::{ConsentIdentity, ConsentRequiredDetails};
use serde::Deserialize;

/// `details` payload for E103 (`missing_required_env`). The desktop
/// reads `missing_schema` only — `missing_keys` is preserved here
/// for diagnostics/logging but must not be index-aligned with the
/// schema array (each `ConfigField` is self-describing via `name`).
#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
pub struct MissingEnvDetailsDto {
    #[serde(default)]
    pub missing_keys: Vec<String>,
    #[serde(default)]
    pub missing_schema: Vec<ConfigField>,
    #[serde(default)]
    pub target: Option<String>,
}

// The consent-required wire shape (`ConsentRequiredDetails`) and its routing
// discriminator (`capsule_wire::consent::CONSENT_REQUIRED_REASON`) now live
// in `capsule-wire`, single-sourced with the CLI producer and the runner's
// stdout consumer. The desktop deserializes into that shared type directly
// (see `consent_required_details` below) instead of mirroring the tuple.

/// Desktop projection of `apps/ato-cli/src/utils/error.rs::AtoErrorEvent`,
/// the on-the-wire shape for any `AtoExecutionError` under `--json`. The
/// desktop only deserializes the fields it needs and keeps `details` as
/// `serde_json::Value` so per-code payloads can be lazy-decoded.
#[derive(Debug, Clone, Deserialize)]
pub struct AtoCliErrorEventDto {
    pub level: String,
    pub code: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub hint: Option<String>,
    #[serde(default)]
    pub details: Option<serde_json::Value>,
}

impl AtoCliErrorEventDto {
    /// Decode `details` as the missing-env shape. Returns `None` if
    /// `details` is absent or shape-incompatible. Callers should treat
    /// `None` as "fall back to opaque error" — never as "no missing
    /// keys" (that case manifests as `Some(MissingEnvDetailsDto { .. })`
    /// with empty vectors, which is itself anomalous).
    pub fn missing_env_details(&self) -> Option<MissingEnvDetailsDto> {
        let value = self.details.clone()?;
        serde_json::from_value(value).ok()
    }

    /// Decode `details` as the consent-required shape, returning the
    /// validated [`ConsentIdentity`]. Returns `None` unless
    /// `details.reason == "execution_plan_consent_required"` AND every
    /// consent-key field is well-formed — both gates (applied by the
    /// shared `capsule-wire` validation) protect the caller from routing
    /// an unrelated or unsatisfiable E302 to the consent modal. Old E302
    /// envelopes (no `reason` field, or a generic `ExecutionContractInvalid`
    /// whose `details` lacks the identity tuple) yield `None` here — the
    /// strict wire shape fails to deserialize, or `consent_required`
    /// rejects it — so they fall through to the existing fatal-toast path.
    pub fn consent_required_details(&self) -> Option<ConsentIdentity> {
        let value = self.details.clone()?;
        let details: ConsentRequiredDetails = serde_json::from_value(value).ok()?;
        details.consent_required().cloned()
    }
}

/// Scan a captured stderr buffer for the LAST line that parses as a
/// "fatal" `AtoCliErrorEventDto`. Tolerates `tracing`/`log` plain-text
/// noise emitted before the structured event.
///
/// # Why reverse iteration
///
/// The CLI's stderr under `--json` is a mix:
/// * `tracing_subscriber::fmt` plain-text spans (`<ts> INFO ato_cli: ...`)
///   for the entire run, and
/// * exactly one trailing JSONL line (`{"level":"fatal",...}`) emitted
///   immediately before exit.
///
/// Iterating from the end + bailing on first valid match finds the
/// envelope in O(1) typical cases and never reads past the boundary
/// of the line that contains it. Multi-line pretty-printed JSON is
/// not produced by `emit_ato_error_jsonl` (`serde_json::to_string`,
/// not `to_string_pretty`), so single-line scan is sound.
pub fn parse_cli_error_event(stderr: &str) -> Option<AtoCliErrorEventDto> {
    for raw_line in stderr.lines().rev() {
        let line = raw_line.trim();
        // Cheap pre-filter: a line that doesn't begin with `{` cannot
        // be a JSON object, so skip the speculative serde call. This
        // matters when stderr is dominated by tracing's
        // `<ts> INFO <target>: <msg>` prefixes.
        if !line.starts_with('{') {
            continue;
        }
        if let Ok(event) = serde_json::from_str::<AtoCliErrorEventDto>(line)
            && event.level == "fatal"
        {
            return Some(event);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule_wire::config::ConfigKind;

    fn fatal_e103_line() -> &'static str {
        r#"{"level":"fatal","code":"E103","name":"missing_required_env","phase":"inference","classification":"manifest","message":"missing required environment variables for target 'main': OPENAI_API_KEY","retryable":false,"interactive_resolution":true,"resource":"environment","target":"main","hint":"set the variable before retrying.","details":{"missing_keys":["OPENAI_API_KEY"],"missing_schema":[{"name":"OPENAI_API_KEY","label":"OpenAI API Key","kind":"secret","placeholder":"sk-..."}],"target":"main"}}"#
    }

    #[test]
    fn parses_clean_envelope() {
        let event = parse_cli_error_event(fatal_e103_line()).expect("must parse");
        assert_eq!(event.code, "E103");
        assert_eq!(event.target.as_deref(), Some("main"));
        let details = event.missing_env_details().expect("details present");
        assert_eq!(details.missing_keys, vec!["OPENAI_API_KEY".to_string()]);
        assert_eq!(details.missing_schema.len(), 1);
        let field = &details.missing_schema[0];
        assert_eq!(field.name, "OPENAI_API_KEY");
        assert_eq!(field.label.as_deref(), Some("OpenAI API Key"));
        assert!(matches!(field.kind, ConfigKind::Secret));
        assert_eq!(field.placeholder.as_deref(), Some("sk-..."));
    }

    #[test]
    fn tolerates_tracing_prefix_noise() {
        let stderr = format!(
            "2026-04-25T10:32:11.001Z  INFO ato_cli: starting run\n\
             2026-04-25T10:32:14.002Z ERROR ato_cli::run: preflight failed\n\
             {}\n",
            fatal_e103_line()
        );
        let event = parse_cli_error_event(&stderr).expect("must extract last JSON line");
        assert_eq!(event.code, "E103");
    }

    #[test]
    fn picks_fatal_over_earlier_non_fatal_jsonl() {
        // If a future CLI feature emits non-fatal JSONL earlier in the
        // run, we must not pick it up as the terminating event.
        let stderr = format!(
            "{}\n{}\n",
            r#"{"level":"info","code":"P001","message":"progress"}"#,
            fatal_e103_line()
        );
        let event = parse_cli_error_event(&stderr).expect("must pick fatal");
        assert_eq!(event.level, "fatal");
        assert_eq!(event.code, "E103");
    }

    #[test]
    fn returns_none_for_garbage_input() {
        assert!(parse_cli_error_event("not json").is_none());
        assert!(parse_cli_error_event("").is_none());
        assert!(parse_cli_error_event("{ broken").is_none());
    }

    #[test]
    fn returns_none_when_only_non_fatal_jsonl_present() {
        let stderr = r#"{"level":"info","code":"P001","message":"progress"}"#;
        assert!(parse_cli_error_event(stderr).is_none());
    }

    #[test]
    fn parses_enum_kind_with_choices_and_default() {
        // Synthetic event exercising the flattened enum variant — the
        // Day 2 contract test on the CLI side guarantees this shape
        // matches what's emitted in production.
        let line = r#"{"level":"fatal","code":"E103","details":{"missing_schema":[{"name":"MODEL","kind":"enum","choices":["gpt-4","gpt-5"],"default":"gpt-4"}]}}"#;
        let event = parse_cli_error_event(line).expect("must parse");
        let details = event.missing_env_details().expect("details parse");
        assert_eq!(details.missing_schema.len(), 1);
        let field = &details.missing_schema[0];
        match &field.kind {
            ConfigKind::Enum { choices } => {
                assert_eq!(choices, &vec!["gpt-4".to_string(), "gpt-5".to_string()]);
            }
            other => panic!("expected Enum, got {other:?}"),
        }
        assert_eq!(field.default.as_deref(), Some("gpt-4"));
    }

    #[test]
    fn ignores_unknown_top_level_fields() {
        // Forward-compat: if the CLI adds new top-level keys, the
        // desktop must keep parsing the ones it cares about.
        let line = r#"{"level":"fatal","code":"E103","brand_new_field":42,"experimental":{"foo":"bar"},"details":{"missing_schema":[]}}"#;
        let event = parse_cli_error_event(line).expect("must parse despite unknown fields");
        assert_eq!(event.code, "E103");
    }

    fn fatal_e302_consent_required_line() -> &'static str {
        r#"{"level":"fatal","code":"ATO_ERR_EXECUTION_CONTRACT_INVALID","name":"execution_contract_invalid","phase":"execution","classification":"execution","message":"ExecutionPlan consent required for this capsule.","retryable":false,"interactive_resolution":true,"resource":"contract","target":"app","hint":"Desktop の承認モーダル...","details":{"reason":"execution_plan_consent_required","scoped_id":"wasedap2p-backend","version":"0.1.0","target_label":"app","policy_segment_hash":"blake3:aaa","provisioning_policy_hash":"blake3:bbb","summary":"Capsule: wasedap2p-backend@0.1.0\nTarget: app"}}"#
    }

    #[test]
    fn parses_consent_required_envelope() {
        let event = parse_cli_error_event(fatal_e302_consent_required_line())
            .expect("must parse fatal envelope");
        let details = event
            .consent_required_details()
            .expect("consent details present");
        assert_eq!(details.scoped_id, "wasedap2p-backend");
        assert_eq!(details.version, "0.1.0");
        assert_eq!(details.target_label, "app");
        assert_eq!(details.policy_segment_hash, "blake3:aaa");
        assert_eq!(details.provisioning_policy_hash, "blake3:bbb");
        assert!(
            !details.summary.is_empty(),
            "summary must be carried inline so no second CLI call is needed"
        );
    }

    #[test]
    fn consent_required_details_returns_none_for_unrelated_e302() {
        // Generic ExecutionContractInvalid (no `reason` discriminator)
        // must fall through — the desktop's existing fatal-toast
        // handler keeps owning these.
        let line = r#"{"level":"fatal","code":"ATO_ERR_EXECUTION_CONTRACT_INVALID","name":"execution_contract_invalid","details":{"field":"some.other.path","service":null}}"#;
        let event = parse_cli_error_event(line).expect("must parse envelope");
        assert!(
            event.consent_required_details().is_none(),
            "unrelated E302 must NOT route to the consent modal"
        );
    }

    #[test]
    fn consent_required_details_rejects_empty_identity_fields() {
        // A consent envelope with empty key fields is not actionable
        // (we can't round-trip it through approve-execution-plan), so
        // it must fall back to the fatal-toast path rather than open a
        // modal we can't satisfy.
        let line = r#"{"level":"fatal","code":"ATO_ERR_EXECUTION_CONTRACT_INVALID","details":{"reason":"execution_plan_consent_required","scoped_id":"","version":"","target_label":"","policy_segment_hash":"","provisioning_policy_hash":"","summary":""}}"#;
        let event = parse_cli_error_event(line).expect("must parse envelope");
        assert!(
            event.consent_required_details().is_none(),
            "empty identity fields must NOT yield consent details"
        );
    }
}
