use capsule_core::mag_uri::{parse_mag_uri, resolve_mag_uri};
use capsule_core::schema_registry::SchemaRegistry;

#[test]
fn parse_mag_uri_with_schema_hash() {
    let uri = "mag://did:key:z6Mko:sha256:deadbeef/root/path";
    let parsed = parse_mag_uri(uri).unwrap();
    assert_eq!(parsed.did_or_domain, "did:key:z6Mko");
    assert_eq!(parsed.schema_hash.as_deref(), Some("sha256:deadbeef"));
    assert_eq!(parsed.merkle_root.as_deref(), Some("root"));
    assert_eq!(parsed.path.as_deref(), Some("path"));
}

#[test]
fn parse_mag_uri_domain_anchor() {
    let uri = "mag://example.com/resource";
    let parsed = parse_mag_uri(uri).unwrap();
    assert_eq!(parsed.did_or_domain, "example.com");
    assert!(parsed.schema_hash.is_none());
    assert_eq!(parsed.merkle_root.as_deref(), Some("resource"));
}

#[test]
fn resolve_mag_uri_with_schema_hash() {
    let registry = SchemaRegistry::default();
    let uri = "mag://did:key:z6Mko:sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/root/path";
    let resolved = resolve_mag_uri(uri, &registry).unwrap();

    assert_eq!(resolved.did, "did:key:z6Mko");
    assert_eq!(
        resolved.schema_hash.as_deref(),
        Some("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
    );
    assert_eq!(resolved.merkle_root.as_deref(), Some("root"));
    assert_eq!(resolved.path.as_deref(), Some("path"));
}
