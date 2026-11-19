//! Public preparation is local orchestration over immutable Capsules.

mod formation;
mod mcp;
mod operator;
mod store;
mod verify;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, IsTerminal, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use ato_formation::authoring::{HTTP_CONTRACT_VERIFIER, PlatformDraft};
use ato_formation::browser::{BrowserContractV0, effective_contract_ref};
use ato_formation::source::{FileVerifiedArchive, SourceLimits};
use ato_portable_application::{build_authored_bundle_v2, build_static_bundle, bundle_sha256};
use ato_runtime_attempt::browser_verify::OllamaJudgeConfig;
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{hub, hub_public, unix_millis};
use store::{Entry, Store};
use verify::Attempt;
pub(super) use verify::worker;

const MAX_SOURCE: u64 = 32 * 1024 * 1024;
const MAX_ARTIFACT: u64 = hub_public::MAX_BUNDLE_BYTES;
const DEFAULT_ATTEMPTS: usize = 4;
const DEFAULT_BUDGET_MS: u64 = 180_000;

#[derive(Subcommand)]
pub(super) enum PublicationCommand {
    /// Serve the owner-scoped publication tools over MCP stdio.
    Mcp(WorkspaceArgs),
    /// Freeze a source directory or a portable application bundle.
    Prepare {
        /// JSON request following the publication_prepare tool schema.
        request: PathBuf,
        #[command(flatten)]
        workspace: WorkspaceArgs,
    },
    /// Start the bounded verification job.
    Start(Id),
    /// Read the current stage and verification report.
    Status(Id),
    /// Wait briefly for a state change, then return the current status.
    Wait(WaitArgs),
    /// Request cancellation of verification.
    Cancel(Id),
    /// Observe the currently active contained browser session.
    BrowserObserve(Id),
    /// Submit a structured browser action from a JSON file.
    BrowserAct { request: PathBuf },
    /// Review and approve this exact report and destination on a terminal.
    Approve(ReportId),
    /// Publish the approved artifact and return instructions for recipients.
    Commit(ReportId),
    /// Reconcile a public Hub upload whose response was lost.
    Reconcile(Id),
}

#[derive(Args)]
pub(super) struct WorkspaceArgs {
    /// Limit source and bundle inputs to this directory.
    #[arg(long, default_value = ".")]
    workspace: PathBuf,
}

#[derive(Args, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Id {
    preparation_id: String,
}

#[derive(Args, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WaitArgs {
    preparation_id: String,
    #[arg(long, default_value_t = 2000)]
    #[serde(default = "default_wait")]
    wait_ms: u64,
}

fn default_wait() -> u64 {
    2000
}

#[derive(Args, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReportId {
    preparation_id: String,
    #[arg(long)]
    report_ref: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Destination {
    Public {
        api_url: String,
        name: String,
        tag: String,
        license: String,
    },
    Link {
        hub_url: String,
        #[serde(default)]
        allow_loopback_http: bool,
    },
}

