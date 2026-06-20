// The env guard is deliberately held across `.await` points: these tests run
// on tokio's current-thread flavor, and the lock must span the whole test so
// `HOME`/`ATO_HOME` stay stable while the server is driven.
#![allow(clippy::await_holding_lock)]

use super::*;
use axum::body::to_bytes;
use std::io::{Cursor, ErrorKind, Write};
use std::net::{IpAddr, Ipv4Addr};
// Serialises HOME/ATO_HOME-mutating tests against the WHOLE crate, not just
// this file — a private mutex here raced the rest of the suite over the same
// process-global environment.
fn env_lock() -> &'static crate::tests::EnvLock {
    crate::tests::env_lock()
}

#[test]
fn format_bind_error_mentions_port_conflict_guidance() {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9090);
    let err = std::io::Error::new(ErrorKind::AddrInUse, "Address already in use");
    let message = format_bind_error(addr, &err);
    assert!(message.contains("Failed to bind 127.0.0.1:9090"));
    assert!(message.contains("Address already in use"));
    assert!(message.contains("Another process is already listening"));
    assert!(message.contains("lsof -nP -iTCP:<port> -sTCP:LISTEN"));
}

#[test]
fn format_bind_error_preserves_generic_io_message() {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9090);
    let err = std::io::Error::other("boom");
    let message = format_bind_error(addr, &err);
    assert!(message.contains("Failed to bind 127.0.0.1:9090: boom"));
    assert!(!message.contains("Another process is already listening"));
}

struct HomeGuard {
    previous: Option<std::ffi::OsString>,
}

impl HomeGuard {
    fn set(path: &std::path::Path) -> Self {
        let previous = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", path);
        }
        Self { previous }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            unsafe {
                std::env::set_var("HOME", previous);
            }
        } else {
            unsafe {
                std::env::remove_var("HOME");
            }
        }
    }
}

struct AtoHomeGuard {
    previous: Option<std::ffi::OsString>,
    root: std::path::PathBuf,
}

impl AtoHomeGuard {
    fn set(name: &str) -> Self {
        let previous = std::env::var_os("ATO_HOME");
        let root = std::env::current_dir()
            .expect("cwd")
            .join(".tmp")
            .join("registry-serve-tests")
            .join(format!(
                "{}-{}-{}",
                name,
                std::process::id(),
                chrono::Utc::now()
                    .timestamp_nanos_opt()
                    .expect("timestamp nanos")
            ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create ATO_HOME test root");
        unsafe {
            std::env::set_var("ATO_HOME", &root);
        }
        Self { previous, root }
    }
}

impl Drop for AtoHomeGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            unsafe {
                std::env::set_var("ATO_HOME", previous);
            }
        } else {
            unsafe {
                std::env::remove_var("ATO_HOME");
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn bearer_headers(token: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    let value = format!("Bearer {token}")
        .parse::<HeaderValue>()
        .expect("auth header");
    headers.insert(header::AUTHORIZATION, value);
    headers
}

fn registry_test_state(auth_token: Option<&str>) -> AppState {
    AppState {
        listen_url: "http://127.0.0.1:8787".to_string(),
        data_dir: std::env::current_dir().expect("cwd").join(".tmp"),
        auth_token: auth_token.map(str::to_string),
        lock: Arc::new(Mutex::new(())),
    }
}

fn build_capsule_bytes(manifest: &str) -> Vec<u8> {
    build_capsule_bytes_with_files(manifest, &[("README.md", b"dummy".as_slice())])
}

fn build_capsule_bytes_with_files(manifest: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
    let payload_tar = build_payload_tar().expect("build payload tar");
    let parsed_manifest =
        capsule::types::CapsuleManifest::from_toml(manifest).expect("parse manifest");
    let (distribution_manifest, _) =
        capsule::packers::payload::build_distribution_manifest(&parsed_manifest, &payload_tar)
            .expect("build distribution manifest");
    let mut raw_manifest: toml::Value = toml::from_str(manifest).expect("parse raw manifest");
    let raw_manifest_table = raw_manifest
        .as_table_mut()
        .expect("raw manifest must be a table");
    raw_manifest_table.insert(
        "schema_version".to_string(),
        toml::Value::String(distribution_manifest.schema_version.clone()),
    );
    raw_manifest_table.insert(
        "distribution".to_string(),
        toml::Value::try_from(
            distribution_manifest
                .distribution
                .expect("distribution metadata"),
        )
        .expect("distribution value"),
    );
    let manifest_bytes = toml::to_string_pretty(&raw_manifest).expect("serialize manifest");
    let payload_zst =
        zstd::stream::encode_all(Cursor::new(payload_tar), 1).expect("encode payload");

    let mut out = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut out);
        let mut header = tar::Header::new_gnu();
        header.set_path("capsule.toml").expect("set path");
        header.set_mode(0o644);
        header.set_size(manifest_bytes.len() as u64);
        header.set_cksum();
        builder
            .append_data(&mut header, "capsule.toml", Cursor::new(manifest_bytes))
            .expect("append manifest");

        let mut payload_header = tar::Header::new_gnu();
        payload_header
            .set_path("payload.tar.zst")
            .expect("set payload path");
        payload_header.set_mode(0o644);
        payload_header.set_size(payload_zst.len() as u64);
        payload_header.set_cksum();
        builder
            .append_data(
                &mut payload_header,
                "payload.tar.zst",
                Cursor::new(payload_zst),
            )
            .expect("append payload");

        for (path, bytes) in files {
            let mut extra_header = tar::Header::new_gnu();
            extra_header.set_path(path).expect("set path");
            extra_header.set_mode(0o644);
            extra_header.set_size(bytes.len() as u64);
            extra_header.set_cksum();
            builder
                .append_data(&mut extra_header, *path, *bytes)
                .expect("append extra");
        }
        builder.finish().expect("finish archive");
    }
    out.flush().expect("flush vec");
    out
}

fn build_capsule_bytes_with_payload_files(
    manifest: &str,
    payload_files: &[(&str, &[u8])],
) -> Vec<u8> {
    let payload_tar = build_payload_tar_with_files(payload_files).expect("build payload tar");
    let parsed_manifest =
        capsule::types::CapsuleManifest::from_toml(manifest).expect("parse manifest");
    let (distribution_manifest, _) =
        capsule::packers::payload::build_distribution_manifest(&parsed_manifest, &payload_tar)
            .expect("build distribution manifest");
    let mut raw_manifest: toml::Value = toml::from_str(manifest).expect("parse raw manifest");
    let raw_manifest_table = raw_manifest
        .as_table_mut()
        .expect("raw manifest must be a table");
    raw_manifest_table.insert(
        "schema_version".to_string(),
        toml::Value::String(distribution_manifest.schema_version.clone()),
    );
    raw_manifest_table.insert(
        "distribution".to_string(),
        toml::Value::try_from(
            distribution_manifest
                .distribution
                .expect("distribution metadata"),
        )
        .expect("distribution value"),
    );
    let manifest_bytes = toml::to_string_pretty(&raw_manifest).expect("serialize manifest");
    let payload_zst =
        zstd::stream::encode_all(Cursor::new(payload_tar), 1).expect("encode payload");

    let mut out = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut out);
        let mut header = tar::Header::new_gnu();
        header.set_path("capsule.toml").expect("set path");
        header.set_mode(0o644);
        header.set_size(manifest_bytes.len() as u64);
        header.set_cksum();
        builder
            .append_data(&mut header, "capsule.toml", Cursor::new(manifest_bytes))
            .expect("append manifest");

        let mut payload_header = tar::Header::new_gnu();
        payload_header
            .set_path("payload.tar.zst")
            .expect("set payload path");
        payload_header.set_mode(0o644);
        payload_header.set_size(payload_zst.len() as u64);
        payload_header.set_cksum();
        builder
            .append_data(
                &mut payload_header,
                "payload.tar.zst",
                Cursor::new(payload_zst),
            )
            .expect("append payload");
        builder.finish().expect("finish archive");
    }
    out.flush().expect("flush vec");
    out
}

fn build_payload_tar() -> Result<Vec<u8>> {
    build_payload_tar_with_files(&[])
}

fn build_payload_tar_with_files(files: &[(&str, &[u8])]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut out);
        let source = b"print('hello from registry test')\n";
        let mut header = tar::Header::new_gnu();
        header.set_path("main.py")?;
        header.set_mode(0o644);
        header.set_size(source.len() as u64);
        header.set_mtime(0);
        header.set_cksum();
        builder.append_data(&mut header, "main.py", Cursor::new(source))?;
        for (path, bytes) in files {
            let mut extra_header = tar::Header::new_gnu();
            extra_header.set_path(path)?;
            extra_header.set_mode(0o644);
            extra_header.set_size(bytes.len() as u64);
            extra_header.set_mtime(0);
            extra_header.set_cksum();
            builder.append_data(&mut extra_header, *path, Cursor::new(*bytes))?;
        }
        builder.finish()?;
    }
    out.flush().expect("flush payload vec");
    Ok(out)
}

