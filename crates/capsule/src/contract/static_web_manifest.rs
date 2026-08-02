//! Layer 2: the immutable `ato.static-web-manifest/v1` delivery contract.
//!
//! The manifest deliberately contains only immutable output facts. Mutable
//! host assignment, delivery state, headers, and deployment credentials belong
//! to the data plane and are never part of this content-addressed payload.

use std::collections::BTreeMap;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use unicode_normalization::UnicodeNormalization;
use url::Url;

/// Schema tag shared with the static-content Worker.
pub const STATIC_WEB_MANIFEST_V1_SCHEMA: &str = "ato.static-web-manifest/v1";

/// The only embedding origins v1 recognizes. A producer always emits all three;
/// readers accept a non-empty subset for backward-compatible immutable artifacts.
pub const STATIC_WEB_FRAME_ANCESTORS_V1: &[&str] = &[
    "https://ato.run",
    "https://app.ato.run",
    "https://staging.ato.run",
];
const MAX_SAFE_JSON_INTEGER: u64 = 9_007_199_254_740_991;
const MAX_FILE_COUNT: usize = 10_000;
const MAX_FILE_SIZE: u64 = 64 * 1024 * 1024;
const MAX_TOTAL_SIZE: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StaticWebManifestError {
    #[error("static web manifest schema must be {STATIC_WEB_MANIFEST_V1_SCHEMA}")]
    InvalidSchema,
    #[error("static web manifest field '{0}' must be non-empty")]
    EmptyField(&'static str),
    #[error("static web manifest path is invalid: {0}")]
    InvalidPath(String),
    #[error("static web manifest contains no entry file")]
    MissingEntry,
    #[error("static web manifest exceeds a v1 closure limit")]
    ClosureLimit,
    #[error("static web manifest blob must be sha256:<64 lowercase hex characters>")]
    InvalidBlob,
    #[error("static web manifest media type is not in the v1 allowlist: {0}")]
    InvalidMediaType(String),
    #[error("static web manifest connect-src is not a public https/wss origin: {0}")]
    InvalidConnectSource(String),
    #[error("static web manifest frame ancestor is not in the v1 trust set: {0}")]
    InvalidFrameAncestor(String),
    #[error("static web manifest has a duplicate security origin: {0}")]
    DuplicateSecurityOrigin(String),
    #[error("failed to canonicalize static web manifest: {0}")]
    Canonicalization(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticWebManifestV1 {
    pub schema: String,
    pub materialization_id: String,
    pub entry_path: String,
    pub routing: StaticWebRoutingV1,
    pub files: BTreeMap<String, StaticWebFileV1>,
    pub security: StaticWebSecurityV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticWebRoutingV1 {
    pub spa_fallback: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticWebFileV1 {
    pub blob: String,
    pub size: u64,
    pub media_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticWebSecurityV1 {
    pub connect_src: Vec<String>,
    pub frame_ancestors: Vec<String>,
}

impl StaticWebSecurityV1 {
    pub fn producer_policy(connect_src: Vec<String>) -> Self {
        Self {
            connect_src,
            frame_ancestors: STATIC_WEB_FRAME_ANCESTORS_V1
                .iter()
                .map(|origin| (*origin).to_owned())
                .collect(),
        }
    }
}

impl StaticWebManifestV1 {
    pub fn validate(&self) -> Result<(), StaticWebManifestError> {
        if self.schema != STATIC_WEB_MANIFEST_V1_SCHEMA {
            return Err(StaticWebManifestError::InvalidSchema);
        }
        if self.materialization_id.is_empty() {
            return Err(StaticWebManifestError::EmptyField("materialization_id"));
        }
        if self.materialization_id.len() > 128
            || !self
                .materialization_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(StaticWebManifestError::EmptyField("materialization_id"));
        }
        validate_relative_path(&self.entry_path)?;
        if self.files.is_empty()
            || self.files.len() > MAX_FILE_COUNT
            || !self.files.contains_key(&self.entry_path)
        {
            return Err(StaticWebManifestError::MissingEntry);
        }
        let mut total_size = 0_u64;
        for (path, file) in &self.files {
            validate_relative_path(path)?;
            validate_blob(&file.blob)?;
            if file.size > MAX_FILE_SIZE || file.size > MAX_SAFE_JSON_INTEGER {
                return Err(StaticWebManifestError::ClosureLimit);
            }
            total_size = total_size
                .checked_add(file.size)
                .ok_or(StaticWebManifestError::ClosureLimit)?;
            if total_size > MAX_TOTAL_SIZE {
                return Err(StaticWebManifestError::ClosureLimit);
            }
            if !is_allowed_media_type(&file.media_type) {
                return Err(StaticWebManifestError::InvalidMediaType(
                    file.media_type.clone(),
                ));
            }
        }
        validate_security(&self.security)
    }

    /// RFC 8785 JCS bytes. No trailing newline is added.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, StaticWebManifestError> {
        self.validate()?;
        serde_jcs::to_vec(self)
            .map_err(|error| StaticWebManifestError::Canonicalization(error.to_string()))
    }
}

pub fn validate_relative_path(path: &str) -> Result<(), StaticWebManifestError> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains('\0')
        || path != path.nfc().collect::<String>()
        || path
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(StaticWebManifestError::InvalidPath(path.to_owned()));
    }
    Ok(())
}

pub fn is_allowed_media_type(media_type: &str) -> bool {
    matches!(
        media_type,
        "application/javascript; charset=utf-8"
            | "application/json; charset=utf-8"
            | "application/wasm"
            | "application/octet-stream"
            | "font/woff2"
            | "image/avif"
            | "image/gif"
            | "image/jpeg"
            | "image/png"
            | "image/svg+xml"
            | "image/webp"
            | "text/css; charset=utf-8"
            | "text/html; charset=utf-8"
            | "text/javascript; charset=utf-8"
            | "text/plain; charset=utf-8"
    )
}

pub fn validate_connect_source(value: &str) -> Result<(), StaticWebManifestError> {
    let parsed = Url::parse(value)
        .map_err(|_| StaticWebManifestError::InvalidConnectSource(value.to_owned()))?;
    if !matches!(parsed.scheme(), "https" | "wss")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.origin().ascii_serialization() != value
    {
        return Err(StaticWebManifestError::InvalidConnectSource(
            value.to_owned(),
        ));
    }
    let Some(host) = parsed.host_str() else {
        return Err(StaticWebManifestError::InvalidConnectSource(
            value.to_owned(),
        ));
    };
    let lower = host.to_ascii_lowercase();
    if host.parse::<IpAddr>().is_ok()
        || lower == "localhost"
        || lower.ends_with(".localhost")
        || lower.ends_with(".local")
        || lower.len() > 253
        || !lower.contains('.')
        || !lower.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
    {
        return Err(StaticWebManifestError::InvalidConnectSource(
            value.to_owned(),
        ));
    }
    Ok(())
}

fn validate_blob(blob: &str) -> Result<(), StaticWebManifestError> {
    let Some(hex) = blob.strip_prefix("sha256:") else {
        return Err(StaticWebManifestError::InvalidBlob);
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err(StaticWebManifestError::InvalidBlob);
    }
    Ok(())
}

fn validate_security(security: &StaticWebSecurityV1) -> Result<(), StaticWebManifestError> {
    let mut seen = std::collections::BTreeSet::new();
    for value in &security.connect_src {
        validate_connect_source(value)?;
        if !seen.insert(value) {
            return Err(StaticWebManifestError::DuplicateSecurityOrigin(
                value.clone(),
            ));
        }
    }
    if security.frame_ancestors.is_empty() {
        return Err(StaticWebManifestError::EmptyField(
            "security.frame_ancestors",
        ));
    }
    seen.clear();
    for value in &security.frame_ancestors {
        if !STATIC_WEB_FRAME_ANCESTORS_V1.contains(&value.as_str()) {
            return Err(StaticWebManifestError::InvalidFrameAncestor(value.clone()));
        }
        if !seen.insert(value) {
            return Err(StaticWebManifestError::DuplicateSecurityOrigin(
                value.clone(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest as _;

    #[test]
    fn golden_fixture_is_exact_jcs_and_has_expected_digest() {
        let input = include_str!("../../tests/fixtures/static-web-manifest-jcs-v1/input.json");
        let expected =
            include_str!("../../tests/fixtures/static-web-manifest-jcs-v1/canonical.json");
        let manifest: StaticWebManifestV1 = serde_json::from_str(input).unwrap();
        let canonical = manifest.canonical_bytes().unwrap();
        assert_eq!(canonical, expected.trim_end_matches('\n').as_bytes());
        assert_eq!(
            format!("sha256:{:x}", sha2::Sha256::digest(&canonical)),
            "sha256:8a06c71db0519bb27f2dc92f88dcd8107f09e8cb52a1495f16cb1bac6a177abd"
        );
    }

    #[test]
    fn rejects_non_public_connect_sources() {
        for value in [
            "https://localhost",
            "https://foo.local",
            "https://127.0.0.1",
            "https://[::1]",
            "https://api.example.com/path",
            "https://*.example.com",
            "http://api.example.com",
            "https://user@api.example.com",
        ] {
            assert!(validate_connect_source(value).is_err(), "{value}");
        }
    }

    #[test]
    fn rejects_unsafe_or_non_normalized_paths() {
        for path in [
            "",
            "/index.html",
            "assets\\app.js",
            "assets/../app.js",
            "assets//app.js",
            "./index.html",
            "cafe\u{301}.txt",
            "nul\0.txt",
        ] {
            assert!(validate_relative_path(path).is_err(), "{path:?}");
        }
    }
}
