//! Wire types for the Runtime Setup feature IPC.
//!
//! These are *feature-level* commands, not app-level: both `ato-onboarding`
//! and `ato-settings` post the exact same `kind`s. The capability gate (see
//! [`RuntimeSetupCommand::required_capability`]) decides what each calling
//! capsule is allowed to do — the command vocabulary itself is shared.

use serde::Deserialize;

use crate::system_capsule::broker::Capability;

/// The Runtime Setup IPC vocabulary. Command `kind`s are feature names
/// (`runtime_setup_status`, `install_runtime_tools`, …) rather than app names
/// (`onboarding_install_…`), so the same backend serves every surface.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuntimeSetupCommand {
    /// Ask the bundled `ato` helper for the current runtime/tool status.
    RuntimeSetupStatus {
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Persist runtime-setup preferences (Podman use + Ato-managed
    /// Node/uv/Python installs). Each field is optional: a field that is
    /// **omitted** (`None`) leaves the stored value untouched. This lets the
    /// Settings → Runtime tab persist only the toggles it actually owns without
    /// silently resetting others (e.g. re-enabling Podman that the user opted
    /// out of during onboarding). Onboarding sends all four explicitly.
    SaveRuntimeSetupSettings {
        #[serde(default)]
        podman_enabled: Option<bool>,
        #[serde(default)]
        node_install_enabled: Option<bool>,
        #[serde(default)]
        uv_install_enabled: Option<bool>,
        #[serde(default)]
        python_install_enabled: Option<bool>,
    },
    /// Foreground-install selected Ato-managed tools. Progress is streamed back
    /// to the calling surface's WebView hydrate hook.
    InstallRuntimeTools {
        #[serde(default)]
        request_id: Option<String>,
        #[serde(default)]
        tools: Vec<String>,
    },
    /// Foreground-prepare a host runtime (Podman): install when supported,
    /// create/start the Ato-managed `ato-podman` machine, verify readiness.
    /// Semantically distinct from `InstallRuntimeTools` (managed toolchains)
    /// even though both stream progress through the same hydrate fields — this
    /// is the only host-state-mutating runtime command and is gated by the
    /// dedicated [`Capability::RuntimeSetupPrepare`].
    PrepareRuntimeTools {
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
    /// Reveal the desktop log directory so the user can inspect runtime-setup
    /// failures. Settings-only (gated by [`Capability::RuntimeSetupOpenLogs`]).
    OpenRuntimeSetupLogs {
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Execute a Windows substrate remediation the Desktop offered (#460 PR2):
    /// enable WSL / WSL2, write a reboot-resume marker, or open virtualization
    /// guidance. Host-state-mutating, so gated by
    /// [`Capability::RuntimeSetupPrepare`]; streams progress like prepare.
    PrepareWindowsRuntimeSubstrate {
        #[serde(default)]
        request_id: Option<String>,
        /// One of the `WindowsSubstrateActionKind` tokens, e.g. `install_wsl`,
        /// `enable_wsl2`, `reboot_required`, `open_virtualization_instructions`.
        action: String,
        /// Which surface initiated it (`onboarding` | `settings`).
        #[serde(default)]
        source_surface: Option<String>,
    },
    /// Repair the Ato-managed Podman machine (restart + verify). Host-mutating,
    /// gated by [`Capability::RuntimeSetupPrepare`]. (#460 PR2)
    RepairHostRuntime {
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Resume Runtime Setup after a reboot: re-check the substrate and report the
    /// next step. Read-only, gated by [`Capability::RuntimeSetupRead`]. (#460 PR2)
    ResumeRuntimeSetupAfterReboot {
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Cancel a pending interrupted-launch (#460 PR3b): clears the launch-intent
    /// marker so Runtime Setup will no longer resume into it. Does not cancel
    /// Runtime Setup itself. Clears a local advisory marker only, so it is gated
    /// by [`Capability::RuntimeSetupRead`].
    CancelPendingLaunch {
        #[serde(default)]
        request_id: Option<String>,
    },
}

impl RuntimeSetupCommand {
    /// The feature-level capability this command requires. The broker checks it
    /// against the *calling capsule's* manifest grant, so onboarding and
    /// settings can share the vocabulary while differing on what they may do.
    pub fn required_capability(&self) -> Capability {
        match self {
            RuntimeSetupCommand::RuntimeSetupStatus { .. } => Capability::RuntimeSetupRead,
            RuntimeSetupCommand::SaveRuntimeSetupSettings { .. } => Capability::RuntimeSetupInstall,
            RuntimeSetupCommand::InstallRuntimeTools { .. } => Capability::RuntimeSetupInstall,
            RuntimeSetupCommand::PrepareRuntimeTools { .. } => Capability::RuntimeSetupPrepare,
            RuntimeSetupCommand::CancelRuntimeInstall { .. } => Capability::RuntimeSetupInstall,
            RuntimeSetupCommand::OpenRuntimeSetupLogs { .. } => Capability::RuntimeSetupOpenLogs,
            // Substrate remediation + repair mutate host state → same gate as prepare.
            RuntimeSetupCommand::PrepareWindowsRuntimeSubstrate { .. } => {
                Capability::RuntimeSetupPrepare
            }
            RuntimeSetupCommand::RepairHostRuntime { .. } => Capability::RuntimeSetupPrepare,
            // Resume only re-reads status → read capability.
            RuntimeSetupCommand::ResumeRuntimeSetupAfterReboot { .. } => {
                Capability::RuntimeSetupRead
            }
            // Cancelling a pending launch only clears a local advisory marker.
            RuntimeSetupCommand::CancelPendingLaunch { .. } => Capability::RuntimeSetupRead,
        }
    }

    /// Whether a JSON `kind` string belongs to the Runtime Setup vocabulary.
    /// Used by the IPC parser to route shared commands away from the
    /// capsule-specific command enums.
    pub fn is_runtime_setup_kind(kind: &str) -> bool {
        matches!(
            kind,
            "runtime_setup_status"
                | "save_runtime_setup_settings"
                | "install_runtime_tools"
                | "prepare_runtime_tools"
                | "cancel_runtime_install"
                | "open_runtime_setup_logs"
                | "prepare_windows_runtime_substrate"
                | "repair_host_runtime"
                | "resume_runtime_setup_after_reboot"
                | "cancel_pending_launch"
        )
    }
}

/// A resolved `ato` helper that predates the `internal runtime` subcommand
/// emits this on stderr. Detecting it lets us surface a clear "your Ato helper
/// is too old" message instead of a raw clap error — this is the exact failure
/// that broke onboarding runtime install when the bundled/dev helper lagged
/// the desktop (issue #420 follow-up). Not a Windows-specific fault.
pub(crate) fn helper_lacks_runtime_subcommand(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("unrecognized subcommand") && s.contains("runtime")
}

/// User-facing message shown when the resolved helper is too old to run
/// Runtime Setup.
pub(crate) const HELPER_TOO_OLD_MESSAGE: &str = "The bundled Ato helper is too old to run Runtime Setup (missing the \
     `internal runtime` command). Reinstall or update Ato so the helper \
     matches this version.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_setup_kinds_are_recognised() {
        for kind in [
            "runtime_setup_status",
            "save_runtime_setup_settings",
            "install_runtime_tools",
            "prepare_runtime_tools",
            "cancel_runtime_install",
            "open_runtime_setup_logs",
        ] {
            assert!(RuntimeSetupCommand::is_runtime_setup_kind(kind));
        }
        assert!(!RuntimeSetupCommand::is_runtime_setup_kind("complete"));
        assert!(!RuntimeSetupCommand::is_runtime_setup_kind("load_snapshot"));
    }

    #[test]
    fn substrate_kinds_are_recognised() {
        for kind in [
            "prepare_windows_runtime_substrate",
            "repair_host_runtime",
            "resume_runtime_setup_after_reboot",
            "cancel_pending_launch",
        ] {
            assert!(RuntimeSetupCommand::is_runtime_setup_kind(kind));
        }
    }

    #[test]
    fn cancel_pending_launch_deserializes_and_is_read_gated() {
        let cmd: RuntimeSetupCommand =
            serde_json::from_str(r#"{"kind":"cancel_pending_launch"}"#).unwrap();
        match &cmd {
            RuntimeSetupCommand::CancelPendingLaunch { request_id } => {
                assert_eq!(request_id.as_deref(), None)
            }
            other => panic!("expected CancelPendingLaunch, got {other:?}"),
        }
        // Clearing a local advisory marker is read-gated (available on both
        // onboarding and settings surfaces).
        assert_eq!(cmd.required_capability(), Capability::RuntimeSetupRead);
    }

    #[test]
    fn substrate_commands_use_expected_capabilities() {
        let prepare = RuntimeSetupCommand::PrepareWindowsRuntimeSubstrate {
            request_id: None,
            action: "install_wsl".into(),
            source_surface: Some("onboarding".into()),
        };
        let repair = RuntimeSetupCommand::RepairHostRuntime { request_id: None };
        let resume = RuntimeSetupCommand::ResumeRuntimeSetupAfterReboot { request_id: None };
        // Host-mutating substrate actions are gated like prepare.
        assert_eq!(
            prepare.required_capability(),
            Capability::RuntimeSetupPrepare
        );
        assert_eq!(
            repair.required_capability(),
            Capability::RuntimeSetupPrepare
        );
        // Resume only re-reads status.
        assert_eq!(resume.required_capability(), Capability::RuntimeSetupRead);
    }

    #[test]
    fn prepare_windows_substrate_deserializes() {
        let cmd: RuntimeSetupCommand = serde_json::from_str(
            r#"{"kind":"prepare_windows_runtime_substrate","action":"enable_wsl2","source_surface":"settings"}"#,
        )
        .unwrap();
        match cmd {
            RuntimeSetupCommand::PrepareWindowsRuntimeSubstrate {
                action,
                source_surface,
                ..
            } => {
                assert_eq!(action, "enable_wsl2");
                assert_eq!(source_surface.as_deref(), Some("settings"));
            }
            other => panic!("expected PrepareWindowsRuntimeSubstrate, got {other:?}"),
        }
    }

    #[test]
    fn install_and_logs_use_distinct_capabilities() {
        let install = RuntimeSetupCommand::InstallRuntimeTools {
            request_id: None,
            tools: vec!["node".into()],
        };
        let prepare = RuntimeSetupCommand::PrepareRuntimeTools {
            request_id: None,
            tools: vec!["podman".into()],
        };
        let logs = RuntimeSetupCommand::OpenRuntimeSetupLogs { request_id: None };
        let status = RuntimeSetupCommand::RuntimeSetupStatus { request_id: None };
        assert_eq!(
            install.required_capability(),
            Capability::RuntimeSetupInstall
        );
        // Prepare (host-runtime mutation) is gated distinctly from install
        // (managed-toolchain provisioning).
        assert_eq!(
            prepare.required_capability(),
            Capability::RuntimeSetupPrepare
        );
        assert_eq!(logs.required_capability(), Capability::RuntimeSetupOpenLogs);
        assert_eq!(status.required_capability(), Capability::RuntimeSetupRead);
    }

    #[test]
    fn save_settings_omitted_fields_are_none() {
        // Settings → Runtime omits podman_enabled; it must deserialize to None
        // (preserve), never to a value that would silently reset the setting.
        let cmd: RuntimeSetupCommand = serde_json::from_str(
            r#"{"kind":"save_runtime_setup_settings","node_install_enabled":false}"#,
        )
        .unwrap();
        match cmd {
            RuntimeSetupCommand::SaveRuntimeSetupSettings {
                podman_enabled,
                node_install_enabled,
                uv_install_enabled,
                python_install_enabled,
            } => {
                assert_eq!(podman_enabled, None);
                assert_eq!(node_install_enabled, Some(false));
                assert_eq!(uv_install_enabled, None);
                assert_eq!(python_install_enabled, None);
            }
            other => panic!("expected SaveRuntimeSetupSettings, got {other:?}"),
        }
    }

    #[test]
    fn detects_old_helper_clap_error() {
        assert!(helper_lacks_runtime_subcommand(
            "error: unrecognized subcommand 'runtime'\n\nUsage: ato.exe internal"
        ));
        assert!(!helper_lacks_runtime_subcommand("network unreachable"));
    }
}
