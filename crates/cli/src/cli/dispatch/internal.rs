//! Dispatch for `ato internal *` plumbing commands.

use anyhow::{Result, anyhow};

use capsule::router::ExecutionProfile;

use capsule::runtime_setup::{ToolKind, WindowsSubstrateActionKind};

use crate::adapters::runtime::process::ProcessManager;
use crate::application::auth::consent_store::approve_execution_plan_consent;
use crate::application::preflight::collect_aggregate_requirements;
use crate::application::runtime_prepare::prepare_tools;
use crate::application::runtime_setup::{collect_setup_status, install_tools};
use crate::cli::{
    CapsuleProtocolInternalCommands, CapsuleSessionInternalCommands, ConsentInternalCommands,
    InternalCommands, RuntimeInternalCommands,
};

pub(crate) fn execute_internal_command(command: InternalCommands) -> Result<()> {
    match command {
        InternalCommands::CapsuleProtocol { command } => execute_capsule_protocol(command),
        InternalCommands::CapsuleSession { command } => execute_capsule_session(command),
        InternalCommands::Consent { command } => execute_consent_command(command),
        InternalCommands::Preflight {
            target,
            community_toml_id,
            json,
        } => execute_preflight_command(target, community_toml_id, json),
        InternalCommands::Runtime { command } => execute_runtime_command(command),
        InternalCommands::ImportPreviewSweep { force, json } => {
            execute_import_preview_sweep_command(force, json)
        }
    }
}

fn execute_capsule_session(command: CapsuleSessionInternalCommands) -> Result<()> {
    use crate::application::capsule_session;

    match command {
        CapsuleSessionInternalCommands::Start {
            bundle,
            into,
            no_attach,
        } => capsule_session::start(&bundle, &into, no_attach),
        CapsuleSessionInternalCommands::Serve {
            session,
            bundle,
            into,
        } => capsule_session::serve(&session, &bundle, &into),
        CapsuleSessionInternalCommands::Attach { session, observe } => {
            capsule_session::attach(&session, observe)
        }
        CapsuleSessionInternalCommands::Branch {
            session,
            into,
            no_attach,
        } => capsule_session::branch(&session, &into, no_attach),
        CapsuleSessionInternalCommands::Suspend { session } => capsule_session::suspend(&session),
        CapsuleSessionInternalCommands::Resume { session } => capsule_session::resume(&session),
        CapsuleSessionInternalCommands::Status { session } => capsule_session::status(&session),
        CapsuleSessionInternalCommands::Kill { session } => capsule_session::kill(&session),
        CapsuleSessionInternalCommands::List => capsule_session::list(),
        CapsuleSessionInternalCommands::Watchdog {
            pid,
            pgid,
            process_start_identity,
            lease_fd,
        } => capsule_session::watchdog(pid, pgid, &process_start_identity, lease_fd),
    }
}

fn execute_capsule_protocol(command: CapsuleProtocolInternalCommands) -> Result<()> {
    use std::collections::BTreeMap;

    use capsule::protocol_bundle::{PortableCapsule, capture_workspace_state};
    use capsule::protocol_runtime::{
        PtyConnector, ReplayEngine, StateRuntime, WorkspaceStateRuntime, pty_descriptor_connector,
        record_pty_command,
    };
    use capsule_protocol::{CURRENT_SCHEMA_VERSION, CapsuleDescriptor};

    match command {
        CapsuleProtocolInternalCommands::Capture {
            workspace,
            output,
            command,
        } => {
            let workspace = workspace.canonicalize().map_err(|error| {
                anyhow!(
                    "failed to resolve workspace {}: {error}",
                    workspace.display()
                )
            })?;
            let (state, object) = capture_workspace_state(&workspace)?;
            let records = record_pty_command(&workspace, &command)?;
            let state_ref = state.state_ref.clone();
            let (connector_id, connector) = pty_descriptor_connector();
            PortableCapsule {
                descriptor: CapsuleDescriptor {
                    schema_version: CURRENT_SCHEMA_VERSION,
                    base_state: state,
                    connectors: BTreeMap::from([(connector_id, connector)]),
                },
                records,
                objects: BTreeMap::from([(state_ref, object)]),
            }
            .write(&output)?;
            println!("wrote Capsule Protocol bundle: {}", output.display());
            Ok(())
        }
        CapsuleProtocolInternalCommands::Replay {
            bundle,
            into,
            no_continue,
        } => {
            let bundle = PortableCapsule::read(&bundle)?;
            let object = bundle
                .objects
                .get(&bundle.descriptor.base_state.state_ref)
                .ok_or_else(|| anyhow!("base state object is absent after bundle validation"))?;
            let state_runtime = WorkspaceStateRuntime {
                object,
                destination: &into,
            };
            let restored = state_runtime.restore(&bundle.descriptor.base_state)?;
            let mut connector = PtyConnector::open(&restored)?;
            let outcome =
                match ReplayEngine::replay(&bundle.descriptor, &bundle.records, &mut connector) {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        let _ = connector.shutdown();
                        return Err(error.into());
                    }
                };
            eprintln!(
                "Capsule Protocol replay complete: {} records; workspace={}",
                outcome.records_processed,
                restored.display()
            );
            if no_continue {
                connector.shutdown()?;
                Ok(())
            } else {
                connector.continue_interactive()?;
                Ok(())
            }
        }
    }
}

