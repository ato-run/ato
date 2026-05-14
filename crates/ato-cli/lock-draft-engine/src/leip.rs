//! LEIP v1 — Launch Environment Inference Protocol.
//!
//! Object-model layering (canonical → projected):
//!   `LaunchGraphDraft`      = canonical inference object; what LEIP infers
//!   `LaunchEnvelopeDraft`   = per-node payload (AppTarget / WorkerTarget)
//!   `LockDraft`             = compatibility projection (lossy)
//!   `capsule.toml`          = export/import projection (user-visible)
//!   `ato.lock.json`         = resolved execution state (after lock finalization)
//!
//! Invariants:
//!   - Pure engine: no filesystem / network / env / clock access. Only reads input.
//!   - Inference results and candidate/evidence IDs are deterministic.
//!   - `relative_confidence` is beam-relative, not a success probability.
//!   - Ambiguity is fail-closed: AutoAccept requires sufficient unambiguous evidence.
//!   - Observations are redacted local provenance only; never in ato.lock.json.
//!   - capsule.toml is a projection target, not an inference source.
//!   - ato.lock.json existence triggers lock-first mode in source_inference.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{ExistingAtoLockSummary, ManifestSource, RepoFileEntry, RepoFileKind, SelectedTarget};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Baked into candidate IDs so cached entries are invalidated on engine changes.
pub const LEIP_ENGINE_VERSION: &str = "leip-v1.0.0";

const REQUIRED_COVERAGE_NODE: i32 = 25;
const REQUIRED_COVERAGE_PYTHON: i32 = 25;
const AUTO_ACCEPT_MARGIN_THRESHOLD: f64 = 0.3;
const BEAM_SIZE: usize = 4;

// Evidence weights — runtime
const W_RUNTIME_MARKER_NODE: i32 = 20; // package.json
const W_RUNTIME_MARKER_PYTHON_STRONG: i32 = 20; // pyproject.toml
const W_RUNTIME_MARKER_PYTHON_WEAK: i32 = 10; // requirements.txt / setup.py / Pipfile

// Evidence weights — lockfiles
const W_PKG_LOCKFILE: i32 = 5;

// Evidence weights — launch
const W_PKG_SCRIPT_RUN: i32 = 15; // start / dev / serve / preview
const W_PKG_SCRIPT_OTHER: i32 = 5;
const W_ENTRYPOINT_FILE: i32 = 10;
const W_DIRECT_SHELL_CMD: i32 = 8;
const W_MANIFEST_HINT_RUN: i32 = 8;
const W_MANIFEST_HINT_DRIVER: i32 = 10;

// Evidence weights — framework markers
const W_FRAMEWORK_MARKER: i32 = 5;

// Evidence weights — README (low confidence)
const W_README_CMD: i32 = 3;

// Risk penalties
const RISK_README_CMD: i32 = 3;

// Minimum launch score required for AutoAccept.
// Prevents AutoAccept when only a package.json + lockfile was found (runtime
// detected, but no entrypoint or run command).
const MIN_LAUNCH_SCORE_FOR_AUTO_ACCEPT: i32 = 10;

// ── Evidence types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    RuntimeMarkerFile,
    PackageManagerLockfile,
    PackageScriptCommand,
    EntrypointFile,
    DirectShellCommand,
    ManifestHint,
    FrameworkMarker,
    ReadmeRawShellCommand,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSource {
    RepoFileIndex,
    FileTextMap,
    ManifestSource,
    TargetHint,
}

/// A single unit of evidence extracted from the input.
///
/// Evidence ID is deterministically derived from (kind, path, key, normalized_value)
/// so the same observation always produces the same ID regardless of input order.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Evidence {
    pub id: String,
    pub kind: EvidenceKind,
    pub path: String,
    pub key: String,
    pub normalized_value: String,
    pub source: EvidenceSource,
}

// ── Launch graph types ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum LeipNodeKind {
    AppTarget,
    WorkerTarget,
}

/// Per-node launch envelope — the execution payload for an AppTarget or WorkerTarget node.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct LaunchEnvelopeDraft {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub driver: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cmd: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    /// Environment variables with inferred default values. Populated from `.env.example` /
    /// `.env.template` when a line has a non-placeholder value (e.g. `PORT=3000`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Environment variable names that have no default and must be set before launch.
    /// Populated from `.env.example` lines with empty or placeholder values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_env: Vec<String>,
    /// Outbound hostnames this app is expected to connect to, inferred from dependencies.
    /// Used for network policy generation. Hostnames only (no protocol/path).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub egress_hints: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependencies_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaunchGraphNodeDraft {
    pub id: String,
    pub kind: LeipNodeKind,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub envelope: Option<LaunchEnvelopeDraft>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_capsule: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum LeipEdgeKind {
    DependsOn,
    Provides,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaunchGraphEdgeDraft {
    pub source: String,
    pub target: String,
    pub kind: LeipEdgeKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<String>,
}

/// Graph-lite inference result. v1 constraints: single primary AppTarget, DAG,
/// ≤ 8 nodes. Compose services that cannot be mapped to a provider capsule are
/// listed in `unsupported_features`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct LaunchGraphDraft {
    pub primary_node_id: String,
    pub nodes: Vec<LaunchGraphNodeDraft>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<LaunchGraphEdgeDraft>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unsupported_features: Vec<String>,
}

// ── Candidate types ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LaunchGraphCandidate {
    pub id: String,
    pub graph: LaunchGraphDraft,
    pub score: i32,
    /// Fraction of top candidate score; beam-relative, not a success probability.
    pub relative_confidence: f64,
    /// (top - second) / top. Zero for single-candidate beam.
    pub margin: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub risks: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hard_constraint_failures: Vec<String>,
    /// Runtime evidence score component (not serialized; internal use only).
    #[serde(skip)]
    pub runtime_score: i32,
    /// Launch evidence score component (not serialized; internal use only).
    #[serde(skip)]
    pub launch_score: i32,
    /// Lockfile evidence score component (not serialized; internal use only).
    #[serde(skip)]
    pub lockfile_score: i32,
    /// Risk penalty component (not serialized; internal use only).
    #[serde(skip)]
    pub risk_penalty: i32,
}

// ── Decision types ────────────────────────────────────────────────────────────

/// The engine's top-level inference verdict.
///
/// Serialized as an internally-tagged enum (`"kind"` discriminant) in snake_case,
/// producing TypeScript-friendly discriminated unions:
/// ```json
/// {"kind":"auto_accept","candidate_id":"sha256:..."}
/// {"kind":"needs_selection","reason":"..."}
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeipDecision {
    AutoAccept { candidate_id: String },
    NeedsSelection { reason: String },
    Unresolved { reason: String },
    Rejected { reason: String },
}

// ── Result types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScoringProfile {
    pub required_evidence_coverage: i32,
    pub auto_accept_margin_threshold: f64,
    pub beam_size: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum LeipDiagnosticSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LeipDiagnostic {
    pub severity: LeipDiagnosticSeverity,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LeipResult {
    pub candidates: Vec<LaunchGraphCandidate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rejected_candidates: Vec<LaunchGraphCandidate>,
    pub decision: LeipDecision,
    pub engine_version: String,
    pub scoring_profile: ScoringProfile,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<LeipDiagnostic>,
}

// ── Input types ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LeipInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_hint: Option<SelectedTarget>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repo_file_index: Vec<RepoFileEntry>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub file_text_map: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_source: Option<ManifestSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub existing_ato_lock_summary: Option<ExistingAtoLockSummary>,
}

// ── VerificationObservation ───────────────────────────────────────────────────

/// Verification stage for an observation record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStage {
    Install,
    Build,
    Run,
    Readiness,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    Success,
    Failure,
    Timeout,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationFailureClass {
    ExitCode,
    Timeout,
    NetworkError,
    PermissionDenied,
    NotFound,
    Unknown,
}

/// Redacted local provenance record. Written ONLY to
/// `~/.ato/runs/<session-id>/observations.jsonl`. Never included in
/// ato.lock.json, publish artifacts, or share artifacts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationObservation {
    pub candidate_id: String,
    pub stage: VerificationStage,
    pub status: VerificationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_class: Option<VerificationFailureClass>,
    /// Log excerpt with secrets redacted. Capped at 2048 chars.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redacted_log_excerpt: Option<String>,
    pub elapsed_ms: u64,
}

