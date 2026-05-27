//! Shared Podman machine list parser.
//!
//! This module owns the rich representation of Podman machine state
//! (`PodmanMachineStatus`) and the JSON parser for `podman machine list --format json`.
//!
//! Both `oci_provider` and `oci_session_store` use these types.  Neither depends on the other,
//! avoiding a layering inversion.

use serde::Deserialize;

/// Rich Podman machine state, preserving machine names for diagnostic display
/// and for deciding which machine to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PodmanMachineStatus {
    /// At least one machine is running.  Only running machine names are listed.
    Running { names: Vec<String> },
    /// One or more machines are configured but none are running.
    Stopped { names: Vec<String> },
    /// No machines are configured (`podman machine list` returned an empty array).
    NotConfigured,
    /// `podman machine list` could not be executed (binary missing, permissions,
    /// etc.).
    Unavailable { reason: String },
    /// `podman machine list` ran but the output could not be parsed.
    Unknown { reason: String },
}

impl PodmanMachineStatus {
    pub fn display_status(&self) -> String {
        match self {
            Self::Running { names } => format!("running{}", display_machine_names(names)),
            Self::Stopped { names } => format!("stopped{}", display_machine_names(names)),
            Self::NotConfigured => "not configured".to_string(),
            Self::Unavailable { reason } => format!("unavailable ({reason})"),
            Self::Unknown { reason } => format!("unknown ({reason})"),
        }
    }

    pub fn status_label(&self) -> &'static str {
        match self {
            Self::Running { .. } => "running",
            Self::Stopped { .. } => "stopped",
            Self::NotConfigured => "not_configured",
            Self::Unavailable { .. } => "unavailable",
            Self::Unknown { .. } => "unknown",
        }
    }

    pub fn machine_names(&self) -> &[String] {
        match self {
            Self::Running { names } | Self::Stopped { names } => names,
            Self::NotConfigured | Self::Unavailable { .. } | Self::Unknown { .. } => &[],
        }
    }

    pub fn is_visible(&self) -> bool {
        !matches!(self, Self::Unavailable { .. } | Self::NotConfigured)
    }
}

/// One entry from `podman machine list --format json`.
#[derive(Debug, Deserialize)]
pub(crate) struct PodmanMachineListEntry {
    #[serde(rename = "Name", default)]
    pub(crate) name: String,
    #[serde(rename = "Running", default)]
    pub(crate) running: bool,
}

/// Parse `podman machine list --format json` stdout into a `PodmanMachineStatus`.
pub(crate) fn parse_podman_machine_list(stdout: &str) -> PodmanMachineStatus {
    let entries: Vec<PodmanMachineListEntry> = match serde_json::from_str(stdout) {
        Ok(entries) => entries,
        Err(err) => {
            return PodmanMachineStatus::Unknown {
                reason: format!("podman machine list output was not recognized: {err}"),
            };
        }
    };
    if entries.is_empty() {
        return PodmanMachineStatus::NotConfigured;
    }

    let running_names: Vec<String> = entries
        .iter()
        .filter(|entry| entry.running)
        .map(machine_display_name)
        .collect();
    if !running_names.is_empty() {
        return PodmanMachineStatus::Running {
            names: running_names,
        };
    }

    PodmanMachineStatus::Stopped {
        names: entries.iter().map(machine_display_name).collect(),
    }
}

pub(crate) fn machine_display_name(entry: &PodmanMachineListEntry) -> String {
    if entry.name.is_empty() {
        "<unnamed>".to_string()
    } else {
        entry.name.clone()
    }
}

pub(crate) fn display_machine_names(names: &[String]) -> String {
    if names.is_empty() {
        String::new()
    } else {
        format!(" ({})", names.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_array_gives_not_configured() {
        let status = parse_podman_machine_list("[]");
        assert_eq!(status, PodmanMachineStatus::NotConfigured);
    }

    #[test]
    fn parse_single_running_machine() {
        let json = r#"[{"Name":"podman-machine-default","Running":true}]"#;
        let status = parse_podman_machine_list(json);
        assert_eq!(
            status,
            PodmanMachineStatus::Running {
                names: vec!["podman-machine-default".to_string()]
            }
        );
    }

    #[test]
    fn parse_single_stopped_machine() {
        let json = r#"[{"Name":"podman-machine-default","Running":false}]"#;
        let status = parse_podman_machine_list(json);
        assert_eq!(
            status,
            PodmanMachineStatus::Stopped {
                names: vec!["podman-machine-default".to_string()]
            }
        );
    }

    #[test]
    fn parse_multiple_machines_one_running() {
        let json = r#"[{"Name":"a","Running":false},{"Name":"b","Running":true}]"#;
        let status = parse_podman_machine_list(json);
        // Only running names appear; stopped ones are omitted from the Running variant.
        assert_eq!(
            status,
            PodmanMachineStatus::Running {
                names: vec!["b".to_string()]
            }
        );
    }

    #[test]
    fn parse_multiple_machines_all_stopped() {
        let json = r#"[{"Name":"a","Running":false},{"Name":"b","Running":false}]"#;
        let status = parse_podman_machine_list(json);
        assert_eq!(
            status,
            PodmanMachineStatus::Stopped {
                names: vec!["a".to_string(), "b".to_string()]
            }
        );
    }

    #[test]
    fn parse_invalid_json_gives_unknown() {
        let status = parse_podman_machine_list("not-json");
        assert!(matches!(status, PodmanMachineStatus::Unknown { .. }));
    }

    #[test]
    fn unnamed_machine_gets_placeholder() {
        let json = r#"[{"Name":"","Running":false}]"#;
        let status = parse_podman_machine_list(json);
        assert_eq!(
            status,
            PodmanMachineStatus::Stopped {
                names: vec!["<unnamed>".to_string()]
            }
        );
    }
}
