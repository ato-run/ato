//! Runtime observation v1 (#490): the *observed* execution-identity layer.
//!
//! The declared and resolved layers are graph-derived
//! ([`super::super::execution_graph::launch_bundle::DerivedExecutionIds`]).
//! This module adds the third layer — what was *actually observed* after the
//! workload spawned and reached readiness — as an explicit, self-contained
//! projection rather than by overloading the resolved graph (which would
//! conflate "what we asked for" with "what we saw").
//!
//! Two types:
//!
//! - [`ObservedLaunchEnvelope`] — the **canonical** observed facts, and the
//!   *only* input to [`ObservedLaunchEnvelope::compute_observed_execution_id`].
//!   Every field is host-independent and leak-free by construction: logical
//!   runtime identity (never a host path), the post-profile *logical*
//!   entrypoint (never the executor's host-path-laden wrapper argv), a
//!   workspace-relative working directory, environment variable **keys** only
//!   (never values), and in-guest mount **targets** (never host source paths).
//!
//! - [`ObservedRuntimeEvidence`] — the envelope plus **diagnostic-only**
//!   facts that must never enter identity: the actual bound port and local URL
//!   (runtime-assigned, vary run-to-run like the effective port). PID,
//!   container id, log paths, and timestamps are deliberately not carried here
//!   at all in v1; if added later they remain diagnostic, never identity.
//!
//! `observed_execution_id` is `sha256:<hex>` to match the declared/resolved
//! graph-id format (`GraphCanonicalForm::digest_hex`). It is derived from the
//! canonical envelope bytes — it is never copied from `resolved_execution_id`
//! (which is one hash input among several, not the output) and is never
//! synthesized merely because a process started.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Domain tag mixed into the canonical bytes so an observed digest can never
/// collide with a declared/resolved graph digest of coincidentally-equal
/// content. Mirrors `CanonicalGraphDomain::Observed` (discriminant 2).
const OBSERVED_ENVELOPE_DOMAIN_TAG: u8 = 2;
const OBSERVED_ENVELOPE_MAGIC: &[u8] = b"ATO-OBSERVED-ENV";
const OBSERVED_ENVELOPE_VERSION: u32 = 1;

/// Canonical, host-independent observed launch-envelope facts (#490).
///
/// This is the sole identity input for `observed_execution_id`. See the module
/// docs for the redaction contract — nothing host-specific or secret-bearing
/// may be added here.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ObservedLaunchEnvelope {
    /// The resolved-domain id this observation is anchored to. Mixed into the
    /// digest as one input among several so observations of different resolved
    /// launches never collide — **not** copied to `observed_execution_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_execution_id: Option<String>,
    /// Actual runtime kind/provider used, logical form (e.g. `"source/node"`,
    /// `"oci/podman"`). Never a host path.
    pub runtime_kind: String,
    /// Resolved runtime identity in logical form (e.g. `"deno 2.6.8"`, an image
    /// digest). Never the host filesystem path of the runtime binary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_identity: Option<String>,
    /// Entrypoint after launch-profile application, as the *logical*
    /// command + args (e.g. `["node", "server.js"]`) — never the executor's
    /// host-path-laden wrapper invocation (`deno run --allow-read=/abs/... `).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entrypoint: Vec<String>,
    /// Working directory relative to the workspace root (`"."`, `"sub/dir"`),
    /// or `None` when it is outside the workspace / not derivable. Never a raw
    /// host path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    /// Observed environment variable **keys** only. Sorted + deduped on
    /// construction via [`ObservedLaunchEnvelope::normalized`]. Never values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_keys: Vec<String>,
    /// Effective in-guest filesystem mount **targets**. Sorted + deduped. Host
    /// source paths are never recorded.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mount_targets: Vec<String>,
    /// Identity/digest of the OCI provider projection already persisted for
    /// this launch, when available. `None` for non-provider launches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_projection_digest: Option<String>,
}