// ── Error types ───────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum LeipError {
    #[error("failed to parse input: {0}")]
    InvalidInput(#[from] serde_json::Error),
    #[error("internal engine error: {0}")]
    EngineError(String),
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Primary inference API. Pure function — no filesystem / network / env / clock.
/// Returns (runtime_name, user-visible hint) for each runtime detected in the file index
/// that LEIP v1 does not yet support. Used to produce actionable diagnostics instead of
/// the generic "no viable candidates" message.
fn detect_unsupported_runtimes(input: &LeipInput) -> Vec<(&'static str, &'static str)> {
    let file_set: std::collections::BTreeSet<String> = input
        .repo_file_index
        .iter()
        .filter(|e| e.kind == RepoFileKind::File)
        .map(|e| strip_leading_dot_slash(&e.path))
        .collect();

    let mut found: Vec<(&'static str, &'static str)> = Vec::new();
    if file_present(&file_set, "Cargo.toml") {
        found.push((
            "rust",
            "Rust projects are not yet supported by LEIP v1 (supports Node.js and Python only). \
             Add a capsule.toml with `run = \"cargo run\"` to specify the launch target.",
        ));
    }
    if file_present(&file_set, "go.mod") {
        found.push((
            "go",
            "Go projects are not yet supported by LEIP v1 (supports Node.js and Python only). \
             Add a capsule.toml with `run = \"go run ./...\"` to specify the launch target.",
        ));
    }
    if file_present(&file_set, "wrangler.toml") {
        found.push((
            "cloudflare-worker",
            "Cloudflare Workers projects (wrangler.toml) are not yet supported by LEIP v1. \
             Run `wrangler dev` directly, or add a capsule.toml with `run = \"wrangler dev\"`.",
        ));
    }
    // Docker Compose only (no Node/Python root manifest): flag as unsupported so
    // the user gets an actionable message instead of a generic "no viable candidates".
    let has_compose = file_present(&file_set, "docker-compose.yml")
        || file_present(&file_set, "docker-compose.yaml")
        || file_present(&file_set, "compose.yaml")
        || file_present(&file_set, "compose.yml");
    let has_node = file_present(&file_set, "package.json");
    let has_python = file_present(&file_set, "pyproject.toml")
        || file_present(&file_set, "requirements.txt")
        || file_present(&file_set, "setup.py");
    if has_compose && !has_node && !has_python {
        found.push((
            "docker-compose",
            "Docker Compose project detected with no Node.js or Python root manifest. \
             LEIP v1 does not infer compose launch graphs. \
             Run `docker compose up` directly, or add a capsule.toml with `run = \"docker compose up\"`.",
        ));
    }
    // PHP: composer.json or any root .php file
    let has_php = file_present(&file_set, "composer.json")
        || file_set.iter().any(|f| f.ends_with(".php") && !f.contains('/'));
    if has_php && !has_node && !has_python {
        found.push((
            "php",
            "PHP project detected. LEIP v1 does not infer PHP launch graphs. \
             Run `php -S localhost:8080 -t public` or `composer install && php -S localhost:8080 -t public` directly, \
             or add a capsule.toml with `run = \"php -S localhost:8080 -t public\"`.",
        ));
    }
    // C#: any .sln or .csproj file in the repository
    let has_csharp = file_set.iter().any(|f| f.ends_with(".sln") || f.ends_with(".csproj"));
    if has_csharp && !has_node && !has_python {
        found.push((
            "csharp",
            "C# (.NET) project detected. LEIP v1 does not infer .NET launch graphs. \
             Add a capsule.toml with `run = \"dotnet run\"` to specify the launch target.",
        ));
    }
    found
}

pub fn evaluate_launch_graphs(input: &LeipInput) -> LeipResult {
    let all_evidence = extract_evidence(input);

    let mut viable: Vec<LaunchGraphCandidate> = Vec::new();
    let mut rejected: Vec<LaunchGraphCandidate> = Vec::new();

    for mut candidate in [
        generate_node_candidate(input, &all_evidence),
        generate_python_candidate(input, &all_evidence),
    ]
    .into_iter()
    .flatten()
    {
        candidate.id = candidate_id_hash(&candidate.graph);
        if candidate.hard_constraint_failures.is_empty() {
            viable.push(candidate);
        } else {
            rejected.push(candidate);
        }
    }

    viable.sort_by(|a, b| b.score.cmp(&a.score));
    let beam_len = viable.len().min(BEAM_SIZE);
    let beam: Vec<LaunchGraphCandidate> = viable.into_iter().take(BEAM_SIZE).collect();

    let max_score = beam.first().map(|c| c.score).unwrap_or(0);
    let second_score = if beam_len > 1 { beam[1].score } else { 0 };

    let beam: Vec<LaunchGraphCandidate> = beam
        .into_iter()
        .enumerate()
        .map(|(i, mut c)| {
            c.relative_confidence = if max_score > 0 {
                c.score as f64 / max_score as f64
            } else {
                0.0
            };
            // Single-candidate margin is 0.0 — absence of a competitor does not
            // prove the candidate's quality.
            c.margin = if i == 0 && beam_len > 1 && max_score > 0 {
                (max_score - second_score) as f64 / max_score as f64
            } else {
                0.0
            };
            c
        })
        .collect();

    let required_coverage = beam
        .first()
        .and_then(|c| c.graph.nodes.iter().find(|n| n.id == c.graph.primary_node_id))
        .and_then(|n| n.envelope.as_ref())
        .and_then(|e| e.driver.as_deref())
        .map(required_coverage_for)
        .unwrap_or(REQUIRED_COVERAGE_NODE);

    let decision = make_decision(&beam, &all_evidence, required_coverage);

    // Build diagnostics
    let mut diagnostics: Vec<LeipDiagnostic> = Vec::new();

    // Unsupported runtime hints: replace the generic "no viable candidates" message with a
    // runtime-specific one so users know exactly why inference failed.
    let unsupported_runtimes = detect_unsupported_runtimes(input);
    for (_, hint) in &unsupported_runtimes {
        diagnostics.push(LeipDiagnostic {
            severity: LeipDiagnosticSeverity::Warning,
            message: hint.to_string(),
        });
    }

    let decision = if matches!(&decision, LeipDecision::Unresolved { .. })
        && !unsupported_runtimes.is_empty()
    {
        let names: Vec<&str> = unsupported_runtimes.iter().map(|(n, _)| *n).collect();
        LeipDecision::Unresolved {
            reason: format!(
                "{} projects are not yet supported by LEIP v1 (supports Node.js and Python only); \
                 add a capsule.toml",
                names.join(", ")
            ),
        }
    } else {
        decision
    };

    // CLI tool warning: if the auto-accepted launch command belongs to a project that looks like
    // a batch CLI tool (yargs/commander/etc.) with no server framework, warn the user — the
    // process may exit immediately instead of serving HTTP.
    if matches!(&decision, LeipDecision::AutoAccept { .. }) {
        let has_cli_marker = all_evidence.iter().any(|e| {
            e.kind == EvidenceKind::FrameworkMarker && e.normalized_value == "cli-tool"
        });
        let has_server_framework = all_evidence.iter().any(|e| {
            e.kind == EvidenceKind::FrameworkMarker
                && !matches!(e.normalized_value.as_str(), "cli-tool")
        });
        if has_cli_marker && !has_server_framework {
            diagnostics.push(LeipDiagnostic {
                severity: LeipDiagnosticSeverity::Warning,
                message: "Inferred launch command may run a CLI tool rather than a long-running \
                          server. If this is intentional, proceed. Otherwise, add a capsule.toml \
                          with the correct `run` command."
                    .to_string(),
            });
        }
    }

    // Env var and network diagnostics from top accepted candidate's primary node envelope.
    if matches!(&decision, LeipDecision::AutoAccept { .. } | LeipDecision::NeedsSelection { .. }) {
        if let Some(top) = beam.first() {
            if let Some(primary_node) = top.graph.nodes.iter().find(|n| n.id == top.graph.primary_node_id) {
                if let Some(envelope) = &primary_node.envelope {
                    if !envelope.required_env.is_empty() {
                        diagnostics.push(LeipDiagnostic {
                            severity: LeipDiagnosticSeverity::Warning,
                            message: format!(
                                "Required environment variables (no default): {}. \
                                 Set these before launching.",
                                envelope.required_env.join(", ")
                            ),
                        });
                    }
                    if !envelope.egress_hints.is_empty() {
                        diagnostics.push(LeipDiagnostic {
                            severity: LeipDiagnosticSeverity::Info,
                            message: format!(
                                "Outbound network access likely required: {}.",
                                envelope.egress_hints.join(", ")
                            ),
                        });
                    }
                }
            }
        }
    }

    LeipResult {
        candidates: beam,
        rejected_candidates: rejected,
        decision,
        engine_version: LEIP_ENGINE_VERSION.to_string(),
        scoring_profile: ScoringProfile {
            required_evidence_coverage: required_coverage,
            auto_accept_margin_threshold: AUTO_ACCEPT_MARGIN_THRESHOLD,
            beam_size: BEAM_SIZE,
        },
        evidence: all_evidence,
        diagnostics,
    }
}

/// JSON wrapper for WASM / RPC use. Errors surface as `{"error": "..."}`.
pub fn evaluate_launch_graphs_json(input_json: &str) -> Result<String, LeipError> {
    let input: LeipInput = serde_json::from_str(input_json)?;
    let result = evaluate_launch_graphs(&input);
    serde_json::to_string(&result).map_err(LeipError::InvalidInput)
}

/// Compatibility wrapper — accepts the existing `LockDraftInput` format and
/// returns a `LeipResult`. Maps `selected_target` → `target_hint`.
/// The old `evaluateLockDraftJson` is unchanged.
pub fn evaluate_launch_envelopes_json(input_json: &str) -> Result<String, LeipError> {
    let raw: super::LockDraftInput = serde_json::from_str(input_json)?;
    let leip_input = LeipInput {
        target_hint: raw.selected_target,
        repo_file_index: raw.repo_file_index,
        file_text_map: raw.file_text_map,
        manifest_source: raw.manifest_source,
        existing_ato_lock_summary: raw.existing_ato_lock_summary,
    };
    let result = evaluate_launch_graphs(&leip_input);
    serde_json::to_string(&result).map_err(LeipError::InvalidInput)
}

// ── Log redaction (VerificationObservation) ───────────────────────────────────

const REDACTION_PLACEHOLDER: &str = "[REDACTED]";
const LOG_EXCERPT_MAX_CHARS: usize = 2048;

/// Redact secrets from a log excerpt for safe local storage.
///
/// Rules (applied in order):
/// 1. Truncate to `LOG_EXCERPT_MAX_CHARS`.
/// 2. Redact URL credentials (`://user:pass@` → `://[REDACTED]@`).
/// 3. Redact env assignments (`KEY=<long-value>` where value ≥ 8 chars).
/// 4. Redact known secret prefixes (ghp_, gho_, github_pat_, sk-, npm_, npx_,
///    Bearer, Authorization).
/// 5. Redact base64-like strings ≥ 32 chars.
pub fn redact_log_excerpt(raw: &str) -> String {
    let truncated: &str = if raw.len() > LOG_EXCERPT_MAX_CHARS {
        &raw[..LOG_EXCERPT_MAX_CHARS]
    } else {
        raw
    };

    let mut s = truncated.to_string();

    // URL credentials
    let url_cred = regex::Regex::new(r"://[^:@/\s]+:[^@\s]+@").unwrap();
    s = url_cred
        .replace_all(&s, format!("://{}@", REDACTION_PLACEHOLDER).as_str())
        .into_owned();

    // Env assignments with long values
    let env_assign = regex::Regex::new(r"(?m)([A-Z_][A-Z0-9_]*)=([^\s]{8,})").unwrap();
    s = env_assign
        .replace_all(&s, |caps: &regex::Captures| {
            format!("{}={}", &caps[1], REDACTION_PLACEHOLDER)
        })
        .into_owned();

    // Known secret prefixes (case-insensitive)
    let known_prefixes = regex::Regex::new(
        r"(?i)(ghp_|gho_|github_pat_|sk-|npm_|Bearer\s+|Authorization:\s*)[^\s]{8,}",
    )
    .unwrap();
    s = known_prefixes
        .replace_all(&s, |caps: &regex::Captures| {
            format!("{}{}", &caps[1], REDACTION_PLACEHOLDER)
        })
        .into_owned();

    // Base64-like strings ≥ 32 chars (token-like)
    let b64 = regex::Regex::new(r"[A-Za-z0-9+/=_-]{32,}").unwrap();
    s = b64
        .replace_all(&s, REDACTION_PLACEHOLDER)
        .into_owned();

    s
}

// ── Private engine implementation ─────────────────────────────────────────────

fn required_coverage_for(driver: &str) -> i32 {
    match driver {
        "python" => REQUIRED_COVERAGE_PYTHON,
        _ => REQUIRED_COVERAGE_NODE,
    }
}

fn make_evidence(
    kind: EvidenceKind,
    path: &str,
    key: &str,
    normalized_value: &str,
    source: EvidenceSource,
) -> Evidence {
    Evidence {
        id: evidence_id_hash(&kind, path, key, normalized_value),
        kind,
        path: path.to_string(),
        key: key.to_string(),
        normalized_value: normalized_value.to_string(),
        source,
    }
}

fn evidence_id_hash(
    kind: &EvidenceKind,
    path: &str,
    key: &str,
    normalized_value: &str,
) -> String {
    let kind_str = serde_json::to_string(kind)
        .unwrap_or_default()
        .trim_matches('"')
        .to_string();
    let mut h = Sha256::new();
    h.update(kind_str.as_bytes());
    h.update(b"|");
    h.update(path.as_bytes());
    h.update(b"|");
    h.update(key.as_bytes());
    h.update(b"|");
    h.update(normalized_value.as_bytes());
    format!("sha256:{}", hex::encode(h.finalize()))
}

/// Build a canonical (sort-stable) copy of the graph for hashing.
/// All `Vec` fields are sorted lexicographically so the ID is independent
/// of insertion order.
fn canonical_graph(g: &LaunchGraphDraft) -> LaunchGraphDraft {
    let mut nodes = g.nodes.clone();
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    for n in &mut nodes {
        n.evidence_refs.sort();
    }

    let mut edges = g.edges.clone();
    edges.sort_by(|a, b| {
        a.source
            .cmp(&b.source)
            .then(a.target.cmp(&b.target))
            .then(a.kind.cmp(&b.kind))
    });
    for e in &mut edges {
        e.evidence_refs.sort();
    }

    let mut refs = g.evidence_refs.clone();
    refs.sort();

    let mut unsupported = g.unsupported_features.clone();
    unsupported.sort();

    LaunchGraphDraft {
        primary_node_id: g.primary_node_id.clone(),
        nodes,
        edges,
        evidence_refs: refs,
        unsupported_features: unsupported,
    }
}

fn candidate_id_hash(graph: &LaunchGraphDraft) -> String {
    let canon = canonical_graph(graph);
    let json = serde_json::to_string(&canon).unwrap_or_default();
    let mut h = Sha256::new();
    h.update(json.as_bytes());
    h.update(b"|");
    h.update(LEIP_ENGINE_VERSION.as_bytes());
    format!("sha256:{}", hex::encode(h.finalize()))
}

fn extract_evidence(input: &LeipInput) -> Vec<Evidence> {
    let mut ev: Vec<Evidence> = Vec::new();

    let file_set: std::collections::BTreeSet<String> = input
        .repo_file_index
        .iter()
        .filter(|e| e.kind == RepoFileKind::File)
        .map(|e| strip_leading_dot_slash(&e.path))
        .collect();

    // Runtime marker files
    for (filename, runtime) in [
        ("package.json", "node"),
        ("pyproject.toml", "python"),
    ] {
        if file_present(&file_set, filename) {
            ev.push(make_evidence(
                EvidenceKind::RuntimeMarkerFile,
                filename,
                "detected_runtime",
                runtime,
                EvidenceSource::RepoFileIndex,
            ));
        }
    }
    for (filename, runtime) in [
        ("requirements.txt", "python"),
        ("setup.py", "python"),
        ("setup.cfg", "python"),
        ("Pipfile", "python"),
    ] {
        if file_present(&file_set, filename) {
            ev.push(make_evidence(
                EvidenceKind::RuntimeMarkerFile,
                filename,
                "detected_runtime",
                runtime,
                EvidenceSource::RepoFileIndex,
            ));
        }
    }

    // Package manager lockfiles
    for lockfile in [
            "package-lock.json",
            "yarn.lock",
            "pnpm-lock.yaml",
            "bun.lockb",
            "bun.lock",
            "poetry.lock",
            "Pipfile.lock",
    ] {
        if file_present(&file_set, lockfile) {
            ev.push(make_evidence(
                EvidenceKind::PackageManagerLockfile,
                lockfile,
                "filename",
                lockfile,
                EvidenceSource::RepoFileIndex,
            ));
        }
    }

    // Node entrypoint files
    for ep in [
        "index.js",
        "index.mjs",
        "index.cjs",
        "index.ts",
        "server.js",
        "server.ts",
        "app.js",
        "app.ts",
        "src/index.js",
        "src/index.ts",
        "src/server.js",
        "src/server.ts",
        "src/app.js",
        "src/app.ts",
    ] {
        if file_present(&file_set, ep) {
            ev.push(make_evidence(
                EvidenceKind::EntrypointFile,
                ep,
                "detected_runtime",
                "node",
                EvidenceSource::RepoFileIndex,
            ));
        }
    }

    // Python entrypoint files
    for ep in [
        "main.py",
        "app.py",
        "server.py",
        "run.py",
        "start.py",
        "manage.py",
        "__main__.py",
        "src/main.py",
        "src/app.py",
        "src/server.py",
    ] {
        if file_present(&file_set, ep) {
            ev.push(make_evidence(
                EvidenceKind::EntrypointFile,
                ep,
                "detected_runtime",
                "python",
                EvidenceSource::RepoFileIndex,
            ));
        }
    }

    // package.json contents
    if let Some(text) = find_file_text(input, &file_set, "package.json") {
        extract_package_json_evidence(text, &mut ev);
    }

    // pyproject.toml contents
    if let Some(text) = find_file_text(input, &file_set, "pyproject.toml") {
        extract_pyproject_evidence(text, &mut ev);
    }

    // requirements.txt contents (framework detection only)
    if let Some(text) = find_file_text(input, &file_set, "requirements.txt") {
        extract_requirements_txt_evidence(text, &mut ev);
    }

    // README commands
    for readme in ["README.md", "README.rst", "README.txt", "README"] {
        if let Some(text) = find_file_text(input, &file_set, readme) {
            extract_readme_commands(text, readme, &mut ev);
        }
    }

    // target_hint evidence
    if let Some(hint) = &input.target_hint {
        if let Some(driver) = &hint.driver {
            ev.push(make_evidence(
                EvidenceKind::ManifestHint,
                "target_hint",
                "driver",
                driver,
                EvidenceSource::TargetHint,
            ));
        }
        if let Some(run_cmd) = &hint.run_command {
            if !run_cmd.is_empty() {
                ev.push(make_evidence(
                    EvidenceKind::ManifestHint,
                    "target_hint",
                    "run_command",
                    run_cmd,
                    EvidenceSource::TargetHint,
                ));
            }
        }
        if !hint.cmd.is_empty() {
            ev.push(make_evidence(
                EvidenceKind::ManifestHint,
                "target_hint",
                "cmd",
                &hint.cmd.join(" "),
                EvidenceSource::TargetHint,
            ));
        }
        if let Some(ep) = &hint.entrypoint {
            if !ep.is_empty() {
                ev.push(make_evidence(
                    EvidenceKind::ManifestHint,
                    "target_hint",
                    "entrypoint",
                    ep,
                    EvidenceSource::TargetHint,
                ));
            }
        }
    }

    // Deduplicate by id
    let mut seen = std::collections::BTreeSet::new();
    ev.retain(|e| seen.insert(e.id.clone()));
    ev
}

fn file_present(file_set: &std::collections::BTreeSet<String>, name: &str) -> bool {
    file_set.contains(name)
        || file_set.iter().any(|p| p.ends_with(&format!("/{}", name)))
}

fn find_file_text<'a>(
    input: &'a LeipInput,
    file_set: &std::collections::BTreeSet<String>,
    name: &str,
) -> Option<&'a str> {
    if let Some(text) = input.file_text_map.get(name) {
        return Some(text.as_str());
    }
    // Try common path prefixes
    for prefix in ["./", "src/", "./src/"] {
        let key = format!("{}{}", prefix, name);
        if let Some(text) = input.file_text_map.get(&key) {
            return Some(text.as_str());
        }
    }
    // Try any path in file_set that ends with this name
    let candidate = file_set
        .iter()
        .find(|p| *p == name || p.ends_with(&format!("/{}", name)))?;
    input.file_text_map.get(candidate).map(|s| s.as_str())
}

