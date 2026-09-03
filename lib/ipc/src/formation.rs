//! `ato.formation-job.v1` and `ato.formation-result.v1` — the Formation wire
//! contract.
//!
//! Formation turns a pinned source into a canonical ComputeSchema and an
//! immutable materialization. P3 turns a ComputeSchema into a Run. This module
//! is the seam, and most of its rules exist to keep the two from bleeding into
//! each other.
//!
//! ## What Formation must never be handed
//!
//! A Formation job describes CODE. It must not carry a ComputeInstance, a Run,
//! a writer fence, a state revision, a host path, a host port, a lease, a
//! stable URL, a secret value or a session. Each of those belongs to a tenant's
//! execution, not to the artifact, and a Formation service that could see one
//! would be a Formation service that could be made to act on one.
//!
//! These are refused BY NAME at parse time rather than ignored, because a
//! field that is silently dropped is a field somebody will keep sending and
//! eventually depend on.
//!
//! ## Identity
//!
//! Seven distinct identities, deliberately not collapsed:
//!
//! ```text
//! formation_job_id        one request
//! formation_attempt_id    one execution of it
//! formation_key           the digest of its normalized inputs
//! source_closure_ref      the source tree's content identity
//! compute_schema_ref      the canonical schema minted from the result
//! materialization_ref     one produced artifact
//! ```
//!
//! and the two the control plane owns and Formation never sees:
//! `compute_instance_id` and `run_id`.
//!
//! In particular a source closure is NOT its archive's transport digest — the
//! same tree can arrive as different bytes — and a build materialization is
//! neither.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const FORMATION_JOB_V1_PROTOCOL: &str = "ato.formation-job.v1";
pub const FORMATION_RESULT_V1_PROTOCOL: &str = "ato.formation-result.v1";

/// Field names a Formation payload may never contain, at any depth.
///
/// Enforced against the raw JSON before typed parsing, so a nested or unknown
/// object cannot smuggle one past `deny_unknown_fields`.
pub const FORBIDDEN_FORMATION_FIELDS: &[&str] = &[
    "compute_instance_id",
    "run_id",
    "writer_fence",
    "revision_ref",
    "state_revision",
    "host_path",
    "host_port",
    "lease_id",
    "stable_url",
    "secret",
    "secret_value",
    "redeemed_grant",
    "session_id",
    "browser_session",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormationError {
    UnsupportedVersion {
        found: String,
        expected: &'static str,
    },
    Forbidden {
        field: String,
    },
    Invalid {
        code: &'static str,
        detail: String,
    },
    Malformed(String),
}

impl FormationError {
    /// A stable code, so a caller can branch without matching on prose.
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedVersion { .. } => "ATO_ERR_FORMATION_UNSUPPORTED_VERSION",
            Self::Forbidden { .. } => "ATO_ERR_FORMATION_FORBIDDEN_FIELD",
            Self::Invalid { code, .. } => code,
            Self::Malformed(_) => "ATO_ERR_FORMATION_MALFORMED",
        }
    }
}

impl std::fmt::Display for FormationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedVersion { found, expected } => write!(
                formatter,
                "unsupported Formation protocol {found:?}; this build speaks {expected}"
            ),
            Self::Forbidden { field } => write!(
                formatter,
                "Formation payload carries {field:?}, which belongs to an execution and never to \
                 a build"
            ),
            Self::Invalid { code, detail } => write!(formatter, "{code}: {detail}"),
            Self::Malformed(detail) => write!(formatter, "malformed Formation payload: {detail}"),
        }
    }
}

impl std::error::Error for FormationError {}

// ───────────────────────────────────────────────────────────────── the source

