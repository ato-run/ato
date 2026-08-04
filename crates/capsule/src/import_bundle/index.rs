//! `index.json` — the `ato.capsule-index/v1` member manifest.
//!
//! This is the signing target of a v3 bundle, so its parser is deliberately the
//! strictest thing in the module: unknown fields, duplicate JSON object keys,
//! non-canonical digests, numeric sizes, out-of-order members, and bytes that
//! are not the exact JCS (RFC 8785) encoding of their own content are all
//! rejections rather than normalizations.
//!
//! The duplicate-key rule is the load-bearing one. A generic map parser that
//! keeps the last occurrence of a repeated key lets an attacker make the signed
//! bytes say one thing while a lenient reader sees another, which is exactly the
//! shape [`parse_index_json`] refuses (see [`reject_duplicate_json_keys`]).

use std::collections::BTreeSet;
use std::fmt;

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

use super::CapsuleImportError;

/// The only `schema` value an `index.json` may carry in v1.
pub const INDEX_SCHEMA: &str = "ato.capsule-index/v1";

/// The exact `path` the `manifest` role member must declare.
pub const MANIFEST_MEMBER_PATH: &str = "capsule.toml";

/// The exact `path` the `source` role member must declare.
pub const SOURCE_MEMBER_PATH: &str = "source.tar.zst";

/// The exact `media_type` the `manifest` role member must declare.
pub const MANIFEST_MEDIA_TYPE: &str = "application/toml";

/// The exact `media_type` the `source` role member must declare.
pub const SOURCE_MEDIA_TYPE: &str = "application/vnd.ato.source-archive.v1+zstd";

// ─────────────────────────────────────────────────────────────────────────────
// Sha256Digest
// ─────────────────────────────────────────────────────────────────────────────

/// A SHA-256 digest in the format's one canonical spelling:
/// `sha256:` + exactly 64 **lowercase** hex characters.
///
/// Parsing is total and case-sensitive on purpose: accepting `SHA256:` or
/// uppercase hex would give one logical digest two byte spellings, and two
/// spellings inside a JCS signing target are two different signatures.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sha256Digest([u8; 32]);

impl Sha256Digest {
    /// Hash `bytes`.
    #[must_use]
    pub fn of_bytes(bytes: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        Self(hasher.finalize().into())
    }

    /// Wrap a finished 32-byte digest.
    #[must_use]
    pub fn from_raw(raw: [u8; 32]) -> Self {
        Self(raw)
    }

    /// Parse the labelled `sha256:<64 lowercase hex>` form.
    ///
    /// # Errors
    ///
    /// Returns a human-readable reason when the prefix, length, or hex casing
    /// deviates from the single canonical spelling.
    pub fn parse(labelled: &str) -> Result<Self, String> {
        let hex_part = labelled
            .strip_prefix("sha256:")
            .ok_or_else(|| format!("digest {labelled:?} must start with `sha256:`"))?;
        if hex_part.len() != 64 {
            return Err(format!(
                "digest {labelled:?} must carry exactly 64 hex characters, got {}",
                hex_part.len()
            ));
        }
        if !hex_part
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(format!(
                "digest {labelled:?} must be lowercase hex ([0-9a-f]) only"
            ));
        }
        let mut raw = [0u8; 32];
        for (index, slot) in raw.iter_mut().enumerate() {
            let byte = &hex_part[index * 2..index * 2 + 2];
            *slot = u8::from_str_radix(byte, 16)
                .map_err(|source| format!("digest {labelled:?} is not valid hex: {source}"))?;
        }
        Ok(Self(raw))
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("sha256:")?;
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self}")
    }
}

impl Serialize for Sha256Digest {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Sha256Digest::parse(&raw).map_err(de::Error::custom)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SizeBytes
// ─────────────────────────────────────────────────────────────────────────────

/// A member size as the format spells it: a **decimal string**, never a JSON
/// number.
///
/// RFC §`index.json`: the format sets no upper bound on member size, and a
/// JavaScript `number` cannot exactly represent every `u64` — so a large value
/// could round-trip differently in the Rust writer and the TypeScript verifier,
/// producing two different JCS canonicalizations of the same logical
/// `index.json` and silently breaking the signature target.
///
/// The value is therefore never parsed into an integer, not for comparison and
/// not for allocation. Verification compares
/// `declared.as_str() == actual.to_string()`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SizeBytes(String);

impl SizeBytes {
    /// Build the canonical decimal spelling of a measured size.
    #[must_use]
    pub fn of_measured(bytes: u64) -> Self {
        Self(bytes.to_string())
    }

