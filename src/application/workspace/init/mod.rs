//! Project init helpers for lock-first durable materialization and legacy manifest scaffolds.

use anyhow::{Context, Result};
use capsule_core::input_resolver::{
    resolve_authoritative_input, ResolveInputOptions, ResolvedCompatibilityProject, ResolvedInput,
    ResolvedSourceOnly,
};
use std::fs;
use std::path::{Path, PathBuf};

use capsule_core::CapsuleReporter;

use crate::application::source_inference;

pub mod detect;
pub mod materialize;
pub mod prompt;
pub mod recipe;

pub struct InitArgs {
    pub path: Option<PathBuf>,
    pub yes: bool,
}

pub struct PromptArgs {
    pub path: Option<PathBuf>,
}

pub fn execute_prompt(
    args: PromptArgs,
    reporter: std::sync::Arc<crate::reporters::CliReporter>,
) -> Result<()> {
    prompt::execute(args, reporter)
}

pub fn execute_durable_init(
    args: InitArgs,
    reporter: std::sync::Arc<crate::reporters::CliReporter>,
) -> Result<()> {
    let input_path = args
        .path
        .unwrap_or_else(|| PathBuf::from("."))
        .canonicalize()
        .context("Failed to resolve project directory")?;

    let resolved = match resolve_authoritative_input(&input_path, ResolveInputOptions::default()) {
        Ok(ResolvedInput::CanonicalLock { canonical, .. }) => {
            anyhow::bail!(
                "{} already exists at {}. `ato init` only materializes a durable baseline when canonical input is missing.",
                capsule_core::input_resolver::ATO_LOCK_FILE_NAME,
                canonical.path.display()
            );
        }
        Ok(resolved) => resolved,
        Err(error)
            if error
                .to_string()
                .contains("is not an authoritative command-entry input") =>
        {
            return Err(error.into());
        }
        Err(_) => ResolvedInput::SourceOnly {
            source: ResolvedSourceOnly {
                project_root: input_path.clone(),
                single_script: None,
            },
            provenance: capsule_core::input_resolver::InputProvenance {
                requested_path: input_path.clone(),
                explicit_input_kind: capsule_core::input_resolver::ExplicitInputKind::Directory,
                project_root: input_path.clone(),
                discovered: capsule_core::input_resolver::DiscoveredArtifacts {
                    canonical_lock_path: None,
                    compatibility_manifest_path: None,
                    compatibility_lock_path: None,
                },
                selected_kind: capsule_core::input_resolver::ResolvedInputKind::SourceOnly,
                authoritative_path: None,
            },
            advisories: Vec::new(),
        },
    };

    let (workspace_root, compatibility_target, source_target): (
        PathBuf,
        Option<ResolvedCompatibilityProject>,
        Option<ResolvedSourceOnly>,
    ) = match resolved {
        ResolvedInput::CompatibilityProject { project, .. } => {
            (project.project_root.clone(), Some(project), None)
        }
        ResolvedInput::SourceOnly { source, .. } => {
            (source.project_root.clone(), None, Some(source))
        }
        ResolvedInput::CanonicalLock { .. } => unreachable!("canonical input handled above"),
    };

    futures::executor::block_on(reporter.notify(format!(
        "🔍 Initializing capsule in: {}\n",
        workspace_root.display()
    )))?;

    let materialized = if let Some(project) = compatibility_target.as_ref() {
        source_inference::execute_init_from_compatibility(project, reporter.clone(), args.yes)?
    } else {
        source_inference::execute_init_from_resolved_source_only(
            source_target
                .as_ref()
                .expect("source-only target must exist"),
            reporter.clone(),
            args.yes,
        )?
    };

    if workspace_root.join(".git").exists() {
        add_to_gitignore(&workspace_root, reporter.clone())?;
    }

    futures::executor::block_on(reporter.notify(format!(
        "\n✨ Created {}!",
        materialized.lock_path.display()
    )))?;
    futures::executor::block_on(reporter.notify(format!(
        "   Source inference provenance: {}",
        materialized.sidecar_path.display()
    )))?;
    futures::executor::block_on(reporter.notify(format!(
        "   Provenance cache: {}",
        materialized.provenance_cache_path.display()
    )))?;
    futures::executor::block_on(reporter.notify(format!(
        "   Workspace binding seed: {}",
        materialized.binding_seed_path.display()
    )))?;
    futures::executor::block_on(reporter.notify(format!(
        "   Workspace policy bundle: {}",
        materialized.policy_bundle_path.display()
    )))?;
    futures::executor::block_on(reporter.notify(format!(
        "   Workspace attestation store: {}",
        materialized.attestation_store_path.display()
    )))?;
    futures::executor::block_on(reporter.notify("\nNext steps:".to_string()))?;
    futures::executor::block_on(
        reporter
            .notify("   ato run .         # Run from the inferred canonical baseline".to_string()),
    )?;
    futures::executor::block_on(
        reporter
            .notify("   ato inspect lock . # Inspect unresolved fields and provenance".to_string()),
    )?;

    Ok(())
}

