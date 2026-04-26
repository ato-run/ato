//! Emit the canonical JSON Schema for `Capabilities` on stdout.
//!
//! Run this whenever `schema/capabilities.rs` changes and commit the
//! resulting `schema/capabilities.schema.json`. CI enforces that the
//! checked-in file matches this generator's output.

use capsule_core::schema::capabilities::{Capabilities, SCHEMA_VERSION};

fn main() {
    let mut root = serde_json::to_value(schemars::schema_for!(Capabilities))
        .expect("schema_for! must produce valid JSON");

    // Decorate with a stable $id and title so external consumers can
    // reference this schema by URI from SKILL.md / web-api vendored copies.
    if let Some(obj) = root.as_object_mut() {
        obj.insert(
            "$id".to_string(),
            serde_json::Value::String(format!(
                "https://capsuled.dev/schema/capabilities/v{SCHEMA_VERSION}"
            )),
        );
        obj.insert(
            "title".to_string(),
            serde_json::Value::String("ato Capsule Capabilities".to_string()),
        );
        obj.insert(
            "x-capsuled-schema-version".to_string(),
            serde_json::Value::String(SCHEMA_VERSION.to_string()),
        );
    }

    let out = serde_json::to_string_pretty(&root).expect("serialize schema");
    println!("{out}");
}
