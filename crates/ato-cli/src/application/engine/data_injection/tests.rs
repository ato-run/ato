use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use super::*;

fn with_cache_dir(test_name: &str) -> (PathBuf, String) {
    let base = std::env::current_dir()
        .unwrap()
        .join(".ato")
        .join("test-scratch")
        .join(test_name);
    if base.exists() {
        let _ = fs::remove_dir_all(&base);
    }
    fs::create_dir_all(&base).unwrap();
    (base.clone(), base.to_string_lossy().to_string())
}

#[tokio::test]
async fn resolves_string_injection_from_cli_binding() {
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
    let mut external_injection = toml::map::Map::new();
    external_injection.insert(
        "API_KEY".to_string(),
        toml::Value::Table(toml::map::Map::from_iter([(
            "type".to_string(),
            toml::Value::String("string".to_string()),
        )])),
    );
    target.insert(
        "external_injection".to_string(),
        toml::Value::Table(external_injection),
    );
    let manifest = toml::Value::Table(toml::map::Map::from_iter([
        ("name".to_string(), toml::Value::String("demo".to_string())),
        ("type".to_string(), toml::Value::String("app".to_string())),
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
        capsule_core::router::ExecutionProfile::Dev,
        Some("default"),
        HashMap::new(),
    )
    .expect("execution descriptor");

    let resolved = resolve_and_record(&plan, &["API_KEY=test-token".to_string()])
        .await
        .expect("resolve injection");
    assert_eq!(resolved.env["API_KEY"], "test-token");
}

#[tokio::test]
async fn resolves_directory_injection_from_file_uri() {
    let (cache_dir, cache_dir_string) = with_cache_dir("data-injection-dir");
    std::env::set_var(ENV_INJECTED_DATA_CACHE_DIR, &cache_dir_string);
    let fixture_root = cache_dir.join("fixture");
    fs::create_dir_all(&fixture_root).unwrap();
    fs::write(fixture_root.join("weights.bin"), b"abc").unwrap();

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
    let mut external_injection = toml::map::Map::new();
    external_injection.insert(
        "MODEL_DIR".to_string(),
        toml::Value::Table(toml::map::Map::from_iter([(
            "type".to_string(),
            toml::Value::String("directory".to_string()),
        )])),
    );
    target.insert(
        "external_injection".to_string(),
        toml::Value::Table(external_injection),
    );
    let manifest = toml::Value::Table(toml::map::Map::from_iter([
        ("name".to_string(), toml::Value::String("demo".to_string())),
        ("type".to_string(), toml::Value::String("app".to_string())),
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
        cache_dir.join("capsule.toml"),
        cache_dir.clone(),
        capsule_core::router::ExecutionProfile::Dev,
        Some("default"),
        HashMap::new(),
    )
    .expect("execution descriptor");

    let resolved = resolve_and_record(
        &plan,
        &[format!("MODEL_DIR=file://{}", fixture_root.display())],
    )
    .await
    .expect("resolve injection");

    let injected_path = PathBuf::from(&resolved.env["MODEL_DIR"]);
    assert!(injected_path.exists());
    assert!(injected_path.join("weights.bin").exists());
    std::env::remove_var(ENV_INJECTED_DATA_CACHE_DIR);
}

#[tokio::test]
async fn resolves_oci_file_injection_as_mount() {
    let (cache_dir, cache_dir_string) = with_cache_dir("data-injection-oci-file");
    std::env::set_var(ENV_INJECTED_DATA_CACHE_DIR, &cache_dir_string);
    let fixture = cache_dir.join("config.json");
    fs::write(&fixture, b"{}\n").unwrap();

    let mut target = toml::map::Map::new();
    target.insert(
        "runtime".to_string(),
        toml::Value::String("oci".to_string()),
    );
    target.insert(
        "image".to_string(),
        toml::Value::String("ghcr.io/example/demo:latest".to_string()),
    );
    target.insert(
        "external_injection".to_string(),
        toml::Value::Table(toml::map::Map::from_iter([(
            "CONFIG_FILE".to_string(),
            toml::Value::Table(toml::map::Map::from_iter([(
                "type".to_string(),
                toml::Value::String("file".to_string()),
            )])),
        )])),
    );
    let manifest = toml::Value::Table(toml::map::Map::from_iter([
        ("name".to_string(), toml::Value::String("demo".to_string())),
        ("type".to_string(), toml::Value::String("app".to_string())),
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
        cache_dir.join("capsule.toml"),
        cache_dir.clone(),
        capsule_core::router::ExecutionProfile::Dev,
        Some("default"),
        HashMap::new(),
    )
    .expect("execution descriptor");

    let resolved = resolve_and_record(
        &plan,
        &[format!("CONFIG_FILE=file://{}", fixture.display())],
    )
    .await
    .expect("resolve injection");

    assert_eq!(
        resolved.env["CONFIG_FILE"],
        "/var/run/ato/injected/CONFIG_FILE"
    );
    assert_eq!(resolved.mounts.len(), 1);
    assert_eq!(
        resolved.mounts[0].target,
        "/var/run/ato/injected/CONFIG_FILE"
    );
    assert!(resolved.mounts[0].readonly);
    std::env::remove_var(ENV_INJECTED_DATA_CACHE_DIR);
}
