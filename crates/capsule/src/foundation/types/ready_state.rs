//! Ready-State Capsule authoring tables (parse-only, schema_version = "0.3").
//!
//! These tables describe how a capsule is *sealed into* and *restored from* a
//! warm/booted runtime snapshot (the "Ready-State Capsule" line) plus the
//! post-restore injection surfaces (secrets, bindings, external capabilities,
//! user context store) that must never be baked into a snapshot.
//!
//! They are **additive and parse-only** in this milestone:
//!
//! * Every field is `Option<…>` or a `#[serde(default)]` collection, so older
//!   binaries that predate these structs ignore the tables (no
//!   `deny_unknown_fields` anywhere in the capsule crate) and newer binaries
//!   read them. No `schema_version` bump is required — a capsule that omits all
//!   of these tables behaves exactly as before.
//! * Nothing here drives runtime behavior yet; the runtime backend
//!   (`crates/snapshot`) and the build/run pipeline wiring land in later
//!   milestones. The structs exist so recipes can start *declaring intent* and
//!   so the eligibility validator can compute Public Instant Run posture from
//!   the manifest alone.
//!
//! Name-collision note: the heavy-external-capability concept uses `[external.*]`
//! (not `[capabilities.*]`) because `capabilities` is already taken twice — by
//! [`CapsuleManifest::capabilities`](super::manifest::CapsuleManifest) (inference
//! model caps) and by `requirements.capabilities` (security posture). See the
//! Ready-State implementation plan §3.2 / capsule-redefinition-research §4.1.

use serde::{Deserialize, Serialize};

use super::manifest::CapsuleManifest;

/// `[snapshot]` — how this capsule is sealed and restored.
///
/// Bound at parse time only. `mode = "none"` (the default when the table is
/// omitted) means the legacy cold path; any other mode marks the capsule as
/// Ready-State-eligible once a snapshot backend + runner capability is present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotConfig {
    /// Sealing strategy. `Warm`/`Booted` require a snapshot backend; `Cold`
    /// is the legacy fast-cold-boot path; `None` disables Ready-State.
    #[serde(default)]
    pub mode: SnapshotMode,

    /// How far to pre-advance the app before capturing the snapshot. The seal
    /// point is always reached with **no secrets and no user data** present.
    #[serde(default)]
    pub boot_until: BootUntil,

    /// Run the post-resume sanitizer (regenerate ids/entropy, reset
    /// sockets/clock, clear request-local state) before exposing a restored
    /// session. Defaults to `true` — sanitizing a clone is the safe default.
    #[serde(default = "default_true")]
    pub sanitize_after_restore: bool,

    /// Author-supplied restore-compatibility constraint (a `runner_class`
    /// label such as `"managed/linux-aarch64"`). The full `runner_class_id` is
    /// resolved from build-host facts; this only *constrains* it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_class: Option<String>,

    /// Restore-latency SLO in seconds. Placement may reject a runner that
    /// cannot meet it. `None` means unspecified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_restore_seconds: Option<u32>,
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        Self {
            mode: SnapshotMode::default(),
            boot_until: BootUntil::default(),
            sanitize_after_restore: true,
            runner_class: None,
            max_restore_seconds: None,
        }
    }
}