    /// Validate the decimal-string grammar: ASCII digits only, no sign, no
    /// decimal point, no exponent, and no leading zero except the literal
    /// `"0"`.
    ///
    /// # Errors
    ///
    /// Returns a human-readable reason on any deviation.
    pub fn parse(raw: &str) -> Result<Self, String> {
        if raw.is_empty() {
            return Err("size_bytes must not be empty".to_string());
        }
        if !raw.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(format!(
                "size_bytes {raw:?} must be ASCII decimal digits only \
                 (no sign, decimal point, or exponent)"
            ));
        }
        if raw.len() > 1 && raw.starts_with('0') {
            return Err(format!(
                "size_bytes {raw:?} has a leading zero; only the literal \"0\" may start with 0"
            ));
        }
        Ok(Self(raw.to_string()))
    }

    /// The declared spelling, exactly as it appeared.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for SizeBytes {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SizeBytes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        SizeBytes::parse(&raw).map_err(de::Error::custom)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// MemberRole
// ─────────────────────────────────────────────────────────────────────────────

/// The role slot a member fills.
///
/// [`MemberRole::Lock`] exists so the parser can say *why* a `role: "lock"`
/// bundle is refused (v1 carries no lock — the local resolver generates one
/// after import) rather than collapsing it into "unknown role".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MemberRole {
    /// The outer, authoritative `capsule.toml`.
    Manifest,
    /// The `ato.source-archive/v1` payload.
    Source,
    /// Never valid in v1.
    Lock,
}

impl MemberRole {
    /// The wire spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manifest => "manifest",
            Self::Source => "source",
            Self::Lock => "lock",
        }
    }
}

impl Serialize for MemberRole {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for MemberRole {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        match raw.as_str() {
            "manifest" => Ok(Self::Manifest),
            "source" => Ok(Self::Source),
            "lock" => Ok(Self::Lock),
            other => Err(de::Error::custom(format!(
                "unknown member role {other:?}; v1 defines only \"manifest\" and \"source\""
            ))),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The document
// ─────────────────────────────────────────────────────────────────────────────

/// One entry of `index.json`'s `members` array.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexMember {
    /// Which slot this member fills.
    pub role: MemberRole,
    /// The outer TAR member path, which v1 fixes per role.
    pub path: String,
    /// The exact media type v1 fixes per role.
    pub media_type: String,
    /// The digest of the member's bytes.
    pub sha256: Sha256Digest,
    /// The member's size, as a decimal string.
    pub size_bytes: SizeBytes,
}

/// A parsed, fully validated `ato.capsule-index/v1` document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapsuleIndexV1 {
    /// Must be [`INDEX_SCHEMA`].
    pub schema: String,
    /// Ascending UTF-8 byte order of `path`; exactly one `manifest` and one
    /// `source`.
    pub members: Vec<IndexMember>,
}

impl CapsuleIndexV1 {
    /// The `manifest` role member. Present by construction after
    /// [`parse_index_json`].
    #[must_use]
    pub fn manifest_member(&self) -> &IndexMember {
        self.members
            .iter()
            .find(|member| member.role == MemberRole::Manifest)
            .expect("validated index always carries exactly one manifest member")
    }

    /// The `source` role member. Present by construction after
    /// [`parse_index_json`].
    #[must_use]
    pub fn source_member(&self) -> &IndexMember {
        self.members
            .iter()
            .find(|member| member.role == MemberRole::Source)
            .expect("validated index always carries exactly one source member")
    }

