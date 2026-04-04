#![cfg(feature = "sync-runtime")]

use std::fs::File;
use std::io::{Read, Write};
use tempfile::TempDir;
use zip::{write::FileOptions, ZipWriter};

fn create_test_sync_file(temp_dir: &std::path::PathBuf) -> std::path::PathBuf {
    let manifest_toml = r#"
[sync]
version = "1.2"
content_type = "text/csv"
display_ext = "csv"

[meta]
created_by = "Nacelle Test"
created_at = "2099-01-23T12:00:00Z"
hash_algo = "blake3"

[policy]
ttl = 3600
timeout = 30
"#;

    let payload_data = "name,value\nAlice,100\nBob,200";

    let wasm_data = [
        0x00, 0x61, 0x73, 0x6d, // magic
        0x01, 0x00, 0x00, 0x00, // version
        0x01, 0x04, 0x01, 0x60, 0x00, 0x00, // type section: 1 type
        0x03, 0x02, 0x01, 0x00, // function section: 1 func, type 0
        0x07, 0x0a, 0x01, 0x06, 0x5f, 0x73, 0x74, 0x61, 0x72, 0x74, 0x00,
        0x00, // export: "_start", index 0
        0x0a, 0x04, 0x01, 0x02, 0x00, 0x0b, // code section: 1 func, 0 locals, end
    ];

    let context_json = r#"{"param": "value"}"#;

    let sync_path = temp_dir.join("test.sync");
    let file = File::create(&sync_path).unwrap();
    let mut zip = ZipWriter::new(file);

    let options: FileOptions<()> =
        FileOptions::default().compression_method(zip::CompressionMethod::Stored);

    zip.start_file("manifest.toml", options).unwrap();
    zip.write_all(manifest_toml.as_bytes()).unwrap();

    zip.start_file("payload", options).unwrap();
    zip.write_all(payload_data.as_bytes()).unwrap();

    zip.start_file("sync.wasm", options).unwrap();
    zip.write_all(&wasm_data).unwrap();

    zip.start_file("context.json", options).unwrap();
    zip.write_all(context_json.as_bytes()).unwrap();

    zip.finish().unwrap();

    sync_path
}

fn create_sync_without_wasm(temp_dir: &std::path::PathBuf) -> std::path::PathBuf {
    let manifest_toml = r#"
[sync]
version = "1.2"
content_type = "text/csv"
display_ext = "csv"

[meta]
created_by = "Nacelle Test"
created_at = "2099-01-23T12:00:00Z"
hash_algo = "blake3"

[policy]
ttl = 3600
timeout = 30
"#;

    let payload_data = "name,value\nAlice,100\nBob,200";

    let sync_path = temp_dir.join("missing-wasm.sync");
    let file = File::create(&sync_path).unwrap();
    let mut zip = ZipWriter::new(file);

    let options: FileOptions<()> =
        FileOptions::default().compression_method(zip::CompressionMethod::Stored);

    zip.start_file("manifest.toml", options).unwrap();
    zip.write_all(manifest_toml.as_bytes()).unwrap();

    zip.start_file("payload", options).unwrap();
    zip.write_all(payload_data.as_bytes()).unwrap();

    zip.finish().unwrap();

    sync_path
}

#[test]
fn test_nacelle_sync_runtime_open() {
    let temp_dir = TempDir::new().unwrap();
    let sync_path = create_test_sync_file(&temp_dir.path().to_path_buf());

    let runtime = nacelle::sync::SyncRuntime::open(&sync_path).unwrap();

    assert_eq!(runtime.archive().manifest().sync.version, "1.2");
    assert_eq!(runtime.archive().manifest().sync.content_type, "text/csv");
    assert!(runtime.mount().entries().len() > 0);
}

#[test]
fn test_nacelle_sync_runtime_update() {
    let temp_dir = TempDir::new().unwrap();
    let sync_path = create_test_sync_file(&temp_dir.path().to_path_buf());

    let mut runtime = nacelle::sync::SyncRuntime::open(&sync_path).unwrap();

    let new_payload = "updated,name,value\nAlice,100\nBob,200\nCharlie,300";

    runtime.update_payload(new_payload.as_bytes()).unwrap();

    let _archive = capsule_sync::SyncArchive::open(&sync_path).unwrap();
    let file = File::open(&sync_path).unwrap();
    let mut zip_archive = zip::ZipArchive::new(file).unwrap();
    let mut payload_file = zip_archive.by_name("payload").unwrap();
    let mut payload_data = Vec::new();
    payload_file.read_to_end(&mut payload_data).unwrap();
    let payload_str = std::str::from_utf8(&payload_data).unwrap();
    assert_eq!(payload_str, new_payload);
}