impl SnapshotConfig {
    /// Whether this capsule opts into the Ready-State (warm/booted) line.
    /// `cold`/`none` stay on the legacy path.
    pub fn is_ready_state(&self) -> bool {
        matches!(self.mode, SnapshotMode::Warm | SnapshotMode::Booted)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SnapshotMode {
    /// No snapshot; legacy install-then-cold-boot (default).
    #[default]
    None,
    /// Fast cold-boot from frozen file state (no warm memory).
    Cold,
    /// Warm memory snapshot captured after boot.
    Warm,
    /// Fully booted-to-readiness snapshot.
    Booted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BootUntil {
    /// Boot until the healthcheck endpoint answers (default; must be reachable
    /// pre-auth / secret-free).
    #[default]
    Healthcheck,
    /// Boot until the app reports fully ready.
    Ready,
    /// Boot just far enough to seal; defer secret-dependent init to the first
    /// post-restore request.
    FirstRequest,
}

/// `[secrets.<name>]` — a required secret as a **ref**, never a value.
///
/// Resolved post-restore via the existing secret-injection path. `delivery =
/// "proxy"` routes through the capability broker so the raw value never enters
/// the app process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretSpec {
    /// Whether the app cannot reach `boot_until` / serve without this secret.
    #[serde(default)]
    pub required: bool,

    /// Target environment variable name inside the guest (when `delivery` is
    /// `env`/`fd`/`file`). `None` for proxy-only delivery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,

    /// How the value reaches the guest.
    #[serde(default)]
    pub delivery: SecretDelivery,

    /// Coarse classification (drives proxy-vs-raw policy; ADR-005).
    #[serde(default)]
    pub class: SecretClass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SecretDelivery {
    /// Passed via an inherited file descriptor.
    Fd,
    /// Injected as an environment variable (default).
    #[default]
    Env,
    /// Written to a file the guest reads.
    File,
    /// Mediated by the capability proxy; the raw value never reaches the app.
    Proxy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SecretClass {
    /// A provider API key (default).
    #[default]
    ApiKey,
    /// An OAuth token / grant.
    Oauth,
    /// A value generated per session/install.
    Generated,
    /// A short-lived session credential.
    Session,
}

/// `[bindings.<name>]` — a post-restore, user/billing/env-specific injection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingSpec {
    /// What kind of resource this binding attaches.
    pub kind: BindingKind,

    /// Whether the session cannot run without it.
    #[serde(default)]
    pub required: bool,

    /// Whose scope the binding is resolved in.
    #[serde(default)]
    pub scope: BindingScope,

    /// Optional guest mount path for mountable kinds (`state`, `user_files`,
    /// `context`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount: Option<String>,

    /// Optional provider hint (e.g. `dedicated`, `cloud`, `local` for a
    /// `runner` binding).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingKind {
    Secret,
    Oauth,
    State,
    UserFiles,
    Llm,
    Runner,
    Context,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum BindingScope {
    /// Bound to the invoking user (default; survives capsule swaps).
    #[default]
    User,
    /// Bound to this capsule install.
    Capsule,
    /// Bound to a single session.
    Session,
}

/// `[external.<name>]` — a heavy external capability (LLM, vector-db, browser).
///
/// Public Instant Run allows at most [`MAX_EXTERNAL_CAPABILITIES`]. The
/// `degraded` policy keeps the session usable (as a demo) when provisioning
/// fails.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalCapabilitySpec {
    /// Capability category (`llm`, `service`, `browser_worker`, …). Authored as
    /// the TOML key `type`.
    #[serde(rename = "type")]
    pub kind: String,

    /// Whether the session cannot run without it.
    #[serde(default)]
    pub required: bool,

    /// Acceptable providers, in preference order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<String>,

    /// Single provider shorthand (when `providers` is not used).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,

    /// Whether independent capabilities may be provisioned concurrently.
    #[serde(default)]
    pub provision: ProvisionMode,

    /// Locality preference for the provider.
    #[serde(default)]
    pub locality: Locality,

    /// What to do when provisioning fails.
    #[serde(default)]
    pub degraded: DegradedMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProvisionMode {
    /// Provision concurrently with other capabilities (default).
    #[default]
    Parallel,
    /// Provision one at a time.
    Sequential,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Locality {
    /// Prefer a local provider, fall back to remote (default).
    #[default]
    LocalPreferred,
    /// Any provider is acceptable.
    Any,
    /// Require a cloud provider.
    Cloud,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DegradedMode {
    /// Fall back to a demo / sample-data mode (default).
    #[default]
    Demo,
    /// Disable the capability but keep the session running.
    Disable,
    /// Fail the run.
    Fail,
}

/// `[context]` — User Context Store binding (state that survives capsule swaps).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextConfig {
    /// Sharing scope of the bound store.
    #[serde(default)]
    pub store: ContextStore,

    /// Persist outputs/files to the user's artifact store.
    #[serde(default)]
    pub artifacts: bool,

    /// Maintain a user-side index/history.
    #[serde(default)]
    pub index: bool,

    /// Guest mount path for the context store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount: Option<String>,

    /// Record grant/provenance metadata for each access.
    #[serde(default)]
    pub provenance: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ContextStore {
    /// Private to this capsule (default).
    #[default]
    AppPrivate,
    /// Bound to the user, single capsule at a time.
    User,
    /// Bound to the user, shared across capsules.
    UserShared,
}

fn default_true() -> bool {
    true
}

/// Maximum number of `[external.*]` capabilities permitted for a Public Instant
/// Run. Authored capsules may declare more, but they fail the eligibility gate.
pub const MAX_EXTERNAL_CAPABILITIES: usize = 3;

/// Computed Public Instant Run eligibility, derived entirely from the manifest.
///
/// This is a *read-only projection*: it never mutates the manifest and is safe
/// to compute at parse time, at store-apply time, or in the run pipeline. It
/// records each predicate independently so callers can surface exactly which
/// gate failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstantRunEligibility {
    /// All predicates passed.
    pub eligible: bool,
    /// Network posture is default-deny (no unrestricted egress).
    pub network_default_deny: bool,
    /// No persistent state is declared.
    pub ephemeral_only: bool,
    /// No required secret blocks a no-setup run.
    pub no_secrets_required: bool,
    /// Number of declared external capabilities.
    pub external_count: usize,
    /// `external_count <= MAX_EXTERNAL_CAPABILITIES`.
    pub external_within_limit: bool,
    /// A readiness/healthcheck signal exists (so an instant run can be detected
    /// as ready).
    pub has_healthcheck: bool,
    /// Human-readable reasons the capsule is ineligible (empty when eligible).
    pub blocking_reasons: Vec<String>,
}

impl CapsuleManifest {
    /// The `[snapshot]` table, or the default (`mode = none`) when omitted.
    pub fn snapshot_config(&self) -> SnapshotConfig {
        self.snapshot.clone().unwrap_or_default()
    }

    /// Whether this capsule opts into the Ready-State (warm/booted) line.
    pub fn is_ready_state_eligible(&self) -> bool {
        self.snapshot
            .as_ref()
            .map(SnapshotConfig::is_ready_state)
            .unwrap_or(false)
    }

    /// Compute Public Instant Run eligibility from the manifest alone.
    ///
    /// Predicates (all must hold):
    /// * **network default-deny** — declared `requirements.capabilities.network`
    ///   is `none`, or it is `egress`/`ingress`/`bidirectional` but constrained
    ///   by a non-empty egress allowlist. An undeclared network posture is
    ///   treated as default-deny (nothing was requested).
    /// * **ephemeral only** — no `[state.*]` entry is `persistent`.
    /// * **no required secrets** — neither `capabilities.secrets_required` nor
    ///   any `[secrets.*]` entry marked `required`.
    /// * **external count ≤ 3** — at most [`MAX_EXTERNAL_CAPABILITIES`]
    ///   `[external.*]` entries.
    /// * **healthcheck present** — a readiness probe exists on the *serving*
    ///   target (`default_target`, or the sole target when none is named) or a
    ///   service in that target's dependency graph. A probe on an unrelated
    ///   target does not count.
    pub fn instant_run_eligibility(&self) -> InstantRunEligibility {
        let mut blocking_reasons = Vec::new();

        // ── network default-deny ──────────────────────────────────────────
        let declared_network = self
            .requirements
            .capabilities
            .as_ref()
            .and_then(|c| c.network);
        let has_egress_allowlist = self
            .network
            .as_ref()
            .map(|n| !n.egress_allow.is_empty() || !n.egress_id_allow.is_empty())
            .unwrap_or(false);
        let network_default_deny = match declared_network {
            None => true,
            Some(crate::schema::capabilities::Network::None) => true,
            Some(_) => has_egress_allowlist,
        };
        if !network_default_deny {
            blocking_reasons.push(
                "network posture is not default-deny: an unrestricted-egress capability is \
                 declared without an egress allowlist"
                    .to_string(),
            );
        }

        // ── ephemeral only ────────────────────────────────────────────────
        let persistent_states: Vec<&str> = self
            .state
            .iter()
            .filter(|(_, req)| matches!(req.durability, super::manifest::StateDurability::Persistent))
            .map(|(name, _)| name.as_str())
            .collect();
        let ephemeral_only = persistent_states.is_empty();
        if !ephemeral_only {
            blocking_reasons.push(format!(
                "persistent state declared: [state.{}]",
                persistent_states.join("], [state.")
            ));
        }

        // ── no required secrets ───────────────────────────────────────────
        let caps_secrets_required = self
            .requirements
            .capabilities
            .as_ref()
            .and_then(|c| c.secrets_required)
            .unwrap_or(false);
        let required_secret_names: Vec<&str> = self
            .secrets
            .iter()
            .filter(|(_, spec)| spec.required)
            .map(|(name, _)| name.as_str())
            .collect();
        let no_secrets_required = !caps_secrets_required && required_secret_names.is_empty();
        if caps_secrets_required {
            blocking_reasons
                .push("requirements.capabilities.secrets_required is true".to_string());
        }
        if !required_secret_names.is_empty() {
            blocking_reasons.push(format!(
                "required secret(s): [secrets.{}]",
                required_secret_names.join("], [secrets.")
            ));
        }

        // ── external capability count ─────────────────────────────────────
        let external_count = self.external.len();
        let external_within_limit = external_count <= MAX_EXTERNAL_CAPABILITIES;
        if !external_within_limit {
            blocking_reasons.push(format!(
                "{external_count} external capabilities declared, limit is \
                 {MAX_EXTERNAL_CAPABILITIES}"
            ));
        }

        // ── healthcheck present (scoped to the SERVING target) ────────────
        // A readiness probe only makes a run ready-detectable if it sits on the
        // target that actually serves — the `default_target` (or, when none is
        // named, the sole target). A probe on some *other* target does not let
        // `ato run` decide the served capsule is ready, so it must NOT count.
        let named = self.targets.as_ref().map(|t| t.named_targets());
        let serving_label: Option<&str> = {
            let dt = self.default_target.trim();
            if !dt.is_empty() {
                Some(dt)
            } else {
                // No default named: unambiguous only when exactly one target.
                named.and_then(|nt| {
                    if nt.len() == 1 {
                        nt.keys().next().map(String::as_str)
                    } else {
                        None
                    }
                })
            }
        };
        let serving_target = serving_label
            .and_then(|label| named.and_then(|nt| nt.get(label)));

        let target_has_probe = serving_target
            .map(|nt| nt.readiness_probe.is_some())
            .unwrap_or(false);

        // Services count only when they belong to the serving target's graph:
        // a service whose `target` is the serving label (or omitted — the router
        // falls back to `default_target`), plus that service's `depends_on`
        // closure. A probe on a service bound to a different target is ignored.
        let service_has_probe = match (serving_label, self.services.as_ref()) {
            (Some(label), Some(services)) => {
                let mut stack: Vec<&str> = services
                    .iter()
                    .filter(|(_, svc)| match &svc.target {
                        Some(t) => t == label,
                        None => true, // router binds target-less services to default_target
                    })
                    .map(|(name, _)| name.as_str())
                    .collect();
                let mut seen = std::collections::HashSet::new();
                let mut found = false;
                while let Some(name) = stack.pop() {
                    if !seen.insert(name) {
                        continue;
                    }
                    if let Some(svc) = services.get(name) {
                        if svc.readiness_probe.is_some() {
                            found = true;
                            break;
                        }
                        if let Some(deps) = &svc.depends_on {
                            stack.extend(deps.iter().map(String::as_str));
                        }
                    }
                }
                found
            }
            _ => false,
        };

        let has_healthcheck = target_has_probe || service_has_probe;
        if !has_healthcheck {
            let where_ = serving_label.unwrap_or("<unresolved default target>");
            blocking_reasons.push(format!(
                "no readiness probe on the serving target '{where_}' or its service graph"
            ));
        }

        let eligible = blocking_reasons.is_empty();
        InstantRunEligibility {
            eligible,
            network_default_deny,
            ephemeral_only,
            no_secrets_required,
            external_count,
            external_within_limit,
            has_healthcheck,
            blocking_reasons,
        }
    }
}

#[cfg(test)]
mod tests;
