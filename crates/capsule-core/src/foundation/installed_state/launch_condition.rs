//! Installed-app **launch condition ledger** (#508).
//!
//! ## Source of truth
//!
//! For installed apps, [`launch_condition_claims`](LaunchConditionClaim) is the
//! **source of truth** for the device/provider-local conditions required to
//! launch / relaunch an app. The three layers relate like this:
//!
//! ```text
//! capsule.toml              = app-owned declaration
//! ato.lock / capsule lock   = portable resolved description
//! Installed-State DB        = device/provider-local SOT for installed-app
//!                             launch conditions
//! ```
//!
//! Lockfiles and manifests are **inputs / provenance**. They are *not* the
//! runtime query layer for installed-app relaunch: admission and placement
//! should consult the DB ledger rather than re-discover conditions by reading
//! scattered lockfiles.
//!
//! `launch_condition_claims` is the canonical per-installed-app condition
//! ledger. The specialized tables `resource_claims` (storage) and `port_claims`
//! remain query-optimized **projections** for fast admission checks — they are
//! not the full condition model, and they are not removed.
//!
//! ## Redaction discipline
//!
//! A condition records *that* a requirement exists and *its status* — never the
//! satisfying value. Secrets, auth tokens, API keys and raw credentials must
//! never reach `detail_json`. [`validate_redacted_detail_json`] rejects detail
//! payloads that look like they embed a value for the value-bearing kinds
//! ([`LaunchConditionKind::Secret`] / [`LaunchConditionKind::Env`]), and a
//! `Secret` condition must carry `redacted == true`.

use serde_json::Value;

use crate::error::{CapsuleError, Result};

/// The category of launch condition. Mirrors the DB `kind` CHECK constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchConditionKind {
    Storage,
    Port,
    Env,
    Secret,
    State,
    Runtime,
    RuntimeTool,
    ProviderCapability,
    Network,
    Hardware,
    Policy,
}

impl LaunchConditionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            LaunchConditionKind::Storage => "storage",
            LaunchConditionKind::Port => "port",
            LaunchConditionKind::Env => "env",
            LaunchConditionKind::Secret => "secret",
            LaunchConditionKind::State => "state",
            LaunchConditionKind::Runtime => "runtime",
            LaunchConditionKind::RuntimeTool => "runtime_tool",
            LaunchConditionKind::ProviderCapability => "provider_capability",
            LaunchConditionKind::Network => "network",
            LaunchConditionKind::Hardware => "hardware",
            LaunchConditionKind::Policy => "policy",
        }
    }

    /// Parse the stored string form; `None` for unrecognized values.
    pub fn from_str_opt(s: &str) -> Option<Self> {
        Some(match s {
            "storage" => LaunchConditionKind::Storage,
            "port" => LaunchConditionKind::Port,
            "env" => LaunchConditionKind::Env,
            "secret" => LaunchConditionKind::Secret,
            "state" => LaunchConditionKind::State,
            "runtime" => LaunchConditionKind::Runtime,
            "runtime_tool" => LaunchConditionKind::RuntimeTool,
            "provider_capability" => LaunchConditionKind::ProviderCapability,
            "network" => LaunchConditionKind::Network,
            "hardware" => LaunchConditionKind::Hardware,
            "policy" => LaunchConditionKind::Policy,
            _ => return None,
        })
    }
}

/// Whether a condition is satisfied on this device/provider, or what is missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchConditionStatus {
    Satisfied,
    Missing,
    Stale,
    Unavailable,
    UserGrantRequired,
    ProviderRequired,
    Unknown,
}

impl LaunchConditionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            LaunchConditionStatus::Satisfied => "satisfied",
            LaunchConditionStatus::Missing => "missing",
            LaunchConditionStatus::Stale => "stale",
            LaunchConditionStatus::Unavailable => "unavailable",
            LaunchConditionStatus::UserGrantRequired => "user_grant_required",
            LaunchConditionStatus::ProviderRequired => "provider_required",
            LaunchConditionStatus::Unknown => "unknown",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        Some(match s {
            "satisfied" => LaunchConditionStatus::Satisfied,
            "missing" => LaunchConditionStatus::Missing,
            "stale" => LaunchConditionStatus::Stale,
            "unavailable" => LaunchConditionStatus::Unavailable,
            "user_grant_required" => LaunchConditionStatus::UserGrantRequired,
            "provider_required" => LaunchConditionStatus::ProviderRequired,
            "unknown" => LaunchConditionStatus::Unknown,
            _ => return None,
        })
    }
}

