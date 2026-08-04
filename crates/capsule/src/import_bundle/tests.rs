//! The v3 import-bundle test matrix, and the generator for the committed golden
//! vectors under `crates/capsule/tests/capsule_format_v3/`.
//!
//! These live in the library rather than in `tests/` because several assertions
//! need crate-internal visibility — most importantly the staging-root accessor
//! that lets [`staging_is_removed_when_derivation_fails`] *prove* cleanup rather
//! than assume it. The committed vectors are additionally replayed through the
//! public API only, by `tests/capsule_format_v3_vectors.rs`, which is the shape
//! the later ato-api-side TypeScript conformance suite will mirror.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signer as _, SigningKey};
use serde_json::{Value, json};
use tar::{Builder, EntryType, Header};
use tempfile::TempDir;

use super::*;
use crate::blob::materialize_source_archive;

// ─────────────────────────────────────────────────────────────────────────────
// Fixtures
// ─────────────────────────────────────────────────────────────────────────────

/// A minimal, strictly valid `capsule.toml` — the same shape the existing
/// `capsule_program_contract` source-suite vectors use, so this suite exercises
/// a manifest the real loader already accepts.
const OUTER_MANIFEST: &str = "\
schema_version = \"0.3\"
name = \"v3-import-demo\"
version = \"0.1.0\"
type = \"app\"
default_target = \"app\"

[targets.app]
runtime = \"oci\"
image = \"ghcr.io/example/app:1\"
port = 8080
";

/// A different manifest, used to prove an inner one has no effect on identity.
const INNER_MANIFEST_DIFFERENT: &str = "\
schema_version = \"0.3\"
name = \"totally-different-name\"
version = \"9.9.9\"
type = \"app\"
default_target = \"app\"

[targets.app]
runtime = \"oci\"
image = \"ghcr.io/example/other:2\"
port = 9999
";

/// The ordinary source files every vector's archive carries.
const SOURCE_FILES: [(&str, &str); 2] = [
    ("app.py", "print(\"hello from v3\")\n"),
    ("lib/util.py", "VALUE = 41\n"),
];

/// A deterministic signer, so the committed golden vectors are fixed bytes.
///
/// Deliberately not [`EphemeralLocalSigner`]: fixed vectors need a fixed key,
/// and a fixed key is exactly what the production local-export path must never
/// have.
struct FixedKeySigner {
    signing_key: SigningKey,
    key_id: DidKey,
}

impl FixedKeySigner {
    fn from_seed(seed: u8) -> Self {
        let signing_key = SigningKey::from_bytes(&[seed; 32]);
        let key_id = DidKey::from_public_key(&signing_key.verifying_key().to_bytes());
        Self {
            signing_key,
            key_id,
        }
    }
}

impl CapsuleIndexSigner for FixedKeySigner {
    fn key_id(&self) -> &DidKey {
        &self.key_id
    }

    fn sign(&self, message: &[u8]) -> Result<[u8; 64], String> {
        Ok(self.signing_key.sign(message).to_bytes())
    }
}

fn signer() -> FixedKeySigner {
    FixedKeySigner::from_seed(0x11)
}

fn local_context() -> CapsuleImportContext {
    CapsuleImportContext::LocalFile {
        expected_bundle_digest: None,
    }
}

fn permissive_local_policy() -> CapsuleTrustPolicy {
    CapsuleTrustPolicy::new().accepting_untrusted_local_signers()
}

fn unbounded() -> CapsuleImportPolicy {
    CapsuleImportPolicy::unbounded()
}

/// Build an `ato.source-archive/v1` `.tar.zst` over `files`, returning its bytes.
///
/// Uses the existing materializer rather than a hand-rolled archiver, so every
/// vector's source member is genuinely the encoding the format reuses.
fn source_archive(files: &[(&str, &str)]) -> Vec<u8> {
    let tree = TempDir::new().expect("source tree");
    for (path, contents) in files {
        let target = tree.path().join(path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(&target, contents).expect("write source file");
    }
    let out = TempDir::new().expect("archive dir");
    let archive = out.path().join("source.tar.zst");
    materialize_source_archive(tree.path(), &archive).expect("materialize source archive");
    fs::read(&archive).expect("read source archive")
}

fn baseline_source_archive() -> Vec<u8> {
    source_archive(&SOURCE_FILES)
}

/// A source archive carrying the ordinary files plus extra root entries.
fn source_archive_with(extra: &[(&str, &str)]) -> Vec<u8> {
    let mut files: Vec<(&str, &str)> = SOURCE_FILES.to_vec();
    files.extend_from_slice(extra);
    source_archive(&files)
}

fn bundle_from(manifest: &str, source: &[u8]) -> Vec<u8> {
    write_capsule_bundle_v3(
        &CapsuleBundleWriteInput {
            manifest_bytes: manifest.as_bytes(),
            source_archive_bytes: source,
            claimed_issuer: ClaimedIssuer::LocalAuthor,
        },
        &signer(),
    )
    .expect("write v3 bundle")
}

fn baseline_bundle() -> Vec<u8> {
    bundle_from(OUTER_MANIFEST, &baseline_source_archive())
}

fn verify_local(bytes: &[u8]) -> Result<VerifiedCapsuleEnvelope, CapsuleImportError> {
    verify_capsule_envelope(
        Cursor::new(bytes.to_vec()),
        local_context(),
        &permissive_local_policy(),
        &unbounded(),
    )
}

fn import_local(bytes: &[u8]) -> Result<VerifiedCapsuleImport, CapsuleImportError> {
    derive_imported_capsule(verify_local(bytes)?)
}

// ─────────────────────────────────────────────────────────────────────────────
// Raw TAR surgery, for the malformed vectors
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct RawMember {
    name: Vec<u8>,
    bytes: Vec<u8>,
    entry_type: EntryType,
    link_name: Option<Vec<u8>>,
}

impl RawMember {
    fn file(name: &str, bytes: Vec<u8>) -> Self {
        Self {
            name: name.as_bytes().to_vec(),
            bytes,
            entry_type: EntryType::Regular,
            link_name: None,
        }
    }

    fn raw_named(name: &[u8], bytes: Vec<u8>) -> Self {
        Self {
            name: name.to_vec(),
            bytes,
            entry_type: EntryType::Regular,
            link_name: None,
        }
    }

    fn special(name: &str, entry_type: EntryType, link_name: Option<&str>) -> Self {
        Self {
            name: name.as_bytes().to_vec(),
            bytes: Vec::new(),
            entry_type,
            link_name: link_name.map(|value| value.as_bytes().to_vec()),
        }
    }
}

/// Assemble an outer TAR from raw member descriptions.
///
/// Header name and link-name fields are written as raw bytes rather than through
/// `Header::set_path`, because several vectors must carry names (`../index.json`,
/// `/capsule.toml`) that a well-behaved writer API refuses to produce — which is
/// precisely why the reader has to refuse to consume them.
fn assemble(members: &[RawMember]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut builder = Builder::new(Cursor::new(&mut out));
        for member in members {
            let mut header = Header::new_gnu();
            header.set_entry_type(member.entry_type);
            header.set_size(if member.entry_type == EntryType::Regular {
                member.bytes.len() as u64
            } else {
                0
            });
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_uid(0);
            header.set_gid(0);
            write_field(header.as_mut_bytes(), 0, 100, &member.name);
            if let Some(link) = member.link_name.as_ref() {
                write_field(header.as_mut_bytes(), 157, 100, link);
            }
            header.set_cksum();
            builder
                .append(&header, member.bytes.as_slice())
                .expect("append raw member");
        }
        builder.finish().expect("finish raw tar");
    }
    out
}

fn write_field(block: &mut [u8], offset: usize, len: usize, value: &[u8]) {
    assert!(value.len() <= len, "raw TAR field overflow");
    for slot in block[offset..offset + len].iter_mut() {
        *slot = 0;
    }
    block[offset..offset + value.len()].copy_from_slice(value);
}