/// `ato internal runtime *` handler. `setup-status` always exits `Ok` (the
/// per-tool `action` carries the verdict); `install` exits non-zero if any
/// requested tool failed to install.
fn execute_runtime_command(command: RuntimeInternalCommands) -> Result<()> {
    match command {
        RuntimeInternalCommands::SetupStatus { json } => {
            let status = collect_setup_status();
            if json {
                println!("{}", serde_json::to_string(&status)?);
            } else {
                println!("runtime setup status:");
                for tool in &status.tools {
                    let version = tool.version.as_deref().unwrap_or("-");
                    println!(
                        "  - {:<14} ready={} version={} action={:?}",
                        tool.kind.as_str(),
                        tool.ready,
                        version,
                        tool.action
                    );
                }
            }
            Ok(())
        }
        RuntimeInternalCommands::Install { tools, json } => {
            let parsed = parse_tool_tokens(&tools)?;
            install_tools(parsed, json)
        }
        RuntimeInternalCommands::Prepare { tools, emit_json } => {
            let parsed = parse_tool_tokens(&tools)?;
            prepare_tools(parsed, emit_json)
        }
        RuntimeInternalCommands::RepairHostRuntime { emit_json } => {
            crate::application::runtime_prepare::repair_host_runtime(emit_json)
        }
        RuntimeInternalCommands::ResumeAfterReboot { json } => {
            crate::application::runtime_setup_resume::resume_after_reboot(json)
        }
        RuntimeInternalCommands::PrepareWindowsSubstrate {
            action,
            source_surface,
            emit_json,
        } => {
            let kind = parse_substrate_action(&action)?;
            crate::application::runtime_setup_resume::prepare_windows_substrate(
                kind,
                source_surface,
                emit_json,
            )
        }
    }
}

/// Parse a `--action` token into a [`WindowsSubstrateActionKind`].
fn parse_substrate_action(token: &str) -> Result<WindowsSubstrateActionKind> {
    use WindowsSubstrateActionKind as K;
    let kind = match token.trim().to_ascii_lowercase().replace('_', "-").as_str() {
        "install-wsl" => K::InstallWsl,
        "enable-wsl2" => K::EnableWsl2,
        "reboot-required" => K::RebootRequired,
        "open-virtualization-instructions" => K::OpenVirtualizationInstructions,
        "repair-podman-machine" => K::RepairPodmanMachine,
        "none" => K::None,
        other => return Err(anyhow!("unknown substrate action: {other}")),
    };
    Ok(kind)
}

/// Parse `--tools` tokens into [`ToolKind`]s, erroring on the first unknown one.
fn parse_tool_tokens(tokens: &[String]) -> Result<Vec<ToolKind>> {
    tokens
        .iter()
        .map(|token| {
            ToolKind::parse_tool(token).ok_or_else(|| anyhow!("unknown runtime tool: {token}"))
        })
        .collect()
}

