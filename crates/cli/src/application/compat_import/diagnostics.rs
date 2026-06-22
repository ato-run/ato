use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CompatibilityDiagnosticSeverity {
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CompatibilityDiagnosticCode {
    DraftNotExecutionUsable,
    PrimaryProcessUnresolved,
    LegacyRuntimeConflict,
    LegacyTargetConflict,
    MissingTargets,
    LegacyLockWithoutResolutionData,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CompatibilityDiagnostic {
    pub code: CompatibilityDiagnosticCode,
    pub severity: CompatibilityDiagnosticSeverity,
    pub lock_path: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<PathBuf>,
}

impl CompatibilityDiagnostic {
    pub(crate) fn new(
        code: CompatibilityDiagnosticCode,
        severity: CompatibilityDiagnosticSeverity,
        lock_path: impl Into<String>,
        message: impl Into<String>,
        source_path: Option<&Path>,
    ) -> Self {
        Self {
            code,
            severity,
            lock_path: lock_path.into(),
            message: message.into(),
            source_path: source_path.map(Path::to_path_buf),
        }
    }
}

pub(crate) fn sort_diagnostics(diagnostics: &mut [CompatibilityDiagnostic]) {
    diagnostics.sort_by(|left, right| {
        left.lock_path
            .cmp(&right.lock_path)
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| left.source_path.cmp(&right.source_path))
            .then_with(|| left.message.cmp(&right.message))
    });
}