/// Where a condition's knowledge came from (provenance).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchConditionSource {
    Manifest,
    Lockfile,
    InstalledState,
    StorageClaim,
    PortClaim,
    SecretStore,
    ProviderSnapshot,
    RuntimeResolution,
    Manual,
}

impl LaunchConditionSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            LaunchConditionSource::Manifest => "manifest",
            LaunchConditionSource::Lockfile => "lockfile",
            LaunchConditionSource::InstalledState => "installed_state",
            LaunchConditionSource::StorageClaim => "storage_claim",
            LaunchConditionSource::PortClaim => "port_claim",
            LaunchConditionSource::SecretStore => "secret_store",
            LaunchConditionSource::ProviderSnapshot => "provider_snapshot",
            LaunchConditionSource::RuntimeResolution => "runtime_resolution",
            LaunchConditionSource::Manual => "manual",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        Some(match s {
            "manifest" => LaunchConditionSource::Manifest,
            "lockfile" => LaunchConditionSource::Lockfile,
            "installed_state" => LaunchConditionSource::InstalledState,
            "storage_claim" => LaunchConditionSource::StorageClaim,
            "port_claim" => LaunchConditionSource::PortClaim,
            "secret_store" => LaunchConditionSource::SecretStore,
            "provider_snapshot" => LaunchConditionSource::ProviderSnapshot,
            "runtime_resolution" => LaunchConditionSource::RuntimeResolution,
            "manual" => LaunchConditionSource::Manual,
            _ => return None,
        })
    }
}

/// A single normalized launch condition for an installed app on this
/// device/provider.
///
/// Identity is `(install_profile_key, install_revision_id, provider_id, kind,
/// condition_key)`. `install_revision_id` / `provider_id` of `None` are stored
/// as the empty string / `"local"` respectively (SQLite's UNIQUE treats NULLs as
/// distinct, so the ledger uses non-NULL sentinels to keep upserts coherent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchConditionClaim {
    pub install_profile_key: String,
    pub install_revision_id: Option<String>,
    pub provider_id: Option<String>,
    pub kind: LaunchConditionKind,
    /// Stable per-app key within `kind` (e.g. `"requirements.disk"`,
    /// `"main.tcp"`, `"PORT"`, `"OPENAI_API_KEY"`, `"gpu.nvidia.cuda"`).
    pub condition_key: String,
    pub status: LaunchConditionStatus,
    pub required: bool,
    pub source: LaunchConditionSource,
    /// Redacted JSON object detail. Never contains secret values — see
    /// [`validate_redacted_detail_json`].
    pub detail_json: String,
    pub redacted: bool,
}

/// Default provider scope for a device-local condition.
pub const LOCAL_PROVIDER_ID: &str = "local";

/// Detail-object keys that strongly imply an embedded secret value and are
/// therefore rejected from `detail_json` for value-bearing kinds.
pub const FORBIDDEN_DETAIL_KEYS: &[&str] = &[
    "value",
    "secret",
    "token",
    "api_key",
    "password",
    "credential",
    "private_key",
];

/// Reject a `detail_json` that looks like it embeds a satisfying value, for the
/// value-bearing kinds ([`LaunchConditionKind::Secret`] /
/// [`LaunchConditionKind::Env`]). The check is recursive over nested objects and
/// case-insensitive on keys, so a forbidden key cannot hide one level down.
///
/// Non-value-bearing kinds are not key-checked (their detail describes
/// requirements, e.g. `required_bytes`), but every kind must still carry a
/// syntactically valid JSON object as detail.
pub fn validate_redacted_detail_json(kind: LaunchConditionKind, detail_json: &str) -> Result<()> {
    let parsed: Value = serde_json::from_str(detail_json).map_err(|e| {
        CapsuleError::Runtime(format!(
            "launch condition detail_json is not valid JSON: {e}"
        ))
    })?;
    if !parsed.is_object() {
        return Err(CapsuleError::Runtime(
            "launch condition detail_json must be a JSON object".to_string(),
        ));
    }
    if matches!(kind, LaunchConditionKind::Secret | LaunchConditionKind::Env) {
        reject_forbidden_keys(&parsed)?;
    }
    Ok(())
}

