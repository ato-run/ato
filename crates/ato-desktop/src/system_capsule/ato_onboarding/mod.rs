use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result as AnyhowResult, anyhow, bail};
use capsule_core::runtime_setup::ToolKind;
use gpui::{AnyWindowHandle, App};
use serde::Deserialize;
use serde_json::Value;

use crate::config::{DesktopConfig, load_config, save_config};
use crate::proc_util::CommandNoWindowExt;
use crate::system_capsule::broker::{BrokerError, Capability};
use crate::window::onboarding_window::ActiveOnboardingShell;

pub const ONBOARDING_VERSION: u16 = 1;

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OnboardingCommand {
    Complete {
        version: u16,
        #[serde(default)]
        skipped: bool,
    },
    /// Persist the host runtime-setup preferences collected on the Runtime
    /// Setup onboarding step (issue #420 revision). Sent before `Complete` so
    /// the choices land in desktop config regardless of how the flow finishes.
    /// Every field defaults on (opt-out), so a missing field is treated as
    /// enabled — the keyboard "finish" path and the button submit the same set.
    SaveRuntimeSetupSettings {
        /// Whether Podman may be used as an OCI provider (`runtime.podman_enabled`).
        #[serde(default = "default_true")]
        podman_enabled: bool,
        /// Whether Ato may install an Ato-managed Node when a recipe needs it.
        #[serde(default = "default_true")]
        node_install_enabled: bool,
        /// Whether Ato may install an Ato-managed uv when a recipe needs it.
        #[serde(default = "default_true")]
        uv_install_enabled: bool,
        /// Whether Ato may install an Ato-managed Python when a recipe needs it.
        #[serde(default = "default_true")]
        python_install_enabled: bool,
    },
    /// Ask the bundled `ato` helper for the current runtime/tool status.
    LoadRuntimeSetupStatus {
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Foreground-install selected Ato-managed tools. The UI receives streamed
    /// progress via `window.__ATO_ONBOARDING_HYDRATE__`.
    InstallRuntimeTools {
        #[serde(default)]
        request_id: Option<String>,
        #[serde(default)]
        tools: Vec<String>,
    },
    /// Cancel an in-flight foreground runtime install.
    CancelRuntimeInstall {
        #[serde(default)]
        request_id: Option<String>,
    },
}

fn default_true() -> bool {
    true
}

impl OnboardingCommand {
    pub fn required_capability(&self) -> Capability {
        // Runtime setup is scoped to first-run onboarding: status reads,
        // foreground installs, preference persistence, and final completion all
        // live behind the same onboarding-only capability.
        Capability::OnboardingComplete
    }
}

#[derive(Clone)]
struct RuntimeInstallJob {
    cancel_requested: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
}

impl RuntimeInstallJob {
    fn new() -> Self {
        Self {
            cancel_requested: Arc::new(AtomicBool::new(false)),
            child: Arc::new(Mutex::new(None)),
        }
    }

    fn cancel(&self) {
        self.cancel_requested.store(true, Ordering::Release);
        if let Ok(mut child) = self.child.lock()
            && let Some(child) = child.as_mut()
        {
            let _ = child.kill();
        }
    }
}

#[derive(Default)]
struct ActiveOnboardingRuntimeInstall(Option<RuntimeInstallJob>);

impl gpui::Global for ActiveOnboardingRuntimeInstall {}

struct RuntimeInstallUiEvent {
    payload: Value,
    terminal: bool,
}

pub fn should_show_onboarding(config: &DesktopConfig) -> bool {
    !config.desktop.onboarding.completed && config.desktop.onboarding.version < ONBOARDING_VERSION
}

pub fn dispatch(
    cx: &mut App,
    host: AnyWindowHandle,
    command: OnboardingCommand,
) -> Result<(), BrokerError> {
    match command {
        OnboardingCommand::Complete { version, skipped } => {
            cancel_active_install(cx);
            let mut config = load_config();
            config.desktop.onboarding.completed = true;
            config.desktop.onboarding.skipped = skipped;
            config.desktop.onboarding.version = version.max(ONBOARDING_VERSION);
            let startup_surface = config.desktop.startup_surface;
            save_config(&config);

            let _ = host.update(cx, |_, window, _| window.remove_window());

            crate::window::open_configured_startup_surface(cx, startup_surface)
                .map_err(|err| BrokerError::Internal(err.to_string()))?;
        }
        OnboardingCommand::SaveRuntimeSetupSettings {
            podman_enabled,
            node_install_enabled,
            uv_install_enabled,
            python_install_enabled,
        } => {
            // Persist only — do not close the window or open the startup
            // surface. The onboarding page sends this immediately before the
            // terminal `Complete` command, which owns the window teardown.
            let mut config = load_config();
            apply_runtime_setup(
                &mut config,
                podman_enabled,
                node_install_enabled,
                uv_install_enabled,
                python_install_enabled,
            );
            save_config(&config);
        }
        OnboardingCommand::LoadRuntimeSetupStatus { request_id } => {
            spawn_runtime_setup_status(cx, request_id);
        }
        OnboardingCommand::InstallRuntimeTools { request_id, tools } => {
            start_runtime_install(cx, request_id, tools);
        }
        OnboardingCommand::CancelRuntimeInstall { request_id } => {
            let cancelled = cancel_active_install(cx);
            let response = serde_json::json!({
                "ok": cancelled,
                "requestId": request_id,
                "runtimeInstallCancelled": cancelled,
                "error": if cancelled { Value::Null } else { serde_json::json!({ "message": "no runtime install is active" }) },
            });
            push_to_onboarding_webview(cx, &response.to_string());
        }
    }

    Ok(())
}

fn ensure_install_global(cx: &mut App) {
    if cx.try_global::<ActiveOnboardingRuntimeInstall>().is_none() {
        cx.set_global(ActiveOnboardingRuntimeInstall::default());
    }
}

fn cancel_active_install(cx: &mut App) -> bool {
    ensure_install_global(cx);
    let Some(job) = cx.global_mut::<ActiveOnboardingRuntimeInstall>().0.take() else {
        return false;
    };
    job.cancel();
    true
}

fn spawn_runtime_setup_status(cx: &mut App, request_id: Option<String>) {
    let async_app = cx.to_async();
    let fe = cx.foreground_executor().clone();
    let be = cx.background_executor().clone();
    let be_for_work = be.clone();
    fe.spawn(async move {
        let payload = be_for_work
            .spawn(async move { runtime_setup_status_response(request_id) })
            .await;
        crate::webview_init_guard::wait_until_idle(&be).await;
        async_app.update(move |cx| {
            push_to_onboarding_webview(cx, &payload.to_string());
        });
    })
    .detach();
}

fn runtime_setup_status_response(request_id: Option<String>) -> Value {
    match crate::orchestrator::resolve_ato_binary().and_then(|ato| run_setup_status(&ato)) {
        Ok(status) => serde_json::json!({
            "ok": true,
            "requestId": request_id,
            "runtimeSetupStatus": status,
        }),
        Err(err) => serde_json::json!({
            "ok": false,
            "requestId": request_id,
            "error": { "message": format!("{err:#}") },
        }),
    }
}

fn run_setup_status(ato: &Path) -> AnyhowResult<Value> {
    let output = Command::new(ato)
        .no_console_window()
        .args(["internal", "runtime", "setup-status", "--json"])
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("failed to run {}", ato.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("runtime setup-status failed: {}", stderr.trim());
    }
    serde_json::from_slice(&output.stdout).context("runtime setup-status emitted invalid JSON")
}

fn start_runtime_install(cx: &mut App, request_id: Option<String>, tools: Vec<String>) {
    ensure_install_global(cx);
    if cx.global::<ActiveOnboardingRuntimeInstall>().0.is_some() {
        push_runtime_setup_error(cx, request_id, "a runtime install is already running");
        return;
    }

    let tools = match parse_installable_tools(&tools) {
        Ok(tools) => tools,
        Err(err) => {
            push_runtime_setup_error(cx, request_id, &format!("{err:#}"));
            return;
        }
    };
    let ato = match crate::orchestrator::resolve_ato_binary() {
        Ok(ato) => ato,
        Err(err) => {
            push_runtime_setup_error(
                cx,
                request_id,
                &format!("failed to resolve ato helper: {err:#}"),
            );
            return;
        }
    };

    let job = RuntimeInstallJob::new();
    cx.global_mut::<ActiveOnboardingRuntimeInstall>().0 = Some(job.clone());
    let started = serde_json::json!({
        "ok": true,
        "requestId": request_id,
        "runtimeInstallStarted": { "tools": tools.clone() },
    });
    push_to_onboarding_webview(cx, &started.to_string());

    let (tx, rx) = std::sync::mpsc::channel::<RuntimeInstallUiEvent>();
    let worker_request_id = request_id.clone();
    let worker_job = job.clone();
    std::thread::spawn(move || {
        run_install_worker(ato, tools, worker_request_id, worker_job, tx);
    });

    let async_app = cx.to_async();
    let fe = cx.foreground_executor().clone();
    let be = cx.background_executor().clone();
    fe.spawn(async move {
        loop {
            let mut terminal = false;
            while let Ok(event) = rx.try_recv() {
                terminal |= event.terminal;
                let payload = event.payload.to_string();
                crate::webview_init_guard::wait_until_idle(&be).await;
                async_app.update(move |cx| {
                    if terminal {
                        ensure_install_global(cx);
                        cx.global_mut::<ActiveOnboardingRuntimeInstall>().0 = None;
                    }
                    push_to_onboarding_webview(cx, &payload);
                });
            }
            if terminal {
                break;
            }
            be.timer(std::time::Duration::from_millis(50)).await;
        }
    })
    .detach();
}

fn parse_installable_tools(tools: &[String]) -> AnyhowResult<Vec<String>> {
    if tools.is_empty() {
        bail!("no runtime tools selected");
    }
    let mut parsed = Vec::with_capacity(tools.len());
    for tool in tools {
        let kind =
            ToolKind::parse_tool(tool).ok_or_else(|| anyhow!("unknown runtime tool: {tool}"))?;
        if !kind.is_managed_installable() {
            bail!(
                "{} cannot be installed by Ato during onboarding",
                kind.as_str()
            );
        }
        let token = kind.as_str().to_string();
        if !parsed.contains(&token) {
            parsed.push(token);
        }
    }
    Ok(parsed)
}

fn run_install_worker(
    ato: PathBuf,
    tools: Vec<String>,
    request_id: Option<String>,
    job: RuntimeInstallJob,
    tx: std::sync::mpsc::Sender<RuntimeInstallUiEvent>,
) {
    let tools_arg = tools.join(",");
    let mut command = Command::new(&ato);
    command
        .no_console_window()
        .args([
            "internal", "runtime", "install", "--tools", &tools_arg, "--json",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            send_terminal_install_event(
                &tx,
                serde_json::json!({
                    "ok": false,
                    "requestId": request_id,
                    "runtimeInstallComplete": {
                        "success": false,
                        "canceled": false,
                        "error": format!("failed to start runtime install: {err}"),
                    },
                }),
            );
            return;
        }
    };

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    if let Ok(mut slot) = job.child.lock() {
        *slot = Some(child);
    }

    let stderr_reader = stderr.map(|stderr| {
        std::thread::spawn(move || {
            let mut text = String::new();
            let mut reader = BufReader::new(stderr);
            let _ = reader.read_to_string(&mut text);
            text
        })
    });

    if let Some(stdout) = stdout {
        for line in BufReader::new(stdout).lines() {
            if job.cancel_requested.load(Ordering::Acquire) {
                break;
            }
            match line {
                Ok(line) if !line.trim().is_empty() => {
                    let event = serde_json::from_str::<Value>(&line).unwrap_or_else(|err| {
                        serde_json::json!({
                            "phase": "failed",
                            "message": format!("invalid install progress JSON: {err}"),
                        })
                    });
                    let _ = tx.send(RuntimeInstallUiEvent {
                        payload: serde_json::json!({
                            "ok": true,
                            "requestId": request_id.clone(),
                            "runtimeInstallProgress": event,
                        }),
                        terminal: false,
                    });
                }
                Ok(_) => {}
                Err(err) => {
                    let _ = tx.send(RuntimeInstallUiEvent {
                        payload: serde_json::json!({
                            "ok": false,
                            "requestId": request_id.clone(),
                            "error": { "message": format!("failed to read install progress: {err}") },
                        }),
                        terminal: false,
                    });
                    break;
                }
            }
        }
    }

    let status = match job.child.lock() {
        Ok(mut slot) => slot.take().map(|mut child| child.wait()),
        Err(_) => None,
    };
    let stderr = stderr_reader
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    let canceled = job.cancel_requested.load(Ordering::Acquire);
    let success = matches!(status, Some(Ok(status)) if status.success()) && !canceled;
    let setup_status = if canceled {
        None
    } else {
        run_setup_status(&ato).ok()
    };
    let error = if success {
        None
    } else if canceled {
        Some("runtime install cancelled".to_string())
    } else if !stderr.trim().is_empty() {
        Some(stderr.trim().to_string())
    } else {
        Some("runtime install failed".to_string())
    };

    send_terminal_install_event(
        &tx,
        serde_json::json!({
            "ok": success,
            "requestId": request_id.clone(),
            "runtimeInstallComplete": {
                "success": success,
                "canceled": canceled,
                "error": error,
                "status": setup_status,
            },
        }),
    );
}

fn send_terminal_install_event(
    tx: &std::sync::mpsc::Sender<RuntimeInstallUiEvent>,
    payload: Value,
) {
    let _ = tx.send(RuntimeInstallUiEvent {
        payload,
        terminal: true,
    });
}

fn push_runtime_setup_error(cx: &mut App, request_id: Option<String>, message: &str) {
    let response = serde_json::json!({
        "ok": false,
        "requestId": request_id,
        "error": { "message": message },
    });
    push_to_onboarding_webview(cx, &response.to_string());
}

fn push_to_onboarding_webview(cx: &mut App, payload_json: &str) {
    let weak = cx
        .try_global::<ActiveOnboardingShell>()
        .and_then(|g| g.0.clone());
    let Some(weak) = weak else {
        return;
    };
    let Some(entity) = weak.upgrade() else {
        return;
    };
    let payload = payload_json.to_string();
    entity.update(cx, |shell, _cx| {
        shell.hydrate(&payload);
    });
}

/// Apply the runtime-setup preferences to an in-memory config. Pure so the
/// persistence semantics are unit-testable without an `App` or disk I/O.
fn apply_runtime_setup(
    config: &mut DesktopConfig,
    podman_enabled: bool,
    node_install_enabled: bool,
    uv_install_enabled: bool,
    python_install_enabled: bool,
) {
    config.runtime.podman_enabled = podman_enabled;
    config.runtime_setup.node_install_enabled = node_install_enabled;
    config.runtime_setup.uv_install_enabled = uv_install_enabled;
    config.runtime_setup.python_install_enabled = python_install_enabled;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DesktopConfig;
    use crate::system_capsule::broker::Capability;

    #[test]
    fn should_show_onboarding_for_default_config() {
        assert!(should_show_onboarding(&DesktopConfig::default()));
    }

    #[test]
    fn should_not_show_when_completed() {
        let mut cfg = DesktopConfig::default();
        cfg.desktop.onboarding.completed = true;
        cfg.desktop.onboarding.version = ONBOARDING_VERSION;
        assert!(!should_show_onboarding(&cfg));
    }

    #[test]
    fn skipped_and_completed_stays_hidden() {
        let mut cfg = DesktopConfig::default();
        cfg.desktop.onboarding.completed = true;
        cfg.desktop.onboarding.skipped = true;
        cfg.desktop.onboarding.version = ONBOARDING_VERSION;
        assert!(!should_show_onboarding(&cfg));
    }

    #[test]
    fn complete_requires_onboarding_capability() {
        let cmd = OnboardingCommand::Complete {
            version: ONBOARDING_VERSION,
            skipped: false,
        };
        assert_eq!(cmd.required_capability(), Capability::OnboardingComplete);
    }

    #[test]
    fn save_runtime_setup_parses_disabled_values() {
        let json = r#"{
            "kind": "save_runtime_setup_settings",
            "podman_enabled": false,
            "node_install_enabled": false,
            "uv_install_enabled": false,
            "python_install_enabled": false
        }"#;
        let cmd: OnboardingCommand = serde_json::from_str(json).unwrap();
        match cmd {
            OnboardingCommand::SaveRuntimeSetupSettings {
                podman_enabled,
                node_install_enabled,
                uv_install_enabled,
                python_install_enabled,
            } => {
                assert!(!podman_enabled);
                assert!(!node_install_enabled);
                assert!(!uv_install_enabled);
                assert!(!python_install_enabled);
            }
            other => panic!("expected SaveRuntimeSetupSettings, got {other:?}"),
        }
    }

    #[test]
    fn save_runtime_setup_defaults_missing_fields_to_enabled() {
        let json = r#"{"kind": "save_runtime_setup_settings"}"#;
        let cmd: OnboardingCommand = serde_json::from_str(json).unwrap();
        match cmd {
            OnboardingCommand::SaveRuntimeSetupSettings {
                podman_enabled,
                node_install_enabled,
                uv_install_enabled,
                python_install_enabled,
            } => {
                assert!(podman_enabled);
                assert!(node_install_enabled);
                assert!(uv_install_enabled);
                assert!(python_install_enabled);
            }
            other => panic!("expected SaveRuntimeSetupSettings, got {other:?}"),
        }
    }

    #[test]
    fn save_runtime_setup_requires_onboarding_capability() {
        let cmd = OnboardingCommand::SaveRuntimeSetupSettings {
            podman_enabled: false,
            node_install_enabled: false,
            uv_install_enabled: false,
            python_install_enabled: false,
        };
        assert_eq!(cmd.required_capability(), Capability::OnboardingComplete);
    }

    #[test]
    fn runtime_setup_commands_require_onboarding_capability() {
        let load = OnboardingCommand::LoadRuntimeSetupStatus {
            request_id: Some("r1".to_string()),
        };
        let install = OnboardingCommand::InstallRuntimeTools {
            request_id: Some("r2".to_string()),
            tools: vec!["node".to_string()],
        };
        let cancel = OnboardingCommand::CancelRuntimeInstall {
            request_id: Some("r3".to_string()),
        };
        assert_eq!(load.required_capability(), Capability::OnboardingComplete);
        assert_eq!(
            install.required_capability(),
            Capability::OnboardingComplete
        );
        assert_eq!(cancel.required_capability(), Capability::OnboardingComplete);
    }

    #[test]
    fn parse_installable_tools_allows_managed_language_tools() {
        let tools = vec!["node".to_string(), "uv".to_string(), "python".to_string()];
        assert_eq!(
            parse_installable_tools(&tools).unwrap(),
            vec!["node".to_string(), "uv".to_string(), "python".to_string()]
        );
    }

    #[test]
    fn parse_installable_tools_rejects_detection_only_tools() {
        let tools = vec!["podman".to_string()];
        let err = parse_installable_tools(&tools).unwrap_err().to_string();
        assert!(err.contains("cannot be installed"));
    }

    #[test]
    fn apply_runtime_setup_sets_flags() {
        let mut config = DesktopConfig::default();
        assert!(config.runtime.podman_enabled);
        assert!(config.runtime_setup.node_install_enabled);

        apply_runtime_setup(&mut config, false, false, false, false);
        assert!(!config.runtime.podman_enabled);
        assert!(!config.runtime_setup.node_install_enabled);
        assert!(!config.runtime_setup.uv_install_enabled);
        assert!(!config.runtime_setup.python_install_enabled);

        apply_runtime_setup(&mut config, true, true, false, true);
        assert!(config.runtime.podman_enabled);
        assert!(config.runtime_setup.node_install_enabled);
        assert!(!config.runtime_setup.uv_install_enabled);
        assert!(config.runtime_setup.python_install_enabled);
        // check_host_tools_on_startup is not touched by onboarding.
        assert!(config.runtime_setup.check_host_tools_on_startup);
    }
}
