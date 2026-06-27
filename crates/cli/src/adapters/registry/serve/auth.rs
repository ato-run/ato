use axum::http::{HeaderMap, header};
use subtle::ConstantTimeEq;

/// Extract a trimmed, non-empty `Authorization: Bearer <token>` value.
fn extract_bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

pub(super) fn validate_write_auth(
    headers: &HeaderMap,
    expected_token: Option<&str>,
) -> Result<(), String> {
    let Some(expected) = expected_token.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(());
    };

    let actual = extract_bearer(headers);

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

    let actual = extract_bearer(headers);

    if constant_time_token_eq(expected.as_bytes(), actual.unwrap_or("").as_bytes()) {
        return Ok(());
    }

    Err("Bearer token is required for manifest read API".to_string())
}

/// Authorization for the Runtime Control API's privileged operations
/// (`launch`, `stop`, `delete`, `add-capsule`, and raw `logs`).
///
/// Unlike [`validate_write_auth`] — which is used by the local registry /
/// publish endpoints and intentionally permits tokenless operation for local
/// developer convenience — the Runtime Control plane is reachable from the
/// embedded PWA WebView and from any CORS-allowlisted browser origin
/// (`https://app.ato.run`, `http://localhost:5173`). It must therefore
/// **fail closed**: if no control token is configured, every privileged call
/// is rejected, and a configured token is required even when the requesting
/// origin is allowlisted. This closes the prior gap where tokenless loopback
/// serving left launch/stop unauthenticated.
pub(super) fn validate_runtime_privileged_auth(
    headers: &HeaderMap,
    expected_token: Option<&str>,
) -> Result<(), String> {
    let Some(expected) = expected_token.map(str::trim).filter(|v| !v.is_empty()) else {
        // Fail closed: no token configured ⇒ no privileged runtime access.
        return Err(
            "Runtime control token is required (server started without --auth-token)".to_string(),
        );
    };

    let actual = extract_bearer(headers);

    if constant_time_token_eq(expected.as_bytes(), actual.unwrap_or("").as_bytes()) {
        return Ok(());
    }

    Err("A valid runtime control bearer token is required".to_string())
}

pub(super) fn constant_time_token_eq(expected: &[u8], actual: &[u8]) -> bool {
    use sha2::{Digest, Sha256};

    let expected_digest = Sha256::digest(expected);
    let actual_digest = Sha256::digest(actual);
    expected_digest[..].ct_eq(&actual_digest[..]).into()
}
