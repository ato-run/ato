//! The receipts a source materialization produces.
//!
//! Two of them, because they answer different questions and have different
//! lifetimes:
//!
//! ```text
//! SourceReceiptV1                 WHAT was resolved   — identity evidence
//! SourceMaterializationReceiptV1  WHERE it was stored — an artifact locator
//! ```
//!
//! The split is load-bearing. `source_archive_digest` is a property of how the
//! bytes were packed, not of the source: two archives of one tree made by
//! different tar or zstd versions differ. Putting it in the identity receipt
//! would make the archiver's version part of the source's identity, which is
//! the same mistake ADR-015 §9.2 records for `mke2fs` and the guest image.
//!
//! # Canonical bytes are pinned, not described
//!
//! The digest is over exact bytes, and the verifier is TypeScript in another
//! repository. Calling the format "RFC 8785 compatible" would be a claim about
//! two implementations agreeing, which is not something a description can
//! establish — so the fixtures under `tests/fixtures/source_receipt/` record the
//! expected bytes and the expected digest, and both languages are tested
//! against those same files.
//!
//! The rules mirrored from `apps/ato-api` `canonicalJson`:
//!
//! * object keys sorted, ASCII here so byte order and UTF-16 code-unit order
//!   agree — the receipt's key set is closed and all-ASCII by construction;
//! * no insignificant whitespace;
//! * JSON string escaping as `JSON.stringify` produces it — `"` and `\`
//!   escaped, C0 controls as their short forms or `\u00xx` lowercase, every
//!   other scalar value emitted literally as UTF-8.
//!
//! # No domain separator
//!
//! Deliberately absent, and worth saying because the reflex here is the other
//! way: `schema_domained_blake3_id` (the execution-contract helper) prepends
//! `UTF8(schema) || 0x00`. This digest does not, because the merged verifier
//! does not. A producer that domain-separated "by convention" would fail every
//! report with a digest mismatch that looks like corruption.

use serde::{Deserialize, Serialize};

/// The wire value of the receipt's `schema` field.
///
/// The FIELD is named `schema`, not `schema_version`, because that is what the
/// merged verifier parses — and it refuses unknown fields, so a producer
/// emitting `schema_version` is rejected wholesale rather than partially read.
pub const SOURCE_RECEIPT_V1_SCHEMA: &str = "ato.source-receipt/v1";
/// The wire value of the materialization receipt's `schema` field.
pub const SOURCE_MATERIALIZATION_RECEIPT_V1_SCHEMA: &str = "ato.source-materialization-receipt/v1";

/// Evidence of WHAT source was resolved.
///
/// Field order in this struct is the canonical (sorted) order, so the
/// serializer below reads top to bottom and a field added out of order is
/// visible in review rather than only in a digest change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceReceiptV1 {
    pub canonical_repository: String,
    pub commit_algorithm: String,
    pub provider: String,
    pub resolved_commit_sha: String,
    pub resolver_contract_version: String,
    pub schema: String,
    pub source_tree_digest: String,
}

impl SourceReceiptV1 {
    /// The exact bytes the digest is taken over.
    pub fn canonical_json(&self) -> String {
        let fields: [(&str, &str); 7] = [
            ("canonical_repository", &self.canonical_repository),
            ("commit_algorithm", &self.commit_algorithm),
            ("provider", &self.provider),
            ("resolved_commit_sha", &self.resolved_commit_sha),
            ("resolver_contract_version", &self.resolver_contract_version),
            ("schema", &self.schema),
            ("source_tree_digest", &self.source_tree_digest),
        ];
        canonical_object(&fields)
    }

    /// `blake3:<hex>` over [`Self::canonical_json`]. No domain separator.
    pub fn digest(&self) -> String {
        blake3_label(self.canonical_json().as_bytes())
    }
}