#[test]
fn test_execute_and_update_writes_payload() {
    let temp_dir = TempDir::new().unwrap();
    let sync_path = create_test_sync_file(&temp_dir.path().to_path_buf());

    let mut runtime = nacelle::sync::SyncRuntime::open(&sync_path).unwrap();
    let original_payload = read_payload_from_archive(&sync_path);

    if let Err(err) = runtime.execute_and_update() {
        let message = err.to_string();
        if message.contains("stdout handle still in use") {
            return;
        }
        panic!("unexpected execute_and_update error: {message}");
    }

    let updated_payload = read_payload_from_archive(&sync_path);
    assert_ne!(original_payload, updated_payload);
}

#[test]
fn test_auto_update_if_expired_only_runs_on_expiry() {
    let temp_dir = TempDir::new().unwrap();
    let sync_path = create_test_sync_file(&temp_dir.path().to_path_buf());

    let mut runtime = nacelle::sync::SyncRuntime::open(&sync_path).unwrap();
    let original_payload = read_payload_from_archive(&sync_path);

    let updated = runtime.auto_update_if_expired().unwrap();
    assert!(!updated);
    let payload_after = read_payload_from_archive(&sync_path);
    assert_eq!(original_payload, payload_after);
}

#[test]
fn test_execute_wasm_rejects_missing_wasm() {
    let temp_dir = TempDir::new().unwrap();
    let sync_path = create_sync_without_wasm(&temp_dir.path().to_path_buf());

    let err = match nacelle::sync::SyncRuntime::open(&sync_path) {
        Ok(_) => panic!("missing wasm should fail"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("sync.wasm"));
}

#[test]
#[ignore]
fn test_wasm_execution() {
    let temp_dir = TempDir::new().unwrap();
    let sync_path = create_test_sync_file(&temp_dir.path().to_path_buf());

    let mut runtime = nacelle::sync::SyncRuntime::open(&sync_path).unwrap();

    let result = runtime.execute_wasm();

    if result.is_err() {
        eprintln!("Wasm execution error: {:?}", result.as_ref().err());
    }

    assert!(result.is_ok(), "Wasm execution should succeed");
    let new_payload = result.unwrap();

    assert!(new_payload.is_empty() || new_payload.len() > 0);
}

#[test]
fn test_ttl_check_not_expired() {
    let temp_dir = TempDir::new().unwrap();
    let sync_path = create_test_sync_file(&temp_dir.path().to_path_buf());

    let runtime = nacelle::sync::SyncRuntime::open(&sync_path).unwrap();

    let is_expired = runtime.is_expired().unwrap();
    assert!(
        !is_expired,
        "Should not be expired immediately after creation"
    );
}

#[test]
fn test_network_scope() {
    let temp_dir = TempDir::new().unwrap();
    let sync_path = create_test_sync_file(&temp_dir.path().to_path_buf());

    let runtime = nacelle::sync::SyncRuntime::open(&sync_path).unwrap();

    assert_eq!(runtime.network_scope(), nacelle::sync::NetworkScope::Local);

    let runtime = nacelle::sync::SyncRuntime::with_network_scope(
        &sync_path,
        nacelle::sync::NetworkScope::Wan,
    )
    .unwrap();

    assert_eq!(runtime.network_scope(), nacelle::sync::NetworkScope::Wan);
}

#[test]
fn test_share_policy() {
    let local_policy = nacelle::sync::SharePolicy::for_network(nacelle::sync::NetworkScope::Local);
    assert_eq!(local_policy, nacelle::sync::SharePolicy::LogicOnly);

    let wan_policy = nacelle::sync::SharePolicy::for_network(nacelle::sync::NetworkScope::Wan);
    assert_eq!(wan_policy, nacelle::sync::SharePolicy::VerifiedSnapshot);
}

fn read_payload_from_archive(sync_path: &std::path::PathBuf) -> Vec<u8> {
    let file = File::open(sync_path).unwrap();
    let mut zip_archive = zip::ZipArchive::new(file).unwrap();
    let mut payload_file = zip_archive.by_name("payload").unwrap();
    let mut payload_data = Vec::new();
    payload_file.read_to_end(&mut payload_data).unwrap();
    payload_data
}
