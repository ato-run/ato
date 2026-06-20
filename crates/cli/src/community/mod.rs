mod capsule_toml;
pub(crate) mod prompt;
pub(crate) mod receipt_upload;
pub(crate) mod submit;

use anyhow::Context;

pub(crate) use capsule_toml::{
    CommunityCapsuleTomlCandidate, SourceValidationOutcome, fetch_capsule_toml_by_id,
    fetch_community_capsule_tomls, fetch_toml_from_url, prompt_community_candidate_selection,
    prompt_no_candidates_flow, sort_candidates, validate_candidate_source_matches_run_target,
    validate_capsule_toml_source_matches_run_target, validate_capsule_toml_source_with_provenance,
};
pub(crate) use prompt::{
    CommunitySubmitOrigin, CommunitySubmitPromptContext, confirm_community_submit_prompt,
    should_prompt_for_community_submit, try_community_submit_after_run,
};

/// Normalize an expected-source string to the API-stored form (`owner/repo`).
///
/// Strips:
///   - `capsule://` scheme prefix
///   - `github.com/` host prefix
///   - trailing slashes
///
/// This mirrors the normalization the CLI performs at submit time.
pub(crate) fn normalize_expected_source(expected_source: &str) -> String {
    let stripped = expected_source
        .strip_prefix("capsule://")
        .unwrap_or(expected_source)
        .trim_end_matches('/');
    stripped
        .strip_prefix("github.com/")
        .unwrap_or(stripped)
        .to_string()
}

