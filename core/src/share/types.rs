//! Share specification, lock, and runtime state types.
//!
//! These types define the wire format for `share.spec.json`, `share.lock.json`,
//! and `state.json` used by `ato encap`, `ato decap`, `ato run <share>`, and
//! ato-desktop's share URL execution path.

use serde::{Deserialize, Serialize};

fn default_git_mode_str() -> String {
    "same-commit".to_string()
}

fn default_runtime_source_str() -> String {
    "system".to_string()
}

// ── Specification types (share.spec.json) ────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareSpec {
    pub schema_version: String,
    pub name: String,
    pub root: String,
    #[serde(default)]
    pub sources: Vec<ShareSourceSpec>,
    #[serde(default)]
    pub tool_requirements: Vec<ToolRequirementSpec>,
    #[serde(default)]
    pub env_requirements: Vec<EnvRequirementSpec>,
    #[serde(default)]
    pub install_steps: Vec<InstallStepSpec>,
    #[serde(default)]
    pub entries: Vec<ShareEntrySpec>,
    #[serde(default)]
    pub services: Vec<ServiceSpec>,
    #[serde(default)]
    pub notes: ShareNotes,
    pub generated_from: GeneratedFrom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareSourceSpec {
    pub id: String,
    pub kind: String,
    pub url: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default)]
    pub evidence: Vec<String>,
    /// "same-commit" | "latest-at-encap" | "archive"
    #[serde(default = "default_git_mode_str")]
    pub git_mode: String,
    /// Base64-encoded gzip tar of the directory — present only when kind = "archive".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolRequirementSpec {
    pub id: String,
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default)]
    pub required_by: Vec<String>,
    #[serde(default)]
    pub evidence: Vec<String>,
    /// "auto" | "ato" | "system"  (v2; defaults to "system" for v1 lock reads)
    #[serde(default = "default_runtime_source_str")]
    pub runtime_source: String,
    /// "uv" | "npm" | "bun" | "pnpm"  (populated when runtime_source != "system")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_toolchain: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvRequirementSpec {
    pub id: String,
    pub path: String,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default)]
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallStepSpec {
    pub id: String,
    pub cwd: String,
    pub run: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareEntrySpec {
    pub id: String,
    pub label: String,
    pub cwd: String,
    pub run: String,
    pub kind: String,
    pub primary: bool,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub env: EntryEnvSpec,
    #[serde(default)]
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EntryEnvSpec {
    #[serde(default)]
    pub required: Vec<String>,
    #[serde(default)]
    pub optional: Vec<String>,
    #[serde(default)]
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceSpec {
    pub id: String,
    pub cwd: String,
    pub run: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    pub kind: String,
    pub optional: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub healthcheck: Option<String>,
    #[serde(default)]
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ShareNotes {
    #[serde(default)]
    pub team_notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedFrom {
    pub root_path: String,
    pub captured_at: String,
    pub host_os: String,
}

// ── Lock types (share.lock.json) ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareLock {
    pub schema_version: String,
    pub spec_digest: String,
    pub generated_guide_digest: String,
    pub revision: u32,
    pub created_at: String,
    pub resolved_sources: Vec<ResolvedSourceLock>,
    pub resolved_tools: Vec<ResolvedToolLock>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedSourceLock {
    pub id: String,
    pub rev: String,
    /// "same-commit" | "latest-at-encap"  (v2)
    #[serde(default = "default_git_mode_str")]
    pub git_mode: String,
    /// Remote branch used when git_mode is "latest-at-encap"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_branch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedToolLock {
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_version: Option<String>,
    /// Kept for backward-compat with v1; not used by v2 decap logic
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary_path: Option<String>,
    /// "auto" | "ato" | "system"  (v2)
    #[serde(default = "default_runtime_source_str")]
    pub runtime_source: String,
    /// Provider toolchain used at encap time, e.g. "uv", "npm"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_toolchain: Option<String>,
    /// Version of the ato-managed runtime recorded at encap time
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_version: Option<String>,
}

// ── Runtime state types (state.json) ─────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceShareState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub share_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_revision_url: Option<String>,
    pub workspace_root: String,
    #[serde(default)]
    pub sources: Vec<ShareSourceState>,
    #[serde(default)]
    pub install_steps: Vec<InstallStepState>,
    #[serde(default)]
    pub env: Vec<EnvState>,
    pub verification: VerificationState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_verified_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareSourceState {
    pub id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_rev: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallStepState {
    pub id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvState {
    pub id: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationState {
    pub result: String,
    #[serde(default)]
    pub issues: Vec<String>,
}

// ── Loaded share input ───────────────────────────────────────────────────────

/// A fully loaded share: spec + lock with optional URL metadata.
/// Produced by `load_share_input()` from either a remote URL or local files.
#[derive(Debug, Clone)]
pub struct LoadedShareInput {
    pub spec: ShareSpec,
    pub lock: ShareLock,
    pub spec_digest_verified: bool,
    pub share_url: Option<String>,
    pub resolved_revision_url: Option<String>,
}

// ── Constants ────────────────────────────────────────────────────────────────

pub const SHARE_DIR: &str = ".ato/share";
pub const SHARE_SPEC_FILE: &str = "share.spec.json";
pub const SHARE_LOCK_FILE: &str = "share.lock.json";
pub const SHARE_STATE_FILE: &str = "state.json";
pub const SHARE_SCHEMA_VERSION: &str = "2";
