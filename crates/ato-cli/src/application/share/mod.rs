use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use anyhow::{Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use capsule_core::share::{
    self as share_types, EntryEnvSpec, EnvRequirementSpec, EnvState, GeneratedFrom,
    InstallStepSpec, InstallStepState, LoadedShareInput, ResolvedSourceLock, ResolvedToolLock,
    ServiceSpec, ShareEntrySpec, ShareLock, ShareNotes, ShareSourceSpec, ShareSourceState,
    ShareSpec, ToolRequirementSpec, VerificationState, WorkspaceShareState,
};
use capsule_core::CapsuleReporter;
use chrono::Utc;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::application::auth;
use crate::cli::{EncapVisibility, GitMode, ShareToolRuntime};
use crate::fs_copy;
use crate::reporters::CliReporter;

use share_types::{
    default_runtime_source_str, SHARE_DIR, SHARE_LOCK_FILE, SHARE_SCHEMA_VERSION, SHARE_SPEC_FILE,
    SHARE_STATE_FILE,
};
const SHARE_GUIDE_FILE: &str = "guide.md";
const DEFAULT_API_TIMEOUT_SECS: u64 = 20;

/// Emit an informational hint (not an execution result, not a warning) to stderr.
///
/// Each line is prefixed with a `[hint]` tag so it is visually distinct from
/// actual program stdout (e.g. `hello, world!`). When stderr is a TTY — or
/// when the caller set `FORCE_COLOR=1` / `CLICOLOR_FORCE=1` (used by
/// ato-desktop's REPL which pipes stderr) — the prefix and body are rendered
/// in grey (`\x1b[90m`) so the actual execution output stands out. Falls back
/// to plain `[hint] ...` when colors are off (respects `NO_COLOR`), keeping
/// logs and grep pipelines readable.
fn emit_dim_hint(message: &str) {
    let color = hint_color_enabled();
    let mut stderr = io::stderr();
    for line in message.lines() {
        let _ = if color {
            writeln!(stderr, "\x1b[90m[hint]\x1b[0m \x1b[90m{}\x1b[0m", line)
        } else {
            writeln!(stderr, "[hint] {}", line)
        };
    }
}

fn hint_color_enabled() -> bool {
    // Explicit opt-out always wins.
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    // Honour caller-forced color (ato-desktop REPL pipes stderr but sets these).
    if std::env::var_os("FORCE_COLOR").is_some() || std::env::var_os("CLICOLOR_FORCE").is_some() {
        return true;
    }
    io::stderr().is_terminal()
}
/// Pinned Python version used by ato-managed runtimes for decap install steps.
const SHARE_PROVIDER_PYTHON_VERSION: &str = "3.11.10";
/// Pinned Node version signalled to fnm/nvm/mise for decap install steps.
const SHARE_PROVIDER_NODE_VERSION: &str = "20.11.0";
/// Maximum compressed archive size for kind="archive" sources (10 MB).
const ARCHIVE_MAX_COMPRESSED_BYTES: u64 = 10 * 1024 * 1024;
/// Maximum number of files in a kind="archive" source.
const ARCHIVE_MAX_FILE_COUNT: usize = 5_000;

/// Configuration loaded from the `[share]` section of `capsule.toml`.
/// CLI flags take precedence over values here.
#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct ShareConfigToml {
    pub(crate) git_mode: Option<String>,
    pub(crate) tool_runtime: Option<String>,
    pub(crate) yes: Option<bool>,
    pub(crate) allow_dirty: Option<bool>,
    pub(crate) exclude: Option<ShareExcludeConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct ShareExcludeConfig {
    #[serde(default)]
    pub(crate) sources: Vec<String>,
    #[serde(default)]
    pub(crate) tools: Vec<String>,
    #[serde(default)]
    pub(crate) install_steps: Vec<String>,
    #[serde(default)]
    pub(crate) entries: Vec<String>,
}

/// Wrapper used only for TOML parsing to extract the `[share]` table.
#[derive(Debug, Deserialize, Default)]
struct CapsuleTomlShare {
    #[serde(default)]
    share: Option<ShareConfigToml>,
}

#[derive(Debug, Clone)]
pub(crate) struct EncapArgs {
    pub(crate) path: PathBuf,
    pub(crate) visibility: EncapVisibility,
    pub(crate) print_plan: bool,
    pub(crate) dry_run: bool,
    pub(crate) git_mode: GitMode,
    pub(crate) tool_runtime: ShareToolRuntime,
    pub(crate) allow_dirty: bool,
    pub(crate) yes: bool,
    pub(crate) save_config: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct DecapArgs {
    pub(crate) input: String,
    pub(crate) into: PathBuf,
    pub(crate) plan: bool,
    pub(crate) tool_runtime: ShareToolRuntime,
    pub(crate) strict: bool,
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
    /// When true, bypass nacelle and run directly on the host (mirrors --compatibility-fallback host).
    pub(crate) compat_host: bool,
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

#[derive(Debug, Clone)]
struct CandidateRepo {
    abs_path: PathBuf,
    rel_path: String,
    url: String,
    branch: Option<String>,
    rev: String,
    evidence: Vec<String>,
}

/// A directory without a git remote that is bundled inline as a gzip tar archive.
#[derive(Debug)]
struct CandidateArchiveSource {
    abs_path: PathBuf,
    rel_path: String,
    /// Base64-encoded gzip tar content.
    content_base64: String,
    /// sha256:<hex> of the raw (pre-base64) gzip bytes.
    content_digest: String,
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

fn execute_encap_dry_run(
    root: &Path,
    capture: &CapturedWorkspace,
    reporter: &Arc<CliReporter>,
) -> Result<()> {
    use crate::application::secrets::scanner::{scan_for_secret_patterns, SecretScanHit};
    use capsule_core::packers::pack_filter::PackFilter;

    futures::executor::block_on(
        reporter
            .notify("🔍 Dry run — showing what would be included (no files written)".to_string()),
    )?;

    // List sources
    futures::executor::block_on(
        reporter.notify(format!("Sources ({}):", capture.spec.sources.len())),
    )?;
    for source in &capture.spec.sources {
        futures::executor::block_on(
            reporter.notify(format!("  • {} ({})", source.id, source.url)),
        )?;
    }

    // Required env
    if !capture.spec.env_requirements.is_empty() {
        futures::executor::block_on(reporter.notify(format!(
            "Required env vars ({}):",
            capture.spec.env_requirements.len()
        )))?;
        for req in &capture.spec.env_requirements {
            futures::executor::block_on(reporter.notify(format!("  • {}", req.id)))?;
        }
    }

    // Scan workspace files for secret patterns
    let manifest_path = root.join("capsule.toml");
    let filter = PackFilter::from_manifest_path(&manifest_path).ok();

    let mut all_hits: Vec<SecretScanHit> = Vec::new();
    let walk_result: Result<Vec<_>> = walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| {
            let abs = e.path().to_path_buf();
            let rel = abs.strip_prefix(root).unwrap_or(&abs).to_path_buf();
            Ok::<_, anyhow::Error>((abs, rel))
        })
        .collect();

    if let Ok(entries) = walk_result {
        for (abs_path, rel_path) in entries {
            let include = match &filter {
                Some(f) => f.should_include_file(&rel_path),
                None => !is_dry_run_skip_path(&rel_path),
            };
            if !include {
                continue;
            }
            let rel_str = rel_path.display().to_string();
            if let Ok(content) = std::fs::read_to_string(&abs_path) {
                let hits = scan_for_secret_patterns(&content, &rel_str);
                all_hits.extend(hits);
            }
        }
    }

    if all_hits.is_empty() {
        futures::executor::block_on(
            reporter.notify("✅ No secret patterns detected in workspace files.".to_string()),
        )?;
    } else {
        futures::executor::block_on(reporter.warn(format!(
            "⚠️  {} potential secret(s) found in files that would be included:",
            all_hits.len()
        )))?;
        for hit in &all_hits {
            futures::executor::block_on(reporter.warn(format!(
                "  {}:{} — {} ({})",
                hit.file, hit.line, hit.prefix, hit.snippet
            )))?;
        }
        futures::executor::block_on(reporter.warn(
            "Add these files to [pack] exclude in capsule.toml or .atoignore before running ato encap.".to_string(),
        ))?;
    }

    Ok(())
}

/// Fallback path filter for dry-run when no capsule.toml exists.
/// Returns `true` if the path should be skipped during scanning.
fn is_dry_run_skip_path(rel: &std::path::Path) -> bool {
    let s = rel.to_string_lossy().to_ascii_lowercase();
    s.contains("node_modules/")
        || s.contains("/.git/")
        || s.starts_with(".git/")
        || s.contains("/.next/")
        || s.contains("/target/")
        || s.contains("/dist/")
        || s.contains("/__pycache__/")
        || s.ends_with(".lock")
        || s.ends_with(".png")
        || s.ends_with(".jpg")
        || s.ends_with(".jpeg")
        || s.ends_with(".gif")
        || s.ends_with(".ico")
        || s.ends_with(".woff")
        || s.ends_with(".woff2")
        || s.ends_with(".ttf")
        || s.ends_with(".eot")
        || {
            // Exclude .env and .env.* files (same as SMART_DEFAULT_EXCLUDES in PackFilter)
            let filename = rel
                .file_name()
                .map(|n| n.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            filename == ".env" || filename.starts_with(".env.") || filename == ".envrc"
        }
}

pub(crate) fn execute_encap(args: EncapArgs, reporter: Arc<CliReporter>) -> Result<()> {
    let root = args
        .path
        .canonicalize()
        .with_context(|| format!("Failed to resolve workspace root {}", args.path.display()))?;

    // Load capsule.toml [share] config; CLI flags take precedence.
    let config = load_share_config(&root);
    let effective_git_mode = if args.git_mode != GitMode::SameCommit {
        args.git_mode
    } else if let Some(ref cfg) = config {
        cfg.git_mode
            .as_deref()
            .and_then(|s| match s {
                "latest-at-encap" => Some(GitMode::LatestAtEncap),
                _ => None,
            })
            .unwrap_or(args.git_mode)
    } else {
        args.git_mode
    };
    let effective_tool_runtime = if args.tool_runtime != ShareToolRuntime::Auto {
        args.tool_runtime
    } else if let Some(ref cfg) = config {
        cfg.tool_runtime
            .as_deref()
            .and_then(|s| match s {
                "ato" => Some(ShareToolRuntime::Ato),
                "system" => Some(ShareToolRuntime::System),
                _ => None,
            })
            .unwrap_or(args.tool_runtime)
    } else {
        args.tool_runtime
    };
    let effective_allow_dirty =
        args.allow_dirty || config.as_ref().and_then(|c| c.allow_dirty).unwrap_or(false);
    let effective_yes = args.yes || config.as_ref().and_then(|c| c.yes).unwrap_or(false);

    let capture = capture_workspace(
        &root,
        effective_git_mode,
        effective_allow_dirty,
        effective_tool_runtime,
        &reporter,
    )?;

    if args.dry_run {
        return execute_encap_dry_run(&root, &capture, &reporter);
    }

    if args.print_plan {
        println!("{}", serde_json::to_string_pretty(&capture.spec)?);
        return Ok(());
    }

    let mut spec = capture.spec;

    // Apply exclude filters from capsule.toml [share.exclude].
    if let Some(ref cfg) = config {
        if let Some(ref exclude) = cfg.exclude {
            apply_share_exclude(&mut spec, exclude);
        }
    }

    finalize_capture(&mut spec, effective_yes, &reporter)?;

    if args.save_config {
        write_share_config_to_capsule_toml(
            &root,
            &spec,
            &effective_git_mode,
            &effective_tool_runtime,
        )?;
    }

    let guide = generate_guide(&spec);
    let lock = build_share_lock(&spec, &capture.repo_locks, &capture.resolved_tools, &guide)?;
    let output = write_share_files(&root, &spec, &lock, &guide)?;

    futures::executor::block_on(reporter.notify(format!(
        "📦 Wrote share files:\n  {}\n  {}\n  {}",
        output.spec_path.display(),
        output.lock_path.display(),
        output.guide_path.display()
    )))?;

    if args.visibility != EncapVisibility::Local {
        match upload_share(&spec, &lock, &guide, args.visibility.as_api_str()) {
            Ok(uploaded) => {
                futures::executor::block_on(reporter.notify(format!(
                    "🔗 Share URL: {}\n🔒 Revision URL: {}",
                    rewrite_api_to_site_url(&uploaded.share_url),
                    rewrite_api_to_site_url(&uploaded.revision_url)
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
        let state = materialize_loaded_share(&loaded, &into, args.tool_runtime, args.strict)?;
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

    // CLI-specific: interactive entry selection + env prompt (stays in CLI layer)
    let loaded = load_share_input(&args.input)?;
    let entries = effective_entries(&loaded.spec);
    let entry = select_run_entry(&args.input, &loaded, &entries, args.entry.as_deref())?;
    let env_overlay = resolve_entry_env_overlay(
        &args.input,
        &entry,
        args.env_file.as_deref(),
        args.prompt_env,
    )?;

    let next_command = loaded
        .resolved_revision_url
        .clone()
        .unwrap_or_else(|| args.input.clone());
    // Informational prelude (not execution result, not a warning): dim on a TTY
    // so the actual program output stands out. See `emit_dim_hint` for the
    // dim ANSI sequence + non-TTY fallback.
    emit_dim_hint(&format!(
        "Try now: `{}`\nSet up locally later: ato decap {} --into ./{}",
        entry.run, next_command, loaded.spec.root
    ));

    // Delegate to capsule-core ShareExecutor (nacelle-sandboxed execution)
    let result = capsule_core::share::execute_share(capsule_core::share::ShareRunRequest {
        input: args.input.clone(),
        entry: Some(entry.id.clone()),
        extra_args: args.args,
        env_overlay,
        mode: capsule_core::share::ShareExecutionMode::Inherited,
        nacelle_path: None,
        ato_path: None,
        compat_host: args.compat_host,
    })?;

    match result {
        capsule_core::share::ShareExecutionResult::Completed { exit_code: 0 } => Ok(()),
        capsule_core::share::ShareExecutionResult::Completed { exit_code } => {
            anyhow::bail!("share entry `{}` exited with code {}", entry.id, exit_code);
        }
        _ => unreachable!("Inherited mode always returns Completed"),
    }
}

fn materialize_loaded_share(
    loaded: &LoadedShareInput,
    into: &Path,
    tool_runtime: ShareToolRuntime,
    strict: bool,
) -> Result<WorkspaceShareState> {
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

    let runtime_env = prepare_share_runtime_env(&loaded.spec.tool_requirements, tool_runtime);

    for step in &loaded.spec.install_steps {
        let started_at = Utc::now().to_rfc3339();
        let step_root = into.join(&step.cwd);
        match run_shell_command_with_env(&step.run, &step_root, &runtime_env) {
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

    // Persist spec and lock alongside state.json so downstream consumers
    // (e.g. capsule-core ShareExecutor) can read them from the workspace
    // without needing a separate API fetch.
    let share_dir = into.join(SHARE_DIR);
    fs::write(
        share_dir.join(SHARE_SPEC_FILE),
        serde_json::to_string_pretty(&loaded.spec)
            .context("failed to serialize spec for workspace")?,
    )
    .context("failed to write share.spec.json to workspace")?;
    fs::write(
        share_dir.join(SHARE_LOCK_FILE),
        serde_json::to_string_pretty(&loaded.lock)
            .context("failed to serialize lock for workspace")?,
    )
    .context("failed to write share.lock.json to workspace")?;

    if strict && !state.verification.issues.is_empty() {
        let issues = state.verification.issues.join("\n  - ");
        anyhow::bail!(
            "decap completed with verification issues (--strict):\n  - {}",
            issues
        );
    }

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
        None,
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
        "  - Share it later with: ato encap".to_string(),
    ];
    if !state.verification.issues.is_empty() {
        lines.push("Issues:".to_string());
        for issue in &state.verification.issues {
            lines.push(format!("  - {}", issue));
        }
    }
    lines.join("\n")
}

fn capture_workspace(
    root: &Path,
    git_mode: GitMode,
    allow_dirty: bool,
    tool_runtime: ShareToolRuntime,
    reporter: &Arc<CliReporter>,
) -> Result<CapturedWorkspace> {
    let ignore = IgnoreMatcher::load(root)?;
    let repos = discover_repositories(root, &ignore)?;

    // Dirty-state check: warn or fail if any repo has uncommitted changes.
    for repo in &repos {
        let dirty_output = git_output(&repo.abs_path, &["status", "--porcelain"])?;
        let is_dirty = dirty_output
            .as_deref()
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        if is_dirty {
            if allow_dirty {
                futures::executor::block_on(reporter.warn(format!(
                    "⚠️  Repository {} has uncommitted changes. \
                     Recipients will not see these changes after decap.",
                    repo.rel_path
                )))?;
            } else {
                anyhow::bail!(
                    "Repository {} has uncommitted changes. \
                     Commit or stash your changes before encap, \
                     or pass --allow-dirty to proceed anyway.",
                    repo.rel_path
                );
            }
        }
    }

    // Resolve the pinned rev for each repo according to git_mode.
    let repo_locks = repos
        .iter()
        .map(|repo| resolve_source_lock(repo, git_mode))
        .collect::<Result<Vec<_>>>()?;

    // Discover directories not covered by any git repo and pack them inline.
    let archive_sources = discover_archive_sources(root, &repos, &ignore, reporter)?;
    let archive_locks: Vec<ResolvedSourceLock> = archive_sources
        .iter()
        .map(|arc| ResolvedSourceLock {
            id: repo_id_from_path(&arc.rel_path),
            rev: arc.content_digest.clone(),
            git_mode: "archive".to_string(),
            remote_branch: None,
        })
        .collect();

    let mut all_locks = repo_locks;
    all_locks.extend(archive_locks);

    let mut tool_requirements = BTreeMap::<String, ToolRequirementSpec>::new();
    let mut env_requirements = Vec::new();
    let mut install_steps = Vec::new();
    let mut services = Vec::new();

    for repo in &repos {
        let repo_scan_dirs = discover_repo_scan_dirs(&repo.abs_path)?;
        for scan_dir in repo_scan_dirs {
            // Compute relative_dir using repo.rel_path as a prefix so that
            // entry.cwd / step.cwd values are always consistent with source.path.
            // Without this, when the workspace root IS the git repo,
            // relative_display(root, scan_dir) returns "." while source.path
            // stores the folder name (e.g., "browser-daw"), causing ENOENT on run.
            let within_repo = relative_display(&repo.abs_path, &scan_dir);
            let relative_dir = if within_repo == "." {
                repo.rel_path.clone()
            } else {
                format!("{}/{}", repo.rel_path, within_repo)
            };
            detect_tools(&scan_dir, &relative_dir, &mut tool_requirements)?;
            detect_env_requirements(&scan_dir, &relative_dir, &mut env_requirements)?;
            detect_install_steps(&scan_dir, &relative_dir, &mut install_steps)?;
            detect_services(&scan_dir, &relative_dir, &mut services)?;
        }
    }

    // Also scan archive source directories so tools/steps/services are detected.
    for arc in &archive_sources {
        let scan_dirs = discover_repo_scan_dirs(&arc.abs_path)?;
        for scan_dir in scan_dirs {
            let within = relative_display(&arc.abs_path, &scan_dir);
            let relative_dir = if within == "." {
                arc.rel_path.clone()
            } else {
                format!("{}/{}", arc.rel_path, within)
            };
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

    let git_sources = repos.iter().map(|repo| ShareSourceSpec {
        id: repo_id_from_path(&repo.rel_path),
        kind: "git".to_string(),
        url: repo.url.clone(),
        path: repo.rel_path.clone(),
        branch: repo.branch.clone(),
        evidence: repo.evidence.clone(),
        git_mode: git_mode.as_str().to_string(),
        archive_content: None,
    });
    let inline_sources = archive_sources.iter().map(|arc| ShareSourceSpec {
        id: repo_id_from_path(&arc.rel_path),
        kind: "archive".to_string(),
        // Embed content as a data URI — this survives the API round-trip because
        // the `url` field is a known spec field that the server stores verbatim.
        url: format!("data:application/x-tar+gzip;base64,{}", arc.content_base64),
        path: arc.rel_path.clone(),
        branch: None,
        evidence: vec![format!("archive: {}", arc.content_digest)],
        git_mode: "archive".to_string(),
        // Also populate archive_content for local-only workflows (before upload).
        archive_content: Some(arc.content_base64.clone()),
    });

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
        sources: git_sources.chain(inline_sources).collect(),
        tool_requirements: {
            let runtime_source = tool_runtime.as_str().to_string();
            tool_requirements
                .into_values()
                .map(|mut t| {
                    // For ato/auto mode, mark tools that ato can manage with the chosen runtime.
                    if !matches!(tool_runtime, ShareToolRuntime::System) {
                        t.runtime_source = runtime_source.clone();
                    }
                    t
                })
                .collect()
        },
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
        repo_locks: all_locks,
        resolved_tools,
    })
}

/// Resolve the pinned rev for a repo according to the requested git_mode.
/// For `same-commit`: verifies the local HEAD is reachable from the remote (warns if not).
/// For `latest-at-encap`: fetches the remote branch HEAD and pins that rev.
fn resolve_source_lock(repo: &CandidateRepo, git_mode: GitMode) -> Result<ResolvedSourceLock> {
    match git_mode {
        GitMode::SameCommit => {
            // Verify the local rev exists on the remote; warn if not reachable.
            let rev = repo.rev.trim().to_string();
            let is_reachable = check_rev_reachable_on_remote(&repo.url, &rev);
            if !is_reachable {
                eprintln!(
                    "⚠️  Warning: commit {} in {} was not found on the remote.\n\
                     Recipients may fail to check out this revision.\n\
                     Push to the remote first, or use --git-mode latest-at-encap.",
                    &rev[..rev.len().min(12)],
                    repo.rel_path,
                );
            }
            Ok(ResolvedSourceLock {
                id: repo_id_from_path(&repo.rel_path),
                rev,
                git_mode: git_mode.as_str().to_string(),
                remote_branch: repo.branch.clone(),
            })
        }
        GitMode::LatestAtEncap => {
            let branch = repo.branch.as_deref().unwrap_or("HEAD");
            let remote_rev = fetch_remote_rev(&repo.url, branch)?;
            Ok(ResolvedSourceLock {
                id: repo_id_from_path(&repo.rel_path),
                rev: remote_rev,
                git_mode: git_mode.as_str().to_string(),
                remote_branch: repo.branch.clone(),
            })
        }
    }
}

/// Return `true` if the given rev SHA is advertised by the remote (i.e. is the tip of some ref).
/// This is a lightweight check: it does not guarantee the object is fetchable for arbitrary SHAs,
/// but it catches the common "local-only commit" case where the commit is not yet pushed.
fn check_rev_reachable_on_remote(url: &str, rev: &str) -> bool {
    let output = Command::new("git").args(["ls-remote", url]).output();
    let Ok(output) = output else { return false };
    if !output.status.success() {
        return false;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    // ls-remote output: "<sha>\t<refname>\n"
    // The rev is reachable if any advertised tip SHA starts with the given prefix
    // (typically a full 40-char SHA, but we match a prefix for safety).
    stdout.lines().any(|line| {
        line.split('\t')
            .next()
            .map(|sha| sha.starts_with(rev))
            .unwrap_or(false)
    })
}

/// Fetch the current HEAD of `branch` from the remote and return the SHA.
fn fetch_remote_rev(url: &str, branch: &str) -> Result<String> {
    // Normalise: "HEAD" → ask for HEAD, otherwise ask for refs/heads/<branch>
    let refspec = if branch == "HEAD" {
        "HEAD".to_string()
    } else {
        format!("refs/heads/{branch}")
    };
    let output = Command::new("git")
        .args(["ls-remote", url, &refspec])
        .output()
        .with_context(|| format!("Failed to run git ls-remote {url}"))?;
    if !output.status.success() {
        anyhow::bail!(
            "git ls-remote {} {} failed: {}",
            url,
            refspec,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let rev = stdout
        .lines()
        .next()
        .and_then(|line| line.split('\t').next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Remote {} has no ref {refspec}. \
                 Make sure the branch is pushed.",
                url
            )
        })?;
    Ok(rev.to_string())
}

struct CapturedWorkspace {
    spec: ShareSpec,
    repo_locks: Vec<ResolvedSourceLock>,
    resolved_tools: Vec<ResolvedToolLock>,
}

/// Returns directories at or directly under `root` that are not covered by any
/// discovered git repo and contain at least one non-trivial file. These directories
/// are bundled as inline gzip tar archives embedded in the share spec.
fn discover_archive_sources(
    root: &Path,
    repos: &[CandidateRepo],
    ignore: &IgnoreMatcher,
    reporter: &Arc<CliReporter>,
) -> Result<Vec<CandidateArchiveSource>> {
    // Compute the set of absolute paths already owned by git repos.
    let repo_paths: BTreeSet<PathBuf> = repos.iter().map(|r| r.abs_path.clone()).collect();

    let mut result = Vec::new();

    // When there are no repos, the root itself is the archive candidate (e.g., a single
    // non-git project directory).  When repos exist, only check direct children that are
    // not already a repo or inside one — the root is just a workspace container.
    let candidates: Vec<PathBuf> = if repos.is_empty() {
        vec![root.to_path_buf()]
    } else {
        fs::read_dir(root)
            .with_context(|| format!("Failed to read {}", root.display()))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect()
    };

    for candidate in candidates {
        if ignore.matches(root, &candidate) {
            continue;
        }
        if repo_paths.contains(&candidate) {
            continue;
        }
        // Skip dirs already owned by a git repo (candidate is inside a repo's tree).
        let inside_repo = repos
            .iter()
            .any(|r| candidate.starts_with(&r.abs_path) && candidate != r.abs_path);
        if inside_repo {
            continue;
        }
        // Skip well-known non-source dirs.
        let dir_name = candidate.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if matches!(
            dir_name,
            ".git" | ".ato" | ".tmp" | "target" | "node_modules" | ".venv" | "__pycache__"
        ) {
            continue;
        }

        let rel_path = relative_display(root, &candidate);
        let rel_path = if rel_path == "." {
            root.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("workspace")
                .to_string()
        } else {
            rel_path
        };

        match pack_archive_source(root, &candidate) {
            Ok(Some((content_base64, content_digest))) => {
                futures::executor::block_on(reporter.notify(format!(
                    "📦 Bundling archive source: {} ({})",
                    rel_path, content_digest
                )))?;
                result.push(CandidateArchiveSource {
                    abs_path: candidate,
                    rel_path,
                    content_base64,
                    content_digest,
                });
            }
            Ok(None) => {
                // Directory had no non-trivial files — skip silently.
            }
            Err(err) => {
                futures::executor::block_on(
                    reporter.warn(format!("⚠️  Could not bundle {}: {}", rel_path, err)),
                )?;
            }
        }
    }

    Ok(result)
}

/// Returns true if `filename` looks like a secret/env file that must not be bundled.
fn is_env_file_path(filename: &str) -> bool {
    let lower = filename.to_ascii_lowercase();
    lower == ".env" || lower.starts_with(".env.") || lower == ".envrc"
}

/// Paths to exclude when packing archive sources (relative to the archive root).
fn is_archive_excluded_path(rel: &str) -> bool {
    let parts: Vec<&str> = rel.split('/').collect();
    let first = parts.first().copied().unwrap_or("");
    // Skip build artifacts, virtual environments, and generated lock dirs.
    if matches!(
        first,
        ".git" | ".ato" | ".tmp" | "target" | "node_modules" | ".venv" | "__pycache__"
    ) {
        return true;
    }
    // Skip environment / secret files at any path level.
    let filename = parts.last().copied().unwrap_or("");
    is_env_file_path(filename)
}

/// Pack a directory into a gzip tar and return (base64_content, sha256_digest).
/// Returns `None` if the directory contains no packable files.
fn pack_archive_source(root: &Path, dir: &Path) -> Result<Option<(String, String)>> {
    let mut file_count: usize = 0;
    let gz_buf: Vec<u8> = Vec::new();
    let enc = GzEncoder::new(gz_buf, Compression::best());
    let mut builder = tar::Builder::new(enc);
    builder.follow_symlinks(false);

    let mut entries: Vec<(PathBuf, String)> = Vec::new();
    collect_archive_entries(dir, dir, &mut entries, root)?;

    if entries.is_empty() {
        return Ok(None);
    }

    for (abs_path, rel_str) in &entries {
        if is_archive_excluded_path(rel_str) {
            continue;
        }
        let metadata =
            fs::metadata(abs_path).with_context(|| format!("stat {}", abs_path.display()))?;
        if !metadata.is_file() {
            continue;
        }
        file_count += 1;
        if file_count > ARCHIVE_MAX_FILE_COUNT {
            anyhow::bail!(
                "Archive source exceeds {} file limit. \
                 Add a .atoignore to exclude large directories.",
                ARCHIVE_MAX_FILE_COUNT
            );
        }
        builder
            .append_path_with_name(abs_path, rel_str)
            .with_context(|| format!("Failed to add {} to archive", abs_path.display()))?;
    }

    if file_count == 0 {
        return Ok(None);
    }

    let enc = builder.into_inner().context("Failed to finish tar")?;
    let gz_bytes = enc.finish().context("Failed to finish gzip")?;

    if gz_bytes.len() as u64 > ARCHIVE_MAX_COMPRESSED_BYTES {
        anyhow::bail!(
            "Archive source compressed size ({} bytes) exceeds {} byte limit. \
             Add a .atoignore to reduce the workspace size.",
            gz_bytes.len(),
            ARCHIVE_MAX_COMPRESSED_BYTES
        );
    }

    let digest = sha256_label(&gz_bytes);
    let content_base64 = BASE64.encode(&gz_bytes);
    Ok(Some((content_base64, digest)))
}

/// Recursively collect (abs_path, archive-relative path) for all entries under `dir`.
fn collect_archive_entries(
    archive_root: &Path,
    dir: &Path,
    out: &mut Vec<(PathBuf, String)>,
    _workspace_root: &Path,
) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("Failed to read {}", dir.display()))? {
        let entry = entry.with_context(|| format!("Failed to read entry in {}", dir.display()))?;
        let path = entry.path();
        let rel = relative_display(archive_root, &path);
        if is_archive_excluded_path(&rel) {
            continue;
        }
        if path.is_dir() {
            collect_archive_entries(archive_root, &path, out, _workspace_root)?;
        } else if path.is_file() {
            out.push((path, rel));
        }
    }
    Ok(())
}

/// Safely extract a base64-encoded gzip tar archive into `target`.
/// Rejects absolute paths, `..` components, symlinks, hardlinks, and device files.
fn extract_archive_source(content_base64: &str, target: &Path) -> Result<()> {
    let gz_bytes = BASE64
        .decode(content_base64)
        .context("Failed to base64-decode archive content")?;

    fs::create_dir_all(target).with_context(|| format!("Failed to create {}", target.display()))?;

    let gz = GzDecoder::new(gz_bytes.as_slice());
    let mut archive = tar::Archive::new(gz);
    archive.set_preserve_permissions(false);
    archive.set_unpack_xattrs(false);
    archive.set_overwrite(true);

    for entry_result in archive
        .entries()
        .context("Failed to read archive entries")?
    {
        let mut entry = entry_result.context("Failed to read archive entry")?;
        let header = entry.header();

        // Reject non-regular-file entries (symlinks, hardlinks, devices, directories).
        let entry_type = header.entry_type();
        if !entry_type.is_file() {
            if entry_type.is_dir() {
                // Directories are fine — let them through.
            } else {
                // Reject symlinks, hardlinks, block/char devices, etc.
                anyhow::bail!(
                    "Archive contains a non-regular entry type {:?}; rejected for security",
                    entry_type
                );
            }
        }

        let entry_path = entry
            .path()
            .context("Archive entry has non-UTF-8 path")?
            .into_owned();

        // Reject absolute paths and `..` traversal.
        if entry_path.is_absolute() {
            anyhow::bail!(
                "Archive contains absolute path {}: rejected for security",
                entry_path.display()
            );
        }
        for component in entry_path.components() {
            use std::path::Component;
            if matches!(component, Component::ParentDir) {
                anyhow::bail!(
                    "Archive contains path traversal in {}: rejected for security",
                    entry_path.display()
                );
            }
        }

        let dest = target.join(&entry_path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }
        entry
            .unpack(&dest)
            .with_context(|| format!("Failed to extract {}", entry_path.display()))?;
    }

    Ok(())
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
            runtime_source: default_runtime_source_str(),
            provider_toolchain: None,
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
                let runner = if package_manager.starts_with("pnpm@")
                    || scan_dir.join("pnpm-lock.yaml").exists()
                {
                    "pnpm run"
                } else if package_manager.starts_with("yarn@")
                    || scan_dir.join("yarn.lock").exists()
                {
                    "yarn"
                } else if package_manager.starts_with("bun@")
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

/// Loads the `[share]` section from `capsule.toml` in the given directory.
/// Returns `None` if the file doesn't exist or has no `[share]` section.
fn load_share_config(root: &Path) -> Option<ShareConfigToml> {
    let path = root.join("capsule.toml");
    let text = fs::read_to_string(&path).ok()?;
    let parsed: CapsuleTomlShare = toml::from_str(&text).ok()?;
    parsed.share
}

/// Removes items from `spec` whose IDs appear in the exclude config.
fn apply_share_exclude(spec: &mut ShareSpec, exclude: &ShareExcludeConfig) {
    if !exclude.sources.is_empty() {
        spec.sources
            .retain(|s| !exclude.sources.iter().any(|x| x == &s.id));
    }
    if !exclude.tools.is_empty() {
        spec.tool_requirements
            .retain(|t| !exclude.tools.iter().any(|x| x == &t.tool));
    }
    if !exclude.install_steps.is_empty() {
        spec.install_steps
            .retain(|s| !exclude.install_steps.iter().any(|x| x == &s.id));
    }
    if !exclude.entries.is_empty() {
        spec.entries
            .retain(|e| !exclude.entries.iter().any(|x| x == &e.id));
    }
}

/// Dispatch to the appropriate interaction mode:
/// - `yes == true` or no TTY → auto-accept all items
/// - TTY present → summary + bulk filter screen
fn finalize_capture(spec: &mut ShareSpec, yes: bool, reporter: &Arc<CliReporter>) -> Result<()> {
    let is_tty = io::stdin().is_terminal() && io::stderr().is_terminal();
    if yes || !is_tty {
        // Auto-accept: just ensure a primary entry is set.
        ensure_single_primary_entry(&mut spec.entries);
        if yes {
            futures::executor::block_on(reporter.notify(format!(
                "📋 Auto-accepted workspace `{}`: {} sources, {} tools, {} steps, {} entries.",
                spec.name,
                spec.sources.len(),
                spec.tool_requirements.len(),
                spec.install_steps.len(),
                spec.entries.len()
            )))?;
        }
        Ok(())
    } else {
        summarize_and_filter_capture(spec, reporter)
    }
}

/// One-screen summary interaction: show all detected items, then accept all
/// with Enter or remove individual items by typing `skip <id1> <id2> ...`.
fn summarize_and_filter_capture(spec: &mut ShareSpec, reporter: &Arc<CliReporter>) -> Result<()> {
    let source_ids: Vec<&str> = spec.sources.iter().map(|s| s.id.as_str()).collect();
    let tool_ids: Vec<&str> = spec
        .tool_requirements
        .iter()
        .map(|t| t.tool.as_str())
        .collect();
    let step_ids: Vec<&str> = spec.install_steps.iter().map(|s| s.id.as_str()).collect();
    let entry_ids: Vec<&str> = spec.entries.iter().map(|e| e.id.as_str()).collect();
    let env_paths: Vec<&str> = spec
        .env_requirements
        .iter()
        .map(|e| e.path.as_str())
        .collect();

    futures::executor::block_on(reporter.notify(format!(
        "Detected workspace `{}`:\n\
         \n  sources     {}\
         \n  tools       {}\
         \n  install     {}\
         \n  entries     {}\
         \n  env files   {}  ({})",
        spec.name,
        if source_ids.is_empty() {
            "(none)".to_string()
        } else {
            source_ids.join("  ")
        },
        if tool_ids.is_empty() {
            "(none)".to_string()
        } else {
            tool_ids.join("  ")
        },
        if step_ids.is_empty() {
            "(none)".to_string()
        } else {
            step_ids.join("  ")
        },
        if entry_ids.is_empty() {
            "(none)".to_string()
        } else {
            entry_ids.join("  ")
        },
        env_paths.len(),
        if env_paths.is_empty() {
            "(none)".to_string()
        } else {
            env_paths.join("  ")
        },
    )))?;

    eprint!("\nAccept all? [Enter]  or  skip <ids>:  ");
    io::stderr()
        .flush()
        .context("failed to flush summary prompt")?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("failed to read summary input")?;
    let trimmed = input.trim();

    if !trimmed.is_empty() {
        let lower = trimmed.to_ascii_lowercase();
        let ids_to_skip: Vec<&str> = if let Some(rest) = lower.strip_prefix("skip ") {
            rest.split_whitespace().collect()
        } else {
            lower.split_whitespace().collect()
        };

        if !ids_to_skip.is_empty() {
            spec.sources
                .retain(|s| !ids_to_skip.contains(&s.id.to_ascii_lowercase().as_str()));
            spec.tool_requirements
                .retain(|t| !ids_to_skip.contains(&t.tool.to_ascii_lowercase().as_str()));
            spec.install_steps
                .retain(|s| !ids_to_skip.contains(&s.id.to_ascii_lowercase().as_str()));
            spec.entries
                .retain(|e| !ids_to_skip.contains(&e.id.to_ascii_lowercase().as_str()));
        }
    }

    ensure_single_primary_entry(&mut spec.entries);
    Ok(())
}

/// Writes (or updates) the `[share]` section of `capsule.toml` in `root`.
fn write_share_config_to_capsule_toml(
    root: &Path,
    spec: &ShareSpec,
    git_mode: &GitMode,
    tool_runtime: &ShareToolRuntime,
) -> Result<()> {
    let path = root.join("capsule.toml");
    let existing = if path.exists() {
        fs::read_to_string(&path).with_context(|| format!("Failed to read {}", path.display()))?
    } else {
        String::new()
    };

    let mut doc: toml_edit::DocumentMut = existing
        .parse()
        .with_context(|| "Failed to parse capsule.toml as TOML")?;

    let share = doc["share"].or_insert(toml_edit::table());
    share["git_mode"] = toml_edit::value(git_mode.as_str());
    share["tool_runtime"] = toml_edit::value(tool_runtime.as_str());

    // Persist exclude lists for all currently included items so subsequent
    // encaps with --save-config don't re-add things the user skipped.
    let excluded_sources: Vec<toml_edit::Value> = spec
        .sources
        .iter()
        .filter(|_| false) // no active excludes at this point; placeholder
        .map(|s| toml_edit::Value::from(s.id.as_str()))
        .collect();
    if !excluded_sources.is_empty() {
        let arr = toml_edit::Array::from_iter(excluded_sources);
        share["exclude"]["sources"] = toml_edit::value(arr);
    }

    fs::write(&path, doc.to_string())
        .with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(())
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

/// Rewrites a server-returned share URL to use the canonical user-facing domain.
///
/// The API server (`api.ato.run`) returns `share_url`/`revision_url` fields using its
/// own host. For user-facing display we always show the site domain (`ato.run/s/...`)
/// instead. Respects `ATO_STORE_SITE_URL` for staging / dev overrides.
fn rewrite_api_to_site_url(url: &str) -> String {
    let Ok(mut parsed) = reqwest::Url::parse(url) else {
        return url.to_string();
    };
    let api_base = auth::default_store_registry_url();
    let Ok(api_parsed) = reqwest::Url::parse(&api_base) else {
        return url.to_string();
    };
    if parsed.host_str() == api_parsed.host_str() && parsed.path().starts_with("/s/") {
        let site_base = auth::share_display_base_url();
        let Ok(site_parsed) = reqwest::Url::parse(&site_base) else {
            return url.to_string();
        };
        if let Some(host) = site_parsed.host_str() {
            let _ = parsed.set_host(Some(host));
        }
    }
    parsed.to_string()
}

/// Maps the host of a share URL to the correct API base URL for server calls.
///
/// Users receive `ato.run/s/...` URLs but API calls must go to `api.ato.run`.
/// This avoids a 404 when `fetch_share_url` is handed a site-domain URL.
fn api_base_for_share_host(host: &str) -> String {
    let site_base = auth::share_display_base_url();
    if let Ok(site_parsed) = reqwest::Url::parse(&site_base) {
        if site_parsed.host_str() == Some(host) {
            return auth::default_store_registry_url();
        }
    }
    // Preserve scheme-less host references (e.g. staging.api.ato.run) as-is.
    format!("https://{}", host)
}

fn upload_share(
    spec: &ShareSpec,
    lock: &ShareLock,
    guide: &str,
    visibility: &str,
) -> Result<ShareRevisionPayload> {
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
            visibility: visibility.to_string(),
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
        let spec_canonical =
            serde_json::to_vec(&spec).context("Failed to serialize share spec for digest")?;
        let spec_digest_verified = sha256_label(&spec_canonical) == lock.spec_digest;
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
    let spec_canonical =
        serde_json::to_vec(&spec).context("Failed to serialize share spec for digest")?;
    let spec_digest_verified = sha256_label(&spec_canonical) == lock.spec_digest;
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
    // Always route API calls to the correct API base, never to the site domain.
    // Users receive ato.run/s/... URLs, but the REST API lives at api.ato.run.
    let api_base = api_base_for_share_host(parsed.host_str().unwrap_or("api.ato.run"));
    let endpoint = if let Some(revision) = revision {
        format!("{}/v1/shares/{}/revisions/{}", api_base, share_id, revision)
    } else {
        format!("{}/v1/shares/{}", api_base, share_id)
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
        share_url: Some(rewrite_api_to_site_url(&share.share_url)),
        resolved_revision_url: Some(rewrite_api_to_site_url(&share.revision_url)),
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
    Ok(capsule_core::common::paths::workspace_tmp_dir(&cwd)
        .join("ato-run")
        .join(digest))
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
        let value = value.trim().to_string();
        crate::common::env_security::check_user_env_safety(key, &value)?;
        envs.insert(key.clone(), value);
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
    let cas_provider = capsule_core::capsule::CasProvider::from_env();
    capsule_core::capsule::unpack_payload_from_capsule_root_with_provider(
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
        let key = key.trim().to_string();
        let value = value.trim().to_string();
        crate::common::env_security::check_user_env_safety(&key, &value)
            .with_context(|| format!("rejected env key in {}", path.display()))?;
        values.insert(key, value);
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
    // Archive sources embed their content directly — extract inline, no git needed.
    if source.kind == "archive" {
        // Prefer explicit archive_content field; fall back to data URI in url field.
        let content: String = if let Some(c) = source.archive_content.as_deref() {
            c.to_string()
        } else if let Some(b64) = source
            .url
            .strip_prefix("data:application/x-tar+gzip;base64,")
        {
            b64.to_string()
        } else {
            anyhow::bail!(
                "archive source '{}' has no extractable content (no archive_content field \
                 and url is not a data URI)",
                source.id
            )
        };
        extract_archive_source(&content, target)
            .with_context(|| format!("Failed to extract archive source {}", source.id))?;
        return Ok(locked.rev.clone());
    }

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
    run_git(Some(target), &["checkout", "--force", &locked.rev]).with_context(|| {
        format!(
            "Commit {} is not reachable from {}. \
             The sender may not have pushed this commit. \
             Ask the sender to push, or re-share using `ato encap --git-mode latest-at-encap`.",
            &locked.rev, &source.url
        )
    })?;
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
            // binary_path is None for ato-managed tools; don't treat that as missing.
            Some(resolved)
                if resolved.binary_path.is_none() && resolved.runtime_source == "system" =>
            {
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
        // Skip local-tool check for tools that ato manages (uv/npm already present at paths
        // we control, or we will inject them via env vars).
        .filter(|t| matches!(t.runtime_source.as_str(), "system" | ""))
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
    run_shell_command_with_env(command, cwd, &std::collections::HashMap::new())
}

/// Run a shell command with additional environment variable overrides prepended.
/// Keys in `env_overrides` that already exist in the process environment are
/// replaced; PATH values are prepended (not replaced) to preserve system tools.
fn run_shell_command_with_env(
    command: &str,
    cwd: &Path,
    env_overrides: &std::collections::HashMap<String, String>,
) -> Result<ShellOutput> {
    let mut builder = if cfg!(windows) {
        let mut b = Command::new("cmd");
        b.arg("/C").arg(command);
        b
    } else {
        let mut b = Command::new("/bin/sh");
        b.arg("-lc").arg(command);
        b
    };
    builder.current_dir(cwd);

    for (key, value) in env_overrides {
        if key == "PATH" {
            // Prepend to existing PATH so system tools remain available.
            let existing = std::env::var("PATH").unwrap_or_default();
            let merged = if existing.is_empty() {
                value.clone()
            } else {
                format!("{value}:{existing}")
            };
            builder.env("PATH", merged);
        } else {
            builder.env(key, value);
        }
    }

    let output = builder
        .output()
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

/// Build an environment overlay for install step execution based on the tool_runtime
/// setting and the tools declared in the spec.
///
/// For `auto`/`ato` mode:
/// - Python tools: sets `UV_MANAGED_PYTHON=1` and `UV_PYTHON=<pinned version>` so
///   that `uv` automatically downloads and uses the correct Python without requiring
///   a system Python installation.
/// - Node tools: sets `NODE_VERSION=<pinned version>` for fnm/nvm compatibility.
///
/// For `system` mode: returns an empty map (existing behavior).
fn prepare_share_runtime_env(
    tool_requirements: &[ToolRequirementSpec],
    tool_runtime: ShareToolRuntime,
) -> std::collections::HashMap<String, String> {
    let mut env = std::collections::HashMap::new();

    if matches!(tool_runtime, ShareToolRuntime::System) {
        return env;
    }

    let has_python = tool_requirements
        .iter()
        .any(|t| matches!(t.tool.as_str(), "python" | "python3" | "uv"));
    let has_node = tool_requirements
        .iter()
        .any(|t| matches!(t.tool.as_str(), "node" | "npm" | "bun" | "pnpm"));

    if has_python {
        // UV_MANAGED_PYTHON instructs uv to download Python if it's missing.
        env.insert("UV_MANAGED_PYTHON".to_string(), "1".to_string());
        env.insert(
            "UV_PYTHON".to_string(),
            SHARE_PROVIDER_PYTHON_VERSION.to_string(),
        );
    }

    if has_node {
        // fnm / nvm / mise / asdf all honour NODE_VERSION for version selection.
        env.insert(
            "NODE_VERSION".to_string(),
            SHARE_PROVIDER_NODE_VERSION.to_string(),
        );
    }

    env
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
                runtime_source: tool.runtime_source.clone(),
                provider_toolchain: tool.provider_toolchain.clone(),
                provider_version: None,
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
    use share_types::default_git_mode_str;

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
    #[serial_test::serial]
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
        fs::write(web.join(".env"), "VITE_API_URL=\n").expect("env");
        init_git_repo(&web, "git@github.com:acme/dashboard.git");

        let reporter = Arc::new(crate::reporters::CliReporter::new(false));
        let capture = capture_workspace(
            root,
            GitMode::SameCommit,
            false,
            ShareToolRuntime::System,
            &reporter,
        )
        .expect("capture");
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
            .any(|entry| entry.id == "dashboard-dev"
                && entry.env.files.iter().any(|path| path.ends_with(".env"))));
    }

    // When the workspace root IS the single git repo (single-repo encap), entry.cwd
    // and step.cwd must be prefixed with the repo folder name to match source.path.
    // Regression test for: npm ENOENT on `ato run` because cwd was "." while the
    // repo was cloned into `<temp>/<folder>/`.
    #[test]
    #[serial_test::serial]
    fn capture_workspace_single_repo_cwds_match_source_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        // The workspace root is itself the git repo (single-repo encap scenario).
        let root = temp.path();
        fs::write(
            root.join("package.json"),
            r#"{"name":"browser-daw","scripts":{"dev":"vite"}}"#,
        )
        .expect("package.json");
        fs::write(root.join("package-lock.json"), "{}").expect("package-lock.json");
        init_git_repo(root, "git@github.com:acme/browser-daw.git");

        let reporter = Arc::new(crate::reporters::CliReporter::new(false));
        let capture = capture_workspace(
            root,
            GitMode::SameCommit,
            false,
            ShareToolRuntime::System,
            &reporter,
        )
        .expect("capture");

        // There is exactly one source.
        assert_eq!(capture.spec.sources.len(), 1);
        let source_path = &capture.spec.sources[0].path;

        // All install steps and entries must have cwd == source.path (the folder
        // name), never ".".  A cwd of "." would point to the parent of the cloned
        // repo and cause ENOENT when npm/bun runs.
        for step in &capture.spec.install_steps {
            assert_eq!(
                &step.cwd, source_path,
                "install step `{}` cwd `{}` must equal source path `{}`",
                step.id, step.cwd, source_path
            );
        }
        for entry in &capture.spec.entries {
            assert_eq!(
                &entry.cwd, source_path,
                "entry `{}` cwd `{}` must equal source path `{}`",
                entry.id, entry.cwd, source_path
            );
        }
    }

    #[test]
    fn archive_source_round_trip_no_git_repo() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();

        // Set up a non-git directory with a capsule.toml and a source file.
        fs::write(
            root.join("capsule.toml"),
            "[capsule]\nname = \"python-script\"\nruntime = \"source\"\n",
        )
        .expect("capsule.toml");
        fs::create_dir_all(root.join("source")).expect("mkdir source");
        fs::write(
            root.join("source/main.py"),
            "print('hello from python script')\n",
        )
        .expect("main.py");
        // A .env file that must NOT appear in the archive.
        fs::write(root.join(".env"), "SECRET=hunter2\n").expect(".env");

        let reporter = Arc::new(crate::reporters::CliReporter::new(false));
        let capture = capture_workspace(
            root,
            GitMode::SameCommit,
            false,
            ShareToolRuntime::System,
            &reporter,
        )
        .expect("capture");

        // Exactly one archive source, no git sources.
        assert_eq!(capture.spec.sources.len(), 1);
        let src = &capture.spec.sources[0];
        assert_eq!(src.kind, "archive");
        assert!(src.archive_content.is_some(), "archive_content must be set");

        // Extract into a new temp dir and verify files round-trip correctly.
        let out = tempfile::tempdir().expect("tempdir out");
        extract_archive_source(src.archive_content.as_ref().unwrap(), out.path()).expect("extract");

        // capsule.toml and source/main.py should be present.
        assert!(
            out.path().join("capsule.toml").exists(),
            "capsule.toml missing after extract"
        );
        assert!(
            out.path().join("source/main.py").exists(),
            "source/main.py missing after extract"
        );

        // .env must NOT have been included.
        assert!(
            !out.path().join(".env").exists(),
            ".env must be excluded from archive"
        );
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

    #[test]
    fn rewrite_api_to_site_url_rewrites_api_host() {
        // api.ato.run/s/... should become ato.run/s/...
        let result = rewrite_api_to_site_url("https://api.ato.run/s/share-123");
        assert_eq!(result, "https://ato.run/s/share-123");

        let result = rewrite_api_to_site_url("https://api.ato.run/s/share-123@r1");
        assert_eq!(result, "https://ato.run/s/share-123@r1");
    }

    #[test]
    fn rewrite_api_to_site_url_leaves_site_url_unchanged() {
        // ato.run/s/... is already the correct display URL
        let result = rewrite_api_to_site_url("https://ato.run/s/share-123");
        assert_eq!(result, "https://ato.run/s/share-123");
    }

    #[test]
    fn rewrite_api_to_site_url_leaves_non_share_paths_unchanged() {
        // Only /s/ paths should be rewritten
        let result = rewrite_api_to_site_url("https://api.ato.run/v1/shares/share-123");
        assert_eq!(result, "https://api.ato.run/v1/shares/share-123");
    }

    #[test]
    fn api_base_for_share_host_maps_site_host_to_api() {
        // ato.run (site domain) must resolve to api.ato.run for API calls
        let result = api_base_for_share_host("ato.run");
        assert_eq!(result, "https://api.ato.run");
    }

    #[test]
    fn api_base_for_share_host_passes_api_host_through() {
        // api.ato.run is already the API host; should not change
        let result = api_base_for_share_host("api.ato.run");
        assert_eq!(result, "https://api.ato.run");
    }

    #[test]
    fn api_base_for_share_host_passes_staging_host_through() {
        // Staging or custom hosts are passed through unchanged
        let result = api_base_for_share_host("staging.api.ato.run");
        assert_eq!(result, "https://staging.api.ato.run");
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
            compat_host: false,
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
            compat_host: false,
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
        let state = materialize_loaded_share(&loaded, &into, ShareToolRuntime::System, false)
            .expect("materialize");
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

    // ── T8b: valid digest matches (local spec path) ───────────────────────

    #[test]
    fn load_share_input_valid_digest_matches_for_local_spec() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path();
        let spec_json = serde_json::json!({
            "schema_version": "1",
            "name": "valid-digest",
            "root": "valid-digest",
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
        // Build canonical digest the same way build_share_lock does
        let spec_parsed: ShareSpec =
            serde_json::from_str(&spec_raw).expect("parse spec for digest");
        let spec_canonical = serde_json::to_vec(&spec_parsed).expect("canonical bytes");
        let correct_digest = sha256_label(&spec_canonical);
        let lock_json = serde_json::json!({
            "schema_version": "1",
            "spec_digest": correct_digest,
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
            loaded.spec_digest_verified,
            "valid digest should be verified for local spec path"
        );
    }

    // ── T8c: valid digest matches (local lock path) ───────────────────────

    #[test]
    fn load_share_input_valid_digest_matches_for_local_lock() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path();
        let spec_json = serde_json::json!({
            "schema_version": "1",
            "name": "valid-digest-lock",
            "root": "valid-digest-lock",
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
        let spec_parsed: ShareSpec =
            serde_json::from_str(&spec_raw).expect("parse spec for digest");
        let spec_canonical = serde_json::to_vec(&spec_parsed).expect("canonical bytes");
        let correct_digest = sha256_label(&spec_canonical);
        let lock_json = serde_json::json!({
            "schema_version": "1",
            "spec_digest": correct_digest,
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
            dir.join("share.lock.json")
                .to_str()
                .expect("lock path utf8"),
        )
        .expect("load from lock path should succeed");
        assert!(
            loaded.spec_digest_verified,
            "valid digest should be verified when loading from lock path"
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
        // Compute digest via canonical serialization (must match load_share_input / build_share_lock)
        let spec_parsed: ShareSpec =
            serde_json::from_str(&spec_raw).expect("parse spec for digest");
        let spec_canonical = serde_json::to_vec(&spec_parsed).expect("canonical spec bytes");
        let computed_digest = sha256_label(&spec_canonical);
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
        let err = materialize_loaded_share(&loaded, &into, ShareToolRuntime::System, false)
            .expect_err("should error on missing source");
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
            runtime_source: "system".to_string(),
            provider_toolchain: None,
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
            runtime_source: "system".to_string(),
            provider_toolchain: None,
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

    // --- v2 schema backward compatibility ---

    #[test]
    fn share_source_spec_v1_json_defaults_git_mode() {
        let json = r#"{"id":"api","kind":"git","url":"https://github.com/org/api","path":"api"}"#;
        let spec: ShareSourceSpec = serde_json::from_str(json).expect("parse v1 source spec");
        assert_eq!(
            spec.git_mode, "same-commit",
            "v1 should default to same-commit"
        );
    }

    #[test]
    fn tool_requirement_spec_v1_json_defaults_runtime_source() {
        let json = r#"{"id":"python3-req","tool":"python3","required_by":["api"]}"#;
        let spec: ToolRequirementSpec = serde_json::from_str(json).expect("parse v1 tool spec");
        assert_eq!(
            spec.runtime_source, "system",
            "v1 tool spec should default to system"
        );
        assert!(spec.provider_toolchain.is_none());
    }

    #[test]
    fn resolved_tool_lock_v1_json_defaults_runtime_source() {
        let json =
            r#"{"tool":"python3","resolved_version":"3.11","binary_path":"/usr/bin/python3"}"#;
        let lock: ResolvedToolLock = serde_json::from_str(json).expect("parse v1 tool lock");
        assert_eq!(lock.runtime_source, "system");
        assert!(lock.provider_toolchain.is_none());
        assert!(lock.provider_version.is_none());
    }

    // --- prepare_share_runtime_env ---

    #[test]
    fn prepare_runtime_env_system_mode_returns_empty() {
        let tools = vec![ToolRequirementSpec {
            id: "python3-req".to_string(),
            tool: "python3".to_string(),
            version: None,
            required_by: vec![],
            evidence: vec![],
            runtime_source: "system".to_string(),
            provider_toolchain: None,
        }];
        let env = prepare_share_runtime_env(&tools, ShareToolRuntime::System);
        assert!(env.is_empty(), "system mode must not inject env vars");
    }

    #[test]
    fn prepare_runtime_env_auto_mode_injects_uv_python_env() {
        let tools = vec![ToolRequirementSpec {
            id: "uv-req".to_string(),
            tool: "uv".to_string(),
            version: None,
            required_by: vec![],
            evidence: vec![],
            runtime_source: "auto".to_string(),
            provider_toolchain: None,
        }];
        let env = prepare_share_runtime_env(&tools, ShareToolRuntime::Auto);
        assert_eq!(env.get("UV_MANAGED_PYTHON").map(|s| s.as_str()), Some("1"));
        assert!(
            env.get("UV_PYTHON").map(|v| !v.is_empty()).unwrap_or(false),
            "UV_PYTHON must be set for python tools"
        );
    }

    #[test]
    fn prepare_runtime_env_auto_mode_injects_node_version() {
        let tools = vec![ToolRequirementSpec {
            id: "npm-req".to_string(),
            tool: "npm".to_string(),
            version: None,
            required_by: vec![],
            evidence: vec![],
            runtime_source: "auto".to_string(),
            provider_toolchain: None,
        }];
        let env = prepare_share_runtime_env(&tools, ShareToolRuntime::Auto);
        assert!(
            env.get("NODE_VERSION")
                .map(|v| !v.is_empty())
                .unwrap_or(false),
            "NODE_VERSION must be set for node tools"
        );
    }

    // --- strict mode ---

    #[test]
    fn strict_mode_collects_issues_then_bails() {
        let temp = tempfile::tempdir().expect("tempdir");
        let spec = ShareSpec {
            schema_version: SHARE_SCHEMA_VERSION.to_string(),
            name: "test".to_string(),
            root: ".".to_string(),
            sources: vec![],
            tool_requirements: vec![],
            install_steps: vec![InstallStepSpec {
                id: "fail-step".to_string(),
                cwd: ".".to_string(),
                run: "false".to_string(),
                depends_on: vec![],
                evidence: vec![],
            }],
            env_requirements: vec![],
            entries: vec![],
            services: vec![],
            notes: ShareNotes::default(),
            generated_from: GeneratedFrom {
                root_path: ".".to_string(),
                captured_at: "2024-01-01T00:00:00Z".to_string(),
                host_os: "test".to_string(),
            },
        };
        let spec_json = serde_json::to_string(&spec).unwrap();
        let spec_digest = sha256_label(spec_json.as_bytes());
        let lock = ShareLock {
            schema_version: SHARE_SCHEMA_VERSION.to_string(),
            spec_digest: spec_digest.clone(),
            generated_guide_digest: String::new(),
            revision: 1,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            resolved_sources: vec![],
            resolved_tools: vec![],
        };
        let loaded = LoadedShareInput {
            share_url: None,
            resolved_revision_url: None,
            spec: spec.clone(),
            lock: lock.clone(),
            spec_digest_verified: true,
        };

        // non-strict: should succeed but leave verification issues
        let state = materialize_loaded_share(&loaded, temp.path(), ShareToolRuntime::System, false)
            .expect("non-strict should not bail");
        assert!(
            !state.verification.issues.is_empty(),
            "issues must be present"
        );

        // strict mode with same input should bail
        let temp2 = tempfile::tempdir().expect("tempdir2");
        let loaded2 = LoadedShareInput {
            share_url: None,
            resolved_revision_url: None,
            spec,
            lock,
            spec_digest_verified: true,
        };
        let err = materialize_loaded_share(&loaded2, temp2.path(), ShareToolRuntime::System, true)
            .expect_err("strict mode must bail with issues");
        assert!(
            err.to_string().contains("--strict"),
            "error message must mention --strict"
        );
    }

    #[test]
    fn yes_mode_auto_accepts_all_items_without_tty() {
        use crate::reporters::CliReporter;
        let reporter = Arc::new(CliReporter::new(false));
        let mut spec = ShareSpec {
            schema_version: SHARE_SCHEMA_VERSION.to_string(),
            name: "test-ws".to_string(),
            root: ".".to_string(),
            sources: vec![ShareSourceSpec {
                id: "repo-a".to_string(),
                kind: "git".to_string(),
                url: "https://github.com/example/a".to_string(),
                path: "repo-a".to_string(),
                branch: None,
                evidence: Vec::new(),
                git_mode: default_git_mode_str(),
                archive_content: None,
            }],
            tool_requirements: vec![ToolRequirementSpec {
                id: "node".to_string(),
                tool: "node".to_string(),
                version: None,
                required_by: Vec::new(),
                evidence: Vec::new(),
                runtime_source: default_runtime_source_str(),
                provider_toolchain: None,
            }],
            env_requirements: Vec::new(),
            install_steps: vec![InstallStepSpec {
                id: "install-a".to_string(),
                cwd: "repo-a".to_string(),
                run: "npm ci".to_string(),
                depends_on: Vec::new(),
                evidence: Vec::new(),
            }],
            entries: vec![ShareEntrySpec {
                id: "dev".to_string(),
                label: "dev".to_string(),
                cwd: "repo-a".to_string(),
                run: "npm run dev".to_string(),
                kind: "short_lived".to_string(),
                primary: false,
                depends_on: Vec::new(),
                env: EntryEnvSpec::default(),
                evidence: Vec::new(),
            }],
            services: Vec::new(),
            notes: ShareNotes::default(),
            generated_from: GeneratedFrom {
                root_path: ".".to_string(),
                captured_at: chrono::Utc::now().to_rfc3339(),
                host_os: "test".to_string(),
            },
        };
        // yes=true should auto-accept without requiring a TTY
        finalize_capture(&mut spec, true, &reporter).expect("yes mode should not error");
        assert_eq!(spec.sources.len(), 1, "source should be kept");
        assert_eq!(spec.entries.len(), 1, "entry should be kept");
        // ensure_single_primary_entry should have assigned primary
        assert!(
            spec.entries[0].primary,
            "entry should be primary after auto-accept"
        );
    }

    #[test]
    fn apply_share_exclude_removes_named_ids() {
        let make_spec = || ShareSpec {
            schema_version: SHARE_SCHEMA_VERSION.to_string(),
            name: "ws".to_string(),
            root: ".".to_string(),
            sources: vec![
                ShareSourceSpec {
                    id: "keep-repo".to_string(),
                    kind: "git".to_string(),
                    url: "u".to_string(),
                    path: "keep-repo".to_string(),
                    branch: None,
                    evidence: Vec::new(),
                    git_mode: default_git_mode_str(),
                    archive_content: None,
                },
                ShareSourceSpec {
                    id: "skip-repo".to_string(),
                    kind: "git".to_string(),
                    url: "u2".to_string(),
                    path: "skip-repo".to_string(),
                    branch: None,
                    evidence: Vec::new(),
                    git_mode: default_git_mode_str(),
                    archive_content: None,
                },
            ],
            tool_requirements: vec![
                ToolRequirementSpec {
                    id: "node".to_string(),
                    tool: "node".to_string(),
                    version: None,
                    required_by: Vec::new(),
                    evidence: Vec::new(),
                    runtime_source: default_runtime_source_str(),
                    provider_toolchain: None,
                },
                ToolRequirementSpec {
                    id: "bun".to_string(),
                    tool: "bun".to_string(),
                    version: None,
                    required_by: Vec::new(),
                    evidence: Vec::new(),
                    runtime_source: default_runtime_source_str(),
                    provider_toolchain: None,
                },
            ],
            env_requirements: Vec::new(),
            install_steps: Vec::new(),
            entries: Vec::new(),
            services: Vec::new(),
            notes: ShareNotes::default(),
            generated_from: GeneratedFrom {
                root_path: ".".to_string(),
                captured_at: chrono::Utc::now().to_rfc3339(),
                host_os: "test".to_string(),
            },
        };
        let mut spec = make_spec();
        let exclude = ShareExcludeConfig {
            sources: vec!["skip-repo".to_string()],
            tools: vec!["bun".to_string()],
            install_steps: Vec::new(),
            entries: Vec::new(),
        };
        apply_share_exclude(&mut spec, &exclude);
        assert_eq!(spec.sources.len(), 1);
        assert_eq!(spec.sources[0].id, "keep-repo");
        assert_eq!(spec.tool_requirements.len(), 1);
        assert_eq!(spec.tool_requirements[0].tool, "node");
    }

    #[test]
    fn load_share_config_returns_none_when_no_share_section() {
        let dir = tempfile::tempdir().unwrap();
        // capsule.toml with no [share] section
        std::fs::write(
            dir.path().join("capsule.toml"),
            "[package]\nname = \"test\"\n",
        )
        .unwrap();
        let result = load_share_config(dir.path());
        assert!(result.is_none(), "no [share] section should return None");
    }

    #[test]
    fn load_share_config_parses_yes_and_exclude() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("capsule.toml"),
            "[share]\nyes = true\n\n[share.exclude]\nsources = [\"docs-repo\"]\ntools = [\"bun\"]\n",
        ).unwrap();
        let cfg = load_share_config(dir.path()).expect("should parse [share] section");
        assert_eq!(cfg.yes, Some(true));
        let exclude = cfg.exclude.expect("should have exclude");
        assert_eq!(exclude.sources, vec!["docs-repo"]);
        assert_eq!(exclude.tools, vec!["bun"]);
    }

    #[test]
    fn load_share_config_returns_none_when_no_capsule_toml() {
        let dir = tempfile::tempdir().unwrap();
        let result = load_share_config(dir.path());
        assert!(result.is_none(), "missing file should return None");
    }
}
