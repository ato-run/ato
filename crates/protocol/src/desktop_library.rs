//! Stable JSON DTOs for the Desktop installed-app library.
//!
//! The CLI produces these values; Desktop shells consume them. Filesystem and
//! SQLite details stay behind the CLI boundary.

use serde::{Deserialize, Serialize};

pub const DESKTOP_LIBRARY_SCHEMA_VERSION: &str = "1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopLibrarySnapshot {
    pub schema_version: String,
    #[serde(default)]
    pub apps: Vec<InstalledAppSummary>,
}

impl DesktopLibrarySnapshot {
    pub fn new(apps: Vec<InstalledAppSummary>) -> Self {
        Self {
            schema_version: DESKTOP_LIBRARY_SCHEMA_VERSION.to_string(),
            apps,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledAppSummary {
    pub installed_app_id: String,
    pub publisher: String,
    pub slug: String,
    pub capsule_handle: String,
    pub version: String,
    pub installed_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub profiles: Vec<InstalledProfileSummary>,
    #[serde(default)]
    pub running_sessions: Vec<InstalledSessionSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledProfileSummary {
    pub profile_id: String,
    pub install_profile_key: String,
    pub current_revision_id: Option<String>,
    pub current_output_dir: Option<String>,
    #[serde(default)]
    pub revisions: Vec<InstalledRevisionSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledRevisionSummary {
    pub revision_id: String,
    pub is_current: bool,
    pub is_pinned: bool,
    pub finalized_at: Option<String>,
    pub output_dir: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledSessionSummary {
    pub session_id: String,
    pub execution_id: Option<String>,
    pub capsule_instance_key: Option<String>,
    pub install_profile_key: Option<String>,
    pub install_revision_id: Option<String>,
    pub pid: Option<i32>,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopOperationKind {
    Install,
    Update,
    Rollback,
    Remove,
    Launch,
    Stop,
    Focus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopOperationStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopOperation {
    pub operation_id: String,
    pub kind: DesktopOperationKind,
    pub status: DesktopOperationStatus,
    pub install_profile_key: Option<String>,
    pub session_id: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledRemoveResult {
    pub schema_version: String,
    pub installed_app_id: String,
    pub profile_id: String,
    pub install_profile_key: String,
    pub state_purged: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_snapshot_has_stable_wire_shape() {
        let value = serde_json::to_value(DesktopLibrarySnapshot::new(Vec::new())).unwrap();
        assert_eq!(value, serde_json::json!({"schema_version":"1","apps":[]}));
    }

    #[test]
    fn operation_enums_are_snake_case() {
        let value = serde_json::to_value(DesktopOperation {
            operation_id: "op-1".into(),
            kind: DesktopOperationKind::Rollback,
            status: DesktopOperationStatus::Succeeded,
            install_profile_key: Some("ipk_1".into()),
            session_id: None,
            message: None,
        })
        .unwrap();
        assert_eq!(value["kind"], "rollback");
        assert_eq!(value["status"], "succeeded");
    }
}
