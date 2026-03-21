use std::path::PathBuf;

use anyhow::Result;

use crate::build::native_delivery;
use crate::cli::ProjectCommands;

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
