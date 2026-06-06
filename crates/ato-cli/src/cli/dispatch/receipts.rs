//! `ato receipts diff <old> <new>` — component-level execution-receipt drift.
//!
//! Loads two receipt JSON files, runs the pure drift differ in `capsule-core`
//! (`execution_identity::diff_receipt_documents`), and prints either a
//! human-readable, class-grouped summary or `--json` machine output. The diff
//! reports which nodes / edges / facet fields changed and classifies each as
//! `DeclaredDrift` or `ResolvedDrift`. It performs no runtime observation —
//! `ObservedDrift` is reserved and never emitted (#496).

use std::path::PathBuf;

use anyhow::Result;
use capsule_core::execution_identity::{
    DriftClass, ReceiptDriftChange, ReceiptDriftReport, diff_receipt_documents,
};
use serde_json::Value;

use crate::application::execution_receipts;
use crate::cli::receipts::ReceiptsCommands;

pub(crate) fn execute_receipts_command(command: ReceiptsCommands, json: bool) -> Result<()> {
    match command {
        ReceiptsCommands::Diff {
            old,
            new,
            json: command_json,
        } => execute_receipts_diff_command(old, new, json || command_json),
    }
}

fn execute_receipts_diff_command(old: PathBuf, new: PathBuf, json: bool) -> Result<()> {
    // The loader's own context messages already name the offending file and the
    // failure mode (read / parse / unsupported schema), so they are propagated
    // verbatim rather than wrapped in a generic "failed to load" layer that the
    // CLI error reporter would show in their place.
    let old_document = execution_receipts::read_receipt_document_from_file(&old)?;
    let new_document = execution_receipts::read_receipt_document_from_file(&new)?;

    let report = diff_receipt_documents(&old_document, &new_document).map_err(|err| {
        anyhow::anyhow!(
            "cannot diff receipts {} and {}: {err}",
            old.display(),
            new.display()
        )
    })?;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human_report(&report);
    }
    Ok(())
}

fn print_human_report(report: &ReceiptDriftReport) {
    if !report.has_drift {
        println!("No receipt drift detected");
        return;
    }

    println!("Drift detected");

    if report.old_execution_id != report.new_execution_id {
        println!();
        println!("execution_id changed:");
        println!("  old: {}", optional_id(&report.old_execution_id));
        println!("  new: {}", optional_id(&report.new_execution_id));
    }

    // Group by class so DeclaredDrift and ResolvedDrift are visibly distinct.
    // ObservedDrift is reserved and never present in v1, but iterate the full
    // set so any future class still renders rather than being silently dropped.
    for class in [
        DriftClass::DeclaredDrift,
        DriftClass::ResolvedDrift,
        DriftClass::ObservedDrift,
    ] {
        let group: Vec<&ReceiptDriftChange> =
            report.changes.iter().filter(|c| c.class == class).collect();
        if group.is_empty() {
            continue;
        }
        println!();
        println!("{}:", class_heading(class));
        for change in group {
            println!(
                "  [{}] {} {}",
                change.component_kind, change.component_id, change.field
            );
            println!("    reason: {}", change.reason);
            println!("    old: {}", render_value(change.old.as_ref()));
            println!("    new: {}", render_value(change.new.as_ref()));
        }
    }
}

fn class_heading(class: DriftClass) -> &'static str {
    match class {
        DriftClass::DeclaredDrift => "DeclaredDrift",
        DriftClass::ResolvedDrift => "ResolvedDrift",
        DriftClass::ObservedDrift => "ObservedDrift",
    }
}

fn optional_id(id: &Option<String>) -> String {
    id.clone().unwrap_or_else(|| "(absent)".to_string())
}

/// Render a change value for humans: bare string for `Value::String` (so hashes
/// and refs read cleanly), compact JSON for arrays/objects, `(absent)` for a
/// missing side (an added/removed component).
fn render_value(value: Option<&Value>) -> String {
    match value {
        None => "(absent)".to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => serde_json::to_string(other).unwrap_or_else(|_| other.to_string()),
    }
}