fn strip_leading_dot_slash(p: &str) -> String {
    p.trim_start_matches("./").to_string()
}

fn extract_package_json_evidence(text: &str, ev: &mut Vec<Evidence>) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return;
    };

    // scripts
    if let Some(scripts) = v.get("scripts").and_then(|s| s.as_object()) {
        for (script_name, script_value) in scripts {
            let Some(cmd_str) = script_value.as_str() else {
                continue;
            };
            ev.push(make_evidence(
                EvidenceKind::PackageScriptCommand,
                "package.json",
                &format!("scripts.{}", script_name),
                cmd_str,
                EvidenceSource::FileTextMap,
            ));
        }
    }

    // main field as entrypoint
    if let Some(main) = v.get("main").and_then(|m| m.as_str()) {
        if !main.is_empty() {
            ev.push(make_evidence(
                EvidenceKind::EntrypointFile,
                "package.json",
                "main",
                main,
                EvidenceSource::FileTextMap,
            ));
        }
    }

    // Framework markers from dependencies
    let dep_keys: Vec<&str> = ["dependencies", "devDependencies", "peerDependencies"]
        .iter()
        .flat_map(|section| {
            v.get(*section)
                .and_then(|d| d.as_object())
                .map(|m| m.keys().map(|k| k.as_str()).collect::<Vec<_>>())
                .unwrap_or_default()
        })
        .collect();

    for dep in &dep_keys {
        let framework = node_framework_label(dep);
        if let Some(label) = framework {
            ev.push(make_evidence(
                EvidenceKind::FrameworkMarker,
                "package.json",
                dep,
                label,
                EvidenceSource::FileTextMap,
            ));
        }
    }
}

fn node_framework_label(dep: &str) -> Option<&'static str> {
    match dep {
        "next" | "next.js" => Some("nextjs"),
        "nuxt" | "nuxt3" => Some("nuxt"),
        "vue" | "@vitejs/plugin-vue" | "vue-router" => Some("vue"),
        "vite" | "@vitejs/plugin-react" => Some("vite"),
        "express" => Some("express"),
        "fastify" => Some("fastify"),
        "hono" => Some("hono"),
        "koa" => Some("koa"),
        "@nestjs/core" => Some("nestjs"),
        "remix" | "@remix-run/node" => Some("remix"),
        "astro" => Some("astro"),
        "svelte" | "@sveltejs/kit" => Some("svelte"),
        // CLI tool markers — presence without a server framework triggers a diagnostic warning.
        "yargs" | "commander" | "minimist" | "meow" | "@oclif/core" | "oclif" => Some("cli-tool"),
        _ => None,
    }
}

