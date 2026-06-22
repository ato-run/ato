#![allow(dead_code)]

mod compiler;
mod diagnostics;
mod legacy_lock_import;
mod manifest_import;
mod provenance;

#[allow(unused_imports)]
pub(crate) use compiler::{
    CompatibilityCompileResult, CompatibilityCompilerInput, DraftCompleteness, DraftGuarantee,
    UnresolvedSummary, compile_compatibility_input, compile_compatibility_project,
};
#[allow(unused_imports)]
pub(crate) use diagnostics::{
    CompatibilityDiagnostic, CompatibilityDiagnosticCode, CompatibilityDiagnosticSeverity,
};
#[allow(unused_imports)]
pub(crate) use provenance::{CompilerOwnedField, ProvenanceKind, ProvenanceRecord};
