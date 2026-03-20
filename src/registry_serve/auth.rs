use super::*;

pub(super) fn validate_write_auth(
    headers: &HeaderMap,
    expected_token: Option<&str>,
) -> Result<(), String> {
    let Some(expected) = expected_token.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(());
    };

    let actual = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|v| !v.is_empty());

    if constant_time_token_eq(expected.as_bytes(), actual.unwrap_or("").as_bytes()) {
        return Ok(());
    }

    Err("Bearer token is required for upload".to_string())
}

pub(super) fn validate_read_auth(
    headers: &HeaderMap,
    expected_token: Option<&str>,
) -> Result<(), String> {
    let Some(expected) = expected_token.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(());
    };

    let actual = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|v| !v.is_empty());

    if constant_time_token_eq(expected.as_bytes(), actual.unwrap_or("").as_bytes()) {
        return Ok(());
    }

    Err("Bearer token is required for manifest read API".to_string())
}

pub(super) fn constant_time_token_eq(expected: &[u8], actual: &[u8]) -> bool {
    use sha2::{Digest, Sha256};

    let expected_digest = Sha256::digest(expected);
    let actual_digest = Sha256::digest(actual);
    expected_digest[..].ct_eq(&actual_digest[..]).into()
}
