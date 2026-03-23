//! Project init helpers for prompt generation and interactive manifest creation.

use anyhow::{Context, Result};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use capsule_core::CapsuleReporter;

pub mod detect;
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

pub fn execute_manifest_init(
    args: InitArgs,
    reporter: std::sync::Arc<crate::reporters::CliReporter>,
) -> Result<()> {
    let project_dir = args
        .path
        .unwrap_or_else(|| PathBuf::from("."))
        .canonicalize()
        .context("Failed to resolve project directory")?;

    futures::executor::block_on(reporter.notify(format!(
        "🔍 Initializing capsule in: {}\n",
        project_dir.display()
    )))?;

    let manifest_path = project_dir.join("capsule.toml");
    if manifest_path.exists() {
        anyhow::bail!(
            "capsule.toml already exists!\n\
            Use 'ato dev --manifest capsule.toml' to run, or delete the file to re-initialize."
        );
    }

    let detected = detect::detect_project(&project_dir)?;
    futures::executor::block_on(reporter.notify(format!(
        "   Detected: {} project",
        detected.project_type.as_str()
    )))?;
    if let Some(node) = detected.node.as_ref() {
        if node.is_bun {
            futures::executor::block_on(reporter.notify("   Node runtime: bun".to_string()))?;
        }
        if node.has_hono {
            futures::executor::block_on(reporter.notify("   Framework: hono".to_string()))?;
        }
    }

    let mut info = recipe::project_info_from_detection(&detected)?;
    if !info.entrypoint.is_empty() {
        futures::executor::block_on(
            reporter.notify(format!("   Entrypoint: {}", info.entrypoint.join(" "))),
        )?;
    }
    if let Some(dev) = info.node_dev_entrypoint.as_ref() {
        futures::executor::block_on(reporter.notify(format!("   Dev: {}", dev.join(" "))))?;
    }
    if let Some(release) = info.node_release_entrypoint.as_ref() {
        futures::executor::block_on(reporter.notify(format!("   Release: {}", release.join(" "))))?;
    }

    if !args.yes {
        info = prompt_for_details(info, reporter.clone())?;
    }

    let description = format!(
        "Capsule generated from existing {} project",
        info.project_type.as_str()
    );
    let manifest_content = recipe::generate_manifest(
        &info,
        recipe::ManifestMeta {
            generated_by: "ato build --init",
            description: &description,
        },
    );
    fs::write(&manifest_path, &manifest_content).context("Failed to write capsule.toml")?;

    if project_dir.join(".git").exists() {
        add_to_gitignore(&project_dir, reporter.clone())?;
    }

    // Opt-in packaging control: if this looks like a Node project and node_modules exists,
    // create a minimal .capsuleignore if it doesn't exist yet.
    maybe_create_capsuleignore(&project_dir, &info, reporter.clone())?;

    futures::executor::block_on(reporter.notify("\n✨ Created capsule.toml!".to_string()))?;
    futures::executor::block_on(reporter.notify("\nNext steps:".to_string()))?;
    futures::executor::block_on(
        reporter.notify("   ato dev           # Run locally (no bundling)".to_string()),
    )?;
    futures::executor::block_on(
        reporter.notify("   ato pack --bundle # Create self-extracting bundle".to_string()),
    )?;

    if matches!(info.project_type, detect::ProjectType::Rust) {
        futures::executor::block_on(reporter.notify("\nNote:".to_string()))?;
        futures::executor::block_on(
            reporter.notify("   For Rust, build a release binary before packing:".to_string()),
        )?;
        if let Some(bin) = release_binary_name(&info) {
            futures::executor::block_on(reporter.notify(format!(
                "   cargo build --release && cp target/release/{bin} ./{bin}"
            )))?;
        } else {
            futures::executor::block_on(reporter.notify("   cargo build --release".to_string()))?;
        }
    }
    if matches!(info.project_type, detect::ProjectType::Go) {
        futures::executor::block_on(reporter.notify("\nNote:".to_string()))?;
        futures::executor::block_on(
            reporter.notify("   For Go, build a binary before packing:".to_string()),
        )?;
        if let Some(bin) = release_binary_name(&info) {
            futures::executor::block_on(reporter.notify(format!("   go build -o {bin} .")))?;
        } else {
            futures::executor::block_on(reporter.notify("   go build -o app .".to_string()))?;
        }
    }

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

    let manifest_path = project_dir.join("capsule.toml");
    if manifest_path.exists() {
        anyhow::bail!(
            "capsule.toml already exists!\n\
            Delete or move the file before creating a manual starter manifest."
        );
    }

    let detected = detect::detect_project(&project_dir)?;
    let manifest_content = recipe::generate_manual_manifest_stub(&detected.name);
    fs::write(&manifest_path, manifest_content).context("Failed to write capsule.toml")?;

    futures::executor::block_on(reporter.notify(format!(
        "📝 Created a manual starter capsule.toml at {}.\nEdit it, then rerun `ato run`.",
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

    if existing.contains(".capsule/") || existing.contains("*.capsule") {
        return Ok(());
    }

    let addition = "\n# Capsule\n.capsule/\n*.capsule\n*.sig\n";
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