impl Destination {
    fn validate(&mut self) -> Result<()> {
        match self {
            Self::Public {
                api_url,
                name,
                tag,
                license,
            } => {
                *api_url = hub_public::api_base(api_url)?;
                hub_public::parse_owner_name(name)?;
                ensure!(hub_public::is_valid_tag(tag), "Invalid publication tag");
                ensure!(
                    license.len() <= 100
                        && !license.is_empty()
                        && license
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"-+.() ".contains(&b)),
                    "Enter the SPDX license identifier or expression for this artifact"
                );
            }
            Self::Link {
                hub_url,
                allow_loopback_http,
            } => {
                *hub_url = hub::resolve_hub_base(hub_url, *allow_loopback_http)?;
            }
        }
        Ok(())
    }

    fn recipient_scope(&self) -> &'static str {
        match self {
            Self::Public { .. } => {
                "Anyone with access to the public Hub can obtain this publication."
            }
            Self::Link { .. } => {
                "Anyone who receives the complete encrypted link can decrypt the bundle."
            }
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HttpCheck {
    id: String,
    path: String,
    status: u16,
    body_sha256: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Prepare {
    source: PathBuf,
    title: Option<String>,
    destination: Destination,
    #[serde(default)]
    http_checks: Vec<HttpCheck>,
    #[serde(default)]
    process_routes: Vec<formation::ProcessRoute>,
    browser_task: Option<String>,
    browser_interactive: Option<bool>,
    target_host: Option<String>,
    #[serde(default)]
    unsupported_required: Vec<String>,
    #[serde(default = "default_attempts")]
    max_attempts: usize,
    #[serde(default = "default_budget")]
    budget_ms: u64,
}

fn default_attempts() -> usize {
    DEFAULT_ATTEMPTS
}
fn default_budget() -> u64 {
    DEFAULT_BUDGET_MS
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    source_kind: String,
    source_ref: String,
    artifact_sha256: String,
    artifact_size: u64,
    base_contract_ref: String,
    effective_contract_ref: String,
    browser_contract: Option<BrowserContractV0>,
    browser_interactive: bool,
    browser_models: Option<BrowserModels>,
    target_host: String,
    derivations: Vec<String>,
    declared_routes: Vec<DeclaredRoute>,
    checks: Vec<Value>,
    blockers: Vec<String>,
    bindings: Vec<String>,
    artifact_review: ArtifactReview,
    destination: Destination,
    max_attempts: usize,
    budget_ms: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeclaredRoute {
    derivation_ref: String,
    realization: String,
    platforms: Vec<PlatformDraft>,
    runtimes: BTreeMap<String, String>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserModels {
    browser_driver: String,
    judge: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ollama_judge: Option<OllamaJudgeConfig>,
}

fn browser_models(interactive: bool) -> Result<BrowserModels> {
    let ollama_judge = match (
        std::env::var("ATO_OLLAMA_JUDGE_URL").ok(),
        std::env::var("ATO_OLLAMA_JUDGE_MODEL").ok(),
        std::env::var("ATO_OLLAMA_JUDGE_DIGEST").ok(),
    ) {
        (None, None, None) => None,
        (Some(url), Some(model), Some(digest)) if interactive => {
            Some(OllamaJudgeConfig::new(&url, model, digest)?)
        }
        _ => bail!(
            "Configure the Ollama judge URL, model and digest together for structured browser verification"
        ),
    };
    Ok(BrowserModels {
        browser_driver: if interactive {
            "structured_mcp_operator".into()
        } else {
            std::env::var("ATO_BROWSER_AGENT_MODEL")
                .unwrap_or_else(|_| "deepseek/deepseek-flash".into())
        },
        judge: ollama_judge.as_ref().map_or_else(
            || std::env::var("ATO_JEV_MODEL").unwrap_or_else(|_| "jev-1.13.0".into()),
            OllamaJudgeConfig::identity,
        ),
        ollama_judge,
    })
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactReview {
    instance_snapshot: bool,
    sensitive_path_count: usize,
    secret_or_publisher_data: String,
    dependency_profiles: Vec<String>,
}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Prepared,
    Queued,
    Verifying,
    Ready,
    Unmet,
    Inconclusive,
    Cancelled,
    Publishing,
    Published,
    PublicationUnknown,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Approval {
    report_ref: String,
    approved_at_ms: u64,
}

pub(super) fn run(command: PublicationCommand) -> Result<()> {
    let store = Store::open()?;
    let result = match command {
        PublicationCommand::Mcp(args) => return mcp::serve(store, &args.workspace),
        PublicationCommand::Prepare { request, workspace } => {
            let file = fs::File::open(request)?;
            let input = read_limited(file, 128 * 1024)?;
            prepare(
                &store,
                &workspace.workspace,
                serde_json::from_slice(&input)?,
            )?
        }
        PublicationCommand::Start(args) => start(&store, &args.preparation_id)?,
        PublicationCommand::Status(args) => status(&store, &args.preparation_id)?,
        PublicationCommand::Wait(args) => wait(&store, &args.preparation_id, args.wait_ms)?,
        PublicationCommand::Cancel(args) => cancel(&store, &args.preparation_id)?,
        PublicationCommand::BrowserObserve(args) => {
            operator::observe(&store, &args.preparation_id)?
        }
        PublicationCommand::BrowserAct { request } => {
            let bytes = read_limited(fs::File::open(request)?, 32 * 1024)?;
            operator::act(&store, serde_json::from_slice(&bytes)?)?
        }
        PublicationCommand::Approve(args) => approve(&store, args)?,
        PublicationCommand::Commit(args) => commit(&store, args)?,
        PublicationCommand::Reconcile(args) => reconcile(&store, &args.preparation_id)?,
    };
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn limited_text(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

fn prepare(store: &Store, workspace: &Path, mut request: Prepare) -> Result<Value> {
    request.destination.validate()?;
    let target_host = request
        .target_host
        .take()
        .unwrap_or_else(ato_formation_worker::local::host_triple);
    ensure!(
        limited_text(&target_host, 64)
            && target_host.contains('-')
            && target_host
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_".contains(&b)),
        "Use a target host triple such as x86_64-unknown-linux-gnu"
    );
    ensure!(
        (1..=8).contains(&request.max_attempts) && (1000..=600_000).contains(&request.budget_ms),
        "Use 1–8 attempts and a total verification budget of 1–600 seconds"
    );
    ensure!(
        request.http_checks.len() <= 16 && request.unsupported_required.len() <= 16,
        "Too many conditions"
    );
    ensure!(
        request.process_routes.len() <= 4,
        "Propose at most four Python process routes"
    );
    let mut ids = BTreeSet::new();
    for check in &request.http_checks {
        ensure!(
            limited_text(&check.id, 64)
                && check
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
                && ids.insert(&check.id),
            "HTTP check identifiers must be unique and use letters, digits, hyphens, underscores, or dots"
        );
        ensure!(
            limited_text(&check.path, 1024)
                && check.path.starts_with('/')
                && !check.path.starts_with("//")
                && !check.path.contains(['?', '#', '\\'])
                && (100..=599).contains(&check.status),
            "A check needs an absolute app path without query or fragment, and an HTTP status"
        );
        if let Some(digest) = &check.body_sha256 {
            hub_public::parse_digest(digest).map_err(anyhow::Error::msg)?;
        }
    }
    for description in &request.unsupported_required {
        ensure!(
            limited_text(description, 250),
            "Use a short description for each unsupported check"
        );
    }
    if let Some(task) = &request.browser_task {
        ensure!(!task.contains('\0'), "Invalid browser task");
    }
    let browser_contract = request
        .browser_task
        .as_deref()
        .map(BrowserContractV0::from_prompt)
        .transpose()?;
    let browser_interactive = request
        .browser_interactive
        .unwrap_or(browser_contract.is_some());
    ensure!(
        !browser_interactive || browser_contract.is_some(),
        "A browser session needs a frozen browser acceptance task"
    );
    let browser_models = browser_contract
        .as_ref()
        .map(|_| browser_models(browser_interactive))
        .transpose()?;
    if let Some(models) = &browser_models {
        ensure!(
            [&models.browser_driver, &models.judge]
                .into_iter()
                .all(|value| {
                    limited_text(value, 128)
                        && value
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"-_./".contains(&b))
                })
                && models.browser_driver != models.judge,
            "Configure a distinct judge and browser driver identity"
        );
    }
    let workspace = workspace
        .canonicalize()
        .context("Cannot open the authorized workspace")?;
    ensure!(
        workspace.is_dir(),
        "The authorized workspace must be a directory"
    );
    let input = workspace
        .join(&request.source)
        .canonicalize()
        .context("Cannot open the requested source")?;
    ensure!(
        input.starts_with(&workspace),
        "The requested source is outside the authorized workspace"
    );

    let temporary = tempfile::tempdir_in(store.root())?;
    let (bytes, source_kind, source_ref) = if input.is_dir() {
        let limits = SourceLimits::default();
        let mut file = ato_formation_worker::local::snapshot_directory_file(
            &input,
            temporary.path(),
            MAX_SOURCE,
        )?;
        file.rewind()?;
        let archive = read_limited(file.try_clone()?, MAX_SOURCE)?;
        let archive_size = u64::try_from(archive.len())?;
        let mut frozen =
            FileVerifiedArchive::verify(file, &bundle_sha256(&archive), archive_size, limits)?;
        let source_ref = frozen.closure_ref("")?.to_string();
        let path = frozen.materialize(&temporary.path().join("source"), "", limits)?;
        let manifest = path.join("capsule.toml");
        let bytes = if manifest.is_file() {
            ensure!(
                request.process_routes.is_empty(),
                "Use the execution routes declared by the authored manifest"
            );
            let document: toml::Value = toml::from_str(&fs::read_to_string(&manifest)?)?;
            if document.get("schema").and_then(toml::Value::as_str) == Some("ato.capsule/2") {
                build_authored_bundle_v2(&path)?.0
            } else {
                build_static_bundle(
                    &path,
                    request.title.as_deref().unwrap_or("Static application"),
                )?
                .0
            }
        } else if !request.process_routes.is_empty() {
            formation::build_process_bundle(
                &path,
                request.title.as_deref().unwrap_or("Python application"),
                &request.http_checks,
                request.process_routes,
            )?
        } else {
            ensure!(
                path.join("index.html").is_file(),
                "Propose a pinned Python process route for a dynamic source, or provide an authoring manifest or a portable bundle"
            );
            ensure!(
                !request.http_checks.is_empty(),
                "Propose at least one required HTTP check for a static source such as GET / returning 200"
            );
            fs::write(&manifest, static_manifest(&request.http_checks)?)?;
            build_static_bundle(
                &path,
                request.title.as_deref().unwrap_or("Static application"),
            )?
            .0
        };
        (bytes, "frozen_source".to_owned(), source_ref)
    } else {
        ensure!(
            request.process_routes.is_empty(),
            "Use the execution routes declared by the existing Capsule"
        );
        ensure!(
            input.is_file() && input.extension().is_some_and(|ext| ext == "capsule"),
            "Choose a source directory or a portable .capsule file"
        );
        let bytes = read_limited(fs::File::open(&input)?, MAX_ARTIFACT)?;
        let digest = bundle_sha256(&bytes);
        (bytes, "existing_capsule".to_owned(), digest)
    };
    ensure!(
        bytes.len() as u64 <= MAX_ARTIFACT,
        "This publication workflow supports bundles up to 8 MiB"
    );
    let formed = hub::validate_formed_bundle_document(&bytes)?;
    let contract = &formed.derivations[0].contract;
    let mut blockers: Vec<_> = request
        .unsupported_required
        .into_iter()
        .map(|value| format!("Required condition has no configured verifier: {value}"))
        .collect();
    for check in &request.http_checks {
        if !contract.requirements.iter().any(|bound| {
            bound.id == check.id
                && bound.verifier == HTTP_CONTRACT_VERIFIER
                && bound.method.as_deref() == Some("GET")
                && bound.path.as_deref() == Some(check.path.as_str())
                && bound.status == Some(check.status)
                && bound.body_digest == check.body_sha256
        }) {
            blockers.push(format!(
                "HTTP check {} is not declared by the fixed Contract",
                check.id
            ));
        }
    }

    let paths: BTreeSet<_> = formed
        .derivations
        .iter()
        .flat_map(|route| route.tree.entries.iter().map(|item| &item.path))
        .collect();
    let sensitive_path_count = paths.iter().filter(|path| sensitive_path(path)).count();
    let instance_snapshot = formed.bundle.index.instance_snapshot_ref.is_some();
    if instance_snapshot {
        blockers.push("This artifact contains an Instance snapshot. Prepare an application-only artifact and verify it again".into());
    }
    if sensitive_path_count > 0 {
        blockers.push("The artifact contains paths commonly used for credentials or private databases. Review and remove them from the source, then prepare again".into());
    }
    let mut bindings = BTreeSet::new();
    let mut dependency_profiles = Vec::new();
    for route in &formed.derivations {
        bindings.extend(
            route
                .application
                .bindings
                .iter()
                .filter(|binding| binding.required)
                .map(|binding| binding.id.clone()),
        );
        if !route.model_sets.is_empty()
            && !dependency_profiles.iter().any(|value| value == "model_set")
        {
            dependency_profiles.push("model_set".to_owned());
        }
    }
    if let Some(portability) = &formed.bundle.portability {
        dependency_profiles.push(
            serde_json::to_value(portability.profile)?
                .as_str()
                .unwrap_or("unknown")
                .to_owned(),
        );
        if !portability.external_objects.is_empty() {
            blockers.push("The artifact needs external objects. Prepare a bundle with those objects embedded and verify it again".into());
        }
    }
    let base_contract_ref = formed.contract_ref;
    let declared_routes = formed
        .derivations
        .iter()
        .map(|route| DeclaredRoute {
            derivation_ref: route.derivation_ref.to_string(),
            realization: route.realization.label().to_owned(),
            platforms: route.derivation.platforms.clone(),
            runtimes: route.derivation.runtimes.clone(),
        })
        .collect();
    let checks = contract
        .requirements
        .iter()
        .map(|item| {
            json!({
                "id": item.id, "verifier": item.verifier, "method": item.method,
                "path": item.path, "status": item.status, "body_sha256": item.body_digest
            })
        })
        .collect();
    let plan = Plan {
        source_kind,
        source_ref,
        artifact_sha256: bundle_sha256(&bytes),
        artifact_size: bytes.len() as u64,
        effective_contract_ref: effective_contract_ref(
            &base_contract_ref,
            browser_contract.as_ref(),
        ),
        base_contract_ref,
        browser_contract,
        browser_interactive,
        browser_models,
        target_host,
        derivations: formed.bundle.index.derivations,
        declared_routes,
        checks,
        blockers,
        bindings: bindings.into_iter().collect(),
        artifact_review: ArtifactReview {
            instance_snapshot,
            sensitive_path_count,
            secret_or_publisher_data: "unconfirmed".into(),
            dependency_profiles,
        },
        destination: request.destination,
        max_attempts: request.max_attempts,
        budget_ms: request.budget_ms,
    };
    let entry = store.create(plan, &bytes)?;
    view(&entry)
}

fn static_manifest(checks: &[HttpCheck]) -> Result<String> {
    let mut manifest = include_str!("publication/static-base.toml").to_owned();
    for check in checks {
        use std::fmt::Write as _;
        // The accepted IDs, paths and digest are UTF-8, with control bytes
        // forbidden. TOML handles their string quoting.
        let id = toml::Value::String(check.id.clone());
        let path = toml::Value::String(check.path.clone());
        writeln!(
            manifest,
            "\n[[contract.require]]\nid = {id}\nuse = \"ato.contract.http@1\"\nport = \"app.http\"\nmethod = \"GET\"\npath = {path}\n[contract.require.expect]\nstatus = {}",
            check.status
        )?;
        if let Some(digest) = &check.body_sha256 {
            writeln!(manifest, "body_digest = \"{digest}\"")?;
        }
    }
    Ok(manifest)
}

fn sensitive_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    path.split('/').any(|part| {
        part == ".env"
            || (part.starts_with(".env.") && part != ".env.example")
            || matches!(
                part,
                ".aws"
                    | ".ssh"
                    | ".git"
                    | ".netrc"
                    | ".npmrc"
                    | "credentials.json"
                    | "secrets.json"
            )
            || [".pem", ".key", ".p12", ".pfx", ".sqlite", ".sqlite3", ".db"]
                .iter()
                .any(|ext| part.ends_with(ext))
    })
}

fn read_limited(file: fs::File, maximum: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take(maximum + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= maximum,
        "Input exceeds the supported size"
    );
    Ok(bytes)
}

fn start(store: &Store, id: &str) -> Result<Value> {
    let _lock = store.edit(id)?;
    let mut entry = store.read(id)?;
    if entry.phase != Phase::Prepared {
        return view(&entry);
    }
    ensure!(
        entry.plan.blockers.is_empty(),
        "Required checks or artifact review are blocked. See publication_status"
    );
    store.artifact(&entry)?;
    entry.phase = Phase::Queued;
    entry.changed_at_ms = unix_millis();
    store.save(&entry)?;
    let mut worker = Command::new(std::env::current_exe()?);
    worker
        .args(["__publication-worker", id])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    ato_local_execution::configure_detached_process(&mut worker);
    match worker.spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(error) => {
            entry.phase = Phase::Inconclusive;
            entry.issue = Some("verification_worker_unavailable".into());
            store.save(&entry)?;
            return Err(error).context("Could not start the verification worker");
        }
    }
    view(&entry)
}

fn status(store: &Store, id: &str) -> Result<Value> {
    let mut entry = store.read(id)?;
    match entry.phase {
        Phase::Queued | Phase::Verifying if store.job_idle(id)? => {
            if entry.phase == Phase::Verifying
                || unix_millis().saturating_sub(entry.changed_at_ms) > 15_000
            {
                entry.phase = Phase::Inconclusive;
                entry.issue = Some("verification_outcome_unknown".into());
            }
        }
        Phase::Publishing if store.edit_idle(id)? => {
            entry.phase = Phase::PublicationUnknown;
            entry.issue = Some("upload_outcome_unknown".into());
        }
        _ => {}
    }
    view(&entry)
}

fn wait(store: &Store, id: &str, duration: u64) -> Result<Value> {
    ensure!(duration <= 5000, "wait_ms must be 0–5000");
    let since = Instant::now();
    loop {
        let current = status(store, id)?;
        if !matches!(
            current["phase"].as_str(),
            Some("queued" | "verifying" | "publishing")
        ) || since.elapsed().as_millis() >= u128::from(duration)
        {
            return Ok(current);
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

fn cancel(store: &Store, id: &str) -> Result<Value> {
    let _lock = store.edit(id)?;
    let mut entry = store.read(id)?;
    match entry.phase {
        Phase::Prepared | Phase::Queued => {
            entry.phase = Phase::Cancelled;
            store.save(&entry)?;
        }
        Phase::Verifying => store.cancel(id)?,
        _ => bail!("This preparation is outside the verification cancellation window"),
    }
    view(&entry)
}

fn report_ref(entry: &Entry) -> Result<String> {
    Ok(bundle_sha256(&serde_jcs::to_vec(&json!({
        "schema":"ato.publication-report/1", "preparation_id":entry.id, "plan":entry.plan, "attempts":entry.attempts
    }))?))
}

fn verified_derivation(entry: &Entry) -> Option<&str> {
    entry
        .attempts
        .iter()
        .find(|attempt| attempt.valid_pass(&entry.plan))
        .map(Attempt::derivation_ref)
}

fn view(entry: &Entry) -> Result<Value> {
    let report = if entry.phase == Phase::Ready || entry.approval.is_some() {
        Some(report_ref(entry)?)
    } else {
        None
    };
    let approval_command = if entry.phase == Phase::Ready && entry.approval.is_none() {
        report.as_ref().map(|reference| {
            [
                "ato",
                "publish",
                "approve",
                &entry.id,
                "--report-ref",
                reference,
            ]
        })
    } else {
        None
    };
    Ok(json!({
        "preparation_id": entry.id, "phase": entry.phase, "issue": entry.issue,
        "plan": entry.plan, "attempts": entry.attempts,
        "recipient_scope": entry.plan.destination.recipient_scope(),
        "report_ref": report, "approval_command": approval_command,
        "verified_derivation": verified_derivation(entry),
        "approval": entry.approval, "publication": entry.publication,
        "unverified": "Results are scoped to the listed attempt environments. Browser page text, action inputs, raw evidence and the link key are excluded from verification proof. Credential and private-data absence is unconfirmed by automated inspection."
    }))
}

fn approved_report(store: &Store, entry: &Entry, reference: &str) -> Result<()> {
    ensure!(
        entry.phase == Phase::Ready,
        "This preparation is not ready to publish"
    );
    ensure!(
        report_ref(entry)? == reference,
        "The approval request names a different report"
    );
    validate_proof(store, entry)
}

fn validate_proof(store: &Store, entry: &Entry) -> Result<()> {
    ensure!(
        entry.plan.blockers.is_empty(),
        "This report has unresolved required checks"
    );
    store.artifact(entry)?;
    ensure!(
        verified_derivation(entry).is_some()
            && entry
                .attempts
                .iter()
                .all(|attempt| attempt.cleanup_confirmed),
        "No compatible completed verification and cleanup"
    );
    Ok(())
}

fn approve(store: &Store, args: ReportId) -> Result<Value> {
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "Approval requires a user-operated terminal"
    );
    let entry = store.read(&args.preparation_id)?;
    approved_report(store, &entry, &args.report_ref)?;
    println!("{}", serde_json::to_string_pretty(&view(&entry)?)?);
    println!(
        "Confirm your redistribution rights and review the artifact for publisher data and credentials."
    );
    println!(
        "Frozen bundle {}",
        store
            .directory(&entry.id)?
            .join("artifact.capsule")
            .display()
    );
    let phrase = format!("publish {}", &args.report_ref[7..19]);
    print!("To approve the displayed report and destination, type {phrase}: ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    ensure!(answer.trim() == phrase, "Approval was not granted");
    let _lock = store.edit(&args.preparation_id)?;
    let mut entry = store.read(&args.preparation_id)?;
    approved_report(store, &entry, &args.report_ref)?;
    entry.approval = Some(Approval {
        report_ref: args.report_ref,
        approved_at_ms: unix_millis(),
    });
    store.save(&entry)?;
    view(&entry)
}

fn commit(store: &Store, args: ReportId) -> Result<Value> {
    let _lock = store.edit(&args.preparation_id)?;
    let mut entry = store.read(&args.preparation_id)?;
    if entry.phase != Phase::Published {
        approved_report(store, &entry, &args.report_ref)?;
    }
    ensure!(
        entry
            .approval
            .as_ref()
            .is_some_and(|approval| approval.report_ref == args.report_ref),
        "Approve this report with ato publish approve from a user-operated terminal"
    );
    if entry.phase == Phase::Published {
        return view(&entry);
    }
    if let Destination::Public { api_url, .. } = &entry.plan.destination {
        crate::auth_device::resolve_token(None, api_url).context(
            "Authenticate with ato login --api-url using the API URL in the approved report",
        )?;
    }
    let bytes = store.artifact(&entry)?;
    entry.phase = Phase::Publishing;
    store.save(&entry)?;
    let derivation = verified_derivation(&entry).context("The approved route is unavailable")?;
    match upload(store, &entry.plan.destination, &bytes, derivation) {
        Ok(publication) => {
            entry.publication = Some(publication);
            entry.phase = Phase::Published;
            store.save(&entry)?;
            view(&entry)
        }
        Err(_) => {
            entry.phase = Phase::PublicationUnknown;
            entry.issue = Some("upload_not_confirmed_use_reconcile".into());
            store.save(&entry)?;
            view(&entry)
        }
    }
}

fn public_recipient(reference: &str, api_url: &str, derivation: &str) -> Value {
    json!({
        "command":"ato run", "reference":reference, "api_url":api_url, "derivation":derivation,
        "argv":["ato","run",reference,"--api-url",api_url,"--derivation",derivation]
    })
}

fn upload(
    store: &Store,
    destination: &Destination,
    bytes: &[u8],
    derivation: &str,
) -> Result<Value> {
    match destination {
        Destination::Public {
            api_url,
            name,
            tag,
            license,
        } => {
            let result = hub_public::publish_pinned_bytes(bytes, api_url, name, tag, license)?;
            Ok(
                json!({"kind":"public", "reference":result.0, "url":result.1,
                "recipient":public_recipient(&result.0, api_url, derivation)}),
            )
        }
        Destination::Link {
            hub_url,
            allow_loopback_http,
        } => {
            let url = hub::push_pinned_bytes(bytes, hub_url, *allow_loopback_http, store.root())?;
            Ok(json!({"kind":"link", "private_link":url,
            "recipient":{"derivation":derivation,"commands":[
                if *allow_loopback_http { "ato hub pull <link> --allow-loopback-http -o app.capsule" }
                else { "ato hub pull <link> -o app.capsule" },
                &format!("ato run ./app.capsule --derivation {derivation}")
            ]}}))
        }
    }
}

fn reconcile(store: &Store, id: &str) -> Result<Value> {
    let _lock = store.edit(id)?;
    let mut entry = store.read(id)?;
    ensure!(
        matches!(entry.phase, Phase::Publishing | Phase::PublicationUnknown),
        "This preparation has no unconfirmed upload"
    );
    let Destination::Public {
        api_url, name, tag, ..
    } = &entry.plan.destination
    else {
        bail!("This encrypted upload cannot be reconciled without its returned link");
    };
    let reference = format!("capsules/{name}:{tag}");
    let found = hub_public::resolve_for_run(
        Path::new(&reference),
        api_url,
        &store.root().join("reconciliation"),
    )?
    .context("Could not resolve the current Hub tag")?;
    ensure!(
        found.digest == entry.plan.artifact_sha256,
        "The Hub tag does not name the approved bundle. The upload remains unconfirmed"
    );
    validate_proof(store, &entry)?;
    let reference = format!("capsules/{name}@{}", found.digest);
    let derivation = verified_derivation(&entry).context("The approved route is unavailable")?;
    entry.publication = Some(
        json!({"kind":"public","reference":reference,"tag_matches":true,
        "recipient":public_recipient(&reference, api_url, derivation)}),
    );
    entry.phase = Phase::Published;
    entry.issue = None;
    store.save(&entry)?;
    view(&entry)
}
