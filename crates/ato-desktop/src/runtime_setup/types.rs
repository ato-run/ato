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
    /// Persist the runtime-setup opt-out preferences (Podman use + Ato-managed
    /// Node/uv/Python installs). Every field defaults on (opt-out), so a
    /// missing field is treated as enabled.
    SaveRuntimeSetupSettings {
        #[serde(default = "default_true")]
        podman_enabled: bool,
        #[serde(default = "default_true")]
        node_install_enabled: bool,
        #[serde(default = "default_true")]
        uv_install_enabled: bool,
        #[serde(default = "default_true")]
        python_install_enabled: bool,
    },
    /// Foreground-install selected Ato-managed tools. Progress is streamed back
    /// to the calling surface's WebView hydrate hook.
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
    /// Reveal the desktop log directory so the user can inspect runtime-setup
    /// failures. Settings-only (gated by [`Capability::RuntimeSetupOpenLogs`]).
    OpenRuntimeSetupLogs {
        #[serde(default)]
        request_id: Option<String>,
    },
}

fn default_true() -> bool {
    true
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
            RuntimeSetupCommand::CancelRuntimeInstall { .. } => Capability::RuntimeSetupInstall,
            RuntimeSetupCommand::OpenRuntimeSetupLogs { .. } => Capability::RuntimeSetupOpenLogs,
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
                | "cancel_runtime_install"
                | "open_runtime_setup_logs"
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
pub(crate) const HELPER_TOO_OLD_MESSAGE: &str =
    "The bundled Ato helper is too old to run Runtime Setup (missing the \
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
            "cancel_runtime_install",
            "open_runtime_setup_logs",
        ] {
            assert!(RuntimeSetupCommand::is_runtime_setup_kind(kind));
        }
        assert!(!RuntimeSetupCommand::is_runtime_setup_kind("complete"));
        assert!(!RuntimeSetupCommand::is_runtime_setup_kind("load_snapshot"));
    }

    #[test]
    fn install_and_logs_use_distinct_capabilities() {
        let install = RuntimeSetupCommand::InstallRuntimeTools {
            request_id: None,
            tools: vec!["node".into()],
        };
        let logs = RuntimeSetupCommand::OpenRuntimeSetupLogs { request_id: None };
        let status = RuntimeSetupCommand::RuntimeSetupStatus { request_id: None };
        assert_eq!(install.required_capability(), Capability::RuntimeSetupInstall);
        assert_eq!(logs.required_capability(), Capability::RuntimeSetupOpenLogs);
        assert_eq!(status.required_capability(), Capability::RuntimeSetupRead);
    }

    #[test]
    fn save_settings_defaults_missing_fields_to_enabled() {
        let cmd: RuntimeSetupCommand =
            serde_json::from_str(r#"{"kind":"save_runtime_setup_settings"}"#).unwrap();
        match cmd {
            RuntimeSetupCommand::SaveRuntimeSetupSettings {
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
    fn detects_old_helper_clap_error() {
        assert!(helper_lacks_runtime_subcommand(
            "error: unrecognized subcommand 'runtime'\n\nUsage: ato.exe internal"
        ));
        assert!(!helper_lacks_runtime_subcommand("network unreachable"));
    }
}
