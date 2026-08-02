//! Layer 2: the immutable `ato.static-web-manifest/v1` delivery contract.
//!
//! The manifest deliberately contains only immutable output facts. Mutable
//! host assignment, delivery state, headers, and deployment credentials belong
//! to the data plane and are never part of this content-addressed payload.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use unicode_normalization::UnicodeNormalization;
use url::Url;

/// Schema tag shared with the static-content Worker.
pub const STATIC_WEB_MANIFEST_V1_SCHEMA: &str = "ato.static-web-manifest/v1";

/// The only embedding origins v1 recognizes, in the only permitted order.
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
    pub fn producer_policy(mut connect_src: Vec<String>) -> Result<Self, StaticWebManifestError> {
        canonicalize_connect_sources(&mut connect_src)?;
        Ok(Self {
            connect_src,
            frame_ancestors: STATIC_WEB_FRAME_ANCESTORS_V1
                .iter()
                .map(|origin| (*origin).to_owned())
                .collect(),
        })
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
        canonical_jcs_bytes(self)
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

/// Validates a policy set and converts it to its sole v1 representation.
pub fn canonicalize_connect_sources(
    connect_src: &mut Vec<String>,
) -> Result<(), StaticWebManifestError> {
    for value in connect_src.iter() {
        validate_connect_source(value)?;
    }
    connect_src.sort_unstable();
    if let Some(pair) = connect_src.windows(2).find(|pair| pair[0] == pair[1]) {
        return Err(StaticWebManifestError::DuplicateSecurityOrigin(
            pair[0].clone(),
        ));
    }
    Ok(())
}

/// Canonical RFC 8785 bytes for the static web contracts.
///
/// `serde_jcs` is used elsewhere in this workspace, but its v0.1 object-key
/// comparator is UTF-8 byte ordered. RFC 8785 requires UTF-16 code-unit order;
/// this bounded encoder is used here so astral Unicode keys match the Worker
/// `canonicalize` implementation exactly. Static v1 has only strings,
/// booleans, safe non-negative integers, arrays, and objects.
pub fn canonical_jcs_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, StaticWebManifestError> {
    let value = serde_json::to_value(value)
        .map_err(|error| StaticWebManifestError::Canonicalization(error.to_string()))?;
    let mut output = Vec::new();
    write_jcs_value(&value, &mut output)?;
    Ok(output)
}

fn write_jcs_value(
    value: &serde_json::Value,
    output: &mut Vec<u8>,
) -> Result<(), StaticWebManifestError> {
    match value {
        serde_json::Value::Null => output.extend_from_slice(b"null"),
        serde_json::Value::Bool(value) => {
            output.extend_from_slice(if *value { b"true" } else { b"false" })
        }
        serde_json::Value::Number(number) if number.as_u64().is_some() => {
            output.extend_from_slice(number.to_string().as_bytes())
        }
        serde_json::Value::Number(_) => {
            return Err(StaticWebManifestError::Canonicalization(
                "static web JCS accepts only non-negative integer values".to_owned(),
            ));
        }
        serde_json::Value::String(value) => {
            output.extend_from_slice(
                serde_json::to_string(value)
                    .map_err(|error| StaticWebManifestError::Canonicalization(error.to_string()))?
                    .as_bytes(),
            );
        }
        serde_json::Value::Array(values) => {
            output.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                write_jcs_value(value, output)?;
            }
            output.push(b']');
        }
        serde_json::Value::Object(values) => {
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_unstable_by(|(left, _), (right, _)| compare_utf16(left, right));
            output.push(b'{');
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                output.extend_from_slice(
                    serde_json::to_string(key)
                        .map_err(|error| {
                            StaticWebManifestError::Canonicalization(error.to_string())
                        })?
                        .as_bytes(),
                );
                output.push(b':');
                write_jcs_value(value, output)?;
            }
            output.push(b'}');
        }
    }
    Ok(())
}

fn compare_utf16(left: &str, right: &str) -> Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
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
    let mut canonical = security.connect_src.clone();
    canonicalize_connect_sources(&mut canonical)?;
    if canonical != security.connect_src {
        return Err(StaticWebManifestError::Canonicalization(
            "security.connect_src must be sorted in ASCII dictionary order".to_owned(),
        ));
    }
    let expected = STATIC_WEB_FRAME_ANCESTORS_V1
        .iter()
        .map(|origin| (*origin).to_owned())
        .collect::<Vec<_>>();
    if security.frame_ancestors != expected {
        return Err(StaticWebManifestError::InvalidFrameAncestor(
            "frame_ancestors must equal the fixed v1 trust set".to_owned(),
        ));
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
            "sha256:6d77d3da709a578e6d58f50d4b8f8cf5c54e2178200821769afb03449c8e6ba2"
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

    #[test]
    fn requires_canonical_connect_order_and_fixed_frame_ancestors() {
        let mut manifest: StaticWebManifestV1 = serde_json::from_str(include_str!(
            "../../tests/fixtures/static-web-manifest-jcs-v1/input.json"
        ))
        .unwrap();
        manifest.security.connect_src.reverse();
        assert!(manifest.validate().is_err());
        manifest.security.connect_src.reverse();
        manifest.security.frame_ancestors.pop();
        assert!(manifest.validate().is_err());
    }
}
