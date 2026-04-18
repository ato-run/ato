use std::fs::{self, OpenOptions};
use std::io::{IsTerminal, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use adk_core::Llm;
use anyhow::{Context, Result};
use capsule_core::execution_plan::error::AtoExecutionError;
use capsule_core::isolation::HostIsolationContext;
use capsule_core::router::ManifestData;
use capsule_core::CapsuleReporter;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use walkdir::WalkDir;

use crate::application::ports::OutputPort;
use crate::executors::launch_context::RuntimeLaunchContext;
use crate::reporters::CliReporter;

const AGENT_MODEL_DEFAULT: &str = "gpt-5-mini";
const AGENT_ARTIFACT_SUBDIR: &str = ".ato/tmp/agent/runs";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SetupFailureKind {
    MissingLockfile,
    DependencyInstall,
    BuildLifecycle,
    RuntimeBootstrap,
    WorkingDirectoryMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ClassifiedFailure {
    pub(crate) stage: String,
    pub(crate) kind: SetupFailureKind,
    pub(crate) message: String,
}

pub(crate) struct AgentFailureClassifier;

impl AgentFailureClassifier {
    pub(crate) fn classify(error: &anyhow::Error, stage: &str) -> Option<ClassifiedFailure> {
        let structured = error.downcast_ref::<AtoExecutionError>();
        if let Some(structured) = structured {
            match structured.code {
                "ATO_ERR_MISSING_REQUIRED_ENV"
                | "ATO_ERR_POLICY_VIOLATION"
                | "ATO_ERR_SECURITY_POLICY_VIOLATION"
                | "ATO_ERR_ARTIFACT_INTEGRITY_FAILURE"
                | "ATO_ERR_LOCKFILE_TAMPERED"
                | "ATO_ERR_PROVISIONING_TLS_TRUST"
                | "ATO_ERR_PROVISIONING_TLS_BOOTSTRAP_REQUIRED" => return None,
                "ATO_ERR_RUNTIME_NOT_RESOLVED" => {
                    return Some(ClassifiedFailure {
                        stage: stage.to_string(),
                        kind: SetupFailureKind::RuntimeBootstrap,
                        message: structured.message.clone(),
                    });
                }
                "ATO_ERR_PROVISIONING_LOCK_INCOMPLETE" => {
                    return Some(ClassifiedFailure {
                        stage: stage.to_string(),
                        kind: classify_message(&structured.message),
                        message: structured.message.clone(),
                    });
                }
                _ => {}
            }
        }

        let rendered = error.to_string();
        let kind = classify_message(&rendered);
        if matches!(
            kind,
            SetupFailureKind::MissingLockfile
                | SetupFailureKind::DependencyInstall
                | SetupFailureKind::BuildLifecycle
                | SetupFailureKind::RuntimeBootstrap
                | SetupFailureKind::WorkingDirectoryMismatch
        ) {
            return Some(ClassifiedFailure {
                stage: stage.to_string(),
                kind,
                message: rendered,
            });
        }

        None
    }
}

fn classify_message(message: &str) -> SetupFailureKind {
    let lowered = message.to_ascii_lowercase();
    if lowered.contains("working directory")
        || lowered.contains("no such file or directory")
        || lowered.contains("cannot find module")
    {
        return SetupFailureKind::WorkingDirectoryMismatch;
    }
    if lowered.contains("runtime is not resolved")
        || lowered.contains("runtime_not_resolved")
        || lowered.contains("requires hermetic")
        || lowered.contains("toolchain")
    {
        return SetupFailureKind::RuntimeBootstrap;
    }
    if lowered.contains("build command failed")
        || lowered.contains(" build [")
        || lowered.contains("build failed with exit code")
        || lowered.contains(" command failed with exit code ")
    {
        return SetupFailureKind::BuildLifecycle;
    }
    if lowered.contains("failed to execute deno cache")
        || lowered.contains("failed to execute deno run")
        || lowered.contains("failed to run npm")
        || lowered.contains("failed to run uv")
    {
        return SetupFailureKind::DependencyInstall;
    }
    SetupFailureKind::MissingLockfile
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CandidateChange {
    pub(crate) path: String,
    pub(crate) change_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum PlannedAction {
    RunCommand {
        reason: String,
        command: String,
        working_dir: String,
    },
    WriteFile {
        reason: String,
        path: String,
        content: String,
    },
    Finish {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AgentAsk {
    pub(crate) reason: String,
    pub(crate) prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AgentPlanSummary {
    pub(crate) model: String,
    pub(crate) hypothesis: String,
    pub(crate) actions: Vec<PlannedAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ask: Option<AgentAsk>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AgentRunSummary {
    pub(crate) trigger: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) failure: Option<ClassifiedFailure>,
    pub(crate) model: String,
    pub(crate) hypothesis: String,
    pub(crate) action_count: usize,
    pub(crate) changed_files: Vec<CandidateChange>,
    pub(crate) rerouted_manifest: String,
}

#[derive(Debug, Clone)]
pub(crate) struct AgentRunRequest {
    pub(crate) project_root: PathBuf,
    pub(crate) source_root: PathBuf,
    pub(crate) manifest_path: PathBuf,
    pub(crate) plan: ManifestData,
    pub(crate) launch_ctx: RuntimeLaunchContext,
    pub(crate) trigger: String,
    pub(crate) failure: Option<ClassifiedFailure>,
    pub(crate) force_reroute: bool,
    pub(crate) reporter: std::sync::Arc<CliReporter>,
    pub(crate) assume_yes: bool,
    pub(crate) use_progressive_ui: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AgentRunOutcome {
    pub(crate) artifact_dir: PathBuf,
    pub(crate) shadow_manifest_path: PathBuf,
    pub(crate) modified: bool,
    pub(crate) summary: AgentRunSummary,
}

#[derive(Debug, Clone, Serialize)]
struct SessionMetadata {
    run_id: String,
    artifact_dir: String,
    source_root: String,
    workspace_dir: String,
    manifest_path: String,
    shadow_manifest_path: String,
}

pub(crate) struct AgentSessionStore {
    run_id: String,
    artifact_dir: PathBuf,
    source_root: PathBuf,
    workspace_dir: PathBuf,
    manifest_relative_path: PathBuf,
    events_path: PathBuf,
    session_path: PathBuf,
    summary_path: PathBuf,
    diff_path: PathBuf,
}

impl AgentSessionStore {
    pub(crate) fn create(
        project_root: &Path,
        source_root: &Path,
        manifest_path: &Path,
    ) -> Result<Self> {
        let project_root = absolute_path(project_root)?;
        let source_root = absolute_path(source_root)?;
        let manifest_path = absolute_path(manifest_path)?;
        let manifest_relative_path = manifest_path
            .strip_prefix(&source_root)
            .with_context(|| {
                format!(
                    "manifest {} is outside source root {}",
                    manifest_path.display(),
                    source_root.display()
                )
            })?
            .to_path_buf();
        let run_id = format!(
            "run-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        );
        let artifact_dir = project_root.join(AGENT_ARTIFACT_SUBDIR).join(&run_id);
        let workspace_dir = artifact_dir.join("workspace");
        fs::create_dir_all(&workspace_dir).with_context(|| {
            format!(
                "failed to create agent workspace {}",
                workspace_dir.display()
            )
        })?;
        copy_workspace_snapshot(&source_root, &workspace_dir, &artifact_dir)?;

        let store = Self {
            run_id,
            artifact_dir: artifact_dir.clone(),
            source_root,
            workspace_dir,
            manifest_relative_path,
            events_path: artifact_dir.join("events.jsonl"),
            session_path: artifact_dir.join("session.json"),
            summary_path: artifact_dir.join("summary.json"),
            diff_path: artifact_dir.join("diff.patch"),
        };
        store.write_session_file()?;
        Ok(store)
    }

    pub(crate) fn artifact_dir(&self) -> &Path {
        &self.artifact_dir
    }

    pub(crate) fn workspace_dir(&self) -> &Path {
        &self.workspace_dir
    }

    pub(crate) fn shadow_manifest_path(&self) -> PathBuf {
        self.workspace_dir.join(&self.manifest_relative_path)
    }

    pub(crate) fn append_event(&self, event_type: &str, payload: Value) -> Result<()> {
        let record = json!({
            "event": event_type,
            "run_id": self.run_id,
            "payload": payload,
        });
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.events_path)
            .with_context(|| format!("failed to open {}", self.events_path.display()))?;
        writeln!(file, "{}", serde_json::to_string(&record)?)
            .with_context(|| format!("failed to append {}", self.events_path.display()))
    }

    pub(crate) fn list_changes(&self) -> Result<Vec<CandidateChange>> {
        let mut changes = Vec::new();
        for entry in WalkDir::new(&self.workspace_dir)
            .into_iter()
            .filter_map(Result::ok)
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let shadow_path = entry.path();
            let relative = match shadow_path.strip_prefix(&self.workspace_dir) {
                Ok(value) => value,
                Err(_) => continue,
            };
            let source_path = self.source_root.join(relative);
            let change_type = if !source_path.exists() {
                "added"
            } else if fs::read(shadow_path)? == fs::read(&source_path)? {
                continue;
            } else {
                "modified"
            };
            changes.push(CandidateChange {
                path: relative.to_string_lossy().to_string(),
                change_type: change_type.to_string(),
            });
        }

        for entry in WalkDir::new(&self.source_root)
            .into_iter()
            .filter_map(Result::ok)
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let source_path = entry.path();
            let relative = match source_path.strip_prefix(&self.source_root) {
                Ok(value) => value,
                Err(_) => continue,
            };
            if should_skip_snapshot(relative) {
                continue;
            }
            let shadow_path = self.workspace_dir.join(relative);
            if !shadow_path.exists() {
                changes.push(CandidateChange {
                    path: relative.to_string_lossy().to_string(),
                    change_type: "removed".to_string(),
                });
            }
        }

        changes.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(changes)
    }

    pub(crate) fn write_summary(&self, summary: &AgentRunSummary) -> Result<()> {
        fs::write(&self.summary_path, serde_json::to_vec_pretty(summary)?)
            .with_context(|| format!("failed to write {}", self.summary_path.display()))
    }

    pub(crate) fn write_diff_patch(&self, changes: &[CandidateChange]) -> Result<()> {
        let patch = if which::which("git").is_ok() {
            let output = Command::new("git")
                .args([
                    "diff",
                    "--no-index",
                    "--binary",
                    "--",
                    self.source_root.to_string_lossy().as_ref(),
                    self.workspace_dir.to_string_lossy().as_ref(),
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .output()
                .context("failed to generate diff.patch with git diff --no-index")?;
            String::from_utf8_lossy(&output.stdout).to_string()
        } else {
            let mut fallback = String::new();
            for change in changes {
                fallback.push_str(&format!("{} {}\n", change.change_type, change.path));
            }
            fallback
        };
        fs::write(&self.diff_path, patch)
            .with_context(|| format!("failed to write {}", self.diff_path.display()))
    }

    fn write_session_file(&self) -> Result<()> {
        let metadata = SessionMetadata {
            run_id: self.run_id.clone(),
            artifact_dir: self.artifact_dir.display().to_string(),
            source_root: self.source_root.display().to_string(),
            workspace_dir: self.workspace_dir.display().to_string(),
            manifest_path: self
                .source_root
                .join(&self.manifest_relative_path)
                .display()
                .to_string(),
            shadow_manifest_path: self.shadow_manifest_path().display().to_string(),
        };
        fs::write(&self.session_path, serde_json::to_vec_pretty(&metadata)?)
            .with_context(|| format!("failed to write {}", self.session_path.display()))
    }
}

pub(crate) struct AgentToolExecutor {
    store: AgentSessionStore,
    plan: ManifestData,
    launch_ctx: RuntimeLaunchContext,
    isolation: HostIsolationContext,
}

impl AgentToolExecutor {
    fn new(
        store: AgentSessionStore,
        plan: ManifestData,
        launch_ctx: RuntimeLaunchContext,
    ) -> Result<Self> {
        let isolation = HostIsolationContext::new(store.artifact_dir(), "agent")
            .with_context(|| "failed to initialize agent host isolation")?;
        Ok(Self {
            store,
            plan,
            launch_ctx,
            isolation,
        })
    }

    fn inspect_workspace(&self) -> WorkspaceInspection {
        let working_dir_relative = self.plan.execution_working_dir().unwrap_or_default();
        let working_dir = if working_dir_relative.trim().is_empty() {
            self.store.workspace_dir().to_path_buf()
        } else {
            self.store.workspace_dir().join(&working_dir_relative)
        };
        let node_lockfiles = [
            "package-lock.json",
            "yarn.lock",
            "pnpm-lock.yaml",
            "bun.lock",
            "bun.lockb",
        ]
        .iter()
        .filter(|name| working_dir.join(name).exists())
        .map(|name| (*name).to_string())
        .collect::<Vec<_>>();

        WorkspaceInspection {
            runtime: self.plan.execution_runtime(),
            driver: self.plan.execution_driver(),
            working_dir: working_dir_relative,
            working_dir_exists: working_dir.exists(),
            has_package_json: working_dir.join("package.json").exists(),
            has_pyproject_toml: working_dir.join("pyproject.toml").exists(),
            has_requirements_txt: working_dir.join("requirements.txt").exists(),
            has_cargo_toml: working_dir.join("Cargo.toml").exists(),
            has_source_dir: self.store.workspace_dir().join("source").is_dir(),
            node_lockfiles,
            deno_lock_exists: working_dir.join("deno.lock").exists(),
            uv_lock_exists: working_dir.join("uv.lock").exists(),
            cargo_lock_exists: working_dir.join("Cargo.lock").exists(),
            entrypoint: self.plan.execution_entrypoint(),
            build_command: self.plan.build_lifecycle_build(),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn read_file(&self, relative: &Path) -> Result<String> {
        let path = self.resolve_workspace_path(relative)?;
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))
    }

    pub(crate) fn write_shadow_file(&self, relative: &Path, content: &str) -> Result<()> {
        let path = self.resolve_workspace_path(relative)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::write(&path, content).with_context(|| format!("failed to write {}", path.display()))
    }

    pub(crate) fn run_shadow_command(&self, command: &str, working_dir: &str) -> Result<String> {
        let parsed = parse_safe_command(command)?;
        let current_dir = self.resolve_workspace_path(Path::new(working_dir))?;
        if !current_dir.exists() {
            anyhow::bail!(
                "agent working directory does not exist: {}",
                current_dir.display()
            );
        }

        let mut cmd = Command::new(&parsed[0]);
        cmd.args(parsed.iter().skip(1))
            .current_dir(&current_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());

        let base_env = crate::runtime::overrides::merged_env(self.plan.execution_env());
        self.isolation.apply_to_command(&mut cmd, base_env);
        self.launch_ctx.apply_allowlisted_env(&mut cmd)?;

        let status = cmd.status().with_context(|| {
            format!(
                "failed to run agent command '{}' in {}",
                command,
                current_dir.display()
            )
        })?;
        if !status.success() {
            anyhow::bail!(
                "agent command failed with exit code {}: {}",
                status.code().unwrap_or(1),
                command
            );
        }

        Ok(format!(
            "completed {} in {}",
            command,
            current_dir.display()
        ))
    }

    pub(crate) fn list_shadow_changes(&self) -> Result<Vec<CandidateChange>> {
        self.store.list_changes()
    }

    pub(crate) fn finish_attempt(&self, summary: &AgentRunSummary) -> Result<()> {
        let changes = self.store.list_changes()?;
        self.store.write_summary(summary)?;
        self.store.write_diff_patch(&changes)?;
        Ok(())
    }

    pub(crate) fn shadow_manifest_path(&self) -> PathBuf {
        self.store.shadow_manifest_path()
    }

    pub(crate) fn artifact_dir(&self) -> &Path {
        self.store.artifact_dir()
    }

    fn resolve_workspace_path(&self, relative: &Path) -> Result<PathBuf> {
        let sanitized = sanitize_relative_path(relative)?;
        Ok(self.store.workspace_dir().join(sanitized))
    }
}

#[derive(Debug, Clone)]
struct WorkspaceInspection {
    runtime: Option<String>,
    driver: Option<String>,
    working_dir: String,
    working_dir_exists: bool,
    has_package_json: bool,
    has_pyproject_toml: bool,
    has_requirements_txt: bool,
    has_cargo_toml: bool,
    has_source_dir: bool,
    node_lockfiles: Vec<String>,
    deno_lock_exists: bool,
    uv_lock_exists: bool,
    cargo_lock_exists: bool,
    entrypoint: Option<String>,
    build_command: Option<String>,
}

pub(crate) struct AgentRunSession {
    request: AgentRunRequest,
    executor: AgentToolExecutor,
}

impl AgentRunSession {
    pub(crate) fn start(request: AgentRunRequest) -> Result<Self> {
        let store = AgentSessionStore::create(
            &request.project_root,
            &request.source_root,
            &request.manifest_path,
        )?;
        let executor =
            AgentToolExecutor::new(store, request.plan.clone(), request.launch_ctx.clone())?;
        Ok(Self { request, executor })
    }

    pub(crate) async fn run(self) -> Result<AgentRunOutcome> {
        let inspection = self.executor.inspect_workspace();
        self.executor.store.append_event(
            "agent_session_started",
            json!({
                "trigger": self.request.trigger,
                "failure": self.request.failure,
                "workspace_dir": self.executor.store.workspace_dir(),
            }),
        )?;
        emit_agent_json_event(
            &self.request.reporter,
            "agent_session_started",
            json!({
                "trigger": self.request.trigger,
                "failure": self.request.failure,
                "workspace_dir": self.executor.store.workspace_dir(),
            }),
        )?;
        emit_agent_surface(
            &self.request.reporter,
            self.request.use_progressive_ui,
            "Agent Trigger",
            &self.request.trigger,
        )
        .await?;

        let skill_context = load_skill_context(&self.request.project_root)?;
        let mut plan = try_plan_with_adk(&self.request, &inspection, skill_context.as_deref())
            .await
            .unwrap_or_else(|_| heuristic_plan(&self.request, &inspection));
        if plan.model.trim().is_empty() {
            plan.model = "heuristic".to_string();
        }

        self.executor
            .store
            .append_event("agent_plan", serde_json::to_value(&plan)?)?;
        emit_agent_json_event(
            &self.request.reporter,
            "agent_plan",
            serde_json::to_value(&plan)?,
        )?;
        emit_agent_surface(
            &self.request.reporter,
            self.request.use_progressive_ui,
            "Agent Hypothesis",
            &format!("{} [{}]", plan.hypothesis, plan.model),
        )
        .await?;

        if let Some(ask) = plan.ask.as_ref() {
            let approved = confirm_agent_ask(
                &self.request.reporter,
                self.request.assume_yes,
                self.request.use_progressive_ui,
                &ask.prompt,
            )?;
            self.executor.store.append_event(
                "agent_ask",
                json!({
                    "reason": ask.reason,
                    "prompt": ask.prompt,
                    "approved": approved,
                }),
            )?;
            emit_agent_json_event(
                &self.request.reporter,
                "agent_ask",
                json!({
                    "reason": ask.reason,
                    "prompt": ask.prompt,
                    "approved": approved,
                }),
            )?;
            if !approved {
                return Err(AtoExecutionError::manual_intervention_required(
                    "agent setup requires explicit approval before it can modify the shadow workspace",
                    Some(self.executor.shadow_manifest_path().to_string_lossy().as_ref()),
                    vec!["re-run interactively or pass --yes to approve the agent ask".to_string()],
                )
                .into());
            }
        }

        for action in &plan.actions {
            let label = match action {
                PlannedAction::RunCommand { command, .. } => command.as_str(),
                PlannedAction::WriteFile { path, .. } => path.as_str(),
                PlannedAction::Finish { reason } => reason.as_str(),
            };
            self.executor.store.append_event(
                "agent_tool_started",
                json!({
                    "action": action,
                }),
            )?;
            emit_agent_json_event(
                &self.request.reporter,
                "agent_tool_started",
                json!({
                    "action": action,
                }),
            )?;
            emit_agent_surface(
                &self.request.reporter,
                self.request.use_progressive_ui,
                "Agent Action",
                label,
            )
            .await?;

            let observation = match action {
                PlannedAction::RunCommand {
                    command,
                    working_dir,
                    ..
                } => self.executor.run_shadow_command(command, working_dir)?,
                PlannedAction::WriteFile { path, content, .. } => {
                    self.executor.write_shadow_file(Path::new(path), content)?;
                    format!("updated {}", path)
                }
                PlannedAction::Finish { reason } => reason.clone(),
            };

            self.executor.store.append_event(
                "agent_tool_finished",
                json!({
                    "action": action,
                    "observation": observation,
                }),
            )?;
            emit_agent_json_event(
                &self.request.reporter,
                "agent_tool_finished",
                json!({
                    "action": action,
                    "observation": observation,
                }),
            )?;
            emit_agent_surface(
                &self.request.reporter,
                self.request.use_progressive_ui,
                "Agent Observation",
                &observation,
            )
            .await?;
        }

        let changes = self.executor.list_shadow_changes()?;
        let summary = AgentRunSummary {
            trigger: self.request.trigger.clone(),
            failure: self.request.failure.clone(),
            model: plan.model.clone(),
            hypothesis: plan.hypothesis.clone(),
            action_count: plan.actions.len(),
            changed_files: changes.clone(),
            rerouted_manifest: self.executor.shadow_manifest_path().display().to_string(),
        };
        self.executor.finish_attempt(&summary)?;
        self.executor
            .store
            .append_event("agent_candidate_changes", serde_json::to_value(&changes)?)?;
        emit_agent_json_event(
            &self.request.reporter,
            "agent_candidate_changes",
            serde_json::to_value(&changes)?,
        )?;
        self.executor
            .store
            .append_event("agent_session_finished", serde_json::to_value(&summary)?)?;
        emit_agent_json_event(
            &self.request.reporter,
            "agent_session_finished",
            serde_json::to_value(&summary)?,
        )?;

        let modified = !changes.is_empty();
        if self.request.use_progressive_ui {
            let body = if changes.is_empty() {
                "No shadow-only file changes were required.".to_string()
            } else {
                changes
                    .iter()
                    .map(|change| format!("{} {}", change.change_type, change.path))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            crate::progressive_ui::show_note("Agent Diff Summary", body)?;
        }

        Ok(AgentRunOutcome {
            artifact_dir: self.executor.artifact_dir().to_path_buf(),
            shadow_manifest_path: self.executor.shadow_manifest_path(),
            modified: modified || self.request.force_reroute,
            summary,
        })
    }
}

pub(crate) async fn run_agent_setup(request: AgentRunRequest) -> Result<AgentRunOutcome> {
    let session = AgentRunSession::start(request)?;
    session.run().await
}

fn heuristic_plan(request: &AgentRunRequest, inspection: &WorkspaceInspection) -> AgentPlanSummary {
    let mut actions = Vec::new();
    let mut ask = None;
    let model = "heuristic".to_string();
    let failure_kind = request
        .failure
        .as_ref()
        .map(|value| value.kind.clone())
        .unwrap_or(SetupFailureKind::DependencyInstall);

    if !inspection.working_dir_exists && inspection.has_source_dir {
        ask = Some(AgentAsk {
            reason: "manifest rewrite".to_string(),
            prompt: "working_dir does not exist. Approve rewriting the shadow manifest to use `source/`?"
                .to_string(),
        });
        if let Ok(updated) =
            rewrite_shadow_manifest_to_source(&request.plan, request.manifest_path.as_path())
        {
            let manifest_relative_path = request
                .manifest_path
                .strip_prefix(&request.source_root)
                .unwrap_or_else(|_| Path::new("capsule.toml"));
            actions.push(PlannedAction::WriteFile {
                reason: "rewrite missing working_dir to source/ inside the shadow manifest"
                    .to_string(),
                path: manifest_relative_path.to_string_lossy().to_string(),
                content: updated,
            });
        }
    }

    let runtime = inspection.runtime.as_deref().unwrap_or_default();
    let driver = inspection.driver.as_deref().unwrap_or_default();
    if runtime.eq_ignore_ascii_case("source") && driver.eq_ignore_ascii_case("node")
        || inspection.has_package_json
    {
        plan_node_actions(
            &mut actions,
            &mut ask,
            &failure_kind,
            inspection,
            request.force_reroute,
        );
    } else if driver.eq_ignore_ascii_case("deno") || runtime.eq_ignore_ascii_case("deno") {
        plan_deno_actions(
            &mut actions,
            &failure_kind,
            inspection,
            request.force_reroute,
        );
    } else if driver.eq_ignore_ascii_case("python")
        || (driver.eq_ignore_ascii_case("native")
            && inspection
                .entrypoint
                .as_deref()
                .unwrap_or_default()
                .ends_with(".py"))
        || inspection.has_pyproject_toml
        || inspection.has_requirements_txt
    {
        plan_python_actions(
            &mut actions,
            &failure_kind,
            inspection,
            request.force_reroute,
        );
    } else if driver.eq_ignore_ascii_case("native") || inspection.has_cargo_toml {
        plan_cargo_actions(
            &mut actions,
            &failure_kind,
            inspection,
            request.force_reroute,
        );
    }

    maybe_add_build_action(
        &mut actions,
        inspection,
        request.force_reroute,
        &failure_kind,
    );
    if actions.is_empty() {
        actions.push(PlannedAction::Finish {
            reason: "no safe shadow-only remediation action was available".to_string(),
        });
    }

    AgentPlanSummary {
        model,
        hypothesis: hypothesis_for_failure(&failure_kind, inspection),
        actions,
        ask,
    }
}

fn plan_node_actions(
    actions: &mut Vec<PlannedAction>,
    ask: &mut Option<AgentAsk>,
    failure_kind: &SetupFailureKind,
    inspection: &WorkspaceInspection,
    force_reroute: bool,
) {
    if inspection.node_lockfiles.len() > 1 {
        *ask = Some(AgentAsk {
            reason: "conflicting lockfiles".to_string(),
            prompt: "multiple node lockfiles were detected. Approve continuing with the first-precedence lockfile heuristic in the shadow workspace?"
                .to_string(),
        });
        return;
    }

    let working_dir = normalize_working_dir(&inspection.working_dir);
    if inspection.node_lockfiles.is_empty()
        && inspection.has_package_json
        && which::which("npm").is_ok()
    {
        actions.push(PlannedAction::RunCommand {
            reason: "generate package-lock.json in the shadow workspace".to_string(),
            command: "npm install --package-lock-only --ignore-scripts".to_string(),
            working_dir: working_dir.clone(),
        });
        if !force_reroute && matches!(failure_kind, SetupFailureKind::MissingLockfile) {
            return;
        }
    }

    let command = inspection
        .node_lockfiles
        .first()
        .and_then(|lock| match lock.as_str() {
            "package-lock.json" if which::which("npm").is_ok() => Some("npm install"),
            "yarn.lock" if which::which("yarn").is_ok() => Some("yarn install"),
            "pnpm-lock.yaml" if which::which("pnpm").is_ok() => Some("pnpm install"),
            "bun.lock" | "bun.lockb" if which::which("bun").is_ok() => Some("bun install"),
            _ => None,
        })
        .map(str::to_string);
    if let Some(command) = command {
        actions.push(PlannedAction::RunCommand {
            reason: "materialize source/node dependencies in the shadow workspace".to_string(),
            command,
            working_dir,
        });
    }
}

fn plan_python_actions(
    actions: &mut Vec<PlannedAction>,
    failure_kind: &SetupFailureKind,
    inspection: &WorkspaceInspection,
    force_reroute: bool,
) {
    if which::which("uv").is_err() {
        return;
    }
    let working_dir = normalize_working_dir(&inspection.working_dir);
    if !inspection.uv_lock_exists {
        let command = if inspection.has_pyproject_toml {
            Some("uv lock".to_string())
        } else if inspection.has_requirements_txt {
            Some("uv pip compile requirements.txt -o uv.lock".to_string())
        } else {
            None
        };
        if let Some(command) = command {
            actions.push(PlannedAction::RunCommand {
                reason: "generate uv.lock inside the shadow workspace".to_string(),
                command,
                working_dir: working_dir.clone(),
            });
            if !force_reroute && matches!(failure_kind, SetupFailureKind::MissingLockfile) {
                return;
            }
        }
    }
    if inspection.uv_lock_exists || force_reroute {
        actions.push(PlannedAction::RunCommand {
            reason: "sync python dependencies inside the shadow workspace".to_string(),
            command: "uv sync --frozen".to_string(),
            working_dir,
        });
    }
}

fn plan_deno_actions(
    actions: &mut Vec<PlannedAction>,
    failure_kind: &SetupFailureKind,
    inspection: &WorkspaceInspection,
    force_reroute: bool,
) {
    if which::which("deno").is_err() {
        return;
    }
    let Some(entrypoint) = inspection.entrypoint.as_deref() else {
        return;
    };
    let working_dir = normalize_working_dir(&inspection.working_dir);
    if !inspection.deno_lock_exists {
        actions.push(PlannedAction::RunCommand {
            reason: "generate deno.lock inside the shadow workspace".to_string(),
            command: format!("deno cache --lock=deno.lock --frozen=false {}", entrypoint),
            working_dir: working_dir.clone(),
        });
        if !force_reroute && matches!(failure_kind, SetupFailureKind::MissingLockfile) {
            return;
        }
    }
    actions.push(PlannedAction::RunCommand {
        reason: "warm the deno cache using the shadow lockfile".to_string(),
        command: format!("deno cache --lock deno.lock --frozen {}", entrypoint),
        working_dir,
    });
}

fn plan_cargo_actions(
    actions: &mut Vec<PlannedAction>,
    failure_kind: &SetupFailureKind,
    inspection: &WorkspaceInspection,
    force_reroute: bool,
) {
    if which::which("cargo").is_err() {
        return;
    }
    let working_dir = normalize_working_dir(&inspection.working_dir);
    if !inspection.cargo_lock_exists {
        actions.push(PlannedAction::RunCommand {
            reason: "generate Cargo.lock inside the shadow workspace".to_string(),
            command: "cargo generate-lockfile".to_string(),
            working_dir: working_dir.clone(),
        });
        if !force_reroute && matches!(failure_kind, SetupFailureKind::MissingLockfile) {
            return;
        }
    }
    actions.push(PlannedAction::RunCommand {
        reason: "fetch cargo dependencies using the shadow lockfile".to_string(),
        command: "cargo fetch --locked".to_string(),
        working_dir,
    });
}

fn maybe_add_build_action(
    actions: &mut Vec<PlannedAction>,
    inspection: &WorkspaceInspection,
    force_reroute: bool,
    failure_kind: &SetupFailureKind,
) {
    if !force_reroute && !matches!(failure_kind, SetupFailureKind::BuildLifecycle) {
        return;
    }
    let Some(command) = inspection
        .build_command
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    if parse_safe_command(command).is_err() {
        return;
    }
    actions.push(PlannedAction::RunCommand {
        reason: "re-run the declared build lifecycle inside the shadow workspace".to_string(),
        command: command.to_string(),
        working_dir: normalize_working_dir(&inspection.working_dir),
    });
}

fn hypothesis_for_failure(
    failure_kind: &SetupFailureKind,
    inspection: &WorkspaceInspection,
) -> String {
    match failure_kind {
        SetupFailureKind::MissingLockfile => {
            "A fail-closed lockfile is missing; generate it in the shadow workspace and retry."
                .to_string()
        }
        SetupFailureKind::DependencyInstall => {
            "Dependencies appear incomplete; install them inside the shadow workspace before rerunning."
                .to_string()
        }
        SetupFailureKind::BuildLifecycle => format!(
            "The build lifecycle failed for runtime={} driver={}; refresh dependencies and re-run the build in shadow.",
            inspection.runtime.as_deref().unwrap_or("unknown"),
            inspection.driver.as_deref().unwrap_or("unknown"),
        ),
        SetupFailureKind::RuntimeBootstrap => {
            "Runtime bootstrap is incomplete; use safe workspace-local provisioning steps first."
                .to_string()
        }
        SetupFailureKind::WorkingDirectoryMismatch => {
            "The configured working_dir does not line up with the workspace layout; adjust the shadow manifest only if approved."
                .to_string()
        }
    }
}

async fn try_plan_with_adk(
    request: &AgentRunRequest,
    inspection: &WorkspaceInspection,
    skill_context: Option<&str>,
) -> Result<AgentPlanSummary> {
    let api_key = match std::env::var("OPENAI_API_KEY") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => anyhow::bail!("OPENAI_API_KEY is not set"),
    };

    let model = std::env::var("ATO_AGENT_MODEL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| AGENT_MODEL_DEFAULT.to_string());

    let config = if model.starts_with('o') {
        adk_model::openai::OpenAIConfig::new(api_key, &model)
            .with_reasoning_effort(adk_model::openai::ReasoningEffort::Medium)
    } else {
        adk_model::openai::OpenAIConfig::new(api_key, &model)
    };
    let client = adk_model::openai::OpenAIClient::new(config)
        .with_context(|| "failed to initialize ADK OpenAI client")?;

    let mut prompt = String::from(
        "You are an agentic setup planner for ato run. Return JSON only.\n\
         Constraints:\n\
         - You may only propose actions using these tools: run_command, write_file, finish.\n\
         - run_command must use one of: cargo, npm, pnpm, bun, uv, deno.\n\
         - working_dir must stay inside the shadow workspace and must be relative.\n\
         - Never use shell operators like &&, ||, ;, >, <, or pipes.\n\
         - Never write outside the shadow workspace.\n\
         - Prefer the smallest safe fix that unblocks ato run.\n",
    );
    if let Some(skill_context) = skill_context {
        prompt.push_str("\nProject setup skill context:\n");
        prompt.push_str(skill_context);
        prompt.push('\n');
    }

    let user_payload = json!({
        "trigger": request.trigger,
        "failure": request.failure,
        "runtime": inspection.runtime,
        "driver": inspection.driver,
        "working_dir": inspection.working_dir,
        "working_dir_exists": inspection.working_dir_exists,
        "has_package_json": inspection.has_package_json,
        "has_pyproject_toml": inspection.has_pyproject_toml,
        "has_requirements_txt": inspection.has_requirements_txt,
        "has_cargo_toml": inspection.has_cargo_toml,
        "has_source_dir": inspection.has_source_dir,
        "node_lockfiles": inspection.node_lockfiles,
        "deno_lock_exists": inspection.deno_lock_exists,
        "uv_lock_exists": inspection.uv_lock_exists,
        "cargo_lock_exists": inspection.cargo_lock_exists,
        "entrypoint": inspection.entrypoint,
        "build_command": inspection.build_command,
    });
    let schema = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["hypothesis", "actions"],
        "properties": {
            "hypothesis": { "type": "string" },
            "ask": {
                "type": "object",
                "additionalProperties": false,
                "required": ["reason", "prompt"],
                "properties": {
                    "reason": { "type": "string" },
                    "prompt": { "type": "string" }
                }
            },
            "actions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["kind", "reason"],
                    "properties": {
                        "kind": {
                            "type": "string",
                            "enum": ["run_command", "write_file", "finish"]
                        },
                        "reason": { "type": "string" },
                        "command": { "type": "string" },
                        "working_dir": { "type": "string" },
                        "path": { "type": "string" },
                        "content": { "type": "string" }
                    }
                }
            }
        }
    });

    let request = adk_core::LlmRequest::new(
        &model,
        vec![
            adk_core::Content::new("system").with_text(prompt),
            adk_core::Content::new("user").with_text(user_payload.to_string()),
        ],
    )
    .with_response_schema(schema)
    .with_config(adk_core::GenerateContentConfig {
        temperature: Some(0.1),
        max_output_tokens: Some(1200),
        ..Default::default()
    });

    let mut stream = client.generate_content(request, false).await?;
    let mut buffer = String::new();
    while let Some(item) = stream.next().await {
        let response = item?;
        if let Some(content) = response.content {
            for part in content.parts {
                if let Some(text) = part.text() {
                    buffer.push_str(text);
                }
            }
        }
    }
    if buffer.trim().is_empty() {
        anyhow::bail!("ADK planner returned an empty response");
    }

    let raw: AdkPlanResponse =
        serde_json::from_str(&buffer).with_context(|| "failed to parse ADK planner JSON")?;
    validate_adk_plan(raw, &model)
}

#[derive(Debug, Deserialize)]
struct AdkPlanResponse {
    hypothesis: String,
    #[serde(default)]
    ask: Option<AgentAsk>,
    actions: Vec<AdkPlanAction>,
}

#[derive(Debug, Deserialize)]
struct AdkPlanAction {
    kind: String,
    reason: String,
    command: Option<String>,
    working_dir: Option<String>,
    path: Option<String>,
    content: Option<String>,
}

fn validate_adk_plan(raw: AdkPlanResponse, model: &str) -> Result<AgentPlanSummary> {
    let mut actions = Vec::new();
    for action in raw.actions {
        match action.kind.as_str() {
            "run_command" => {
                let command = action
                    .command
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .context("ADK planner omitted command for run_command")?;
                parse_safe_command(command)?;
                actions.push(PlannedAction::RunCommand {
                    reason: action.reason,
                    command: command.to_string(),
                    working_dir: normalize_working_dir(action.working_dir.as_deref().unwrap_or("")),
                });
            }
            "write_file" => {
                let path = action
                    .path
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .context("ADK planner omitted path for write_file")?;
                sanitize_relative_path(Path::new(path))?;
                actions.push(PlannedAction::WriteFile {
                    reason: action.reason,
                    path: path.to_string(),
                    content: action.content.unwrap_or_default(),
                });
            }
            "finish" => actions.push(PlannedAction::Finish {
                reason: action.reason,
            }),
            other => anyhow::bail!("ADK planner returned unsupported action kind: {}", other),
        }
    }

    Ok(AgentPlanSummary {
        model: model.to_string(),
        hypothesis: raw.hypothesis,
        actions,
        ask: raw.ask,
    })
}

async fn emit_agent_surface(
    reporter: &std::sync::Arc<CliReporter>,
    use_progressive_ui: bool,
    label: &str,
    body: &str,
) -> Result<()> {
    if reporter.is_json() {
        return Ok(());
    }

    if use_progressive_ui {
        crate::progressive_ui::show_step(format!("{}: {}", label, body))?;
    } else {
        reporter.notify(format!("{}: {}", label, body)).await?;
    }
    Ok(())
}

fn emit_agent_json_event(
    reporter: &std::sync::Arc<CliReporter>,
    event_type: &str,
    payload: Value,
) -> Result<()> {
    if reporter.is_json() {
        println!(
            "{}",
            serde_json::to_string(&json!({
                "type": event_type,
                "payload": payload,
            }))?
        );
    }
    Ok(())
}

fn confirm_agent_ask(
    _reporter: &std::sync::Arc<CliReporter>,
    assume_yes: bool,
    use_progressive_ui: bool,
    prompt: &str,
) -> Result<bool> {
    if assume_yes {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return Err(AtoExecutionError::manual_intervention_required(
            "agent setup requires interactive approval but no TTY is available",
            None,
            vec!["re-run with --yes to auto-approve the agent ask".to_string()],
        )
        .into());
    }
    crate::progressive_ui::confirm_with_fallback(prompt, true, use_progressive_ui)
}

fn load_skill_context(project_root: &Path) -> Result<Option<String>> {
    for relative in [Path::new(".skills/setup/SKILL.md"), Path::new("SKILL.md")] {
        let path = project_root.join(relative);
        if path.exists() {
            let content = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let trimmed = content.trim();
            if !trimmed.is_empty() {
                return Ok(Some(trimmed.to_string()));
            }
        }
    }
    Ok(None)
}

fn rewrite_shadow_manifest_to_source(plan: &ManifestData, manifest_path: &Path) -> Result<String> {
    let manifest_text = fs::read_to_string(manifest_path)
        .with_context(|| format!("failed to read manifest {}", manifest_path.display()))?;
    let mut document = manifest_text
        .parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("failed to parse manifest {}", manifest_path.display()))?;
    document["targets"][plan.selected_target_label()]["working_dir"] = toml_edit::value("source");
    Ok(document.to_string())
}

fn normalize_working_dir(working_dir: &str) -> String {
    let trimmed = working_dir.trim();
    if trimmed.is_empty() || trimmed == "." {
        ".".to_string()
    } else {
        trimmed.to_string()
    }
}

fn parse_safe_command(command: &str) -> Result<Vec<String>> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        anyhow::bail!("agent command is empty");
    }
    if trimmed.contains("&&")
        || trimmed.contains("||")
        || trimmed.contains(';')
        || trimmed.contains('|')
        || trimmed.contains('>')
        || trimmed.contains('<')
        || trimmed.contains('\n')
    {
        anyhow::bail!("agent command contains unsupported shell control operators");
    }
    let parsed = shell_words::split(trimmed).with_context(|| "failed to parse agent command")?;
    let Some(program) = parsed.first() else {
        anyhow::bail!("agent command is empty");
    };
    if !matches!(
        program.as_str(),
        "cargo" | "npm" | "pnpm" | "bun" | "uv" | "deno"
    ) {
        anyhow::bail!("agent command '{}' is not allowlisted", program);
    }
    Ok(parsed)
}

fn sanitize_relative_path(path: &Path) -> Result<PathBuf> {
    let mut sanitized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => sanitized.push(part),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!("path escapes the shadow workspace: {}", path.display());
            }
        }
    }
    Ok(sanitized)
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()
            .with_context(|| "failed to read current directory")?
            .join(path))
    }
}