/// Fetch a community capsule.toml by ID and validate its source identity
/// against the **actual registry provenance** stored in the community API.
///
/// `expected_source` may be any of:
///   - `github.com/owner/repo`
///   - `capsule://github.com/owner/repo`
///   - `owner/repo`  (already-normalized API form)
///
/// ## Validation algorithm
///
/// 1. Normalize `expected_source` to the API form (`owner/repo`).
/// 2. Query `GET /v1/capsule-tomls?source=<normalized>` to get the
///    candidates registered under that source.
/// 3. Verify that the specified `ctoml_id` appears in the result.
///    Fail closed if not — this means the registry does not associate
///    the given ID with the expected source.
/// 4. Fetch the raw TOML from `GET /v1/capsule-tomls/<id>`.
/// 5. Validate the TOML's `[source].repository` (if present) against the
///    candidate's `source` field using `validate_capsule_toml_source_with_provenance`.
///    If the TOML has no `[source].repository`, the registry provenance
///    serves as the identity anchor (consistent with submit-time behaviour).
///
/// Fails closed on: candidate not found for source, API 404/non-2xx,
/// invalid TOML, source mismatch.
pub(crate) fn fetch_and_validate_community_toml(
    ctoml_id: &str,
    expected_source: &str,
) -> anyhow::Result<String> {
    let normalized_api = normalize_expected_source(expected_source);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .with_context(|| "failed to build tokio runtime for community TOML fetch")?;

    // Step 1: Fetch candidates for the expected source to get registry provenance.
    let candidates = rt
        .block_on(fetch_community_capsule_tomls(&normalized_api))
        .with_context(|| {
            format!("failed to fetch community candidates for source '{normalized_api}'")
        })?;

    // Step 2: Verify the ctoml_id belongs to this source.
    let candidate: &CommunityCapsuleTomlCandidate = candidates
        .iter()
        .find(|c| c.id == ctoml_id)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "community capsule.toml '{}' is not registered under source '{}'; \
                 refusing to launch — possible source-binding mismatch",
                ctoml_id,
                normalized_api
            )
        })?;

    // Step 3: Use the registry-stored `source` as the authoritative provenance.
    let registry_provenance = &candidate.source;

    // Step 4: Fetch the TOML content.
    let content = rt
        .block_on(fetch_capsule_toml_by_id(ctoml_id))
        .with_context(|| format!("failed to fetch TOML content for '{ctoml_id}'"))?;

    // Step 5: Validate TOML source identity against registry provenance.
    match validate_capsule_toml_source_with_provenance(
        &content,
        &normalized_api,
        registry_provenance,
    ) {
        SourceValidationOutcome::Match => {}
        SourceValidationOutcome::MissingSource => {
            // Should not be reachable when provenance == normalized_api
            anyhow::bail!(
                "community capsule.toml '{}' has no verifiable source identity for '{}'",
                ctoml_id,
                normalized_api
            );
        }
        SourceValidationOutcome::Mismatch {
            toml_source,
            expected_source: expected,
        } => {
            anyhow::bail!(
                "community capsule.toml '{}' source mismatch: \
                 TOML declares '{}' but registry provenance is '{}'",
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
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn mock_http_sequence(responses: Vec<(u16, &'static str)>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock community API");
        let port = listener.local_addr().expect("mock addr").port();
        std::thread::spawn(move || {
            for (status, body) in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut buf = vec![0u8; 4096];
                let _ = stream.read(&mut buf);
                let reason = match status {
                    200 => "OK",
                    404 => "Not Found",
                    500 => "Internal Server Error",
                    _ => "Unknown",
                };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://127.0.0.1:{port}")
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

    /// Missing source identity with matching provenance returns Match.
    #[test]
    fn validate_missing_source_ok_with_matching_provenance() {
        let toml_content = r#"
[metadata]
title = "some capsule"
"#;
        let outcome =
            validate_capsule_toml_source_with_provenance(toml_content, "owner/repo", "owner/repo");
        assert!(
            matches!(outcome, SourceValidationOutcome::Match),
            "missing source with matching provenance should be Match"
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

    /// Invalid TOML content behaves as missing source.
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

    /// normalize_expected_source strips capsule:// and github.com/ prefixes.
    #[test]
    fn normalize_expected_source_strips_prefixes() {
        assert_eq!(
            normalize_expected_source("capsule://github.com/owner/repo"),
            "owner/repo"
        );
        assert_eq!(
            normalize_expected_source("github.com/owner/repo"),
            "owner/repo"
        );
        assert_eq!(normalize_expected_source("owner/repo"), "owner/repo");
        assert_eq!(
            normalize_expected_source("github.com/owner/repo/"),
            "owner/repo"
        );
    }

    /// ctoml_id not found in candidates for source must fail closed.
    #[test]
    fn validate_ctoml_id_not_in_candidates_fails_closed() {
        // The candidates list for "owner/repo" does not contain "ctoml_wrong".
        let candidates: Vec<CommunityCapsuleTomlCandidate> = vec![CommunityCapsuleTomlCandidate {
            id: "ctoml_correct".to_string(),
            title: "Correct".to_string(),
            source: "owner/repo".to_string(),
            trust: crate::community::capsule_toml::CommunityTrustLevel::Community,
            stars: 0,
            platforms: vec![],
            last_verified_at: None,
            permissions_summary: vec![],
            capsule_toml_url: "https://api.ato.run/v1/capsule-tomls/ctoml_correct".to_string(),
            revision: None,
            successful_receipts: None,
            failed_receipts: None,
            current_platform_verified: None,
            source_ref: None,
            source_ref_age_days: None,
            risk_score: None,
        }];
        let result = candidates.iter().find(|c| c.id == "ctoml_wrong");
        assert!(
            result.is_none(),
            "ctoml_wrong must not be found in candidates for owner/repo"
        );
    }

    #[test]
    fn fetch_and_validate_rejects_ctoml_id_from_another_source() {
        let _guard = super::capsule_toml::COMMUNITY_URL_MUTEX.blocking_lock();
        let base = mock_http_sequence(vec![(
            200,
            r#"{"candidates":[
                {"id":"ctoml_other","title":"Other","source":"owner/repo","trust":"community","stars":0,"platforms":[],"lastVerifiedAt":null,"permissionsSummary":[],"capsuleTomlUrl":"http://x/ctoml_other","revision":null}
            ]}"#,
        )]);
        unsafe {
            std::env::set_var("ATO_COMMUNITY_API_URL", &base);
        }
        let err = fetch_and_validate_community_toml("ctoml_requested", "github.com/owner/repo")
            .expect_err("ctoml id must be registered under expected source");
        unsafe {
            std::env::remove_var("ATO_COMMUNITY_API_URL");
        }
        assert!(err.to_string().contains("not registered under source"));
    }

    #[test]
    fn fetch_and_validate_rejects_toml_source_mismatch() {
        let _guard = super::capsule_toml::COMMUNITY_URL_MUTEX.blocking_lock();
        let base = mock_http_sequence(vec![
            (
                200,
                r#"{"candidates":[
                    {"id":"ctoml_ok","title":"Recipe","source":"owner/repo","trust":"community","stars":0,"platforms":[],"lastVerifiedAt":null,"permissionsSummary":[],"capsuleTomlUrl":"http://x/ctoml_ok","revision":null}
                ]}"#,
            ),
            (200, "[source]\nrepository = \"other/repo\"\n"),
        ]);
        unsafe {
            std::env::set_var("ATO_COMMUNITY_API_URL", &base);
        }
        let err = fetch_and_validate_community_toml("ctoml_ok", "github.com/owner/repo")
            .expect_err("TOML source mismatch must fail closed");
        unsafe {
            std::env::remove_var("ATO_COMMUNITY_API_URL");
        }
        assert!(err.to_string().contains("source mismatch"));
    }

    #[test]
    fn fetch_and_validate_allows_missing_toml_source_when_registry_matches() {
        let _guard = super::capsule_toml::COMMUNITY_URL_MUTEX.blocking_lock();
        let base = mock_http_sequence(vec![
            (
                200,
                r#"{"candidates":[
                    {"id":"ctoml_ok","title":"Recipe","source":"owner/repo","trust":"community","stars":0,"platforms":[],"lastVerifiedAt":null,"permissionsSummary":[],"capsuleTomlUrl":"http://x/ctoml_ok","revision":null}
                ]}"#,
            ),
            (
                200,
                "schema_version = \"0.3\"\nname = \"recipe\"\ntype = \"app\"\n",
            ),
        ]);
        unsafe {
            std::env::set_var("ATO_COMMUNITY_API_URL", &base);
        }
        let content = fetch_and_validate_community_toml("ctoml_ok", "github.com/owner/repo")
            .expect("registry provenance should anchor missing TOML source");
        unsafe {
            std::env::remove_var("ATO_COMMUNITY_API_URL");
        }
        assert!(content.contains("schema_version"));
    }
}
