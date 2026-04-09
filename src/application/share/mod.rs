use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use anyhow::{Context, Result};
use capsule_core::CapsuleReporter;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::application::auth;
use crate::application::ports::OutputPort;
use crate::progressive_ui;
use crate::reporters::CliReporter;

const SHARE_DIR: &str = ".ato/share";
const SHARE_SPEC_FILE: &str = "share.spec.json";
const SHARE_LOCK_FILE: &str = "share.lock.json";
const SHARE_GUIDE_FILE: &str = "guide.md";
const SHARE_STATE_FILE: &str = "state.json";
const SHARE_SCHEMA_VERSION: &str = "1";
const DEFAULT_API_TIMEOUT_SECS: u64 = 20;

#[derive(Debug, Clone)]
pub(crate) struct EncapArgs {
    pub(crate) path: PathBuf,
    pub(crate) share: bool,
    pub(crate) save_only: bool,
    pub(crate) print_plan: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct DecapArgs {
    pub(crate) input: String,
    pub(crate) into: PathBuf,
    pub(crate) plan: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ShareSpec {
    pub(crate) schema_version: String,
    pub(crate) name: String,
    pub(crate) root: String,
    #[serde(default)]
    pub(crate) sources: Vec<ShareSourceSpec>,
    #[serde(default)]
    pub(crate) tool_requirements: Vec<ToolRequirementSpec>,
    #[serde(default)]
    pub(crate) env_requirements: Vec<EnvRequirementSpec>,
    #[serde(default)]
    pub(crate) install_steps: Vec<InstallStepSpec>,
    #[serde(default)]
    pub(crate) services: Vec<ServiceSpec>,
    #[serde(default)]
    pub(crate) notes: ShareNotes,
    pub(crate) generated_from: GeneratedFrom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ShareSourceSpec {
    pub(crate) id: String,
    pub(crate) kind: String,
    pub(crate) url: String,
    pub(crate) path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) branch: Option<String>,
    #[serde(default)]
    pub(crate) evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ToolRequirementSpec {
    pub(crate) id: String,
    pub(crate) tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) version: Option<String>,
    #[serde(default)]
    pub(crate) required_by: Vec<String>,
    #[serde(default)]
    pub(crate) evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct EnvRequirementSpec {
    pub(crate) id: String,
    pub(crate) path: String,
    pub(crate) required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) template_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) note: Option<String>,
    #[serde(default)]
    pub(crate) evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct InstallStepSpec {
    pub(crate) id: String,
    pub(crate) cwd: String,
    pub(crate) run: String,
    #[serde(default)]
    pub(crate) depends_on: Vec<String>,
    #[serde(default)]
    pub(crate) evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ServiceSpec {
    pub(crate) id: String,
    pub(crate) cwd: String,
    pub(crate) run: String,
    #[serde(default)]
    pub(crate) depends_on: Vec<String>,
    pub(crate) kind: String,
    pub(crate) optional: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) healthcheck: Option<String>,
    #[serde(default)]
    pub(crate) evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct ShareNotes {
    #[serde(default)]
    pub(crate) team_notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct GeneratedFrom {
    pub(crate) root_path: String,
    pub(crate) captured_at: String,
    pub(crate) host_os: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ShareLock {
    pub(crate) schema_version: String,
    pub(crate) spec_digest: String,
    pub(crate) generated_guide_digest: String,
    pub(crate) revision: u32,
    pub(crate) created_at: String,
    pub(crate) resolved_sources: Vec<ResolvedSourceLock>,
    pub(crate) resolved_tools: Vec<ResolvedToolLock>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ResolvedSourceLock {
    pub(crate) id: String,
    pub(crate) rev: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ResolvedToolLock {
    pub(crate) tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) resolved_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) binary_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WorkspaceShareState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) share_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) resolved_revision_url: Option<String>,
    pub(crate) workspace_root: String,
    #[serde(default)]
    pub(crate) sources: Vec<ShareSourceState>,
    #[serde(default)]
    pub(crate) install_steps: Vec<InstallStepState>,
    #[serde(default)]
    pub(crate) env: Vec<EnvState>,
    pub(crate) verification: VerificationState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_verified_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ShareSourceState {
    pub(crate) id: String,
    pub(crate) status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) current_rev: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct InstallStepState {
    pub(crate) id: String,
    pub(crate) status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) finished_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stdout_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stderr_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct EnvState {
    pub(crate) id: String,
    pub(crate) status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct VerificationState {
    pub(crate) result: String,
    #[serde(default)]
    pub(crate) issues: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ShareApiCreateRequest {
    title: String,
    visibility: String,
    spec: ShareSpec,
    lock: ShareLock,
    guide_markdown: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ShareRevisionPayload {
    share_id: String,
    title: String,
    visibility: String,
    revision: u32,
    share_url: String,
    revision_url: String,
    spec: ShareSpec,
    lock: ShareLock,
    guide_markdown: String,
    updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptDecision {
    Keep,
    Edit,
    Skip,
}

#[derive(Debug, Clone)]
struct CandidateRepo {
    abs_path: PathBuf,
    rel_path: String,
    url: String,
    branch: Option<String>,
    rev: String,
    evidence: Vec<String>,
}

#[derive(Debug, Clone)]
struct IgnoreMatcher {
    entries: Vec<String>,
}

impl IgnoreMatcher {
    fn load(root: &Path) -> Result<Self> {
        let path = root.join(".atoignore");
        if !path.exists() {
            return Ok(Self {
                entries: Vec::new(),
            });
        }
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let entries = raw
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|line| line.trim_end_matches('/').to_string())
            .collect();
        Ok(Self { entries })
    }

    fn matches(&self, root: &Path, candidate: &Path) -> bool {
        if self.entries.is_empty() {
            return false;
        }
        let relative = candidate
            .strip_prefix(root)
            .unwrap_or(candidate)
            .to_string_lossy()
            .replace('\\', "/");
        let basename = candidate
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();

        self.entries.iter().any(|entry| {
            relative == *entry || relative.starts_with(&format!("{entry}/")) || basename == entry
        })
    }
}

pub(crate) fn execute_encap(args: EncapArgs, reporter: Arc<CliReporter>) -> Result<()> {
    let root = args
        .path
        .canonicalize()
        .with_context(|| format!("Failed to resolve workspace root {}", args.path.display()))?;
    let capture = capture_workspace(&root)?;

    if args.print_plan {
        println!("{}", serde_json::to_string_pretty(&capture.spec)?);
        return Ok(());
    }

    let mut spec = capture.spec;
    interactively_finalize_capture(&mut spec, &reporter)?;
    let guide = generate_guide(&spec);
    let lock = build_share_lock(&spec, &capture.repo_locks, &capture.resolved_tools, &guide)?;
    let output = write_share_files(&root, &spec, &lock, &guide)?;

    futures::executor::block_on(reporter.notify(format!(
        "📦 Wrote share files:\n  {}\n  {}\n  {}",
        output.spec_path.display(),
        output.lock_path.display(),
        output.guide_path.display()
    )))?;

    if args.share && !args.save_only {
        match upload_share(&spec, &lock, &guide) {
            Ok(uploaded) => {
                futures::executor::block_on(reporter.notify(format!(
                    "🔗 Share URL: {}\n🔒 Revision URL: {}",
                    uploaded.share_url, uploaded.revision_url
                )))?;
            }
            Err(error) => {
                futures::executor::block_on(reporter.warn(format!(
                    "Local capture was saved, but share upload failed: {}",
                    error
                )))?;
            }
        }
    }

    Ok(())
}

pub(crate) fn execute_decap(args: DecapArgs, reporter: Arc<CliReporter>) -> Result<()> {
    let into = args.into;
    ensure_target_root_ready(&into)?;
    let loaded = load_share_input(&args.input)?;

    if args.plan {
        println!("{}", serde_json::to_string_pretty(&loaded.spec)?);
        return Ok(());
    }

    let mut state = WorkspaceShareState {
        share_url: loaded.share_url.clone(),
        resolved_revision_url: loaded.resolved_revision_url.clone(),
        workspace_root: into.display().to_string(),
        sources: Vec::new(),
        install_steps: Vec::new(),
        env: Vec::new(),
        verification: VerificationState {
            result: "pending".to_string(),
            issues: Vec::new(),
        },
        last_verified_at: None,
    };

    fs::create_dir_all(&into)
        .with_context(|| format!("Failed to create target root {}", into.display()))?;

    for source in &loaded.spec.sources {
        let locked = loaded
            .lock
            .resolved_sources
            .iter()
            .find(|entry| entry.id == source.id)
            .with_context(|| format!("Missing resolved source for {}", source.id))?;
        let source_path = into.join(&source.path);
        match materialize_source(source, locked, &source_path) {
            Ok(current_rev) => state.sources.push(ShareSourceState {
                id: source.id.clone(),
                status: "ok".to_string(),
                current_rev: Some(current_rev),
                last_error: None,
            }),
            Err(error) => {
                state.sources.push(ShareSourceState {
                    id: source.id.clone(),
                    status: "error".to_string(),
                    current_rev: None,
                    last_error: Some(error.to_string()),
                });
                state
                    .verification
                    .issues
                    .push(format!("source {} failed: {}", source.id, error));
            }
        }
    }

    let missing_tools = verify_tools(&loaded.lock.resolved_tools);
    for tool in missing_tools {
        state
            .verification
            .issues
            .push(format!("missing tool: {}", tool));
    }

    for step in &loaded.spec.install_steps {
        let started_at = Utc::now().to_rfc3339();
        let step_root = into.join(&step.cwd);
        match run_shell_command(&step.run, &step_root) {
            Ok(output) => state.install_steps.push(InstallStepState {
                id: step.id.clone(),
                status: "ok".to_string(),
                started_at: Some(started_at),
                finished_at: Some(Utc::now().to_rfc3339()),
                stdout_digest: Some(sha256_label(output.stdout.as_bytes())),
                stderr_digest: Some(sha256_label(output.stderr.as_bytes())),
                last_error: None,
            }),
            Err(error) => {
                state.install_steps.push(InstallStepState {
                    id: step.id.clone(),
                    status: "error".to_string(),
                    started_at: Some(started_at),
                    finished_at: Some(Utc::now().to_rfc3339()),
                    stdout_digest: None,
                    stderr_digest: None,
                    last_error: Some(error.to_string()),
                });
                state
                    .verification
                    .issues
                    .push(format!("install step {} failed: {}", step.id, error));
            }
        }
    }

    for env_requirement in &loaded.spec.env_requirements {
        let path = into.join(&env_requirement.path);
        let status = if path.exists() { "present" } else { "missing" };
        if status == "missing" && env_requirement.required {
            state
                .verification
                .issues
                .push(format!("missing env file: {}", env_requirement.path));
        }
        state.env.push(EnvState {
            id: env_requirement.id.clone(),
            status: status.to_string(),
        });
    }

    state.verification.result = if state.verification.issues.is_empty() {
        "ok".to_string()
    } else {
        "warning".to_string()
    };
    state.last_verified_at = Some(Utc::now().to_rfc3339());
    write_share_state(&into, &state)?;

    futures::executor::block_on(reporter.notify(build_decap_summary(&loaded.spec, &state)))?;

    Ok(())
}

fn capture_workspace(root: &Path) -> Result<CapturedWorkspace> {
    let ignore = IgnoreMatcher::load(root)?;
    let repos = discover_repositories(root, &ignore)?;
    let repo_locks = repos
        .iter()
        .map(|repo| ResolvedSourceLock {
            id: repo_id_from_path(&repo.rel_path),
            rev: repo.rev.clone(),
        })
        .collect::<Vec<_>>();
    let mut tool_requirements = BTreeMap::<String, ToolRequirementSpec>::new();
    let mut env_requirements = Vec::new();
    let mut install_steps = Vec::new();
    let mut services = Vec::new();

    for repo in &repos {
        let repo_scan_dirs = discover_repo_scan_dirs(&repo.abs_path)?;
        for scan_dir in repo_scan_dirs {
            let relative_dir = relative_display(root, &scan_dir);
            detect_tools(&scan_dir, &relative_dir, &mut tool_requirements)?;
            detect_env_requirements(&scan_dir, &relative_dir, &mut env_requirements)?;
            detect_install_steps(&scan_dir, &relative_dir, &mut install_steps)?;
            detect_services(&scan_dir, &relative_dir, &mut services)?;
        }
    }

    install_steps.sort_by(|left, right| left.id.cmp(&right.id));
    services.sort_by(|left, right| left.id.cmp(&right.id));
    env_requirements.sort_by(|left, right| left.path.cmp(&right.path));

    let spec = ShareSpec {
        schema_version: SHARE_SCHEMA_VERSION.to_string(),
        name: root
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("workspace")
            .to_string(),
        root: root
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("workspace")
            .to_string(),
        sources: repos
            .iter()
            .map(|repo| ShareSourceSpec {
                id: repo_id_from_path(&repo.rel_path),
                kind: "git".to_string(),
                url: repo.url.clone(),
                path: repo.rel_path.clone(),
                branch: repo.branch.clone(),
                evidence: repo.evidence.clone(),
            })
            .collect(),
        tool_requirements: tool_requirements.into_values().collect(),
        env_requirements,
        install_steps,
        services,
        notes: ShareNotes::default(),
        generated_from: GeneratedFrom {
            root_path: root.display().to_string(),
            captured_at: Utc::now().to_rfc3339(),
            host_os: std::env::consts::OS.to_string(),
        },
    };
    let resolved_tools = resolve_tools(&spec.tool_requirements);

    Ok(CapturedWorkspace {
        spec,
        repo_locks,
        resolved_tools,
    })
}

struct CapturedWorkspace {
    spec: ShareSpec,
    repo_locks: Vec<ResolvedSourceLock>,
    resolved_tools: Vec<ResolvedToolLock>,
}

fn discover_repositories(root: &Path, ignore: &IgnoreMatcher) -> Result<Vec<CandidateRepo>> {
    let mut candidates = Vec::new();
    let mut seen = BTreeSet::new();

    for candidate in std::iter::once(root.to_path_buf()).chain(
        fs::read_dir(root)
            .with_context(|| format!("Failed to read {}", root.display()))?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.is_dir()),
    ) {
        if ignore.matches(root, &candidate) {
            continue;
        }
        if let Some(repo) = load_repository_candidate(root, &candidate)? {
            if seen.insert(repo.rel_path.clone()) {
                candidates.push(repo);
            }
        }
    }

    candidates.sort_by(|left, right| left.rel_path.cmp(&right.rel_path));
    Ok(candidates)
}

fn load_repository_candidate(root: &Path, candidate: &Path) -> Result<Option<CandidateRepo>> {
    let Some(toplevel) = git_output(candidate, &["rev-parse", "--show-toplevel"])? else {
        return Ok(None);
    };
    let toplevel = PathBuf::from(toplevel.trim());
    let normalized_toplevel = toplevel.canonicalize().unwrap_or_else(|_| toplevel.clone());
    let normalized_candidate = candidate
        .canonicalize()
        .unwrap_or_else(|_| candidate.to_path_buf());
    if normalized_toplevel != normalized_candidate {
        return Ok(None);
    }
    let Some(url) = git_output(candidate, &["remote", "get-url", "origin"])? else {
        return Ok(None);
    };
    let Some(rev) = git_output(candidate, &["rev-parse", "HEAD"])? else {
        return Ok(None);
    };
    let branch = git_output(candidate, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let rel_path = relative_display(root, candidate);
    Ok(Some(CandidateRepo {
        abs_path: candidate.to_path_buf(),
        rel_path: if rel_path == "." {
            root.file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("workspace")
                .to_string()
        } else {
            rel_path
        },
        url: url.trim().to_string(),
        branch: branch
            .map(|value| value.trim().to_string())
            .filter(|value| value != "HEAD"),
        rev: rev.trim().to_string(),
        evidence: vec![
            format!("git remote origin: {}", url.trim()),
            format!("git rev-parse HEAD: {}", rev.trim()),
        ],
    }))
}

fn discover_repo_scan_dirs(repo_root: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs = vec![repo_root.to_path_buf()];
    for entry in fs::read_dir(repo_root)
        .with_context(|| format!("Failed to read {}", repo_root.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if matches!(
            name,
            ".git" | ".ato" | ".tmp" | "node_modules" | "target" | ".venv"
        ) {
            continue;
        }
        if contains_supported_manifest(&path)? {
            dirs.push(path);
        }
    }
    Ok(dirs)
}

fn contains_supported_manifest(dir: &Path) -> Result<bool> {
    Ok([
        "pyproject.toml",
        "uv.lock",
        "requirements.txt",
        "package.json",
        "bun.lock",
        "bun.lockb",
        "pnpm-lock.yaml",
        "package-lock.json",
        "Cargo.toml",
        "deno.json",
        "mint.json",
    ]
    .iter()
    .any(|file| dir.join(file).exists()))
}

fn detect_tools(
    scan_dir: &Path,
    relative_dir: &str,
    acc: &mut BTreeMap<String, ToolRequirementSpec>,
) -> Result<()> {
    let has_pyproject = scan_dir.join("pyproject.toml").exists();
    let has_uv_lock = scan_dir.join("uv.lock").exists();
    let has_requirements = scan_dir.join("requirements.txt").exists();
    let has_package_json = scan_dir.join("package.json").exists();
    let has_bun_lock = scan_dir.join("bun.lock").exists() || scan_dir.join("bun.lockb").exists();
    let has_pnpm_lock = scan_dir.join("pnpm-lock.yaml").exists();
    let has_package_lock = scan_dir.join("package-lock.json").exists();
    let has_cargo = scan_dir.join("Cargo.toml").exists();
    let has_deno = scan_dir.join("deno.json").exists();
    let has_mint = scan_dir.join("mint.json").exists();

    if has_pyproject || has_uv_lock || has_requirements {
        add_tool_requirement(
            acc,
            "python",
            None,
            relative_dir,
            if has_uv_lock || has_pyproject {
                "python project detected from pyproject.toml/uv.lock"
            } else {
                "python project detected from requirements.txt"
            },
        );
    }
    if has_pyproject || has_uv_lock || has_requirements {
        add_tool_requirement(
            acc,
            "uv",
            None,
            relative_dir,
            "uv-backed python setup inferred from pyproject.toml/uv.lock/requirements.txt",
        );
    }
    if has_bun_lock {
        add_tool_requirement(
            acc,
            "bun",
            None,
            relative_dir,
            "bun project detected from bun.lock/bun.lockb",
        );
        add_tool_requirement(
            acc,
            "node",
            None,
            relative_dir,
            "node runtime required by bun-based workspace",
        );
    } else if has_pnpm_lock {
        add_tool_requirement(
            acc,
            "pnpm",
            None,
            relative_dir,
            "pnpm project detected from pnpm-lock.yaml",
        );
        add_tool_requirement(
            acc,
            "node",
            None,
            relative_dir,
            "node runtime required by pnpm-based workspace",
        );
    } else if has_package_json || has_package_lock {
        add_tool_requirement(
            acc,
            "node",
            None,
            relative_dir,
            "node project detected from package.json/package-lock.json",
        );
        add_tool_requirement(
            acc,
            "npm",
            None,
            relative_dir,
            "npm install path inferred from package.json/package-lock.json",
        );
    }
    if has_cargo {
        add_tool_requirement(
            acc,
            "cargo",
            None,
            relative_dir,
            "cargo project detected from Cargo.toml",
        );
        add_tool_requirement(
            acc,
            "rustc",
            None,
            relative_dir,
            "rust toolchain required by Cargo.toml",
        );
    }
    if has_deno {
        add_tool_requirement(
            acc,
            "deno",
            None,
            relative_dir,
            "deno runtime detected from deno.json",
        );
    }
    if has_mint {
        add_tool_requirement(
            acc,
            "npx",
            None,
            relative_dir,
            "mintlify docs detected from mint.json",
        );
        add_tool_requirement(
            acc,
            "node",
            None,
            relative_dir,
            "node runtime required by mintlify docs",
        );
    }

    Ok(())
}

fn add_tool_requirement(
    acc: &mut BTreeMap<String, ToolRequirementSpec>,
    tool: &str,
    version: Option<String>,
    required_by: &str,
    evidence: &str,
) {
    let entry = acc
        .entry(tool.to_string())
        .or_insert_with(|| ToolRequirementSpec {
            id: tool.to_string(),
            tool: tool.to_string(),
            version,
            required_by: Vec::new(),
            evidence: Vec::new(),
        });
    if !entry.required_by.iter().any(|value| value == required_by) {
        entry.required_by.push(required_by.to_string());
    }
    if !entry.evidence.iter().any(|value| value == evidence) {
        entry.evidence.push(evidence.to_string());
    }
}

fn detect_env_requirements(
    scan_dir: &Path,
    relative_dir: &str,
    acc: &mut Vec<EnvRequirementSpec>,
) -> Result<()> {
    let candidates = [
        ".env",
        ".env.local",
        ".env.development",
        ".env.example",
        ".env.local.example",
        ".env.sample",
    ];
    let mut seen = BTreeSet::new();
    for name in candidates {
        let path = scan_dir.join(name);
        if !path.exists() {
            continue;
        }
        let relative_path = if relative_dir == "." {
            name.to_string()
        } else {
            format!("{relative_dir}/{name}")
        };
        if !seen.insert(relative_path.clone()) {
            continue;
        }
        let required = matches!(name, ".env" | ".env.local" | ".env.development");
        let template_path = if name.ends_with(".example") || name.ends_with(".sample") {
            None
        } else if scan_dir.join(format!("{name}.example")).exists() {
            Some(format!("{relative_path}.example"))
        } else {
            None
        };
        acc.push(EnvRequirementSpec {
            id: relative_path.replace('/', "-"),
            path: relative_path,
            required,
            template_path,
            note: if required {
                Some("Values are intentionally not captured.".to_string())
            } else {
                None
            },
            evidence: vec![format!("env file candidate detected at {}", path.display())],
        });
    }
    Ok(())
}

fn detect_install_steps(
    scan_dir: &Path,
    relative_dir: &str,
    acc: &mut Vec<InstallStepSpec>,
) -> Result<()> {
    if scan_dir.join("uv.lock").exists() || scan_dir.join("pyproject.toml").exists() {
        acc.push(InstallStepSpec {
            id: step_id(relative_dir, "install"),
            cwd: relative_dir.to_string(),
            run: "uv sync".to_string(),
            depends_on: Vec::new(),
            evidence: vec!["uv sync inferred from pyproject.toml/uv.lock".to_string()],
        });
        return Ok(());
    }

    if scan_dir.join("requirements.txt").exists() {
        acc.push(InstallStepSpec {
            id: step_id(relative_dir, "install"),
            cwd: relative_dir.to_string(),
            run: "uv venv && uv pip install -r requirements.txt".to_string(),
            depends_on: Vec::new(),
            evidence: vec!["requirements.txt inferred into uv venv install".to_string()],
        });
        return Ok(());
    }

    if scan_dir.join("bun.lock").exists() || scan_dir.join("bun.lockb").exists() {
        acc.push(InstallStepSpec {
            id: step_id(relative_dir, "install"),
            cwd: relative_dir.to_string(),
            run: "bun install".to_string(),
            depends_on: Vec::new(),
            evidence: vec!["bun install inferred from bun.lock/bun.lockb".to_string()],
        });
        return Ok(());
    }

    if scan_dir.join("pnpm-lock.yaml").exists() {
        acc.push(InstallStepSpec {
            id: step_id(relative_dir, "install"),
            cwd: relative_dir.to_string(),
            run: "pnpm install --frozen-lockfile".to_string(),
            depends_on: Vec::new(),
            evidence: vec!["pnpm install inferred from pnpm-lock.yaml".to_string()],
        });
        return Ok(());
    }

    if scan_dir.join("package-lock.json").exists() || scan_dir.join("package.json").exists() {
        acc.push(InstallStepSpec {
            id: step_id(relative_dir, "install"),
            cwd: relative_dir.to_string(),
            run: "npm ci".to_string(),
            depends_on: Vec::new(),
            evidence: vec!["npm ci inferred from package.json/package-lock.json".to_string()],
        });
        return Ok(());
    }

    if scan_dir.join("Cargo.toml").exists() {
        acc.push(InstallStepSpec {
            id: step_id(relative_dir, "install"),
            cwd: relative_dir.to_string(),
            run: "cargo build".to_string(),
            depends_on: Vec::new(),
            evidence: vec!["cargo build inferred from Cargo.toml".to_string()],
        });
    }
    Ok(())
}

fn detect_services(scan_dir: &Path, relative_dir: &str, acc: &mut Vec<ServiceSpec>) -> Result<()> {
    if scan_dir.join("package.json").exists() {
        let raw = fs::read_to_string(scan_dir.join("package.json"))
            .with_context(|| format!("Failed to read {}/package.json", scan_dir.display()))?;
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) {
            let scripts = json.get("scripts").and_then(|value| value.as_object());
            let package_manager = json
                .get("packageManager")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if let Some(scripts) = scripts {
                let runner = if package_manager.starts_with("bun@")
                    || scan_dir.join("bun.lock").exists()
                    || scan_dir.join("bun.lockb").exists()
                {
                    "bun run"
                } else {
                    "npm run"
                };
                for (script_name, optional) in [("dev", false), ("start", false)] {
                    if scripts.contains_key(script_name) {
                        acc.push(ServiceSpec {
                            id: step_id(relative_dir, script_name),
                            cwd: relative_dir.to_string(),
                            run: format!("{runner} {script_name}"),
                            depends_on: Vec::new(),
                            kind: "long_running".to_string(),
                            optional,
                            port: infer_port_hint(
                                scripts
                                    .get(script_name)
                                    .and_then(|value| value.as_str())
                                    .unwrap_or_default(),
                            ),
                            healthcheck: None,
                            evidence: vec![format!(
                                "{} script detected in package.json",
                                script_name
                            )],
                        });
                        break;
                    }
                }
            }
        }
    }

    for script in ["main.py", "app.py", "bot.py"] {
        if scan_dir.join(script).exists() {
            let run =
                if scan_dir.join("uv.lock").exists() || scan_dir.join("pyproject.toml").exists() {
                    format!("uv run {}", script)
                } else {
                    format!("python {}", script)
                };
            acc.push(ServiceSpec {
                id: step_id(relative_dir, script.trim_end_matches(".py")),
                cwd: relative_dir.to_string(),
                run,
                depends_on: Vec::new(),
                kind: "long_running".to_string(),
                optional: false,
                port: None,
                healthcheck: None,
                evidence: vec![format!("python entrypoint detected: {script}")],
            });
            break;
        }
    }

    if scan_dir.join("mint.json").exists() {
        acc.push(ServiceSpec {
            id: step_id(relative_dir, "docs"),
            cwd: relative_dir.to_string(),
            run: "npx mintlify dev".to_string(),
            depends_on: Vec::new(),
            kind: "long_running".to_string(),
            optional: true,
            port: None,
            healthcheck: None,
            evidence: vec!["mintlify docs detected from mint.json".to_string()],
        });
    }

    Ok(())
}

fn infer_port_hint(script: &str) -> Option<u16> {
    for token in script.split_whitespace() {
        if let Some(port) = token.strip_prefix("--port=") {
            if let Ok(parsed) = port.parse::<u16>() {
                return Some(parsed);
            }
        }
    }
    None
}

fn interactively_finalize_capture(spec: &mut ShareSpec, reporter: &Arc<CliReporter>) -> Result<()> {
    let use_tui = progressive_ui::can_use_progressive_ui(reporter.is_json());
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        anyhow::bail!(
            "`ato encap` requires an interactive TTY so detected steps can be confirmed."
        );
    }

    futures::executor::block_on(reporter.notify(format!(
        "Detected workspace `{}` with {} repos, {} tools, {} install steps, {} services, {} env files.",
        spec.name,
        spec.sources.len(),
        spec.tool_requirements.len(),
        spec.install_steps.len(),
        spec.services.len(),
        spec.env_requirements.len()
    )))?;

    let mut kept_sources = Vec::new();
    for source in &spec.sources {
        if progressive_ui::confirm_with_fallback(
            &format!("Include source {} -> {}? [Y/n] ", source.path, source.url),
            true,
            use_tui,
        )? {
            kept_sources.push(source.clone());
        }
    }
    spec.sources = kept_sources;

    let mut kept_tools = Vec::new();
    for tool in &spec.tool_requirements {
        if progressive_ui::confirm_with_fallback(
            &format!("Include tool requirement {}? [Y/n] ", tool.tool),
            true,
            use_tui,
        )? {
            kept_tools.push(tool.clone());
        }
    }
    spec.tool_requirements = kept_tools;

    let mut kept_env = Vec::new();
    for env_requirement in &spec.env_requirements {
        if progressive_ui::confirm_with_fallback(
            &format!("Include env requirement {}? [Y/n] ", env_requirement.path),
            true,
            use_tui,
        )? {
            let mut entry = env_requirement.clone();
            entry.required = progressive_ui::confirm_with_fallback(
                &format!("Mark {} as required? [Y/n] ", env_requirement.path),
                env_requirement.required,
                use_tui,
            )?;
            kept_env.push(entry);
        }
    }
    spec.env_requirements = kept_env;

    let mut kept_steps = Vec::new();
    for step in &spec.install_steps {
        match prompt_editable_entry(
            &format!("Install step {} -> ({}) {}", step.id, step.cwd, step.run),
            use_tui,
        )? {
            PromptDecision::Keep => kept_steps.push(step.clone()),
            PromptDecision::Edit => {
                let mut updated = step.clone();
                updated.cwd = prompt_text("New cwd (blank keeps current): ", &step.cwd)?;
                updated.run = prompt_text("New command (blank keeps current): ", &step.run)?;
                kept_steps.push(updated);
            }
            PromptDecision::Skip => {}
        }
    }
    spec.install_steps = kept_steps;

    let mut kept_services = Vec::new();
    for service in &spec.services {
        match prompt_editable_entry(
            &format!(
                "Service {} -> ({}) {}",
                service.id, service.cwd, service.run
            ),
            use_tui,
        )? {
            PromptDecision::Keep => kept_services.push(service.clone()),
            PromptDecision::Edit => {
                let mut updated = service.clone();
                updated.cwd = prompt_text("New service cwd (blank keeps current): ", &service.cwd)?;
                updated.run =
                    prompt_text("New service command (blank keeps current): ", &service.run)?;
                let optional_input =
                    prompt_optional_text("Port override (blank keeps current / 'none' clears): ")?;
                if let Some(port_input) = optional_input {
                    updated.port =
                        if port_input.eq_ignore_ascii_case("none") {
                            None
                        } else {
                            Some(port_input.parse::<u16>().with_context(|| {
                                format!("Invalid port override: {}", port_input)
                            })?)
                        };
                }
                kept_services.push(updated);
            }
            PromptDecision::Skip => {}
        }
    }
    spec.services = kept_services;

    let maybe_name = prompt_optional_text(&format!("Workspace name [{}]: ", spec.name))?;
    if let Some(name) = maybe_name.filter(|value| !value.is_empty()) {
        spec.name = name;
    }

    Ok(())
}

fn prompt_editable_entry(label: &str, use_tui: bool) -> Result<PromptDecision> {
    if use_tui {
        eprint!("{label} [Y/e/n] ");
    } else {
        eprint!("{label} [Y/e/n] ");
    }
    io::stderr()
        .flush()
        .context("failed to flush editable prompt")?;
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("failed to read editable prompt")?;
    let trimmed = input.trim().to_ascii_lowercase();
    Ok(match trimmed.as_str() {
        "" | "y" | "yes" => PromptDecision::Keep,
        "e" | "edit" => PromptDecision::Edit,
        "n" | "no" | "skip" => PromptDecision::Skip,
        _ => PromptDecision::Keep,
    })
}

fn prompt_text(prompt: &str, current: &str) -> Result<String> {
    eprint!("{prompt}");
    io::stderr()
        .flush()
        .context("failed to flush text prompt")?;
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("failed to read text prompt")?;
    let trimmed = input.trim();
    if trimmed.is_empty() {
        Ok(current.to_string())
    } else {
        Ok(trimmed.to_string())
    }
}

fn prompt_optional_text(prompt: &str) -> Result<Option<String>> {
    eprint!("{prompt}");
    io::stderr()
        .flush()
        .context("failed to flush optional text prompt")?;
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("failed to read optional text prompt")?;
    let trimmed = input.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

fn build_share_lock(
    spec: &ShareSpec,
    resolved_sources: &[ResolvedSourceLock],
    resolved_tools: &[ResolvedToolLock],
    guide: &str,
) -> Result<ShareLock> {
    let spec_raw = serde_json::to_vec(spec).context("Failed to serialize share spec")?;
    Ok(ShareLock {
        schema_version: SHARE_SCHEMA_VERSION.to_string(),
        spec_digest: sha256_label(&spec_raw),
        generated_guide_digest: sha256_label(guide.as_bytes()),
        revision: 1,
        created_at: Utc::now().to_rfc3339(),
        resolved_sources: resolved_sources.to_vec(),
        resolved_tools: resolved_tools.to_vec(),
    })
}

struct ShareFileOutput {
    spec_path: PathBuf,
    lock_path: PathBuf,
    guide_path: PathBuf,
}

fn write_share_files(
    root: &Path,
    spec: &ShareSpec,
    lock: &ShareLock,
    guide: &str,
) -> Result<ShareFileOutput> {
    let share_dir = root.join(SHARE_DIR);
    fs::create_dir_all(&share_dir)
        .with_context(|| format!("Failed to create {}", share_dir.display()))?;
    let spec_path = share_dir.join(SHARE_SPEC_FILE);
    let lock_path = share_dir.join(SHARE_LOCK_FILE);
    let guide_path = share_dir.join(SHARE_GUIDE_FILE);
    fs::write(&spec_path, serde_json::to_string_pretty(spec)?)
        .with_context(|| format!("Failed to write {}", spec_path.display()))?;
    fs::write(&lock_path, serde_json::to_string_pretty(lock)?)
        .with_context(|| format!("Failed to write {}", lock_path.display()))?;
    fs::write(&guide_path, guide)
        .with_context(|| format!("Failed to write {}", guide_path.display()))?;
    Ok(ShareFileOutput {
        spec_path,
        lock_path,
        guide_path,
    })
}

fn generate_guide(spec: &ShareSpec) -> String {
    let mut lines = Vec::new();
    lines.push(format!("# {}", spec.name));
    lines.push(String::new());
    lines.push("## Workspace".to_string());
    lines.push(format!("- Root: `{}`", spec.root));
    lines.push(String::new());
    lines.push("## Repositories".to_string());
    if spec.sources.is_empty() {
        lines.push("- None detected".to_string());
    } else {
        for source in &spec.sources {
            lines.push(format!("- `{}` <- {}", source.path, source.url));
        }
    }
    lines.push(String::new());
    lines.push("## Required Tools".to_string());
    if spec.tool_requirements.is_empty() {
        lines.push("- None detected".to_string());
    } else {
        for tool in &spec.tool_requirements {
            lines.push(format!("- `{}`", tool.tool));
        }
    }
    lines.push(String::new());
    lines.push("## Environment Files".to_string());
    if spec.env_requirements.is_empty() {
        lines.push("- None detected".to_string());
    } else {
        for env_requirement in &spec.env_requirements {
            lines.push(format!(
                "- `{}`{}",
                env_requirement.path,
                if env_requirement.required {
                    " (required)"
                } else {
                    ""
                }
            ));
        }
    }
    lines.push(String::new());
    lines.push("## Install Steps".to_string());
    if spec.install_steps.is_empty() {
        lines.push("- None detected".to_string());
    } else {
        for step in &spec.install_steps {
            lines.push(format!("- `{}`: `{}` in `{}`", step.id, step.run, step.cwd));
        }
    }
    lines.push(String::new());
    lines.push("## Run Services".to_string());
    if spec.services.is_empty() {
        lines.push("- None detected".to_string());
    } else {
        for service in &spec.services {
            let mut suffix = String::new();
            if service.optional {
                suffix.push_str(" (optional)");
            }
            if let Some(port) = service.port {
                suffix.push_str(&format!(" port={}", port));
            }
            lines.push(format!(
                "- `{}`: `{}` in `{}`{}",
                service.id, service.run, service.cwd, suffix
            ));
        }
    }
    lines.push(String::new());
    lines.push("## Troubleshooting".to_string());
    lines.push(String::new());
    lines.push("_Add project-specific troubleshooting notes here._".to_string());
    lines.push(String::new());
    lines.push("## Team Notes".to_string());
    lines.push(String::new());
    if spec.notes.team_notes.trim().is_empty() {
        lines.push("_Add team notes here._".to_string());
    } else {
        lines.push(spec.notes.team_notes.clone());
    }
    lines.join("\n")
}

fn upload_share(spec: &ShareSpec, lock: &ShareLock, guide: &str) -> Result<ShareRevisionPayload> {
    let token = auth::require_session_token()?;
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(DEFAULT_API_TIMEOUT_SECS))
        .build()
        .context("Failed to build share upload HTTP client")?;
    let response = client
        .post(format!("{}/v1/shares", auth::default_store_registry_url()))
        .bearer_auth(token)
        .header("Accept", "application/json")
        .json(&ShareApiCreateRequest {
            title: spec.name.clone(),
            visibility: "unlisted".to_string(),
            spec: spec.clone(),
            lock: lock.clone(),
            guide_markdown: guide.to_string(),
        })
        .send()
        .context("Failed to upload share")?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().unwrap_or_default();
        anyhow::bail!("share upload failed ({}): {}", status, body);
    }
    let body = response
        .json::<serde_json::Value>()
        .context("Failed to parse share upload response")?;
    serde_json::from_value(body["share"].clone()).context("Invalid share upload response payload")
}

struct LoadedShareInput {
    share_url: Option<String>,
    resolved_revision_url: Option<String>,
    spec: ShareSpec,
    lock: ShareLock,
}

fn load_share_input(input: &str) -> Result<LoadedShareInput> {
    if input.starts_with("http://") || input.starts_with("https://") {
        return fetch_share_url(input);
    }

    let path = PathBuf::from(input);
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read share input {}", path.display()))?;
    if path
        .file_name()
        .and_then(|value| value.to_str())
        .map(|value| value == SHARE_LOCK_FILE)
        .unwrap_or(false)
    {
        let lock = serde_json::from_str::<ShareLock>(&raw)
            .with_context(|| format!("Failed to parse {}", path.display()))?;
        let spec_path = path
            .parent()
            .map(|parent| parent.join(SHARE_SPEC_FILE))
            .context("share.lock.json has no parent directory")?;
        let spec = serde_json::from_str::<ShareSpec>(
            &fs::read_to_string(&spec_path)
                .with_context(|| format!("Failed to read {}", spec_path.display()))?,
        )
        .with_context(|| format!("Failed to parse {}", spec_path.display()))?;
        return Ok(LoadedShareInput {
            share_url: None,
            resolved_revision_url: None,
            spec,
            lock,
        });
    }

    let spec = serde_json::from_str::<ShareSpec>(&raw)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    let lock_path = path
        .parent()
        .map(|parent| parent.join(SHARE_LOCK_FILE))
        .context("share.spec.json has no parent directory")?;
    let lock = serde_json::from_str::<ShareLock>(
        &fs::read_to_string(&lock_path)
            .with_context(|| format!("Failed to read {}", lock_path.display()))?,
    )
    .with_context(|| format!("Failed to parse {}", lock_path.display()))?;
    Ok(LoadedShareInput {
        share_url: None,
        resolved_revision_url: None,
        spec,
        lock,
    })
}

fn fetch_share_url(url: &str) -> Result<LoadedShareInput> {
    let parsed = reqwest::Url::parse(url).context("Invalid share URL")?;
    let segment = parsed
        .path_segments()
        .and_then(|segments| segments.last())
        .filter(|segment| !segment.is_empty())
        .context("Share URL is missing an id")?;
    let (share_id, revision) = parse_share_revision_segment(segment)?;
    let base = format!(
        "{}://{}",
        parsed.scheme(),
        parsed.host_str().unwrap_or("ato.run")
    );
    let endpoint = if let Some(revision) = revision {
        format!("{}/v1/shares/{}/revisions/{}", base, share_id, revision)
    } else {
        format!("{}/v1/shares/{}", base, share_id)
    };
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(DEFAULT_API_TIMEOUT_SECS))
        .build()
        .context("Failed to build share fetch client")?;
    let response = client
        .get(&endpoint)
        .header("Accept", "application/json")
        .send()
        .context("Failed to fetch share URL")?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().unwrap_or_default();
        anyhow::bail!("share fetch failed ({}): {}", status, body);
    }
    let body = response
        .json::<serde_json::Value>()
        .context("Failed to parse share response")?;
    let share = serde_json::from_value::<ShareRevisionPayload>(body["share"].clone())
        .context("Invalid share response payload")?;
    Ok(LoadedShareInput {
        share_url: Some(share.share_url.clone()),
        resolved_revision_url: Some(share.revision_url.clone()),
        spec: share.spec,
        lock: share.lock,
    })
}

fn parse_share_revision_segment(segment: &str) -> Result<(&str, Option<u32>)> {
    if let Some((share_id, revision)) = segment.rsplit_once("@r") {
        return Ok((
            share_id,
            Some(
                revision
                    .parse::<u32>()
                    .with_context(|| format!("Invalid share revision: {}", revision))?,
            ),
        ));
    }
    Ok((segment, None))
}

fn ensure_target_root_ready(target: &Path) -> Result<()> {
    if !target.exists() {
        return Ok(());
    }
    let mut read_dir =
        fs::read_dir(target).with_context(|| format!("Failed to inspect {}", target.display()))?;
    if read_dir.next().is_some() {
        anyhow::bail!(
            "`ato decap` requires an empty target directory. Refusing to overwrite {}",
            target.display()
        );
    }
    Ok(())
}

fn materialize_source(
    source: &ShareSourceSpec,
    locked: &ResolvedSourceLock,
    target: &Path,
) -> Result<String> {
    if !target.exists() {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }
        run_git(
            None,
            &[
                "clone",
                "--origin",
                "origin",
                &source.url,
                &target.display().to_string(),
            ],
        )?;
    } else {
        run_git(Some(target), &["fetch", "--all", "--tags"])?;
    }
    run_git(Some(target), &["checkout", "--force", &locked.rev])?;
    let current_rev = git_output(target, &["rev-parse", "HEAD"])?
        .unwrap_or_else(|| locked.rev.clone())
        .trim()
        .to_string();
    Ok(current_rev)
}

fn verify_tools(resolved_tools: &[ResolvedToolLock]) -> Vec<String> {
    resolved_tools
        .iter()
        .filter(|tool| tool.binary_path.is_none())
        .map(|tool| tool.tool.clone())
        .collect()
}

struct ShellOutput {
    stdout: String,
    stderr: String,
}

fn run_shell_command(command: &str, cwd: &Path) -> Result<ShellOutput> {
    let output = if cfg!(windows) {
        Command::new("cmd")
            .arg("/C")
            .arg(command)
            .current_dir(cwd)
            .output()
    } else {
        Command::new("/bin/sh")
            .arg("-lc")
            .arg(command)
            .current_dir(cwd)
            .output()
    }
    .with_context(|| format!("Failed to launch shell command in {}", cwd.display()))?;
    if output.status.success() {
        Ok(ShellOutput {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    } else {
        anyhow::bail!(
            "command failed in {}: {}",
            cwd.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn write_share_state(root: &Path, state: &WorkspaceShareState) -> Result<()> {
    let share_dir = root.join(SHARE_DIR);
    fs::create_dir_all(&share_dir)
        .with_context(|| format!("Failed to create {}", share_dir.display()))?;
    let path = share_dir.join(SHARE_STATE_FILE);
    fs::write(&path, serde_json::to_string_pretty(state)?)
        .with_context(|| format!("Failed to write {}", path.display()))
}

fn build_decap_summary(spec: &ShareSpec, state: &WorkspaceShareState) -> String {
    let mut lines = vec![
        format!("Workspace ready at {}", state.workspace_root),
        format!("Sources: {}", state.sources.len()),
        format!("Install steps: {}", state.install_steps.len()),
        format!("Env files: {}", state.env.len()),
    ];
    if state.verification.issues.is_empty() {
        lines.push("Verification: ok".to_string());
    } else {
        lines.push("Verification issues:".to_string());
        for issue in &state.verification.issues {
            lines.push(format!("  - {}", issue));
        }
    }
    lines.push("Next:".to_string());
    for service in &spec.services {
        lines.push(format!(
            "  - {} -> {} ({})",
            service.id, service.run, service.cwd
        ));
    }
    lines.join("\n")
}

fn resolve_tools(requirements: &[ToolRequirementSpec]) -> Vec<ResolvedToolLock> {
    requirements
        .iter()
        .map(|tool| {
            let binary = binary_name_for_tool(&tool.tool);
            let binary_path = which::which(binary)
                .ok()
                .map(|path| path.display().to_string());
            let resolved_version = binary_path
                .as_ref()
                .and_then(|_| command_version(binary).ok().flatten());
            ResolvedToolLock {
                tool: tool.tool.clone(),
                resolved_version,
                binary_path,
            }
        })
        .collect()
}

fn binary_name_for_tool(tool: &str) -> &str {
    match tool {
        "python" => "python3",
        "rustc" => "rustc",
        other => other,
    }
}

fn command_version(binary: &str) -> Result<Option<String>> {
    let output = Command::new(binary).arg("--version").output();
    match output {
        Ok(output) if output.status.success() => Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
        )),
        Ok(_) => Ok(None),
        Err(_) => Ok(None),
    }
}

fn git_output(dir: &Path, args: &[&str]) -> Result<Option<String>> {
    let output = Command::new("git").args(args).current_dir(dir).output();
    match output {
        Ok(output) if output.status.success() => Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
        )),
        Ok(_) => Ok(None),
        Err(_) => Ok(None),
    }
}

fn run_git(dir: Option<&Path>, args: &[&str]) -> Result<()> {
    let mut command = Command::new("git");
    command.args(args);
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    let output = command.output().context("failed to launch git command")?;
    if output.status.success() {
        Ok(())
    } else {
        anyhow::bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
        .if_empty_then(".")
}

trait IfEmptyThen {
    fn if_empty_then(self, fallback: &str) -> String;
}

impl IfEmptyThen for String {
    fn if_empty_then(self, fallback: &str) -> String {
        if self.is_empty() {
            fallback.to_string()
        } else {
            self
        }
    }
}

fn step_id(relative_dir: &str, suffix: &str) -> String {
    format!(
        "{}-{}",
        repo_id_from_path(relative_dir),
        suffix.replace('.', "-")
    )
}

fn repo_id_from_path(path: &str) -> String {
    path.replace(['/', '.'], "-")
}

fn sha256_label(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_share_revision_segment_supports_mutable_and_immutable() {
        let (share_id, revision) = parse_share_revision_segment("abc123").expect("mutable");
        assert_eq!(share_id, "abc123");
        assert_eq!(revision, None);

        let (share_id, revision) = parse_share_revision_segment("abc123@r7").expect("immutable");
        assert_eq!(share_id, "abc123");
        assert_eq!(revision, Some(7));
    }

    #[test]
    fn ensure_target_root_ready_rejects_non_empty_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("hello.txt"), "hello").expect("write");
        let error = ensure_target_root_ready(temp.path()).expect_err("must reject non-empty");
        assert!(error.to_string().contains("empty target directory"));
    }

    #[test]
    fn capture_workspace_detects_sources_steps_services_and_env() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        let agent = root.join("agent");
        fs::create_dir_all(agent.join("server")).expect("mkdir");
        fs::write(
            agent.join("server/pyproject.toml"),
            "[project]\nname='agent'\n",
        )
        .expect("pyproject");
        fs::write(agent.join("server/.env"), "SECRET=hidden\n").expect("env");
        init_git_repo(&agent, "git@github.com:acme/agent.git");

        let web = root.join("dashboard");
        fs::create_dir_all(&web).expect("mkdir");
        fs::write(
            web.join("package.json"),
            r#"{"name":"dashboard","packageManager":"bun@1.1.0","scripts":{"dev":"vite --port=5173"}}"#,
        )
        .expect("package.json");
        fs::write(web.join("bun.lock"), "").expect("bun.lock");
        init_git_repo(&web, "git@github.com:acme/dashboard.git");

        let capture = capture_workspace(root).expect("capture");
        assert_eq!(capture.spec.sources.len(), 2);
        assert!(capture
            .spec
            .install_steps
            .iter()
            .any(|step| step.run == "uv sync"));
        assert!(capture
            .spec
            .install_steps
            .iter()
            .any(|step| step.run == "bun install"));
        assert!(capture
            .spec
            .services
            .iter()
            .any(|service| service.run.contains("bun run dev")));
        assert!(capture
            .spec
            .env_requirements
            .iter()
            .all(|env| !env.evidence.iter().any(|line| line.contains("SECRET="))));
    }

    fn init_git_repo(path: &Path, remote: &str) {
        Command::new("git")
            .arg("init")
            .current_dir(path)
            .output()
            .expect("git init");
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(path)
            .output()
            .expect("git config email");
        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(path)
            .output()
            .expect("git config name");
        Command::new("git")
            .args(["remote", "add", "origin", remote])
            .current_dir(path)
            .output()
            .expect("git remote add");
        fs::write(path.join("README.md"), "# demo\n").expect("write readme");
        Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .output()
            .expect("git add");
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(path)
            .output()
            .expect("git commit");
    }
}