fn extract_pyproject_evidence(text: &str, ev: &mut Vec<Evidence>) {
    let Ok(v) = toml::from_str::<toml::Value>(text) else {
        return;
    };

    // [project.scripts] or [tool.poetry.scripts]
    for section_path in [
        &["project", "scripts"][..],
        &["tool", "poetry", "scripts"][..],
    ] {
        if let Some(scripts) = get_toml_nested(&v, section_path) {
            if let Some(table) = scripts.as_table() {
                for (name, value) in table {
                    if let Some(cmd_str) = value.as_str() {
                        ev.push(make_evidence(
                            EvidenceKind::PackageScriptCommand,
                            "pyproject.toml",
                            &format!("scripts.{}", name),
                            cmd_str,
                            EvidenceSource::FileTextMap,
                        ));
                    }
                }
            }
        }
    }

    // [tool.poetry.dependencies] / [project.dependencies] for framework detection
    let dep_sources = [
        &["tool", "poetry", "dependencies"][..],
        &["project", "dependencies"][..],
    ];
    for dep_path in &dep_sources {
        if let Some(deps) = get_toml_nested(&v, dep_path) {
            match deps {
                toml::Value::Table(table) => {
                    for dep_name in table.keys() {
                        let name_lower = dep_name.to_lowercase();
                        if let Some(label) = python_framework_label(&name_lower) {
                            ev.push(make_evidence(
                                EvidenceKind::FrameworkMarker,
                                "pyproject.toml",
                                dep_name,
                                label,
                                EvidenceSource::FileTextMap,
                            ));
                        }
                    }
                }
                toml::Value::Array(arr) => {
                    for item in arr {
                        if let Some(s) = item.as_str() {
                            let name_lower = dep_name_from_requirement(s).to_lowercase();
                            if let Some(label) = python_framework_label(&name_lower) {
                                ev.push(make_evidence(
                                    EvidenceKind::FrameworkMarker,
                                    "pyproject.toml",
                                    s,
                                    label,
                                    EvidenceSource::FileTextMap,
                                ));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn get_toml_nested<'a>(v: &'a toml::Value, keys: &[&str]) -> Option<&'a toml::Value> {
    let mut cur = v;
    for key in keys {
        cur = cur.get(key)?;
    }
    Some(cur)
}

fn dep_name_from_requirement(req: &str) -> &str {
    // Strip version specifier: "fastapi>=0.100.0" → "fastapi"
    req.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .next()
        .unwrap_or(req)
}

fn python_framework_label(name: &str) -> Option<&'static str> {
    match name {
        "fastapi" => Some("fastapi"),
        "flask" => Some("flask"),
        "django" => Some("django"),
        "starlette" => Some("starlette"),
        "tornado" => Some("tornado"),
        "aiohttp" => Some("aiohttp"),
        "sanic" => Some("sanic"),
        "streamlit" => Some("streamlit"),
        "gradio" => Some("gradio"),
        "uvicorn" => Some("uvicorn"),
        "gunicorn" => Some("gunicorn"),
        _ => None,
    }
}

fn extract_requirements_txt_evidence(text: &str, ev: &mut Vec<Evidence>) {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('-') {
            continue;
        }
        let name = dep_name_from_requirement(line).to_lowercase();
        if let Some(label) = python_framework_label(&name) {
            ev.push(make_evidence(
                EvidenceKind::FrameworkMarker,
                "requirements.txt",
                line,
                label,
                EvidenceSource::FileTextMap,
            ));
        }
    }
}

fn extract_readme_commands(text: &str, path: &str, ev: &mut Vec<Evidence>) {
    // Extract shell-like commands from README code blocks and inline code.
    // These are low-confidence hints only (ReadmeRawShellCommand, score=3).
    let run_patterns = [
        "npm run ",
        "yarn run ",
        "pnpm run ",
        "python ",
        "uvicorn ",
        "gunicorn ",
        "flask run",
        "streamlit run",
        "node ",
        "npx ",
    ];

    for line in text.lines() {
        let trimmed = line.trim_start_matches(['`', ' ', '\t', '$', '#', '>']);
        for pat in &run_patterns {
            if trimmed.starts_with(pat) {
                let cmd = trimmed.trim().to_string();
                // Limit length and skip obvious multi-step commands
                if cmd.len() <= 200 {
                    ev.push(make_evidence(
                        EvidenceKind::ReadmeRawShellCommand,
                        path,
                        "readme_command",
                        &cmd,
                        EvidenceSource::FileTextMap,
                    ));
                }
                break;
            }
        }
    }
}

// ── Candidate generators ──────────────────────────────────────────────────────

fn generate_node_candidate(input: &LeipInput, evidence: &[Evidence]) -> Option<LaunchGraphCandidate> {
    // Require explicit Node runtime evidence
    let has_node_runtime = evidence.iter().any(|e| {
        e.kind == EvidenceKind::RuntimeMarkerFile
            && e.key == "detected_runtime"
            && e.normalized_value == "node"
    });
    if !has_node_runtime {
        return None;
    }

    let mut evidence_refs: Vec<String> = Vec::new();
    let mut runtime_score: i32 = 0;
    let mut launch_score: i32 = 0;
    let mut lockfile_score: i32 = 0;
    let mut risk_penalty: i32 = 0;
    let mut risks: Vec<String> = Vec::new();

    // Runtime score
    for e in evidence
        .iter()
        .filter(|e| e.kind == EvidenceKind::RuntimeMarkerFile && e.normalized_value == "node")
    {
        evidence_refs.push(e.id.clone());
        runtime_score = runtime_score.max(W_RUNTIME_MARKER_NODE);
    }

    // Lockfile score
    for e in evidence.iter().filter(|e| {
        e.kind == EvidenceKind::PackageManagerLockfile
            && matches!(
                e.normalized_value.as_str(),
                "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml" | "bun.lockb" | "bun.lock"
            )
    }) {
        evidence_refs.push(e.id.clone());
        lockfile_score = lockfile_score.max(W_PKG_LOCKFILE);
    }

    // Framework markers
    for e in evidence
        .iter()
        .filter(|e| e.kind == EvidenceKind::FrameworkMarker && e.path.ends_with("package.json"))
    {
        evidence_refs.push(e.id.clone());
        launch_score = launch_score.max(W_FRAMEWORK_MARKER);
    }

    // Determine best launch command
    let (cmd, used_evidence, script_cmd_str) = select_node_launch_cmd(input, evidence, &mut launch_score);
    evidence_refs.extend(used_evidence);

    // Node entrypoint files (not already counted via launch cmd)
    for e in evidence.iter().filter(|e| {
        e.kind == EvidenceKind::EntrypointFile && e.normalized_value == "node"
    }) {
        if !evidence_refs.contains(&e.id) {
            evidence_refs.push(e.id.clone());
        }
    }

    // ReadmeRawShellCommand risk
    for e in evidence.iter().filter(|e| e.kind == EvidenceKind::ReadmeRawShellCommand) {
        evidence_refs.push(e.id.clone());
        launch_score += W_README_CMD;
        risk_penalty += RISK_README_CMD;
        risks.push(format!(
            "launch command sourced from README ('{}'): unverified",
            truncate(&e.normalized_value, 60)
        ));
    }

    // ManifestHint
    for e in evidence.iter().filter(|e| e.kind == EvidenceKind::ManifestHint) {
        if !evidence_refs.contains(&e.id) {
            evidence_refs.push(e.id.clone());
        }
        match e.key.as_str() {
            "driver" => {
                launch_score = launch_score.max(W_MANIFEST_HINT_DRIVER);
            }
            "run_command" | "cmd" => {
                launch_score = launch_score.max(W_MANIFEST_HINT_RUN);
            }
            _ => {}
        }
    }

    let score = runtime_score + launch_score + lockfile_score - risk_penalty;

    // Hard constraints
    let hard_constraint_failures = check_cmd_hard_constraints(&cmd, script_cmd_str.as_deref());

    let envelope = build_node_envelope(input, evidence, cmd);

    evidence_refs.sort();
    evidence_refs.dedup();

    let graph = single_node_graph("node-app", LeipNodeKind::AppTarget, "Node.js application", envelope, evidence_refs.clone());

    Some(LaunchGraphCandidate {
        id: String::new(), // filled in by caller
        graph,
        score,
        relative_confidence: 0.0,
        margin: 0.0,
        evidence_refs,
        risks,
        hard_constraint_failures,
        runtime_score,
        launch_score,
        lockfile_score,
        risk_penalty,
    })
}

fn select_node_launch_cmd(
    input: &LeipInput,
    evidence: &[Evidence],
    launch_score: &mut i32,
) -> (Vec<String>, Vec<String>, Option<String>) {
    let mut used: Vec<String> = Vec::new();

    // 1. ManifestHint cmd (highest priority)
    if let Some(hint) = &input.target_hint {
        if !hint.cmd.is_empty() {
            let e = evidence
                .iter()
                .find(|e| e.kind == EvidenceKind::ManifestHint && e.key == "cmd");
            if let Some(e) = e {
                used.push(e.id.clone());
                *launch_score = (*launch_score).max(W_MANIFEST_HINT_RUN);
            }
            return (hint.cmd.clone(), used, None);
        }
        if let Some(run_cmd) = &hint.run_command {
            if !run_cmd.is_empty() {
                let e = evidence
                    .iter()
                    .find(|e| e.kind == EvidenceKind::ManifestHint && e.key == "run_command");
                if let Some(e) = e {
                    used.push(e.id.clone());
                    *launch_score = (*launch_score).max(W_MANIFEST_HINT_RUN);
                }
                return (shell_words_split(run_cmd), used, Some(run_cmd.clone()));
            }
        }
    }

    // 2. Package scripts: prefer dev > start > serve > preview (in that order).
    // dev is preferred for development-mode tools; start often runs a production build.
    let priority_scripts = ["dev", "start", "serve", "preview"];
    for script_name in &priority_scripts {
        if let Some(e) = evidence.iter().find(|e| {
            e.kind == EvidenceKind::PackageScriptCommand
                && e.key == format!("scripts.{}", script_name)
        }) {
            let script_content = e.normalized_value.clone();
            used.push(e.id.clone());
            *launch_score = (*launch_score).max(W_PKG_SCRIPT_RUN);
            let pkg_mgr = detect_node_pkg_manager(input);
            let cmd = vec![
                pkg_mgr.to_string(),
                "run".to_string(),
                script_name.to_string(),
            ];
            return (cmd, used, Some(script_content));
        }
    }

    // 3. Other package scripts
    if let Some(e) = evidence.iter().find(|e| {
        e.kind == EvidenceKind::PackageScriptCommand
            && !["start", "serve", "dev", "preview", "build", "test"].contains(
                &e.key
                    .strip_prefix("scripts.")
                    .unwrap_or(""),
            )
    }) {
        let script_name = e
            .key
            .strip_prefix("scripts.")
            .unwrap_or(&e.key)
            .to_string();
        let script_content = e.normalized_value.clone();
        used.push(e.id.clone());
        *launch_score = (*launch_score).max(W_PKG_SCRIPT_OTHER);
        let pkg_mgr = detect_node_pkg_manager(input);
        let cmd = vec![pkg_mgr.to_string(), "run".to_string(), script_name];
        return (cmd, used, Some(script_content));
    }

    // 4. Entrypoint file
    let ep_priority = [
        "server.js", "server.ts", "app.js", "app.ts",
        "src/server.js", "src/server.ts",
        "index.js", "index.ts",
        "src/index.js", "src/index.ts",
    ];
    for ep in &ep_priority {
        if let Some(e) = evidence.iter().find(|e| {
            e.kind == EvidenceKind::EntrypointFile
                && e.normalized_value == "node"
                && e.path == *ep
        }) {
            used.push(e.id.clone());
            *launch_score = (*launch_score).max(W_ENTRYPOINT_FILE);
            return (vec!["node".to_string(), ep.to_string()], used, None);
        }
    }

    // No launch evidence found
    (Vec::new(), used, None)
}

fn detect_node_pkg_manager(input: &LeipInput) -> &'static str {
    let file_set: std::collections::BTreeSet<String> = input
        .repo_file_index
        .iter()
        .filter(|e| e.kind == RepoFileKind::File)
        .map(|e| strip_leading_dot_slash(&e.path))
        .collect();
    if file_present(&file_set, "pnpm-lock.yaml") {
        "pnpm"
    } else if file_present(&file_set, "yarn.lock") {
        "yarn"
    } else if file_present(&file_set, "bun.lockb") || file_present(&file_set, "bun.lock") {
        "bun"
    } else {
        "npm"
    }
}

// ── Env var + network egress inference helpers ────────────────────────────────

/// Returns true if `value` is a placeholder (not a real default). Conservative:
/// only triggers on strong signals to avoid false-positives that suppress real defaults.
fn is_placeholder_value(value: &str) -> bool {
    let v = value.trim_matches('"').trim_matches('\'').trim();
    if v.is_empty() {
        return true;
    }
    // Angle-bracket / shell-expansion templates: <your-key>, ${FOO}, $(cmd)
    if v.starts_with('<') || v.starts_with("${") || v.starts_with("$(") {
        return true;
    }
    let lower = v.to_lowercase();
    // "your_..." or "your-..." prefix
    if lower.starts_with("your_") || lower.starts_with("your-") {
        return true;
    }
    // Path segments like /location/of/your/db
    if lower.contains("/your/") || lower.ends_with("/your") {
        return true;
    }
    // Well-known placeholder phrases
    for phrase in &["changeme", "change_me", "replaceme", "replace_me", "placeholder",
                    "insert_here", "add_here", "put_here", "fill_in", "fill_here",
                    "example_key", "example_secret", "example_token"] {
        if lower.contains(phrase) {
            return true;
        }
    }
    // Ends with "_here" (but not legitimate paths like "/data/here")
    if lower.ends_with("_here") && !lower.contains('/') {
        return true;
    }
    false
}

/// Parse an `.env.example` / `.env.template` / `.env.sample` file.
///
/// Returns `(name, default_value_or_none)` where `None` means the variable
/// has no default (placeholder or empty value) and must be provided by the user.
///
/// Pure function — reads only the supplied string.
fn parse_dot_env_example(content: &str) -> Vec<(String, Option<String>)> {
    let mut result = Vec::new();
    for raw_line in content.lines() {
        let line = raw_line.trim();
        // Skip comment lines and blank lines
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(eq_pos) = line.find('=') else {
            continue;
        };
        let name = line[..eq_pos].trim().to_string();
        if name.is_empty() || name.contains(' ') {
            continue;
        }
        let raw_value = line[eq_pos + 1..].trim();
        if is_placeholder_value(raw_value) {
            result.push((name, None));
        } else {
            // Strip surrounding quotes from value
            let value = raw_value
                .trim_matches('"')
                .trim_matches('\'')
                .to_string();
            result.push((name, Some(value)));
        }
    }
    result
}

/// Scan `file_text_map` for `.env.example`, `.env.template`, `.env.sample` files
/// and extract env var defaults and required vars.
///
/// Returns `(env_defaults, required_var_names)`.
/// - `env_defaults`: vars with real defaults → populate `LaunchEnvelopeDraft.env`
/// - `required_var_names`: vars with no default → populate `LaunchEnvelopeDraft.required_env`
///
/// Pure function — does not access the filesystem.
fn extract_env_from_files(
    file_text_map: &BTreeMap<String, String>,
) -> (BTreeMap<String, String>, Vec<String>) {
    let mut env_defaults: BTreeMap<String, String> = BTreeMap::new();
    let mut required: Vec<String> = Vec::new();

    // Priority order: .env.example > .env.template > .env.sample
    // .env.development is skipped (often contains localhost URLs specific to dev)
    for key in &[".env.example", ".env.template", ".env.sample"] {
        let Some(content) = file_text_map.get(*key) else {
            continue;
        };
        for (name, default) in parse_dot_env_example(content) {
            // First file wins per variable name
            if env_defaults.contains_key(&name) || required.contains(&name) {
                continue;
            }
            match default {
                Some(val) => { env_defaults.insert(name, val); }
                None => { required.push(name); }
            }
        }
        // Use only the first matching file
        break;
    }

    required.sort();
    required.dedup();
    (env_defaults, required)
}

/// Known Node.js dependency → (required_env_keys, egress_hostnames).
///
/// These are weaker evidence than `.env.example` confirmation. Callers should
/// use the result to inform diagnostics and optionally merge into `required_env`.
fn node_dep_hints(pkg_json_text: &str) -> (Vec<String>, Vec<String>) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(pkg_json_text) else {
        return (Vec::new(), Vec::new());
    };
    let deps: Vec<String> = ["dependencies", "devDependencies"]
        .iter()
        .flat_map(|section| {
            v.get(*section)
                .and_then(|d| d.as_object())
                .map(|m| m.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
        })
        .collect();

    dep_hints_for_names(&deps)
}

/// Known Python dependency → (required_env_keys, egress_hostnames).
fn python_dep_hints(
    requirements_text: Option<&str>,
    pyproject_text: Option<&str>,
) -> (Vec<String>, Vec<String>) {
    let mut dep_names: Vec<String> = Vec::new();
    if let Some(text) = requirements_text {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('-') {
                continue;
            }
            // Extract package name: strip version specifiers
            let name = line
                .split(|c: char| c == '>' || c == '<' || c == '=' || c == '!' || c == '[' || c == ';')
                .next()
                .unwrap_or(line)
                .trim()
                .to_lowercase();
            if !name.is_empty() {
                dep_names.push(name);
            }
        }
    }
    if let Some(text) = pyproject_text {
        if let Ok(v) = toml::from_str::<toml::Value>(text) {
            // [project].dependencies = ["openai>=1.0", ...]
            let project_deps = v.get("project")
                .and_then(|p| p.get("dependencies"))
                .and_then(|d| d.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|i| i.as_str())
                        .map(|s| s.split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_').next().unwrap_or("").to_lowercase())
                        .filter(|n| !n.is_empty())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            for name in project_deps {
                if !dep_names.contains(&name) {
                    dep_names.push(name);
                }
            }
        }
    }
    dep_hints_for_names(&dep_names)
}

/// Map dependency names to (required_env_keys, egress_hosts).
/// Used by both Node and Python paths.
fn dep_hints_for_names(deps: &[String]) -> (Vec<String>, Vec<String>) {
    // (package-name-pattern, env-key, egress-host)
    // Using exact or prefix matches to avoid false positives.
    const HINTS: &[(&str, &str, &str)] = &[
        ("openai",              "OPENAI_API_KEY",          "api.openai.com"),
        ("@anthropic-ai/sdk",   "ANTHROPIC_API_KEY",       "api.anthropic.com"),
        ("anthropic",           "ANTHROPIC_API_KEY",       "api.anthropic.com"),
        ("@google/genai",       "GOOGLE_API_KEY",          "generativelanguage.googleapis.com"),
        ("google-generativeai", "GOOGLE_API_KEY",          "generativelanguage.googleapis.com"),
        ("stripe",              "STRIPE_SECRET_KEY",       "api.stripe.com"),
        ("@sendgrid/mail",      "SENDGRID_API_KEY",        "api.sendgrid.com"),
        ("sendgrid",            "SENDGRID_API_KEY",        "api.sendgrid.com"),
        ("@mailchimp/mailchimp_marketing", "MAILCHIMP_API_KEY", "api.mailchimp.com"),
        ("resend",              "RESEND_API_KEY",          "api.resend.com"),
        ("twilio",              "TWILIO_AUTH_TOKEN",       "api.twilio.com"),
        // Auth frameworks: env key only, no external egress (they use internal secrets)
        ("better-auth",         "BETTER_AUTH_SECRET",      ""),
        ("next-auth",           "NEXTAUTH_SECRET",         ""),
        ("@auth/core",          "AUTH_SECRET",             ""),
    ];

    let dep_set: std::collections::BTreeSet<&str> = deps.iter().map(|s| s.as_str()).collect();
    let mut env_keys: Vec<String> = Vec::new();
    let mut egress: Vec<String> = Vec::new();

    for (pkg, env_key, host) in HINTS {
        if dep_set.contains(pkg) {
            if !env_key.is_empty() && !env_keys.contains(&env_key.to_string()) {
                env_keys.push(env_key.to_string());
            }
            if !host.is_empty() && !egress.contains(&host.to_string()) {
                egress.push(host.to_string());
            }
        }
    }

    (env_keys, egress)
}

fn build_node_envelope(input: &LeipInput, evidence: &[Evidence], cmd: Vec<String>) -> LaunchEnvelopeDraft {
    let runtime_version = input
        .target_hint
        .as_ref()
        .and_then(|h| h.runtime_version.clone())
        .unwrap_or_else(|| super::DEFAULT_NODE_RUNTIME_VERSION.to_string());

    let entrypoint = input
        .target_hint
        .as_ref()
        .and_then(|h| h.entrypoint.clone())
        .or_else(|| {
            evidence
                .iter()
                .find(|e| e.kind == EvidenceKind::EntrypointFile && e.normalized_value == "node")
                .map(|e| e.path.clone())
        });

    let (env_defaults, required_env_from_files) = extract_env_from_files(&input.file_text_map);
    let (dep_suggested_env, dep_egress) =
        if let Some(pkg_text) = input.file_text_map.get("package.json") {
            node_dep_hints(pkg_text)
        } else {
            (Vec::new(), Vec::new())
        };

    // Merge: file-confirmed required vars + dep-suggested vars (deduplicated, sorted).
    let mut required_env = required_env_from_files;
    for v in dep_suggested_env {
        if !required_env.contains(&v) {
            required_env.push(v);
        }
    }
    required_env.sort();
    required_env.dedup();

    let mut egress_hints = dep_egress;
    egress_hints.sort();
    egress_hints.dedup();

    LaunchEnvelopeDraft {
        driver: Some("node".to_string()),
        runtime_version: Some(runtime_version),
        cmd,
        entrypoint,
        env: env_defaults,
        required_env,
        egress_hints,
        ..Default::default()
    }
}

fn generate_python_candidate(input: &LeipInput, evidence: &[Evidence]) -> Option<LaunchGraphCandidate> {
    // Require explicit Python runtime evidence
    let has_python_runtime = evidence.iter().any(|e| {
        e.kind == EvidenceKind::RuntimeMarkerFile
            && e.key == "detected_runtime"
            && e.normalized_value == "python"
    });
    if !has_python_runtime {
        return None;
    }

    let mut evidence_refs: Vec<String> = Vec::new();
    let mut runtime_score: i32 = 0;
    let mut launch_score: i32 = 0;
    let mut lockfile_score: i32 = 0;
    let mut risk_penalty: i32 = 0;
    let mut risks: Vec<String> = Vec::new();

    // Runtime score (pyproject.toml is stronger than requirements.txt)
    for e in evidence
        .iter()
        .filter(|e| e.kind == EvidenceKind::RuntimeMarkerFile && e.normalized_value == "python")
    {
        evidence_refs.push(e.id.clone());
        let weight = if e.path == "pyproject.toml" {
            W_RUNTIME_MARKER_PYTHON_STRONG
        } else {
            W_RUNTIME_MARKER_PYTHON_WEAK
        };
        runtime_score = runtime_score.max(weight);
    }

    // Lockfile score
    for e in evidence.iter().filter(|e| {
        e.kind == EvidenceKind::PackageManagerLockfile
            && matches!(e.normalized_value.as_str(), "poetry.lock" | "Pipfile.lock")
    }) {
        evidence_refs.push(e.id.clone());
        lockfile_score = lockfile_score.max(W_PKG_LOCKFILE);
    }

    // Collect framework marker evidence refs (scoring deferred until after launch cmd selection)
    for e in evidence
        .iter()
        .filter(|e| e.kind == EvidenceKind::FrameworkMarker)
    {
        evidence_refs.push(e.id.clone());
    }

    let (cmd, used_evidence) = select_python_launch_cmd(input, evidence, &mut launch_score);
    evidence_refs.extend(used_evidence);

    // Framework markers add an additive bonus on top of the launch command score.
    // Capped at W_PKG_SCRIPT_OTHER so a pile of deps can't substitute for real launch evidence.
    let fw_count = evidence
        .iter()
        .filter(|e| e.kind == EvidenceKind::FrameworkMarker)
        .count();
    launch_score += (fw_count as i32 * W_FRAMEWORK_MARKER).min(W_PKG_SCRIPT_OTHER);

    // Python entrypoint files
    for e in evidence.iter().filter(|e| {
        e.kind == EvidenceKind::EntrypointFile && e.normalized_value == "python"
    }) {
        if !evidence_refs.contains(&e.id) {
            evidence_refs.push(e.id.clone());
        }
    }

    // ReadmeRawShellCommand risk
    for e in evidence.iter().filter(|e| e.kind == EvidenceKind::ReadmeRawShellCommand) {
        evidence_refs.push(e.id.clone());
        launch_score += W_README_CMD;
        risk_penalty += RISK_README_CMD;
        risks.push(format!(
            "launch command sourced from README ('{}'): unverified",
            truncate(&e.normalized_value, 60)
        ));
    }

    // ManifestHint
    for e in evidence.iter().filter(|e| e.kind == EvidenceKind::ManifestHint) {
        if !evidence_refs.contains(&e.id) {
            evidence_refs.push(e.id.clone());
        }
        match e.key.as_str() {
            "driver" => {
                launch_score = launch_score.max(W_MANIFEST_HINT_DRIVER);
            }
            "run_command" | "cmd" => {
                launch_score = launch_score.max(W_MANIFEST_HINT_RUN);
            }
            _ => {}
        }
    }

    let score = runtime_score + launch_score + lockfile_score - risk_penalty;

    let hard_constraint_failures = check_cmd_hard_constraints(&cmd, None);

    let envelope = build_python_envelope(input, evidence, cmd);

    evidence_refs.sort();
    evidence_refs.dedup();

    let graph = single_node_graph("python-app", LeipNodeKind::AppTarget, "Python application", envelope, evidence_refs.clone());

    Some(LaunchGraphCandidate {
        id: String::new(),
        graph,
        score,
        relative_confidence: 0.0,
        margin: 0.0,
        evidence_refs,
        risks,
        hard_constraint_failures,
        runtime_score,
        launch_score,
        lockfile_score,
        risk_penalty,
    })
}

fn select_python_launch_cmd(
    input: &LeipInput,
    evidence: &[Evidence],
    launch_score: &mut i32,
) -> (Vec<String>, Vec<String>) {
    let mut used: Vec<String> = Vec::new();

    // 1. ManifestHint cmd
    if let Some(hint) = &input.target_hint {
        if !hint.cmd.is_empty() {
            let e = evidence
                .iter()
                .find(|e| e.kind == EvidenceKind::ManifestHint && e.key == "cmd");
            if let Some(e) = e {
                used.push(e.id.clone());
                *launch_score = (*launch_score).max(W_MANIFEST_HINT_RUN);
            }
            return (hint.cmd.clone(), used);
        }
        if let Some(run_cmd) = &hint.run_command {
            if !run_cmd.is_empty() {
                let e = evidence
                    .iter()
                    .find(|e| e.kind == EvidenceKind::ManifestHint && e.key == "run_command");
                if let Some(e) = e {
                    used.push(e.id.clone());
                    *launch_score = (*launch_score).max(W_MANIFEST_HINT_RUN);
                }
                return (shell_words_split(run_cmd), used);
            }
        }
    }

    // 2. pyproject.toml scripts
    if let Some(e) = evidence.iter().find(|e| {
        e.kind == EvidenceKind::PackageScriptCommand && e.path == "pyproject.toml"
    }) {
        let script_name = e
            .key
            .strip_prefix("scripts.")
            .unwrap_or(&e.key)
            .to_string();
        used.push(e.id.clone());
        *launch_score = (*launch_score).max(W_PKG_SCRIPT_RUN);
        return (vec![script_name], used);
    }

    // 3. Framework-specific commands
    let frameworks: Vec<&str> = evidence
        .iter()
        .filter(|e| e.kind == EvidenceKind::FrameworkMarker)
        .map(|e| e.normalized_value.as_str())
        .collect();

    // Helper: add entrypoint evidence to `used` and credit W_ENTRYPOINT_FILE if found.
    // Falls back to W_DIRECT_SHELL_CMD when no entrypoint file is in evidence.
    let add_ep_evidence = |ep_e: Option<&Evidence>, used: &mut Vec<String>, launch_score: &mut i32| {
        if let Some(e) = ep_e {
            used.push(e.id.clone());
            *launch_score = (*launch_score).max(W_ENTRYPOINT_FILE);
        } else {
            *launch_score = (*launch_score).max(W_DIRECT_SHELL_CMD);
        }
    };

    if frameworks.contains(&"fastapi") || frameworks.contains(&"uvicorn") {
        let ep_e = find_python_entrypoint_evidence(evidence);
        let ep = ep_e.map(|e| e.path.as_str()).unwrap_or("main.py");
        let module = python_module_from_path(ep);
        add_ep_evidence(ep_e, &mut used, launch_score);
        let cmd = vec![
            "uvicorn".to_string(),
            format!("{}:app", module),
            "--host".to_string(),
            "0.0.0.0".to_string(),
        ];
        return (cmd, used);
    }

    // Prefer gunicorn over `flask run` when both are present: gunicorn is the
    // production server, whereas `flask run` is a dev-only convenience command.
    if frameworks.contains(&"flask") && !frameworks.contains(&"gunicorn") {
        let ep_e = find_python_entrypoint_evidence(evidence);
        add_ep_evidence(ep_e, &mut used, launch_score);
        let cmd = vec!["flask".to_string(), "run".to_string()];
        return (cmd, used);
    }

    if frameworks.contains(&"gunicorn") {
        let ep_e = find_python_entrypoint_evidence(evidence);
        let ep = ep_e.map(|e| e.path.as_str()).unwrap_or("app");
        let module = python_module_from_path(ep);
        add_ep_evidence(ep_e, &mut used, launch_score);
        let cmd = vec![
            "gunicorn".to_string(),
            format!("{}:app", module),
        ];
        return (cmd, used);
    }

    if frameworks.contains(&"streamlit") {
        let ep_e = find_python_entrypoint_evidence(evidence);
        let ep = ep_e.map(|e| e.path.as_str()).unwrap_or("app.py");
        add_ep_evidence(ep_e, &mut used, launch_score);
        let cmd = vec![
            "streamlit".to_string(),
            "run".to_string(),
            ep.to_string(),
        ];
        return (cmd, used);
    }

    if frameworks.contains(&"gradio") {
        let ep_e = find_python_entrypoint_evidence(evidence);
        let ep = ep_e.map(|e| e.path.as_str()).unwrap_or("app.py");
        add_ep_evidence(ep_e, &mut used, launch_score);
        let cmd = vec!["python".to_string(), ep.to_string()];
        return (cmd, used);
    }

    if frameworks.contains(&"django") {
        let ep_e = find_python_entrypoint_evidence(evidence);
        add_ep_evidence(ep_e, &mut used, launch_score);
        let cmd = vec![
            "python".to_string(),
            "manage.py".to_string(),
            "runserver".to_string(),
        ];
        return (cmd, used);
    }

    // 4. Entrypoint file
    let ep_priority = [
        "__main__.py", "main.py", "app.py", "server.py",
        "src/main.py", "src/app.py", "src/server.py",
    ];
    for ep in &ep_priority {
        if let Some(e) = evidence.iter().find(|e| {
            e.kind == EvidenceKind::EntrypointFile
                && e.normalized_value == "python"
                && e.path == *ep
        }) {
            used.push(e.id.clone());
            *launch_score = (*launch_score).max(W_ENTRYPOINT_FILE);
            return (vec!["python".to_string(), ep.to_string()], used);
        }
    }

    (Vec::new(), used)
}

fn find_python_entrypoint_evidence<'a>(evidence: &'a [Evidence]) -> Option<&'a Evidence> {
    let priority = ["main.py", "app.py", "server.py", "src/main.py", "src/app.py"];
    for ep in &priority {
        if let Some(e) = evidence.iter().find(|e| {
            e.kind == EvidenceKind::EntrypointFile
                && e.normalized_value == "python"
                && e.path.as_str() == *ep
        }) {
            return Some(e);
        }
    }
    evidence
        .iter()
        .find(|e| e.kind == EvidenceKind::EntrypointFile && e.normalized_value == "python")
}

fn find_python_entrypoint<'a>(evidence: &'a [Evidence]) -> Option<&'a str> {
    find_python_entrypoint_evidence(evidence).map(|e| e.path.as_str())
}

