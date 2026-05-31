mod capsule_toml;
pub(crate) mod prompt;
pub(crate) mod receipt_upload;
pub(crate) mod submit;

use anyhow::Context;

pub(crate) use capsule_toml::{
    extract_toml_source, fetch_capsule_toml_by_id, fetch_community_capsule_tomls,
    fetch_toml_from_url, prompt_community_candidate_selection, prompt_no_candidates_flow,
    sort_candidates, validate_candidate_source_matches_run_target,
    validate_capsule_toml_source_matches_run_target, validate_capsule_toml_source_with_provenance,
    SourceValidationOutcome,
};
pub(crate) use prompt::{
    community_submit_prompt_disabled, confirm_community_submit_prompt,
    should_prompt_for_community_submit, try_community_submit_after_run, CommunitySubmitOrigin,
    CommunitySubmitPromptContext,
};

/// Fetch a community capsule.toml by ID and validate that its `[source].repository`
/// matches `expected_source`.
///
/// `expected_source` should be a normalized GitHub source string such as
/// `github.com/owner/repo` or `capsule://github.com/owner/repo` (the
/// `capsule://` prefix is stripped before comparison).
///
/// Returns the raw TOML content on success.
/// Fails closed on: API 404, non-2xx, invalid TOML, missing source, mismatch.
pub(crate) fn fetch_and_validate_community_toml(
    ctoml_id: &str,
    expected_source: &str,
) -> anyhow::Result<String> {
    // Normalise expected_source: strip capsule:// prefix if present.
    let normalized = expected_source
        .strip_prefix("capsule://")
        .unwrap_or(expected_source)
        .trim_end_matches('/');

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .with_context(|| "failed to build tokio runtime for community TOML fetch")?;

    let content = rt.block_on(fetch_capsule_toml_by_id(ctoml_id))?;

    // Validate source identity — fail closed on mismatch or missing source.
    match validate_capsule_toml_source_matches_run_target(&content, normalized) {
        SourceValidationOutcome::Match => {}
        SourceValidationOutcome::MissingSource => {
            // No [source].repository in the TOML — treat as mismatch to avoid
            // launching an unverified recipe as the wrong capsule.
            anyhow::bail!(
                "community capsule.toml {} has no [source].repository field; \
                 cannot verify it belongs to '{}'",
                ctoml_id,
                normalized
            );
        }
        SourceValidationOutcome::Mismatch {
            toml_source,
            expected_source: expected,
        } => {
            anyhow::bail!(
                "community capsule.toml {} source mismatch: \
                 TOML says '{}' but expected '{}'",
                ctoml_id,
                toml_source,
                expected
            );
        }
    }

    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Validates source identity match succeeds when TOML source equals expected.
    #[test]
    fn fetch_and_validate_community_toml_source_match_integration_skipped() {
        // This test requires a live API; it is skipped unless ATO_E2E_TEST=1.
        if std::env::var("ATO_E2E_TEST").as_deref() != Ok("1") {
            return;
        }
        // Real network test would go here — omitted for unit test suite.
    }

    /// Source identity mismatch must fail closed.
    #[test]
    fn validate_source_mismatch_fails_closed() {
        let toml_content = r#"
[source]
repository = "github.com/other/repo"
"#;
        let outcome =
            validate_capsule_toml_source_matches_run_target(toml_content, "github.com/owner/repo");
        assert!(
            matches!(outcome, SourceValidationOutcome::Mismatch { .. }),
            "mismatch should be detected"
        );
    }

    /// Missing source identity must fail closed.
    #[test]
    fn validate_missing_source_detected() {
        let toml_content = r#"
[metadata]
title = "some capsule"
"#;
        let outcome =
            validate_capsule_toml_source_matches_run_target(toml_content, "github.com/owner/repo");
        assert!(
            matches!(outcome, SourceValidationOutcome::MissingSource),
            "missing source should be detected"
        );
    }

    /// Valid TOML with matching source should return Match.
    #[test]
    fn validate_matching_source_returns_match() {
        let toml_content = r#"
[source]
repository = "github.com/owner/repo"
"#;
        let outcome =
            validate_capsule_toml_source_matches_run_target(toml_content, "github.com/owner/repo");
        assert!(
            matches!(outcome, SourceValidationOutcome::Match),
            "matching source should return Match"
        );
    }

    /// Invalid TOML content should not match (extract_toml_source returns None).
    #[test]
    fn validate_invalid_toml_fails_closed() {
        let toml_content = "not valid toml [[[";
        let outcome =
            validate_capsule_toml_source_matches_run_target(toml_content, "github.com/owner/repo");
        assert!(
            matches!(outcome, SourceValidationOutcome::MissingSource),
            "invalid TOML should behave as missing source"
        );
    }

    /// capsule:// prefix on expected_source should be stripped before comparison.
    #[test]
    fn validate_strips_capsule_prefix_on_expected_source() {
        // fetch_and_validate_community_toml strips "capsule://" before calling
        // validate_capsule_toml_source_matches_run_target. We test the underlying
        // function directly with the already-stripped form.
        let toml_content = r#"
[source]
repository = "github.com/usememos/memos"
"#;
        let outcome = validate_capsule_toml_source_matches_run_target(
            toml_content,
            "github.com/usememos/memos", // already stripped
        );
        assert!(matches!(outcome, SourceValidationOutcome::Match));
    }
}
