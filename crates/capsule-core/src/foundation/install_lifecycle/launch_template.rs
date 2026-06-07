//! Reusable launch-time inputs: binding assignments, launch templates, the
//! launch-template cache key, and the runner compatibility index
//! (RFC: Ato Resource Namespace §"Install Outputs and Launch Reuse"; #581 Stage 2).
//!
//! # The cache-key contract
//!
//! A [`LaunchTemplate`] is **session-independent**: it is built once per
//! `(install revision, profile, bindings, policies, runner class)` and reused
//! across launches. Its identity is [`LaunchTemplateKey`], which by construction
//! contains **only** stable install-time inputs:
//!
//! ```text
//! install_revision_id
//! profile_hash
//! requirement_graph_hash
//! binding_set_hash
//! network_policy_hash
//! capability_policy_hash
//! state_contract_hash
//! runner_compatibility_class
//! ```
//!
//! It must **never** contain a `session_id`, dynamic port, process / container
//! id, live route, log cursor, observed status, timestamp, or secret value.
//! Those are runtime-observed facts; feeding them into the key would make every
//! launch miss the cache and would leak volatile state into a "stable" identity.
//! The exclusion is asserted by
//! [`tests::template_key_ignores_session_and_observed_facts`].

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::hashing::canonical_hash;
use super::ids::InstallRevisionId;

// ── Runner class / compatibility class ───────────────────────────────────────

/// Runner classes recognised by the v0 launch-reuse model (RFC §"Runner Classes").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerClass {
    ManagedRunner,
    DesktopRunner,
    ExternalRunner,
    BrowserRunner,
    BrowserPreviewRunner,
}

/// A stable, coarse compatibility class a launch template is built for.
///
/// This is a launch-template input (it changes the key), unlike a concrete
/// `runner_id` or a live capability snapshot (which are placement-time and
/// session-time facts and must not enter the key). Example values:
/// `"managed_runner/linux-x86_64"`, `"browser_runner/wasm"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunnerCompatibilityClass(String);

impl RunnerCompatibilityClass {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ── BindingAssignmentSet ─────────────────────────────────────────────────────

/// What kind of binding a requirement resolved to (RFC `RequirementBindingKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementBindingKind {
    Resource,
    ResourceSet,
    RunnerCapability,
    EnforcementOnly,
    ConsentOnly,
    ProvisionedResource,
}

/// A normalized, resolved binding for one requirement.
///
/// This is the *resolved* representation — not the raw desired JSONC/profile
/// text, which is never the install authority. Resource references are stored,
/// never secret values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequirementBinding {
    pub requirement_id: String,
    pub binding_kind: RequirementBindingKind,
    /// Single resolved resource reference (a namespace path / id), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_resource_ref: Option<String>,
    /// Multiple resolved resource references, for set/enforcement bindings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolved_resource_refs: Vec<String>,
    /// Whether this binding flows into launch-envelope (execution) identity.
    pub affects_execution_identity: bool,
}

/// Source the binding assignment set was normalized from (diagnostic only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingAssignmentSource {
    ProfileExplicit,
    UserDefault,
    WorkspaceDefault,
    ManagedDefault,
    Provisioned,
}

/// The normalized / resolved representation of desired bindings.
///
/// Session-independent. `binding_set_hash` flows into [`LaunchTemplateKey`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingAssignmentSet {
    pub binding_set_id: String,
    pub binding_set_hash: String,
    pub install_profile_key: super::ids::InstallProfileKey,
    pub requirement_graph_hash: String,
    pub assignments: Vec<RequirementBinding>,
    pub created_from: BindingAssignmentSource,
}

impl BindingAssignmentSet {
    /// Build a set, computing `binding_set_hash` from the normalized assignments.
    ///
    /// Assignments are sorted by `requirement_id` before hashing so the hash is
    /// independent of the order they were resolved in.
    pub fn new(
        binding_set_id: impl Into<String>,
        install_profile_key: super::ids::InstallProfileKey,
        requirement_graph_hash: impl Into<String>,
        mut assignments: Vec<RequirementBinding>,
        created_from: BindingAssignmentSource,
    ) -> Result<Self> {
        assignments.sort_by(|a, b| a.requirement_id.cmp(&b.requirement_id));
        let binding_set_hash = canonical_hash(&assignments)?;
        Ok(Self {
            binding_set_id: binding_set_id.into(),
            binding_set_hash,
            install_profile_key,
            requirement_graph_hash: requirement_graph_hash.into(),
            assignments,
            created_from,
        })
    }
}

// ── CompatibilityIndex ───────────────────────────────────────────────────────