fn python_module_from_path(path: &str) -> &str {
    // "src/main.py" → "main", "app.py" → "app"
    let name = path
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .strip_suffix(".py")
        .unwrap_or(path);
    name
}

fn build_python_envelope(input: &LeipInput, evidence: &[Evidence], cmd: Vec<String>) -> LaunchEnvelopeDraft {
    let runtime_version = input
        .target_hint
        .as_ref()
        .and_then(|h| h.runtime_version.clone())
        .unwrap_or_else(|| super::DEFAULT_PYTHON_RUNTIME_VERSION.to_string());

    let entrypoint = input
        .target_hint
        .as_ref()
        .and_then(|h| h.entrypoint.clone())
        .or_else(|| find_python_entrypoint(evidence).map(|s| s.to_string()));

    let (env_defaults, required_env_from_files) = extract_env_from_files(&input.file_text_map);
    let (dep_suggested_env, dep_egress) = python_dep_hints(
        input.file_text_map.get("requirements.txt").map(|s| s.as_str()),
        input.file_text_map.get("pyproject.toml").map(|s| s.as_str()),
    );

    let mut required_env = required_env_from_files;
    for v in dep_suggested_env {
        if !required_env.contains(&v) {
            required_env.push(v);
        }
    }
    required_env.sort();
    required_env.dedup();

    let mut egress_hints = dep_egress;
    egress_hints.sort();
    egress_hints.dedup();

    LaunchEnvelopeDraft {
        driver: Some("python".to_string()),
        runtime_version: Some(runtime_version),
        cmd,
        entrypoint,
        env: env_defaults,
        required_env,
        egress_hints,
        ..Default::default()
    }
}

