//! Desktop machine-interface DTOs for inspecting the active Run of a Capsule
//! project. These are the wire types the Tauri shell uses to ask the `ato` CLI
//! for the current Run state and its presentation surfaces.
//!
//! The shape is deliberately presentation-oriented and CLI-owned: the CLI is
//! the only process that may derive a surface from Runtime state, so these
//! types live next to `ComputationCommand` rather than in the Semantic Core.

use serde::{Deserialize, Serialize};

/// Request the CLI to inspect the active Run of a Capsule project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopRunInspectRequest {
    pub project: String,
}

/// Status of a Run as surfaced to the desktop shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopRunStatus {
    Starting,
    Active,
    Stopping,
    Inactive,
    Failed,
}

/// One presentation surface the desktop shell may open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DesktopSurfaceView {
    Web {
        /// Absolute loopback HTTP origin returned by the active Run.
        url: String,
        profile: String,
    },
    Terminal {
        profile: String,
    },
}

/// Inspect result: the active Run (or `Inactive`) and its presentation
/// surfaces. When `status` is `Inactive`, `branch`, `head`, and `surfaces` are
/// empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopRunView {
    pub project: String,
    pub branch: String,
    pub head: String,
    pub status: DesktopRunStatus,
    pub surfaces: Vec<DesktopSurfaceView>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_view_round_trips() {
        let view = DesktopRunView {
            project: "/tmp/demo".to_owned(),
            branch: "main".to_owned(),
            head: "blake3:aaaa".to_owned(),
            status: DesktopRunStatus::Active,
            surfaces: vec![DesktopSurfaceView::Web {
                url: "http://127.0.0.1:8000".to_owned(),
                profile: "ato.web-surface.v1".to_owned(),
            }],
        };
        let json = serde_json::to_string(&view).unwrap();
        let back: DesktopRunView = serde_json::from_str(&json).unwrap();
        assert_eq!(view, back);
    }

    #[test]
    fn inactive_view_round_trips() {
        let view = DesktopRunView {
            project: "demo".to_owned(),
            branch: String::new(),
            head: String::new(),
            status: DesktopRunStatus::Inactive,
            surfaces: Vec::new(),
        };
        let json = serde_json::to_string(&view).unwrap();
        let back: DesktopRunView = serde_json::from_str(&json).unwrap();
        assert_eq!(view, back);
        assert_eq!(back.status, DesktopRunStatus::Inactive);
    }

    #[test]
    fn status_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&DesktopRunStatus::Active).unwrap(),
            "\"active\""
        );
        assert_eq!(
            serde_json::to_string(&DesktopRunStatus::Inactive).unwrap(),
            "\"inactive\""
        );
    }

    #[test]
    fn surface_kind_is_a_tagged_union() {
        let surface = DesktopSurfaceView::Web {
            url: "http://127.0.0.1:8000".to_owned(),
            profile: "ato.web-surface.v1".to_owned(),
        };
        let json = serde_json::to_string(&surface).unwrap();
        assert!(json.contains("\"kind\":\"web\""));
        let back: DesktopSurfaceView = serde_json::from_str(&json).unwrap();
        assert_eq!(surface, back);
    }

    #[test]
    fn unknown_fields_are_rejected() {
        assert!(serde_json::from_str::<DesktopRunView>(
            r#"{"project":"p","branch":"b","head":"h","status":"active","surfaces":[],"bogus":1}"#
        )
        .is_err());
        assert!(
            serde_json::from_str::<DesktopRunInspectRequest>(r#"{"project":"p","bogus":1}"#)
                .is_err()
        );
        assert!(
            serde_json::from_str::<DesktopSurfaceView>(
                r#"{"kind":"web","url":"http://127.0.0.1:1","profile":"p","bogus":1}"#
            )
            .is_err()
        );
    }
}
