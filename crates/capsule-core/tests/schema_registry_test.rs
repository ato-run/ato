use capsule_core::schema_registry::SchemaRegistry;
use serde_json::json;

#[test]
fn schema_hash_is_deterministic() {
    let first = json!({"b": 1, "a": 2});
    let second = json!({"a": 2, "b": 1});

    let hash_first = SchemaRegistry::hash_schema_value(&first).unwrap();
    let hash_second = SchemaRegistry::hash_schema_value(&second).unwrap();

    assert_eq!(hash_first, hash_second);
    assert!(hash_first.starts_with("sha256:"));
}
