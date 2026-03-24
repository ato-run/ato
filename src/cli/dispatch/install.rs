use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::Result;

use crate::install;
use crate::install::support::can_prompt_interactively;

pub(crate) struct InstallCommandArgs {
    pub(crate) slug: Option<String>,
    pub(crate) from_gh_repo: Option<String>,
    pub(crate) registry: Option<String>,
    pub(crate) version: Option<String>,
    pub(crate) default: bool,
    pub(crate) yes: bool,
    pub(crate) skip_verify_legacy: bool,
    pub(crate) allow_unverified: bool,
    pub(crate) output: Option<PathBuf>,
    pub(crate) project: bool,
    pub(crate) no_project: bool,
    pub(crate) json: bool,
    pub(crate) keep_failed_artifacts: bool,
}

pub(crate) fn execute_install_command(args: InstallCommandArgs) -> Result<()> {
    if args.skip_verify_legacy {
        anyhow::bail!(
            "--skip-verify is no longer supported. Signature/hash verification is always required."
        );
    }

    let projection_preference = projection_preference(args.project, args.no_project);
    let can_prompt = !args.json
        && can_prompt_interactively(
            std::io::stdin().is_terminal(),
            std::io::stderr().is_terminal(),
        );
    let rt = tokio::runtime::Runtime::new()?;

    if let Some(repository) = args.from_gh_repo.as_deref() {
        if args.registry.is_some() {
            anyhow::bail!("--registry cannot be used with --from-gh-repo");
        }
        if args.version.is_some() {
            anyhow::bail!("--version cannot be used with --from-gh-repo");
        }

        let result = rt.block_on(install::support::install_github_repository(
            repository,
            args.output,
            args.yes,
            projection_preference,
            args.json,
            can_prompt,
            args.keep_failed_artifacts,
        ))?;
        render_install_result(&result, args.json, args.no_project)?;
        return Ok(());
    }

    rt.block_on(async {
        let slug = args.slug.ok_or_else(|| {
            anyhow::anyhow!("capsule slug is required when not using --from-gh-repo")
        })?;
        if install::is_slug_only_ref(&slug) {
            anyhow::bail!(
                "{}",
                crate::scoped_id_prompt::install_scoped_id_prompt(&slug, args.registry.as_deref(),)
                    .await?
            );
        }

        let result = install::install_app(
            &slug,
            args.registry.as_deref(),
            args.version.as_deref(),
            args.output,
            args.default,
            args.yes,
            projection_preference,
            args.allow_unverified,
            false,
            args.json,
            can_prompt,
        )
        .await?;

        render_install_result(&result, args.json, args.no_project)
    })
}

fn projection_preference(project: bool, no_project: bool) -> install::ProjectionPreference {
    if project {
        install::ProjectionPreference::Force
    } else if no_project {
        install::ProjectionPreference::Skip
    } else {
        install::ProjectionPreference::Prompt
    }
}

fn render_install_result(
    result: &install::InstallResult,
    json: bool,
    no_project: bool,
) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(result)?);
    } else if crate::progressive_ui::can_use_progressive_ui(false) {
    } else {
        println!("\n✅ Installation complete!");
        println!("   Capsule: {}", result.slug);
        println!("   Version: {}", result.version);
        println!("   Path:    {}", result.path.display());
        println!("   Hash:    {}", result.content_hash);
        if let Some(launchable) = &result.launchable {
            match launchable {
                install::LaunchableTarget::CapsuleArchive { path } => {
                    println!("   Launch:  ato run {}", path.display());
                }
                install::LaunchableTarget::DerivedApp { path } => {
                    println!("   App:     {}", path.display());
                }
            }
        }
        if let Some(projection) = &result.projection {
            if projection.performed {
                if let Some(projected_path) = &projection.projected_path {
                    println!("   Launcher: {}", projected_path.display());
                }
            } else if no_project {
                println!("   Launcher: skipped");
            }
        }
    }
    Ok(())
}