pub fn write_manual_manifest_stub(
    path: Option<PathBuf>,
    reporter: std::sync::Arc<crate::reporters::CliReporter>,
) -> Result<PathBuf> {
    let project_dir = path
        .unwrap_or_else(|| PathBuf::from("."))
        .canonicalize()
        .context("Failed to resolve project directory")?;

    match resolve_authoritative_input(&project_dir, ResolveInputOptions::default()) {
        Ok(ResolvedInput::CanonicalLock { canonical, .. }) => {
            anyhow::bail!(
                "{} already exists at {}. `ato init` manual starter generation is not available on top of canonical lock input.",
                capsule_core::input_resolver::ATO_LOCK_FILE_NAME,
                canonical.path.display()
            );
        }
        Ok(ResolvedInput::CompatibilityProject { project, .. }) => {
            anyhow::bail!(
                "capsule.toml already exists at {}. Delete or move the file before creating a manual starter manifest.",
                project.manifest.path.display()
            );
        }
        Ok(ResolvedInput::SourceOnly { .. }) => {}
        Err(error)
            if error
                .to_string()
                .contains("is not an authoritative command-entry input") =>
        {
            return Err(error.into());
        }
        Err(_) => {}
    }

    let manifest_path = project_dir.join("capsule.toml");

    let detected = detect::detect_project(&project_dir)?;
    let manifest_content = recipe::generate_manual_manifest_stub(&detected.name);
    fs::write(&manifest_path, manifest_content).context("Failed to write capsule.toml")?;

    futures::executor::block_on(reporter.notify(format!(
        "📝 Created a manual starter capsule.toml at {}.\nEdit it, then rerun `ato run`.",
        manifest_path.display()
    )))?;

    Ok(manifest_path)
}

pub fn write_legacy_detected_manifest(
    path: Option<PathBuf>,
    reporter: std::sync::Arc<crate::reporters::CliReporter>,
) -> Result<PathBuf> {
    let project_dir = path
        .unwrap_or_else(|| PathBuf::from("."))
        .canonicalize()
        .context("Failed to resolve project directory")?;

    match resolve_authoritative_input(&project_dir, ResolveInputOptions::default()) {
        Ok(ResolvedInput::CanonicalLock { canonical, .. }) => {
            anyhow::bail!(
                "{} already exists at {}. Legacy manifest generation is not available on top of canonical lock input.",
                capsule_core::input_resolver::ATO_LOCK_FILE_NAME,
                canonical.path.display()
            );
        }
        Ok(ResolvedInput::CompatibilityProject { project, .. }) => {
            anyhow::bail!(
                "capsule.toml already exists at {}. Delete or move the file before generating a legacy compatibility manifest.",
                project.manifest.path.display()
            );
        }
        Ok(ResolvedInput::SourceOnly { .. }) => {}
        Err(error)
            if error
                .to_string()
                .contains("is not an authoritative command-entry input") =>
        {
            return Err(error.into());
        }
        Err(_) => {}
    }

    let manifest_path = project_dir.join("capsule.toml");
    let detected = detect::detect_project(&project_dir)?;
    let info = recipe::project_info_from_detection(&detected)?;
    let manifest_content = recipe::generate_manifest(
        &info,
        recipe::ManifestMeta {
            generated_by: "ato init --legacy prompt",
            description: "Legacy compatibility manifest inferred from local source detection",
        },
    );
    fs::write(&manifest_path, manifest_content).context("Failed to write capsule.toml")?;
    maybe_create_capsuleignore(&project_dir, &info, reporter.clone())?;

    futures::executor::block_on(reporter.notify(format!(
        "📝 Created an inferred compatibility capsule.toml at {}.",
        manifest_path.display()
    )))?;

    Ok(manifest_path)
}

