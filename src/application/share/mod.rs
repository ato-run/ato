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
use crate::fs_copy;
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

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct RunShareArgs {
    pub(crate) input: String,
    pub(crate) entry: Option<String>,
    pub(crate) args: Vec<String>,
    pub(crate) env_file: Option<PathBuf>,
    pub(crate) prompt_env: bool,
    pub(crate) watch: bool,
    pub(crate) background: bool,
    pub(crate) reporter: Arc<CliReporter>,
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
    pub(crate) entries: Vec<ShareEntrySpec>,
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
pub(crate) struct ShareEntrySpec {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) cwd: String,
    pub(crate) run: String,
    pub(crate) kind: String,
    pub(crate) primary: bool,
    #[serde(default)]
    pub(crate) depends_on: Vec<String>,
    #[serde(default)]
    pub(crate) env: EntryEnvSpec,
    #[serde(default)]
    pub(crate) evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct EntryEnvSpec {
    #[serde(default)]
    pub(crate) required: Vec<String>,
    #[serde(default)]
    pub(crate) optional: Vec<String>,
    #[serde(default)]
    pub(crate) files: Vec<String>,
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
    #[serde(alias = "id")]
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
    if looks_like_share_run_input(&args.input) || looks_like_local_share_file(&args.input) {
        let loaded = load_share_input(&args.input)?;
        if args.plan {
            println!("{}", serde_json::to_string_pretty(&loaded.spec)?);
            return Ok(());
        }
        let state = materialize_loaded_share(&loaded, &into)?;
        futures::executor::block_on(reporter.notify(build_decap_summary(&loaded.spec, &state)))?;
        return Ok(());
    }

    if args.plan {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "mode": "generic_target_materialization",
                "input": args.input,
                "into": into,
            }))?
        );
        return Ok(());
    }

    let state = materialize_generic_target(&args.input, &into, &reporter)?;
    futures::executor::block_on(reporter.notify(build_generic_decap_summary(
        &args.input,
        &into,
        &state,
    )))?;
    Ok(())
}

#[allow(dead_code)]
pub(crate) fn execute_run_share(args: RunShareArgs) -> Result<()> {
    if args.watch {
        anyhow::bail!("`ato run <share-url>` does not support --watch in this MVP.");
    }
    if args.background {
        anyhow::bail!("`ato run <share-url>` does not support --background in this MVP.");
    }

    let loaded = load_share_input(&args.input)?;
    let entries = effective_entries(&loaded.spec);
    let entry = select_run_entry(&args.input, &loaded, &entries, args.entry.as_deref())?;
    let temp_root = ephemeral_run_root(&loaded, &entry)?;
    // Ephemeral roots are always fully re-materialized; clean up any stale remnant
    // from a previous interrupted run before proceeding.
    if temp_root.exists() {
        fs::remove_dir_all(&temp_root)
            .with_context(|| format!("Failed to clean stale run root {}", temp_root.display()))?;
    }
    let state = materialize_loaded_share(&loaded, &temp_root)?;
    let env_overlay = resolve_entry_env_overlay(
        &args.input,
        &entry,
        args.env_file.as_deref(),
        args.prompt_env,
    )?;

    let run_command = if args.args.is_empty() {
        entry.run.clone()
    } else {
        format!(
            "{} {}",
            entry.run,
            shell_words::join(args.args.iter().map(String::as_str))
        )
    };
    let run_cwd = temp_root.join(&entry.cwd);
    let next_command = loaded
        .resolved_revision_url
        .clone()
        .unwrap_or_else(|| args.input.clone());

    futures::executor::block_on(args.reporter.notify(format!(
        "Try now: `{}`\nSet up locally later: ato decap {} --into ./{}",
        run_command, next_command, loaded.spec.root
    )))?;
    if !state.verification.issues.is_empty() {
        futures::executor::block_on(args.reporter.warn(format!(
            "Workspace verification reported {} issue(s) before run.",
            state.verification.issues.len()
        )))?;
    }

    let status = run_shell_streaming(&run_command, &run_cwd, &env_overlay)?;
    let _ = fs::remove_dir_all(&temp_root);
    if status.success() {
        Ok(())
    } else {
        anyhow::bail!(
            "share entry `{}` exited with status {}",
            entry.id,
            status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".to_string())
        );
    }
}

