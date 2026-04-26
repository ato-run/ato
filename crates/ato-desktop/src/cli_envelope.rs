//! Wire-format DTOs and parser for `ato-cli` JSON error events.
//!
//! When `ato-cli` is invoked with `--json` and an `AtoExecutionError`
//! escapes its `main_entry`, the error is serialized as a single line
//! of JSON to **stderr** by `apps/ato-cli/src/utils/error.rs::
//! emit_ato_error_jsonl`. The shape is FLAT (no `schema_version` /
//! `error` wrapper) and discriminated by `level: "fatal"`. Example:
//!
//! ```jsonc
//! {"level":"fatal","code":"E103","name":"missing_required_env",
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
        if let Ok(event) = serde_json::from_str::<AtoCliErrorEventDto>(line) {
            if event.level == "fatal" {
                return Some(event);
            }
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
}