fn add_to_gitignore(
    dir: &Path,
    reporter: std::sync::Arc<crate::reporters::CliReporter>,
) -> Result<()> {
    let gitignore_path = dir.join(".gitignore");

    let existing = if gitignore_path.exists() {
        fs::read_to_string(&gitignore_path).unwrap_or_default()
    } else {
        String::new()
    };

    if existing.contains(".ato/")
        && existing.contains(".capsule/")
        && existing.contains("*.capsule")
    {
        return Ok(());
    }

    let addition = "\n# Ato\n.ato/\n\n# Capsule\n.capsule/\n*.capsule\n*.sig\n";
    let new_content = format!("{}{}", existing.trim_end(), addition);

    fs::write(&gitignore_path, new_content).context("Failed to update .gitignore")?;
    futures::executor::block_on(reporter.notify("   ✓ Updated .gitignore".to_string()))?;
    Ok(())
}

fn maybe_create_capsuleignore(
    dir: &Path,
    info: &recipe::ProjectInfo,
    reporter: std::sync::Arc<crate::reporters::CliReporter>,
) -> Result<()> {
    let capsuleignore_path = dir.join(".capsuleignore");
    if capsuleignore_path.exists() {
        return Ok(());
    }

    match info.project_type {
        detect::ProjectType::NodeJs => {
            if !dir.join("node_modules").exists() {
                return Ok(());
            }

            fs::write(&capsuleignore_path, "node_modules/\n")
                .context("Failed to write .capsuleignore")?;
            futures::executor::block_on(
                reporter.notify("   ✓ Created .capsuleignore (excludes node_modules/)".to_string()),
            )?;
        }
        detect::ProjectType::Rust => {
            if !dir.join("target").exists() {
                return Ok(());
            }

            fs::write(&capsuleignore_path, "target/\n")
                .context("Failed to write .capsuleignore")?;
            futures::executor::block_on(
                reporter.notify("   ✓ Created .capsuleignore (excludes target/)".to_string()),
            )?;
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reporters::CliReporter;
    use std::sync::Arc;

    #[test]
    fn durable_init_accepts_single_typescript_script_path() {
        if std::process::Command::new("deno")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let script_path = dir.path().join("scratch.ts");
        std::fs::write(&script_path, "console.log('hello durable init');\n").expect("write script");

        execute_durable_init(
            InitArgs {
                path: Some(script_path),
                yes: true,
            },
            Arc::new(CliReporter::new(true)),
        )
        .expect("durable init");

        assert!(dir.path().join("ato.lock.json").exists());
        assert!(dir.path().join("main.ts").exists());
        assert!(dir.path().join("deno.json").exists());
    }

    #[test]
    fn durable_init_accepts_single_python_script_path() {
        if std::process::Command::new("uv")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let script_path = dir.path().join("scratch.py");
        std::fs::write(
            &script_path,
            "# /// script\n# requires-python = \">=3.11\"\n# ///\nprint('hello durable init')\n",
        )
        .expect("write script");

        execute_durable_init(
            InitArgs {
                path: Some(script_path),
                yes: true,
            },
            Arc::new(CliReporter::new(true)),
        )
        .expect("durable init");

        assert!(dir.path().join("ato.lock.json").exists());
        assert!(dir.path().join("main.py").exists());
        assert!(dir.path().join("pyproject.toml").exists());
        assert!(dir.path().join("uv.lock").exists());
    }
}