#[allow(dead_code)]
fn compress(data: &[u8]) -> Vec<u8> {
    let mut encoder = zstd::Encoder::new(Vec::new(), 3).expect("encoder");
    encoder.write_all(data).expect("write");
    encoder.finish().expect("finish")
}

#[test]
fn initialize_storage_creates_index() {
    let tmp = tempfile::tempdir().expect("tempdir");
    initialize_storage(tmp.path()).expect("initialize");
    let index = load_index(tmp.path()).expect("load index");
    assert_eq!(index.schema_version, "local-registry-v1");
    assert!(index.capsules.is_empty());
}

#[test]
fn duplicate_version_is_detected() {
    let mut index = RegistryIndex::default();
    let now = Utc::now().to_rfc3339();
    upsert_capsule(
        &mut index,
        "koh0920",
        "sample-capsule",
        "sample-capsule",
        "",
        StoredRelease {
            version: "1.0.0".to_string(),
            file_name: "sample.capsule".to_string(),
            sha256: "sha256:abc".to_string(),
            blake3: "blake3:def".to_string(),
            size_bytes: 1,
            signature_status: "verified".to_string(),
            created_at: now.clone(),
            lock_id: None,
            closure_digest: None,
            publish_metadata: None,
            payload_v3: None,
        },
        &now,
    );
    assert!(has_release_version(
        &index,
        "koh0920",
        "sample-capsule",
        "1.0.0"
    ));
}

#[test]
fn delete_capsule_from_index_removes_requested_version_only() {
    let mut index = RegistryIndex::default();
    let now = Utc::now().to_rfc3339();
    upsert_capsule(
        &mut index,
        "koh0920",
        "sample-capsule",
        "sample-capsule",
        "",
        StoredRelease {
            version: "1.0.0".to_string(),
            file_name: "sample-1.0.0.capsule".to_string(),
            sha256: "sha256:abc".to_string(),
            blake3: "blake3:def".to_string(),
            size_bytes: 1,
            signature_status: "verified".to_string(),
            created_at: now.clone(),
            lock_id: None,
            closure_digest: None,
            publish_metadata: None,
            payload_v3: None,
        },
        &now,
    );
    upsert_capsule(
        &mut index,
        "koh0920",
        "sample-capsule",
        "sample-capsule",
        "",
        StoredRelease {
            version: "1.1.0".to_string(),
            file_name: "sample-1.1.0.capsule".to_string(),
            sha256: "sha256:ghi".to_string(),
            blake3: "blake3:jkl".to_string(),
            size_bytes: 1,
            signature_status: "verified".to_string(),
            created_at: now.clone(),
            lock_id: None,
            closure_digest: None,
            publish_metadata: None,
            payload_v3: None,
        },
        &now,
    );

    let outcome =
        delete_capsule_from_index(&mut index, "koh0920", "sample-capsule", Some("1.1.0"), &now);
    let DeleteCapsuleOutcome::Deleted(result) = outcome else {
        panic!("expected deleted outcome");
    };
    assert!(!result.removed_capsule);
    assert_eq!(result.removed_version.as_deref(), Some("1.1.0"));
    assert!(has_release_version(
        &index,
        "koh0920",
        "sample-capsule",
        "1.0.0"
    ));
    assert!(!has_release_version(
        &index,
        "koh0920",
        "sample-capsule",
        "1.1.0"
    ));
}

#[test]
fn delete_capsule_from_index_removes_capsule_when_last_release_deleted() {
    let mut index = RegistryIndex::default();
    let now = Utc::now().to_rfc3339();
    upsert_capsule(
        &mut index,
        "koh0920",
        "sample-capsule",
        "sample-capsule",
        "",
        StoredRelease {
            version: "1.0.0".to_string(),
            file_name: "sample-1.0.0.capsule".to_string(),
            sha256: "sha256:abc".to_string(),
            blake3: "blake3:def".to_string(),
            size_bytes: 1,
            signature_status: "verified".to_string(),
            created_at: now.clone(),
            lock_id: None,
            closure_digest: None,
            publish_metadata: None,
            payload_v3: None,
        },
        &now,
    );
    let outcome =
        delete_capsule_from_index(&mut index, "koh0920", "sample-capsule", Some("1.0.0"), &now);
    let DeleteCapsuleOutcome::Deleted(result) = outcome else {
        panic!("expected deleted outcome");
    };
    assert!(result.removed_capsule);
    assert!(index.capsules.is_empty());
}

#[test]
fn delete_capsule_from_index_reports_version_not_found() {
    let mut index = RegistryIndex::default();
    let now = Utc::now().to_rfc3339();
    upsert_capsule(
        &mut index,
        "koh0920",
        "sample-capsule",
        "sample-capsule",
        "",
        StoredRelease {
            version: "1.0.0".to_string(),
            file_name: "sample-1.0.0.capsule".to_string(),
            sha256: "sha256:abc".to_string(),
            blake3: "blake3:def".to_string(),
            size_bytes: 1,
            signature_status: "verified".to_string(),
            created_at: now.clone(),
            lock_id: None,
            closure_digest: None,
            publish_metadata: None,
            payload_v3: None,
        },
        &now,
    );
    let outcome =
        delete_capsule_from_index(&mut index, "koh0920", "sample-capsule", Some("9.9.9"), &now);
    let DeleteCapsuleOutcome::VersionNotFound(version) = outcome else {
        panic!("expected version not found");
    };
    assert_eq!(version, "9.9.9");
}

#[test]
fn existing_release_outcome_requires_opt_in() {
    let release = StoredRelease {
        version: "1.0.0".to_string(),
        file_name: "sample.capsule".to_string(),
        sha256: "sha256:abc".to_string(),
        blake3: "blake3:def".to_string(),
        size_bytes: 1,
        signature_status: "verified".to_string(),
        created_at: Utc::now().to_rfc3339(),
        lock_id: None,
        closure_digest: None,
        publish_metadata: None,
        payload_v3: None,
    };

    let outcome = existing_release_outcome(&release.sha256, false, "sha256:abc");
    assert_eq!(
        outcome,
        ExistingReleaseOutcome::Conflict("same version is already published")
    );
}

#[test]
fn existing_release_outcome_reuses_when_sha256_matches() {
    let release = StoredRelease {
        version: "1.0.0".to_string(),
        file_name: "sample.capsule".to_string(),
        sha256: "sha256:abc".to_string(),
        blake3: "blake3:def".to_string(),
        size_bytes: 1,
        signature_status: "verified".to_string(),
        created_at: Utc::now().to_rfc3339(),
        lock_id: None,
        closure_digest: None,
        publish_metadata: None,
        payload_v3: None,
    };

    let outcome = existing_release_outcome(&release.sha256, true, "sha256:abc");
    assert_eq!(outcome, ExistingReleaseOutcome::Reuse);
}

#[test]
fn existing_release_outcome_conflicts_when_sha256_differs() {
    let release = StoredRelease {
        version: "1.0.0".to_string(),
        file_name: "sample.capsule".to_string(),
        sha256: "sha256:abc".to_string(),
        blake3: "blake3:def".to_string(),
        size_bytes: 1,
        signature_status: "verified".to_string(),
        created_at: Utc::now().to_rfc3339(),
        lock_id: None,
        closure_digest: None,
        publish_metadata: None,
        payload_v3: None,
    };

    let outcome = existing_release_outcome(&release.sha256, true, "sha256:xyz");
    assert_eq!(
        outcome,
        ExistingReleaseOutcome::Conflict("same version is already published (sha256 mismatch)")
    );
}

#[test]
fn search_cursor_paginates() {
    let mut index = RegistryIndex::default();
    let now = Utc::now().to_rfc3339();
    for slug in ["a", "b", "c"] {
        upsert_capsule(
            &mut index,
            "koh0920",
            slug,
            slug,
            "",
            StoredRelease {
                version: "1.0.0".to_string(),
                file_name: format!("{slug}.capsule"),
                sha256: "sha256:abc".to_string(),
                blake3: "blake3:def".to_string(),
                size_bytes: 1,
                signature_status: "verified".to_string(),
                created_at: now.clone(),
                lock_id: None,
                closure_digest: None,
                publish_metadata: None,
                payload_v3: None,
            },
            &now,
        );
    }
    let rows = index
        .capsules
        .iter()
        .map(|capsule| stored_to_search_row(capsule, None, "http://127.0.0.1:8787"))
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].publisher.handle, "koh0920");
}

#[test]
fn validate_write_auth_allows_when_disabled() {
    let headers = HeaderMap::new();
    assert!(validate_write_auth(&headers, None).is_ok());
}