/// Where the code comes from. Three ways in, one identity out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FormationSourceV1 {
    /// A repository plus the ref the requester named. The ref is resolved to a
    /// full commit at pin time and never used as identity afterwards: a branch
    /// moves, and a Formation that followed it would build something else on a
    /// retry.
    GitHub(GitHubSourceV1),
    /// Bytes the requester already uploaded, addressed by digest.
    UploadedArchive(UploadedArchiveSourceV1),
    /// A closure that already exists. Re-verified rather than trusted.
    ExistingSourceClosure(ExistingClosureSourceV1),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitHubSourceV1 {
    pub owner: String,
    pub repository: String,
    /// What the requester asked for: a branch, a tag or a SHA.
    pub requested_ref: String,
    /// The full commit it resolved to. Short SHAs are refused — a prefix is
    /// ambiguous, and ambiguity in an identity is a collision waiting.
    pub resolved_commit_sha: String,
    /// Repository-relative, `""` for the root. Containment is validated.
    #[serde(default)]
    pub subdirectory: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadedArchiveSourceV1 {
    pub archive_ref: String,
    /// The digest the requester claims. Verified against the bytes.
    pub expected_archive_digest: String,
    /// Optional second check over the extracted tree, which is a different
    /// thing from the archive's bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_source_tree_digest: Option<String>,
    #[serde(default)]
    pub subdirectory: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExistingClosureSourceV1 {
    pub source_closure_ref: String,
}

// ─────────────────────────────────────────────────────────────── the requests

/// What the job is asked to produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestedOutputV1 {
    StaticWeb,
    ProcessWorkspace,
    /// Reserved so the vocabulary is stable. B1 refuses it rather than
    /// half-implementing it.
    OciImage,
}

/// The execution target a materialization must match.
///
/// A process workspace holds an installed dependency tree, which is
/// target-specific: a wheel built for `x86_64-linux-gnu` does not run
/// elsewhere. Recording the target is what lets a Runner refuse a workspace it
/// cannot execute instead of failing obscurely at import time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormationTargetV1 {
    /// e.g. `x86_64-linux-gnu`.
    pub triple: String,
    /// The guest path the workspace is built at AND restored to. They must be
    /// the same string, or an interpreter shebang and a `pyvenv.cfg` written
    /// during the build point somewhere that does not exist at runtime.
    pub workspace_guest_root: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormationPolicyV1 {
    /// Which policy vocabulary this job was compiled under. Part of the
    /// formation key: the same inputs under different rules are a different
    /// build, and coalescing them would serve one requester another's answer.
    pub policy_version: String,
    /// Whether the build may reach the network, and how far.
    pub network: FormationNetworkPolicyV1,
    /// Whether the result may be published for untrusted consumption. A build
    /// whose isolation cannot support that must not set it.
    #[serde(default)]
    pub publish_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FormationNetworkPolicyV1 {
    /// No network at all. The only policy this build can currently ENFORCE for
    /// an untrusted source (ADR-018).
    Denied,
    /// Dependency resolution is allowed. Honest about what it is: today this
    /// means unrestricted egress, so it is confined to trusted sources until a
    /// mediated path exists.
    DependencyResolution,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormationJobV1 {
    pub protocol: String,
    pub job_id: String,
    /// Same key, same job. Deduplicates a resubmitted request before any work
    /// starts, which is different from `formation_key` coalescing two distinct
    /// requests that happen to describe the same build.
    pub request_idempotency_key: String,
    pub source: FormationSourceV1,
    pub requested_outputs: Vec<RequestedOutputV1>,
    pub target: FormationTargetV1,
    pub policy: FormationPolicyV1,
    /// Authored manifest and overrides, verbatim. Authored intent outranks
    /// inference, and the provenance records which field came from where.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authoring: Option<FormationAuthoringV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormationAuthoringV1 {
    /// The manifest as authored, if the source carries one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_toml: Option<String>,
    /// Explicit overrides, applied over anything inferred.
    #[serde(default)]
    pub overrides: BTreeMap<String, String>,
}

// ──────────────────────────────────────────────────────────────── the results

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FormationStatusV1 {
    Succeeded,
    Failed,
    Cancelled,
}

/// One produced artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormationMaterializationV1 {
    pub kind: MaterializationKindV1,
    /// Content address. The only way this artifact is ever named.
    pub content_ref: String,
    pub media_type: String,
    pub digest: String,
    pub size_bytes: u64,
    /// Where it belongs at runtime, in GUEST terms.
    pub target: FormationTargetV1,
    /// What must be true of a Runner for this artifact to execute.
    pub compatibility: BTreeMap<String, String>,
    /// Which producer made it, so a defect is traceable to a version.
    pub producer: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterializationKindV1 {
    StaticWeb,
    ProcessWorkspace,
    Oci,
}

/// The canonical facts a ComputeSchema is minted from.
///
/// Minted HERE, not re-derived by the control plane. If ato-api re-inferred an
/// argv or a port from the same source it would be a second detector, and two
/// detectors are two answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormationResultV1 {
    pub protocol: String,

    pub job_id: String,
    pub attempt_id: String,
    /// Monotonic per job. A result from a superseded attempt is refused on
    /// this alone — a slow build finishing after its retry must not publish.
    pub attempt_fence: u64,
    pub status: FormationStatusV1,

    /// Digest of the normalized inputs. Two jobs with the same key are the
    /// same build and may share one execution.
    pub formation_key: String,

    pub source_revision_ref: String,
    /// The tree's identity, distinct from any archive that carried it.
    pub source_closure_ref: String,

    pub program_intent_ref: String,
    pub effective_build_plan_ref: String,

    /// Left optional on purpose. The mint order for a root Computation is not
    /// determined by the current code, and inventing a Capsule with unclear
    /// meaning to fill the field would be worse than leaving it absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_computation_ref: Option<String>,
    pub compute_schema_ref: String,

    pub materializations: Vec<FormationMaterializationV1>,
    pub runtime_requirements: Vec<RuntimeRequirementV1>,
    pub realization_candidates: Vec<RealizationCandidateV1>,

    pub exported_ports: Vec<ExportedPortV1>,
    pub readiness_contracts: Vec<ReadinessContractV1>,
    pub state_slot_declarations: Vec<StateSlotDeclarationV1>,
    pub binding_requirements: Vec<BindingRequirementV1>,

    pub provenance: FormationProvenanceV1,
    pub diagnostics: Vec<FormationDiagnosticV1>,
    /// Everything that decided this build, digested. Equal digests must mean
    /// equal outputs, or determinism is a claim rather than a property.
    pub deterministic_inputs_digest: String,
}

/// A LOGICAL requirement — `python = 3.12.7` — never a physical path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeRequirementV1 {
    pub name: String,
    pub version: String,
    /// How the version was arrived at, so an operator can see whether it was
    /// pinned by the author or chosen by a default.
    pub resolution: RequirementResolutionV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementResolutionV1 {
    Authored,
    DetectedFromSource,
    PolicyDefault,
}

/// One way this schema could be realized. P3 consumes the process arm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RealizationCandidateV1 {
    Process(ProcessRealizationCandidateV1),
    StaticBrowser(StaticBrowserCandidateV1),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessRealizationCandidateV1 {
    /// The exact argv. Not a template, not a shell string: a shell string is a
    /// second place where quoting can go wrong.
    pub argv: Vec<String>,
    /// Workspace-relative. `""` is the workspace root.
    #[serde(default)]
    pub cwd_relative: String,
    /// Non-secret values only. A secret here would be published.
    #[serde(default)]
    pub public_env: BTreeMap<String, String>,
    pub workspace_materialization_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticBrowserCandidateV1 {
    pub materialization_ref: String,
    pub entry_path: String,
    #[serde(default)]
    pub spa_fallback: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportedPortV1 {
    pub name: String,
    pub protocol: String,
    /// The port INSIDE the guest. The host port is the Runner's business and
    /// is never decided here.
    pub guest_port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReadinessContractV1 {
    Http { port_name: String, path: String },
    Tcp { port_name: String },
    Process,
}

/// A durable state namespace the schema declares. Declared, never inferred:
/// guessing that an app wants persistent state at a path is how an app ends up
/// writing to somewhere that is silently discarded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateSlotDeclarationV1 {
    pub state_key: String,
    /// Absolute guest path.
    pub mount_target: String,
    pub access: StateAccessDeclarationV1,
    pub protocol: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateAccessDeclarationV1 {
    ReadOnly,
    ReadWrite,
}

/// A named input the schema needs at launch. Carries NO value — a binding
/// requirement is a question, and the answer belongs to an instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BindingRequirementV1 {
    pub name: String,
    pub kind: BindingKindV1,
    pub required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingKindV1 {
    Secret,
    ExternalService,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormationProvenanceV1 {
    pub formation_service_version: String,
    pub builder_catalog_version: String,
    pub policy_version: String,
    /// The network policy that was ACTUALLY in force, recorded so a later
    /// reader can tell whether an artifact was built under isolation or not.
    pub network_policy: FormationNetworkPolicyV1,
    pub isolation: String,
    /// Which fields were authored, which inferred, which defaulted.
    #[serde(default)]
    pub field_origins: BTreeMap<String, RequirementResolutionV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormationDiagnosticV1 {
    pub severity: DiagnosticSeverityV1,
    pub code: String,
    /// Bounded and redacted before it gets here. A build log can contain a
    /// token the source itself printed.
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverityV1 {
    Info,
    Warning,
    Error,
}

// ────────────────────────────────────────────────────────────────── validation

fn is_content_address(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

fn is_full_commit_sha(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

/// A repository-relative path that cannot leave its repository.
fn is_contained_subdirectory(value: &str) -> bool {
    if value.is_empty() {
        return true;
    }
    if value.starts_with('/') || value.starts_with('\\') || value.contains('\0') {
        return false;
    }
    value
        .split('/')
        .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

/// Walk raw JSON for any forbidden key, at any depth.
///
/// Run BEFORE typed parsing. `deny_unknown_fields` catches an unexpected field
/// where a struct is expected, but a forbidden key inside a free-form map — an
/// override table, a compatibility map — would sail through, and those maps are
/// exactly where somebody would try to hide one.
pub fn reject_forbidden_fields(value: &serde_json::Value) -> Result<(), FormationError> {
    match value {
        serde_json::Value::Object(map) => {
            for (key, nested) in map {
                let normalized = key.to_ascii_lowercase();
                if FORBIDDEN_FORMATION_FIELDS
                    .iter()
                    .any(|forbidden| normalized == *forbidden)
                {
                    return Err(FormationError::Forbidden { field: key.clone() });
                }
                reject_forbidden_fields(nested)?;
            }
            Ok(())
        }
        serde_json::Value::Array(items) => {
            for item in items {
                reject_forbidden_fields(item)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn invalid(code: &'static str, detail: impl Into<String>) -> FormationError {
    FormationError::Invalid {
        code,
        detail: detail.into(),
    }
}

impl FormationTargetV1 {
    fn validate(&self) -> Result<(), FormationError> {
        if self.triple.trim().is_empty() {
            return Err(invalid(
                "ATO_ERR_FORMATION_TARGET_INVALID",
                "target triple is empty",
            ));
        }
        if !self.workspace_guest_root.starts_with('/')
            || self.workspace_guest_root.contains("/../")
            || self.workspace_guest_root.ends_with("/..")
        {
            return Err(invalid(
                "ATO_ERR_FORMATION_TARGET_INVALID",
                format!(
                    "workspace_guest_root {:?} must be an absolute, traversal-free guest path",
                    self.workspace_guest_root
                ),
            ));
        }
        Ok(())
    }
}

impl FormationJobV1 {
    /// Every invariant a Formation worker is entitled to assume.
    ///
    /// Run at BOTH ends: the control plane must not emit a job it knows is
    /// invalid, and the worker must not trust that it didn't.
    pub fn validate(&self) -> Result<(), FormationError> {
        if self.protocol != FORMATION_JOB_V1_PROTOCOL {
            return Err(FormationError::UnsupportedVersion {
                found: self.protocol.clone(),
                expected: FORMATION_JOB_V1_PROTOCOL,
            });
        }
        if self.job_id.trim().is_empty() {
            return Err(invalid("ATO_ERR_FORMATION_JOB_INVALID", "job_id is empty"));
        }
        if self.request_idempotency_key.len() < 8 {
            return Err(invalid(
                "ATO_ERR_FORMATION_JOB_INVALID",
                "request_idempotency_key is too short to deduplicate anything",
            ));
        }
        if self.requested_outputs.is_empty() {
            return Err(invalid(
                "ATO_ERR_FORMATION_JOB_INVALID",
                "a job that requests no output has nothing to produce",
            ));
        }
        if self
            .requested_outputs
            .contains(&RequestedOutputV1::OciImage)
        {
            // Reserved in the vocabulary, refused in this build. Accepting it
            // and producing nothing would be worse than saying so.
            return Err(invalid(
                "ATO_ERR_FORMATION_OUTPUT_UNSUPPORTED",
                "oci_image is reserved but not produced by this build (P5)",
            ));
        }

        match &self.source {
            FormationSourceV1::GitHub(source) => {
                if source.owner.trim().is_empty() || source.repository.trim().is_empty() {
                    return Err(invalid(
                        "ATO_ERR_FORMATION_SOURCE_INVALID",
                        "GitHub source needs an owner and a repository",
                    ));
                }
                if !is_full_commit_sha(&source.resolved_commit_sha) {
                    // A short SHA is ambiguous, and ambiguity in an identity is
                    // a collision waiting to be found.
                    return Err(invalid(
                        "ATO_ERR_FORMATION_SOURCE_NOT_PINNED",
                        format!(
                            "resolved_commit_sha {:?} is not a full commit id",
                            source.resolved_commit_sha
                        ),
                    ));
                }
                if !is_contained_subdirectory(&source.subdirectory) {
                    return Err(invalid(
                        "ATO_ERR_FORMATION_SOURCE_INVALID",
                        format!(
                            "subdirectory {:?} escapes the repository",
                            source.subdirectory
                        ),
                    ));
                }
            }
            FormationSourceV1::UploadedArchive(source) => {
                if !is_content_address(&source.expected_archive_digest) {
                    return Err(invalid(
                        "ATO_ERR_FORMATION_SOURCE_INVALID",
                        "expected_archive_digest is not a content address",
                    ));
                }
                if let Some(tree) = &source.expected_source_tree_digest
                    && !is_content_address(tree)
                {
                    return Err(invalid(
                        "ATO_ERR_FORMATION_SOURCE_INVALID",
                        "expected_source_tree_digest is not a content address",
                    ));
                }
                if !is_contained_subdirectory(&source.subdirectory) {
                    return Err(invalid(
                        "ATO_ERR_FORMATION_SOURCE_INVALID",
                        format!("subdirectory {:?} escapes the archive", source.subdirectory),
                    ));
                }
            }
            FormationSourceV1::ExistingSourceClosure(source) => {
                if !is_content_address(&source.source_closure_ref) {
                    return Err(invalid(
                        "ATO_ERR_FORMATION_SOURCE_INVALID",
                        "source_closure_ref is not a content address",
                    ));
                }
            }
        }

        self.target.validate()?;

        if self.policy.policy_version.trim().is_empty() {
            return Err(invalid(
                "ATO_ERR_FORMATION_POLICY_INVALID",
                "policy_version is empty; the rules a build ran under are part of its identity",
            ));
        }
        // ADR-018. A build that needs the network cannot currently be confined
        // for an untrusted source, so the two settings together are refused
        // rather than quietly allowed.
        if self.policy.publish_enabled
            && self.policy.network == FormationNetworkPolicyV1::DependencyResolution
        {
            return Err(invalid(
                "ATO_ERR_FORMATION_POLICY_UNSAFE",
                "publish_enabled with dependency_resolution network is refused: this build cannot \
                 confine a networked untrusted source (ADR-018)",
            ));
        }
        Ok(())
    }

    /// Parse, refusing forbidden fields before anything is typed.
    pub fn parse(raw: &str) -> Result<Self, FormationError> {
        let value: serde_json::Value = serde_json::from_str(raw)
            .map_err(|error| FormationError::Malformed(error.to_string()))?;
        reject_forbidden_fields(&value)?;
        let job: Self = serde_json::from_value(value)
            .map_err(|error| FormationError::Malformed(error.to_string()))?;
        job.validate()?;
        Ok(job)
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, FormationError> {
        self.validate()?;
        serde_jcs::to_vec(self).map_err(|error| FormationError::Malformed(error.to_string()))
    }

    pub fn canonical_digest(&self) -> Result<String, FormationError> {
        Ok(sha256_ref(&self.canonical_bytes()?))
    }
}

impl FormationResultV1 {
    pub fn validate(&self) -> Result<(), FormationError> {
        if self.protocol != FORMATION_RESULT_V1_PROTOCOL {
            return Err(FormationError::UnsupportedVersion {
                found: self.protocol.clone(),
                expected: FORMATION_RESULT_V1_PROTOCOL,
            });
        }
        for (label, value) in [
            ("formation_key", &self.formation_key),
            ("source_closure_ref", &self.source_closure_ref),
            ("program_intent_ref", &self.program_intent_ref),
            ("effective_build_plan_ref", &self.effective_build_plan_ref),
            ("compute_schema_ref", &self.compute_schema_ref),
            (
                "deterministic_inputs_digest",
                &self.deterministic_inputs_digest,
            ),
        ] {
            if !is_content_address(value) {
                return Err(invalid(
                    "ATO_ERR_FORMATION_RESULT_INVALID",
                    format!("{label} is not a content address"),
                ));
            }
        }
        if let Some(root) = &self.root_computation_ref
            && root.trim().is_empty()
        {
            return Err(invalid(
                "ATO_ERR_FORMATION_RESULT_INVALID",
                "root_computation_ref is present but empty; omit it instead",
            ));
        }

        if self.status == FormationStatusV1::Succeeded && self.materializations.is_empty() {
            return Err(invalid(
                "ATO_ERR_FORMATION_RESULT_INVALID",
                "a succeeded Formation produced no materialization",
            ));
        }
        for materialization in &self.materializations {
            if !is_content_address(&materialization.content_ref)
                || !is_content_address(&materialization.digest)
            {
                return Err(invalid(
                    "ATO_ERR_FORMATION_RESULT_INVALID",
                    "materialization is not content-addressed",
                ));
            }
            materialization.target.validate()?;
        }

        // Every candidate must name a materialization this result actually
        // produced. A candidate pointing elsewhere would have the control plane
        // register a schema whose artifact nothing here vouches for.
        let produced: Vec<&str> = self
            .materializations
            .iter()
            .map(|materialization| materialization.content_ref.as_str())
            .collect();
        for candidate in &self.realization_candidates {
            let named = match candidate {
                RealizationCandidateV1::Process(process) => &process.workspace_materialization_ref,
                RealizationCandidateV1::StaticBrowser(static_web) => {
                    &static_web.materialization_ref
                }
            };
            if !produced.contains(&named.as_str()) {
                return Err(invalid(
                    "ATO_ERR_FORMATION_RESULT_INVALID",
                    format!(
                        "realization candidate names {named:?}, which this result did not produce"
                    ),
                ));
            }
        }

        // Readiness must name a port that exists, or a Runner has nothing to
        // probe and the Run hangs until its timeout.
        for readiness in &self.readiness_contracts {
            let port_name = match readiness {
                ReadinessContractV1::Http { port_name, .. }
                | ReadinessContractV1::Tcp { port_name } => Some(port_name),
                ReadinessContractV1::Process => None,
            };
            if let Some(name) = port_name
                && !self.exported_ports.iter().any(|port| &port.name == name)
            {
                return Err(invalid(
                    "ATO_ERR_FORMATION_RESULT_INVALID",
                    format!("readiness names port {name:?}, which is not exported"),
                ));
            }
        }

        for slot in &self.state_slot_declarations {
            if !slot.mount_target.starts_with('/')
                || slot.mount_target.contains("/../")
                || slot.mount_target.ends_with("/..")
            {
                return Err(invalid(
                    "ATO_ERR_FORMATION_RESULT_INVALID",
                    format!(
                        "state slot {:?} has mount target {:?}, which is not an absolute, \
                         traversal-free guest path",
                        slot.state_key, slot.mount_target
                    ),
                ));
            }
        }
        Ok(())
    }

    pub fn parse(raw: &str) -> Result<Self, FormationError> {
        let value: serde_json::Value = serde_json::from_str(raw)
            .map_err(|error| FormationError::Malformed(error.to_string()))?;
        reject_forbidden_fields(&value)?;
        let result: Self = serde_json::from_value(value)
            .map_err(|error| FormationError::Malformed(error.to_string()))?;
        result.validate()?;
        Ok(result)
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, FormationError> {
        self.validate()?;
        serde_jcs::to_vec(self).map_err(|error| FormationError::Malformed(error.to_string()))
    }

    pub fn canonical_digest(&self) -> Result<String, FormationError> {
        Ok(sha256_ref(&self.canonical_bytes()?))
    }
}

fn sha256_ref(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("sha256:{:x}", Sha256::digest(bytes))
}
