//! Upload / download / delete authorization.
//!
//! Tokens are compared in constant time over fixed-length digests, so neither
//! the token value nor its length is exposed by comparison timing. Tokens are
//! never logged and never appear in `Debug`/`Display` output.

use std::fmt;

use hyper::header::AUTHORIZATION;
use sha2::{Digest, Sha256};

/// A credential that must never be rendered.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Exposes the raw value for comparison only.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret(<redacted>)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

/// Constant-time equality over SHA-256 digests of both inputs.
///
/// Digests are fixed length, so equal-length content and different-length
/// content take the same comparison path.
pub fn token_matches(expected: &str, presented: &str) -> bool {
    let expected = Sha256::digest(expected.as_bytes());
    let presented = Sha256::digest(presented.as_bytes());
    expected
        .iter()
        .zip(presented.iter())
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

/// Extracts a `Bearer` token from an `Authorization` header value.
pub fn bearer_token(value: &str) -> Option<&str> {
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    if token.is_empty() { None } else { Some(token) }
}

/// Checks an `Authorization` header against an optional configured secret.
///
/// Returns `false` when no secret is configured (fail closed) or when the
/// header is absent or malformed.
pub fn authorized(configured: Option<&Secret>, headers: &hyper::HeaderMap) -> bool {
    let Some(configured) = configured else {
        return false;
    };
    let Some(value) = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Some(presented) = bearer_token(value) else {
        return false;
    };
    token_matches(configured.expose(), presented)
}

#[cfg(test)]
mod tests {
    use hyper::HeaderMap;
    use hyper::header::{AUTHORIZATION, HeaderValue};

    use super::*;

    #[test]
    fn token_comparison_is_exact() {
        assert!(token_matches("s3cret", "s3cret"));
        assert!(!token_matches("s3cret", "s3cres"));
        assert!(!token_matches("s3cret", "s3cret "));
        assert!(!token_matches("s3cret", ""));
    }

    #[test]
    fn bearer_extraction_is_strict() {
        assert_eq!(bearer_token("Bearer abc"), Some("abc"));
        assert_eq!(bearer_token("bearer abc"), Some("abc"));
        assert_eq!(bearer_token("Basic abc"), None);
        assert_eq!(bearer_token("Bearer"), None);
        assert_eq!(bearer_token("Bearer   "), None);
    }

    #[test]
    fn authorization_fails_closed_without_a_configured_secret() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer abc"));
        assert!(!authorized(None, &headers));
        assert!(authorized(Some(&Secret::new("abc")), &headers));
        assert!(!authorized(Some(&Secret::new("abd")), &headers));
    }

    #[test]
    fn secrets_are_never_rendered() {
        let secret = Secret::new("super-secret-token");
        for rendered in [format!("{secret:?}"), format!("{secret}")] {
            assert!(!rendered.contains("super-secret-token"));
            assert!(rendered.contains("redacted"));
        }
    }
}