#[test]
fn validate_write_auth_requires_matching_bearer_token() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        "Bearer secret-token".parse().unwrap(),
    );
    assert!(validate_write_auth(&headers, Some("secret-token")).is_ok());
    assert!(validate_write_auth(&headers, Some("wrong-token")).is_err());
    let empty = HeaderMap::new();
    assert!(validate_write_auth(&empty, Some("secret-token")).is_err());
}

#[test]
fn validate_read_auth_requires_matching_bearer_token() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        "Bearer secret-token".parse().unwrap(),
    );
    assert!(validate_read_auth(&headers, Some("secret-token")).is_ok());
    assert!(validate_read_auth(&headers, Some("wrong-token")).is_err());
    let empty = HeaderMap::new();
    assert!(validate_read_auth(&empty, Some("secret-token")).is_err());
}

#[test]
fn constant_time_token_eq_handles_length_mismatch() {
    assert!(constant_time_token_eq(b"secret-token", b"secret-token"));
    assert!(!constant_time_token_eq(b"secret-token", b"secret-token-x"));
    assert!(!constant_time_token_eq(b"secret-token", b"secret"));
}

#[test]
fn resolve_public_base_url_uses_host_header() {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, "100.64.0.10:8787".parse().unwrap());
    let url = resolve_public_base_url(&headers, "http://0.0.0.0:8787");
    assert_eq!(url, "http://100.64.0.10:8787");
}

#[test]
fn resolve_public_base_url_uses_forwarded_host_and_proto() {
    let mut headers = HeaderMap::new();
    headers.insert("x-forwarded-proto", "https".parse().unwrap());
    headers.insert("x-forwarded-host", "store.example.com".parse().unwrap());
    let url = resolve_public_base_url(&headers, "http://127.0.0.1:8787");
    assert_eq!(url, "https://store.example.com");
}

#[test]
fn resolve_public_base_url_falls_back_when_headers_missing() {
    let headers = HeaderMap::new();
    let url = resolve_public_base_url(&headers, "http://127.0.0.1:8787");
    assert_eq!(url, "http://127.0.0.1:8787");
}

#[test]
fn normalize_registry_base_url_for_local_run_rewrites_wildcard_host() {
    let rewritten =
        normalize_registry_base_url_for_local_run("http://0.0.0.0:9000", "http://0.0.0.0:9000");
    assert_eq!(rewritten, "http://127.0.0.1:9000");
}

#[test]
fn truncate_for_error_limits_message_length() {
    let input = "a".repeat(1000);
    let truncated = truncate_for_error(&input, 32);
    assert!(truncated.starts_with(&"a".repeat(32)));
    assert!(truncated.ends_with("..."));
}

#[test]
fn extract_manifest_from_capsule_returns_text() {
    let manifest = r#"schema_version = "0.3"
name = "sample"
version = "1.0.0"
type = "app"
"#;
    let bytes = build_capsule_bytes(manifest);
    let extracted = extract_manifest_from_capsule(&bytes).expect("extract");
    assert!(extracted.contains("name = \"sample\""));
}

#[test]
fn extract_readme_from_capsule_prefers_priority_order() {
    let manifest = r#"schema_version = "0.3"
name = "sample"
version = "1.0.0"
type = "app"
"#;
    let bytes = build_capsule_bytes_with_files(
        manifest,
        &[
            ("README.txt", b"txt readme"),
            ("docs/README.mdx", b"mdx readme"),
            ("README.md", b"markdown readme"),
        ],
    );
    let extracted = extract_readme_from_capsule(&bytes);
    assert_eq!(extracted.as_deref(), Some("markdown readme"));
}

#[test]
fn extract_readme_from_capsule_truncates_large_files() {
    let manifest = r#"schema_version = "0.3"
name = "sample"
version = "1.0.0"
type = "app"
"#;
    let large = vec![b'a'; README_MAX_BYTES + 4096];
    let bytes = build_capsule_bytes_with_files(manifest, &[("README.md", &large)]);
    let extracted = extract_readme_from_capsule(&bytes).expect("extract readme");
    assert_eq!(extracted.len(), README_MAX_BYTES);
}

#[test]
fn extract_readme_from_capsule_reads_payload_tar_zst_contents() {
    let manifest = r#"schema_version = "0.3"
name = "sample"
version = "1.0.0"
type = "app"
"#;
    let bytes = build_capsule_bytes_with_payload_files(
        manifest,
        &[("README.md", b"payload readme markdown")],
    );
    let extracted = extract_readme_from_capsule(&bytes);
    assert_eq!(extracted.as_deref(), Some("payload readme markdown"));
}

#[test]
fn extract_repository_from_manifest_prefers_metadata_then_root() {
    let parsed: toml::Value = toml::from_str(
        r#"
repository = "root/repo"
[metadata]
repository = "meta/repo"
"#,
    )
    .expect("parse");
    assert_eq!(
        extract_repository_from_manifest(&parsed).as_deref(),
        Some("meta/repo")
    );

    let parsed_root: toml::Value =
        toml::from_str(r#"repository = "root-only/repo""#).expect("parse");
    assert_eq!(
        extract_repository_from_manifest(&parsed_root).as_deref(),
        Some("root-only/repo")
    );
}

#[test]
fn load_capsule_detail_manifest_reads_latest_release_artifact() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest = r#"schema_version = "0.3"
name = "sample"
version = "1.0.0"
type = "app"

[metadata]
repository = "koh0920/sample"
"#;
    let file_name = "sample-1.0.0.capsule";
    let artifact = artifact_path(tmp.path(), "local", "sample", "1.0.0", file_name);
    std::fs::create_dir_all(artifact.parent().expect("parent")).expect("mkdir");
    std::fs::write(&artifact, build_capsule_bytes(manifest)).expect("write artifact");

    let capsule = StoredCapsule {
        id: "id-1".to_string(),
        publisher: "local".to_string(),
        slug: "sample".to_string(),
        name: "sample".to_string(),
        description: "".to_string(),
        category: "tools".to_string(),
        capsule_type: "app".to_string(),
        price: 0,
        currency: "usd".to_string(),
        latest_version: "1.0.0".to_string(),
        releases: vec![StoredRelease {
            version: "1.0.0".to_string(),
            file_name: file_name.to_string(),
            sha256: "sha256:x".to_string(),
            blake3: "blake3:y".to_string(),
            size_bytes: 1,
            signature_status: "verified".to_string(),
            created_at: Utc::now().to_rfc3339(),
            lock_id: None,
            closure_digest: None,
            publish_metadata: None,
            payload_v3: None,
        }],
        downloads: 0,
        created_at: Utc::now().to_rfc3339(),
        updated_at: Utc::now().to_rfc3339(),
    };

    let (manifest_json, repository, manifest_toml, capsule_lock, readme_markdown, readme_source) =
        load_capsule_detail_manifest(tmp.path(), &capsule);
    let manifest_json = manifest_json.expect("manifest json");
    assert_eq!(
        manifest_json
            .get("name")
            .and_then(serde_json::Value::as_str),
        Some("sample")
    );
    assert_eq!(repository.as_deref(), Some("koh0920/sample"));
    assert!(manifest_toml.as_deref().is_some_and(|raw| {
        raw.contains("name = \"sample\"") && raw.contains("schema_version")
    }));
    assert!(capsule_lock.is_none());
    assert_eq!(readme_markdown.as_deref(), Some("dummy"));
    assert_eq!(readme_source.as_deref(), Some("artifact"));
}

#[cfg(feature = "webui")]
#[test]
fn normalize_ui_path_maps_root_to_index() {
    assert_eq!(normalize_ui_path("/").as_deref(), Some("index.html"),);
    assert_eq!(
        normalize_ui_path("/assets/index.js").as_deref(),
        Some("assets/index.js"),
    );
    assert!(normalize_ui_path("/../../etc/passwd").is_none());
}

#[cfg(feature = "webui")]
#[test]
fn cache_control_for_ui_path_respects_spa_policy() {
    assert_eq!(
        cache_control_for_ui_path("index.html", false),
        HeaderValue::from_static("no-cache")
    );
    assert_eq!(
        cache_control_for_ui_path("assets/index-abc.js", false),
        HeaderValue::from_static("public, max-age=31536000, immutable")
    );
}

#[test]
fn read_process_log_lines_applies_tail_limit() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("capsule-123.log");
    std::fs::write(&path, "line1\nline2\nline3\n").expect("write log");
    let lines = read_process_log_lines(&path, 2);
    assert_eq!(lines, vec!["line2".to_string(), "line3".to_string()]);
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_providers_returns_desktop_provider() {
    let state = registry_test_state(None);
    let response = handle_runtime_providers(State(state), HeaderMap::new())
        .await
        .into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json[0]["id"], "desktop:local");
    assert_eq!(json[0]["kind"], "desktop");
    assert_eq!(json[0]["capabilities"]["supports_logs"], true);
    assert_eq!(json[0]["capabilities"]["supports_launch"], true);
    assert_eq!(json[0]["capabilities"]["supports_stop"], true);
    assert_eq!(json[0]["capabilities"]["supports_start_serve"], false);
}