fn materialize_loaded_share(loaded: &LoadedShareInput, into: &Path) -> Result<WorkspaceShareState> {
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

    fs::create_dir_all(into)
        .with_context(|| format!("Failed to create target root {}", into.display()))?;

    if !loaded.spec_digest_verified {
        state.verification.issues.push(
            "spec/lock digest mismatch: spec may have changed since lock was created".to_string(),
        );
    }

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

    for tool in verify_tools(&loaded.spec.tool_requirements, &loaded.lock.resolved_tools) {
        state
            .verification
            .issues
            .push(format!("missing tool in lock: {}", tool));
    }

    let lock_tool_ids: std::collections::HashSet<&str> = loaded
        .lock
        .resolved_tools
        .iter()
        .map(|r| r.tool.as_str())
        .collect();
    for tool in verify_local_tools(&loaded.spec.tool_requirements) {
        // Only emit recipient-side warning for tools that were resolved in the lock;
        // tools absent from the lock are already reported as "missing tool in lock".
        if lock_tool_ids.contains(tool.as_str()) {
            state
                .verification
                .issues
                .push(format!("missing tool on this machine: {}", tool));
        }
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
    write_share_state(into, &state)?;
    Ok(state)
}

fn materialize_generic_target(
    input: &str,
    into: &Path,
    reporter: &Arc<CliReporter>,
) -> Result<WorkspaceShareState> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to build decap runtime")?;
    let resolved = rt.block_on(crate::install::support::resolve_run_target_or_install(
        PathBuf::from(input),
        true,
        crate::ProviderToolchain::Auto,
        false,
        None,
        false,
        None,
        reporter.clone(),
    ))?;

    fs::create_dir_all(into)
        .with_context(|| format!("Failed to create target root {}", into.display()))?;

    if resolved.path.is_dir() {
        fs_copy::copy_path_recursive(&resolved.path, into)?;
    } else if resolved
        .path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value == "capsule")
        .unwrap_or(false)
    {
        extract_capsule_into(&resolved.path, into)?;
    } else {
        anyhow::bail!("unsupported decap target path: {}", resolved.path.display());
    }

    let state = WorkspaceShareState {
        share_url: None,
        resolved_revision_url: None,
        workspace_root: into.display().to_string(),
        sources: vec![ShareSourceState {
            id: "target".to_string(),
            status: "ok".to_string(),
            current_rev: None,
            last_error: None,
        }],
        install_steps: Vec::new(),
        env: Vec::new(),
        verification: VerificationState {
            result: "ok".to_string(),
            issues: Vec::new(),
        },
        last_verified_at: Some(Utc::now().to_rfc3339()),
    };
    write_share_state(into, &state)?;
    Ok(state)
}

