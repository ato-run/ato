use super::*;

pub(crate) fn persist_promotion_info(
    artifact_path: &Path,
    promotion_source: Option<&PromotionSourceInfo>,
    content_hash: &str,
) -> Result<Option<PromotionInfo>> {
    let Some(source) = promotion_source else {
        return Ok(None);
    };

    let install_dir = artifact_path.parent().ok_or_else(|| {
        anyhow::anyhow!(
            "installed artifact must have a parent directory: {}",
            artifact_path.display()
        )
    })?;
    let metadata_path = install_dir.join("promotion.json");
    let promotion = PromotionInfo {
        performed: true,
        preview_id: Some(source.preview_id.clone()),
        source_reference: Some(source.source_reference.clone()),
        source_metadata_path: Some(source.source_metadata_path.clone()),
        source_manifest_path: Some(source.source_manifest_path.clone()),
        manifest_source: source.manifest_source.clone(),
        inference_mode: source.inference_mode.clone(),
        resolved_ref: source.resolved_ref.clone(),
        derived_plan: Some(source.derived_plan.clone()),
        promotion_metadata_path: Some(metadata_path.clone()),
        content_hash: Some(content_hash.to_string()),
    };
    let serialized =
        serde_json::to_vec_pretty(&promotion).context("Failed to serialize promotion metadata")?;
    std::fs::write(&metadata_path, serialized).with_context(|| {
        format!(
            "Failed to write promotion metadata: {}",
            metadata_path.display()
        )
    })?;
    Ok(Some(promotion))
}

pub(crate) fn persist_installed_artifact(
    output_dir: Option<PathBuf>,
    publisher: &str,
    slug: &str,
    version: &str,
    normalized_file_name: &str,
    bytes: &[u8],
    content_hash: &str,
) -> Result<PathBuf> {
    let store_root = output_dir.unwrap_or_else(capsule_core::common::paths::ato_store_dir);
    let install_dir = store_root.join(publisher).join(slug).join(version);
    std::fs::create_dir_all(&install_dir).with_context(|| {
        format!(
            "Failed to create store directory: {}",
            install_dir.display()
        )
    })?;

    let output_path = install_dir.join(normalized_file_name);
    sweep_stale_tmp_capsules(&install_dir)?;
    write_capsule_atomic(&output_path, bytes, content_hash)?;
    runtime_tree::prepare_runtime_tree(publisher, slug, version, bytes)?;
    Ok(output_path)
}

pub(crate) fn prompt_for_confirmation(prompt: &str, default_yes: bool) -> Result<bool> {
    crate::progressive_ui::confirm_with_fallback(
        prompt,
        default_yes,
        crate::progressive_ui::can_use_progressive_ui(false),
    )
}