/// `ato internal preflight <target> [--json]` handler. Delegates to
/// the side-effect-free preflight collector and serializes the result
/// for stdout.
///
/// Stdout policy:
/// - `--json`: single-line aggregate envelope. The desktop launch
///   worker scrapes this exact shape — see
///   `crate::application::preflight::AggregatePreflightResult` for the
///   field set.
/// - default: brief human-readable summary, one line per pending
///   requirement.
///
/// Exit policy: returns `Ok(())` regardless of whether requirements
/// are pending (the caller decides what to do based on the
/// `requirements` array). Non-zero exits are reserved for genuine
/// failures (manifest missing, derivation failed, consent store
/// unreadable). This matches `ato inspect requirements`'s convention.
fn execute_preflight_command(
    target: String,
    community_toml_id: Option<String>,
    json: bool,
) -> Result<()> {
    // If --community-toml-id is given, fetch and validate the community TOML,
    // write it to a temp file, and run preflight against that path so the
    // consent UI reflects the selected recipe's targets / secrets / policy.
    let _temp_dir_guard;
    let effective_target = if let Some(ctoml_id) = &community_toml_id {
        let toml_content =
            crate::community::fetch_and_validate_community_toml(ctoml_id, &target)
                .map_err(|err| anyhow!("preflight: community TOML fetch/validate failed: {err}"))?;

        let tmp = tempfile::Builder::new()
            .prefix("ato-preflight-community-")
            .tempdir()
            .map_err(|err| anyhow!("preflight: failed to create temp dir: {err}"))?;
        let toml_path = tmp.path().join("capsule.toml");
        std::fs::write(&toml_path, &toml_content)
            .map_err(|err| anyhow!("preflight: failed to write temp TOML: {err}"))?;
        let path_str = toml_path.display().to_string();
        _temp_dir_guard = Some(tmp);
        path_str
    } else {
        _temp_dir_guard = None;
        target.clone()
    };

    let result = collect_aggregate_requirements(&effective_target, ExecutionProfile::Dev)
        .map_err(|err| anyhow!("preflight collection failed: {err}"))?;

    if json {
        let payload = serde_json::json!({
            "schema_version": "1",
            "ok": result.is_empty(),
            "capsule_id": result.capsule_id,
            "capsule_version": result.capsule_version,
            "visited_targets": result.visited_targets,
            "requirements": result.requirements,
        });
        println!("{payload}");
    } else if result.is_empty() {
        println!(
            "preflight: {}@{} — no pending requirements; launch can proceed.",
            result.capsule_id, result.capsule_version
        );
    } else {
        println!(
            "preflight: {}@{} — {} requirement(s) across {} target(s):",
            result.capsule_id,
            result.capsule_version,
            result.requirements.len(),
            result.visited_targets.len()
        );
        for envelope in &result.requirements {
            println!("  - {}", envelope.display.message);
        }
    }
    Ok(())
}

fn execute_import_preview_sweep_command(force: bool, json: bool) -> Result<()> {
    let process_manager = ProcessManager::new()?;
    let report = process_manager.sweep_import_preview_sessions(force)?;
    let ok = report.stale_sessions_failed == 0;
    if json {
        let payload = serde_json::json!({
            "ok": ok,
            "import_preview": report,
        });
        println!("{payload}");
    } else {
        println!(
            "import-preview sweep: kept={}, stopped={}, already_gone={}, failed={}, env_groups_stopped={}",
            report.active_sessions_kept,
            report.stale_sessions_stopped,
            report.stale_sessions_already_gone,
            report.stale_sessions_failed,
            report.env_process_groups_stopped
        );
    }
    if ok {
        Ok(())
    } else {
        Err(anyhow!(
            "import-preview sweep reported stale session cleanup failures"
        ))
    }
}

fn execute_consent_command(command: ConsentInternalCommands) -> Result<()> {
    match command {
        ConsentInternalCommands::ApproveExecutionPlan {
            scoped_id,
            version,
            target_label,
            policy_segment_hash,
            provisioning_policy_hash,
            json,
        } => {
            approve_execution_plan_consent(
                &scoped_id,
                &version,
                &target_label,
                &policy_segment_hash,
                &provisioning_policy_hash,
            )?;

            if json {
                // Single-line JSON envelope, parse-friendly for the
                // desktop's CLI envelope reader.
                let payload = serde_json::json!({
                    "ok": true,
                    "consent": {
                        "scoped_id": scoped_id,
                        "version": version,
                        "target_label": target_label,
                        "policy_segment_hash": policy_segment_hash,
                        "provisioning_policy_hash": provisioning_policy_hash,
                    }
                });
                println!("{payload}");
            }
            Ok(())
        }
    }
}