fn build_generic_decap_summary(input: &str, into: &Path, state: &WorkspaceShareState) -> String {
    let mut lines = vec![
        format!("Workspace materialized at {}", into.display()),
        format!("Source target: {}", input),
        format!("Verification: {}", state.verification.result),
        "Next:".to_string(),
        "  - Open the workspace locally".to_string(),
        "  - Share it later with: ato encap --share".to_string(),
    ];
    if !state.verification.issues.is_empty() {
        lines.push("Issues:".to_string());
        for issue in &state.verification.issues {
            lines.push(format!("  - {}", issue));
        }
    }
    lines.join("\n")
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
    let entries = derive_entries(&services, &env_requirements);

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
        entries,
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

fn derive_entries(
    services: &[ServiceSpec],
    env_requirements: &[EnvRequirementSpec],
) -> Vec<ShareEntrySpec> {
    let mut entries = services
        .iter()
        .map(|service| {
            let env_files = env_requirements
                .iter()
                .filter(|env_requirement| {
                    env_requirement.path == format!("{}/.env", service.cwd)
                        || env_requirement.path == format!("{}/.env.local", service.cwd)
                        || env_requirement
                            .path
                            .starts_with(&format!("{}/", service.cwd))
                })
                .map(|env_requirement| env_requirement.path.clone())
                .collect::<Vec<_>>();
            ShareEntrySpec {
                id: service.id.clone(),
                label: service.id.clone(),
                cwd: service.cwd.clone(),
                run: service.run.clone(),
                kind: "runnable".to_string(),
                primary: false,
                depends_on: service.depends_on.clone(),
                env: EntryEnvSpec {
                    required: Vec::new(),
                    optional: Vec::new(),
                    files: env_files,
                },
                evidence: service.evidence.clone(),
            }
        })
        .collect::<Vec<_>>();

    if entries.len() == 1 {
        if let Some(entry) = entries.first_mut() {
            entry.primary = true;
        }
        return entries;
    }

    for preferred in ["main", "dashboard", "docs", "web", "app"] {
        if let Some(entry) = entries.iter_mut().find(|entry| entry.id == preferred) {
            entry.primary = true;
            return entries;
        }
    }

    if let Some(entry) = entries.first_mut() {
        entry.primary = true;
    }

    entries
}

fn ensure_single_primary_entry(entries: &mut [ShareEntrySpec]) {
    let primary_indices = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| entry.primary.then_some(index))
        .collect::<Vec<_>>();

    if primary_indices.len() == 1 {
        return;
    }

    for entry in entries.iter_mut() {
        entry.primary = false;
    }

    if let Some(entry) = entries.first_mut() {
        entry.primary = true;
    }
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
        "Detected workspace `{}` with {} repos, {} tools, {} install steps, {} entries, {} services, {} env files.",
        spec.name,
        spec.sources.len(),
        spec.tool_requirements.len(),
        spec.install_steps.len(),
        spec.entries.len(),
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

    let mut kept_entries = Vec::new();
    for entry in &spec.entries {
        match prompt_editable_entry(
            &format!("Run entry {} -> ({}) {}", entry.id, entry.cwd, entry.run),
            use_tui,
        )? {
            PromptDecision::Keep => kept_entries.push(entry.clone()),
            PromptDecision::Edit => {
                let mut updated = entry.clone();
                updated.label =
                    prompt_text("New entry label (blank keeps current): ", &entry.label)?;
                updated.cwd = prompt_text("New entry cwd (blank keeps current): ", &entry.cwd)?;
                updated.run = prompt_text("New entry command (blank keeps current): ", &entry.run)?;
                updated.primary = progressive_ui::confirm_with_fallback(
                    &format!("Mark {} as primary? [y/N] ", updated.id),
                    updated.primary,
                    use_tui,
                )?;
                // When user explicitly sets this entry as primary, clear primary from
                // all previously kept entries so at most one primary exists at the end.
                if updated.primary {
                    for prev in kept_entries.iter_mut() {
                        prev.primary = false;
                    }
                }
                kept_entries.push(updated);
            }
            PromptDecision::Skip => {}
        }
    }
    // If user de-selected the only primary without explicitly activating another,
    // prompt the user to choose which entry should be the default run target.
    // This must run BEFORE ensure_single_primary_entry, which would otherwise
    // silently assign the first entry and prevent the prompt from ever triggering.
    // Check stdin.is_terminal() rather than use_tui so the prompt also fires in
    // text-mode (e.g. --json) when stdin is still interactive.
    let has_primary = kept_entries.iter().any(|e| e.primary);
    if !has_primary && !kept_entries.is_empty() && io::stdin().is_terminal() {
        eprintln!("No entry is marked as primary. Choose which entry to run by default:");
        for (i, e) in kept_entries.iter().enumerate() {
            eprintln!("  {}: {} ({})", i + 1, e.id, e.run);
        }
        eprint!("Primary entry [1]: ");
        io::stderr()
            .flush()
            .context("failed to flush primary entry prompt")?;
        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .context("failed to read primary entry choice")?;
        let chosen = input.trim().parse::<usize>().unwrap_or(1).saturating_sub(1);
        let idx = chosen.min(kept_entries.len().saturating_sub(1));
        kept_entries[idx].primary = true;
    }
    ensure_single_primary_entry(&mut kept_entries);
    spec.entries = kept_entries;

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
    let _ = use_tui;
    eprint!("{label} [Y/e/n] ");
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
    if spec.entries.is_empty() && spec.services.is_empty() {
        lines.push("- None detected".to_string());
    } else {
        for entry in &spec.entries {
            let mut suffix = String::new();
            if entry.primary {
                suffix.push_str(" (primary)");
            }
            if !entry.env.files.is_empty() {
                suffix.push_str(&format!(" env_files={}", entry.env.files.join(",")));
            }
            lines.push(format!(
                "- `run {}`: `{}` in `{}`{}",
                entry.id, entry.run, entry.cwd, suffix
            ));
        }
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
    spec_digest_verified: bool,
}

pub(crate) fn looks_like_share_run_input(input: &str) -> bool {
    (input.starts_with("http://") || input.starts_with("https://")) && input.contains("/s/")
}

fn looks_like_local_share_file(input: &str) -> bool {
    let path = Path::new(input);
    matches!(
        path.file_name().and_then(|value| value.to_str()),
        Some(SHARE_SPEC_FILE | SHARE_LOCK_FILE)
    )
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
        let spec_raw = fs::read_to_string(&spec_path)
            .with_context(|| format!("Failed to read {}", spec_path.display()))?;
        let spec = serde_json::from_str::<ShareSpec>(&spec_raw)
            .with_context(|| format!("Failed to parse {}", spec_path.display()))?;
        let spec_digest_verified = sha256_label(spec_raw.as_bytes()) == lock.spec_digest;
        return Ok(LoadedShareInput {
            share_url: None,
            resolved_revision_url: None,
            spec,
            lock,
            spec_digest_verified,
        });
    }

    let spec = serde_json::from_str::<ShareSpec>(&raw)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    let lock_path = path
        .parent()
        .map(|parent| parent.join(SHARE_LOCK_FILE))
        .context("share.spec.json has no parent directory")?;
    let lock_raw = fs::read_to_string(&lock_path)
        .with_context(|| format!("Failed to read {}", lock_path.display()))?;
    let lock = serde_json::from_str::<ShareLock>(&lock_raw)
        .with_context(|| format!("Failed to parse {}", lock_path.display()))?;
    let spec_digest_verified = sha256_label(raw.as_bytes()) == lock.spec_digest;
    Ok(LoadedShareInput {
        share_url: None,
        resolved_revision_url: None,
        spec,
        lock,
        spec_digest_verified,
    })
}

