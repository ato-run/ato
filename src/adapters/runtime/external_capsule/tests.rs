use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use capsule_core::router::ExecutionProfile;

use super::bindings::{connection_env_vars, merged_dependency_bindings, sanitize_alias};

#[test]
fn builds_parent_connection_env() {
    let env = connection_env_vars("auth-svc", 8080);
    assert_eq!(env["ATO_PKG_AUTH_SVC_HOST"], "127.0.0.1");
    assert_eq!(env["ATO_PKG_AUTH_SVC_PORT"], "8080");
    assert_eq!(env["ATO_PKG_AUTH_SVC_URL"], "http://127.0.0.1:8080");
}

#[test]
fn sanitize_alias_normalizes_non_alnum() {
    assert_eq!(sanitize_alias("api-gateway/v1"), "API_GATEWAY_V1");
}

#[test]
fn cli_bindings_override_locked_dependency_bindings() {
    let mut target = toml::map::Map::new();
    target.insert(
        "runtime".to_string(),
        toml::Value::String("source".to_string()),
    );
    target.insert(
        "driver".to_string(),
        toml::Value::String("native".to_string()),
    );
    target.insert(
        "entrypoint".to_string(),
        toml::Value::String("main.py".to_string()),
    );
    target.insert(
        "external_injection".to_string(),
        toml::Value::Table(toml::map::Map::from_iter([(
            "MODEL_DIR".to_string(),
            toml::Value::Table(toml::map::Map::from_iter([(
                "type".to_string(),
                toml::Value::String("directory".to_string()),
            )])),
        )])),
    );
    let manifest = toml::Value::Table(toml::map::Map::from_iter([
        ("name".to_string(), toml::Value::String("demo".to_string())),
        (
            "default_target".to_string(),
            toml::Value::String("default".to_string()),
        ),
        (
            "targets".to_string(),
            toml::Value::Table(toml::map::Map::from_iter([(
                "default".to_string(),
                toml::Value::Table(target),
            )])),
        ),
    ]));
    let plan = capsule_core::router::execution_descriptor_from_manifest_parts(
        manifest,
        PathBuf::from("capsule.toml"),
        PathBuf::from("."),
        ExecutionProfile::Dev,
        Some("default"),
        HashMap::new(),
    )
    .expect("execution descriptor");
    let locked = capsule_core::lockfile::LockedCapsuleDependency {
        name: "worker".to_string(),
        source: "capsule://store/acme/worker".to_string(),
        source_type: "store".to_string(),
        injection_bindings: BTreeMap::from([(
            "MODEL_DIR".to_string(),
            "https://data.tld/default.zip".to_string(),
        )]),
        resolved_version: Some("1.0.0".to_string()),
        digest: None,
        sha256: None,
        artifact_url: None,
    };
    let cli = BTreeMap::from([("MODEL_DIR".to_string(), "file://./local-model".to_string())]);

    let bindings = merged_dependency_bindings(&plan, &locked, &cli);
    assert_eq!(bindings, vec!["MODEL_DIR=file://./local-model".to_string()]);
}
