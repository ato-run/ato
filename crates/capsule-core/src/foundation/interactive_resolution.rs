//! Typed payload describing an error that requires user-driven resolution.
//!
//! The legacy on-the-wire shape for E103 (`MissingRequiredEnv`) and E302
//! (`ExecutionPlanConsentRequired`) keeps each variant's per-field
//! representation in `AtoError::details()` (e.g. `details.missing_keys`
//! for E103, `details.reason == "execution_plan_consent_required"` plus
//! the five identity fields for E302). Existing desktop deserializers
//! (`MissingEnvDetailsDto`, `ConsentRequiredDetailsDto`) read those
//! per-variant shapes and must keep working — that is the backward-
//! compatibility contract this module preserves.
//!
//! This module adds an *additional* typed envelope, surfaced alongside
//! the legacy details, that represents both kinds under one Rust type.
//! It is the seed for the eventual aggregate envelope described in
//! issue #117 (one launch presents many requirements at once); for now
//! it always carries exactly one requirement at a time, so consumers
//! can begin migrating off the per-variant `details.reason` branching
//! without committing to the aggregate UX yet.
//!
//! Issue refs: #96 (this envelope), #117 (aggregate UX, deferred), #126
//! (the non-TTY emission path that already writes the legacy shape to
//! stderr — the new envelope rides alongside it on the same JSONL).

use serde::{Deserialize, Serialize};

use crate::types::ConfigField;

/// One unit of interactive resolution work the user must complete
/// before a launch can proceed.
///
/// Future expansion: when issue #117 lands, the aggregate envelope
/// will carry `Vec<InteractiveResolutionEnvelope>`, with `display`
/// fields merged into a single modal/TUI. Today every emission is a
/// single envelope.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InteractiveResolutionEnvelope {
    pub kind: InteractiveResolutionKind,
    pub display: ResolutionDisplay,
}

/// Discriminated union of supported interactive-resolution kinds.
///
/// Serialized with `tag = "type"`, value `snake_case`, so JSON looks
/// like `{ "type": "secrets_required", "target": "main", "schema": [...] }`
/// or `{ "type": "consent_required", "scoped_id": "...", ... }`.
///
/// Adding a new kind in the future is non-breaking for existing
/// consumers as long as they treat unknown `type` values as
/// "unsupported, fall back to legacy details".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InteractiveResolutionKind {
    /// Maps from `AtoError::MissingRequiredEnv` (E103). Carries the
    /// rich field schema the desktop renders in its dynamic config UI;
    /// `target` matches the manifest's target label that owns the
    /// missing env.
    SecretsRequired {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        schema: Vec<ConfigField>,
    },
    /// Maps from `AtoError::ExecutionPlanConsentRequired` (E302). The
    /// five identity fields are the exact arguments
    /// `ato internal consent approve-execution-plan` expects; the
    /// pre-rendered `summary` is the same human-readable text the
    /// CLI's TTY prompt and the desktop modal both display.
    ConsentRequired {
        scoped_id: String,
        version: String,
        target_label: String,
        policy_segment_hash: String,
        provisioning_policy_hash: String,
        summary: String,
    },
}

