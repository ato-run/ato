//! Wire v2/v3/v4 dispatch and profile gates fail closed.
//!
//! Every expectation here is a rejection. The decoded digests are never checked
//! in these documents, so each case shows that the *gate itself* refuses the
//! document before any content is read: a wrong profile is not excused by a
//! later digest failure, and a v2/v3/v4 document cannot be read as another
//! version's shape.
//!
//! Run: see formal/lean-bugcheck/run.sh

use ato_objects::{
    CapsuleBundleDocument, CapsuleBundleDocumentError, BundleError, PortableBundleError,
    decode_capsule_bundle_document,
};

fn hex(c: char) -> String {
    format!("sha256:{}", c.to_string().repeat(64))
}

fn blake3_hex(c: char) -> String {
    format!("blake3:{}", c.to_string().repeat(64))
}

fn index(fields: &[(&str, String)]) -> String {
    let mut sorted: Vec<(String, String)> =
        fields.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    sorted.sort_by(|left, right| left.0.cmp(&right.0));
    format!(
        "{{{}}}",
        sorted
            .iter()
            .map(|(k, v)| format!("\"{k}\":{v}"))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn bundle(index: &str, payloads: &str, portability: Option<&str>) -> String {
    let mut parts = vec![
        format!("\"index\":{index}"),
        format!("\"payloads\":{payloads}"),
    ];
    if let Some(value) = portability {
        parts.push(format!("\"portability\":{value}"));
    }
    parts.sort();
    format!("{{{}}}", parts.join(","))
}

fn refs(values: &[String]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| format!("\"{value}\""))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn descriptor(reference: &str, kind: &str, schema: Option<&str>, size: u64) -> String {
    let schema = schema
        .map(|schema| format!(",\"schema\":\"{schema}\""))
        .unwrap_or_default();
    format!("{{\"kind\":\"{kind}\",\"reference\":\"{reference}\"{schema},\"size\":{size}}}")
}

fn payload(reference: &str, bytes: &str) -> String {
    format!("{{\"bytes\":\"{bytes}\",\"reference\":\"{reference}\"}}")
}

const PROFILE_V3: &str = "\"ato.portable-application/1\"";
const PROFILE_V4: &str = "\"ato.portable-application/2\"";

/// A v3 index with the given extra/override fields.
fn v3(fields: &[(&str, String)], derivations: &[String]) -> String {
    let mut all: Vec<(&str, String)> = vec![
        ("application_ref", format!("\"{}\"", hex('a'))),
        ("profile", PROFILE_V3.to_owned()),
        ("root_contract_ref", format!("\"{}\"", hex('b'))),
        ("version", "3".to_owned()),
        ("derivations", refs(derivations)),
        ("objects", "[]".to_owned()),
    ];
    for (key, value) in fields {
        all.retain(|(existing, _)| existing != key);
        all.push((key, value.clone()));
    }
    index(&all)
}

struct Case {
    name: &'static str,
    document: String,
    expect: fn(&CapsuleBundleDocumentError) -> bool,
    expect_name: &'static str,
}

fn is_unsupported_profile(error: &CapsuleBundleDocumentError) -> bool {
    matches!(
        error,
        CapsuleBundleDocumentError::PortableV3(PortableBundleError::UnsupportedProfile(_))
    )
}

fn is_missing_derivation(error: &CapsuleBundleDocumentError) -> bool {
    matches!(
        error,
        CapsuleBundleDocumentError::PortableV3(PortableBundleError::MissingDerivation)
    )
}

fn is_noncanonical_order(error: &CapsuleBundleDocumentError) -> bool {
    matches!(
        error,
        CapsuleBundleDocumentError::PortableV3(PortableBundleError::NonCanonicalOrder)
    )
}

fn is_duplicate_root(error: &CapsuleBundleDocumentError) -> bool {
    matches!(
        error,
        CapsuleBundleDocumentError::PortableV3(PortableBundleError::DuplicateRoot(_))
    )
}

fn is_object_count(error: &CapsuleBundleDocumentError) -> bool {
    matches!(
        error,
        CapsuleBundleDocumentError::PortableV3(PortableBundleError::DescriptorMismatch(_))
    )
}

fn is_incomplete_closure(error: &CapsuleBundleDocumentError) -> bool {
    matches!(
        error,
        CapsuleBundleDocumentError::PortableV3(PortableBundleError::IncompleteClosure(_))
    )
}

fn is_invalid_object_kind(error: &CapsuleBundleDocumentError) -> bool {
    matches!(
        error,
        CapsuleBundleDocumentError::PortableV3(PortableBundleError::InvalidObjectKind(_))
    )
}

fn is_invalid_external(error: &CapsuleBundleDocumentError) -> bool {
    matches!(
        error,
        CapsuleBundleDocumentError::PortableV3(PortableBundleError::InvalidExternalObject(_))
    )
}

fn is_unsupported_version(error: &CapsuleBundleDocumentError) -> bool {
    matches!(error, CapsuleBundleDocumentError::UnsupportedVersion(5))
}

fn is_json(error: &CapsuleBundleDocumentError) -> bool {
    matches!(
        error,
        CapsuleBundleDocumentError::Json(_)
            | CapsuleBundleDocumentError::BundleV2(BundleError::Json(_))
            | CapsuleBundleDocumentError::PortableV3(PortableBundleError::Json(_))
    )
}

fn cases() -> Vec<Case> {
    let mut out = Vec::new();

    // 1. A v3 index labelled with the v4 profile.
    let bad_profile = v3(
        &[("profile", PROFILE_V4.to_owned())],
        &[hex('c')],
    );
    out.push(Case {
        name: "v3 index with the v4 profile",
        document: bundle(&bad_profile, "[]", None),
        expect: is_unsupported_profile,
        expect_name: "UnsupportedProfile",
    });

    // 2. A v4 index labelled with the v3 profile.
    let v4_fields = vec![
        ("application_ref", format!("\"{}\"", hex('a'))),
        ("derivations", refs(&[hex('c')])),
        ("objects", "[]".to_owned()),
        ("profile", "\"ato.portable-application/1\"".to_owned()),
        ("root_contract_ref", format!("\"{}\"", hex('b'))),
        ("version", "4".to_owned()),
    ];
    out.push(Case {
        name: "v4 index with the v3 profile",
        document: bundle(
            &index(&v4_fields),
            "[]",
            Some("{\"external_objects\":[],\"profile\":\"cached\"}"),
        ),
        expect: is_unsupported_profile,
        expect_name: "UnsupportedProfile",
    });

    // 3. v3 carrying the v4-only dependency transport manifest.
    let v3_index = v3(&[], &[hex('c')]);
    out.push(Case {
        name: "v3 index with a portability manifest",
        document: bundle(
            &v3_index,
            "[]",
            Some("{\"external_objects\":[],\"profile\":\"thin\"}"),
        ),
        expect: is_unsupported_profile,
        expect_name: "UnsupportedProfile",
    });

    // 4. v3 carrying the v4-only Instance snapshot reference.
    let snapshot_index = v3(
        &[("instance_snapshot_ref", format!("\"{}\"", hex('e')))],
        &[hex('c')],
    );
    out.push(Case {
        name: "v3 index with instance_snapshot_ref",
        document: bundle(&snapshot_index, "[]", None),
        expect: is_unsupported_profile,
        expect_name: "UnsupportedProfile",
    });

    // 5. v4 without the dependency transport manifest.
    let v4_fields_ok = vec![
        ("application_ref", format!("\"{}\"", hex('a'))),
        ("derivations", refs(&[hex('c')])),
        ("objects", "[]".to_owned()),
        ("profile", PROFILE_V4.to_owned()),
        ("root_contract_ref", format!("\"{}\"", hex('b'))),
        ("version", "4".to_owned()),
    ];
    out.push(Case {
        name: "v4 index without a portability manifest",
        document: bundle(&index(&v4_fields_ok), "[]", None),
        expect: is_object_count,
        expect_name: "DescriptorMismatch (missing portability manifest)",
    });

    // 6. No Derivation at all.
    let empty = v3(&[], &[]);
    out.push(Case {
        name: "v3 index with no Derivation",
        document: bundle(&empty, "[]", None),
        expect: is_missing_derivation,
        expect_name: "MissingDerivation",
    });

    // 7. Derivations out of order.
    let unsorted = v3(&[], &[hex('d'), hex('c')]);
    out.push(Case {
        name: "unsorted Derivations",
        document: bundle(&unsorted, "[]", None),
        expect: is_noncanonical_order,
        expect_name: "NonCanonicalOrder",
    });

    // 8. A repeated Derivation root.
    let repeated = v3(&[], &[hex('c'), hex('c')]);
    out.push(Case {
        name: "duplicate Derivation roots",
        document: bundle(&repeated, "[]", None),
        expect: is_duplicate_root,
        expect_name: "DuplicateRoot",
    });

    // 9. One descriptor with no payload.
    let counted = v3(
        &[(
            "objects",
            format!("[{}]", descriptor(&hex('c'), "blob", None, 0)),
        )],
        &[hex('c')],
    );
    out.push(Case {
        name: "descriptor count does not match payloads",
        document: bundle(&counted, "[]", None),
        expect: is_object_count,
        expect_name: "DescriptorMismatch (object count)",
    });

    // 10. A root that is declared as an opaque Blob.
    let root_is_blob = index(&[
        ("application_ref", format!("\"{}\"", hex('a'))),
        ("derivations", refs(&[hex('d')])),
        ("objects", format!("[{}]", descriptor(&hex('c'), "blob", None, 0))),
        ("profile", "\"ato.portable-application/1\"".to_owned()),
        ("root_contract_ref", format!("\"{}\"", hex('c'))),
        ("version", "3".to_owned()),
    ]);
    out.push(Case {
        name: "root_contract_ref names a Blob descriptor",
        document: bundle(&root_is_blob, &format!("[{}]", payload(&hex('c'), "")), None),
        expect: is_invalid_object_kind,
        expect_name: "InvalidObjectKind",
    });

    // 11. A v4 external object that is declared as structured.
    let external_structured = index(&[
        ("application_ref", format!("\"{}\"", hex('c'))),
        ("derivations", refs(&[hex('c')])),
        ("objects", format!("[{}]", descriptor(&hex('c'), "structured", Some("ato.contract/1"), 0))),
        ("profile", "\"ato.portable-application/2\"".to_owned()),
        ("root_contract_ref", format!("\"{}\"", hex('c'))),
        ("version", "4".to_owned()),
    ]);
    out.push(Case {
        name: "v4 external object declared structured",
        document: bundle(
            &external_structured,
            "[]",
            Some(&format!(
                "{{\"external_objects\":[{{\"reference\":\"{}\",\"sources\":[\"https://example.test/o\"]}}],\"profile\":\"thin\"}}",
                hex('c')
            )),
        ),
        expect: is_invalid_external,
        expect_name: "InvalidExternalObject",
    });

    // 12. The profile gate passes; the closure check is what refuses the route.
    let unresolved = v3(&[], &[hex('c')]);
    out.push(Case {
        name: "v3 profile accepted, closure refused",
        document: bundle(&unresolved, "[]", None),
        expect: is_incomplete_closure,
        expect_name: "IncompleteClosure (the profile gate itself passes)",
    });

    // 13. An unknown wire version.
    let mut unknown = v3(&[], &[hex('c')]);
    unknown = unknown.replace("\"version\":3", "\"version\":5");
    out.push(Case {
        name: "unknown wire version 5",
        document: bundle(&unknown, "[]", None),
        expect: is_unsupported_version,
        expect_name: "UnsupportedVersion(5)",
    });

    // 14. A v3-only field inside a v2 index.
    out.push(Case {
        name: "v2 index with a v3-only field",
        document: format!(
            "{{\"index\":{{\"objects\":[],\"profile\":\"ato.portable-application/1\",\"root\":\"{}\",\"version\":2}},\"payloads\":[]}}",
            blake3_hex('a')
        ),
        expect: is_json,
        expect_name: "Json (deny_unknown_fields)",
    });

    // 15. A v2-only field inside a v3 index.
    let mixed = v3(&[("root", format!("\"{}\"", blake3_hex('a')))], &[hex('c')]);
    out.push(Case {
        name: "v3 index with the v2 root field",
        document: bundle(&mixed, "[]", None),
        expect: is_json,
        expect_name: "Json (deny_unknown_fields)",
    });

    out
}

fn main() {
    for case in cases() {
        match decode_capsule_bundle_document(case.document.as_bytes()) {
            Ok(document) => panic!(
                "{}: accepted as {:?}, expected {}",
                case.name, document, case.expect_name
            ),
            Err(error) => {
                assert!(
                    (case.expect)(&error),
                    "{}: got {:?}, expected {}",
                    case.name,
                    error,
                    case.expect_name
                );
                println!("refused {} -> {}", case.name, case.expect_name);
            }
        }
    }

    // Positive control: a v2 document is still read by the v2 reader.
    let v2 = format!(
        "{{\"index\":{{\"objects\":[],\"root\":\"{}\",\"version\":2}},\"payloads\":[]}}",
        blake3_hex('a')
    );
    match decode_capsule_bundle_document(v2.as_bytes()).expect("v2 document decodes") {
        CapsuleBundleDocument::ComputationV2(_) => println!("v2 document -> ComputationV2"),
        other => panic!("v2 document dispatched to {other:?}"),
    }

    // Positive control: a v3 document with a matching profile reaches the
    // profile-specific reader rather than the v2 one.
    let good = v3(&[], &[hex('c')]);
    match decode_capsule_bundle_document(bundle(&good, "[]", None).as_bytes()) {
        Err(error) => {
            assert!(is_incomplete_closure(&error), "unexpected error {error:?}");
            println!("v3 document -> PortableV3 reader (closure incomplete)");
        }
        Ok(other) => panic!("v3 document dispatched to {other:?}"),
    }

    println!("ALL WAYS REFUSED: no cross-version or cross-profile acceptance observed");
}