impl ObservedLaunchEnvelope {
    /// Return a copy with set-valued fields normalized (sorted + deduped) so
    /// the digest is independent of collection order. `entrypoint` is **not**
    /// sorted — argv order is semantic.
    pub fn normalized(&self) -> Self {
        let mut env_keys = self.env_keys.clone();
        env_keys.sort();
        env_keys.dedup();
        let mut mount_targets = self.mount_targets.clone();
        mount_targets.sort();
        mount_targets.dedup();
        Self {
            resolved_execution_id: self.resolved_execution_id.clone(),
            runtime_kind: self.runtime_kind.clone(),
            runtime_identity: self.runtime_identity.clone(),
            entrypoint: self.entrypoint.clone(),
            working_directory: self.working_directory.clone(),
            env_keys,
            mount_targets,
            provider_projection_digest: self.provider_projection_digest.clone(),
        }
    }

    /// Deterministic, framing-unambiguous canonical bytes for the envelope.
    ///
    /// Length-prefixed throughout (a `None`/absent value is encoded as a `0`
    /// presence byte, distinct from a present empty string) so no two distinct
    /// envelopes can serialize to the same bytes.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let n = self.normalized();
        let mut out = Vec::new();
        out.extend_from_slice(OBSERVED_ENVELOPE_MAGIC);
        out.extend_from_slice(&OBSERVED_ENVELOPE_VERSION.to_le_bytes());
        out.push(OBSERVED_ENVELOPE_DOMAIN_TAG);
        write_opt_str(&mut out, n.resolved_execution_id.as_deref());
        write_lp_str(&mut out, &n.runtime_kind);
        write_opt_str(&mut out, n.runtime_identity.as_deref());
        write_str_vec(&mut out, &n.entrypoint);
        write_opt_str(&mut out, n.working_directory.as_deref());
        write_str_vec(&mut out, &n.env_keys);
        write_str_vec(&mut out, &n.mount_targets);
        write_opt_str(&mut out, n.provider_projection_digest.as_deref());
        out
    }

    /// Compute `observed_execution_id` (`sha256:<64-hex>`) from the canonical
    /// envelope bytes. Pure: the same envelope always yields the same id, and
    /// the id changes iff a canonical field changes.
    pub fn compute_observed_execution_id(&self) -> String {
        let digest = Sha256::digest(self.canonical_bytes());
        format!("sha256:{}", hex::encode(digest))
    }

    /// Whether the envelope carries enough real observed evidence to honestly
    /// identify the observed launch. An envelope with no runtime kind and no
    /// entrypoint is not a real observation (e.g. a failed/early-exit probe
    /// that collected nothing) — callers must treat this as "not observed" and
    /// leave `observed_execution_id = None`.
    pub fn has_minimal_evidence(&self) -> bool {
        !self.runtime_kind.trim().is_empty() && !self.entrypoint.is_empty()
    }
}

/// The observed launch envelope plus diagnostic-only runtime facts.
///
/// Persisted into `ExecutionReceiptV2.observed_runtime`. Only [`Self::envelope`]
/// feeds `observed_execution_id`; the remaining fields are diagnostic and must
/// never become identity inputs (they are runtime-assigned and vary run-to-run).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ObservedRuntimeEvidence {
    pub envelope: ObservedLaunchEnvelope,
    /// Actual bound web port. Diagnostic only — runtime-assigned (may be
    /// remapped off an occupied declared port), so it is **not** an identity
    /// input, mirroring the launch digest's exclusion of the effective port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_port: Option<u16>,
    /// Local URL the runtime bound (e.g. `http://127.0.0.1:<port>/`).
    /// Diagnostic only; derived from `bound_port`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_url: Option<String>,
}

impl ObservedRuntimeEvidence {
    pub fn new(envelope: ObservedLaunchEnvelope) -> Self {
        Self {
            envelope,
            bound_port: None,
            local_url: None,
        }
    }

    pub fn with_bound_port(mut self, port: Option<u16>) -> Self {
        self.bound_port = port;
        self
    }

    pub fn with_local_url(mut self, url: Option<String>) -> Self {
        self.local_url = url;
        self
    }
}

fn write_lp_str(out: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    let len: u32 = bytes.len().try_into().unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(bytes);
}

fn write_opt_str(out: &mut Vec<u8>, s: Option<&str>) {
    match s {
        Some(value) => {
            out.push(1);
            write_lp_str(out, value);
        }
        None => out.push(0),
    }
}

