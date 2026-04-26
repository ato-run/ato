use super::locks::env_lock;
use super::policy::{resolve_auto_bootstrap_policy, AutoBootstrapMode};
use super::*;
use capsule_core::bootstrap::{BootstrapAuthorityKind, BootstrapCacheScope};

#[test]
fn test_parse_engine_filename() {
    let em = EngineManager::new().unwrap();
    let info = em
        .parse_engine_filename("nacelle-v1.2.3-darwin-x64")
        .unwrap();
    assert_eq!(info.name, "nacelle");
    assert_eq!(info.version, "v1.2.3");
    assert_eq!(info.os, "darwin");
    assert_eq!(info.arch, "x64");
}

#[test]
fn test_parse_engine_filename_linux_arm64() {
    let em = EngineManager::new().unwrap();
    let info = em
        .parse_engine_filename("nacelle-v2.0.0-linux-arm64")
        .unwrap();
    assert_eq!(info.name, "nacelle");
    assert_eq!(info.version, "v2.0.0");
    assert_eq!(info.os, "linux");
    assert_eq!(info.arch, "arm64");
}

#[test]
fn test_parse_engine_filename_invalid() {
    let em = EngineManager::new().unwrap();
    let info = em.parse_engine_filename("invalid");
    assert!(info.is_none());
}

#[test]
fn test_parse_engine_filename_too_short() {
    let em = EngineManager::new().unwrap();
    let info = em.parse_engine_filename("nacelle-v1");
    assert!(info.is_none());
}

#[test]
fn test_engine_path() {
    let em = EngineManager::new().unwrap();
    let path = em.engine_path("nacelle", "v1.2.3");
    let path_str = path.to_string_lossy();
    assert!(path_str.contains("nacelle-v1.2.3"));
    assert!(path_str.contains(".ato/engines"));
}

#[test]
fn test_engine_info_serialization() {
    let info = EngineInfo {
        name: "nacelle".to_string(),
        version: "v1.2.3".to_string(),
        url: "https://example.com/nacelle".to_string(),
        sha256: "abc123".to_string(),
        arch: "x64".to_string(),
        os: "darwin".to_string(),
    };

    let serialized = serde_json::to_string(&info).expect("Failed to serialize");
    let deserialized: EngineInfo =
        serde_json::from_str(&serialized).expect("Failed to deserialize");

    assert_eq!(info.name, deserialized.name);
    assert_eq!(info.version, deserialized.version);
    assert_eq!(info.url, deserialized.url);
    assert_eq!(info.sha256, deserialized.sha256);
    assert_eq!(info.arch, deserialized.arch);
    assert_eq!(info.os, deserialized.os);
}

#[test]
fn parse_sha256_for_artifact_supports_sha256sums_format() {
    let body = "\
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  nacelle-v1.2.3-darwin-arm64
bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb  nacelle-v1.2.3-linux-x64
";
    let parsed = parse_sha256_for_artifact(body, "nacelle-v1.2.3-linux-x64");
    assert_eq!(
        parsed.as_deref(),
        Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
    );
}

#[test]
fn parse_sha256_for_artifact_supports_bsd_style_format() {
    let body = "SHA256 (nacelle-v1.2.3-darwin-arm64) = CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC";
    let parsed = parse_sha256_for_artifact(body, "nacelle-v1.2.3-darwin-arm64");
    assert_eq!(
        parsed.as_deref(),
        Some("cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc")
    );
}

#[test]
fn extract_first_sha256_hex_reads_single_file_checksum() {
    let body = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd  nacelle-v1.2.3-darwin-arm64";
    let parsed = extract_first_sha256_hex(body);
    assert_eq!(
        parsed.as_deref(),
        Some("dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd")
    );
}

#[test]
fn auto_bootstrap_policy_defaults_to_pinned_release() {
    let policy = resolve_auto_bootstrap_policy(
        AutoBootstrapMode::Auto,
        PINNED_NACELLE_VERSION.to_string(),
        DEFAULT_NACELLE_RELEASE_BASE_URL.to_string(),
        false,
        false,
    );
    assert!(policy.network_allowed);
    assert_eq!(policy.version, PINNED_NACELLE_VERSION);
    assert_eq!(policy.release_base_url, DEFAULT_NACELLE_RELEASE_BASE_URL);
}

#[test]
fn auto_bootstrap_policy_disables_network_in_ci_by_default() {
    let policy = resolve_auto_bootstrap_policy(
        AutoBootstrapMode::Auto,
        PINNED_NACELLE_VERSION.to_string(),
        DEFAULT_NACELLE_RELEASE_BASE_URL.to_string(),
        true,
        false,
    );
    assert!(!policy.network_allowed);
    assert_eq!(
        policy.disabled_reason.as_deref(),
        Some("CI environment requires prefetched nacelle")
    );
}

#[test]
fn auto_bootstrap_policy_force_mode_overrides_ci() {
    let policy = resolve_auto_bootstrap_policy(
        AutoBootstrapMode::Force,
        PINNED_NACELLE_VERSION.to_string(),
        DEFAULT_NACELLE_RELEASE_BASE_URL.to_string(),
        true,
        false,
    );
    assert!(policy.network_allowed);
    assert!(policy.disabled_reason.is_none());
}

#[test]
fn auto_bootstrap_policy_maps_to_engine_boundary() {
    let policy = resolve_auto_bootstrap_policy(
        AutoBootstrapMode::Auto,
        PINNED_NACELLE_VERSION.to_string(),
        DEFAULT_NACELLE_RELEASE_BASE_URL.to_string(),
        false,
        false,
    );
    let boundary = policy.bootstrap_boundary();
    assert_eq!(
        boundary.authority_kind,
        BootstrapAuthorityKind::NetworkBootstrap
    );
    assert_eq!(boundary.cache_scope, BootstrapCacheScope::EngineCache);
    assert!(boundary.network_policy.network_allowed);
}

#[test]
fn auto_bootstrap_policy_reads_env_overrides() {
    let _guard = env_lock().lock().expect("env lock");
    std::env::set_var(AUTO_BOOTSTRAP_ENV, "true");
    std::env::set_var(NACELLE_VERSION_ENV, "v9.9.9");
    std::env::set_var(
        NACELLE_RELEASE_BASE_URL_ENV,
        "https://mirror.example.com/nacelle/",
    );
    std::env::set_var("CI", "true");

    let policy = resolve_auto_bootstrap_policy_from_env();
    assert!(policy.network_allowed);
    assert_eq!(policy.version, "v9.9.9");
    assert_eq!(
        policy.release_base_url,
        "https://mirror.example.com/nacelle"
    );

    std::env::remove_var(AUTO_BOOTSTRAP_ENV);
    std::env::remove_var(NACELLE_VERSION_ENV);
    std::env::remove_var(NACELLE_RELEASE_BASE_URL_ENV);
    std::env::remove_var("CI");
}
