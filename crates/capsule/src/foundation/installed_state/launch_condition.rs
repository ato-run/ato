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
//! capsule.lock (canonical)   = portable resolved description
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
//! never reach `detail_json`.
//!
//! For the value-bearing kinds ([`LaunchConditionKind::Secret`] /
//! [`LaunchConditionKind::Env`]) [`validate_redacted_detail_json`] enforces a
//! strict **allowlist**: only redacted-reference metadata keys are permitted
//! (see [`SECRET_DETAIL_ALLOWED_KEYS`] / [`ENV_DETAIL_ALLOWED_KEYS`]) and every
//! value must be a scalar or array of scalars. So an arbitrary key — e.g. an
//! env-var name carrying its value, `{"OPENAI_API_KEY": "sk-..."}` — is
//! rejected, and no nested object can smuggle a value one level down. A
//! `Secret` condition must additionally carry `redacted == true`. A denylist was
//! deliberately *not* used: it would pass any key it didn't enumerate, which is
//! exactly how a raw value under an env-var name slips through.

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

/// All launch condition kinds in declaration order. Used to compute which
/// extractors are still missing for the ledger baseline marker.
pub const ALL_LAUNCH_CONDITION_KINDS: &[LaunchConditionKind] = &[
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

/// `condition_key` of the ledger baseline marker written for every installed
/// revision (see [`launch_condition_extraction_status`]).
pub const LEDGER_EXTRACTION_STATUS_KEY: &str = "ledger.extraction_status";

/// The only `detail_json` keys permitted for a [`LaunchConditionKind::Secret`]
/// condition — all redacted-reference metadata, never a value.
pub const SECRET_DETAIL_ALLOWED_KEYS: &[&str] = &[
    "projection",
    "grant_ref",
    "scope",
    "store",
    "required",
    "projectable_to",
];

/// The only `detail_json` keys permitted for a [`LaunchConditionKind::Env`]
/// condition — projection metadata pointing at where the value comes from,
/// never the value itself. `grant_ref` is a redacted secret-grant reference for a
/// sensitive `env.*=grant:<id>` condition (#549), exactly like the Secret kind —
/// it is a logical id, never the value.
pub const ENV_DETAIL_ALLOWED_KEYS: &[&str] = &[
    "source",
    "projection",
    "ref",
    "logical_endpoint",
    "condition_ref",
    "grant_ref",
];

/// Validate that `detail_json` is a redacted JSON object suitable for storage.
///
/// Every kind must carry a syntactically valid JSON object. For the
/// value-bearing kinds ([`LaunchConditionKind::Secret`] /
/// [`LaunchConditionKind::Env`]) an **allowlist** is enforced: each top-level
/// key must be one of the kind's permitted redacted-reference keys, and each
/// value must be a scalar (or array of scalars) — never a nested object/array
/// that could hide a value. This rejects an arbitrary env-var-name key bearing a
/// raw value (the failure mode a denylist cannot catch).
///
/// Non-value-bearing kinds describe requirements (e.g. `required_bytes`), carry
/// no credential, and so are not key-restricted.
pub fn validate_redacted_detail_json(kind: LaunchConditionKind, detail_json: &str) -> Result<()> {
    let parsed: Value = serde_json::from_str(detail_json).map_err(|e| {
        CapsuleError::Runtime(format!(
            "launch condition detail_json is not valid JSON: {e}"
        ))
    })?;
    let Value::Object(map) = &parsed else {
        return Err(CapsuleError::Runtime(
            "launch condition detail_json must be a JSON object".to_string(),
        ));
    };
    let allowed = match kind {
        LaunchConditionKind::Secret => SECRET_DETAIL_ALLOWED_KEYS,
        LaunchConditionKind::Env => ENV_DETAIL_ALLOWED_KEYS,
        _ => return Ok(()),
    };
    for (key, value) in map {
        if !allowed.contains(&key.as_str()) {
            return Err(CapsuleError::Runtime(format!(
                "{kind} launch condition detail_json key '{key}' is not allowed; only \
                 redacted-reference keys {allowed:?} are permitted — store a reference such as a \
                 grant_ref, never a value",
                kind = kind.as_str(),
            )));
        }
        if !is_redacted_scalar(value) {
            return Err(CapsuleError::Runtime(format!(
                "{kind} launch condition detail_json key '{key}' must be a scalar or array of \
                 scalars; a nested object/array could smuggle a value",
                kind = kind.as_str(),
            )));
        }
    }
    Ok(())
}

/// A value safe to store in a redacted detail: a scalar, or an array of scalars.
/// Nested objects (and arrays containing objects/arrays) are rejected because
/// they could embed a value under an unchecked key.
fn is_redacted_scalar(value: &Value) -> bool {
    match value {
        Value::Object(_) => false,
        Value::Array(items) => items
            .iter()
            .all(|item| !matches!(item, Value::Object(_) | Value::Array(_))),
        _ => true,
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

/// The canonical logical endpoint string for an installed app's service:
/// `ato://app/<install_profile_key>/<service>`. Matches the form used by the
/// launch-time port admission (`#523`) so an install-time port *declaration* and
/// the launch-time port *claim* share the same `condition_key`.
pub fn app_service_endpoint(install_profile_key: &str, service_name: &str) -> String {
    format!("ato://app/{install_profile_key}/{service_name}")
}

/// Project an install-time **port declaration** into a launch condition.
///
/// Unlike [`launch_condition_from_port_claim`] (a launch-time *claim* recording
/// the resolved/actual port), this records a *declaration*: "this app requires
/// this logical endpoint / preferred port / protocol". Status defaults to
/// `Unknown` because OS availability and the final remap are only known at
/// launch — a launch-time port claim later supersedes it with `Satisfied`.
///
/// `preferred_port` of `Some(0)` (auto-assign) is not a concrete port and is
/// stored as absent. The `condition_key` is `<logical_endpoint>.<protocol>`,
/// matching the port-claim projection so the two never diverge.
#[allow(clippy::too_many_arguments)]
pub fn launch_condition_from_port_declaration(
    install_profile_key: &str,
    install_revision_id: Option<&str>,
    logical_endpoint: &str,
    protocol: &str,
    preferred_port: Option<u16>,
    source: &str,
    conflict_policy: Option<&str>,
    status: LaunchConditionStatus,
) -> LaunchConditionClaim {
    let mut detail = serde_json::Map::new();
    detail.insert(
        "logical_endpoint".to_string(),
        Value::String(logical_endpoint.to_string()),
    );
    detail.insert("protocol".to_string(), Value::String(protocol.to_string()));
    // Port 0 = "auto-assign", not a concrete declared port — store as absent.
    if let Some(port) = preferred_port.filter(|&p| p != 0) {
        detail.insert("preferred_port".to_string(), Value::from(port));
    }
    detail.insert("source".to_string(), Value::String(source.to_string()));
    if let Some(policy) = conflict_policy {
        detail.insert(
            "conflict_policy".to_string(),
            Value::String(policy.to_string()),
        );
    }
    LaunchConditionClaim {
        install_profile_key: install_profile_key.to_string(),
        install_revision_id: install_revision_id.map(str::to_string),
        provider_id: None,
        kind: LaunchConditionKind::Port,
        condition_key: format!("{logical_endpoint}.{protocol}"),
        status,
        required: true,
        source: LaunchConditionSource::Manifest,
        detail_json: Value::Object(detail).to_string(),
        redacted: true,
    }
}

/// Project an environment-variable requirement into a launch condition.
///
/// `env_name` is the variable name (the `condition_key`, e.g. `PORT`,
/// `DATABASE_URL`). `source` is a free provenance string for `detail.source`
/// (e.g. `manifest.execution`, `manifest.required_env`). `projection_ref`, when
/// set, is a redacted reference to where the value is projected from (e.g. a
/// logical endpoint); it is **never** the value itself. The caller supplies
/// `status` (e.g. `Satisfied` for a value declared in the manifest, `Unknown`
/// for a name required from the host whose presence can't be confirmed at
/// install).
///
/// The detail uses only [`ENV_DETAIL_ALLOWED_KEYS`], so a raw env value can
/// never be recorded here.
pub fn launch_condition_from_env_projection(
    install_profile_key: &str,
    install_revision_id: Option<&str>,
    env_name: &str,
    source: &str,
    projection_ref: Option<&str>,
    status: LaunchConditionStatus,
) -> LaunchConditionClaim {
    let mut detail = serde_json::Map::new();
    detail.insert("source".to_string(), Value::String(source.to_string()));
    if let Some(reference) = projection_ref {
        detail.insert("ref".to_string(), Value::String(reference.to_string()));
    }
    LaunchConditionClaim {
        install_profile_key: install_profile_key.to_string(),
        install_revision_id: install_revision_id.map(str::to_string),
        provider_id: None,
        kind: LaunchConditionKind::Env,
        condition_key: env_name.to_string(),
        status,
        required: true,
        source: LaunchConditionSource::Manifest,
        detail_json: Value::Object(detail).to_string(),
        redacted: true,
    }
}

/// Project a secret requirement into a launch condition. `secret_name` is the
/// `condition_key` (e.g. a credential name). The detail records only a
/// **redacted reference** — `projection` (how the value is delivered, e.g.
/// `env`), an optional `grant_ref` (the grant locator, or `null` when not yet
/// granted), and an optional `scope` — never the secret value.
///
/// Status is derived from the grant: `Satisfied` when a `grant_ref` is present,
/// otherwise `UserGrantRequired`. The detail uses only
/// [`SECRET_DETAIL_ALLOWED_KEYS`].
pub fn launch_condition_from_secret_requirement(
    install_profile_key: &str,
    install_revision_id: Option<&str>,
    secret_name: &str,
    projection: &str,
    grant_ref: Option<&str>,
    scope: Option<&str>,
) -> LaunchConditionClaim {
    let mut detail = serde_json::Map::new();
    detail.insert(
        "projection".to_string(),
        Value::String(projection.to_string()),
    );
    detail.insert(
        "grant_ref".to_string(),
        grant_ref.map_or(Value::Null, |g| Value::String(g.to_string())),
    );
    if let Some(scope) = scope {
        detail.insert("scope".to_string(), Value::String(scope.to_string()));
    }
    let status = if grant_ref.is_some() {
        LaunchConditionStatus::Satisfied
    } else {
        LaunchConditionStatus::UserGrantRequired
    };
    LaunchConditionClaim {
        install_profile_key: install_profile_key.to_string(),
        install_revision_id: install_revision_id.map(str::to_string),
        provider_id: None,
        kind: LaunchConditionKind::Secret,
        condition_key: secret_name.to_string(),
        status,
        required: true,
        source: LaunchConditionSource::Manifest,
        detail_json: Value::Object(detail).to_string(),
        redacted: true,
    }
}

/// Project a state requirement / binding into a launch condition. `state_key` is
/// the logical state name (the `condition_key`). The detail carries only logical
/// metadata — an optional `binding_ref` (a logical state locator such as
/// `ato-state://…`, never a raw host path), an optional `durability`, and an
/// optional `mount_target`.
///
/// `mount_target` is the **guest** mount path the state directory is exposed at
/// inside the launched process/container (e.g. `/app/data`), taken from the
/// manifest's `services.main.state_bindings[].target` at install time. It is a
/// guest-side, non-sensitive path — never the raw host `target_path` — and exists
/// so the runtime materialization step (#508) can place the bound directory at the
/// correct guest target without re-reading the manifest/lockfile at relaunch.
pub fn launch_condition_from_state_binding(
    install_profile_key: &str,
    install_revision_id: Option<&str>,
    state_key: &str,
    binding_ref: Option<&str>,
    durability: Option<&str>,
    mount_target: Option<&str>,
    status: LaunchConditionStatus,
) -> LaunchConditionClaim {
    let mut detail = serde_json::Map::new();
    if let Some(reference) = binding_ref {
        detail.insert(
            "binding_ref".to_string(),
            Value::String(reference.to_string()),
        );
    }
    if let Some(durability) = durability {
        detail.insert(
            "durability".to_string(),
            Value::String(durability.to_string()),
        );
    }
    if let Some(mount_target) = mount_target {
        detail.insert(
            "mount_target".to_string(),
            Value::String(mount_target.to_string()),
        );
    }
    LaunchConditionClaim {
        install_profile_key: install_profile_key.to_string(),
        install_revision_id: install_revision_id.map(str::to_string),
        provider_id: None,
        kind: LaunchConditionKind::State,
        condition_key: state_key.to_string(),
        status,
        required: true,
        source: LaunchConditionSource::Manifest,
        detail_json: Value::Object(detail).to_string(),
        redacted: true,
    }
}

/// Build the ledger **baseline marker** for an installed revision: a
/// non-required `Policy` condition (`condition_key =
/// `[`LEDGER_EXTRACTION_STATUS_KEY`]`) recording which condition extractors have
/// run and which are still missing.
///
/// Writing this on every install guarantees an installed revision *always* has a
/// ledger, so an **empty** ledger is never mistaken for "this app has no launch
/// conditions" — empty unambiguously means "nothing recorded". Consumers read
/// `complete` / `missing_extractors` to tell "no such condition" apart from "not
/// yet extracted".
///
/// `extracted_kinds` are the kinds whose extractors ran; the missing set is the
/// complement against [`ALL_LAUNCH_CONDITION_KINDS`]. Status is `Satisfied` once
/// nothing is missing, else `Unknown`.
pub fn launch_condition_extraction_status(
    install_profile_key: &str,
    install_revision_id: Option<&str>,
    extracted_kinds: &[LaunchConditionKind],
) -> LaunchConditionClaim {
    let extracted: Vec<&str> = extracted_kinds.iter().map(|k| k.as_str()).collect();
    let missing: Vec<&str> = ALL_LAUNCH_CONDITION_KINDS
        .iter()
        .filter(|k| !extracted_kinds.contains(k))
        .map(|k| k.as_str())
        .collect();
    let complete = missing.is_empty();
    let detail = serde_json::json!({
        "complete": complete,
        "extracted_kinds": extracted,
        "missing_extractors": missing,
    });
    LaunchConditionClaim {
        install_profile_key: install_profile_key.to_string(),
        install_revision_id: install_revision_id.map(str::to_string),
        provider_id: None,
        kind: LaunchConditionKind::Policy,
        condition_key: LEDGER_EXTRACTION_STATUS_KEY.to_string(),
        status: if complete {
            LaunchConditionStatus::Satisfied
        } else {
            LaunchConditionStatus::Unknown
        },
        required: false,
        source: LaunchConditionSource::InstalledState,
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
    fn secret_launch_condition_rejects_env_name_raw_value() {
        // The core guarantee: a raw value under an arbitrary env-var-name key is
        // rejected. A denylist would pass this; the allowlist does not.
        let detail = r#"{"OPENAI_API_KEY":"sk-abc123"}"#;
        assert!(
            validate_redacted_detail_json(LaunchConditionKind::Secret, detail).is_err(),
            "a raw secret value under its env-var name must be rejected"
        );
        // Explicit value-bearing keys are likewise rejected.
        assert!(
            validate_redacted_detail_json(
                LaunchConditionKind::Secret,
                r#"{"projection":"env","value":"sk-abc123"}"#
            )
            .is_err()
        );
    }

    #[test]
    fn env_launch_condition_rejects_env_name_raw_value() {
        let detail = r#"{"OPENAI_API_KEY":"sk-abc123"}"#;
        assert!(
            validate_redacted_detail_json(LaunchConditionKind::Env, detail).is_err(),
            "a raw value under its env-var name must be rejected for Env too"
        );
        // An api_key key is not in the Env allowlist either.
        assert!(
            validate_redacted_detail_json(
                LaunchConditionKind::Env,
                r#"{"projection":"env","api_key":"sk-abc"}"#
            )
            .is_err()
        );
    }

    #[test]
    fn secret_launch_condition_accepts_redacted_grant_ref_shape() {
        let detail = r#"{"projection":"env","grant_ref":"ato-secret://x","scope":"capsule_instance","store":"local_keychain"}"#;
        assert!(validate_redacted_detail_json(LaunchConditionKind::Secret, detail).is_ok());
        // grant_ref may be null (not yet granted) and projectable_to an array.
        let pending = r#"{"projection":"env","grant_ref":null,"scope":"capsule_instance","projectable_to":["env","file"]}"#;
        assert!(validate_redacted_detail_json(LaunchConditionKind::Secret, pending).is_ok());
    }

    #[test]
    fn env_launch_condition_accepts_projection_ref_shape() {
        let detail = r#"{"source":"port_claim","logical_endpoint":"ato://app/x/main"}"#;
        assert!(validate_redacted_detail_json(LaunchConditionKind::Env, detail).is_ok());
    }

    #[test]
    fn secret_detail_nested_object_value_is_rejected() {
        // An allowed key whose value is a nested object could smuggle a value.
        let detail = r#"{"projection":{"nested":"x"}}"#;
        assert!(
            validate_redacted_detail_json(LaunchConditionKind::Secret, detail).is_err(),
            "a nested object under an allowed key must be rejected"
        );
        // ...and an unknown key holding a nested object is rejected too.
        let nested = r#"{"grant":{"token":"abc"}}"#;
        assert!(validate_redacted_detail_json(LaunchConditionKind::Secret, nested).is_err());
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

    #[test]
    fn port_declaration_condition_uses_logical_endpoint_shape() {
        let endpoint = app_service_endpoint("ipk_a", "main");
        let c = launch_condition_from_port_declaration(
            "ipk_a",
            Some("rev1"),
            &endpoint,
            "tcp",
            Some(3000),
            "manifest.targets.port",
            Some("remap"),
            LaunchConditionStatus::Unknown,
        );
        assert_eq!(c.kind, LaunchConditionKind::Port);
        assert_eq!(c.condition_key, "ato://app/ipk_a/main.tcp");
        assert_eq!(c.status, LaunchConditionStatus::Unknown);
        assert_eq!(c.source, LaunchConditionSource::Manifest);
        assert!(
            c.detail_json
                .contains("\"logical_endpoint\":\"ato://app/ipk_a/main\"")
        );
        assert!(c.detail_json.contains("\"protocol\":\"tcp\""));
        assert!(c.detail_json.contains("\"preferred_port\":3000"));
        assert!(c.detail_json.contains("\"conflict_policy\":\"remap\""));
        validate_redacted_detail_json(c.kind, &c.detail_json).unwrap();
    }

    #[test]
    fn port_declaration_without_preferred_port_is_allowed() {
        let endpoint = app_service_endpoint("ipk_a", "worker");
        let c = launch_condition_from_port_declaration(
            "ipk_a",
            Some("rev1"),
            &endpoint,
            "tcp",
            None,
            "manifest.services.worker.expose",
            None,
            LaunchConditionStatus::Unknown,
        );
        assert_eq!(c.condition_key, "ato://app/ipk_a/worker.tcp");
        assert!(!c.detail_json.contains("preferred_port"));
        validate_redacted_detail_json(c.kind, &c.detail_json).unwrap();
    }

    #[test]
    fn port_declaration_rejects_zero_or_omits_preferred_port() {
        // Port 0 ("auto-assign") is not a concrete declared port → stored absent.
        let endpoint = app_service_endpoint("ipk_a", "main");
        let c = launch_condition_from_port_declaration(
            "ipk_a",
            None,
            &endpoint,
            "tcp",
            Some(0),
            "manifest.targets.port",
            Some("remap"),
            LaunchConditionStatus::Unknown,
        );
        assert!(
            !c.detail_json.contains("preferred_port"),
            "port 0 must not be stored as a concrete port: {}",
            c.detail_json
        );
    }

    #[test]
    fn port_declaration_condition_key_matches_port_claim_projection_shape() {
        // An install-time declaration and a launch-time claim for the same
        // endpoint+protocol must produce the same condition_key, so they address
        // the same ledger row.
        let endpoint = app_service_endpoint("ipk_a", "main");
        let declaration = launch_condition_from_port_declaration(
            "ipk_a",
            Some("rev1"),
            &endpoint,
            "tcp",
            Some(3000),
            "manifest.targets.port",
            Some("remap"),
            LaunchConditionStatus::Unknown,
        );
        let claim = launch_condition_from_port_claim(&super::super::port::PortClaim {
            install_profile_key: "ipk_a".to_string(),
            logical_endpoint: endpoint,
            preferred_port: 3000,
            last_actual_port: Some(49152),
            protocol: "tcp".to_string(),
            conflict_policy: super::super::port::ConflictPolicy::Remap,
        });
        assert_eq!(declaration.condition_key, claim.condition_key);
    }

    #[test]
    fn env_projection_condition_uses_redacted_reference_shape() {
        let c = launch_condition_from_env_projection(
            "app",
            Some("rev1"),
            "DATABASE_URL",
            "manifest.execution",
            None,
            LaunchConditionStatus::Satisfied,
        );
        assert_eq!(c.kind, LaunchConditionKind::Env);
        assert_eq!(c.condition_key, "DATABASE_URL");
        assert_eq!(c.status, LaunchConditionStatus::Satisfied);
        assert!(c.detail_json.contains("\"source\":\"manifest.execution\""));
        validate_redacted_detail_json(c.kind, &c.detail_json).unwrap();

        // With a projection reference (e.g. a port endpoint), still no raw value.
        let c2 = launch_condition_from_env_projection(
            "app",
            Some("rev1"),
            "PORT",
            "port_claim",
            Some("ato://app/app/main"),
            LaunchConditionStatus::Satisfied,
        );
        assert!(c2.detail_json.contains("\"ref\":\"ato://app/app/main\""));
        validate_redacted_detail_json(c2.kind, &c2.detail_json).unwrap();
    }

    #[test]
    fn secret_requirement_condition_uses_grant_ref_not_value() {
        // No grant yet → UserGrantRequired, grant_ref null.
        let pending = launch_condition_from_secret_requirement(
            "app",
            Some("rev1"),
            "OPENAI_API_KEY",
            "env",
            None,
            Some("capsule_instance"),
        );
        assert_eq!(pending.kind, LaunchConditionKind::Secret);
        assert_eq!(pending.status, LaunchConditionStatus::UserGrantRequired);
        assert!(pending.detail_json.contains("\"grant_ref\":null"));
        validate_redacted_detail_json(pending.kind, &pending.detail_json).unwrap();

        // Granted → Satisfied with a redacted grant locator, never the value.
        let granted = launch_condition_from_secret_requirement(
            "app",
            Some("rev1"),
            "OPENAI_API_KEY",
            "env",
            Some("ato-secret://store/openai"),
            Some("capsule_instance"),
        );
        assert_eq!(granted.status, LaunchConditionStatus::Satisfied);
        assert!(granted.detail_json.contains("ato-secret://store/openai"));
        validate_redacted_detail_json(granted.kind, &granted.detail_json).unwrap();
    }

    #[test]
    fn state_binding_condition_uses_binding_ref_not_raw_value() {
        let c = launch_condition_from_state_binding(
            "app",
            Some("rev1"),
            "data",
            Some("ato-state://app/data"),
            Some("persistent"),
            Some("/app/data"),
            LaunchConditionStatus::Satisfied,
        );
        assert_eq!(c.kind, LaunchConditionKind::State);
        assert_eq!(c.condition_key, "data");
        assert!(
            c.detail_json
                .contains("\"binding_ref\":\"ato-state://app/data\"")
        );
        assert!(c.detail_json.contains("\"durability\":\"persistent\""));
        // The guest mount target is recorded (not the host path).
        assert!(c.detail_json.contains("\"mount_target\":\"/app/data\""));
        // Never a raw host path.
        assert!(!c.detail_json.contains("/Users/"));
        assert!(!c.detail_json.contains("/home/"));
        validate_redacted_detail_json(c.kind, &c.detail_json).unwrap();
    }

    #[test]
    fn env_projection_rejects_raw_env_value() {
        // A raw env value under its name, or under a `value` key, is rejected by
        // the Env allowlist — the helper never produces such a shape.
        let raw = r#"{"DATABASE_URL":"postgres://user:pw@host/db"}"#;
        assert!(validate_redacted_detail_json(LaunchConditionKind::Env, raw).is_err());
        let with_value = r#"{"source":"manifest","value":"postgres://x"}"#;
        assert!(validate_redacted_detail_json(LaunchConditionKind::Env, with_value).is_err());
    }

    #[test]
    fn secret_requirement_rejects_raw_secret_value() {
        let raw = r#"{"OPENAI_API_KEY":"sk-abc123"}"#;
        assert!(validate_redacted_detail_json(LaunchConditionKind::Secret, raw).is_err());
        let with_value = r#"{"projection":"env","value":"sk-abc123"}"#;
        assert!(validate_redacted_detail_json(LaunchConditionKind::Secret, with_value).is_err());
    }

    #[test]
    fn ledger_extraction_status_marks_incomplete_kinds() {
        let marker = launch_condition_extraction_status(
            "app",
            Some("rev1"),
            &[LaunchConditionKind::Storage],
        );
        assert_eq!(marker.kind, LaunchConditionKind::Policy);
        assert_eq!(marker.condition_key, LEDGER_EXTRACTION_STATUS_KEY);
        assert_eq!(marker.status, LaunchConditionStatus::Unknown);
        assert!(
            !marker.required,
            "the baseline marker is not a launch requirement"
        );
        assert_eq!(marker.source, LaunchConditionSource::InstalledState);

        let detail: Value = serde_json::from_str(&marker.detail_json).unwrap();
        assert_eq!(detail["complete"], Value::Bool(false));
        let extracted: Vec<&str> = detail["extracted_kinds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(extracted, vec!["storage"]);
        let missing: Vec<&str> = detail["missing_extractors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        // Everything except the one extracted kind is reported missing.
        assert!(!missing.contains(&"storage"));
        for kind in [
            "port",
            "env",
            "secret",
            "state",
            "provider_capability",
            "hardware",
        ] {
            assert!(missing.contains(&kind), "{kind} must be reported missing");
        }
        // The marker itself is a valid, redacted detail.
        validate_redacted_detail_json(marker.kind, &marker.detail_json).unwrap();
    }

    #[test]
    fn ledger_extraction_status_is_complete_when_all_kinds_extracted() {
        let marker =
            launch_condition_extraction_status("app", Some("rev1"), ALL_LAUNCH_CONDITION_KINDS);
        assert_eq!(marker.status, LaunchConditionStatus::Satisfied);
        let detail: Value = serde_json::from_str(&marker.detail_json).unwrap();
        assert_eq!(detail["complete"], Value::Bool(true));
        assert!(detail["missing_extractors"].as_array().unwrap().is_empty());
    }
}
