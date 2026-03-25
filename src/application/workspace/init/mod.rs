//! Project init helpers for lock-first durable materialization and legacy manifest scaffolds.

use anyhow::{Context, Result};
use capsule_core::input_resolver::{
    resolve_authoritative_input, ResolveInputOptions, ResolvedInput,
};
use std::fs;
use std::io::{self, Write};
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
    let project_dir = args
        .path
        .unwrap_or_else(|| PathBuf::from("."))
        .canonicalize()
        .context("Failed to resolve project directory")?;

    let init_target = match resolve_authoritative_input(
        &project_dir,
        ResolveInputOptions::default(),
    ) {
        Ok(ResolvedInput::CanonicalLock { canonical, .. }) => {
            anyhow::bail!(
                "{} already exists at {}. `ato init` only materializes a durable baseline when canonical input is missing.",
                capsule_core::input_resolver::ATO_LOCK_FILE_NAME,
                canonical.path.display()
            );
        }
        Ok(ResolvedInput::CompatibilityProject { project, .. }) => Some(project),
        Ok(ResolvedInput::SourceOnly { .. }) => None,
        Err(error)
            if error
                .to_string()
                .contains("is not an authoritative command-entry input") =>
        {
            return Err(error.into());
        }
        Err(_) => None,
    };

    futures::executor::block_on(reporter.notify(format!(
        "🔍 Initializing capsule in: {}\n",
        project_dir.display()
    )))?;

    let materialized = if let Some(project) = init_target.as_ref() {
        source_inference::execute_init_from_compatibility(project, reporter.clone(), args.yes)?
    } else {
        source_inference::execute_init_from_source_only(&project_dir, reporter.clone(), args.yes)?
    };

    if project_dir.join(".git").exists() {
        add_to_gitignore(&project_dir, reporter.clone())?;
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

fn prompt_for_details(
    mut info: recipe::ProjectInfo,
    reporter: std::sync::Arc<crate::reporters::CliReporter>,
) -> Result<recipe::ProjectInfo> {
    futures::executor::block_on(reporter.notify(format!("\n? Package name: ({}) ", info.name)))?;
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim();
    if !input.is_empty() {
        info.name = input.to_string();
    }

    let default_cmd = if info.entrypoint.is_empty() {
        String::new()
    } else {
        info.entrypoint.join(" ")
    };

    if default_cmd.is_empty() {
        futures::executor::block_on(reporter.notify("? Entry command: ".to_string()))?;
    } else {
        futures::executor::block_on(
            reporter.notify(format!("? Entry command: ({}) ", default_cmd)),
        )?;
    }
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim();
    if !input.is_empty() {
        info.entrypoint = input.split_whitespace().map(|s| s.to_string()).collect();
        if matches!(info.project_type, detect::ProjectType::NodeJs) {
            info.node_dev_entrypoint = Some(info.entrypoint.clone());
            info.node_release_entrypoint = Some(info.entrypoint.clone());
        }
    }

    Ok(info)
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

fn release_binary_name(info: &recipe::ProjectInfo) -> Option<String> {
    let release = info
        .node_release_entrypoint
        .as_ref()
        .and_then(|v| v.first())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())?;

    // We only try to infer a single-file entrypoint like "./my-app".
    if release.contains(' ') || release.contains('\t') {
        return None;
    }

    let release = release.strip_prefix("./").unwrap_or(release);
    if release.contains('/') {
        return None;
    }

    Some(release.to_string())
}
