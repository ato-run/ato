use anyhow::Result;

use crate::install;

async fn render_ambiguous_scoped_id_message(
    slug: &str,
    registry: Option<&str>,
    missing_message: String,
    heading: &str,
) -> Result<String> {
    let suggestions = install::suggest_scoped_capsules(slug, registry, 5).await?;
    if suggestions.is_empty() {
        return Ok(missing_message);
    }

    let mut message = format!("{heading}\n\nDid you mean one of these?");
    for suggestion in suggestions {
        message.push_str(&format!(
            "\n  - {}  ({} downloads)",
            suggestion.scoped_id, suggestion.downloads
        ));
    }
    message.push_str("\n\nRun `ato search ");
    message.push_str(slug.trim());
    message.push_str("` to see more options.");
    Ok(message)
}

pub(crate) async fn install_scoped_id_prompt(slug: &str, registry: Option<&str>) -> Result<String> {
    render_ambiguous_scoped_id_message(
        slug,
        registry,
        format!(
            "scoped_id_required: '{}' is ambiguous. Use publisher/slug (for example: koh0920/{})",
            slug, slug
        ),
        &format!("scoped_id_required: '{}' requires publisher scope.", slug),
    )
    .await
}

pub(crate) async fn run_scoped_id_prompt(slug: &str, registry: Option<&str>) -> Result<String> {
    render_ambiguous_scoped_id_message(
        slug,
        registry,
        format!(
            "scoped_id_required: '{}' is not valid for `ato run`. Use publisher/slug (for example: koh0920/{}).",
            slug,
            slug.trim()
        ),
        &format!(
            "scoped_id_required: '{}' is ambiguous. Specify publisher/slug.",
            slug
        ),
    )
    .await
}