    /// The JCS (RFC 8785) canonical encoding of this document — the bytes that
    /// must appear on disk and the bytes the signature covers.
    ///
    /// # Errors
    ///
    /// Returns [`CapsuleImportError::CapsuleInvalid`] if canonicalization fails,
    /// which for this always-finite, always-string-valued shape cannot happen in
    /// practice but is not worth an `unwrap`.
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, CapsuleImportError> {
        serde_jcs::to_vec(self).map_err(|source| {
            CapsuleImportError::invalid(format!("failed to canonicalize index.json: {source}"))
        })
    }
}

/// Parse and fully validate `index.json` bytes.
///
/// Order matters and is fixed: duplicate-key rejection runs over the raw bytes
/// **before** typed parsing, so a duplicate key can never be silently resolved
/// by whichever occurrence serde happens to keep. The JCS self-consistency check
/// runs last, over the parsed content, so the returned document provably
/// re-serializes to exactly the bytes that were handed in — which is what makes
/// those bytes a well-defined signing target.
///
/// # Errors
///
/// [`CapsuleImportError::CapsuleInvalid`] with the specific reason.
pub fn parse_index_json(bytes: &[u8]) -> Result<CapsuleIndexV1, CapsuleImportError> {
    reject_duplicate_json_keys(bytes).map_err(|reason| {
        CapsuleImportError::invalid(format!("index.json is not strictly parseable: {reason}"))
    })?;

    let index: CapsuleIndexV1 = serde_json::from_slice(bytes).map_err(|source| {
        CapsuleImportError::invalid(format!(
            "index.json does not match ato.capsule-index/v1: {source}"
        ))
    })?;

    if index.schema != INDEX_SCHEMA {
        return Err(CapsuleImportError::invalid(format!(
            "index.json schema is {:?}; expected {INDEX_SCHEMA:?}",
            index.schema
        )));
    }

    validate_member_set(&index)?;
    validate_member_order(&index)?;

    let canonical = index.to_canonical_bytes()?;
    if canonical != bytes {
        return Err(CapsuleImportError::invalid(
            "index.json bytes are not the exact JCS (RFC 8785) canonicalization of their own \
             content; the signing target is undefined for such a bundle",
        ));
    }

    Ok(index)
}

fn validate_member_set(index: &CapsuleIndexV1) -> Result<(), CapsuleImportError> {
    let mut manifest_count = 0usize;
    let mut source_count = 0usize;
    let mut seen_paths: BTreeSet<&str> = BTreeSet::new();

    for member in &index.members {
        if !seen_paths.insert(member.path.as_str()) {
            return Err(CapsuleImportError::invalid(format!(
                "index.json declares member path {:?} more than once",
                member.path
            )));
        }
        match member.role {
            MemberRole::Lock => {
                return Err(CapsuleImportError::invalid(
                    "index.json declares a `lock` role member; a v1 source-only bundle carries no \
                     lock (the local resolver generates capsule.lock after import)",
                ));
            }
            MemberRole::Manifest => {
                manifest_count += 1;
                require_exact(&member.path, MANIFEST_MEMBER_PATH, "manifest", "path")?;
                require_exact(
                    &member.media_type,
                    MANIFEST_MEDIA_TYPE,
                    "manifest",
                    "media_type",
                )?;
            }
            MemberRole::Source => {
                source_count += 1;
                require_exact(&member.path, SOURCE_MEMBER_PATH, "source", "path")?;
                require_exact(
                    &member.media_type,
                    SOURCE_MEDIA_TYPE,
                    "source",
                    "media_type",
                )?;
            }
        }
    }

    if manifest_count != 1 {
        return Err(CapsuleImportError::invalid(format!(
            "index.json must declare exactly 1 `manifest` role member, found {manifest_count}"
        )));
    }
    if source_count != 1 {
        return Err(CapsuleImportError::invalid(format!(
            "index.json must declare exactly 1 `source` role member, found {source_count}"
        )));
    }
    Ok(())
}

fn require_exact(
    actual: &str,
    expected: &str,
    role: &str,
    field: &str,
) -> Result<(), CapsuleImportError> {
    if actual == expected {
        return Ok(());
    }
    Err(CapsuleImportError::invalid(format!(
        "index.json `{role}` member {field} is {actual:?}; v1 fixes it to {expected:?}"
    )))
}

/// RFC §`index.json`: `members` is ordered by ascending UTF-8 **byte** order of
/// `path`. JCS canonicalizes object key order but does not reorder arrays, so
/// without this rule two writers could emit two different signing targets for
/// the same logical bundle. Out of order is a rejection, never a silent re-sort.
fn validate_member_order(index: &CapsuleIndexV1) -> Result<(), CapsuleImportError> {
    for pair in index.members.windows(2) {
        if pair[0].path.as_bytes() >= pair[1].path.as_bytes() {
            return Err(CapsuleImportError::invalid(format!(
                "index.json members are not in ascending UTF-8 byte order of `path`: {:?} \
                 precedes {:?}",
                pair[0].path, pair[1].path
            )));
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Duplicate JSON object keys
// ─────────────────────────────────────────────────────────────────────────────

/// Reject a JSON document that repeats an object key **at any depth**.
///
/// It leans on `serde_json`'s own tokenizer for correctness (string escapes,
/// numbers, nesting) but replaces its map handling with a visitor that records
/// every key it has seen in the current object. `serde_derive`'s
/// `duplicate field` check would catch repeats of *known* struct fields on its
/// own; this runs first and independently so the rule holds for the whole
/// document rather than as a side effect of how the target struct is shaped.
///
/// # Errors
///
/// Returns the duplicated key, or the JSON syntax error that stopped the walk.
pub(crate) fn reject_duplicate_json_keys(bytes: &[u8]) -> Result<(), String> {
    serde_json::from_slice::<DuplicateKeyProbe>(bytes)
        .map(|_| ())
        .map_err(|source| source.to_string())
}

struct DuplicateKeyProbe;

impl<'de> Deserialize<'de> for DuplicateKeyProbe {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(DuplicateKeyProbeVisitor)
    }
}

struct DuplicateKeyProbeVisitor;

impl<'de> Visitor<'de> for DuplicateKeyProbeVisitor {
    type Value = DuplicateKeyProbe;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("any JSON value with no repeated object key")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key.clone()) {
                return Err(de::Error::custom(format!(
                    "duplicate JSON object key {key:?}"
                )));
            }
            map.next_value::<DuplicateKeyProbe>()?;
        }
        Ok(DuplicateKeyProbe)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        while seq.next_element::<DuplicateKeyProbe>()?.is_some() {}
        Ok(DuplicateKeyProbe)
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
        Ok(DuplicateKeyProbe)
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
        Ok(DuplicateKeyProbe)
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
        Ok(DuplicateKeyProbe)
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
        Ok(DuplicateKeyProbe)
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
        Ok(DuplicateKeyProbe)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(DuplicateKeyProbe)
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(DuplicateKeyProbe)
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_any(DuplicateKeyProbeVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_parse_rejects_uppercase_and_bad_prefix() {
        assert!(Sha256Digest::parse(&format!("sha256:{}", "A".repeat(64))).is_err());
        assert!(Sha256Digest::parse(&format!("SHA256:{}", "a".repeat(64))).is_err());
        assert!(Sha256Digest::parse(&"a".repeat(64)).is_err());
        assert!(Sha256Digest::parse(&format!("sha256:{}", "a".repeat(63))).is_err());
        assert!(Sha256Digest::parse(&format!("sha256:{}", "a".repeat(64))).is_ok());
    }

    #[test]
    fn digest_round_trips_through_display() {
        let digest = Sha256Digest::of_bytes(b"hello");
        let parsed = Sha256Digest::parse(&digest.to_string()).expect("canonical digest re-parses");
        assert_eq!(digest, parsed);
    }

    #[test]
    fn size_bytes_grammar() {
        assert!(SizeBytes::parse("0").is_ok());
        assert!(SizeBytes::parse("1234").is_ok());
        assert!(
            SizeBytes::parse("18446744073709551616").is_ok(),
            "beyond u64 is still a valid string"
        );
        assert!(SizeBytes::parse("").is_err());
        assert!(SizeBytes::parse("01").is_err());
        assert!(SizeBytes::parse("-1").is_err());
        assert!(SizeBytes::parse("1.0").is_err());
        assert!(SizeBytes::parse("1e3").is_err());
        assert!(SizeBytes::parse("+1").is_err());
    }

    #[test]
    fn duplicate_keys_are_detected_at_every_depth() {
        assert!(reject_duplicate_json_keys(br#"{"a":1,"a":2}"#).is_err());
        assert!(reject_duplicate_json_keys(br#"{"a":[{"b":1,"b":2}]}"#).is_err());
        assert!(reject_duplicate_json_keys(br#"{"a":{"b":{"c":1,"c":2}}}"#).is_err());
        assert!(reject_duplicate_json_keys(br#"{"a":1,"b":2}"#).is_ok());
        assert!(reject_duplicate_json_keys(br#"{"a":[{"b":1},{"b":2}]}"#).is_ok());
    }
}