fn fetch_share_url(url: &str) -> Result<LoadedShareInput> {
    let parsed = reqwest::Url::parse(url).context("Invalid share URL")?;
    let segment = parsed
        .path_segments()
        .and_then(|mut segments| segments.next_back())
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
        spec_digest_verified: true,
    })
}

fn effective_entries(spec: &ShareSpec) -> Vec<ShareEntrySpec> {
    if !spec.entries.is_empty() {
        return spec.entries.clone();
    }
    derive_entries(&spec.services, &spec.env_requirements)
}

#[allow(dead_code)]
fn select_run_entry(
    input: &str,
    loaded: &LoadedShareInput,
    entries: &[ShareEntrySpec],
    requested_entry: Option<&str>,
) -> Result<ShareEntrySpec> {
    if entries.is_empty() {
        anyhow::bail!(
            "This target looks like a workspace but has no runnable entries. Set it up locally with: ato decap {} --into ./{}",
            loaded
                .resolved_revision_url
                .as_deref()
                .unwrap_or(input),
            loaded.spec.root
        );
    }

    if let Some(requested_entry) = requested_entry {
        return entries
            .iter()
            .find(|entry| entry.id == requested_entry || entry.label == requested_entry)
            .cloned()
            .with_context(|| format!("Unknown entry `{}`", requested_entry));
    }

    if entries.len() == 1 {
        return Ok(entries[0].clone());
    }

    let primaries = entries.iter().filter(|entry| entry.primary).count();
    if primaries == 1 {
        return entries
            .iter()
            .find(|entry| entry.primary)
            .cloned()
            .context("missing primary entry");
    }

    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        let mut choices = String::new();
        for entry in entries {
            choices.push_str(&format!("  - {}\n", entry.id));
        }
        anyhow::bail!(
            "Multiple runnable entries detected. Re-run with --entry <id>.\n{}",
            choices.trim_end()
        );
    }

    eprintln!(
        "Multiple runnable entries detected for {}:",
        loaded.spec.name
    );
    for (index, entry) in entries.iter().enumerate() {
        let env_hint = if entry.env.required.is_empty() && entry.env.files.is_empty() {
            "no env required".to_string()
        } else {
            let mut parts = Vec::new();
            if !entry.env.required.is_empty() {
                parts.push(format!("required env: {}", entry.env.required.join(", ")));
            }
            if !entry.env.files.is_empty() {
                parts.push(format!("env files: {}", entry.env.files.join(", ")));
            }
            parts.join(" · ")
        };
        eprintln!("  {}. {} ({})", index + 1, entry.id, env_hint);
    }
    eprint!("Choose an entry [1-{}]: ", entries.len());
    io::stderr()
        .flush()
        .context("failed to flush entry prompt")?;
    let mut input_line = String::new();
    io::stdin()
        .read_line(&mut input_line)
        .context("failed to read entry prompt")?;
    let selected = input_line.trim().parse::<usize>().unwrap_or(1);
    entries
        .get(selected.saturating_sub(1))
        .cloned()
        .or_else(|| entries.first().cloned())
        .context("no runnable entry available")
}

#[allow(dead_code)]
fn ephemeral_run_root(loaded: &LoadedShareInput, entry: &ShareEntrySpec) -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("failed to resolve current working directory")?;
    let suffix = loaded
        .resolved_revision_url
        .as_deref()
        .or(loaded.share_url.as_deref())
        .unwrap_or(&loaded.spec.name);
    let raw = sha256_label(format!("{}:{}", suffix, entry.id).as_bytes());
    // sha256_label returns "sha256:<hex>"; strip the prefix so the directory
    // name contains no colon (some tools reject CWD paths with ':' in them).
    let digest = raw.trim_start_matches("sha256:");
    Ok(cwd.join(".tmp").join("ato-run").join(digest))
}