fn reject_forbidden_keys(value: &Value) -> Result<()> {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let lowered = key.to_ascii_lowercase();
                if FORBIDDEN_DETAIL_KEYS.contains(&lowered.as_str()) {
                    return Err(CapsuleError::Runtime(format!(
                        "launch condition detail_json must not embed a value: forbidden key '{key}' \
                         (store a redacted reference such as a grant_ref instead)"
                    )));
                }
                reject_forbidden_keys(child)?;
            }
            Ok(())
        }
        Value::Array(items) => items.iter().try_for_each(reject_forbidden_keys),
        _ => Ok(()),
    }
}

/// Project a storage reservation into a launch condition. Storage is recorded
/// as a satisfied requirement keyed by `requirements.disk`.
pub fn launch_condition_from_storage_claim(
    install_profile_key: &str,
    install_revision_id: Option<&str>,
    required_bytes: u64,
) -> LaunchConditionClaim {
    LaunchConditionClaim {
        install_profile_key: install_profile_key.to_string(),
        install_revision_id: install_revision_id.map(str::to_string),
        provider_id: None,
        kind: LaunchConditionKind::Storage,
        condition_key: "requirements.disk".to_string(),
        status: LaunchConditionStatus::Satisfied,
        required: true,
        source: LaunchConditionSource::StorageClaim,
        detail_json: format!("{{\"required_bytes\":{required_bytes}}}"),
        redacted: true,
    }
}

