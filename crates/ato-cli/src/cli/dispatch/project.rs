use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{json, Value};

use crate::application::source_inference::{
    execute_shared_engine, MaterializationMode, SourceEvidenceInput, SourceInferenceInput,
    SourceInferenceResult,
};
use crate::application::workspace::init::{
    detect::detect_project,
    recipe::{generate_manifest, project_info_from_detection, ManifestMeta},
};
use crate::build::native_delivery;
use crate::cli::ProjectCommands;
use crate::reporters::CliReporter;

pub(super) fn execute_project_command(
    derived_app_path: Option<PathBuf>,
    launcher_dir: Option<PathBuf>,
    json_mode: bool,
    command: Option<ProjectCommands>,
) -> Result<()> {
    match command {
        Some(ProjectCommands::Ls { json }) => {
            let result = native_delivery::execute_project_ls()?;
            if json_mode || json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else if result.projections.is_empty() {
                println!("No experimental projections found.");
            } else {
                for projection in result.projections {
                    let marker = if projection.state == "ok" {
                        "✅"
                    } else {
                        "⚠️"
                    };
                    println!(
                        "{} [{}] {} -> {}",
                        marker,
                        projection.state,
                        projection.projected_path.display(),
                        projection.derived_app_path.display()
                    );
                    println!("   ID:       {}", projection.projection_id);
                    if !projection.problems.is_empty() {
                        println!("   Problems: {}", projection.problems.join(", "));
                    }
                }
            }
            Ok(())
        }
        Some(ProjectCommands::InferManifest { path, json }) => {
            execute_infer_manifest_command(path, json_mode || json)
        }
        None => {
            let derived_app_path = derived_app_path.ok_or_else(|| {
                anyhow::anyhow!(
                    "ato project requires <DERIVED_APP_PATH> or use `ato project ls` for read-only status"
                )
            })?;
            let result =
                native_delivery::execute_project(&derived_app_path, launcher_dir.as_deref())?;
            if json_mode {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("✅ Projected to: {}", result.projected_path.display());
                println!("   ID:       {}", result.projection_id);
                println!("   Target:   {}", result.derived_app_path.display());
                println!("   State:    {}", result.state);
                println!("   Metadata: {}", result.metadata_path.display());
            }
            Ok(())
        }
    }
}