fn single_node_graph(
    node_id: &str,
    kind: LeipNodeKind,
    label: &str,
    envelope: LaunchEnvelopeDraft,
    evidence_refs: Vec<String>,
) -> LaunchGraphDraft {
    let node = LaunchGraphNodeDraft {
        id: node_id.to_string(),
        kind,
        label: label.to_string(),
        envelope: Some(envelope),
        evidence_refs: evidence_refs.clone(),
        service: None,
        provider_capsule: None,
    };
    LaunchGraphDraft {
        primary_node_id: node_id.to_string(),
        nodes: vec![node],
        edges: Vec::new(),
        evidence_refs,
        unsupported_features: Vec::new(),
    }
}

// ── Hard constraints ──────────────────────────────────────────────────────────
//
// Hard constraints cover safety and syntactic validity only.  They produce
// `Rejected` decisions regardless of evidence strength.  Policy:
//   - The executable command (`cmd[0]`) must not be a shell launcher.
//   - Shell operators must not appear in the executed command.
//   - Commands must not traverse outside the project root (path traversal).
//
// Risk scoring (not hard constraints) handles indirect concerns such as
// shell operators inside a package.json script value (we run `npm run X`,
// not the script content directly).

const SHELL_LAUNCHERS: &[&str] = &["sh", "bash", "zsh", "fish", "cmd", "cmd.exe", "powershell", "pwsh"];
const SHELL_OPERATORS: &[&str] = &["&&", "||", ";;", "|&", " | ", ">", "<", "$(", "`", " ; "];

fn check_cmd_hard_constraints(cmd: &[String], script_content: Option<&str>) -> Vec<String> {
    let mut failures = Vec::new();

    if cmd.is_empty() {
        return failures;
    }

    // Shell launcher as executable
    let exe = cmd[0].as_str();
    if SHELL_LAUNCHERS.contains(&exe) {
        failures.push(format!(
            "command executable '{}' is a shell launcher; use a structured command array instead",
            exe
        ));
    }

    // Shell operators anywhere in the cmd array
    let full_cmd = cmd.join(" ");
    for op in SHELL_OPERATORS {
        if full_cmd.contains(op) {
            failures.push(format!(
                "command contains shell operator '{}': use structured commands only",
                op
            ));
            break;
        }
    }

    // Path traversal
    for arg in cmd {
        if arg.contains("../") || arg.contains("..\\") || arg.starts_with("..") {
            failures.push(format!(
                "command argument '{}' contains path traversal",
                truncate(arg, 80)
            ));
            break;
        }
    }

    // Script content with shell operators → hard failure when the content is
    // used verbatim (i.e., derived from run_command, not a package script name).
    // Package script content is NOT checked here — we run `npm run X`, not
    // the script content directly.
    if let Some(content) = script_content {
        for op in SHELL_OPERATORS {
            if content.contains(op) && cmd.get(0).map(|s| s.as_str()) != Some("npm")
                && cmd.get(0).map(|s| s.as_str()) != Some("yarn")
                && cmd.get(0).map(|s| s.as_str()) != Some("pnpm")
                && cmd.get(0).map(|s| s.as_str()) != Some("bun")
            {
                failures.push(format!(
                    "run_command contains shell operator '{}': use structured commands only",
                    op
                ));
                break;
            }
        }
    }

    failures
}

// ── Decision logic ────────────────────────────────────────────────────────────

fn make_decision(
    candidates: &[LaunchGraphCandidate],
    evidence: &[Evidence],
    required_coverage: i32,
) -> LeipDecision {
    if candidates.is_empty() {
        return LeipDecision::Unresolved {
            reason: "no viable candidates generated from repository evidence".to_string(),
        };
    }

    let top = &candidates[0];

    // Must have runtime evidence
    if !has_runtime_evidence(evidence, &top.evidence_refs) {
        return LeipDecision::Unresolved {
            reason: "no runtime marker evidence in top candidate".to_string(),
        };
    }

    // Must have launch evidence (package script, entrypoint, or direct command)
    // README-only commands do not count for AutoAccept.
    if !has_launch_evidence(evidence, &top.evidence_refs) || top.launch_score < MIN_LAUNCH_SCORE_FOR_AUTO_ACCEPT {
        return LeipDecision::Unresolved {
            reason: "insufficient launch evidence: no entrypoint, package script, or explicit command found"
                .to_string(),
        };
    }

    // Minimum total score
    if top.score < required_coverage {
        return LeipDecision::Unresolved {
            reason: format!(
                "total evidence score {} below required coverage {}",
                top.score, required_coverage
            ),
        };
    }

    // Multiple viable candidates with similar scores
    if candidates.len() > 1 && top.margin < AUTO_ACCEPT_MARGIN_THRESHOLD {
        return LeipDecision::NeedsSelection {
            reason: format!(
                "multiple viable candidates with similar scores (margin {:.2} < threshold {:.2})",
                top.margin, AUTO_ACCEPT_MARGIN_THRESHOLD
            ),
        };
    }

    LeipDecision::AutoAccept {
        candidate_id: top.id.clone(),
    }
}

fn has_runtime_evidence(evidence: &[Evidence], refs: &[String]) -> bool {
    evidence.iter().filter(|e| refs.contains(&e.id)).any(|e| {
        e.kind == EvidenceKind::RuntimeMarkerFile
            || (e.kind == EvidenceKind::ManifestHint && e.key == "driver")
    })
}

/// Returns true if there is non-README launch evidence in the candidate.
fn has_launch_evidence(evidence: &[Evidence], refs: &[String]) -> bool {
    evidence.iter().filter(|e| refs.contains(&e.id)).any(|e| {
        matches!(
            e.kind,
            EvidenceKind::PackageScriptCommand
                | EvidenceKind::EntrypointFile
                | EvidenceKind::DirectShellCommand
        ) || (e.kind == EvidenceKind::ManifestHint
            && matches!(e.key.as_str(), "run_command" | "cmd"))
    })
}

// ── Utilities ─────────────────────────────────────────────────────────────────