/// Pre-rendered presentation text shared across surfaces (CLI human
/// diagnostic, desktop modal, future TUI). The `message` is the short
/// one-line "what is required"; `hint` is the optional longer
/// "how to resolve" recipe.
///
/// We carry this on the envelope (rather than only on the error's
/// top-level `message` / `hint`) because the aggregate envelope in
/// #117 will need per-requirement display strings while the wrapping
/// error has only one `message` / `hint`. Today the values are simply
/// copied from the underlying `AtoError`'s message/hint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolutionDisplay {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AtoError;
    use capsule_wire::config::{ConfigField, ConfigKind};

    fn sample_config_field(name: &str) -> ConfigField {
        ConfigField {
            name: name.to_string(),
            label: Some(format!("Label for {name}")),
            description: None,
            kind: ConfigKind::Secret,
            default: None,
            placeholder: None,
        }
    }

    /// E103 → SecretsRequired. Locks the field mapping so future
    /// renames or restructures of `MissingRequiredEnv` fail loudly here
    /// instead of silently corrupting the envelope shape consumers
    /// (today: future #117 aggregate UX) will route on.
    #[test]
    fn envelope_for_missing_required_env_maps_to_secrets_required() {
        let err = AtoError::MissingRequiredEnv {
            message: "missing required env".to_string(),
            hint: Some("set OPENAI_API_KEY before retrying".to_string()),
            missing_keys: vec!["OPENAI_API_KEY".to_string()],
            missing_schema: vec![sample_config_field("OPENAI_API_KEY")],
            target: Some("main".to_string()),
        };

        let envelope = err
            .interactive_resolution_envelope()
            .expect("E103 must produce a typed envelope");

        match envelope.kind {
            InteractiveResolutionKind::SecretsRequired { target, schema } => {
                assert_eq!(target.as_deref(), Some("main"));
                assert_eq!(schema.len(), 1);
                assert_eq!(schema[0].name, "OPENAI_API_KEY");
            }
            other => panic!("expected SecretsRequired, got {other:?}"),
        }
        assert_eq!(envelope.display.message, "missing required env");
        assert_eq!(
            envelope.display.hint.as_deref(),
            Some("set OPENAI_API_KEY before retrying")
        );
    }

    /// E302 → ConsentRequired carrying the five identity fields the
    /// CLI's `ato internal consent approve-execution-plan` expects, plus
    /// the pre-rendered summary the desktop's modal already shows.
    #[test]
    fn envelope_for_consent_required_maps_all_identity_fields() {
        let err = AtoError::ExecutionPlanConsentRequired {
            message: "consent required".to_string(),
            hint: Some("approve via desktop modal".to_string()),
            scoped_id: "publisher/app".to_string(),
            version: "1.0.0".to_string(),
            target_label: "cli".to_string(),
            policy_segment_hash: "blake3:aaa".to_string(),
            provisioning_policy_hash: "blake3:bbb".to_string(),
            summary: "Capsule: publisher/app@1.0.0".to_string(),
        };

        let envelope = err
            .interactive_resolution_envelope()
            .expect("E302 must produce a typed envelope");

        match envelope.kind {
            InteractiveResolutionKind::ConsentRequired {
                scoped_id,
                version,
                target_label,
                policy_segment_hash,
                provisioning_policy_hash,
                summary,
            } => {
                assert_eq!(scoped_id, "publisher/app");
                assert_eq!(version, "1.0.0");
                assert_eq!(target_label, "cli");
                assert_eq!(policy_segment_hash, "blake3:aaa");
                assert_eq!(provisioning_policy_hash, "blake3:bbb");
                assert_eq!(summary, "Capsule: publisher/app@1.0.0");
            }
            other => panic!("expected ConsentRequired, got {other:?}"),
        }
        assert_eq!(envelope.display.message, "consent required");
    }

    /// Variants without a typed UI today must return None so consumers
    /// can route on `Option<envelope>` cleanly. The set returned by
    /// `interactive_resolution_envelope` is intentionally narrower than
    /// the `interactive_resolution: bool` flag — the latter signals
    /// "pause and show *something*"; the former requires an actual
    /// typed payload.
    #[test]
    fn envelope_for_internal_error_is_none() {
        let err = AtoError::InternalError {
            message: "boom".to_string(),
            hint: None,
            component: Some("dispatch".to_string()),
        };

        assert!(err.interactive_resolution_envelope().is_none());
    }

    /// Round-trip: serializing then deserializing the envelope must
    /// preserve every field, including the `type` discriminator on
    /// the kind union. Locks the wire shape across releases.
    #[test]
    fn envelope_serializes_and_deserializes_round_trip() {
        let original = InteractiveResolutionEnvelope {
            kind: InteractiveResolutionKind::ConsentRequired {
                scoped_id: "publisher/app".to_string(),
                version: "1.0.0".to_string(),
                target_label: "cli".to_string(),
                policy_segment_hash: "blake3:aaa".to_string(),
                provisioning_policy_hash: "blake3:bbb".to_string(),
                summary: "summary text".to_string(),
            },
            display: ResolutionDisplay {
                message: "consent required".to_string(),
                hint: Some("approve".to_string()),
            },
        };

        let json = serde_json::to_string(&original).expect("serialize");
        // The discriminator must serialize with `type` (snake_case
        // value) so external consumers can route without a Rust
        // dependency.
        assert!(
            json.contains(r#""type":"consent_required""#),
            "envelope must serialize the kind discriminator as `type`: {json}"
        );

        let round_tripped: InteractiveResolutionEnvelope =
            serde_json::from_str(&json).expect("deserialize");
        assert_eq!(round_tripped, original);
    }

    /// Backward-compat anchor for #96: the legacy E302 `details` shape
    /// the desktop's `ConsentRequiredDetailsDto` reads must remain
    /// deserializable from the typed envelope's `kind.consent_required`
    /// payload. We mimic the desktop's struct here (instead of pulling
    /// `ato-desktop` into the workspace, which is intentionally
    /// excluded for crates.io packaging reasons) to lock the structural
    /// contract: every field name + type the desktop expects must be
    /// emitted by `InteractiveResolutionKind::ConsentRequired`. If a
    /// future rename breaks this mapping the test fails here, BEFORE
    /// the desktop encounters a broken stderr envelope at runtime.
    #[test]
    fn consent_kind_payload_round_trips_into_desktop_shape() {
        #[derive(Debug, Deserialize, PartialEq, Eq)]
        struct DesktopConsentRequiredDetailsDto {
            scoped_id: String,
            version: String,
            target_label: String,
            policy_segment_hash: String,
            provisioning_policy_hash: String,
            summary: String,
        }

        let kind = InteractiveResolutionKind::ConsentRequired {
            scoped_id: "publisher/app".to_string(),
            version: "1.0.0".to_string(),
            target_label: "cli".to_string(),
            policy_segment_hash: "blake3:aaa".to_string(),
            provisioning_policy_hash: "blake3:bbb".to_string(),
            summary: "Capsule: publisher/app@1.0.0".to_string(),
        };

        let json = serde_json::to_value(&kind).expect("serialize kind");
        // Desktop reads these field names directly out of details
        // today; the typed envelope must mirror them so a future
        // consumer can decode either shape into the same DTO.
        let dto: DesktopConsentRequiredDetailsDto =
            serde_json::from_value(json.clone()).expect("desktop shape decodes");
        assert_eq!(dto.scoped_id, "publisher/app");
        assert_eq!(dto.target_label, "cli");
        assert_eq!(dto.policy_segment_hash, "blake3:aaa");
    }

    /// SecretsRequired serializes the `target` field only when present
    /// and emits the discriminator as snake_case. Locks the wire
    /// shape (the desktop deserializer in this PR doesn't read this
    /// envelope yet, but future consumers will and any change here
    /// must be visible).
    #[test]
    fn secrets_required_serializes_with_snake_case_discriminator() {
        let envelope = InteractiveResolutionEnvelope {
            kind: InteractiveResolutionKind::SecretsRequired {
                target: Some("main".to_string()),
                schema: vec![sample_config_field("API_KEY")],
            },
            display: ResolutionDisplay {
                message: "missing".to_string(),
                hint: None,
            },
        };

        let json = serde_json::to_string(&envelope).expect("serialize");
        assert!(json.contains(r#""type":"secrets_required""#), "{json}");
        assert!(json.contains(r#""target":"main""#), "{json}");
        // hint is None → must be skipped, not serialized as null.
        assert!(!json.contains(r#""hint":"#), "{json}");
    }
}