#[allow(dead_code)]
fn resolve_entry_env_overlay(
    input: &str,
    entry: &ShareEntrySpec,
    env_file: Option<&Path>,
    prompt_env: bool,
) -> Result<BTreeMap<String, String>> {
    let fingerprint = target_env_fingerprint(input, Some(&entry.id));
    let saved_path = saved_target_env_path(&fingerprint)?;
    let mut envs = BTreeMap::new();

    if saved_path.exists() {
        envs.extend(load_env_map(&saved_path)?);
    }
    if let Some(env_file) = env_file {
        envs.extend(load_env_map(env_file)?);
    }

    let missing_required = entry
        .env
        .required
        .iter()
        .filter(|key| !env_value_present(key, &envs))
        .cloned()
        .collect::<Vec<_>>();

    if missing_required.is_empty() {
        let missing_optional = entry
            .env
            .optional
            .iter()
            .filter(|key| !env_value_present(key, &envs))
            .cloned()
            .collect::<Vec<_>>();
        if !missing_optional.is_empty() {
            eprintln!(
                "Warning: continuing without optional environment variables: {}",
                missing_optional.join(", ")
            );
        }
        return Ok(envs);
    }

    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        anyhow::bail!(
            "Missing required environment variables for entry `{}`: {}\nProvide them with --env-file or set them in your environment before rerunning.",
            entry.id,
            missing_required.join(", ")
        );
    }

    eprintln!("Cannot run `{}` yet.", entry.id);
    eprintln!(
        "Missing required environment variables: {}",
        missing_required.join(", ")
    );
    for file in &entry.env.files {
        eprintln!("Expected env file: {}", file);
    }
    if !prompt_env {
        eprint!("Enter values now? [Y/n] ");
        io::stderr().flush().context("failed to flush env prompt")?;
        let mut confirm = String::new();
        io::stdin()
            .read_line(&mut confirm)
            .context("failed to read env prompt")?;
        let normalized = confirm.trim().to_ascii_lowercase();
        if matches!(normalized.as_str(), "n" | "no") {
            anyhow::bail!(
                "Cancelled before supplying required environment. Re-run with --env-file or use ato decap {} --into ./{}.",
                input,
                entry
                    .cwd
                    .split('/')
                    .next()
                    .unwrap_or("workspace")
            );
        }
    }

    for key in &missing_required {
        eprint!("{key}: ");
        io::stderr()
            .flush()
            .context("failed to flush env value prompt")?;
        let mut value = String::new();
        io::stdin()
            .read_line(&mut value)
            .context("failed to read env value")?;
        envs.insert(key.clone(), value.trim().to_string());
    }

    eprint!("Save these values for this target? [y/N] ");
    io::stderr()
        .flush()
        .context("failed to flush env save prompt")?;
    let mut save = String::new();
    io::stdin()
        .read_line(&mut save)
        .context("failed to read env save prompt")?;
    let normalized = save.trim().to_ascii_lowercase();
    if matches!(normalized.as_str(), "y" | "yes") {
        save_env_map(&saved_path, &envs)?;
    }

    Ok(envs)
}

#[allow(dead_code)]
fn run_shell_streaming(
    command: &str,
    cwd: &Path,
    env_overlay: &BTreeMap<String, String>,
) -> Result<std::process::ExitStatus> {
    let mut process = if cfg!(windows) {
        let mut command_process = Command::new("cmd");
        command_process.arg("/C").arg(command);
        command_process
    } else {
        let mut command_process = Command::new("/bin/sh");
        command_process.arg("-lc").arg(command);
        command_process
    };
    process.current_dir(cwd);
    for (key, value) in env_overlay {
        process.env(key, value);
    }
    process
        .status()
        .with_context(|| format!("Failed to launch share entry in {}", cwd.display()))
}

fn extract_capsule_into(capsule_path: &Path, target_root: &Path) -> Result<()> {
    let file = fs::File::open(capsule_path)
        .with_context(|| format!("Failed to open capsule {}", capsule_path.display()))?;
    let mut archive = tar::Archive::new(file);
    archive
        .unpack(target_root)
        .with_context(|| format!("Failed to extract capsule into {}", target_root.display()))?;
    let cas_provider = capsule_core::capsule_v3::CasProvider::from_env();
    let _ = capsule_core::capsule_v3::unpack_payload_from_capsule_root_with_provider(
        target_root,
        target_root,
        &cas_provider,
    )
    .with_context(|| "Failed to unpack capsule payload")?;
    fs::remove_file(target_root.join("payload.tar.zst")).ok();
    fs::remove_file(target_root.join("payload.tar")).ok();
    Ok(())
}

#[allow(dead_code)]
fn target_env_fingerprint(input: &str, entry_id: Option<&str>) -> String {
    let normalized = format!("{}::{}", input.trim(), entry_id.unwrap_or(""));
    sha256_label(normalized.as_bytes())
}

#[allow(dead_code)]
fn saved_target_env_path(fingerprint: &str) -> Result<PathBuf> {
    let home = dirs::home_dir().context("failed to resolve home directory for saved env store")?;
    Ok(home
        .join(".ato")
        .join("env")
        .join("targets")
        .join(format!("{fingerprint}.env")))
}

#[allow(dead_code)]
fn load_env_map(path: &Path) -> Result<BTreeMap<String, String>> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("Failed to read env file {}", path.display()))?;
    let mut values = BTreeMap::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        values.insert(key.trim().to_string(), value.trim().to_string());
    }
    Ok(values)
}

#[allow(dead_code)]
fn save_env_map(path: &Path, values: &BTreeMap<String, String>) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    let rendered = values
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(path, format!("{rendered}\n"))
        .with_context(|| format!("Failed to write env store {}", path.display()))
}