/// WHERE a resolved source was stored, and as what.
///
/// Never part of source identity: see the module doc.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceMaterializationReceiptV1 {
    pub archive_format_version: String,
    pub object_key: String,
    pub schema: String,
    pub size_bytes: u64,
    pub source_archive_digest: String,
    /// The tie back to the identity receipt. The two must name one tree.
    pub source_tree_digest: String,
}

impl SourceMaterializationReceiptV1 {
    pub fn canonical_json(&self) -> String {
        let size = self.size_bytes.to_string();
        let fields: [(&str, &str); 6] = [
            ("archive_format_version", &self.archive_format_version),
            ("object_key", &self.object_key),
            ("schema", &self.schema),
            // Serialized as a JSON NUMBER, not a string — handled below.
            ("size_bytes", &size),
            ("source_archive_digest", &self.source_archive_digest),
            ("source_tree_digest", &self.source_tree_digest),
        ];
        let mut out = String::from("{");
        for (i, (key, value)) in fields.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&json_string(key));
            out.push(':');
            if *key == "size_bytes" {
                // A u64 has exactly one JSON spelling, and JS emits the same
                // for any value below 2^53. Values above that would diverge, so
                // an archive larger than 9 PiB is not representable here —
                // which is a bound worth having rather than a silent hazard.
                out.push_str(value);
            } else {
                out.push_str(&json_string(value));
            }
        }
        out.push('}');
        out
    }

    pub fn digest(&self) -> String {
        blake3_label(self.canonical_json().as_bytes())
    }
}

fn canonical_object(fields: &[(&str, &str)]) -> String {
    let mut out = String::from("{");
    for (i, (key, value)) in fields.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&json_string(key));
        out.push(':');
        out.push_str(&json_string(value));
    }
    out.push('}');
    out
}