fn write_str_vec(out: &mut Vec<u8>, values: &[String]) {
    let count: u32 = values.len().try_into().unwrap_or(u32::MAX);
    out.extend_from_slice(&count.to_le_bytes());
    for value in values {
        write_lp_str(out, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ObservedLaunchEnvelope {
        ObservedLaunchEnvelope {
            resolved_execution_id: Some("sha256:resolved".to_string()),
            runtime_kind: "source/node".to_string(),
            runtime_identity: Some("deno 2.6.8".to_string()),
            entrypoint: vec!["node".to_string(), "server.js".to_string()],
            working_directory: Some(".".to_string()),
            env_keys: vec!["PORT".to_string(), "NODE_ENV".to_string()],
            mount_targets: vec!["/app/data".to_string()],
            provider_projection_digest: None,
        }
    }

    #[test]
    fn observed_id_is_sha256_prefixed_lowercase_hex() {
        let id = sample().compute_observed_execution_id();
        assert!(id.starts_with("sha256:"), "got {id}");
        assert_eq!(id.len(), "sha256:".len() + 64);
        assert!(
            id[7..]
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    #[test]
    fn observed_id_is_stable_and_order_independent_for_sets() {
        let a = sample();
        let mut b = sample();
        b.env_keys.reverse();
        b.mount_targets.reverse();
        assert_eq!(
            a.compute_observed_execution_id(),
            b.compute_observed_execution_id(),
            "env/mount ordering must not change the observed id"
        );
    }

    #[test]
    fn observed_id_changes_when_an_envelope_fact_changes() {
        let base = sample().compute_observed_execution_id();
        let mut changed = sample();
        changed.entrypoint.push("--flag".to_string());
        assert_ne!(base, changed.compute_observed_execution_id());

        let mut changed_rt = sample();
        changed_rt.runtime_identity = Some("deno 2.6.9".to_string());
        assert_ne!(base, changed_rt.compute_observed_execution_id());

        let mut changed_env = sample();
        changed_env.env_keys.push("EXTRA".to_string());
        assert_ne!(base, changed_env.compute_observed_execution_id());
    }

    #[test]
    fn observed_id_is_not_copied_from_resolved_execution_id() {
        let env = sample();
        let id = env.compute_observed_execution_id();
        assert_ne!(
            id,
            env.resolved_execution_id.clone().unwrap(),
            "observed id must be derived, never a copy of resolved id"
        );
    }

    #[test]
    fn entrypoint_order_is_significant() {
        let mut swapped = sample();
        swapped.entrypoint.swap(0, 1);
        assert_ne!(
            sample().compute_observed_execution_id(),
            swapped.compute_observed_execution_id(),
            "argv order is semantic and must affect the id"
        );
    }

    #[test]
    fn none_and_empty_string_do_not_collide() {
        let mut none_rt = sample();
        none_rt.runtime_identity = None;
        let mut empty_rt = sample();
        empty_rt.runtime_identity = Some(String::new());
        assert_ne!(
            none_rt.compute_observed_execution_id(),
            empty_rt.compute_observed_execution_id(),
        );
    }

    #[test]
    fn env_values_can_never_be_serialized_only_keys() {
        // The model carries environment *keys* only — there is no field for
        // values — so a secret env value can never be written into a receipt.
        let evidence = ObservedRuntimeEvidence::new(ObservedLaunchEnvelope {
            runtime_kind: "source/node".to_string(),
            entrypoint: vec!["node".to_string()],
            env_keys: vec!["DATABASE_URL".to_string(), "API_KEY".to_string()],
            ..Default::default()
        });
        let json = serde_json::to_string(&evidence).expect("serialize");
        // Keys are present...
        assert!(json.contains("DATABASE_URL"));
        assert!(json.contains("API_KEY"));
        // ...but no value-bearing field exists, so a secret value cannot appear.
        assert!(!json.contains("env_values"));
        assert!(!json.contains("\"values\""));
        assert!(!json.contains("postgres://"));
    }

    #[test]
    fn has_minimal_evidence_requires_kind_and_entrypoint() {
        assert!(sample().has_minimal_evidence());
        let mut no_entry = sample();
        no_entry.entrypoint.clear();
        assert!(!no_entry.has_minimal_evidence());
        let mut no_kind = sample();
        no_kind.runtime_kind = String::new();
        assert!(!no_kind.has_minimal_evidence());
    }
}
