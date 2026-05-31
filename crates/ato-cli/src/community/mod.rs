mod capsule_toml;
pub(crate) mod prompt;
pub(crate) mod receipt_upload;
pub(crate) mod submit;

pub(crate) use capsule_toml::{
    extract_toml_source, fetch_capsule_toml_by_id, fetch_community_capsule_tomls,
    fetch_toml_from_url, prompt_community_candidate_selection, sort_candidates,
    validate_candidate_source_matches_run_target, validate_capsule_toml_source_matches_run_target,
    validate_capsule_toml_source_with_provenance, SourceValidationOutcome,
};
pub(crate) use prompt::{
    community_submit_prompt_disabled, confirm_community_submit_prompt,
    should_prompt_for_community_submit, try_community_submit_after_run, CommunitySubmitOrigin,
    CommunitySubmitPromptContext,
};