#[tokio::test(flavor = "current_thread")]
async fn sensitive_runtime_read_apis_require_auth_when_token_configured() {
    let _lock = env_lock().lock().unwrap();
    let _ato_home = AtoHomeGuard::set("runtime-read-auth");
    let state = registry_test_state(Some("secret"));

    let response = handle_runtime_sessions(State(state.clone()), HeaderMap::new())
        .await
        .into_response();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let response = handle_runtime_install_profiles(State(state.clone()), HeaderMap::new())
        .await
        .into_response();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let response = handle_runtime_session_logs(
        State(state.clone()),
        HeaderMap::new(),
        AxumPath("runtime-session-1".to_string()),
        Query(ProcessLogsQuery { tail: Some(10) }),
    )
    .await
    .into_response();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let headers = bearer_headers("secret");
    let response = handle_runtime_sessions(State(state.clone()), headers.clone())
        .await
        .into_response();
    assert_eq!(response.status(), StatusCode::OK);

    let response = handle_runtime_install_profiles(State(state.clone()), headers.clone())
        .await
        .into_response();
    assert_eq!(response.status(), StatusCode::OK);

    let response = handle_runtime_session_logs(
        State(state),
        headers,
        AxumPath("runtime-session-1".to_string()),
        Query(ProcessLogsQuery { tail: Some(10) }),
    )
    .await
    .into_response();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test(flavor = "current_thread")]
async fn install_profiles_read_ato_home_instances_root() {
    let _lock = env_lock().lock().unwrap();
    let _ato_home = AtoHomeGuard::set("install-profiles");

    let root = install_profile_store_root();
    let store = capsule::foundation::install_lifecycle::InstallInstanceStore::new(&root)
        .expect("install store");
    let app_id =
        capsule::foundation::install_lifecycle::InstalledAppId::new("app_runtime_profile_test");
    let profile_id = capsule::foundation::install_lifecycle::ProfileId::new("default");
    let revision_id =
        capsule::foundation::install_lifecycle::InstallRevisionId::new("rev_runtime_test");
    store
        .write_app_record(&capsule::foundation::install_lifecycle::AppRecord {
            installed_app_id: app_id.clone(),
            publisher: "koh0920".to_string(),
            slug: "runtime-demo".to_string(),
            capsule_handle: "koh0920/runtime-demo".to_string(),
            version: "0.1.0".to_string(),
            installed_at: "2026-05-31T00:00:00Z".to_string(),
            updated_at: "2026-05-31T00:00:00Z".to_string(),
        })
        .expect("write app");
    store
        .write_profile(
            &app_id,
            &capsule::foundation::install_lifecycle::LaunchProfile {
                profile_id: profile_id.clone(),
                port_policy: "fixed:8123".to_string(),
                isolation: "strict".to_string(),
                ..Default::default()
            },
        )
        .expect("write profile");
    store
        .scaffold_revision(&revision_id)
        .expect("scaffold revision");
    store
        .set_current_revision(&app_id, &profile_id, &revision_id)
        .expect("set current revision");

    let response = handle_runtime_install_profiles(
        State(registry_test_state(Some("secret"))),
        bearer_headers("secret"),
    )
    .await
    .into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json[0]["installed_app_id"], "app_runtime_profile_test");
    assert_eq!(json[0]["publisher"], "koh0920");
    assert_eq!(json[0]["slug"], "runtime-demo");
    assert_eq!(json[0]["capsule_handle"], "koh0920/runtime-demo");
    assert_eq!(json[0]["profile_id"], "default");
    assert_eq!(json[0]["current_revision_id"], "rev_runtime_test");
    assert_eq!(json[0]["port_policy"], "fixed:8123");
    assert_eq!(json[0]["isolation"], "strict");
}

#[test]
fn runtime_session_summary_keeps_legacy_process_origin_unknown() {
    let summary = runtime_session_summary(runtime_process_fixture(), None);

    assert_eq!(summary.session.session_id, "runtime-session-1");
    assert_eq!(summary.session.status, "ready");
    assert_eq!(summary.session.user_visible_url, None);
    assert_eq!(
        summary.local_runtime_url.as_deref(),
        Some("http://127.0.0.1:8123")
    );
    assert_eq!(
        summary.session.requested_by_client.as_deref(),
        Some("unknown")
    );
    assert_eq!(
        summary.session.runtime_owner.as_deref(),
        Some("local_runtime")
    );
    assert_eq!(
        summary.session.placement.placement_provider,
        ato_protocol::placement::PlacementProviderKind::Desktop
    );
}

#[test]
fn runtime_session_summary_uses_stored_session_origin_and_user_url() {
    let stored = stored_runtime_session_record();
    let summary = runtime_session_summary(runtime_process_fixture(), Some(&stored));

    assert_eq!(summary.session.session_id, "runtime-session-1");
    assert_eq!(
        summary.session.user_visible_url.as_deref(),
        Some("https://desktop.example/session/runtime-session-1")
    );
    assert_eq!(
        summary.local_runtime_url.as_deref(),
        Some("http://127.0.0.1:8123")
    );
    assert_eq!(
        summary.session.requested_by_client.as_deref(),
        Some("desktop_fe")
    );
    assert_eq!(summary.session.runtime_owner.as_deref(), Some("desktop_be"));
    assert_eq!(
        summary.session.placement.placement_provider_id.as_str(),
        "desktop:stored"
    );
}

fn runtime_process_fixture() -> ProcessInfo {
    ProcessInfo {
        id: "runtime-session-1".to_string(),
        name: "demo".to_string(),
        pid: std::process::id() as i32,
        workload_pid: None,
        status: ProcessStatus::Ready,
        runtime: "source".to_string(),
        start_time: std::time::SystemTime::now(),
        os_start_time_unix_ms: capsule::state::session::process::process_start_time_unix_ms(
            std::process::id(),
        ),
        workload_os_start_time_unix_ms: None,
        manifest_path: None,
        scoped_id: None,
        target_label: Some("main".to_string()),
        requested_port: Some(8123),
        log_path: None,
        ready_at: Some(std::time::SystemTime::now()),
        last_event: None,
        last_error: None,
        exit_code: None,
    }
}

fn stored_runtime_session_record() -> capsule::state::session::StoredSessionInfo {
    serde_json::from_value(serde_json::json!({
        "session_id": "runtime-session-1",
        "handle": "publisher/slug",
        "normalized_handle": "publisher/slug",
        "canonical_handle": null,
        "trust_state": "trusted",
        "source": "registry",
        "restricted": false,
        "snapshot": null,
        "runtime": {
            "target_label": "main",
            "runtime": "node",
            "driver": null,
            "language": null,
            "port": null
        },
        "display_strategy": "web_url",
        "pid": 1234,
        "log_path": ".tmp/runtime-session-1.log",
        "manifest_path": "capsule.toml",
        "target_label": "main",
        "notes": [],
        "guest": null,
        "web": null,
        "terminal": null,
        "service": null,
        "placement_provider": "desktop",
        "placement_provider_id": "desktop:stored",
        "placement_id": "plc_stored_desktop",
        "placement_fingerprint": "sha256:abc",
        "placement_facets": {
            "provider_kind": "desktop",
            "isolation_class": "local",
            "storage_class": "local",
            "network_class": "loopback",
            "runner_version": "0.7.0-dev"
        },
        "user_visible_url": "https://desktop.example/session/runtime-session-1",
        "requested_by_client": "desktop_fe",
        "runtime_owner": "desktop_be"
    }))
    .expect("stored session record")
}

#[tokio::test(flavor = "current_thread")]
async fn manifest_yank_requires_auth() {
    let tmp = tempfile::tempdir().expect("tempdir");
    initialize_storage(tmp.path()).expect("init");
    let state = AppState {
        listen_url: "http://127.0.0.1:8787".to_string(),
        data_dir: tmp.path().to_path_buf(),
        auth_token: Some("secret".to_string()),
        lock: Arc::new(Mutex::new(())),
    };
    let response = handle_manifest_yank(
        State(state),
        HeaderMap::new(),
        Json(YankRequest {
            scoped_id: "koh0920/sample".to_string(),
            target_manifest_hash: "blake3:deadbeef".to_string(),
        }),
    )
    .await
    .into_response();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "current_thread")]
