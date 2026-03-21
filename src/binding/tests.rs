use std::fs;

use capsule_core::types::CapsuleManifest;

use super::contract::{
    auto_bindable_service_names, derive_service_endpoint_locator, derive_service_upstream_locator,
    ingress_binding_contract, local_service_binding_contract, normalize_endpoint_locator,
    normalize_local_service_locator, SERVICE_BINDING_KIND_SERVICE,
};
use super::manifest::load_manifest;
use super::parse_binding_reference;

#[test]
fn parse_binding_reference_accepts_bare_binding_id() {
    assert_eq!(
        parse_binding_reference("binding-demo"),
        Some("binding-demo")
    );
    assert_eq!(parse_binding_reference("https://example.com"), None);
}

#[test]
fn normalize_endpoint_locator_requires_http_or_https() {
    assert_eq!(
        normalize_endpoint_locator("https://example.com/api").expect("normalize https"),
        "https://example.com/api"
    );
    assert!(normalize_endpoint_locator("tcp://127.0.0.1:8080").is_err());
}

#[test]
fn normalize_local_service_locator_requires_loopback_host() {
    assert_eq!(
        normalize_local_service_locator("http://127.0.0.1:8080/").expect("loopback"),
        "http://127.0.0.1:8080/"
    );
    assert!(normalize_local_service_locator("https://example.com/api").is_err());
}

#[test]
fn ingress_binding_contract_carries_allow_from_metadata() {
    let manifest = CapsuleManifest::from_toml(
        r#"
schema_version = "0.2"
name = "demo-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:latest"

[services.api]
target = "app"
network = { publish = true, allow_from = ["web", "worker"] }
"#,
    )
    .expect("manifest");

    let contract =
        ingress_binding_contract(&manifest, "api", "https://demo.local/").expect("contract");
    assert_eq!(contract.allowed_callers, vec!["web", "worker"]);
}

#[test]
fn local_service_binding_contract_allows_non_published_services() {
    let manifest = CapsuleManifest::from_toml(
        r#"
schema_version = "0.2"
name = "demo-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:latest"

[services.api]
target = "app"
network = { allow_from = ["web"] }
"#,
    )
    .expect("manifest");

    let contract = local_service_binding_contract(&manifest, "api", "http://127.0.0.1:4310/")
        .expect("contract");
    assert_eq!(contract.binding_kind, SERVICE_BINDING_KIND_SERVICE);
    assert_eq!(contract.allowed_callers, vec!["web"]);
}

#[test]
fn derive_service_upstream_locator_uses_target_port() {
    let manifest = CapsuleManifest::from_toml(
        r#"
schema_version = "0.2"
name = "demo-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:latest"
port = 4310

[services.main]
target = "app"
network = { publish = true }
"#,
    )
    .expect("manifest");

    let upstream = derive_service_upstream_locator(&manifest, "main").expect("upstream");
    assert_eq!(upstream, "http://127.0.0.1:4310/");
}

#[test]
fn derive_service_endpoint_locator_honors_target_and_port_overrides() {
    let manifest = CapsuleManifest::from_toml(
        r#"
schema_version = "0.2"
name = "demo-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:latest"
port = 4310

[targets.alt]
runtime = "oci"
image = "ghcr.io/example/app:alt"
port = 5320

[services.api]
network = { allow_from = ["web"] }
"#,
    )
    .expect("manifest");

    let derived = derive_service_endpoint_locator(&manifest, "api", Some("alt"), None)
        .expect("derived endpoint");
    assert_eq!(derived, "http://127.0.0.1:5320/");

    let overridden = derive_service_endpoint_locator(&manifest, "api", Some("alt"), Some(6123))
        .expect("overridden endpoint");
    assert_eq!(overridden, "http://127.0.0.1:6123/");
}

#[test]
fn load_manifest_reads_capsule_artifact() {
    let dir = tempfile::tempdir().expect("tempdir");
    let capsule_path = dir.path().join("demo.capsule");
    let file = fs::File::create(&capsule_path).expect("create capsule");
    let mut builder = tar::Builder::new(file);
    let manifest = r#"
schema_version = "0.2"
name = "demo-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:latest"
port = 4310

[services.main]
network = { publish = true }
"#;
    let mut header = tar::Header::new_gnu();
    header.set_size(manifest.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, "capsule.toml", manifest.as_bytes())
        .expect("append manifest");
    builder.finish().expect("finish archive");

    let loaded = load_manifest(&capsule_path).expect("load artifact manifest");
    assert_eq!(loaded.name, "demo-app");
}

#[test]
fn auto_bindable_service_names_select_publish_and_allow_from() {
    let manifest = CapsuleManifest::from_toml(
        r#"
schema_version = "0.2"
name = "demo-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:latest"
port = 4310

[services.main]
network = { publish = true }

[services.api]
network = { allow_from = ["main"] }

[services.worker]
network = {}
"#,
    )
    .expect("manifest");

    assert_eq!(auto_bindable_service_names(&manifest), vec!["api", "main"]);
}
