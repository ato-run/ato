use super::*;
use axum::body::to_bytes;
use std::io::{Cursor, Write};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Mutex as StdMutex, OnceLock};

fn env_lock() -> &'static StdMutex<()> {
    static LOCK: OnceLock<StdMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| StdMutex::new(()))
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
        std::env::set_var("HOME", path);
        Self { previous }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            std::env::set_var("HOME", previous);
        } else {
            std::env::remove_var("HOME");
        }
    }
}

fn build_capsule_bytes(manifest: &str) -> Vec<u8> {
    build_capsule_bytes_with_files(manifest, &[("README.md", b"dummy".as_slice())])
}

fn build_capsule_bytes_with_files(manifest: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
    let payload_tar = build_payload_tar().expect("build payload tar");
    let parsed_manifest =
        capsule_core::types::CapsuleManifest::from_toml(manifest).expect("parse manifest");
    let (distribution_manifest, _) =
        capsule_core::packers::payload::build_distribution_manifest(&parsed_manifest, &payload_tar)
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
        capsule_core::types::CapsuleManifest::from_toml(manifest).expect("parse manifest");
    let (distribution_manifest, _) =
        capsule_core::packers::payload::build_distribution_manifest(&parsed_manifest, &payload_tar)
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
    let manifest = r#"schema_version = "0.2"
name = "sample"
version = "1.0.0"
type = "app"
default_target = "cli"
"#;
    let bytes = build_capsule_bytes(manifest);
    let extracted = extract_manifest_from_capsule(&bytes).expect("extract");
    assert!(extracted.contains("name = \"sample\""));
}

#[test]
fn extract_readme_from_capsule_prefers_priority_order() {
    let manifest = r#"schema_version = "0.2"
name = "sample"
version = "1.0.0"
type = "app"
default_target = "cli"
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
    let manifest = r#"schema_version = "0.2"
name = "sample"
version = "1.0.0"
type = "app"
default_target = "cli"
"#;
    let large = vec![b'a'; README_MAX_BYTES + 4096];
    let bytes = build_capsule_bytes_with_files(manifest, &[("README.md", &large)]);
    let extracted = extract_readme_from_capsule(&bytes).expect("extract readme");
    assert_eq!(extracted.len(), README_MAX_BYTES);
}

#[test]
fn extract_readme_from_capsule_reads_payload_tar_zst_contents() {
    let manifest = r#"schema_version = "0.2"
name = "sample"
version = "1.0.0"
type = "app"
default_target = "cli"
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
    let manifest = r#"schema_version = "0.2"
name = "sample"
version = "1.0.0"
type = "app"
default_target = "cli"

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
    assert!(manifest_toml
        .as_deref()
        .is_some_and(|raw| raw.contains("default_target = \"cli\"")));
    assert!(capsule_lock.is_none());
    assert_eq!(readme_markdown.as_deref(), Some("dummy"));
    assert_eq!(readme_source.as_deref(), Some("artifact"));
}

#[test]
fn normalize_ui_path_maps_root_to_index() {
    assert_eq!(normalize_ui_path("/").as_deref(), Some("index.html"),);
    assert_eq!(
        normalize_ui_path("/assets/index.js").as_deref(),
        Some("assets/index.js"),
    );
    assert!(normalize_ui_path("/../../etc/passwd").is_none());
}

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
async fn persistent_state_local_api_registers_and_lists_records() {
    let (_home, _home_guard, manifest_path, bind_dir, state) = {
        let _guard = env_lock().lock().expect("env lock");
        let home = tempfile::tempdir().expect("home");
        let home_guard = HomeGuard::set(home.path());

        let manifest_dir = home.path().join("workspace");
        std::fs::create_dir_all(&manifest_dir).expect("create manifest dir");
        let manifest_path = manifest_dir.join("capsule.toml");
        std::fs::write(
            &manifest_path,
            r#"
schema_version = "0.2"
name = "demo-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
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
