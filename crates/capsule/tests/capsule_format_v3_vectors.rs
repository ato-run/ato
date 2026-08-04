//! Replays the committed `capsule_format_v3/` golden vectors through the
//! **public** `capsule::import_bundle` API only.
//!
//! The in-crate suite (`src/import_bundle/tests.rs`) builds its bundles in
//! memory and can reach crate internals. This one deliberately cannot: it reads
//! the same fixed bytes a second implementation would read, drives them through
//! the same entry points a real caller has, and asserts the same verdicts. That
//! is the shape the ato-api-side TypeScript conformance suite will mirror, and
//! the reason the vector bytes are committed rather than generated at test time.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use capsule::import_bundle::{
    CapsuleImportContext, CapsuleImportError, CapsuleImportPolicy, CapsuleTrustPolicy, DidKey,
    NormalizedOrigin, PinnedStoreOrigin, Sha256Digest, SignerTrust, derive_imported_capsule,
    verify_capsule_envelope,
};
use serde_json::Value;

fn vector_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/capsule_format_v3")
}

fn vector_manifest() -> Value {
    let raw = fs::read(vector_dir().join("manifest.json")).expect("read vector manifest");
    serde_json::from_slice(&raw).expect("parse vector manifest")
}

fn local_context() -> CapsuleImportContext {
    CapsuleImportContext::LocalFile {
        expected_bundle_digest: None,
    }
}

/// A local policy that hands back the classification instead of refusing an
/// unrecognized signer — the CLI's posture, where a prompt happens above this
/// layer.
fn local_policy() -> CapsuleTrustPolicy {
    CapsuleTrustPolicy::new().accepting_untrusted_local_signers()
}

/// A Store policy pinning a key that signs nothing in this fixture set, so a
/// `store`-context vector can only ever resolve to `untrusted_key`.
fn store_policy() -> CapsuleTrustPolicy {
    let unrelated = DidKey::from_public_key(&[0xAB; 32]);
    CapsuleTrustPolicy::new().with_store_pins(
        PinnedStoreOrigin::new(
            NormalizedOrigin::parse("https://api.ato.run").expect("origin"),
            vec![unrelated],
        )
        .expect("pin array"),
    )
}

fn store_context(bytes: &[u8]) -> CapsuleImportContext {
    CapsuleImportContext::Store {
        api_origin: NormalizedOrigin::parse("https://api.ato.run").expect("origin"),
        expected_bundle_digest: Sha256Digest::of_bytes(bytes),
    }
}

fn error_code(error: &CapsuleImportError) -> &'static str {
    match error {
        CapsuleImportError::CapsuleInvalid(_) => "capsule_invalid",
        CapsuleImportError::SignatureInvalid(_) => "signature_invalid",
        CapsuleImportError::BundleDigestMismatch { .. } => "bundle_digest_mismatch",
        CapsuleImportError::ResourceBudgetExceeded(_) => "resource_budget_exceeded",
        CapsuleImportError::InsufficientLocalStorage(_) => "insufficient_local_storage",
        CapsuleImportError::UntrustedSigner(_) => "untrusted_signer",
        CapsuleImportError::NotV3Bundle => "not_v3_bundle",
        CapsuleImportError::Io(_) => "io",
    }
}

fn field<'a>(entry: &'a Value, name: &str) -> &'a str {
    entry[name]
        .as_str()
        .unwrap_or_else(|| panic!("vector entry is missing a string `{name}`: {entry}"))
}