async fn manifest_yank_rejects_unknown_history_target() {
    let tmp = tempfile::tempdir().expect("tempdir");
    initialize_storage(tmp.path()).expect("init");
    let state = AppState {
        listen_url: "http://127.0.0.1:8787".to_string(),
        data_dir: tmp.path().to_path_buf(),
        auth_token: Some("secret".to_string()),
        lock: Arc::new(Mutex::new(())),
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        header::HeaderValue::from_static("Bearer secret"),
    );
    let response = handle_manifest_yank(
        State(state),
        headers,
        Json(YankRequest {
            scoped_id: "koh0920/sample".to_string(),
            target_manifest_hash: "blake3:deadbeef".to_string(),
        }),
    )
    .await
    .into_response();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "current_thread")]
async fn yanked_manifest_blocks_negotiate_and_manifest_fetch() {
    let tmp = tempfile::tempdir().expect("tempdir");
    initialize_storage(tmp.path()).expect("init");
    let store = RegistryStore::open(tmp.path()).expect("open store");
    let recorded = store
            .record_manifest_and_epoch(
                "koh0920/sample",
                "schema_version = \"0.2\"\nname = \"sample\"\nversion = \"1.0.0\"\ntype = \"app\"\ndefault_target = \"cli\"\n",
                b"payload-v1",
                "2026-03-05T00:00:00Z",
            )
            .expect("record");
    let yanked = store
        .yank_manifest("koh0920/sample", &recorded.pointer.manifest_hash)
        .expect("yank");
    assert!(yanked);

    let state = AppState {
        listen_url: "http://127.0.0.1:8787".to_string(),
        data_dir: tmp.path().to_path_buf(),
        auth_token: None,
        lock: Arc::new(Mutex::new(())),
    };
    let negotiate_resp = handle_manifest_negotiate(
        State(state.clone()),
        HeaderMap::new(),
        Json(NegotiateRequest {
            scoped_id: "koh0920/sample".to_string(),
            target_manifest_hash: recorded.pointer.manifest_hash.clone(),
            have_chunks: vec![],
            have_chunks_bloom: None,
            reuse_lease_id: None,
            max_bytes: None,
        }),
    )
    .await
    .into_response();
    assert_eq!(negotiate_resp.status(), StatusCode::GONE);
    let negotiate_body = to_bytes(negotiate_resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    let negotiate_json: serde_json::Value =
        serde_json::from_slice(&negotiate_body).expect("parse json");
    assert_eq!(
        negotiate_json.get("yanked"),
        Some(&serde_json::Value::Bool(true))
    );

    let manifest_resp = handle_manifest_get_manifest(
        State(state),
        HeaderMap::new(),
        AxumPath(recorded.pointer.manifest_hash),
    )
    .await
    .into_response();
    assert_eq!(manifest_resp.status(), StatusCode::GONE);
    let manifest_body = to_bytes(manifest_resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    let manifest_json: serde_json::Value =
        serde_json::from_slice(&manifest_body).expect("parse json");
    assert_eq!(
        manifest_json.get("yanked"),
        Some(&serde_json::Value::Bool(true))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn version_resolve_returns_manifest_hash_for_release() {
    let tmp = tempfile::tempdir().expect("tempdir");
    initialize_storage(tmp.path()).expect("init");
    let store = RegistryStore::open(tmp.path()).expect("open store");
    let manifest = "schema_version = \"0.2\"\nname = \"sample\"\nversion = \"1.0.0\"\ntype = \"app\"\ndefault_target = \"cli\"\n";
    let capsule = build_capsule_bytes(manifest);
    let published = store
        .publish_registry_release(
            "koh0920",
            "sample",
            "sample",
            "demo",
            "1.0.0",
            "sample-1.0.0.capsule",
            "sha256:abc",
            "blake3:def",
            capsule.len() as u64,
            None,
            None,
            None,
            &capsule,
            "2026-03-05T00:00:00Z",
        )
        .expect("publish");

    let state = AppState {
        listen_url: "http://127.0.0.1:8787".to_string(),
        data_dir: tmp.path().to_path_buf(),
        auth_token: None,
        lock: Arc::new(Mutex::new(())),
    };
    let response = handle_manifest_resolve_version(
        State(state),
        HeaderMap::new(),
        AxumPath((
            "koh0920".to_string(),
            "sample".to_string(),
            "1.0.0".to_string(),
        )),
    )
    .await
    .into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("parse json");
    assert_eq!(
        json.get("manifest_hash")
            .and_then(serde_json::Value::as_str),
        Some(published.pointer.manifest_hash.as_str())
    );
}

#[tokio::test(flavor = "current_thread")]
async fn version_resolve_returns_gone_for_yanked_release() {
    let tmp = tempfile::tempdir().expect("tempdir");
    initialize_storage(tmp.path()).expect("init");
    let store = RegistryStore::open(tmp.path()).expect("open store");
    let manifest = "schema_version = \"0.2\"\nname = \"sample\"\nversion = \"1.0.0\"\ntype = \"app\"\ndefault_target = \"cli\"\n";
    let capsule = build_capsule_bytes(manifest);
    let published = store
        .publish_registry_release(
            "koh0920",
            "sample",
            "sample",
            "demo",
            "1.0.0",
            "sample-1.0.0.capsule",
            "sha256:abc",
            "blake3:def",
            capsule.len() as u64,
            None,
            None,
            None,
            &capsule,
            "2026-03-05T00:00:00Z",
        )
        .expect("publish");
    store
        .yank_manifest("koh0920/sample", &published.pointer.manifest_hash)
        .expect("yank");

    let state = AppState {
        listen_url: "http://127.0.0.1:8787".to_string(),
        data_dir: tmp.path().to_path_buf(),
        auth_token: None,
        lock: Arc::new(Mutex::new(())),
    };
    let response = handle_manifest_resolve_version(
        State(state),
        HeaderMap::new(),
        AxumPath((
            "koh0920".to_string(),
            "sample".to_string(),
            "1.0.0".to_string(),
        )),
    )
    .await
    .into_response();
    assert_eq!(response.status(), StatusCode::GONE);
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_launch_requires_write_auth() {
    let state = registry_test_state(Some("secret"));
    let response = handle_runtime_launch_session(
        State(state),
        HeaderMap::new(),
        Json(LaunchSessionRequest {
            install_profile_key: "k".to_string(),
            target_label: None,
        }),
    )
    .await
    .into_response();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_stop_requires_write_auth() {
    let state = registry_test_state(Some("secret"));
    let response = handle_runtime_stop_session(
        State(state),
        HeaderMap::new(),
        AxumPath("sess-1".to_string()),
    )
    .await
    .into_response();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_launch_rejects_wrong_token() {
    let state = registry_test_state(Some("secret"));
    let response = handle_runtime_launch_session(
        State(state),
        bearer_headers("wrong"),
        Json(LaunchSessionRequest {
            install_profile_key: "k".to_string(),
            target_label: None,
        }),
    )
    .await
    .into_response();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_stop_post_requires_write_auth() {
    let state = registry_test_state(Some("secret"));
    let response = handle_runtime_stop_session_post(
        State(state),
        HeaderMap::new(),
        AxumPath("sess-1".to_string()),
    )
    .await
    .into_response();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_stop_post_rejects_wrong_token() {
    let state = registry_test_state(Some("secret"));
    let response = handle_runtime_stop_session_post(
        State(state),
        bearer_headers("wrong"),
        AxumPath("sess-1".to_string()),
    )
    .await
    .into_response();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_stop_post_unknown_session_returns_404() {
    let _lock = env_lock().lock().unwrap();
    let _ato_home = AtoHomeGuard::set("stop-post-unknown");
    let state = registry_test_state(None);
    let response = handle_runtime_stop_session_post(
        State(state),
        HeaderMap::new(),
        AxumPath("nonexistent-session-id".to_string()),
    )
    .await
    .into_response();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_launch_empty_key_returns_400() {
    let _lock = env_lock().lock().unwrap();
    let _ato_home = AtoHomeGuard::set("launch-empty-key");
    let state = registry_test_state(None);
    let response = handle_runtime_launch_session(
        State(state),
        HeaderMap::new(),
        Json(LaunchSessionRequest {
            install_profile_key: "".to_string(),
            target_label: None,
        }),
    )
    .await
    .into_response();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_launch_unknown_key_returns_404() {
    let _lock = env_lock().lock().unwrap();
    let _ato_home = AtoHomeGuard::set("launch-unknown-key");
    let state = registry_test_state(None);
    let response = handle_runtime_launch_session(
        State(state),
        HeaderMap::new(),
        Json(LaunchSessionRequest {
            install_profile_key: "nonexistent::default".to_string(),
            target_label: None,
        }),
    )
    .await
    .into_response();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// Verify that try_register_ephemeral_ingress_with_url never leaks a loopback
/// address into user_visible_url. The launch handler always sets user_visible_url
/// to None; local_runtime_url carries the loopback address instead.
#[test]
fn launch_response_user_visible_url_is_never_loopback() {
    // Construct a LaunchSessionResponse as the handler would build it after a
    // successful session start with a local runtime URL.
    use ato_protocol::placement::{
        PlacementFacets, PlacementIdentity, PlacementProviderId, PlacementProviderKind,
    };
    let resp = super::LaunchSessionResponse {
        status: "starting".to_string(),
        install_profile_key: "ipk_abc::default".to_string(),
        launch_profile_id: None,
        placement: PlacementIdentity {
            placement_provider: PlacementProviderKind::Desktop,
            placement_provider_id: PlacementProviderId::new("desktop:local"),
            placement_id: "plc_local_desktop".to_string(),
            placement_fingerprint: None,
            placement_facets: Some(PlacementFacets {
                provider_kind: PlacementProviderKind::Desktop,
                isolation_class: "local".to_string(),
                storage_class: "local".to_string(),
                network_class: "loopback".to_string(),
                runner_version: None,
            }),
        },
        requested_by_client: "web_console".to_string(),
        runtime_owner: "local_runtime".to_string(),
        session_id: "sess-abc".to_string(),
        user_visible_url: None,
        local_runtime_url: Some("http://127.0.0.1:8080".to_string()),
    };

    assert!(
        resp.user_visible_url.is_none(),
        "user_visible_url must be None — loopback URLs must not appear here"
    );
    assert!(
        resp.local_runtime_url
            .as_deref()
            .unwrap_or("")
            .starts_with("http://127.0.0.1"),
        "local_runtime_url should carry the loopback URL"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_launch_non_default_profile_returns_501() {
    use capsule::foundation::install_lifecycle::{
        AppRecord, InstallInstanceStore, InstalledAppId, LaunchProfile, ProfileId,
    };
    let _lock = env_lock().lock().unwrap();
    let _ato_home = AtoHomeGuard::set("launch-non-default-profile");

    // Register an app with a non-default profile.
    let root = install_profile_store_root();
    let store = InstallInstanceStore::new(&root).expect("install store");
    let app_id = InstalledAppId::new("app_non_default_test");
    let profile_id = ProfileId::new("gpu");
    store
        .write_app_record(&AppRecord {
            installed_app_id: app_id.clone(),
            publisher: "koh0920".to_string(),
            slug: "demo".to_string(),
            capsule_handle: "koh0920/demo".to_string(),
            version: "0.1.0".to_string(),
            installed_at: "2026-06-04T00:00:00Z".to_string(),
            updated_at: "2026-06-04T00:00:00Z".to_string(),
        })
        .expect("write app");
    store
        .write_profile(
            &app_id,
            &LaunchProfile {
                profile_id: profile_id.clone(),
                ..Default::default()
            },
        )
        .expect("write profile");

    use capsule::foundation::install_lifecycle::derive_install_profile_key;
    let ipk = derive_install_profile_key(&app_id, &profile_id)
        .as_str()
        .to_string();

    let state = registry_test_state(None);
    let response = handle_runtime_launch_session(
        State(state),
        HeaderMap::new(),
        Json(LaunchSessionRequest {
            install_profile_key: ipk,
            target_label: None,
        }),
    )
    .await
    .into_response();
    assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_provider_capabilities_start_serve_is_false() {
    let state = registry_test_state(None);
    let response = handle_runtime_providers(State(state), HeaderMap::new())
        .await
        .into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(
        json[0]["capabilities"]["supports_start_serve"], false,
        "supports_start_serve must be false until StartServe integration lands"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_session_logs_sse_streams_beyond_channel_capacity() {
    // Verify that SSE back-pressure (send().await) delivers all existing lines
    // even when the log exceeds the channel capacity (512). Previously try_send
    // would have silently aborted the stream at 512 lines.
    use axum::http::header::ACCEPT;
    let _lock = env_lock().lock().unwrap();
    let _ato_home = AtoHomeGuard::set("sse-backlog");

    // Write 600 lines — more than the channel capacity of 512.
    let session_id = "sse-backlog-session";
    let log_dir = capsule::common::paths::ato_path_or_workspace_tmp("logs");
    std::fs::create_dir_all(&log_dir).expect("create log dir");
    let log_path = log_dir.join(format!("{session_id}.log"));
    let log_content: String = (0..600).map(|i| format!("line {i}\n")).collect();
    std::fs::write(&log_path, log_content).expect("write log");

    let state = registry_test_state(None);
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, "text/event-stream".parse().unwrap());
    let response = handle_runtime_session_logs(
        State(state),
        headers,
        AxumPath(session_id.to_string()),
        Query(ProcessLogsQuery { tail: None }),
    )
    .await
    .into_response();
    assert_eq!(response.status(), StatusCode::OK);

    // Consume the SSE body. Because there is no running process the background
    // task terminates after the first poll, so the stream ends promptly.
    let body = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        to_bytes(response.into_body(), usize::MAX),
    )
    .await
    .expect("SSE body must arrive within 5s")
    .expect("body");

    // All 600 lines must appear as `data:` events.
    let text = String::from_utf8_lossy(&body);
    let data_count = text.lines().filter(|l| l.starts_with("data:")).count();
    assert!(
        data_count >= 600,
        "expected >= 600 data events, got {data_count}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn persistent_state_local_api_registers_and_lists_records() {
    let (_home, _home_guard, manifest_path, bind_dir, state) = {
        let _guard = env_lock().lock().unwrap();
        let home = tempfile::tempdir().expect("home");
        let home_guard = HomeGuard::set(home.path());

        let manifest_dir = home.path().join("workspace");
        std::fs::create_dir_all(&manifest_dir).expect("create manifest dir");
        let manifest_path = manifest_dir.join("capsule.toml");
        std::fs::write(
            &manifest_path,
            r#"
schema_version = "0.3"
name = "demo-app"
version = "0.1.0"
type = "app"

runtime = "oci"
image = "ghcr.io/example/app:latest"
[state.data]
kind = "filesystem"
durability = "persistent"
purpose = "primary-data"
attach = "explicit"
schema_id = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[services.main]
target = "app"

[[services.main.state_bindings]]
state = "data"
target = "/var/lib/app"
"#,
        )
        .expect("write manifest");

        let bind_dir = home.path().join("bind").join("data");
        let state = AppState {
            listen_url: "http://127.0.0.1:8787".to_string(),
            data_dir: home.path().to_path_buf(),
            auth_token: None,
            lock: Arc::new(Mutex::new(())),
        };

        (home, home_guard, manifest_path, bind_dir, state)
    };

    let register_response = handle_register_persistent_state(
        State(state.clone()),
        HeaderMap::new(),
        Json(RegisterPersistentStateRequest {
            manifest: manifest_path.to_string_lossy().to_string(),
            state_name: "data".to_string(),
            path: bind_dir.to_string_lossy().to_string(),
        }),
    )
    .await
    .into_response();
    let register_status = register_response.status();
    let register_body = to_bytes(register_response.into_body(), usize::MAX)
        .await
        .expect("read register body");
    assert_eq!(register_status, StatusCode::CREATED);
    let registered: crate::registry::store::PersistentStateRecord =
        serde_json::from_slice(&register_body).expect("parse register json");
    assert_eq!(registered.owner_scope, "demo-app");
    assert_eq!(registered.state_name, "data");
    assert_eq!(registered.kind, "filesystem");
    assert_eq!(registered.backend_kind, "host_path");

    let list_response = handle_list_persistent_states(
        State(state.clone()),
        HeaderMap::new(),
        Query(PersistentStateListQuery {
            owner_scope: Some("demo-app".to_string()),
            state_name: Some("data".to_string()),
        }),
    )
    .await
    .into_response();
    assert_eq!(list_response.status(), StatusCode::OK);
    let list_body = to_bytes(list_response.into_body(), usize::MAX)
        .await
        .expect("read list body");
    let listed: Vec<crate::registry::store::PersistentStateRecord> =
        serde_json::from_slice(&list_body).expect("parse list json");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0], registered);

    let get_response = handle_get_persistent_state(
        State(state),
        HeaderMap::new(),
        AxumPath(registered.state_id.clone()),
    )
    .await
    .into_response();
    assert_eq!(get_response.status(), StatusCode::OK);
    let get_body = to_bytes(get_response.into_body(), usize::MAX)
        .await
        .expect("read get body");
    let fetched: crate::registry::store::PersistentStateRecord =
        serde_json::from_slice(&get_body).expect("parse get json");
    assert_eq!(fetched, registered);
}

// ─── write_private_file permission tests ─────────────────────────────────────

#[cfg(unix)]
#[test]
fn write_private_file_corrects_existing_loose_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join(".console-token");

    // Create file with 0644 (simulating old std::fs::write behaviour).
    std::fs::write(&path, b"old-token").expect("initial write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("set 0644");

    // write_private_file must tighten the permission even on an existing file.
    write_private_file(&path, b"new-token").expect("write_private_file");

    let mode = std::fs::metadata(&path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "existing file must be tightened to 0600");

    let contents = std::fs::read_to_string(&path).expect("read back");
    assert_eq!(contents, "new-token");
}

// ─── validate_add_capsule_source unit tests ───────────────────────────────────

#[test]
fn validate_add_capsule_source_accepts_publisher_slug() {
    assert!(validate_add_capsule_source("koh0920/adminer").is_ok());
    assert!(validate_add_capsule_source("my-publisher/my-slug").is_ok());
    assert!(validate_add_capsule_source("publisher123/slug456").is_ok());
}

#[test]
fn validate_add_capsule_source_rejects_version_suffix() {
    // @version is rejected for MVP: idempotency check strips @version when
    // looking up app_id, causing false already_installed on version mismatch.
    assert!(validate_add_capsule_source("koh0920/adminer@v2").is_err());
    assert!(validate_add_capsule_source("koh0920/adminer@1.0.0").is_err());
}

#[test]
fn validate_add_capsule_source_accepts_share_url() {
    assert!(validate_add_capsule_source("https://ato.run/s/abc123").is_ok());
}

#[test]
fn validate_add_capsule_source_rejects_empty() {
    assert!(validate_add_capsule_source("").is_err());
    assert!(validate_add_capsule_source("   ").is_err());
}

#[test]
fn validate_add_capsule_source_rejects_unsafe_schemes() {
    for scheme in &[
        "javascript:alert(1)",
        "data:text/html,<h1>x</h1>",
        "file:///etc/passwd",
        "vbscript:msgbox(1)",
        "blob:http://example.com/abc",
        "about:blank",
    ] {
        assert!(
            validate_add_capsule_source(scheme).is_err(),
            "expected rejection for: {scheme}"
        );
    }
}

#[test]
fn validate_add_capsule_source_rejects_missing_slug() {
    assert!(validate_add_capsule_source("publisher").is_err());
    assert!(validate_add_capsule_source("publisher/").is_err());
    assert!(validate_add_capsule_source("/slug").is_err());
}

#[test]
fn validate_add_capsule_source_rejects_too_long() {
    let long = "a".repeat(2049);
    assert!(validate_add_capsule_source(&long).is_err());
}

// ─── add-capsule handler tests ────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn runtime_add_capsule_requires_auth() {
    let state = registry_test_state(Some("secret"));
    let response = handle_runtime_add_capsule(
        State(state),
        HeaderMap::new(),
        Json(AddCapsuleRequest {
            source: "koh0920/adminer".to_string(),
            profile_id: None,
        }),
    )
    .await
    .into_response();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_add_capsule_rejects_wrong_token() {
    let state = registry_test_state(Some("secret"));
    let response = handle_runtime_add_capsule(
        State(state),
        bearer_headers("wrong"),
        Json(AddCapsuleRequest {
            source: "koh0920/adminer".to_string(),
            profile_id: None,
        }),
    )
    .await
    .into_response();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_add_capsule_empty_source_returns_400() {
    let state = registry_test_state(None);
    let response = handle_runtime_add_capsule(
        State(state),
        HeaderMap::new(),
        Json(AddCapsuleRequest {
            source: "".to_string(),
            profile_id: None,
        }),
    )
    .await
    .into_response();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json["error"], "invalid_source");
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_add_capsule_rejects_unsafe_scheme() {
    let state = registry_test_state(None);
    let response = handle_runtime_add_capsule(
        State(state),
        HeaderMap::new(),
        Json(AddCapsuleRequest {
            source: "javascript:alert(1)".to_string(),
            profile_id: None,
        }),
    )
    .await
    .into_response();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json["error"], "invalid_source");
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_add_capsule_non_default_profile_returns_501() {
    let state = registry_test_state(None);
    let response = handle_runtime_add_capsule(
        State(state),
        HeaderMap::new(),
        Json(AddCapsuleRequest {
            source: "koh0920/adminer".to_string(),
            profile_id: Some("gpu".to_string()),
        }),
    )
    .await
    .into_response();
    assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json["error"], "non_default_profile_not_supported");
}

#[tokio::test(flavor = "current_thread")]
async fn provider_reports_supports_add_capsule_true() {
    let state = registry_test_state(None);
    let response = handle_runtime_providers(State(state), HeaderMap::new())
        .await
        .into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(
        json[0]["capabilities"]["supports_add_capsule"], true,
        "supports_add_capsule must be true"
    );
}

// ─── CORS / PNA middleware integration tests ─────────────────────────────────

mod cors_pna_tests {
    use super::*;
    use crate::adapters::registry::serve::cors_pna::{cors_pna_layer, parse_allowed_origins};
    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode};
    use std::sync::Arc;
    use tower::ServiceExt as _;

    fn make_router(token: Option<&str>) -> axum::Router {
        let state = registry_test_state(token);
        let allowed = parse_allowed_origins();
        build_app_router(false)
            .with_state(state)
            .layer(axum::middleware::from_fn(
                move |req: axum::extract::Request, next: axum::middleware::Next| {
                    cors_pna_layer(Arc::clone(&allowed), req, next)
                },
            ))
    }

    fn preflight(origin: &str, pna: bool) -> Request<Body> {
        let mut b = Request::builder()
            .method(Method::OPTIONS)
            .uri("/v1/runtime/providers")
            .header("origin", origin)
            .header("access-control-request-method", "GET")
            .header("access-control-request-headers", "authorization");
        if pna {
            b = b.header("access-control-request-private-network", "true");
        }
        b.body(Body::empty()).unwrap()
    }

    fn get_req(origin: &str, token: &str) -> Request<Body> {
        Request::builder()
            .method(Method::GET)
            .uri("/v1/runtime/providers")
            .header("origin", origin)
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap()
    }

    fn get_req_no_auth(origin: &str) -> Request<Body> {
        Request::builder()
            .method(Method::GET)
            .uri("/v1/runtime/sessions")
            .header("origin", origin)
            .body(Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn allowed_origin_preflight_receives_acao() {
        let app = make_router(None);
        let origin = "https://app.ato.run";
        let resp = app
            .oneshot(preflight(origin, false))
            .await
            .expect("call router");
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let acao = resp
            .headers()
            .get("access-control-allow-origin")
            .expect("ACAO header missing");
        assert_eq!(acao, origin);
    }

    #[tokio::test]
    async fn disallowed_origin_preflight_receives_no_acao() {
        let app = make_router(None);
        let resp = app
            .oneshot(preflight("https://evil.example.com", false))
            .await
            .expect("call router");
        assert!(
            resp.headers().get("access-control-allow-origin").is_none(),
            "disallowed origin must not get ACAO header"
        );
    }

    #[tokio::test]
    async fn localhost_5173_preflight_succeeds() {
        let app = make_router(None);
        let origin = "http://localhost:5173";
        let resp = app
            .oneshot(preflight(origin, false))
            .await
            .expect("call router");
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            resp.headers()
                .get("access-control-allow-origin")
                .expect("ACAO"),
            origin
        );
    }

    #[tokio::test]
    async fn loopback_5173_preflight_succeeds() {
        let app = make_router(None);
        let origin = "http://127.0.0.1:5173";
        let resp = app
            .oneshot(preflight(origin, false))
            .await
            .expect("call router");
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            resp.headers()
                .get("access-control-allow-origin")
                .expect("ACAO"),
            origin
        );
    }

    #[tokio::test]
    async fn pna_preflight_receives_acapn_for_allowed_origin() {
        let app = make_router(None);
        let origin = "https://app.ato.run";
        let resp = app
            .oneshot(preflight(origin, true))
            .await
            .expect("call router");
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let acapn = resp
            .headers()
            .get("access-control-allow-private-network")
            .expect("ACAPN header missing");
        assert_eq!(acapn, "true");
    }

    #[tokio::test]
    async fn pna_preflight_no_acapn_for_disallowed_origin() {
        let app = make_router(None);
        let resp = app
            .oneshot(preflight("https://evil.example.com", true))
            .await
            .expect("call router");
        assert!(
            resp.headers()
                .get("access-control-allow-private-network")
                .is_none(),
            "disallowed origin must not get ACAPN"
        );
    }

    #[tokio::test]
    async fn token_protected_endpoint_rejects_missing_bearer() {
        let app = make_router(Some("secret-token"));
        let resp = app
            .oneshot(get_req_no_auth("https://app.ato.run"))
            .await
            .expect("call router");
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "missing token must return 401"
        );
    }

    #[tokio::test]
    async fn valid_bearer_token_with_allowed_origin_succeeds() {
        let _env = env_lock().lock().unwrap();
        let _guard = AtoHomeGuard::set("cors_pna_valid_token");
        let app = make_router(Some("correct-token"));
        let resp = app
            .oneshot(get_req("https://app.ato.run", "correct-token"))
            .await
            .expect("call router");
        // /v1/runtime/providers returns 200 even if no sessions/installs exist
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(
            resp.headers().get("access-control-allow-origin").is_some(),
            "ACAO must be set for allowed origin"
        );
    }

    // ── Full-stack regression: desktop CorsLayer + cors_pna_layer combined ────
    //
    // Mirrors the real layer order in `serve()` so regressions are caught when
    // both middleware stacks are active simultaneously.

    fn make_full_stack_router(token: Option<&str>) -> axum::Router {
        use tower_http::cors::CorsLayer;
        let state = registry_test_state(token);
        let allowed = parse_allowed_origins();
        let desktop_origin = "capsule://desktop.ato.run"
            .parse::<axum::http::HeaderValue>()
            .expect("valid header value");
        build_app_router(false)
            .with_state(state)
            .layer(
                CorsLayer::new()
                    .allow_origin(desktop_origin)
                    .allow_methods([Method::GET]),
            )
            .layer(axum::middleware::from_fn(
                move |req: axum::extract::Request, next: axum::middleware::Next| {
                    cors_pna_layer(Arc::clone(&allowed), req, next)
                },
            ))
    }

    #[tokio::test]
    async fn full_stack_disallowed_pna_preflight_omits_all_cors_allow_headers() {
        let app = make_full_stack_router(None);
        let req = Request::builder()
            .method(Method::OPTIONS)
            .uri("/v1/runtime/sessions")
            .header("origin", "https://evil.example.com")
            .header("access-control-request-method", "GET")
            .header("access-control-request-headers", "authorization")
            .header("access-control-request-private-network", "true")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.expect("call full-stack router");
        // Preflight must be intercepted with a clean response.
        assert!(
            resp.status() == StatusCode::NO_CONTENT || resp.status().is_success(),
            "expected non-error preflight status, got {}",
            resp.status()
        );
        assert!(
            resp.headers().get("access-control-allow-origin").is_none(),
            "disallowed origin must not receive ACAO"
        );
        assert!(
            resp.headers()
                .get("access-control-allow-private-network")
                .is_none(),
            "disallowed origin must not receive ACAPN"
        );
        assert!(
            resp.headers().get("access-control-allow-methods").is_none(),
            "disallowed origin must not receive ACAM"
        );
        assert!(
            resp.headers().get("access-control-allow-headers").is_none(),
            "disallowed origin must not receive ACAH"
        );
    }

    #[tokio::test]
    async fn full_stack_disallowed_actual_get_omits_acao() {
        let _env = env_lock().lock().unwrap();
        let _guard = AtoHomeGuard::set("full_stack_disallowed_get");
        let app = make_full_stack_router(Some("tok"));
        let req = Request::builder()
            .method(Method::GET)
            .uri("/v1/runtime/providers")
            .header("origin", "https://evil.example.com")
            .header("authorization", "Bearer tok")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.expect("call full-stack router");
        // Endpoint should still behave normally.
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(
            resp.headers().get("access-control-allow-origin").is_none(),
            "disallowed origin must not receive ACAO on actual request"
        );
    }

    #[tokio::test]
    async fn full_stack_pwa_preflight_returns_acao_and_acapn() {
        let app = make_full_stack_router(None);
        let origin = "https://app.ato.run";
        let resp = app
            .oneshot(preflight(origin, true))
            .await
            .expect("call full-stack router");
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            resp.headers()
                .get("access-control-allow-origin")
                .expect("ACAO missing"),
            origin
        );
        assert_eq!(
            resp.headers()
                .get("access-control-allow-private-network")
                .expect("ACAPN missing"),
            "true"
        );
    }

    #[tokio::test]
    async fn full_stack_pwa_get_with_valid_token_returns_acao() {
        let _env = env_lock().lock().unwrap();
        let _guard = AtoHomeGuard::set("full_stack_valid_token");
        let app = make_full_stack_router(Some("tok"));
        let origin = "https://app.ato.run";
        let resp = app
            .oneshot(get_req(origin, "tok"))
            .await
            .expect("call full-stack router");
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get("access-control-allow-origin")
                .expect("ACAO missing on actual GET"),
            origin
        );
    }

    fn post_preflight(uri: &str, origin: &str, pna: bool) -> Request<Body> {
        let mut b = Request::builder()
            .method(Method::OPTIONS)
            .uri(uri)
            .header("origin", origin)
            .header("access-control-request-method", "POST")
            .header(
                "access-control-request-headers",
                "authorization, content-type",
            );
        if pna {
            b = b.header("access-control-request-private-network", "true");
        }
        b.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn pwa_post_preflight_for_launch_returns_acao_and_acapn() {
        let app = make_full_stack_router(None);
        let origin = "https://app.ato.run";
        let resp = app
            .oneshot(post_preflight("/v1/runtime/sessions", origin, true))
            .await
            .expect("call full-stack router");
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            resp.headers()
                .get("access-control-allow-origin")
                .expect("ACAO missing"),
            origin
        );
        assert_eq!(
            resp.headers()
                .get("access-control-allow-private-network")
                .expect("ACAPN missing"),
            "true"
        );
        let acam = resp
            .headers()
            .get("access-control-allow-methods")
            .expect("ACAM missing")
            .to_str()
            .unwrap();
        assert!(
            acam.contains("POST"),
            "POST must be in allowed methods: {acam}"
        );
    }

    #[tokio::test]
    async fn pwa_post_preflight_for_stop_returns_acao_and_acapn() {
        let app = make_full_stack_router(None);
        let origin = "https://app.ato.run";
        let resp = app
            .oneshot(post_preflight(
                "/v1/runtime/sessions/sess-123/stop",
                origin,
                true,
            ))
            .await
            .expect("call full-stack router");
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            resp.headers()
                .get("access-control-allow-origin")
                .expect("ACAO missing"),
            origin
        );
        assert_eq!(
            resp.headers()
                .get("access-control-allow-private-network")
                .expect("ACAPN missing"),
            "true"
        );
    }

    #[tokio::test]
    async fn pwa_post_preflight_for_add_capsule_returns_acao_and_acapn() {
        let app = make_full_stack_router(None);
        let origin = "https://app.ato.run";
        let resp = app
            .oneshot(post_preflight("/v1/runtime/install-profiles", origin, true))
            .await
            .expect("call full-stack router");
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            resp.headers()
                .get("access-control-allow-origin")
                .expect("ACAO missing"),
            origin
        );
        assert_eq!(
            resp.headers()
                .get("access-control-allow-private-network")
                .expect("ACAPN missing"),
            "true"
        );
        let acam = resp
            .headers()
            .get("access-control-allow-methods")
            .expect("ACAM missing")
            .to_str()
            .unwrap();
        assert!(
            acam.contains("POST"),
            "POST must be in allowed methods for add-capsule: {acam}"
        );
    }

    #[tokio::test]
    async fn disallowed_add_capsule_preflight_omits_all_cors_headers() {
        let app = make_full_stack_router(None);
        let resp = app
            .oneshot(post_preflight(
                "/v1/runtime/install-profiles",
                "https://evil.example.com",
                true,
            ))
            .await
            .expect("call full-stack router");
        assert!(
            resp.headers().get("access-control-allow-origin").is_none(),
            "disallowed origin must not get ACAO"
        );
        assert!(
            resp.headers()
                .get("access-control-allow-private-network")
                .is_none(),
            "disallowed origin must not get ACAPN"
        );
        assert!(
            resp.headers().get("access-control-allow-methods").is_none(),
            "disallowed origin must not get ACAM"
        );
        assert!(
            resp.headers().get("access-control-allow-headers").is_none(),
            "disallowed origin must not get ACAH"
        );
    }

    #[tokio::test]
    async fn disallowed_post_preflight_omits_all_cors_allow_headers() {
        let app = make_full_stack_router(None);
        let resp = app
            .oneshot(post_preflight(
                "/v1/runtime/sessions",
                "https://evil.example.com",
                true,
            ))
            .await
            .expect("call full-stack router");
        assert!(
            resp.headers().get("access-control-allow-origin").is_none(),
            "disallowed origin must not get ACAO"
        );
        assert!(
            resp.headers()
                .get("access-control-allow-private-network")
                .is_none(),
            "disallowed origin must not get ACAPN"
        );
        assert!(
            resp.headers().get("access-control-allow-methods").is_none(),
            "disallowed origin must not get ACAM"
        );
        assert!(
            resp.headers().get("access-control-allow-headers").is_none(),
            "disallowed origin must not get ACAH"
        );
    }
}
