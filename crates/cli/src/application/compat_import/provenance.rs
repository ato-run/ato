use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProvenanceKind {
    ManifestExplicit,
    LegacyLockResolved,
    NormalizedDefault,
    CompilerInferred,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(crate) struct CompilerOwnedField {
    pub section: &'static str,
    pub key: &'static str,
}

impl CompilerOwnedField {
    pub(crate) const fn new(section: &'static str, key: &'static str) -> Self {
        Self { section, key }
    }

    pub(crate) fn lock_path(&self) -> String {
        format!("{}.{}", self.section, self.key)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ProvenanceRecord {
    pub field: CompilerOwnedField,
    pub kind: ProvenanceKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl ProvenanceRecord {
    pub(crate) fn new(
        field: CompilerOwnedField,
        kind: ProvenanceKind,
        source_path: Option<&Path>,
        source_field: Option<&str>,
        note: Option<&str>,
    ) -> Self {
        Self {
            field,
            kind,
            source_path: source_path.map(Path::to_path_buf),
            source_field: source_field.map(str::to_string),
            note: note.map(str::to_string),
        }
    }
}

pub(crate) fn sort_provenance(records: &mut [ProvenanceRecord]) {
    records.sort_by(|left, right| {
        left.field
            .cmp(&right.field)
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.source_path.cmp(&right.source_path))
            .then_with(|| left.source_field.cmp(&right.source_field))
            .then_with(|| left.note.cmp(&right.note))
    });
}
