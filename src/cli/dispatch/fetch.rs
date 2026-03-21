use std::path::PathBuf;

use anyhow::Result;

use crate::build::native_delivery;
use crate::install;

pub(super) fn execute_fetch_command(
    capsule_ref: String,
    registry: Option<String>,
    version: Option<String>,
    json_mode: bool,
) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        if install::is_slug_only_ref(&capsule_ref) {
            let suggestions =
                install::suggest_scoped_capsules(&capsule_ref, registry.as_deref(), 5).await?;
            if suggestions.is_empty() {
                anyhow::bail!(
                    "scoped_id_required: '{}' is ambiguous. Use publisher/slug (for example: koh0920/{})",
                    capsule_ref,
                    capsule_ref
                );
            }
            let mut message = format!(
                "scoped_id_required: '{}' requires publisher scope.\n\nDid you mean one of these?",
                capsule_ref
            );
            for suggestion in suggestions {
                message.push_str(&format!(
                    "\n  - {}  ({} downloads)",
                    suggestion.scoped_id, suggestion.downloads
                ));
            }
            anyhow::bail!(message);
        }

        let result = native_delivery::execute_fetch(
            &capsule_ref,
            registry.as_deref(),
            version.as_deref(),
        )
        .await?;
        if json_mode {
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            println!("✅ Fetched to: {}", result.cache_dir.display());
            println!("   Scoped ID: {}", result.scoped_id);
            println!("   Version:   {}", result.version);
            println!("   Digest:    {}", result.parent_digest);
        }
        Ok(())
    })
}

pub(super) fn execute_finalize_command(
    fetched_artifact_dir: PathBuf,
    allow_external_finalize: bool,
    output_dir: PathBuf,
    json_mode: bool,
) -> Result<()> {
    let result = native_delivery::execute_finalize(
        &fetched_artifact_dir,
        &output_dir,
        allow_external_finalize,
    )?;
    if json_mode {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("✅ Finalized to: {}", result.output_dir.display());
        println!("   App:      {}", result.derived_app_path.display());
        println!("   Parent:   {}", result.parent_digest);
        println!("   Derived:  {}", result.derived_digest);
    }
    Ok(())
}