/// A precheck record for which runner classes / capabilities can launch a
/// revision (RFC `CompatibilityIndex`). Built once at install time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompatibilityIndex {
    pub index_id: String,
    pub supported_runner_classes: Vec<RunnerClass>,
    pub denied_runner_classes: Vec<RunnerClass>,
    pub required_capabilities: Vec<String>,
    pub optional_capabilities: Vec<String>,
    pub precheck_hash: String,
}

impl CompatibilityIndex {
    pub fn new(
        index_id: impl Into<String>,
        supported_runner_classes: Vec<RunnerClass>,
        denied_runner_classes: Vec<RunnerClass>,
        required_capabilities: Vec<String>,
        optional_capabilities: Vec<String>,
    ) -> Result<Self> {
        let precheck_hash = canonical_hash(&(
            &supported_runner_classes,
            &denied_runner_classes,
            &required_capabilities,
            &optional_capabilities,
        ))?;
        Ok(Self {
            index_id: index_id.into(),
            supported_runner_classes,
            denied_runner_classes,
            required_capabilities,
            optional_capabilities,
            precheck_hash,
        })
    }

    /// Explicit precheck: is `class` allowed to launch this revision?
    ///
    /// A class is supported when it is in `supported_runner_classes` and not in
    /// `denied_runner_classes` (deny wins). This is the v0 precheck; first-fit
    /// fallback among supported classes is a later concern.
    pub fn is_supported(&self, class: &RunnerClass) -> bool {
        if self.denied_runner_classes.contains(class) {
            return false;
        }
        self.supported_runner_classes.contains(class)
    }
}

// ── LaunchTemplateKey ─────────────────────────────────────────────────────────

/// The cache identity of a [`LaunchTemplate`].
///
/// Contains only stable install-time inputs (see module docs). There is
/// deliberately no field for any session-specific or observed fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchTemplateKey {
    pub install_revision_id: InstallRevisionId,
    pub profile_hash: String,
    pub requirement_graph_hash: String,
    pub binding_set_hash: String,
    pub network_policy_hash: String,
    pub capability_policy_hash: String,
    pub state_contract_hash: String,
    pub runner_compatibility_class: RunnerCompatibilityClass,
}

impl LaunchTemplateKey {
    /// Stable `blake3:<hex>` digest of the key, used for cache lookup / equality
    /// of templates. Two launches with identical install-time inputs produce the
    /// same digest; changing any stable input changes it.
    pub fn key_hash(&self) -> Result<String> {
        canonical_hash(self)
    }
}

// ── LaunchTemplate ────────────────────────────────────────────────────────────

/// A reusable, session-independent launch envelope template.
///
/// May depend on a runner *class* (`runner_compatibility_class`) but never on a
/// specific session, runner instance, or live route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchTemplate {
    pub template_id: String,
    pub template_hash: String,
    pub key: LaunchTemplateKey,
    /// Reference to the launch profile node.
    pub profile_ref: String,
    /// Reference to the artifact (content-addressed).
    pub artifact_ref: String,
    /// Reference to the requirement-graph snapshot.
    pub requirement_graph_ref: String,
    /// Reference to the binding-assignment set.
    pub binding_assignment_set_ref: String,
    /// Hash of the filesystem-view template (not the materialized view).
    pub filesystem_view_template_hash: String,
    /// Hash of the network-policy template.
    pub network_policy_template_hash: String,
    /// Hash of the capability-policy template.
    pub capability_policy_template_hash: String,
    pub runner_compatibility_class: RunnerCompatibilityClass,
}

