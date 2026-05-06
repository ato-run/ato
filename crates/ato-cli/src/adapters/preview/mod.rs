mod draft;
mod manifest;
mod storage;

#[allow(unused_imports)]
pub(crate) use crate::application::preview::{
    DerivedExecutionPlan, GitHubPreviewPreparation, PreviewPromotionEligibility, PreviewSession,
    PreviewStorageLayout, PreviewTargetKind,
};
#[allow(unused_imports)]
pub(crate) use draft::{
    draft_requires_manual_review, github_draft_manual_review_reason, prepare_github_preview_session,
};
#[allow(unused_imports)]
pub(crate) use manifest::required_env_from_preview_toml;
#[allow(unused_imports)]
pub(crate) use storage::{
    load_preview_session_for_manifest, persist_session_with_warning, preview_root,
};

pub(super) const ENV_PREVIEW_ROOT: &str = "ATO_PREVIEW_ROOT";
pub(super) const PREVIEW_METADATA_FILE_NAME: &str = "metadata.json";
pub(super) const PREVIEW_MANIFEST_FILE_NAME: &str = "capsule.toml";

#[cfg(test)]
mod tests;