/// Produce a canonical `signature.json` over arbitrary index bytes.
///
/// Signing whatever index the vector actually carries is what makes an
/// index-focused vector attributable: the rejection is the index's fault, never
/// a signature that happens not to match.
fn sign_index_bytes(index_bytes: &[u8], issuer: ClaimedIssuer) -> Vec<u8> {
    let signer = signer();
    let raw = signer.sign(&signing_message(index_bytes)).expect("sign");
    let signature = CapsuleIndexSignatureV1 {
        schema: SIGNATURE_SCHEMA.to_string(),
        algorithm: "ed25519".to_string(),
        key_id: signer.key_id().clone(),
        claimed_issuer: issuer,
        index_digest: Sha256Digest::of_bytes(index_bytes),
        signature: Ed25519SignatureBytes::from_raw(raw),
    };
    signature.to_canonical_bytes().expect("canonical signature")
}

/// The canonical index JSON for a manifest/source pair, as a mutable `Value`.
fn index_value(manifest: &[u8], source: &[u8]) -> Value {
    json!({
        "schema": INDEX_SCHEMA,
        "members": [
            {
                "role": "manifest",
                "path": MANIFEST_MEMBER_PATH,
                "media_type": MANIFEST_MEDIA_TYPE,
                "sha256": Sha256Digest::of_bytes(manifest).to_string(),
                "size_bytes": manifest.len().to_string(),
            },
            {
                "role": "source",
                "path": SOURCE_MEMBER_PATH,
                "media_type": SOURCE_MEDIA_TYPE,
                "sha256": Sha256Digest::of_bytes(source).to_string(),
                "size_bytes": source.len().to_string(),
            },
        ]
    })
}

/// `serde_json::Map` is a `BTreeMap` here (the `preserve_order` feature is off),
/// so a plain `to_string` of an all-string document already IS its JCS form.
fn canonical_json(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).expect("serialize json")
}

/// Assemble a bundle from explicit index bytes, signing whatever they are.
fn bundle_with_index(manifest: &[u8], source: &[u8], index_bytes: Vec<u8>) -> Vec<u8> {
    let signature_bytes = sign_index_bytes(&index_bytes, ClaimedIssuer::LocalAuthor);
    assemble(&[
        RawMember::file(MANIFEST_MEMBER_PATH, manifest.to_vec()),
        RawMember::file(INDEX_MEMBER_PATH, index_bytes),
        RawMember::file(SIGNATURE_MEMBER_PATH, signature_bytes),
        RawMember::file(SOURCE_MEMBER_PATH, source.to_vec()),
    ])
}

/// Assemble a bundle from a canonical index plus explicit signature bytes.
fn bundle_with_signature(manifest: &[u8], source: &[u8], signature_bytes: Vec<u8>) -> Vec<u8> {
    let index_bytes = canonical_json(&index_value(manifest, source));
    assemble(&[
        RawMember::file(MANIFEST_MEMBER_PATH, manifest.to_vec()),
        RawMember::file(INDEX_MEMBER_PATH, index_bytes),
        RawMember::file(SIGNATURE_MEMBER_PATH, signature_bytes),
        RawMember::file(SOURCE_MEMBER_PATH, source.to_vec()),
    ])
}

/// The canonical `signature.json` as a mutable `Value`, for signature vectors.
fn signature_value(index_bytes: &[u8], issuer: ClaimedIssuer) -> Value {
    let bytes = sign_index_bytes(index_bytes, issuer);
    serde_json::from_slice(&bytes).expect("parse canonical signature")
}

// ─────────────────────────────────────────────────────────────────────────────
// Writer
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn writer_output_is_byte_identical_for_the_same_input_and_signer() {
    let source = baseline_source_archive();
    let first = bundle_from(OUTER_MANIFEST, &source);
    let second = bundle_from(OUTER_MANIFEST, &source);
    assert_eq!(
        first, second,
        "same input + same signer must produce byte-identical bundles"
    );
}

#[test]
fn writer_emits_exactly_four_members_in_ascending_byte_order() {
    let bundle = baseline_bundle();
    let mut archive = tar::Archive::new(Cursor::new(&bundle));
    let names: Vec<String> = archive
        .entries()
        .expect("entries")
        .map(|entry| {
            let entry = entry.expect("entry");
            assert_eq!(entry.header().entry_type(), EntryType::Regular);
            assert_eq!(entry.header().mode().expect("mode"), 0o644);
            assert_eq!(entry.header().mtime().expect("mtime"), 0);
            String::from_utf8_lossy(entry.path_bytes().as_ref()).into_owned()
        })
        .collect();
    assert_eq!(names, V3_OUTER_MEMBER_PATHS.to_vec());
}

#[test]
fn writer_reuses_source_archive_bytes_verbatim() {
    let source = baseline_source_archive();
    let bundle = bundle_from(OUTER_MANIFEST, &source);
    let mut archive = tar::Archive::new(Cursor::new(&bundle));
    let mut found = None;
    for entry in archive.entries().expect("entries") {
        let mut entry = entry.expect("entry");
        if entry.path_bytes().as_ref() == SOURCE_MEMBER_PATH.as_bytes() {
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut bytes).expect("read");
            found = Some(bytes);
        }
    }
    assert_eq!(found.expect("source member present"), source);
}

#[test]
fn writer_index_is_exact_jcs_and_matches_measured_members() {
    let source = baseline_source_archive();
    let bundle = bundle_from(OUTER_MANIFEST, &source);
    let envelope = verify_local(&bundle).expect("baseline verifies");
    let index = envelope.index();

    assert_eq!(
        index.manifest_member().sha256,
        Sha256Digest::of_bytes(OUTER_MANIFEST.as_bytes())
    );
    assert_eq!(
        index.manifest_member().size_bytes.as_str(),
        OUTER_MANIFEST.len().to_string()
    );
    assert_eq!(
        index.source_member().sha256,
        Sha256Digest::of_bytes(&source)
    );
    assert_eq!(
        index.source_member().size_bytes.as_str(),
        source.len().to_string()
    );

    // The JCS self-consistency check inside `parse_index_json` already ran; this
    // pins the other direction — the writer's bytes ARE the canonicalization.
    let canonical = index.to_canonical_bytes().expect("canonical");
    assert_eq!(
        canonical,
        canonical_json(&index_value(OUTER_MANIFEST.as_bytes(), &source))
    );
}

#[test]
fn writer_signature_verifies_and_declares_ed25519() {
    let bundle = baseline_bundle();
    let envelope = verify_local(&bundle).expect("verifies");
    assert_eq!(envelope.claimed_issuer(), ClaimedIssuer::LocalAuthor);
    assert_eq!(envelope.key_id(), signer().key_id());
}

#[test]
fn ephemeral_local_signer_produces_a_verifiable_bundle() {
    let source = baseline_source_archive();
    let ephemeral = EphemeralLocalSigner::generate();
    let bundle = write_capsule_bundle_v3(
        &CapsuleBundleWriteInput {
            manifest_bytes: OUTER_MANIFEST.as_bytes(),
            source_archive_bytes: &source,
            claimed_issuer: ClaimedIssuer::LocalAuthor,
        },
        &ephemeral,
    )
    .expect("write");
    let envelope = verify_local(&bundle).expect("verifies");
    assert_eq!(envelope.signer_trust(), SignerTrust::UntrustedKey);
    assert_eq!(envelope.key_id(), ephemeral.key_id());
}

// ─────────────────────────────────────────────────────────────────────────────
// Outer reader
// ─────────────────────────────────────────────────────────────────────────────

fn assert_invalid(result: Result<VerifiedCapsuleEnvelope, CapsuleImportError>, context: &str) {
    match result {
        Err(CapsuleImportError::CapsuleInvalid(_)) => {}
        other => panic!("{context}: expected CapsuleInvalid, got {other:?}"),
    }
}

fn assert_signature_invalid(
    result: Result<VerifiedCapsuleEnvelope, CapsuleImportError>,
    context: &str,
) {
    match result {
        Err(CapsuleImportError::SignatureInvalid(_)) => {}
        other => panic!("{context}: expected SignatureInvalid, got {other:?}"),
    }
}