#[test]
fn every_valid_vector_imports_to_the_baseline_identity_and_workspace() {
    let manifest = vector_manifest();
    let entries = manifest["valid"].as_array().expect("valid array");
    assert!(
        !entries.is_empty(),
        "the valid vector set must not be empty"
    );

    let mut baseline: Option<ImportSnapshot> = None;
    for entry in entries {
        let file = field(entry, "file");
        let note = field(entry, "note");
        let bytes = fs::read(vector_dir().join("valid").join(file))
            .unwrap_or_else(|source| panic!("read valid vector {file}: {source}"));

        let envelope = verify_capsule_envelope(
            Cursor::new(bytes.clone()),
            local_context(),
            &local_policy(),
            &CapsuleImportPolicy::unbounded(),
        )
        .unwrap_or_else(|error| panic!("{file} ({note}) must verify, got {error}"));
        assert_eq!(
            envelope.signer_trust(),
            SignerTrust::UntrustedKey,
            "{file}: a locally exported bundle is signed by an unrecognizable key"
        );
        assert_eq!(envelope.bundle_digest(), Sha256Digest::of_bytes(&bytes));

        let workspace = derive_imported_capsule(envelope)
            .unwrap_or_else(|error| panic!("{file} ({note}) must import, got {error}"))
            .into_workspace();
        let observed = (
            workspace.capsule_program_id().to_string(),
            workspace_contents(workspace.path()),
        );

        // "Same as the no-inner-control-file case" is the actual claim the RFC
        // makes about every valid inner-control-file shape, so it is what gets
        // asserted — not merely that the import succeeded.
        match baseline.as_ref() {
            None => baseline = Some(observed),
            Some(expected) => assert_eq!(
                &observed, expected,
                "{file} ({note}) must be indistinguishable from the baseline import"
            ),
        }
    }
}

#[test]
fn every_invalid_vector_is_rejected_with_its_declared_category() {
    let manifest = vector_manifest();
    let entries = manifest["invalid"].as_array().expect("invalid array");
    assert!(
        !entries.is_empty(),
        "the invalid vector set must not be empty"
    );

    for entry in entries {
        let file = field(entry, "file");
        let note = field(entry, "note");
        let stage = field(entry, "stage");
        let expected = field(entry, "expected_error");
        let bytes = fs::read(vector_dir().join("invalid").join(file))
            .unwrap_or_else(|source| panic!("read invalid vector {file}: {source}"));

        let (context, policy) = match field(entry, "context") {
            "local" => (local_context(), local_policy()),
            "store" => (store_context(&bytes), store_policy()),
            other => panic!("{file}: unknown vector context {other:?}"),
        };

        let verified = verify_capsule_envelope(
            Cursor::new(bytes),
            context,
            &policy,
            &CapsuleImportPolicy::unbounded(),
        );

        match stage {
            "envelope" => match verified {
                Err(error) => assert_eq!(
                    error_code(&error),
                    expected,
                    "{file} ({note}) rejected with the wrong category: {error}"
                ),
                Ok(_) => panic!("{file} ({note}) must be rejected during envelope verification"),
            },
            "derivation" => {
                let envelope = verified.unwrap_or_else(|error| {
                    panic!("{file} ({note}) must verify before failing derivation, got {error}")
                });
                match derive_imported_capsule(envelope) {
                    Err(error) => assert_eq!(
                        error_code(&error),
                        expected,
                        "{file} ({note}) rejected with the wrong category: {error}"
                    ),
                    Ok(_) => panic!("{file} ({note}) must be rejected during derivation"),
                }
            }
            other => panic!("{file}: unknown vector stage {other:?}"),
        }
    }
}

/// Every committed file must be described by `manifest.json`, and vice versa —
/// otherwise a vector could be added and silently never run.
#[test]
fn the_vector_manifest_and_the_committed_files_agree() {
    let manifest = vector_manifest();
    for (bucket, dir) in [("valid", "valid"), ("invalid", "invalid")] {
        let declared: Vec<String> = manifest[bucket]
            .as_array()
            .expect("bucket array")
            .iter()
            .map(|entry| field(entry, "file").to_string())
            .collect();
        let mut on_disk: Vec<String> = fs::read_dir(vector_dir().join(dir))
            .expect("read vector dir")
            .map(|entry| {
                entry
                    .expect("dir entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        on_disk.sort();
        let mut declared_sorted = declared.clone();
        declared_sorted.sort();
        assert_eq!(
            declared_sorted, on_disk,
            "the {bucket} vector manifest and the committed files must match exactly"
        );
    }
}

/// A `(capsule_program_id, workspace file set)` pair — what every valid vector
/// must agree on with the baseline.
type ImportSnapshot = (String, Vec<(String, Vec<u8>)>);

fn workspace_contents(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut entries = Vec::new();
    collect(root, "", &mut entries);
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    return entries;

    fn collect(dir: &Path, prefix: &str, out: &mut Vec<(String, Vec<u8>)>) {
        let mut paths: Vec<PathBuf> = fs::read_dir(dir)
            .expect("read workspace dir")
            .map(|entry| entry.expect("dir entry").path())
            .collect();
        paths.sort();
        for path in paths {
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
