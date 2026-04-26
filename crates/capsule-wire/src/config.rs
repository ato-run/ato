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