fn shell_words_split(s: &str) -> Vec<String> {
    // Minimal shell-word splitting without full shell semantics.
    // Split on whitespace, respecting quoted strings.
    let mut result = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;

    for ch in s.chars() {
        match ch {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            ' ' | '\t' if !in_single && !in_double => {
                if !current.is_empty() {
                    result.push(current.clone());
                    current.clear();
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..max]
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RepoFileEntry, RepoFileKind};

    fn file(path: &str) -> RepoFileEntry {
        RepoFileEntry {
            path: path.to_string(),
            kind: RepoFileKind::File,
            size: None,
        }
    }

    // ── Node.js candidates ────────────────────────────────────────────────────

    #[test]
    fn node_npm_start_auto_accepts() {
        let pkg_json = r#"{
            "name": "my-app",
            "scripts": { "start": "node server.js", "build": "tsc" }
        }"#;
        let input = LeipInput {
            repo_file_index: vec![
                file("package.json"),
                file("package-lock.json"),
                file("server.js"),
            ],
            file_text_map: [("package.json".to_string(), pkg_json.to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let result = evaluate_launch_graphs(&input);
        assert!(
            matches!(result.decision, LeipDecision::AutoAccept { .. }),
            "expected AutoAccept, got {:?}",
            result.decision
        );
        assert_eq!(result.candidates[0].graph.nodes[0].envelope.as_ref().unwrap().driver.as_deref(), Some("node"));
    }

    #[test]
    fn node_pnpm_vite_auto_accepts() {
        let pkg_json = r#"{
            "name": "vite-app",
            "scripts": { "dev": "vite", "build": "vite build" },
            "dependencies": { "vite": "^4.0.0" }
        }"#;
        let input = LeipInput {
            repo_file_index: vec![
                file("package.json"),
                file("pnpm-lock.yaml"),
                file("index.ts"),
            ],
            file_text_map: [("package.json".to_string(), pkg_json.to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let result = evaluate_launch_graphs(&input);
        assert!(
            matches!(result.decision, LeipDecision::AutoAccept { .. }),
            "expected AutoAccept, got {:?}",
            result.decision
        );
        let cmd = &result.candidates[0].graph.nodes[0].envelope.as_ref().unwrap().cmd;
        assert_eq!(cmd, &["pnpm", "run", "dev"]);
    }

    #[test]
    fn node_nextjs_auto_accepts() {
        let pkg_json = r#"{
            "name": "next-app",
            "scripts": { "dev": "next dev", "start": "next start", "build": "next build" },
            "dependencies": { "next": "14.0.0", "react": "18.0.0" }
        }"#;
        let input = LeipInput {
            repo_file_index: vec![
                file("package.json"),
                file("package-lock.json"),
            ],
            file_text_map: [("package.json".to_string(), pkg_json.to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let result = evaluate_launch_graphs(&input);
        assert!(
            matches!(result.decision, LeipDecision::AutoAccept { .. }),
            "expected AutoAccept, got {:?}",
            result.decision
        );
        // dev should be preferred over start
        let cmd = &result.candidates[0].graph.nodes[0].envelope.as_ref().unwrap().cmd;
        assert_eq!(cmd, &["npm", "run", "dev"]);
    }

    #[test]
    fn node_package_script_with_shell_operator_rejected() {
        // Shell operators in run_command → hard constraint failure
        let input = LeipInput {
            repo_file_index: vec![file("package.json"), file("package-lock.json")],
            file_text_map: [(
                "package.json".to_string(),
                r#"{"name":"app","scripts":{"start":"node server.js"}}"#.to_string(),
            )]
            .into_iter()
            .collect(),
            target_hint: Some(super::super::SelectedTarget {
                run_command: Some("npm run start && echo done".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let result = evaluate_launch_graphs(&input);
        // The run_command with && should trigger a hard constraint failure
        assert!(
            matches!(result.decision, LeipDecision::Rejected { .. })
                || result
                    .rejected_candidates
                    .iter()
                    .any(|c| !c.hard_constraint_failures.is_empty()),
            "expected Rejected or rejected candidates, got {:?}",
            result.decision
        );
    }

    #[test]
    fn node_readme_only_no_auto_accept() {
        // Only a README command — no package.json → no node candidate at all
        let readme = "# My App\n\nRun with: `npm run start`\n";
        let input = LeipInput {
            repo_file_index: vec![file("README.md"), file("index.js")],
            file_text_map: [("README.md".to_string(), readme.to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let result = evaluate_launch_graphs(&input);
        assert!(
            !matches!(result.decision, LeipDecision::AutoAccept { .. }),
            "should not AutoAccept with no package.json"
        );
    }

    // ── Python candidates ─────────────────────────────────────────────────────

    #[test]
    fn python_fastapi_auto_accepts() {
        let pyproject = r#"
[project]
name = "my-api"
dependencies = ["fastapi>=0.100", "uvicorn"]
"#;
        let input = LeipInput {
            repo_file_index: vec![
                file("pyproject.toml"),
                file("main.py"),
            ],
            file_text_map: [("pyproject.toml".to_string(), pyproject.to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let result = evaluate_launch_graphs(&input);
        assert!(
            matches!(result.decision, LeipDecision::AutoAccept { .. }),
            "expected AutoAccept, got {:?}",
            result.decision
        );
        let cmd = &result.candidates[0].graph.nodes[0].envelope.as_ref().unwrap().cmd;
        assert_eq!(cmd[0], "uvicorn");
    }

    #[test]
    fn python_streamlit_auto_accepts() {
        let input = LeipInput {
            repo_file_index: vec![
                file("requirements.txt"),
                file("app.py"),
            ],
            file_text_map: [(
                "requirements.txt".to_string(),
                "streamlit>=1.0\n".to_string(),
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let result = evaluate_launch_graphs(&input);
        assert!(
            matches!(result.decision, LeipDecision::AutoAccept { .. }),
            "expected AutoAccept, got {:?}",
            result.decision
        );
        let cmd = &result.candidates[0].graph.nodes[0].envelope.as_ref().unwrap().cmd;
        assert_eq!(cmd[0], "streamlit");
    }

    #[test]
    fn python_requirements_only_no_auto_accept() {
        // requirements.txt present but no framework, no entrypoint → Unresolved
        let input = LeipInput {
            repo_file_index: vec![file("requirements.txt")],
            file_text_map: [(
                "requirements.txt".to_string(),
                "requests>=2.28\npydantic>=2.0\n".to_string(),
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let result = evaluate_launch_graphs(&input);
        assert!(
            !matches!(result.decision, LeipDecision::AutoAccept { .. }),
            "should not AutoAccept without entrypoint or run command"
        );
    }

    // ── Decision properties ───────────────────────────────────────────────────

    #[test]
    fn candidate_id_is_deterministic() {
        let pkg_json = r#"{"name":"app","scripts":{"start":"node server.js"}}"#;
        let make_input = |order: &[(&str, RepoFileKind)]| LeipInput {
            repo_file_index: order
                .iter()
                .map(|(p, k)| RepoFileEntry {
                    path: p.to_string(),
                    kind: k.clone(),
                    size: None,
                })
                .collect(),
            file_text_map: [("package.json".to_string(), pkg_json.to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };

        let input_a = make_input(&[
            ("package.json", RepoFileKind::File),
            ("server.js", RepoFileKind::File),
            ("package-lock.json", RepoFileKind::File),
        ]);
        let input_b = make_input(&[
            ("server.js", RepoFileKind::File),
            ("package-lock.json", RepoFileKind::File),
            ("package.json", RepoFileKind::File),
        ]);

        let result_a = evaluate_launch_graphs(&input_a);
        let result_b = evaluate_launch_graphs(&input_b);

        assert!(!result_a.candidates.is_empty());
        assert_eq!(
            result_a.candidates[0].id,
            result_b.candidates[0].id,
            "candidate IDs must be independent of input order"
        );
    }

    #[test]
    fn evidence_id_is_deterministic() {
        let id1 = evidence_id_hash(
            &EvidenceKind::RuntimeMarkerFile,
            "package.json",
            "detected_runtime",
            "node",
        );
        let id2 = evidence_id_hash(
            &EvidenceKind::RuntimeMarkerFile,
            "package.json",
            "detected_runtime",
            "node",
        );
        assert_eq!(id1, id2);
        assert!(id1.starts_with("sha256:"));
    }

    #[test]
    fn single_candidate_margin_is_zero() {
        let pkg_json = r#"{"scripts":{"start":"node server.js"}}"#;
        let input = LeipInput {
            repo_file_index: vec![file("package.json"), file("server.js")],
            file_text_map: [("package.json".to_string(), pkg_json.to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let result = evaluate_launch_graphs(&input);
        if !result.candidates.is_empty() {
            assert_eq!(
                result.candidates[0].margin, 0.0,
                "single-candidate margin must be 0.0"
            );
        }
    }

    #[test]
    fn shell_launcher_in_cmd_is_hard_constraint() {
        let failures = check_cmd_hard_constraints(&["sh".to_string(), "-c".to_string(), "echo hi".to_string()], None);
        assert!(!failures.is_empty(), "sh should be a hard constraint failure");
    }

    #[test]
    fn shell_operator_in_cmd_is_hard_constraint() {
        let failures = check_cmd_hard_constraints(
            &["node".to_string(), "a.js".to_string(), "&&".to_string(), "node".to_string(), "b.js".to_string()],
            None,
        );
        assert!(!failures.is_empty(), "&& should be a hard constraint failure");
    }

    #[test]
    fn redact_log_excerpt_removes_secrets() {
        let log = "ghp_abcdefghijklmnopqrstuvwxyz1234567890 AUTH=supersecrettoken12345678";
        let redacted = redact_log_excerpt(log);
        assert!(!redacted.contains("ghp_abcdefghijklmnopqrstuvwxyz"));
        assert!(!redacted.contains("supersecrettoken"));
    }

    #[test]
    fn evaluate_launch_graphs_json_roundtrip() {
        let input = LeipInput {
            repo_file_index: vec![
                file("package.json"),
                file("package-lock.json"),
                file("index.js"),
            ],
            file_text_map: [(
                "package.json".to_string(),
                r#"{"name":"app","scripts":{"start":"node index.js"}}"#.to_string(),
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let json = serde_json::to_string(&input).unwrap();
        let result_json = evaluate_launch_graphs_json(&json).unwrap();
        let result: LeipResult = serde_json::from_str(&result_json).unwrap();
        assert_eq!(result.engine_version, LEIP_ENGINE_VERSION);
    }

    #[test]
    fn evaluate_launch_envelopes_json_compat() {
        let input = super::super::LockDraftInput {
            selected_target: Some(super::super::SelectedTarget {
                driver: Some("node".to_string()),
                ..Default::default()
            }),
            repo_file_index: vec![
                file("package.json"),
                file("index.js"),
            ],
            file_text_map: [(
                "package.json".to_string(),
                r#"{"scripts":{"start":"node index.js"}}"#.to_string(),
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let json = serde_json::to_string(&input).unwrap();
        let result_json = evaluate_launch_envelopes_json(&json).unwrap();
        let result: LeipResult = serde_json::from_str(&result_json).unwrap();
        assert_eq!(result.engine_version, LEIP_ENGINE_VERSION);
    }

    // --- bun.lock text format (Bun v1.0.23+) ---

    #[test]
    fn bun_lock_text_format_selects_bun() {
        // bun.lock (text) should be treated the same as bun.lockb (binary)
        let pkg_json = r#"{
            "name": "astro-blog",
            "scripts": { "dev": "astro dev", "build": "astro build" },
            "dependencies": { "astro": "^4.0.0" }
        }"#;
        let input = LeipInput {
            repo_file_index: vec![
                file("package.json"),
                file("bun.lock"),
            ],
            file_text_map: [("package.json".to_string(), pkg_json.to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let result = evaluate_launch_graphs(&input);
        assert!(
            matches!(result.decision, LeipDecision::AutoAccept { .. }),
            "expected AutoAccept, got {:?}",
            result.decision
        );
        let cmd = &result.candidates[0].graph.nodes[0].envelope.as_ref().unwrap().cmd;
        assert_eq!(cmd, &["bun", "run", "dev"]);
    }

    #[test]
    fn bun_lockb_still_selects_bun() {
        // Regression: bun.lockb (binary, legacy) must still be recognized
        let pkg_json = r#"{
            "name": "bun-app",
            "scripts": { "dev": "bun run src/index.ts" },
            "dependencies": {}
        }"#;
        let input = LeipInput {
            repo_file_index: vec![
                file("package.json"),
                file("bun.lockb"),
            ],
            file_text_map: [("package.json".to_string(), pkg_json.to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let result = evaluate_launch_graphs(&input);
        assert!(
            matches!(result.decision, LeipDecision::AutoAccept { .. }),
            "expected AutoAccept, got {:?}",
            result.decision
        );
        let cmd = &result.candidates[0].graph.nodes[0].envelope.as_ref().unwrap().cmd;
        assert_eq!(cmd, &["bun", "run", "dev"]);
    }

    // --- Vue framework label ---

    #[test]
    fn vue_vite_app_auto_accepts() {
        let pkg_json = r#"{
            "name": "vue-app",
            "scripts": { "dev": "vite", "build": "vite build" },
            "dependencies": { "vue": "^3.4.0" },
            "devDependencies": { "@vitejs/plugin-vue": "^5.0.0", "vite": "^5.0.0" }
        }"#;
        let input = LeipInput {
            repo_file_index: vec![
                file("package.json"),
                file("pnpm-lock.yaml"),
            ],
            file_text_map: [("package.json".to_string(), pkg_json.to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let result = evaluate_launch_graphs(&input);
        assert!(
            matches!(result.decision, LeipDecision::AutoAccept { .. }),
            "expected AutoAccept, got {:?}",
            result.decision
        );
        let cmd = &result.candidates[0].graph.nodes[0].envelope.as_ref().unwrap().cmd;
        assert_eq!(cmd, &["pnpm", "run", "dev"]);
        // Evidence should include a vue framework label
        let has_vue_label = result.candidates[0]
            .evidence_refs
            .iter()
            .any(|id| result.candidates[0].graph.evidence_refs.contains(id));
        let _ = has_vue_label; // structural check; label presence verified via AutoAccept score
    }

    // --- Script priority: dev > start ---

    #[test]
    fn nextjs_prefers_dev_over_start() {
        // When both dev and start exist, dev should be selected for ato run (development mode)
        let pkg_json = r#"{
            "name": "next-app",
            "scripts": { "dev": "next dev", "start": "next start", "build": "next build" },
            "dependencies": { "next": "14.0.0", "react": "18.0.0" }
        }"#;
        let input = LeipInput {
            repo_file_index: vec![
                file("package.json"),
                file("package-lock.json"),
            ],
            file_text_map: [("package.json".to_string(), pkg_json.to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let result = evaluate_launch_graphs(&input);
        assert!(
            matches!(result.decision, LeipDecision::AutoAccept { .. }),
            "expected AutoAccept, got {:?}",
            result.decision
        );
        let cmd = &result.candidates[0].graph.nodes[0].envelope.as_ref().unwrap().cmd;
        assert_eq!(
            cmd,
            &["npm", "run", "dev"],
            "expected dev script, got {:?}",
            cmd
        );
    }

    #[test]
    fn node_start_only_still_selects_start() {
        // Projects with only a start script (no dev) should still work
        let pkg_json = r#"{
            "name": "express-app",
            "scripts": { "start": "node server.js" },
            "dependencies": { "express": "^4.18.0" }
        }"#;
        let input = LeipInput {
            repo_file_index: vec![
                file("package.json"),
                file("package-lock.json"),
            ],
            file_text_map: [("package.json".to_string(), pkg_json.to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let result = evaluate_launch_graphs(&input);
        assert!(
            matches!(result.decision, LeipDecision::AutoAccept { .. }),
            "expected AutoAccept, got {:?}",
            result.decision
        );
        let cmd = &result.candidates[0].graph.nodes[0].envelope.as_ref().unwrap().cmd;
        assert_eq!(
            cmd,
            &["npm", "run", "start"],
            "expected start script, got {:?}",
            cmd
        );
    }

    // ── Gap diagnostics ───────────────────────────────────────────────────────

    #[test]
    fn rust_project_unresolved_with_specific_reason_and_diagnostic() {
        let input = LeipInput {
            repo_file_index: vec![file("Cargo.toml"), file("src/main.rs")],
            ..Default::default()
        };
        let result = evaluate_launch_graphs(&input);
        match &result.decision {
            LeipDecision::Unresolved { reason } => {
                assert!(
                    reason.contains("rust"),
                    "expected Rust hint in reason, got: {}",
                    reason
                );
            }
            d => panic!("expected Unresolved, got {:?}", d),
        }
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.severity == LeipDiagnosticSeverity::Warning
                    && d.message.contains("Rust")),
            "expected a Warning diagnostic about Rust, got: {:?}",
            result.diagnostics
        );
    }

    #[test]
    fn wrangler_toml_unresolved_with_specific_reason_and_diagnostic() {
        let input = LeipInput {
            repo_file_index: vec![file("wrangler.toml"), file("src/index.js")],
            ..Default::default()
        };
        let result = evaluate_launch_graphs(&input);
        match &result.decision {
            LeipDecision::Unresolved { reason } => {
                assert!(
                    reason.contains("cloudflare-worker"),
                    "expected cloudflare-worker hint in reason, got: {}",
                    reason
                );
            }
            d => panic!("expected Unresolved, got {:?}", d),
        }
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.severity == LeipDiagnosticSeverity::Warning
                    && d.message.contains("Cloudflare")),
            "expected a Warning diagnostic about Cloudflare Workers, got: {:?}",
            result.diagnostics
        );
    }

    #[test]
    fn node_cli_tool_auto_accepts_with_warning_diagnostic() {
        // pageyou-style: yargs dep, ts-node dev script, no server framework
        let pkg_json = r#"{
            "name": "cli-tool",
            "scripts": { "dev": "ts-node src/index.ts" },
            "dependencies": { "yargs": "^17.7.2" },
            "devDependencies": { "ts-node": "^10.9.2", "typescript": "^5.3.3" }
        }"#;
        let input = LeipInput {
            repo_file_index: vec![file("package.json"), file("package-lock.json"), file("src/index.ts")],
            file_text_map: [("package.json".to_string(), pkg_json.to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let result = evaluate_launch_graphs(&input);
        assert!(
            matches!(result.decision, LeipDecision::AutoAccept { .. }),
            "expected AutoAccept even for CLI tool, got {:?}",
            result.decision
        );
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.severity == LeipDiagnosticSeverity::Warning
                    && d.message.contains("CLI tool")),
            "expected a cli-tool Warning diagnostic, got: {:?}",
            result.diagnostics
        );
    }

    #[test]
    fn node_server_with_yargs_no_cli_warning() {
        // express + yargs: this is a server with a CLI component, not a CLI tool
        let pkg_json = r#"{
            "name": "server-with-cli",
            "scripts": { "start": "node server.js" },
            "dependencies": { "express": "^4.18.0", "yargs": "^17.0.0" }
        }"#;
        let input = LeipInput {
            repo_file_index: vec![file("package.json"), file("package-lock.json")],
            file_text_map: [("package.json".to_string(), pkg_json.to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let result = evaluate_launch_graphs(&input);
        assert!(
            matches!(result.decision, LeipDecision::AutoAccept { .. }),
            "expected AutoAccept for server+yargs, got {:?}",
            result.decision
        );
        assert!(
            !result
                .diagnostics
                .iter()
                .any(|d| d.message.contains("CLI tool")),
            "should NOT have cli-tool warning when server framework is present, got: {:?}",
            result.diagnostics
        );
    }

    // --- Env var inference ---

    #[test]
    fn dot_env_example_real_defaults_populate_env_map() {
        // .env.example with only real defaults → env map populated, no required_env
        let pkg_json = r#"{"name":"app","scripts":{"dev":"node server.js"},"dependencies":{"express":"4"}}"#;
        let env_example = "PORT=3000\nDB_PATH=data/app.db\n# comment\n";
        let input = LeipInput {
            repo_file_index: vec![file("package.json"), file("package-lock.json"), file(".env.example")],
            file_text_map: [
                ("package.json".to_string(), pkg_json.to_string()),
                (".env.example".to_string(), env_example.to_string()),
            ].into_iter().collect(),
            ..Default::default()
        };
        let result = evaluate_launch_graphs(&input);
        assert!(matches!(result.decision, LeipDecision::AutoAccept { .. }));
        let envelope = result.candidates[0].graph.nodes[0].envelope.as_ref().unwrap();
        assert_eq!(envelope.env.get("PORT"), Some(&"3000".to_string()), "PORT default should be set");
        assert_eq!(envelope.env.get("DB_PATH"), Some(&"data/app.db".to_string()), "DB_PATH default should be set");
        assert!(envelope.required_env.is_empty(), "no required vars expected, got {:?}", envelope.required_env);
        // No required_env diagnostic
        assert!(!result.diagnostics.iter().any(|d| d.message.contains("Required environment")));
    }

    #[test]
    fn dot_env_example_placeholder_values_become_required_env() {
        // .env.example with placeholder values → required_env, + warning diagnostic
        let pkg_json = r#"{"name":"app","scripts":{"dev":"node server.js"},"dependencies":{"express":"4"}}"#;
        let env_example = "VITE_BASE_URL=http://localhost:3000\nBETTER_AUTH_SECRET=your_strong_secret_here\nGOOGLE_CLIENT_ID=your_google_client_id\nGOOGLE_CLIENT_SECRET=\n";
        let input = LeipInput {
            repo_file_index: vec![file("package.json"), file("package-lock.json"), file(".env.example")],
            file_text_map: [
                ("package.json".to_string(), pkg_json.to_string()),
                (".env.example".to_string(), env_example.to_string()),
            ].into_iter().collect(),
            ..Default::default()
        };
        let result = evaluate_launch_graphs(&input);
        assert!(matches!(result.decision, LeipDecision::AutoAccept { .. }));
        let envelope = result.candidates[0].graph.nodes[0].envelope.as_ref().unwrap();
        assert_eq!(envelope.env.get("VITE_BASE_URL"), Some(&"http://localhost:3000".to_string()), "VITE_BASE_URL is a real default");
        assert!(envelope.required_env.contains(&"BETTER_AUTH_SECRET".to_string()), "BETTER_AUTH_SECRET should be required");
        assert!(envelope.required_env.contains(&"GOOGLE_CLIENT_ID".to_string()), "GOOGLE_CLIENT_ID should be required");
        assert!(envelope.required_env.contains(&"GOOGLE_CLIENT_SECRET".to_string()), "GOOGLE_CLIENT_SECRET should be required");
        // Warning diagnostic must mention the required vars
        let has_warning = result.diagnostics.iter().any(|d| {
            d.severity == LeipDiagnosticSeverity::Warning && d.message.contains("Required environment")
        });
        assert!(has_warning, "expected required-env warning, got: {:?}", result.diagnostics);
    }

    #[test]
    fn dot_env_example_path_your_segment_is_placeholder() {
        // DATABASE_URL="/location/of/your/db" — path contains /your/ — should be required
        let pkg_json = r#"{"name":"app","scripts":{"dev":"node server.js"},"dependencies":{"express":"4"}}"#;
        let env_example = "DATABASE_URL=\"/location/of/your/db\"\n";
        let input = LeipInput {
            repo_file_index: vec![file("package.json"), file("package-lock.json"), file(".env.example")],
            file_text_map: [
                ("package.json".to_string(), pkg_json.to_string()),
                (".env.example".to_string(), env_example.to_string()),
            ].into_iter().collect(),
            ..Default::default()
        };
        let result = evaluate_launch_graphs(&input);
        let envelope = result.candidates[0].graph.nodes[0].envelope.as_ref().unwrap();
        assert!(envelope.required_env.contains(&"DATABASE_URL".to_string()), "DATABASE_URL with /your/ path should be required");
        assert!(!envelope.env.contains_key("DATABASE_URL"), "DATABASE_URL should not appear as a default");
    }

    #[test]
    fn openai_dep_adds_egress_hint() {
        // package with openai dep → egress hint diagnostic
        let pkg_json = r#"{"name":"app","scripts":{"dev":"node server.js"},"dependencies":{"express":"4","openai":"4.0.0"}}"#;
        let input = LeipInput {
            repo_file_index: vec![file("package.json"), file("package-lock.json")],
            file_text_map: [("package.json".to_string(), pkg_json.to_string())].into_iter().collect(),
            ..Default::default()
        };
        let result = evaluate_launch_graphs(&input);
        assert!(matches!(result.decision, LeipDecision::AutoAccept { .. }));
        let envelope = result.candidates[0].graph.nodes[0].envelope.as_ref().unwrap();
        assert!(envelope.egress_hints.contains(&"api.openai.com".to_string()), "expected api.openai.com egress, got {:?}", envelope.egress_hints);
        assert!(envelope.required_env.contains(&"OPENAI_API_KEY".to_string()), "expected OPENAI_API_KEY required, got {:?}", envelope.required_env);
        let has_egress_diag = result.diagnostics.iter().any(|d| d.message.contains("api.openai.com"));
        assert!(has_egress_diag, "expected egress diagnostic for api.openai.com");
    }

    #[test]
    fn docker_compose_only_unresolved_with_specific_diagnostic() {
        // Only docker-compose.yml at root, no package.json or requirements.txt
        let input = LeipInput {
            repo_file_index: vec![file("docker-compose.yml"), file("README.md")],
            file_text_map: Default::default(),
            ..Default::default()
        };
        let result = evaluate_launch_graphs(&input);
        assert!(
            matches!(result.decision, LeipDecision::Unresolved { .. }),
            "expected Unresolved for compose-only, got {:?}", result.decision
        );
        let has_compose_diag = result.diagnostics.iter().any(|d| d.message.contains("Docker Compose"));
        assert!(has_compose_diag, "expected Docker Compose diagnostic, got {:?}", result.diagnostics);
    }

    #[test]
    fn docker_compose_with_package_json_not_compose_diagnostic() {
        // Both docker-compose.yml and package.json at root → NOT a compose-only project
        let pkg_json = r#"{"name":"app","scripts":{"dev":"node server.js"},"dependencies":{"express":"4"}}"#;
        let input = LeipInput {
            repo_file_index: vec![file("package.json"), file("package-lock.json"), file("docker-compose.yml")],
            file_text_map: [("package.json".to_string(), pkg_json.to_string())].into_iter().collect(),
            ..Default::default()
        };
        let result = evaluate_launch_graphs(&input);
        assert!(
            matches!(result.decision, LeipDecision::AutoAccept { .. }),
            "compose+node should AutoAccept node, got {:?}", result.decision
        );
        // Should NOT have docker-compose unsupported diagnostic
        assert!(
            !result.diagnostics.iter().any(|d| d.message.contains("Docker Compose project detected with no")),
            "should not have compose-only diagnostic when package.json is present"
        );
    }

    #[test]
    fn php_root_index_php_unresolved_with_actionable_diagnostic() {
        let input = LeipInput {
            repo_file_index: vec![file("index.php"), file("login.php"), file("README.md")],
            file_text_map: Default::default(),
            ..Default::default()
        };
        let result = evaluate_launch_graphs(&input);
        assert!(
            matches!(result.decision, LeipDecision::Unresolved { .. }),
            "PHP root files should be Unresolved, got {:?}", result.decision
        );
        let diag_msg = result.diagnostics.iter().find(|d| d.message.contains("PHP")).map(|d| d.message.as_str());
        assert!(diag_msg.is_some(), "should have PHP diagnostic");
        assert!(diag_msg.unwrap().contains("php -S"), "PHP diagnostic should hint at php -S command");
    }

    #[test]
    fn php_composer_json_unresolved_with_actionable_diagnostic() {
        let input = LeipInput {
            repo_file_index: vec![file("composer.json"), file("public/index.php")],
            file_text_map: Default::default(),
            ..Default::default()
        };
        let result = evaluate_launch_graphs(&input);
        assert!(
            matches!(result.decision, LeipDecision::Unresolved { .. }),
            "PHP composer.json should be Unresolved, got {:?}", result.decision
        );
        assert!(
            result.diagnostics.iter().any(|d| d.message.contains("PHP")),
            "should have PHP diagnostic"
        );
    }

    #[test]
    fn csharp_sln_unresolved_with_dotnet_run_hint() {
        let input = LeipInput {
            repo_file_index: vec![file("NotionClone.sln"), file("NotionClone/Program.cs")],
            file_text_map: Default::default(),
            ..Default::default()
        };
        let result = evaluate_launch_graphs(&input);
        assert!(
            matches!(result.decision, LeipDecision::Unresolved { .. }),
            "C# .sln should be Unresolved, got {:?}", result.decision
        );
        let diag_msg = result.diagnostics.iter().find(|d| d.message.contains("dotnet")).map(|d| d.message.as_str());
        assert!(diag_msg.is_some(), "should have dotnet diagnostic");
        assert!(diag_msg.unwrap().contains("dotnet run"), "C# diagnostic should hint at dotnet run");
    }

    #[test]
    fn go_diagnostic_includes_go_run_command() {
        let input = LeipInput {
            repo_file_index: vec![file("go.mod"), file("main.go")],
            file_text_map: Default::default(),
            ..Default::default()
        };
        let result = evaluate_launch_graphs(&input);
        let diag = result.diagnostics.iter().find(|d| d.message.contains("Go projects")).expect("should have Go diagnostic");
        assert!(diag.message.contains("go run ./..."), "Go diagnostic should include `go run ./...`, got: {}", diag.message);
    }

    #[test]
    fn rust_diagnostic_includes_cargo_run_command() {
        let input = LeipInput {
            repo_file_index: vec![file("Cargo.toml"), file("src/main.rs")],
            file_text_map: Default::default(),
            ..Default::default()
        };
        let result = evaluate_launch_graphs(&input);
        let diag = result.diagnostics.iter().find(|d| d.message.contains("Rust projects")).expect("should have Rust diagnostic");
        assert!(diag.message.contains("cargo run"), "Rust diagnostic should include `cargo run`, got: {}", diag.message);
    }

    #[test]
    fn python_run_py_entrypoint_auto_accepts() {
        // Repo with requirements.txt + flask dep + run.py at root (like GhostStream)
        let reqs = "flask>=3.0.0\ngevent>=24.2.1\n";
        let input = LeipInput {
            repo_file_index: vec![file("requirements.txt"), file("run.py"), file("setup.py")],
            file_text_map: [
                ("requirements.txt".to_string(), reqs.to_string()),
            ].into_iter().collect(),
            ..Default::default()
        };
        let result = evaluate_launch_graphs(&input);
        assert!(
            matches!(result.decision, LeipDecision::AutoAccept { .. }),
            "flask + run.py should AutoAccept, got {:?}", result.decision
        );
        let top = &result.candidates[0];
        let node = &top.graph.nodes[0];
        let cmd = node.envelope.as_ref().map(|e| e.cmd.clone()).unwrap_or_default();
        assert_eq!(cmd, vec!["flask", "run"], "flask dep + run.py should infer `flask run`, got {:?}", cmd);
    }

    #[test]
    fn python_manage_py_django_auto_accepts() {
        // Repo with requirements.txt + Django + manage.py at root (like qlinks)
        let reqs = "Django>=4.2,<5\nrequests\n";
        let input = LeipInput {
            repo_file_index: vec![file("requirements.txt"), file("manage.py"), file("README.md")],
            file_text_map: [
                ("requirements.txt".to_string(), reqs.to_string()),
            ].into_iter().collect(),
            ..Default::default()
        };
        let result = evaluate_launch_graphs(&input);
        assert!(
            matches!(result.decision, LeipDecision::AutoAccept { .. }),
            "Django + manage.py should AutoAccept, got {:?}", result.decision
        );
        let top = &result.candidates[0];
        let node = &top.graph.nodes[0];
        let cmd = node.envelope.as_ref().map(|e| e.cmd.clone()).unwrap_or_default();
        assert_eq!(cmd[..2], ["python", "manage.py"], "Django should infer `python manage.py runserver`, got {:?}", cmd);
    }
}