fn copy_workspace_snapshot(
    source_root: &Path,
    destination_root: &Path,
    shadow_root: &Path,
) -> Result<()> {
    for entry in WalkDir::new(source_root).into_iter().filter_map(Result::ok) {
        let path = entry.path();
        if path == shadow_root || path.starts_with(shadow_root) {
            continue;
        }
        let relative = match path.strip_prefix(source_root) {
            Ok(relative) if !relative.as_os_str().is_empty() => relative,
            _ => continue,
        };
        if should_skip_snapshot(relative) {
            continue;
        }
        let destination = destination_root.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&destination)
                .with_context(|| format!("failed to create {}", destination.display()))?;
            continue;
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::copy(path, &destination).with_context(|| {
            format!(
                "failed to copy workspace file {} -> {}",
                path.display(),
                destination.display()
            )
        })?;
    }
    Ok(())
}

fn should_skip_snapshot(relative: &Path) -> bool {
    let components = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    components
        .windows(2)
        .any(|window| window[0].as_str() == ".ato" && window[1].as_str() == "tmp")
        || components.iter().any(|name| {
            matches!(
                name.as_str(),
                ".git" | ".tmp" | "target" | "node_modules" | ".venv"
            )
        })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use capsule_core::router::ExecutionProfile;

    fn test_plan(dir: &Path, manifest: &str) -> ManifestData {
        let manifest_path = dir.join("capsule.toml");
        std::fs::write(&manifest_path, manifest).expect("manifest");
        capsule_core::router::execution_descriptor_from_manifest_parts(
            toml::from_str(&std::fs::read_to_string(&manifest_path).expect("read")).expect("parse"),
            manifest_path,
            dir.to_path_buf(),
            ExecutionProfile::Dev,
            Some("app"),
            HashMap::new(),
        )
        .expect("execution descriptor")
    }

    #[test]
    fn classifier_rejects_missing_env() {
        let err = anyhow::anyhow!(AtoExecutionError::missing_required_env(
            "missing required environment variables for target 'app': DATABASE_URL",
            vec!["DATABASE_URL".to_string()],
            Some("app"),
        ));
        assert!(AgentFailureClassifier::classify(&err, "prepare").is_none());
    }

    #[test]
    fn classifier_accepts_lock_incomplete() {
        let err = anyhow::anyhow!(AtoExecutionError::lock_incomplete(
            "source/node target requires one of package-lock.json, yarn.lock, pnpm-lock.yaml, bun.lock, or bun.lockb",
            Some("package-lock.json"),
        ));
        let classified = AgentFailureClassifier::classify(&err, "prepare").expect("classified");
        assert_eq!(classified.kind, SetupFailureKind::MissingLockfile);
    }

    #[test]
    fn session_store_uses_ato_tmp_agent_and_skips_tmp_copy() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("capsule.toml"), "name = 'demo'").expect("manifest");
        std::fs::write(tmp.path().join("package.json"), "{}").expect("package json");
        std::fs::create_dir_all(tmp.path().join(".tmp").join("ignore")).expect("tmp");
        std::fs::write(tmp.path().join(".tmp").join("ignore").join("x"), "secret")
            .expect("tmp file");
        std::fs::create_dir_all(tmp.path().join(".ato").join("tmp").join("ignore"))
            .expect("ato tmp");
        std::fs::write(
            tmp.path().join(".ato").join("tmp").join("ignore").join("y"),
            "secret",
        )
        .expect("ato tmp file");

        let store =
            AgentSessionStore::create(tmp.path(), tmp.path(), &tmp.path().join("capsule.toml"))
                .expect("store");
        assert!(store
            .artifact_dir()
            .to_string_lossy()
            .contains(".ato/tmp/agent/runs/run-"));
        assert!(store.workspace_dir().join("package.json").exists());
        assert!(!store.workspace_dir().join(".tmp").exists());
        assert!(!store.workspace_dir().join(".ato").join("tmp").exists());
    }

    #[test]
    fn heuristic_plan_generates_node_lockfile_action() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("package.json"), "{}").expect("package json");
        let plan = test_plan(
            tmp.path(),
            r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
driver = "node"
run_command = "node server.js"
"#,
        );
        let request = AgentRunRequest {
            project_root: tmp.path().to_path_buf(),
            source_root: tmp.path().to_path_buf(),
            manifest_path: tmp.path().join("capsule.toml"),
            plan: plan.clone(),
            launch_ctx: RuntimeLaunchContext::empty(),
            trigger: "prepare".to_string(),
            failure: Some(ClassifiedFailure {
                stage: "prepare".to_string(),
                kind: SetupFailureKind::MissingLockfile,
                message: "missing".to_string(),
            }),
            force_reroute: false,
            reporter: std::sync::Arc::new(CliReporter::new(false)),
            assume_yes: true,
            use_progressive_ui: false,
        };
        let store =
            AgentSessionStore::create(tmp.path(), tmp.path(), &tmp.path().join("capsule.toml"))
                .expect("store");
        let executor =
            AgentToolExecutor::new(store, plan, RuntimeLaunchContext::empty()).expect("executor");
        let inspection = executor.inspect_workspace();
        let summary = heuristic_plan(&request, &inspection);
        let has_npm = which::which("npm").is_ok();
        let planned_lockfile_generation = summary.actions.iter().any(|action| {
            matches!(
                action,
                PlannedAction::RunCommand { command, .. }
                    if command == "npm install --package-lock-only --ignore-scripts"
            )
        });
        assert_eq!(planned_lockfile_generation, has_npm);
    }

    #[test]
    fn parse_safe_command_rejects_shell_control_operators() {
        assert!(parse_safe_command("npm ci && npm run build").is_err());
        assert!(parse_safe_command("rm -rf /").is_err());
    }
}