/// A JSON string literal, byte-identical to what `JSON.stringify` produces.
///
/// `serde_json` would also do this, but going through it means the format is
/// whatever that crate does this version. Writing it out means the rule is
/// visible next to the fixtures that pin it.
fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // C0 controls with no short form. Lowercase hex, as JS emits.
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            // Everything else literal, including non-ASCII — JSON.stringify
            // does not escape it either, and the bytes are UTF-8 on both sides.
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn blake3_label(bytes: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(bytes);
    format!("blake3:{}", hex::encode(hasher.finalize().as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt() -> SourceReceiptV1 {
        SourceReceiptV1 {
            canonical_repository: "https://github.com/acme/menuflow".to_string(),
            commit_algorithm: "sha1".to_string(),
            provider: "github".to_string(),
            resolved_commit_sha: "a".repeat(40),
            resolver_contract_version: "ato.capsule-program-source-projection/v1".to_string(),
            schema: SOURCE_RECEIPT_V1_SCHEMA.to_string(),
            source_tree_digest: format!("sha256:{}", "1".repeat(64)),
        }
    }

    /// The canonical key order IS sorted. Asserted rather than assumed, because
    /// the serializer reads the array top to bottom and a mis-ordered field
    /// would silently produce bytes the verifier disagrees with.
    #[test]
    fn the_canonical_key_order_is_sorted() {
        let json = receipt().canonical_json();
        let keys: Vec<&str> = json
            .split(',')
            .filter_map(|part| part.split(':').next())
            .map(|k| k.trim_start_matches('{').trim_matches('"'))
            .collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted, "canonical JSON keys must be sorted: {json}");
    }

    #[test]
    fn the_canonical_form_has_no_insignificant_whitespace() {
        let json = receipt().canonical_json();
        assert!(!json.contains(' '), "{json}");
        assert!(!json.contains('\n'));
    }

    /// Escaping matches `JSON.stringify`, including the lowercase `\\u00xx`
    /// form for a control character with no short spelling.
    #[test]
    fn string_escaping_matches_json_stringify() {
        assert_eq!(json_string("plain"), "\"plain\"");
        assert_eq!(json_string("a\"b"), "\"a\\\"b\"");
        assert_eq!(json_string("a\\b"), "\"a\\\\b\"");
        assert_eq!(json_string("a\nb"), "\"a\\nb\"");
        assert_eq!(json_string("a\tb"), "\"a\\tb\"");
        assert_eq!(json_string("a\u{1}b"), "\"a\\u0001b\"");
        // Non-ASCII is NOT escaped — JSON.stringify leaves it literal too.
        assert_eq!(json_string("café"), "\"café\"");
        assert_eq!(json_string("日本語"), "\"日本語\"");
    }

    /// The digest is over the canonical bytes with NO domain separator.
    ///
    /// Pinned explicitly because the repo's other identity helper
    /// (`schema_domained_blake3_id`) DOES prepend one, and matching the wrong
    /// habit here fails every report with what looks like corruption.
    #[test]
    fn the_digest_has_no_domain_separator() {
        let r = receipt();
        let mut plain = blake3::Hasher::new();
        plain.update(r.canonical_json().as_bytes());
        assert_eq!(
            r.digest(),
            format!("blake3:{}", hex::encode(plain.finalize().as_bytes()))
        );

        let mut domained = blake3::Hasher::new();
        domained.update(SOURCE_RECEIPT_V1_SCHEMA.as_bytes());
        domained.update(&[0]);
        domained.update(r.canonical_json().as_bytes());
        assert_ne!(
            r.digest(),
            format!("blake3:{}", hex::encode(domained.finalize().as_bytes())),
            "a domain-separated digest must not be mistaken for this one"
        );
    }

    /// Equivalent JSON spellings converge: the struct is the input, so key
    /// order in any source document cannot reach the digest.
    #[test]
    fn deserializing_any_key_order_yields_one_digest() {
        let a: SourceReceiptV1 = serde_json::from_str(
            r#"{"schema":"ato.source-receipt/v1","provider":"github","canonical_repository":"https://github.com/acme/menuflow","commit_algorithm":"sha1","resolved_commit_sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","source_tree_digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","resolver_contract_version":"ato.capsule-program-source-projection/v1"}"#,
        )
        .unwrap();
        let b: SourceReceiptV1 = serde_json::from_str(
            r#"{"source_tree_digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","resolver_contract_version":"ato.capsule-program-source-projection/v1","resolved_commit_sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","provider":"github","commit_algorithm":"sha1","canonical_repository":"https://github.com/acme/menuflow","schema":"ato.source-receipt/v1"}"#,
        )
        .unwrap();
        assert_eq!(a.digest(), b.digest());
        assert_eq!(a.digest(), receipt().digest());
    }

    /// `size_bytes` is a JSON number, not a string.
    #[test]
    fn the_materialization_receipt_serializes_size_as_a_number() {
        let m = SourceMaterializationReceiptV1 {
            archive_format_version: "ato.source-archive/v1".to_string(),
            object_key: "source-archives/blake3/abc".to_string(),
            schema: SOURCE_MATERIALIZATION_RECEIPT_V1_SCHEMA.to_string(),
            size_bytes: 1234,
            source_archive_digest: format!("blake3:{}", "2".repeat(64)),
            source_tree_digest: format!("sha256:{}", "1".repeat(64)),
        };
        let json = m.canonical_json();
        assert!(json.contains("\"size_bytes\":1234"), "{json}");
        assert!(!json.contains("\"1234\""), "{json}");
    }

    /// The archive digest is NOT in the identity receipt.
    ///
    /// Two archives of one tree differ by tar/zstd version, so including it
    /// would make the archiver's version part of the source's identity.
    #[test]
    fn the_identity_receipt_carries_no_archive_information() {
        let json = receipt().canonical_json();
        assert!(!json.contains("archive"), "{json}");
        assert!(!json.contains("object_key"), "{json}");
        assert!(!json.contains("size_bytes"), "{json}");
    }
}