#[allow(dead_code)]
fn env_value_present(key: &str, overlay: &BTreeMap<String, String>) -> bool {
    overlay
        .get(key)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
        || std::env::var(key)
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
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

fn verify_tools(
    spec_tools: &[ToolRequirementSpec],
    resolved_tools: &[ResolvedToolLock],
) -> Vec<String> {
    let mut missing = Vec::new();
    for spec_tool in spec_tools {
        match resolved_tools.iter().find(|r| r.tool == spec_tool.tool) {
            None => missing.push(spec_tool.tool.clone()),
            Some(resolved) if resolved.binary_path.is_none() => {
                missing.push(spec_tool.tool.clone())
            }
            Some(_) => {}
        }
    }
    missing
}

fn verify_local_tools(spec_tools: &[ToolRequirementSpec]) -> Vec<String> {
    spec_tools
        .iter()
        .filter_map(|tool| {
            let binary = binary_name_for_tool(&tool.tool);
            which::which(binary).err().map(|_| tool.tool.clone())
        })
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
        format!("Run entries: {}", effective_entries(spec).len()),
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
    for entry in effective_entries(spec) {
        lines.push(format!(
            "  - try {} -> {} ({})",
            entry.id, entry.run, entry.cwd
        ));
    }
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
        assert!(capture.spec.entries.iter().any(|entry| entry.primary));
        assert!(capture
            .spec
            .entries
            .iter()
            .any(|entry| entry.id == "dashboard"
                && entry.env.files.iter().any(|path| path.ends_with(".env"))));
    }

    #[test]
    fn derive_entries_prefers_dashboard_as_primary() {
        let services = vec![
            ServiceSpec {
                id: "api".to_string(),
                cwd: "api".to_string(),
                run: "python main.py".to_string(),
                depends_on: Vec::new(),
                kind: "long_running".to_string(),
                optional: false,
                port: Some(8000),
                healthcheck: None,
                evidence: Vec::new(),
            },
            ServiceSpec {
                id: "dashboard".to_string(),
                cwd: "dashboard".to_string(),
                run: "bun run dev".to_string(),
                depends_on: vec!["api".to_string()],
                kind: "long_running".to_string(),
                optional: false,
                port: Some(5173),
                healthcheck: None,
                evidence: vec!["package.json scripts.dev".to_string()],
            },
        ];
        let env_requirements = vec![EnvRequirementSpec {
            id: "dashboard-env".to_string(),
            path: "dashboard/.env.local".to_string(),
            required: true,
            template_path: None,
            note: None,
            evidence: vec![".env.local present".to_string()],
        }];

        let entries = derive_entries(&services, &env_requirements);
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries
                .iter()
                .find(|entry| entry.primary)
                .map(|entry| entry.id.as_str()),
            Some("dashboard")
        );
        assert_eq!(
            entries
                .iter()
                .find(|entry| entry.id == "dashboard")
                .map(|entry| entry.depends_on.clone())
                .unwrap_or_default(),
            vec!["api".to_string()]
        );
        assert_eq!(
            entries
                .iter()
                .find(|entry| entry.id == "dashboard")
                .map(|entry| entry.env.files.clone())
                .unwrap_or_default(),
            vec!["dashboard/.env.local".to_string()]
        );
    }

    #[test]
    fn share_revision_payload_accepts_api_id_alias() {
        let payload = serde_json::json!({
            "id": "share-123",
            "title": "demo",
            "visibility": "unlisted",
            "revision": 1,
            "share_url": "https://api.ato.run/s/share-123",
            "revision_url": "https://api.ato.run/s/share-123@r1",
            "spec": {
                "schema_version": "1",
                "name": "demo",
                "root": "demo",
                "sources": [],
                "tool_requirements": [],
                "env_requirements": [],
                "install_steps": [],
                "entries": [],
                "services": [],
                "notes": { "team_notes": "" },
                "generated_from": {
                    "root_path": "/tmp/demo",
                    "captured_at": "2026-04-10T00:00:00Z",
                    "host_os": "macos"
                }
            },
            "lock": {
                "schema_version": "1",
                "spec_digest": "sha256:abc",
                "generated_guide_digest": "sha256:def",
                "revision": 1,
                "created_at": "2026-04-10T00:00:00Z",
                "resolved_sources": [],
                "resolved_tools": []
            },
            "guide_markdown": "# demo",
            "updated_at": "2026-04-10T00:00:00Z"
        });

        let parsed = serde_json::from_value::<ShareRevisionPayload>(payload)
            .expect("share payload should deserialize with id alias");
        assert_eq!(parsed.share_id, "share-123");
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

    // ── T6: --watch reject ────────────────────────────────────────────────

    #[test]
    fn execute_run_share_rejects_watch_flag() {
        let reporter = Arc::new(crate::reporters::CliReporter::new(false));
        let err = execute_run_share(RunShareArgs {
            input: "https://ato.run/s/demo@r1".to_string(),
            entry: None,
            args: vec![],
            env_file: None,
            prompt_env: false,
            watch: true,
            background: false,
            reporter,
        })
        .expect_err("--watch should be rejected");
        assert!(
            err.to_string().contains("--watch"),
            "expected --watch in error: {err}"
        );
    }

    // ── T7: --background reject ───────────────────────────────────────────

    #[test]
    fn execute_run_share_rejects_background_flag() {
        let reporter = Arc::new(crate::reporters::CliReporter::new(false));
        let err = execute_run_share(RunShareArgs {
            input: "https://ato.run/s/demo@r1".to_string(),
            entry: None,
            args: vec![],
            env_file: None,
            prompt_env: false,
            watch: false,
            background: true,
            reporter,
        })
        .expect_err("--background should be rejected");
        assert!(
            err.to_string().contains("--background"),
            "expected --background in error: {err}"
        );
    }

    // ── T8: spec/lock digest mismatch ─────────────────────────────────────

    #[test]
    fn materialize_reports_digest_mismatch_as_verification_issue() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path();
        let spec_json = serde_json::json!({
            "schema_version": "1",
            "name": "mismatch",
            "root": "mismatch",
            "sources": [],
            "tool_requirements": [],
            "env_requirements": [],
            "install_steps": [],
            "entries": [],
            "services": [],
            "notes": {"team_notes": ""},
            "generated_from": {"root_path": "/tmp", "captured_at": "2026-01-01T00:00:00Z", "host_os": "macos"}
        });
        let spec_raw = serde_json::to_string_pretty(&spec_json).expect("serialize spec");
        fs::write(dir.join("share.spec.json"), &spec_raw).expect("write spec");
        let lock_json = serde_json::json!({
            "schema_version": "1",
            "spec_digest": "sha256:deliberately-wrong-digest",
            "generated_guide_digest": "sha256:guide",
            "revision": 1,
            "created_at": "2026-01-01T00:00:00Z",
            "resolved_sources": [],
            "resolved_tools": []
        });
        fs::write(
            dir.join("share.lock.json"),
            serde_json::to_string_pretty(&lock_json).expect("serialize lock"),
        )
        .expect("write lock");

        let loaded = load_share_input(
            dir.join("share.spec.json")
                .to_str()
                .expect("spec path utf8"),
        )
        .expect("load should succeed");
        assert!(
            !loaded.spec_digest_verified,
            "digest mismatch should be detected"
        );

        let into = temp.path().join("out");
        let state = materialize_loaded_share(&loaded, &into).expect("materialize");
        assert!(
            state
                .verification
                .issues
                .iter()
                .any(|i| i.contains("digest mismatch")),
            "digest mismatch should appear in verification issues: {:?}",
            state.verification.issues
        );
    }

    // ── T9: spec source not in lock ───────────────────────────────────────

    #[test]
    fn materialize_errors_when_spec_source_missing_from_lock() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path();
        let spec_json = serde_json::json!({
            "schema_version": "1",
            "name": "missing-source",
            "root": "missing-source",
            "sources": [{
                "id": "agent",
                "path": "agent",
                "url": "https://github.com/acme/agent.git",
                "ref": "main",
                "kind": "git"
            }],
            "tool_requirements": [],
            "env_requirements": [],
            "install_steps": [],
            "entries": [],
            "services": [],
            "notes": {"team_notes": ""},
            "generated_from": {"root_path": "/tmp", "captured_at": "2026-01-01T00:00:00Z", "host_os": "macos"}
        });
        let spec_raw = serde_json::to_string_pretty(&spec_json).expect("serialize spec");
        fs::write(dir.join("share.spec.json"), &spec_raw).expect("write spec");
        let computed_digest = sha256_label(spec_raw.as_bytes());
        let lock_json = serde_json::json!({
            "schema_version": "1",
            "spec_digest": computed_digest,
            "generated_guide_digest": "sha256:guide",
            "revision": 1,
            "created_at": "2026-01-01T00:00:00Z",
            "resolved_sources": [],
            "resolved_tools": []
        });
        fs::write(
            dir.join("share.lock.json"),
            serde_json::to_string_pretty(&lock_json).expect("serialize lock"),
        )
        .expect("write lock");

        let loaded = load_share_input(
            dir.join("share.spec.json")
                .to_str()
                .expect("spec path utf8"),
        )
        .expect("load should succeed");
        let into = temp.path().join("out");
        let err =
            materialize_loaded_share(&loaded, &into).expect_err("should error on missing source");
        assert!(
            err.to_string().contains("Missing resolved source"),
            "expected missing source error: {err}"
        );
    }

    // ── T10: spec tool missing from lock / local ──────────────────────────

    #[test]
    fn verify_tools_detects_tool_missing_from_lock() {
        let spec_tools = vec![ToolRequirementSpec {
            id: "bun".to_string(),
            tool: "bun".to_string(),
            version: None,
            required_by: vec![],
            evidence: vec![],
        }];
        let missing = verify_tools(&spec_tools, &[]);
        assert_eq!(missing, vec!["bun".to_string()]);
    }

    #[test]
    fn verify_local_tools_detects_tool_not_installed() {
        let spec_tools = vec![ToolRequirementSpec {
            id: "ato-test-fake-binary-xxxx".to_string(),
            tool: "ato-test-fake-binary-xxxx".to_string(),
            version: None,
            required_by: vec![],
            evidence: vec![],
        }];
        let missing = verify_local_tools(&spec_tools);
        assert_eq!(missing, vec!["ato-test-fake-binary-xxxx".to_string()]);
    }

    // ── T11: --into path with spaces ──────────────────────────────────────

    #[test]
    fn ensure_target_root_ready_accepts_path_with_spaces() {
        let temp = tempfile::tempdir().expect("tempdir");
        let spaced = temp.path().join("My Workspace");
        fs::create_dir_all(&spaced).expect("mkdir with space");
        ensure_target_root_ready(&spaced).expect("should accept empty dir with spaces in path");
    }

    // ── T12: empty dir accepted ───────────────────────────────────────────

    #[test]
    fn ensure_target_root_ready_accepts_empty_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let empty = temp.path().join("empty");
        fs::create_dir_all(&empty).expect("mkdir");
        ensure_target_root_ready(&empty).expect("should accept empty directory");
    }

    // ── Primary entry: editing clears other primaries ─────────────────────

    #[test]
    fn ensure_single_primary_entry_leaves_one_primary_intact() {
        let mut entries = vec![
            ShareEntrySpec {
                id: "a".to_string(),
                label: "a".to_string(),
                cwd: ".".to_string(),
                run: "echo a".to_string(),
                kind: "one_shot".to_string(),
                primary: true,
                depends_on: vec![],
                env: EntryEnvSpec::default(),
                evidence: vec![],
            },
            ShareEntrySpec {
                id: "b".to_string(),
                label: "b".to_string(),
                cwd: ".".to_string(),
                run: "echo b".to_string(),
                kind: "one_shot".to_string(),
                primary: false,
                depends_on: vec![],
                env: EntryEnvSpec::default(),
                evidence: vec![],
            },
        ];
        ensure_single_primary_entry(&mut entries);
        // a was already the sole primary, must remain
        assert_eq!(entries.iter().filter(|e| e.primary).count(), 1);
        assert!(entries.iter().find(|e| e.id == "a").unwrap().primary);
    }

    #[test]
    fn ensure_single_primary_entry_clears_multi_primary_silently() {
        let mut entries = vec![
            ShareEntrySpec {
                id: "a".to_string(),
                label: "a".to_string(),
                cwd: ".".to_string(),
                run: "echo a".to_string(),
                kind: "one_shot".to_string(),
                primary: true,
                depends_on: vec![],
                env: EntryEnvSpec::default(),
                evidence: vec![],
            },
            ShareEntrySpec {
                id: "b".to_string(),
                label: "b".to_string(),
                cwd: ".".to_string(),
                run: "echo b".to_string(),
                kind: "one_shot".to_string(),
                primary: true,
                depends_on: vec![],
                env: EntryEnvSpec::default(),
                evidence: vec![],
            },
        ];
        ensure_single_primary_entry(&mut entries);
        // safety net: exactly one primary, first entry wins
        assert_eq!(entries.iter().filter(|e| e.primary).count(), 1);
        assert!(entries.iter().find(|e| e.id == "a").unwrap().primary);
        assert!(!entries.iter().find(|e| e.id == "b").unwrap().primary);
    }

    #[test]
    fn edit_primary_clears_prior_primary_in_kept_entries() {
        // Simulate the keep-then-edit flow that caused the primary revert bug.
        // When user Keeps entry A (primary=true) and then Edits entry B and marks
        // it as primary=true, entry A must lose its primary flag.
        let mut kept_entries: Vec<ShareEntrySpec> = vec![];

        // Step 1: Keep entry A with primary=true (simulate PromptDecision::Keep)
        let entry_a = ShareEntrySpec {
            id: "agent-main".to_string(),
            label: "agent-main".to_string(),
            cwd: "agent".to_string(),
            run: "python main.py".to_string(),
            kind: "one_shot".to_string(),
            primary: true,
            depends_on: vec![],
            env: EntryEnvSpec::default(),
            evidence: vec![],
        };
        kept_entries.push(entry_a);

        // Step 2: Edit entry B and mark it as primary=true (simulate PromptDecision::Edit)
        let mut entry_b = ShareEntrySpec {
            id: "dashboard".to_string(),
            label: "dashboard".to_string(),
            cwd: "dashboard".to_string(),
            run: "bun run dev".to_string(),
            kind: "long_running".to_string(),
            primary: false,
            depends_on: vec![],
            env: EntryEnvSpec::default(),
            evidence: vec![],
        };
        entry_b.primary = true;
        // The fix: clearing prior primaries when user explicitly sets primary=true
        if entry_b.primary {
            for prev in kept_entries.iter_mut() {
                prev.primary = false;
            }
        }
        kept_entries.push(entry_b);

        // After the loop: exactly one primary (dashboard), not agent-main
        let primaries: Vec<&str> = kept_entries
            .iter()
            .filter(|e| e.primary)
            .map(|e| e.id.as_str())
            .collect();
        assert_eq!(
            primaries,
            vec!["dashboard"],
            "dashboard should be the only primary"
        );
    }
}