#[test]
fn outer_reader_rejects_duplicate_extra_and_missing_members() {
    let source = baseline_source_archive();
    let manifest = OUTER_MANIFEST.as_bytes().to_vec();
    let index_bytes = canonical_json(&index_value(&manifest, &source));
    let signature_bytes = sign_index_bytes(&index_bytes, ClaimedIssuer::LocalAuthor);

    let base = vec![
        RawMember::file(MANIFEST_MEMBER_PATH, manifest.clone()),
        RawMember::file(INDEX_MEMBER_PATH, index_bytes.clone()),
        RawMember::file(SIGNATURE_MEMBER_PATH, signature_bytes.clone()),
        RawMember::file(SOURCE_MEMBER_PATH, source.clone()),
    ];
    assert!(
        verify_local(&assemble(&base)).is_ok(),
        "control case must pass"
    );

    let mut duplicated = base.clone();
    duplicated.push(RawMember::file(INDEX_MEMBER_PATH, index_bytes.clone()));
    assert_invalid(verify_local(&assemble(&duplicated)), "duplicate member");

    let mut extra = base.clone();
    extra.push(RawMember::file("README.md", b"hi\n".to_vec()));
    assert_invalid(verify_local(&assemble(&extra)), "extra member");

    let missing_signature: Vec<RawMember> = base
        .iter()
        .filter(|member| member.name != SIGNATURE_MEMBER_PATH.as_bytes())
        .cloned()
        .collect();
    assert_invalid(
        verify_local(&assemble(&missing_signature)),
        "missing signature member",
    );
}

#[test]
fn outer_reader_rejects_every_path_alias() {
    let source = baseline_source_archive();
    let manifest = OUTER_MANIFEST.as_bytes().to_vec();
    let index_bytes = canonical_json(&index_value(&manifest, &source));
    let signature_bytes = sign_index_bytes(&index_bytes, ClaimedIssuer::LocalAuthor);

    for alias in [
        b"./index.json".to_vec(),
        b"/index.json".to_vec(),
        b"../index.json".to_vec(),
        b"a/../index.json".to_vec(),
        b".\\index.json".to_vec(),
        b"index.json/".to_vec(),
        b"index.json\0x".to_vec(),
    ] {
        let members = vec![
            RawMember::file(MANIFEST_MEMBER_PATH, manifest.clone()),
            RawMember::raw_named(&alias, index_bytes.clone()),
            RawMember::file(SIGNATURE_MEMBER_PATH, signature_bytes.clone()),
            RawMember::file(SOURCE_MEMBER_PATH, source.clone()),
        ];
        let rendered = String::from_utf8_lossy(&alias).into_owned();
        match verify_local(&assemble(&members)) {
            // An alias is never the member it imitates: either the entry is
            // refused outright, or `index.json` is simply absent and the bundle
            // is not v3 at all. Both are correct; silently accepting is not.
            Err(CapsuleImportError::CapsuleInvalid(_) | CapsuleImportError::NotV3Bundle) => {}
            other => panic!("alias {rendered:?}: expected rejection, got {other:?}"),
        }
    }
}

#[test]
fn outer_reader_rejects_non_regular_entry_kinds() {
    let source = baseline_source_archive();
    let manifest = OUTER_MANIFEST.as_bytes().to_vec();
    let index_bytes = canonical_json(&index_value(&manifest, &source));
    let signature_bytes = sign_index_bytes(&index_bytes, ClaimedIssuer::LocalAuthor);

    for (label, special) in [
        (
            "symlink",
            RawMember::special(
                MANIFEST_MEMBER_PATH,
                EntryType::Symlink,
                Some("/etc/passwd"),
            ),
        ),
        (
            "hardlink",
            RawMember::special(MANIFEST_MEMBER_PATH, EntryType::Link, Some("index.json")),
        ),
        (
            "char device",
            RawMember::special(MANIFEST_MEMBER_PATH, EntryType::Char, None),
        ),
        (
            "block device",
            RawMember::special(MANIFEST_MEMBER_PATH, EntryType::Block, None),
        ),
        (
            "fifo",
            RawMember::special(MANIFEST_MEMBER_PATH, EntryType::Fifo, None),
        ),
        (
            "directory",
            RawMember::special(MANIFEST_MEMBER_PATH, EntryType::Directory, None),
        ),
    ] {
        let members = vec![
            special,
            RawMember::file(INDEX_MEMBER_PATH, index_bytes.clone()),
            RawMember::file(SIGNATURE_MEMBER_PATH, signature_bytes.clone()),
            RawMember::file(SOURCE_MEMBER_PATH, source.clone()),
        ];
        assert_invalid(verify_local(&assemble(&members)), label);
    }
}

#[test]
fn index_present_never_falls_back_to_v2() {
    let source = baseline_source_archive();
    let manifest = OUTER_MANIFEST.as_bytes().to_vec();
    // A structurally hopeless index. A reader that fell back to v2 would report
    // something other than a v3 rejection.
    let bundle = bundle_with_index(&manifest, &source, b"{not json at all".to_vec());
    match verify_local(&bundle) {
        Err(CapsuleImportError::CapsuleInvalid(_)) => {}
        other => panic!("expected a v3 rejection with no v2 fallback, got {other:?}"),
    }
    assert_eq!(
        classify_bundle_format(Cursor::new(&bundle)).expect("classify"),
        BundleFormat::V3
    );
}