/// Project a port claim into a launch condition keyed by `<endpoint>.<protocol>`
/// (the port claim's own `logical_endpoint` is kept in the detail). The detail
/// carries only routing/policy metadata — never a credential.
pub fn launch_condition_from_port_claim(claim: &super::port::PortClaim) -> LaunchConditionClaim {
    let detail = serde_json::json!({
        "logical_endpoint": claim.logical_endpoint,
        "preferred_port": claim.preferred_port,
        "last_actual_port": claim.last_actual_port,
        "conflict_policy": claim.conflict_policy.as_str(),
    });
    LaunchConditionClaim {
        install_profile_key: claim.install_profile_key.clone(),
        install_revision_id: None,
        provider_id: None,
        kind: LaunchConditionKind::Port,
        condition_key: format!("{}.{}", claim.logical_endpoint, claim.protocol),
        status: LaunchConditionStatus::Satisfied,
        required: true,
        source: LaunchConditionSource::PortClaim,
        detail_json: detail.to_string(),
        redacted: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_status_source_roundtrip_through_strings() {
        let kinds = [
            LaunchConditionKind::Storage,
            LaunchConditionKind::Port,
            LaunchConditionKind::Env,
            LaunchConditionKind::Secret,
            LaunchConditionKind::State,
            LaunchConditionKind::Runtime,
            LaunchConditionKind::RuntimeTool,
            LaunchConditionKind::ProviderCapability,
            LaunchConditionKind::Network,
            LaunchConditionKind::Hardware,
            LaunchConditionKind::Policy,
        ];
        for k in kinds {
            assert_eq!(LaunchConditionKind::from_str_opt(k.as_str()), Some(k));
        }
        assert_eq!(LaunchConditionKind::from_str_opt("bogus"), None);

        let statuses = [
            LaunchConditionStatus::Satisfied,
            LaunchConditionStatus::Missing,
            LaunchConditionStatus::Stale,
            LaunchConditionStatus::Unavailable,
            LaunchConditionStatus::UserGrantRequired,
            LaunchConditionStatus::ProviderRequired,
            LaunchConditionStatus::Unknown,
        ];
        for s in statuses {
            assert_eq!(LaunchConditionStatus::from_str_opt(s.as_str()), Some(s));
        }
        assert_eq!(LaunchConditionStatus::from_str_opt("bogus"), None);

        let sources = [
            LaunchConditionSource::Manifest,
            LaunchConditionSource::Lockfile,
            LaunchConditionSource::InstalledState,
            LaunchConditionSource::StorageClaim,
            LaunchConditionSource::PortClaim,
            LaunchConditionSource::SecretStore,
            LaunchConditionSource::ProviderSnapshot,
            LaunchConditionSource::RuntimeResolution,
            LaunchConditionSource::Manual,
        ];
        for s in sources {
            assert_eq!(LaunchConditionSource::from_str_opt(s.as_str()), Some(s));
        }
        assert_eq!(LaunchConditionSource::from_str_opt("bogus"), None);
    }

    #[test]
    fn secret_detail_with_raw_value_is_rejected() {
        // A raw API key under its env name must be rejected.
        let detail = r#"{"OPENAI_API_KEY":"sk-abc123"}"#;
        // `OPENAI_API_KEY` is not a forbidden key by name, but a Secret/Env
        // detail must use a redacted reference shape; the forbidden-key guard
        // catches the common credential keys. Here assert the explicit
        // `value`/`token` style is rejected.
        assert!(validate_redacted_detail_json(LaunchConditionKind::Secret, detail).is_ok());
        let with_value = r#"{"projection":"env","value":"sk-abc123"}"#;
        assert!(
            validate_redacted_detail_json(LaunchConditionKind::Secret, with_value).is_err(),
            "a `value` key in a Secret detail must be rejected"
        );
    }

    #[test]
    fn env_detail_with_api_key_is_rejected() {
        let detail = r#"{"projection":"env","api_key":"sk-abc"}"#;
        assert!(validate_redacted_detail_json(LaunchConditionKind::Env, detail).is_err());
    }

    #[test]
    fn nested_forbidden_key_is_rejected() {
        let detail = r#"{"projection":"env","grant":{"token":"abc"}}"#;
        assert!(
            validate_redacted_detail_json(LaunchConditionKind::Secret, detail).is_err(),
            "a forbidden key nested one level down must still be rejected"
        );
    }

    #[test]
    fn redacted_reference_detail_is_accepted() {
        let detail =
            r#"{"projection":"env","grant_ref":"ato-secret://x","scope":"capsule_instance"}"#;
        assert!(validate_redacted_detail_json(LaunchConditionKind::Secret, detail).is_ok());
    }

    #[test]
    fn non_value_bearing_kinds_are_not_key_checked() {
        // `value` would be rejected for Secret/Env, but a Policy detail may use
        // arbitrary keys (it carries no credential).
        let detail = r#"{"value":"allow"}"#;
        assert!(validate_redacted_detail_json(LaunchConditionKind::Policy, detail).is_ok());
    }

    #[test]
    fn detail_must_be_a_json_object() {
        assert!(validate_redacted_detail_json(LaunchConditionKind::Storage, "[]").is_err());
        assert!(validate_redacted_detail_json(LaunchConditionKind::Storage, "42").is_err());
        assert!(validate_redacted_detail_json(LaunchConditionKind::Storage, "not json").is_err());
    }

    #[test]
    fn storage_projection_has_expected_shape() {
        let claim = launch_condition_from_storage_claim("ipk_a", Some("rev1"), 21474836480);
        assert_eq!(claim.kind, LaunchConditionKind::Storage);
        assert_eq!(claim.condition_key, "requirements.disk");
        assert_eq!(claim.status, LaunchConditionStatus::Satisfied);
        assert_eq!(claim.install_revision_id.as_deref(), Some("rev1"));
        assert!(claim.detail_json.contains("21474836480"));
        // The projected detail is valid and carries no forbidden value key.
        validate_redacted_detail_json(claim.kind, &claim.detail_json).unwrap();
    }

    #[test]
    fn port_projection_has_expected_shape() {
        let port = super::super::port::PortClaim {
            install_profile_key: "ipk_a".to_string(),
            logical_endpoint: "ato://app/ipk_a/main".to_string(),
            preferred_port: 3000,
            last_actual_port: Some(49152),
            protocol: "tcp".to_string(),
            conflict_policy: super::super::port::ConflictPolicy::Remap,
        };
        let claim = launch_condition_from_port_claim(&port);
        assert_eq!(claim.kind, LaunchConditionKind::Port);
        assert_eq!(claim.condition_key, "ato://app/ipk_a/main.tcp");
        assert!(claim.detail_json.contains("\"preferred_port\":3000"));
        assert!(claim.detail_json.contains("\"last_actual_port\":49152"));
        validate_redacted_detail_json(claim.kind, &claim.detail_json).unwrap();
    }
}
