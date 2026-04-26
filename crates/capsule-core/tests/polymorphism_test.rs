use capsule_core::schema_registry::SchemaRegistry;
use capsule_core::types::{CapsuleManifest, PolymorphismConfig};
use serde_json::json;

#[test]
fn implements_schema_resolves_aliases() {
    let mut registry = SchemaRegistry::default();
    let schema_hash = SchemaRegistry::hash_schema_value(&json!({"type":"todo"})).unwrap();
    registry.register_alias("std.todo.v1", &schema_hash);

    let mut manifest = CapsuleManifest::from_json(
        r#"{
        "schema_version":"0.2",
        "name":"todo-app",
        "version":"0.1.0",
        "type":"app",
        "default_target":"cli",
        "targets":{
          "cli":{
            "runtime":"source",
            "driver":"deno",
            "runtime_version":"1.46.3",
            "entrypoint":"main.ts"
          }
        },
        "polymorphism":{"implements":["std.todo.v1"]}
        }"#,
    )
    .unwrap();

    manifest.polymorphism = Some(PolymorphismConfig {
        implements: vec!["std.todo.v1".to_string()],
    });

    let is_match = manifest
        .implements_schema("std.todo.v1", &registry)
        .unwrap();
    assert!(is_match);
}