#[test]
fn index_absent_dispatches_to_the_v2_reader() {
    // A v2-shaped archive, including the members an earlier RFC revision's
    // (wrong) v2 allowlist omitted — a reader that enumerated v2's shape would
    // have rejected these.
    let v2 = assemble(&[
        RawMember::file("capsule.toml", OUTER_MANIFEST.as_bytes().to_vec()),
        RawMember::file("capsule.lock.json", b"{}\n".to_vec()),
        RawMember::file("signature.json", b"{\"signed\":false}\n".to_vec()),
        RawMember::file("payload.tar.zst", b"\x28\xb5\x2f\xfd".to_vec()),
        RawMember::file("sbom.spdx.json", b"{}\n".to_vec()),
        RawMember::file("README.md", b"readme\n".to_vec()),
    ]);
    assert_eq!(
        classify_bundle_format(Cursor::new(&v2)).expect("classify"),
        BundleFormat::V2Legacy
    );
    match verify_local(&v2) {
        Err(CapsuleImportError::NotV3Bundle) => {}
        other => panic!("expected a v2 hand-off, got {other:?}"),
    }

    // A v2-shaped archive MISSING a required v2 member is still v2's problem to
    // report, not a v3 rejection.
    let incomplete = assemble(&[RawMember::file(
        "capsule.toml",
        OUTER_MANIFEST.as_bytes().to_vec(),
    )]);
    match verify_local(&incomplete) {
        Err(CapsuleImportError::NotV3Bundle) => {}
        other => panic!("expected a v2 hand-off, got {other:?}"),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// index.json
// ─────────────────────────────────────────────────────────────────────────────

fn index_vector_bytes(mutate: impl FnOnce(&mut Value)) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let source = baseline_source_archive();
    let manifest = OUTER_MANIFEST.as_bytes().to_vec();
    let mut value = index_value(&manifest, &source);
    mutate(&mut value);
    (manifest, source, canonical_json(&value))
}

fn assert_index_vector_rejected(label: &str, mutate: impl FnOnce(&mut Value)) {
    let (manifest, source, index_bytes) = index_vector_bytes(mutate);
    let bundle = bundle_with_index(&manifest, &source, index_bytes);
    assert_invalid(verify_local(&bundle), label);
}

#[test]
fn index_rejects_schema_role_path_and_media_type_violations() {
    assert_index_vector_rejected("wrong schema", |value| {
        value["schema"] = json!("ato.capsule-index/v2");
    });
    assert_index_vector_rejected("role lock", |value| {
        value["members"][0]["role"] = json!("lock");
    });
    assert_index_vector_rejected("unknown role", |value| {
        value["members"][0]["role"] = json!("sbom");
    });
    assert_index_vector_rejected("wrong manifest path", |value| {
        value["members"][0]["path"] = json!("Capsule.toml");
    });
    assert_index_vector_rejected("wrong manifest media type", |value| {
        value["members"][0]["media_type"] = json!("text/toml");
    });
    assert_index_vector_rejected("wrong source media type", |value| {
        value["members"][1]["media_type"] = json!("application/zstd");
    });
    assert_index_vector_rejected("missing source member", |value| {
        value["members"] = json!([value["members"][0].clone()]);
    });
}

#[test]
fn index_rejects_unknown_fields_at_both_levels() {
    assert_index_vector_rejected("unknown top-level field", |value| {
        value["extra"] = json!("nope");
    });
    assert_index_vector_rejected("unknown member field", |value| {
        value["members"][0]["extra"] = json!("nope");
    });
}

#[test]
fn index_rejects_duplicate_json_keys_at_both_levels() {
    let source = baseline_source_archive();
    let manifest = OUTER_MANIFEST.as_bytes().to_vec();
    let canonical =
        String::from_utf8(canonical_json(&index_value(&manifest, &source))).expect("utf8 index");

    let top_level = canonical.replacen(
        "{\"members\"",
        "{\"schema\":\"ato.capsule-index/v1\",\"members\"",
        1,
    );
    assert_ne!(top_level, canonical);
    assert_invalid(
        verify_local(&bundle_with_index(
            &manifest,
            &source,
            top_level.into_bytes(),
        )),
        "duplicate top-level key",
    );

    let per_member = canonical.replacen(
        "{\"media_type\":\"application/toml\"",
        "{\"media_type\":\"application/toml\",\"media_type\":\"application/toml\"",
        1,
    );
    assert_ne!(per_member, canonical);
    assert_invalid(
        verify_local(&bundle_with_index(
            &manifest,
            &source,
            per_member.into_bytes(),
        )),
        "duplicate per-member key",
    );
}

#[test]
fn index_rejects_non_canonical_digests_and_sizes() {
    assert_index_vector_rejected("uppercase digest hex", |value| {
        let digest = value["members"][0]["sha256"]
            .as_str()
            .expect("digest")
            .to_string();
        value["members"][0]["sha256"] = json!(digest.to_uppercase());
    });
    assert_index_vector_rejected("unlabelled digest", |value| {
        let digest = value["members"][0]["sha256"]
            .as_str()
            .expect("digest")
            .to_string();
        value["members"][0]["sha256"] =
            json!(digest.strip_prefix("sha256:").expect("prefix").to_string());
    });
    assert_index_vector_rejected("wrong digest", |value| {
        value["members"][0]["sha256"] = json!(format!("sha256:{}", "0".repeat(64)));
    });
    assert_index_vector_rejected("leading-zero size", |value| {
        let size = value["members"][0]["size_bytes"]
            .as_str()
            .expect("size")
            .to_string();
        value["members"][0]["size_bytes"] = json!(format!("0{size}"));
    });
    assert_index_vector_rejected("numeric size", |value| {
        value["members"][0]["size_bytes"] = json!(123);
    });
    assert_index_vector_rejected("wrong size", |value| {
        value["members"][0]["size_bytes"] = json!("999999");
    });
}

#[test]
fn index_rejects_duplicate_paths_and_out_of_order_members() {
    assert_index_vector_rejected("duplicate path across roles", |value| {
        let mut second = value["members"][1].clone();
        second["path"] = json!(MANIFEST_MEMBER_PATH);
        second["media_type"] = json!(MANIFEST_MEDIA_TYPE);
        value["members"] = json!([value["members"][0].clone(), second]);
    });
    assert_index_vector_rejected("out of order members", |value| {
        value["members"] = json!([value["members"][1].clone(), value["members"][0].clone()]);
    });
}

#[test]
fn index_rejects_bytes_that_are_not_their_own_jcs_canonicalization() {
    let source = baseline_source_archive();
    let manifest = OUTER_MANIFEST.as_bytes().to_vec();
    let pretty = serde_json::to_vec_pretty(&index_value(&manifest, &source)).expect("pretty");
    assert_invalid(
        verify_local(&bundle_with_index(&manifest, &source, pretty)),
        "index.json not in JCS form",
    );
}

#[test]
fn manifest_member_tampered_post_signing_is_a_member_digest_mismatch() {
    let source = baseline_source_archive();
    let manifest = OUTER_MANIFEST.as_bytes().to_vec();
    let index_bytes = canonical_json(&index_value(&manifest, &source));
    let signature_bytes = sign_index_bytes(&index_bytes, ClaimedIssuer::LocalAuthor);
    let tampered = format!("{OUTER_MANIFEST}\n# injected\n").into_bytes();

    let bundle = assemble(&[
        RawMember::file(MANIFEST_MEMBER_PATH, tampered),
        RawMember::file(INDEX_MEMBER_PATH, index_bytes),
        RawMember::file(SIGNATURE_MEMBER_PATH, signature_bytes),
        RawMember::file(SOURCE_MEMBER_PATH, source),
    ]);
    match verify_local(&bundle) {
        Err(CapsuleImportError::CapsuleInvalid(message)) => {
            assert!(
                message.contains("capsule.toml") && message.contains("hashes to"),
                "expected a member digest mismatch, got {message}"
            );
        }
        other => panic!("expected a member digest mismatch, got {other:?}"),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// signature.json
// ─────────────────────────────────────────────────────────────────────────────

fn assert_signature_vector_rejected(label: &str, mutate: impl FnOnce(&mut Value)) {
    let source = baseline_source_archive();
    let manifest = OUTER_MANIFEST.as_bytes().to_vec();
    let index_bytes = canonical_json(&index_value(&manifest, &source));
    let mut value = signature_value(&index_bytes, ClaimedIssuer::LocalAuthor);
    mutate(&mut value);
    let bundle = bundle_with_signature(&manifest, &source, canonical_json(&value));
    assert_signature_invalid(verify_local(&bundle), label);
}

#[test]
fn signature_rejects_schema_algorithm_and_key_violations() {
    assert_signature_vector_rejected("wrong schema", |value| {
        value["schema"] = json!("ato.capsule-index-signature/v2");
    });
    assert_signature_vector_rejected("uppercase algorithm", |value| {
        value["algorithm"] = json!("Ed25519");
    });
    assert_signature_vector_rejected("other algorithm", |value| {
        value["algorithm"] = json!("ed25519ph");
    });
    assert_signature_vector_rejected("non-did key_id", |value| {
        value["key_id"] = json!("ed25519:AAAA");
    });
    assert_signature_vector_rejected("undecodable did:key", |value| {
        value["key_id"] = json!("did:key:z000");
    });
    assert_signature_vector_rejected("unknown field", |value| {
        value["previous_key"] = json!("did:key:z6Mk");
    });
}

#[test]
fn signature_rejects_non_canonical_encodings() {
    assert_signature_vector_rejected("padded base64url", |value| {
        let raw = value["signature"].as_str().expect("signature").to_string();
        value["signature"] = json!(format!("{raw}=="));
    });
    assert_signature_vector_rejected("standard base64", |value| {
        use base64::Engine as _;
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(value["signature"].as_str().expect("signature"))
            .expect("decode");
        value["signature"] = json!(base64::engine::general_purpose::STANDARD.encode(decoded));
    });
    assert_signature_vector_rejected("wrong signature length", |value| {
        use base64::Engine as _;
        value["signature"] =
            json!(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([1u8; 32]));
    });
    assert_signature_vector_rejected("uppercase index_digest", |value| {
        let digest = value["index_digest"].as_str().expect("digest").to_string();
        value["index_digest"] = json!(digest.to_uppercase());
    });
}

#[test]
fn signature_rejects_duplicate_json_keys() {
    let source = baseline_source_archive();
    let manifest = OUTER_MANIFEST.as_bytes().to_vec();
    let index_bytes = canonical_json(&index_value(&manifest, &source));
    let canonical = String::from_utf8(sign_index_bytes(&index_bytes, ClaimedIssuer::LocalAuthor))
        .expect("utf8 signature");
    let duplicated = canonical.replacen(
        "{\"algorithm\":\"ed25519\"",
        "{\"algorithm\":\"ed25519\",\"algorithm\":\"ed25519\"",
        1,
    );
    assert_ne!(duplicated, canonical);
    assert_signature_invalid(
        verify_local(&bundle_with_signature(
            &manifest,
            &source,
            duplicated.into_bytes(),
        )),
        "duplicate signature key",
    );
}

#[test]
fn signature_rejects_wrong_index_digest_and_bad_signature_bytes() {
    assert_signature_vector_rejected("index_digest mismatch", |value| {
        value["index_digest"] = json!(format!("sha256:{}", "0".repeat(64)));
    });
    assert_signature_vector_rejected("forged signature bytes", |value| {
        use base64::Engine as _;
        value["signature"] =
            json!(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([7u8; 64]));
    });
    // A signature made by a different key over the same index: structurally
    // perfect, cryptographically wrong.
    let source = baseline_source_archive();
    let manifest = OUTER_MANIFEST.as_bytes().to_vec();
    let index_bytes = canonical_json(&index_value(&manifest, &source));
    let other = FixedKeySigner::from_seed(0x22);
    let raw = other.sign(&signing_message(&index_bytes)).expect("sign");
    let mut value = signature_value(&index_bytes, ClaimedIssuer::LocalAuthor);
    use base64::Engine as _;
    value["signature"] = json!(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw));
    assert_signature_invalid(
        verify_local(&bundle_with_signature(
            &manifest,
            &source,
            canonical_json(&value),
        )),
        "signature by a different key",
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Trust
// ─────────────────────────────────────────────────────────────────────────────

fn store_context(origin: &str, digest: Sha256Digest) -> CapsuleImportContext {
    CapsuleImportContext::Store {
        api_origin: NormalizedOrigin::parse(origin).expect("origin"),
        expected_bundle_digest: digest,
    }
}

fn pinned_policy(origin: &str) -> CapsuleTrustPolicy {
    CapsuleTrustPolicy::new().with_store_pins(
        PinnedStoreOrigin::new(
            NormalizedOrigin::parse(origin).expect("origin"),
            vec![signer().key_id().clone()],
        )
        .expect("pin array"),
    )
}

#[test]
fn store_pin_for_one_origin_does_not_authenticate_another() {
    let bundle = baseline_bundle();
    let digest = Sha256Digest::of_bytes(&bundle);
    let policy = pinned_policy("https://api.ato.run");

    let trusted = verify_capsule_envelope(
        Cursor::new(bundle.clone()),
        store_context("https://api.ato.run", digest),
        &policy,
        &unbounded(),
    )
    .expect("origin A is pinned");
    assert_eq!(trusted.signer_trust(), SignerTrust::TrustedStore);

    match verify_capsule_envelope(
        Cursor::new(bundle),
        store_context("https://staging-api.ato.run", digest),
        &policy,
        &unbounded(),
    ) {
        Err(CapsuleImportError::UntrustedSigner(_)) => {}
        other => panic!("a pin must not cross the origin boundary, got {other:?}"),
    }
}

#[test]
fn claimed_issuer_ato_store_does_not_upgrade_trust() {
    let source = baseline_source_archive();
    let bundle = write_capsule_bundle_v3(
        &CapsuleBundleWriteInput {
            manifest_bytes: OUTER_MANIFEST.as_bytes(),
            source_archive_bytes: &source,
            // The lie an attacker tells.
            claimed_issuer: ClaimedIssuer::AtoStore,
        },
        &FixedKeySigner::from_seed(0x33),
    )
    .expect("write");

    let envelope = verify_local(&bundle).expect("structurally valid");
    assert_eq!(envelope.claimed_issuer(), ClaimedIssuer::AtoStore);
    assert_eq!(
        envelope.signer_trust(),
        SignerTrust::UntrustedKey,
        "claimed_issuer must never influence trust"
    );

    // And on the Store path — where an unpinned key is fatal — it is refused.
    let digest = Sha256Digest::of_bytes(&bundle);
    match verify_capsule_envelope(
        Cursor::new(bundle),
        store_context("https://api.ato.run", digest),
        &pinned_policy("https://api.ato.run"),
        &unbounded(),
    ) {
        Err(CapsuleImportError::UntrustedSigner(_)) => {}
        other => panic!("expected UntrustedSigner, got {other:?}"),
    }
}

#[test]
fn bundle_digest_mismatch_is_its_own_category_and_precedes_parsing() {
    let bundle = baseline_bundle();
    let wrong = Sha256Digest::of_bytes(b"not this bundle");
    match verify_capsule_envelope(
        Cursor::new(bundle),
        store_context("https://api.ato.run", wrong),
        &pinned_policy("https://api.ato.run"),
        &unbounded(),
    ) {
        Err(CapsuleImportError::BundleDigestMismatch { .. }) => {}
        other => panic!("expected BundleDigestMismatch, got {other:?}"),
    }

    // It runs before v3 parsing: a bundle that is BOTH the wrong bytes and
    // structurally broken still reports the digest mismatch.
    let broken = bundle_with_index(
        OUTER_MANIFEST.as_bytes(),
        &baseline_source_archive(),
        b"{".to_vec(),
    );
    match verify_capsule_envelope(
        Cursor::new(broken),
        store_context("https://api.ato.run", wrong),
        &pinned_policy("https://api.ato.run"),
        &unbounded(),
    ) {
        Err(CapsuleImportError::BundleDigestMismatch { .. }) => {}
        other => panic!("digest check must precede parsing, got {other:?}"),
    }
}

#[test]
fn local_expected_digest_is_optional_but_checked_when_present() {
    let bundle = baseline_bundle();
    let digest = Sha256Digest::of_bytes(&bundle);

    verify_capsule_envelope(
        Cursor::new(bundle.clone()),
        CapsuleImportContext::LocalFile {
            expected_bundle_digest: Some(digest),
        },
        &permissive_local_policy(),
        &unbounded(),
    )
    .expect("matching digest");

    match verify_capsule_envelope(
        Cursor::new(bundle),
        CapsuleImportContext::LocalFile {
            expected_bundle_digest: Some(Sha256Digest::of_bytes(b"other")),
        },
        &permissive_local_policy(),
        &unbounded(),
    ) {
        Err(CapsuleImportError::BundleDigestMismatch { .. }) => {}
        other => panic!("expected BundleDigestMismatch, got {other:?}"),
    }
}

#[test]
fn local_unknown_signer_is_classified_not_prompted() {
    let bundle = baseline_bundle();
    let envelope = verify_local(&bundle).expect("accepted with confirmation");
    assert_eq!(envelope.signer_trust(), SignerTrust::UntrustedKey);
    assert_eq!(envelope.bundle_digest(), Sha256Digest::of_bytes(&bundle));

    // Fail-closed default: a caller that has not opted into confirming an
    // unknown signer gets a refusal instead of a silent import.
    match verify_capsule_envelope(
        Cursor::new(bundle),
        local_context(),
        &CapsuleTrustPolicy::new(),
        &unbounded(),
    ) {
        Err(CapsuleImportError::UntrustedSigner(_)) => {}
        other => panic!("expected UntrustedSigner, got {other:?}"),
    }
}

#[test]
fn slice_one_never_produces_publisher_or_local_key_trust() {
    // Exhaustive over the two contexts this slice supports, with and without a
    // matching pin: only TrustedStore and UntrustedKey are reachable.
    let bundle = baseline_bundle();
    let digest = Sha256Digest::of_bytes(&bundle);
    let observed = [
        verify_local(&bundle).expect("local").signer_trust(),
        verify_capsule_envelope(
            Cursor::new(bundle),
            store_context("https://api.ato.run", digest),
            &pinned_policy("https://api.ato.run"),
            &unbounded(),
        )
        .expect("store")
        .signer_trust(),
    ];
    for trust in observed {
        assert!(
            matches!(trust, SignerTrust::TrustedStore | SignerTrust::UntrustedKey),
            "Slice 1 must not produce {trust:?}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Program derivation and inner control files
// ─────────────────────────────────────────────────────────────────────────────

/// The workspace's file set and bytes, as a sorted, comparable value.
fn workspace_contents(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut entries = Vec::new();
    collect(root, "", &mut entries);
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    return entries;

    fn collect(dir: &Path, prefix: &str, out: &mut Vec<(String, Vec<u8>)>) {
        let mut names: Vec<PathBuf> = fs::read_dir(dir)
            .expect("read workspace dir")
            .map(|entry| entry.expect("dir entry").path())
            .collect();
        names.sort();
        for path in names {
            let name = path
                .file_name()
                .expect("name")
                .to_string_lossy()
                .into_owned();
            let joined = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            if path.is_dir() {
                collect(&path, &joined, out);
            } else {
                out.push((joined, fs::read(&path).expect("read workspace file")));
            }
        }
    }
}

struct Imported {
    program_id: String,
    contents: Vec<(String, Vec<u8>)>,
}

fn import_and_snapshot(bundle: &[u8]) -> Imported {
    let import = import_local(bundle).expect("import succeeds");
    let workspace = import.into_workspace();
    Imported {
        program_id: workspace.capsule_program_id().to_string(),
        contents: workspace_contents(workspace.path()),
    }
}

fn baseline_import() -> Imported {
    import_and_snapshot(&baseline_bundle())
}

#[test]
fn baseline_import_yields_manifest_plus_projected_source_and_no_lock() {
    let baseline = baseline_import();
    let names: Vec<&str> = baseline
        .contents
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(names, vec!["app.py", "capsule.toml", "lib/util.py"]);

    let manifest = baseline
        .contents
        .iter()
        .find(|(name, _)| name == "capsule.toml")
        .expect("manifest present");
    assert_eq!(manifest.1, OUTER_MANIFEST.as_bytes());
    assert!(
        !names.contains(&"capsule.lock") && !names.contains(&"ato.lock.json"),
        "no lock may be written into the workspace"
    );
    assert!(baseline.program_id.starts_with("blake3:"));
}

/// Every valid inner-control-file shape must be indistinguishable from an
/// archive that never carried one — the same `capsule_program_id` AND the same
/// workspace bytes, not merely "import succeeded".
fn assert_identical_to_baseline(label: &str, extra_inner_files: &[(&str, &str)]) {
    let baseline = baseline_import();
    let bundle = bundle_from(OUTER_MANIFEST, &source_archive_with(extra_inner_files));
    let observed = import_and_snapshot(&bundle);
    assert_eq!(
        observed.program_id, baseline.program_id,
        "{label}: inner control files must not move capsule_program_id"
    );
    assert_eq!(
        observed.contents, baseline.contents,
        "{label}: inner control files must not change the workspace"
    );
}

#[test]
fn inner_manifest_absent_matches_the_baseline() {
    assert_identical_to_baseline("inner manifest absent", &[]);
}

#[test]
fn inner_manifest_differing_from_outer_is_accepted_and_has_no_effect() {
    assert_identical_to_baseline(
        "inner manifest differs",
        &[("capsule.toml", INNER_MANIFEST_DIFFERENT)],
    );
}

#[test]
fn inner_manifest_malformed_is_accepted_and_never_parsed() {
    assert_identical_to_baseline(
        "inner manifest malformed",
        &[("capsule.toml", "this is not [ valid toml at ] all = = =\n")],
    );
}

#[test]
fn inner_capsule_lock_alone_is_excluded() {
    assert_identical_to_baseline(
        "inner capsule.lock",
        &[("capsule.lock", "{\"schema\":\"cloud-built\"}\n")],
    );
}

#[test]
fn inner_ato_lock_json_alone_is_excluded() {
    assert_identical_to_baseline(
        "inner ato.lock.json",
        &[("ato.lock.json", "{\"schema\":\"cloud-built\"}\n")],
    );
}

#[test]
fn inner_split_brain_locks_are_rejected() {
    let bundle = bundle_from(
        OUTER_MANIFEST,
        &source_archive_with(&[("capsule.lock", "{}\n"), ("ato.lock.json", "{}\n")]),
    );
    match import_local(&bundle) {
        Err(CapsuleImportError::CapsuleInvalid(message)) => {
            assert!(
                message.contains("capsule.lock") && message.contains("ato.lock.json"),
                "expected the split-brain-lock refusal, got {message}"
            );
        }
        other => panic!("expected a split-brain-lock rejection, got {other:?}"),
    }
}

#[test]
fn imported_workspace_reports_its_own_trust_and_identity() {
    let bundle = baseline_bundle();
    let import = import_local(&bundle).expect("import");
    assert_eq!(import.signer_trust(), SignerTrust::UntrustedKey);
    assert_eq!(import.bundle_digest(), Sha256Digest::of_bytes(&bundle));
    let program_id = import.capsule_program_id().clone();
    let workspace = import.into_workspace();
    assert_eq!(workspace.capsule_program_id(), &program_id);
    assert_eq!(workspace.signer_trust(), SignerTrust::UntrustedKey);
}

#[test]
fn workspace_directory_is_removed_when_dropped() {
    let workspace = import_local(&baseline_bundle())
        .expect("import")
        .into_workspace();
    let root = workspace.path().to_path_buf();
    assert!(root.exists());
    drop(workspace);
    assert!(
        !root.exists(),
        "the workspace TempDir must be removed on drop"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Policy
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn resource_budget_exceeded_is_distinguishable_from_format_invalid() {
    let bundle = baseline_bundle();
    let policy = CapsuleImportPolicy {
        temporary_storage_budget: Some(8),
        ..CapsuleImportPolicy::default()
    };
    match verify_capsule_envelope(
        Cursor::new(bundle.clone()),
        local_context(),
        &permissive_local_policy(),
        &policy,
    ) {
        Err(CapsuleImportError::ResourceBudgetExceeded(_)) => {}
        other => panic!("expected ResourceBudgetExceeded, got {other:?}"),
    }

    let disk = CapsuleImportPolicy {
        available_disk_bytes: Some(8),
        ..CapsuleImportPolicy::default()
    };
    match verify_capsule_envelope(
        Cursor::new(bundle),
        local_context(),
        &permissive_local_policy(),
        &disk,
    ) {
        Err(CapsuleImportError::InsufficientLocalStorage(_)) => {}
        other => panic!("expected InsufficientLocalStorage, got {other:?}"),
    }
}

#[test]
fn declared_size_is_never_used_to_preallocate() {
    // An index declaring an absurd size — beyond u64, so nothing could allocate
    // from it even if it tried — must be caught by the size comparison.
    assert_index_vector_rejected("astronomically large declared size", |value| {
        value["members"][0]["size_bytes"] = json!("99999999999999999999999999999999");
    });
}

#[test]
fn staging_is_removed_when_derivation_fails() {
    // A bundle whose source member is not a source archive at all: the envelope
    // verifies (its digest and size are honest), and derivation then fails.
    let junk = b"definitely not a zstd source archive".to_vec();
    let bundle = bundle_from(OUTER_MANIFEST, &junk);
    let envelope = verify_local(&bundle).expect("envelope verifies");
    let staging_root = envelope.staging_root_for_test().to_path_buf();
    assert!(staging_root.exists());

    let error = derive_imported_capsule(envelope).expect_err("derivation must fail");
    assert!(matches!(error, CapsuleImportError::CapsuleInvalid(_)));
    assert!(
        !staging_root.exists(),
        "a failed derivation must not leak its staging directory"
    );
}

#[test]
fn staging_is_removed_when_the_envelope_is_dropped() {
    let envelope = verify_local(&baseline_bundle()).expect("verifies");
    let staging_root = envelope.staging_root_for_test().to_path_buf();
    assert!(staging_root.exists());
    drop(envelope);
    assert!(!staging_root.exists());
}

// ─────────────────────────────────────────────────────────────────────────────
// Golden vectors
// ─────────────────────────────────────────────────────────────────────────────

fn vector_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/capsule_format_v3")
}

/// Regenerate the committed golden vectors.
///
/// `#[ignore]`d so it never runs in ordinary CI; the committed bytes are the
/// artifact, and `tests/capsule_format_v3_vectors.rs` is what checks them.
///
/// ```sh
/// cargo test -p capsule --lib -- --ignored --exact \
///     import_bundle::tests::regenerate_capsule_format_v3_vectors
/// ```
#[test]
#[ignore = "fixture generator; run explicitly to regenerate the committed vectors"]
fn regenerate_capsule_format_v3_vectors() {
    let root = vector_dir();
    let valid_dir = root.join("valid");
    let invalid_dir = root.join("invalid");
    for dir in [&valid_dir, &invalid_dir] {
        if dir.exists() {
            fs::remove_dir_all(dir).expect("clear vector dir");
        }
        fs::create_dir_all(dir).expect("create vector dir");
    }

    let mut manifest = json!({ "valid": [], "invalid": [] });
    for vector in valid_vectors() {
        fs::write(valid_dir.join(&vector.file), &vector.bytes).expect("write vector");
        manifest["valid"]
            .as_array_mut()
            .expect("array")
            .push(json!({
                "file": vector.file,
                "note": vector.note,
                "same_program_id_as": vector.same_program_id_as,
            }));
    }
    for vector in invalid_vectors() {
        fs::write(invalid_dir.join(&vector.file), &vector.bytes).expect("write vector");
        manifest["invalid"]
            .as_array_mut()
            .expect("array")
            .push(json!({
                "file": vector.file,
                "note": vector.note,
                "context": vector.context,
                "expected_error": vector.expected_error,
                "stage": vector.stage,
            }));
    }
    fs::write(
        root.join("manifest.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&manifest).expect("pretty manifest")
        ),
    )
    .expect("write vector manifest");
}

struct ValidVector {
    file: String,
    note: &'static str,
    same_program_id_as: &'static str,
    bytes: Vec<u8>,
}

struct InvalidVector {
    file: String,
    note: &'static str,
    context: &'static str,
    expected_error: &'static str,
    stage: &'static str,
    bytes: Vec<u8>,
}

fn valid_vectors() -> Vec<ValidVector> {
    /// (vector name, note, extra root files the inner source archive carries).
    type ValidCase = (
        &'static str,
        &'static str,
        Vec<(&'static str, &'static str)>,
    );
    let cases: Vec<ValidCase> = vec![
        (
            "baseline",
            "source archive with no inner control files at all",
            vec![],
        ),
        (
            "inner-manifest-absent",
            "no inner capsule.toml; the outer manifest applies regardless",
            vec![],
        ),
        (
            "inner-manifest-differs",
            "inner capsule.toml disagrees with the outer one; outer wins",
            vec![("capsule.toml", INNER_MANIFEST_DIFFERENT)],
        ),
        (
            "inner-manifest-malformed",
            "inner capsule.toml is unparseable; excluded by name, never parsed",
            vec![("capsule.toml", "this is not [ valid toml at ] all = = =\n")],
        ),
        (
            "inner-capsule-lock",
            "inner capsule.lock alone; excluded from the projection",
            vec![("capsule.lock", "{\"schema\":\"cloud-built\"}\n")],
        ),
        (
            "inner-ato-lock-json",
            "inner ato.lock.json alone; excluded from the projection",
            vec![("ato.lock.json", "{\"schema\":\"cloud-built\"}\n")],
        ),
    ];
    cases
        .into_iter()
        .map(|(name, note, extra)| ValidVector {
            file: format!("{name}.capsule"),
            note,
            same_program_id_as: "baseline.capsule",
            bytes: bundle_from(OUTER_MANIFEST, &source_archive_with(&extra)),
        })
        .collect()
}

#[allow(clippy::too_many_lines)]
fn invalid_vectors() -> Vec<InvalidVector> {
    let source = baseline_source_archive();
    let manifest = OUTER_MANIFEST.as_bytes().to_vec();
    let index_bytes = canonical_json(&index_value(&manifest, &source));
    let signature_bytes = sign_index_bytes(&index_bytes, ClaimedIssuer::LocalAuthor);
    let base = vec![
        RawMember::file(MANIFEST_MEMBER_PATH, manifest.clone()),
        RawMember::file(INDEX_MEMBER_PATH, index_bytes.clone()),
        RawMember::file(SIGNATURE_MEMBER_PATH, signature_bytes.clone()),
        RawMember::file(SOURCE_MEMBER_PATH, source.clone()),
    ];

    let mut vectors: Vec<InvalidVector> = Vec::new();
    let mut push = |file: &str,
                    note: &'static str,
                    context: &'static str,
                    expected_error: &'static str,
                    stage: &'static str,
                    bytes: Vec<u8>| {
        vectors.push(InvalidVector {
            file: format!("{file}.capsule"),
            note,
            context,
            expected_error,
            stage,
            bytes,
        });
    };

    // ── outer container ──────────────────────────────────────────────────
    let mut duplicated = base.clone();
    duplicated.push(RawMember::file(INDEX_MEMBER_PATH, index_bytes.clone()));
    push(
        "outer-duplicate-member",
        "index.json appears twice in the outer TAR",
        "local",
        "capsule_invalid",
        "envelope",
        assemble(&duplicated),
    );

    let mut extra = base.clone();
    extra.push(RawMember::file("README.md", b"readme\n".to_vec()));
    push(
        "outer-extra-member",
        "a fifth outer member outside the exact-four allowlist",
        "local",
        "capsule_invalid",
        "envelope",
        assemble(&extra),
    );

    push(
        "outer-missing-signature",
        "signature.json absent; v3 has no degrade-to-unsigned path",
        "local",
        "capsule_invalid",
        "envelope",
        assemble(
            &base
                .iter()
                .filter(|member| member.name != SIGNATURE_MEMBER_PATH.as_bytes())
                .cloned()
                .collect::<Vec<_>>(),
        ),
    );

    for (file, note, alias) in [
        (
            "outer-path-traversal",
            "outer member named ../capsule.toml",
            b"../capsule.toml".to_vec(),
        ),
        (
            "outer-absolute-path",
            "outer member named /capsule.toml",
            b"/capsule.toml".to_vec(),
        ),
        (
            "outer-dot-slash-alias",
            "outer member named ./capsule.toml",
            b"./capsule.toml".to_vec(),
        ),
        (
            "outer-backslash-alias",
            "outer member named .\\capsule.toml",
            b".\\capsule.toml".to_vec(),
        ),
        (
            "outer-nul-in-name-field",
            "outer member whose TAR name field carries bytes after its NUL terminator",
            b"capsule.toml\0x".to_vec(),
        ),
    ] {
        let mut members = base.clone();
        members[0] = RawMember::raw_named(&alias, manifest.clone());
        push(
            file,
            note,
            "local",
            "capsule_invalid",
            "envelope",
            assemble(&members),
        );
    }

    for (file, note, entry_type, link) in [
        (
            "outer-symlink-member",
            "capsule.toml is a symlink",
            EntryType::Symlink,
            Some("/etc/passwd"),
        ),
        (
            "outer-hardlink-member",
            "capsule.toml is a hardlink",
            EntryType::Link,
            Some("index.json"),
        ),
        (
            "outer-device-member",
            "capsule.toml is a character device",
            EntryType::Char,
            None,
        ),
        (
            "outer-fifo-member",
            "capsule.toml is a FIFO",
            EntryType::Fifo,
            None,
        ),
        (
            "outer-directory-member",
            "capsule.toml is a directory entry",
            EntryType::Directory,
            None,
        ),
    ] {
        let mut members = base.clone();
        members[0] = RawMember::special(MANIFEST_MEMBER_PATH, entry_type, link);
        push(
            file,
            note,
            "local",
            "capsule_invalid",
            "envelope",
            assemble(&members),
        );
    }

    push(
        "v2-shaped-no-index",
        "no root index.json: hand off to the v2 reader, never parse as v3",
        "local",
        "not_v3_bundle",
        "envelope",
        assemble(&[
            RawMember::file("capsule.toml", manifest.clone()),
            RawMember::file("capsule.lock.json", b"{}\n".to_vec()),
            RawMember::file("signature.json", b"{\"signed\":false}\n".to_vec()),
            RawMember::file("payload.tar.zst", b"\x28\xb5\x2f\xfd".to_vec()),
        ]),
    );
    push(
        "v2-shaped-missing-member",
        "a v2 archive missing a required v2 member; still v2's question, not v3's",
        "local",
        "not_v3_bundle",
        "envelope",
        assemble(&[RawMember::file("capsule.toml", manifest.clone())]),
    );

    // ── index.json ───────────────────────────────────────────────────────
    let index_case = |mutate: &dyn Fn(&mut Value)| -> Vec<u8> {
        let mut value = index_value(&manifest, &source);
        mutate(&mut value);
        bundle_with_index(&manifest, &source, canonical_json(&value))
    };

    for (file, note, mutate) in [
        (
            "index-wrong-schema",
            "index.json declares an unknown schema",
            &(|value: &mut Value| value["schema"] = json!("ato.capsule-index/v2"))
                as &dyn Fn(&mut Value),
        ),
        (
            "index-role-lock",
            "a role:\"lock\" member; v1 carries no lock",
            &|value: &mut Value| value["members"][0]["role"] = json!("lock"),
        ),
        (
            "index-unknown-role",
            "an unknown role value",
            &|value: &mut Value| value["members"][0]["role"] = json!("sbom"),
        ),
        (
            "index-wrong-member-path",
            "the manifest member declares a path v1 does not fix it to",
            &|value: &mut Value| value["members"][0]["path"] = json!("Capsule.toml"),
        ),
        (
            "index-wrong-media-type",
            "the manifest member declares the wrong media type",
            &|value: &mut Value| value["members"][0]["media_type"] = json!("text/toml"),
        ),
        (
            "index-unknown-top-level-field",
            "an unknown top-level field",
            &|value: &mut Value| value["extra"] = json!("nope"),
        ),
        (
            "index-unknown-member-field",
            "an unknown per-member field",
            &|value: &mut Value| value["members"][0]["extra"] = json!("nope"),
        ),
        (
            "index-uppercase-digest-hex",
            "a sha256 spelled with uppercase hex",
            &|value: &mut Value| {
                let digest = value["members"][0]["sha256"]
                    .as_str()
                    .expect("d")
                    .to_string();
                value["members"][0]["sha256"] = json!(digest.to_uppercase());
            },
        ),
        (
            "index-member-digest-mismatch",
            "a declared sha256 that does not match the member bytes",
            &|value: &mut Value| {
                value["members"][0]["sha256"] = json!(format!("sha256:{}", "0".repeat(64)));
            },
        ),
        (
            "index-member-size-mismatch",
            "a declared size_bytes that does not match the member bytes",
            &|value: &mut Value| value["members"][0]["size_bytes"] = json!("999999"),
        ),
        (
            "index-leading-zero-size",
            "size_bytes with a leading zero",
            &|value: &mut Value| {
                let size = value["members"][0]["size_bytes"]
                    .as_str()
                    .expect("s")
                    .to_string();
                value["members"][0]["size_bytes"] = json!(format!("0{size}"));
            },
        ),
        (
            "index-numeric-size",
            "size_bytes as a JSON number rather than a decimal string",
            &|value: &mut Value| value["members"][0]["size_bytes"] = json!(1234),
        ),
        (
            "index-members-out-of-order",
            "members not in ascending UTF-8 byte order of path",
            &|value: &mut Value| {
                value["members"] =
                    json!([value["members"][1].clone(), value["members"][0].clone()]);
            },
        ),
        (
            "index-duplicate-member-path",
            "two members declaring the same path under different roles",
            &|value: &mut Value| {
                let mut second = value["members"][1].clone();
                second["path"] = json!(MANIFEST_MEMBER_PATH);
                second["media_type"] = json!(MANIFEST_MEDIA_TYPE);
                value["members"] = json!([value["members"][0].clone(), second]);
            },
        ),
    ] {
        push(
            file,
            note,
            "local",
            "capsule_invalid",
            "envelope",
            index_case(mutate),
        );
    }

    push(
        "index-not-jcs",
        "index.json bytes are pretty-printed, not the JCS canonicalization of their own content",
        "local",
        "capsule_invalid",
        "envelope",
        bundle_with_index(
            &manifest,
            &source,
            serde_json::to_vec_pretty(&index_value(&manifest, &source)).expect("pretty"),
        ),
    );

    let canonical_index_text = String::from_utf8(index_bytes.clone()).expect("utf8");
    push(
        "index-duplicate-json-key-top-level",
        "index.json repeats a top-level key",
        "local",
        "capsule_invalid",
        "envelope",
        bundle_with_index(
            &manifest,
            &source,
            canonical_index_text
                .replacen(
                    "{\"members\"",
                    "{\"schema\":\"ato.capsule-index/v1\",\"members\"",
                    1,
                )
                .into_bytes(),
        ),
    );
    push(
        "index-duplicate-json-key-member",
        "index.json repeats a key inside one member object",
        "local",
        "capsule_invalid",
        "envelope",
        bundle_with_index(
            &manifest,
            &source,
            canonical_index_text
                .replacen(
                    "{\"media_type\":\"application/toml\"",
                    "{\"media_type\":\"application/toml\",\"media_type\":\"application/toml\"",
                    1,
                )
                .into_bytes(),
        ),
    );

    push(
        "manifest-member-tampered",
        "capsule.toml bytes altered post-signing with index.json untouched: a member digest \
         mismatch, caught before signature verification",
        "local",
        "capsule_invalid",
        "envelope",
        assemble(&[
            RawMember::file(
                MANIFEST_MEMBER_PATH,
                format!("{OUTER_MANIFEST}\n# injected\n").into_bytes(),
            ),
            RawMember::file(INDEX_MEMBER_PATH, index_bytes.clone()),
            RawMember::file(SIGNATURE_MEMBER_PATH, signature_bytes.clone()),
            RawMember::file(SOURCE_MEMBER_PATH, source.clone()),
        ]),
    );

    // ── signature.json ───────────────────────────────────────────────────
    let signature_case = |mutate: &dyn Fn(&mut Value)| -> Vec<u8> {
        let mut value = signature_value(&index_bytes, ClaimedIssuer::LocalAuthor);
        mutate(&mut value);
        bundle_with_signature(&manifest, &source, canonical_json(&value))
    };

    for (file, note, mutate) in [
        (
            "signature-wrong-schema",
            "signature.json declares an unknown schema",
            &(|value: &mut Value| value["schema"] = json!("ato.capsule-index-signature/v2"))
                as &dyn Fn(&mut Value),
        ),
        (
            "signature-wrong-algorithm-case",
            "algorithm spelled \"Ed25519\"",
            &|value: &mut Value| value["algorithm"] = json!("Ed25519"),
        ),
        (
            "signature-other-algorithm",
            "a non-ed25519 algorithm",
            &|value: &mut Value| value["algorithm"] = json!("ed25519ph"),
        ),
        (
            "signature-invalid-did-key",
            "key_id is not a canonical did:key",
            &|value: &mut Value| value["key_id"] = json!("did:key:z000"),
        ),
        (
            "signature-unknown-field",
            "an unknown top-level field (previous_key, which Slice 1 has no field for)",
            &|value: &mut Value| value["previous_key"] = json!("did:key:z6Mk"),
        ),
        (
            "signature-padded-base64",
            "signature encoded with base64url padding",
            &|value: &mut Value| {
                let raw = value["signature"].as_str().expect("s").to_string();
                value["signature"] = json!(format!("{raw}=="));
            },
        ),
        (
            "signature-standard-base64",
            "signature encoded with the standard (non-url) base64 alphabet",
            &|value: &mut Value| {
                use base64::Engine as _;
                let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(value["signature"].as_str().expect("s"))
                    .expect("decode");
                value["signature"] =
                    json!(base64::engine::general_purpose::STANDARD.encode(decoded));
            },
        ),
        (
            "signature-wrong-length",
            "signature decodes to 32 bytes rather than 64",
            &|value: &mut Value| {
                use base64::Engine as _;
                value["signature"] =
                    json!(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([1u8; 32]));
            },
        ),
        (
            "signature-index-digest-mismatch",
            "index_digest does not match index.json's bytes",
            &|value: &mut Value| {
                value["index_digest"] = json!(format!("sha256:{}", "0".repeat(64)));
            },
        ),
        (
            "signature-uppercase-index-digest",
            "index_digest spelled with uppercase hex",
            &|value: &mut Value| {
                let digest = value["index_digest"].as_str().expect("d").to_string();
                value["index_digest"] = json!(digest.to_uppercase());
            },
        ),
        (
            "signature-forged-bytes",
            "structurally perfect signature bytes that do not verify",
            &|value: &mut Value| {
                use base64::Engine as _;
                value["signature"] =
                    json!(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([7u8; 64]));
            },
        ),
    ] {
        push(
            file,
            note,
            "local",
            "signature_invalid",
            "envelope",
            signature_case(mutate),
        );
    }

    let canonical_signature_text = String::from_utf8(signature_bytes.clone()).expect("utf8");
    push(
        "signature-duplicate-json-key",
        "signature.json repeats a key",
        "local",
        "signature_invalid",
        "envelope",
        bundle_with_signature(
            &manifest,
            &source,
            canonical_signature_text
                .replacen(
                    "{\"algorithm\":\"ed25519\"",
                    "{\"algorithm\":\"ed25519\",\"algorithm\":\"ed25519\"",
                    1,
                )
                .into_bytes(),
        ),
    );

    // ── trust ────────────────────────────────────────────────────────────
    push(
        "claimed-issuer-ato-store-unpinned-key",
        "claimed_issuer says ato-store but the signing key matches no pin for the declared \
         origin; must resolve to untrusted_key",
        "store",
        "untrusted_signer",
        "envelope",
        write_capsule_bundle_v3(
            &CapsuleBundleWriteInput {
                manifest_bytes: &manifest,
                source_archive_bytes: &source,
                claimed_issuer: ClaimedIssuer::AtoStore,
            },
            &FixedKeySigner::from_seed(0x33),
        )
        .expect("write"),
    );

    // ── derivation ───────────────────────────────────────────────────────
    push(
        "inner-split-brain-locks",
        "the source archive carries both capsule.lock and ato.lock.json at its root",
        "local",
        "capsule_invalid",
        "derivation",
        bundle_from(
            OUTER_MANIFEST,
            &source_archive_with(&[("capsule.lock", "{}\n"), ("ato.lock.json", "{}\n")]),
        ),
    );

    vectors
}
