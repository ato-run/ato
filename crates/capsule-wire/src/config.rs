//! Dynamic config-form schema returned in the E103 missing-env envelope.
//!
//! Producer (`ato-cli`) emits these when a capsule reports unresolved
//! required env vars; consumer (`ato-desktop`) renders them as a dynamic
//! form. Sharing the canonical struct here makes silent drift between the
//! two sides impossible (M5 — wire-shape unification).
//!
//! Serialization contract (DO NOT change without coordinating producer +
//! consumer + spec docs):
//!
//! - `ConfigKind` uses internal tagging (`#[serde(tag = "kind")]`) with
//!   `rename_all = "snake_case"`. The `kind` discriminator and any
//!   variant-specific fields (`choices` for `Enum`) appear flattened into
//!   the outer object.
//! - `ConfigField` flattens the kind via `#[serde(flatten)]` so the TOML
//!   source reads naturally:
//!
//!   ```toml
//!   [[targets.main.config_schema]]
//!   name = "MODEL"
//!   kind = "enum"
//!   choices = ["gpt-4", "gpt-5"]
//!   ```
//!
//! - All optional fields use `#[serde(default, skip_serializing_if =
//!   "Option::is_none")]` so absent fields round-trip cleanly.
//! - Unrecognised `kind` values deserialize to [`ConfigKind::Unknown`]
//!   (`#[serde(other)]`) so an older consumer degrades a single field
//!   from a newer producer instead of rejecting the whole schema array.

use serde::{Deserialize, Serialize};

/// Kind of a user-facing config field. Drives per-kind UI rendering on the
/// desktop (masked input for `Secret`, dropdown for `Enum`, etc.) and
/// downstream persistence (secrets go to the `SecretStore`; others to a
/// capsule-scoped `.env` file).
///
/// Serialized with internal tagging under the `kind` discriminator so the
/// flattened TOML form reads naturally:
///
/// ```toml
/// [[targets.main.config_schema]]
/// name = "OPENAI_API_KEY"
/// kind = "secret"
/// label = "OpenAI API Key"
/// ```
///
/// ```toml
/// [[targets.main.config_schema]]
/// name = "MODEL"
/// kind = "enum"
/// choices = ["gpt-4", "gpt-5"]
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ConfigKind {
    /// Write-only secret. Masked in UI and stored in the SecretStore.
    #[default]
    Secret,
    /// Free-form string.
    String,
    /// Numeric value.
    Number,
    /// One-of selection.
    Enum { choices: Vec<String> },
    /// Forward-compat fallback: any `kind` discriminator this build does
    /// not recognise (e.g. emitted by a newer `ato-cli` across a
    /// CLI/desktop version skew). `#[serde(other)]` catches the unknown
    /// tag so a single unrecognised field degrades to a read-only
    /// "unsupported" input instead of failing the whole `missing_schema`
    /// array (and with it the entire dynamic config form). Any
    /// variant-specific fields the newer kind carries are dropped on
    /// deserialization. Never emitted by this build's producers.
    #[serde(other)]
    Unknown,
}

/// Rich metadata for a single config input surfaced by the capsule. When a
/// capsule populates `config_schema` on a target, the desktop uses this
/// metadata to render a dynamic form (label/description/placeholder/default)
/// instead of a bare env-var name.
///
/// `config_schema` is additive alongside the legacy `required_env: Vec<String>`
/// list — the resolver (`NamedTarget::resolved_config_schema`) prefers
/// `config_schema` when non-empty and otherwise derives default
/// `ConfigKind::Secret` entries from `required_env`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigField {
    /// Environment variable name used at runtime (e.g. `OPENAI_API_KEY`).
    pub name: String,
    /// Human-readable label for the UI (e.g. "OpenAI API Key").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Helper text rendered under the input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Input kind + kind-specific data, flattened into the outer table so
    /// `kind = "enum"` sits next to `choices = [...]` in the TOML source.
    #[serde(flatten)]
    pub kind: ConfigKind,
    /// Optional default value prefilled in the form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// Optional placeholder hint shown in empty inputs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_kinds_round_trip() {
        let json = r#"{"name":"MODEL","kind":"enum","choices":["gpt-4","gpt-5"]}"#;
        let field: ConfigField = serde_json::from_str(json).expect("must parse");
        assert_eq!(field.name, "MODEL");
        assert_eq!(
            field.kind,
            ConfigKind::Enum {
                choices: vec!["gpt-4".to_string(), "gpt-5".to_string()],
            }
        );
        let back = serde_json::to_value(&field).expect("must serialize");
        assert_eq!(back["kind"], "enum");
        assert_eq!(back["choices"][1], "gpt-5");
    }

    #[test]
    fn unknown_kind_falls_back_instead_of_failing() {
        // A newer producer may emit kinds this build has never heard of,
        // including variant-specific payload fields ("rows" here). The
        // field must degrade to `Unknown`, not error.
        let json = r#"{"name":"PROMPT","label":"Prompt","kind":"multiline","rows":4}"#;
        let field: ConfigField = serde_json::from_str(json).expect("unknown kind must not fail");
        assert_eq!(field.name, "PROMPT");
        assert_eq!(field.label.as_deref(), Some("Prompt"));
        assert_eq!(field.kind, ConfigKind::Unknown);
    }

    #[test]
    fn one_unknown_kind_does_not_drop_sibling_fields() {
        // Regression for #651: a single unknown `kind` used to fail the
        // whole `missing_schema` array, which made the desktop drop the
        // entire dynamic config form.
        let json = r#"[
            {"name":"OPENAI_API_KEY","kind":"secret"},
            {"name":"PROMPT","kind":"multiline"},
            {"name":"MODEL","kind":"enum","choices":["gpt-4"]}
        ]"#;
        let fields: Vec<ConfigField> = serde_json::from_str(json).expect("array must parse");
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].kind, ConfigKind::Secret);
        assert_eq!(fields[1].kind, ConfigKind::Unknown);
        assert!(matches!(fields[2].kind, ConfigKind::Enum { .. }));
    }

    #[test]
    fn unknown_round_trips_as_unknown() {
        // `Unknown` serializes under the same internal tag and lands back
        // on the `#[serde(other)]` arm — re-serialization by a consumer
        // stays parseable rather than poisoning the wire.
        let field = ConfigField {
            name: "X".to_string(),
            label: None,
            description: None,
            kind: ConfigKind::Unknown,
            default: None,
            placeholder: None,
        };
        let json = serde_json::to_string(&field).expect("must serialize");
        let back: ConfigField = serde_json::from_str(&json).expect("must re-parse");
        assert_eq!(back.kind, ConfigKind::Unknown);
    }
}