impl LaunchTemplate {
    /// Build a template, computing `template_hash` from the key digest plus the
    /// template-shape hashes. The template is identified by its key; the
    /// `template_hash` additionally binds the projected template payload.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        template_id: impl Into<String>,
        key: LaunchTemplateKey,
        profile_ref: impl Into<String>,
        artifact_ref: impl Into<String>,
        requirement_graph_ref: impl Into<String>,
        binding_assignment_set_ref: impl Into<String>,
        filesystem_view_template_hash: impl Into<String>,
        network_policy_template_hash: impl Into<String>,
        capability_policy_template_hash: impl Into<String>,
    ) -> Result<Self> {
        let runner_compatibility_class = key.runner_compatibility_class.clone();
        let filesystem_view_template_hash = filesystem_view_template_hash.into();
        let network_policy_template_hash = network_policy_template_hash.into();
        let capability_policy_template_hash = capability_policy_template_hash.into();
        let template_hash = canonical_hash(&(
            key.key_hash()?,
            &filesystem_view_template_hash,
            &network_policy_template_hash,
            &capability_policy_template_hash,
        ))?;
        Ok(Self {
            template_id: template_id.into(),
            template_hash,
            key,
            profile_ref: profile_ref.into(),
            artifact_ref: artifact_ref.into(),
            requirement_graph_ref: requirement_graph_ref.into(),
            binding_assignment_set_ref: binding_assignment_set_ref.into(),
            filesystem_view_template_hash,
            network_policy_template_hash,
            capability_policy_template_hash,
            runner_compatibility_class,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_key() -> LaunchTemplateKey {
        LaunchTemplateKey {
            install_revision_id: InstallRevisionId::new("rev_aaaa"),
            profile_hash: "blake3:prof".into(),
            requirement_graph_hash: "blake3:graph".into(),
            binding_set_hash: "blake3:bind".into(),
            network_policy_hash: "blake3:net".into(),
            capability_policy_hash: "blake3:cap".into(),
            state_contract_hash: "blake3:state".into(),
            runner_compatibility_class: RunnerCompatibilityClass::new(
                "managed_runner/linux-x86_64",
            ),
        }
    }

    // ── Acceptance: key stable across repeated launches with same inputs ──────

    #[test]
    fn template_key_stable_across_repeated_launches() {
        let k1 = sample_key();
        let k2 = sample_key();
        assert_eq!(
            k1.key_hash().unwrap(),
            k2.key_hash().unwrap(),
            "same install-time inputs must produce the same key hash on every launch"
        );
    }

    // ── Acceptance: changing a stable input changes the key ───────────────────

    #[test]
    fn template_key_changes_when_stable_inputs_change() {
        let base = sample_key().key_hash().unwrap();

        let mut k = sample_key();
        k.profile_hash = "blake3:prof2".into();
        assert_ne!(base, k.key_hash().unwrap(), "profile_hash must affect key");

        let mut k = sample_key();
        k.binding_set_hash = "blake3:bind2".into();
        assert_ne!(
            base,
            k.key_hash().unwrap(),
            "binding_set_hash must affect key"
        );

        let mut k = sample_key();
        k.network_policy_hash = "blake3:net2".into();
        assert_ne!(
            base,
            k.key_hash().unwrap(),
            "network_policy_hash must affect key"
        );

        let mut k = sample_key();
        k.requirement_graph_hash = "blake3:graph2".into();
        assert_ne!(
            base,
            k.key_hash().unwrap(),
            "requirement_graph_hash must affect key"
        );

        let mut k = sample_key();
        k.state_contract_hash = "blake3:state2".into();
        assert_ne!(
            base,
            k.key_hash().unwrap(),
            "state_contract_hash must affect key"
        );

        let mut k = sample_key();
        k.runner_compatibility_class = RunnerCompatibilityClass::new("browser_runner/wasm");
        assert_ne!(
            base,
            k.key_hash().unwrap(),
            "runner_compatibility_class must affect key"
        );

        let mut k = sample_key();
        k.install_revision_id = InstallRevisionId::new("rev_bbbb");
        assert_ne!(
            base,
            k.key_hash().unwrap(),
            "install_revision_id must affect key"
        );
    }

    // ── Acceptance: observed/session facts do NOT change the key ──────────────

    #[test]
    fn template_key_ignores_session_and_observed_facts() {
        // Compute the key.
        let base = sample_key().key_hash().unwrap();

        // A launch happens: it produces all of the following observed/session
        // facts. None of them are fields of LaunchTemplateKey, so none can be
        // fed into the key — the digest is unchanged across two launches whose
        // only difference is these runtime facts.
        struct ObservedSessionFacts {
            session_id: &'static str,
            dynamic_port: u16,
            process_id: u32,
            container_id: &'static str,
            live_route: &'static str,
            log_cursor: &'static str,
            observed_status: &'static str,
            timestamp: &'static str,
            secret_value: &'static str,
        }
        let launch_a = ObservedSessionFacts {
            session_id: "ses_a",
            dynamic_port: 40001,
            process_id: 111,
            container_id: "ctr_a",
            live_route: "https://a.live",
            log_cursor: "cursor:1",
            observed_status: "running",
            timestamp: "2026-06-08T00:00:00Z",
            secret_value: "hunter2",
        };
        let launch_b = ObservedSessionFacts {
            session_id: "ses_b",
            dynamic_port: 59999,
            process_id: 222,
            container_id: "ctr_b",
            live_route: "https://b.live",
            log_cursor: "cursor:9999",
            observed_status: "stopping",
            timestamp: "2026-06-08T12:00:00Z",
            secret_value: "swordfish",
        };
        // Touch the fields so the compiler does not optimise them away and so a
        // future refactor that tried to thread them into the key would surface
        // here.
        let _ = (
            launch_a.session_id,
            launch_a.dynamic_port,
            launch_a.process_id,
            launch_a.container_id,
            launch_a.live_route,
            launch_a.log_cursor,
            launch_a.observed_status,
            launch_a.timestamp,
            launch_a.secret_value,
            launch_b.session_id,
            launch_b.dynamic_port,
            launch_b.process_id,
            launch_b.container_id,
            launch_b.live_route,
            launch_b.log_cursor,
            launch_b.observed_status,
            launch_b.timestamp,
            launch_b.secret_value,
        );

        let key_after_launch_a = sample_key().key_hash().unwrap();
        let key_after_launch_b = sample_key().key_hash().unwrap();
        assert_eq!(base, key_after_launch_a);
        assert_eq!(base, key_after_launch_b);
        assert_eq!(
            key_after_launch_a, key_after_launch_b,
            "observed/session facts must never change the launch template key"
        );
    }

    // ── BindingAssignmentSet hashing ──────────────────────────────────────────

    #[test]
    fn binding_set_hash_is_order_independent_and_content_sensitive() {
        let ipk = super::super::ids::InstallProfileKey::new("ipk_x");
        let a = RequirementBinding {
            requirement_id: "secret.database_url".into(),
            binding_kind: RequirementBindingKind::Resource,
            resolved_resource_ref: Some("/secrets/sec_db".into()),
            resolved_resource_refs: vec![],
            affects_execution_identity: true,
        };
        let b = RequirementBinding {
            requirement_id: "storage.user_data".into(),
            binding_kind: RequirementBindingKind::Resource,
            resolved_resource_ref: Some("/storage/stor_s3".into()),
            resolved_resource_refs: vec![],
            affects_execution_identity: true,
        };
        let set1 = BindingAssignmentSet::new(
            "bset_1",
            ipk.clone(),
            "blake3:graph",
            vec![a.clone(), b.clone()],
            BindingAssignmentSource::ProfileExplicit,
        )
        .unwrap();
        let set2 = BindingAssignmentSet::new(
            "bset_2",
            ipk.clone(),
            "blake3:graph",
            vec![b, a],
            BindingAssignmentSource::ProfileExplicit,
        )
        .unwrap();
        assert_eq!(
            set1.binding_set_hash, set2.binding_set_hash,
            "binding set hash must be independent of assignment order"
        );

        // Changing a resolved resource changes the hash.
        let c = RequirementBinding {
            requirement_id: "secret.database_url".into(),
            binding_kind: RequirementBindingKind::Resource,
            resolved_resource_ref: Some("/secrets/sec_db_OTHER".into()),
            resolved_resource_refs: vec![],
            affects_execution_identity: true,
        };
        let set3 = BindingAssignmentSet::new(
            "bset_3",
            ipk,
            "blake3:graph",
            vec![c],
            BindingAssignmentSource::ProfileExplicit,
        )
        .unwrap();
        assert_ne!(set1.binding_set_hash, set3.binding_set_hash);
    }

    // ── CompatibilityIndex precheck is explicit ───────────────────────────────

    #[test]
    fn compatibility_index_precheck_is_explicit() {
        let idx = CompatibilityIndex::new(
            "cidx_pgweb",
            vec![RunnerClass::ManagedRunner, RunnerClass::DesktopRunner],
            vec![RunnerClass::BrowserRunner],
            vec!["server_process".into()],
            vec![],
        )
        .unwrap();

        assert!(idx.is_supported(&RunnerClass::ManagedRunner));
        assert!(idx.is_supported(&RunnerClass::DesktopRunner));
        assert!(
            !idx.is_supported(&RunnerClass::BrowserRunner),
            "browser runner must be rejected by precheck for a server-process app"
        );
        assert!(
            !idx.is_supported(&RunnerClass::ExternalRunner),
            "a class neither supported nor denied is not supported (closed by default)"
        );
    }

    #[test]
    fn compatibility_index_deny_wins_over_supported() {
        // Even if a class is in both lists, deny wins (fail-closed).
        let idx = CompatibilityIndex::new(
            "cidx_conflict",
            vec![RunnerClass::BrowserRunner],
            vec![RunnerClass::BrowserRunner],
            vec![],
            vec![],
        )
        .unwrap();
        assert!(!idx.is_supported(&RunnerClass::BrowserRunner));
    }

    #[test]
    fn launch_template_builds_and_roundtrips() {
        let tmpl = LaunchTemplate::new(
            "ltmpl_1",
            sample_key(),
            "/instances/ipk_x/profiles/default",
            "/artifacts/blake3/3333",
            "snap1",
            "bset_1",
            "blake3:fsview",
            "blake3:net",
            "blake3:cap",
        )
        .unwrap();
        assert!(tmpl.template_hash.starts_with("blake3:"));
        let json = serde_json::to_string(&tmpl).unwrap();
        let back: LaunchTemplate = serde_json::from_str(&json).unwrap();
        assert_eq!(tmpl, back);
    }
}