pub(super) fn execute_unproject_command(projection_ref: String, json_mode: bool) -> Result<()> {
    let result = native_delivery::execute_unproject(&projection_ref)?;
    if json_mode {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("✅ Unprojected: {}", result.projected_path.display());
        println!("   ID:      {}", result.projection_id);
        println!("   State:   {}", result.state_before);
        println!(
            "   Removed: metadata={}, symlink={}",
            result.removed_metadata, result.removed_projected_path
        );
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct InferredManifestOutput {
    ok: bool,
    inference_mode: &'static str,
    manifest_toml: String,
    diagnostics: Vec<Value>,
    unresolved: Vec<String>,
    selection_gate: Option<Value>,
    approval_gate: Option<Value>,
}

fn execute_infer_manifest_command(path: PathBuf, json_mode: bool) -> Result<()> {
    let project_root = std::fs::canonicalize(&path)
        .with_context(|| format!("missing path: {}", path.display()))?;
    if !project_root.is_dir() {
        anyhow::bail!(
            "source path must be a directory: {}",
            project_root.display()
        );
    }

    let inferred = execute_shared_engine(
        SourceInferenceInput::SourceEvidence(SourceEvidenceInput {
            project_root: project_root.clone(),
            explicit_native_artifact: None,
            single_script_language: None,
            authoritative_root: None,
        }),
        MaterializationMode::InitWorkspace,
        true,
        Arc::new(CliReporter::new_run(json_mode)),
    );
    let output = match inferred {
        Ok(result) => inferred_manifest_output(&project_root, &result)?,
        Err(error) => {
            legacy_manifest_output(&project_root, &error.to_string()).with_context(|| {
                format!(
                    "static inference failed and legacy fallback could not inspect source: {error}"
                )
            })?
        }
    };
    if json_mode {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        print!("{}", output.manifest_toml);
    }
    Ok(())
}

fn inferred_manifest_output(
    project_root: &Path,
    result: &SourceInferenceResult,
) -> Result<InferredManifestOutput> {
    let unresolved = combined_unresolved(result);
    let manifest_blocking_unresolved = unresolved.iter().any(|field| {
        matches!(
            field.as_str(),
            "contract.process" | "resolution.runtime" | "resolution.resolved_targets"
        )
    });
    let ok = !manifest_blocking_unresolved
        && result.diagnostics.iter().all(|diagnostic| {
            !matches!(
                diagnostic.severity,
                crate::application::source_inference::SourceInferenceDiagnosticSeverity::Error
            )
        });
    let inference_mode = if ok {
        "static_inference"
    } else {
        "static_inference_unresolved"
    };

    Ok(InferredManifestOutput {
        ok,
        inference_mode,
        manifest_toml: manifest_toml_from_inference(project_root, result),
        diagnostics: result
            .diagnostics
            .iter()
            .map(serde_json::to_value)
            .collect::<std::result::Result<Vec<_>, _>>()?,
        unresolved,
        selection_gate: result
            .selection_gate
            .as_ref()
            .map(serde_json::to_value)
            .transpose()?,
        approval_gate: result
            .approval_gate
            .as_ref()
            .map(serde_json::to_value)
            .transpose()?,
    })
}

fn legacy_manifest_output(
    project_root: &Path,
    source_error: &str,
) -> Result<InferredManifestOutput> {
    let detected = detect_project(project_root)?;
    let info = project_info_from_detection(&detected)?;
    let manifest_toml = generate_manifest(
        &info,
        ManifestMeta {
            generated_by: "ato project infer-manifest legacy fallback",
            description: "Generated from legacy source detection after static inference failed.",
        },
    );
    Ok(InferredManifestOutput {
        ok: false,
        inference_mode: "legacy_fallback",
        manifest_toml,
        diagnostics: vec![json!({
            "severity": "warning",
            "field": "source_inference",
            "message": format!("static inference failed; used legacy manifest detection fallback: {source_error}"),
        })],
        unresolved: Vec::new(),
        selection_gate: None,
        approval_gate: None,
    })
}

fn manifest_toml_from_inference(project_root: &Path, result: &SourceInferenceResult) -> String {
    let metadata = result.lock.contract.entries.get("metadata");
    let name = inferred_manifest_name(project_root, metadata);
    let version = metadata
        .and_then(|value| value.get("version"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("0.1.0");
    let capsule_type = metadata
        .and_then(|value| value.get("capsule_type"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("app");

    let process = result.lock.contract.entries.get("process");
    let target = selected_resolved_target(result);
    let runtime = string_field(process, "runtime")
        .or_else(|| string_field(target, "runtime"))
        .or_else(|| {
            result
                .lock
                .resolution
                .entries
                .get("runtime")
                .and_then(|value| value.get("kind"))
                .and_then(Value::as_str)
        })
        .unwrap_or("source");
    let driver = string_field(process, "driver")
        .or_else(|| string_field(target, "driver"))
        .or_else(|| {
            result
                .lock
                .resolution
                .entries
                .get("runtime")
                .and_then(|value| value.get("kind"))
                .and_then(Value::as_str)
                .filter(|value| !matches!(*value, "source" | "web" | "wasm" | "oci"))
        });
    let runtime_version = string_field(target, "runtime_version").or_else(|| {
        result
            .lock
            .resolution
            .entries
            .get("runtime")
            .and_then(|value| value.get("version"))
            .and_then(Value::as_str)
    });
    let entrypoint =
        string_field(process, "entrypoint").or_else(|| string_field(target, "entrypoint"));
    let run_command =
        string_field(process, "run_command").or_else(|| string_field(target, "run_command"));
    let cmd = string_array_field(process, "cmd")
        .or_else(|| string_array_field(target, "cmd"))
        .or_else(|| string_array_field(process, "args"))
        .unwrap_or_default();
    let port = target
        .and_then(|value| value.get("port"))
        .and_then(Value::as_u64)
        .filter(|port| *port <= u16::MAX as u64);
    let working_dir = string_field(process, "working_dir")
        .or_else(|| string_field(target, "working_dir"))
        .unwrap_or(".");

    let mut lines = vec![
        "# Capsule Manifest - Static inference draft".to_string(),
        "# Generated by: ato project infer-manifest".to_string(),
        String::new(),
        "schema_version = \"0.3\"".to_string(),
        format!("name = {}", toml_string(&name)),
        format!("version = {}", toml_string(version)),
        format!("type = {}", toml_string(capsule_type)),
        String::new(),
        format!("runtime = {}", toml_string(runtime)),
    ];

    if let Some(driver) = driver {
        lines.push(format!("driver = {}", toml_string(driver)));
    }
    if let Some(runtime_version) = runtime_version {
        lines.push(format!(
            "runtime_version = {}",
            toml_string(runtime_version)
        ));
    }
    if let Some(run_command) = run_command {
        lines.push(format!("run = {}", toml_string(run_command)));
    } else if let Some(entrypoint) = entrypoint {
        lines.push(format!("run = {}", toml_string(entrypoint)));
        if !cmd.is_empty() {
            lines.push(format!("cmd = [{}]", toml_string_array(&cmd)));
        }
    }
    if let Some(port) = port {
        lines.push(format!("port = {port}"));
    }
    lines.push(format!("working_dir = {}", toml_string(working_dir)));
    lines.push(String::new());
    lines.push("[metadata]".to_string());
    lines.push(format!(
        "description = {}",
        toml_string("Generated from static source inference.")
    ));
    lines.push(String::new());

    lines.join("\n")
}

fn selected_resolved_target(result: &SourceInferenceResult) -> Option<&Value> {
    let targets = result
        .lock
        .resolution
        .entries
        .get("resolved_targets")
        .and_then(Value::as_array)?;
    let default_target = result
        .lock
        .resolution
        .entries
        .get("target_selection")
        .and_then(|value| value.get("default_target"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(default_target) = default_target {
        if let Some(target) = targets.iter().find(|target| {
            target
                .get("label")
                .and_then(Value::as_str)
                .map(|label| label == default_target)
                .unwrap_or(false)
        }) {
            return Some(target);
        }
    }
    if targets.len() == 1 {
        targets.first()
    } else {
        None
    }
}

fn combined_unresolved(result: &SourceInferenceResult) -> Vec<String> {
    let mut unresolved = result
        .infer
        .unresolved
        .iter()
        .chain(result.resolve.unresolved.iter())
        .cloned()
        .collect::<Vec<_>>();
    unresolved.sort();
    unresolved.dedup();
    unresolved
}

fn string_field<'a>(value: Option<&'a Value>, key: &str) -> Option<&'a str> {
    value?
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn string_array_field(value: Option<&Value>, key: &str) -> Option<Vec<String>> {
    let array = value?.get(key)?.as_array()?;
    let strings = array
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect::<Vec<_>>();
    if strings.is_empty() {
        None
    } else {
        Some(strings)
    }
}

fn fallback_project_name(project_root: &Path) -> String {
    project_root
        .file_name()
        .and_then(|name| name.to_str())
        .map(slugify)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "new-capsule".to_string())
}

fn inferred_manifest_name(project_root: &Path, metadata: Option<&Value>) -> String {
    infer_package_json_string(project_root, "name")
        .or_else(|| infer_pyproject_string(project_root, "name"))
        .or_else(|| infer_cargo_package_string(project_root, "name"))
        .or_else(|| {
            metadata
                .and_then(|value| value.get("name"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| fallback_project_name(project_root))
}

fn infer_package_json_string(project_root: &Path, field: &str) -> Option<String> {
    let raw = std::fs::read_to_string(project_root.join("package.json")).ok()?;
    let value = serde_json::from_str::<Value>(&raw).ok()?;
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn infer_pyproject_string(project_root: &Path, field: &str) -> Option<String> {
    let raw = std::fs::read_to_string(project_root.join("pyproject.toml")).ok()?;
    let value = toml::from_str::<toml::Value>(&raw).ok()?;
    value
        .get("project")
        .and_then(|value| value.get(field))
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn infer_cargo_package_string(project_root: &Path, field: &str) -> Option<String> {
    let raw = std::fs::read_to_string(project_root.join("Cargo.toml")).ok()?;
    let value = toml::from_str::<toml::Value>(&raw).ok()?;
    value
        .get("package")
        .and_then(|value| value.get(field))
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn slugify(input: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in input.trim().to_ascii_lowercase().chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            out.push(ch);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let slug = out.trim_matches('-').to_string();
    if slug.is_empty() {
        "new-capsule".to_string()
    } else {
        slug
    }
}

fn toml_string(value: &str) -> String {
    toml::Value::String(value.to_string()).to_string()
}

fn toml_string_array(values: &[String]) -> String {
    values
        .iter()
        .map(|value| toml_string(value))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule_core::ato_lock::AtoLock;
    use serde_json::json;

    #[test]
    fn manifest_projection_prefers_run_command_from_static_inference() {
        let mut lock = AtoLock::default();
        lock.contract.entries.insert(
            "metadata".to_string(),
            json!({"name": "demo-node", "version": "1.2.3", "capsule_type": "app"}),
        );
        lock.contract.entries.insert(
            "process".to_string(),
            json!({"entrypoint": "npm", "cmd": ["start"], "run_command": "npm start"}),
        );
        lock.resolution.entries.insert(
            "runtime".to_string(),
            json!({"kind": "node", "version": "20"}),
        );
        lock.resolution.entries.insert(
            "resolved_targets".to_string(),
            json!([{"label": "default", "runtime": "source", "driver": "node", "compatible": true}]),
        );
        let result = SourceInferenceResult {
            input_kind:
                crate::application::source_inference::SourceInferenceInputKind::SourceEvidence,
            lock,
            provenance: Vec::new(),
            diagnostics: Vec::new(),
            infer: crate::application::source_inference::InferResult {
                candidate_sets: Vec::new(),
                unresolved: Vec::new(),
            },
            resolve: crate::application::source_inference::ResolveResult {
                resolved_process: true,
                resolved_runtime: true,
                resolved_target_compatibility: true,
                resolved_dependency_closure: true,
                unresolved: Vec::new(),
            },
            selection_gate: None,
            approval_gate: None,
        };

        let manifest = manifest_toml_from_inference(Path::new("/workspace/demo-node"), &result);

        assert!(manifest.contains("name = \"demo-node\""));
        assert!(manifest.contains("version = \"1.2.3\""));
        assert!(manifest.contains("driver = \"node\""));
        assert!(manifest.contains("runtime_version = \"20\""));
        assert!(manifest.contains("run = \"npm start\""));
    }

    #[test]
    fn manifest_projection_uses_run_field_for_entrypoint_when_no_run_command() {
        let mut lock = AtoLock::default();
        lock.contract.entries.insert(
            "metadata".to_string(),
            json!({"name": "py-app", "version": "0.1.0", "capsule_type": "app"}),
        );
        lock.contract.entries.insert(
            "process".to_string(),
            json!({"entrypoint": "main.py", "cmd": ["--port", "8080"]}),
        );
        lock.resolution.entries.insert(
            "resolved_targets".to_string(),
            json!([{"label": "default", "runtime": "source", "driver": "python", "compatible": true}]),
        );
        let result = SourceInferenceResult {
            input_kind:
                crate::application::source_inference::SourceInferenceInputKind::SourceEvidence,
            lock,
            provenance: Vec::new(),
            diagnostics: Vec::new(),
            infer: crate::application::source_inference::InferResult {
                candidate_sets: Vec::new(),
                unresolved: Vec::new(),
            },
            resolve: crate::application::source_inference::ResolveResult {
                resolved_process: true,
                resolved_runtime: true,
                resolved_target_compatibility: true,
                resolved_dependency_closure: true,
                unresolved: Vec::new(),
            },
            selection_gate: None,
            approval_gate: None,
        };

        let manifest = manifest_toml_from_inference(Path::new("/workspace/py-app"), &result);

        assert!(manifest.contains("driver = \"python\""));
        assert!(manifest.contains("run = \"main.py\""));
        assert!(manifest.contains("cmd = [\"--port\", \"8080\"]"));
    }
}
