use super::*;
use crate::application::ports::publish::{PublishArtifactIdentityClass, PublishArtifactMetadata};
use rusqlite::Connection;
use std::io::Cursor;
use std::io::Write;

fn manifest(version: &str) -> String {
    format!(
        r#"
schema_version = "0.3"
name = "sample"
version = "{}"
type = "app"
"#,
        version
    )
}

fn build_capsule_bytes(manifest: &str) -> Vec<u8> {
    let payload_tar = build_payload_tar().expect("build payload tar");
    let parsed_manifest = CapsuleManifest::from_toml(manifest).expect("parse manifest");
    let (_, manifest_bytes) =
        manifest_payload::build_distribution_manifest(&parsed_manifest, &payload_tar)
            .expect("build manifest");
    let payload_zst =
        zstd::stream::encode_all(Cursor::new(payload_tar), 1).expect("encode payload");

    let mut capsule = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut capsule);

        let mut manifest_header = tar::Header::new_gnu();
        manifest_header.set_size(manifest_bytes.len() as u64);
        manifest_header.set_mode(0o644);
        manifest_header.set_mtime(0);
        manifest_header.set_cksum();
        builder
            .append_data(
                &mut manifest_header,
                "capsule.toml",
                Cursor::new(manifest_bytes),
            )
            .expect("append manifest");

        let mut payload_header = tar::Header::new_gnu();
        payload_header.set_size(payload_zst.len() as u64);
        payload_header.set_mode(0o644);
        payload_header.set_mtime(0);
        payload_header.set_cksum();
        builder
            .append_data(
                &mut payload_header,
                "payload.tar.zst",
                Cursor::new(payload_zst),
            )
            .expect("append payload");

        builder.finish().expect("finish capsule");
    }
    capsule
}

fn build_payload_tar() -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut payload);
        let source = b"print('hello from payload')\n";
        let mut header = tar::Header::new_gnu();
        header.set_path("main.py")?;
        header.set_mode(0o644);
        header.set_size(source.len() as u64);
        header.set_mtime(0);
        header.set_cksum();
        builder.append_data(&mut header, "main.py", Cursor::new(source))?;
        builder.finish()?;
    }
    payload.flush().expect("flush payload vec");
    Ok(payload)
}

#[test]
fn rollback_creates_forward_epoch_transition() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = RegistryStore::open(temp.path()).expect("open store");

    let first = store
        .record_manifest_and_epoch(
            "koh0920/sample",
            &manifest("1.0.0"),
            b"payload-v1",
            "2026-03-05T00:00:00Z",
        )
        .expect("record first");
    let second = store
        .record_manifest_and_epoch(
            "koh0920/sample",
            &manifest("1.1.0"),
            b"payload-v2",
            "2026-03-05T00:00:01Z",
        )
        .expect("record second");
    assert_eq!(second.pointer.epoch, first.pointer.epoch + 1);

    let rolled = store
        .rollback_to_manifest("koh0920/sample", &first.pointer.manifest_hash)
        .expect("rollback")
        .expect("rollback response");
    assert_eq!(rolled.pointer.manifest_hash, first.pointer.manifest_hash);
    assert_eq!(rolled.pointer.epoch, second.pointer.epoch + 1);
}

#[test]
fn open_migrates_legacy_registry_releases_before_creating_lock_id_index() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join(DB_FILE_NAME);
    let conn = Connection::open(&db_path).expect("open legacy db");
    conn.execute_batch(
        "
        CREATE TABLE registry_releases(
          scoped_id TEXT NOT NULL,
          version TEXT NOT NULL,
          manifest_hash TEXT NOT NULL,
          file_name TEXT NOT NULL,
          sha256 TEXT NOT NULL,
          blake3 TEXT NOT NULL,
          size_bytes INTEGER NOT NULL,
          signature_status TEXT NOT NULL,
          created_at TEXT NOT NULL,
          PRIMARY KEY(scoped_id, version)
        );
        CREATE TABLE IF NOT EXISTS schema_migrations(
          migration_id TEXT PRIMARY KEY,
          applied_at TEXT NOT NULL
        );
        ",
    )
    .expect("seed legacy schema");
    drop(conn);

    let store = RegistryStore::open(temp.path()).expect("open migrated store");
    let conn = store.connect().expect("connect migrated store");

    let has_lock_id: bool = conn
        .prepare("PRAGMA table_info(registry_releases)")
        .expect("prepare table info")
        .query_map([], |row| row.get::<_, String>(1))
        .expect("query table info")
        .filter_map(Result::ok)
        .any(|column| column == "lock_id");
    assert!(has_lock_id);

    let has_lock_id_index: bool = conn
        .prepare("PRAGMA index_list(registry_releases)")
        .expect("prepare index list")
        .query_map([], |row| row.get::<_, String>(1))
        .expect("query index list")
        .filter_map(Result::ok)
        .any(|index| index == "idx_registry_releases_lock_id");
    assert!(has_lock_id_index);
}

#[test]
fn rollback_fails_when_chunk_missing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = RegistryStore::open(temp.path()).expect("open store");
    let first = store
        .record_manifest_and_epoch(
            "koh0920/sample",
            &manifest("1.0.0"),
            b"payload-v1",
            "2026-03-05T00:00:00Z",
        )
        .expect("record first");
    let _second = store
        .record_manifest_and_epoch(
            "koh0920/sample",
            &manifest("1.1.0"),
            b"payload-v2",
            "2026-03-05T00:00:01Z",
        )
        .expect("record second");
    let conn = store.connect().expect("connect");
    let chunk_hash: String = conn
        .query_row(
            "SELECT chunk_hash FROM manifest_chunks WHERE manifest_hash=?1 ORDER BY ordinal ASC LIMIT 1",
            params![&first.pointer.manifest_hash],
            |row| row.get(0),
        )
        .expect("chunk hash");
    let chunk_path = store.chunk_path(&normalize_blake3_hash(&chunk_hash));
    std::fs::remove_file(&chunk_path).expect("remove chunk");

    let err = store
        .rollback_to_manifest("koh0920/sample", &first.pointer.manifest_hash)
        .expect_err("rollback must fail");
    assert!(err
        .to_string()
        .contains(crate::error_codes::ATO_ERR_INTEGRITY_FAILURE));
}

#[test]
fn rollback_untombstones_manifest_and_chunks() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = RegistryStore::open(temp.path()).expect("open store");
    let first = store
        .record_manifest_and_epoch(
            "koh0920/sample",
            &manifest("1.0.0"),
            b"payload-v1",
            "2026-03-05T00:00:00Z",
        )
        .expect("record first");
    let _second = store
        .record_manifest_and_epoch(
            "koh0920/sample",
            &manifest("1.1.0"),
            b"payload-v2",
            "2026-03-05T00:00:01Z",
        )
        .expect("record second");
    store
        .tombstone_manifest("koh0920/sample", &first.pointer.manifest_hash)
        .expect("tombstone");

    let rolled = store
        .rollback_to_manifest("koh0920/sample", &first.pointer.manifest_hash)
        .expect("rollback")
        .expect("rollback result");
    assert_eq!(rolled.pointer.manifest_hash, first.pointer.manifest_hash);

    let conn = store.connect().expect("connect");
    let manifest_tombstoned: Option<String> = conn
        .query_row(
            "SELECT tombstoned_at FROM manifests WHERE manifest_hash=?1",
            params![&first.pointer.manifest_hash],
            |row| row.get(0),
        )
        .expect("manifest tombstoned");
    assert!(manifest_tombstoned.is_none());

    let still_tombstoned: i64 = conn
        .query_row(
            "SELECT COUNT(1)
                 FROM chunks
                 WHERE chunk_hash IN (
                   SELECT chunk_hash FROM manifest_chunks WHERE manifest_hash=?1
                 )
                 AND tombstoned_at IS NOT NULL",
            params![&first.pointer.manifest_hash],
            |row| row.get(0),
        )
        .expect("chunk tombstoned count");
    assert_eq!(still_tombstoned, 0);
}

#[test]
fn rollback_clears_gc_queue_for_target_chunks() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = RegistryStore::open(temp.path()).expect("open store");
    let first = store
        .record_manifest_and_epoch(
            "koh0920/sample",
            &manifest("1.0.0"),
            b"payload-v1",
            "2026-03-05T00:00:00Z",
        )
        .expect("record first");
    let _second = store
        .record_manifest_and_epoch(
            "koh0920/sample",
            &manifest("1.1.0"),
            b"payload-v2",
            "2026-03-05T00:00:01Z",
        )
        .expect("record second");
    store
        .tombstone_manifest("koh0920/sample", &first.pointer.manifest_hash)
        .expect("tombstone");
    store
        .enqueue_manifest_chunks_for_gc(
            &first.pointer.manifest_hash,
            "unit-test",
            &chrono::Utc::now().to_rfc3339(),
        )
        .expect("enqueue");

    let conn = store.connect().expect("connect");
    let queued_before: i64 = conn
        .query_row(
            "SELECT COUNT(1)
                 FROM gc_queue
                 WHERE chunk_hash IN (
                   SELECT chunk_hash FROM manifest_chunks WHERE manifest_hash=?1
                 )",
            params![&first.pointer.manifest_hash],
            |row| row.get(0),
        )
        .expect("queued before");
    assert!(queued_before > 0);

    store
        .rollback_to_manifest("koh0920/sample", &first.pointer.manifest_hash)
        .expect("rollback")
        .expect("rollback result");

    let queued_after: i64 = conn
        .query_row(
            "SELECT COUNT(1)
                 FROM gc_queue
                 WHERE chunk_hash IN (
                   SELECT chunk_hash FROM manifest_chunks WHERE manifest_hash=?1
                 )",
            params![&first.pointer.manifest_hash],
            |row| row.get(0),
        )
        .expect("queued after");
    assert_eq!(queued_after, 0);
}

#[test]
fn rollback_rejects_yanked_manifest() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = RegistryStore::open(temp.path()).expect("open store");
    let first = store
        .record_manifest_and_epoch(
            "koh0920/sample",
            &manifest("1.0.0"),
            b"payload-v1",
            "2026-03-05T00:00:00Z",
        )
        .expect("record first");
    let _second = store
        .record_manifest_and_epoch(
            "koh0920/sample",
            &manifest("1.1.0"),
            b"payload-v2",
            "2026-03-05T00:00:01Z",
        )
        .expect("record second");
    let yanked = store
        .yank_manifest("koh0920/sample", &first.pointer.manifest_hash)
        .expect("yank");
    assert!(yanked);

    let err = store
        .rollback_to_manifest("koh0920/sample", &first.pointer.manifest_hash)
        .expect_err("rollback must fail");
    assert!(err.to_string().contains("yanked"));
}

#[test]
fn negotiate_rejects_unknown_manifest_history() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = RegistryStore::open(temp.path()).expect("open store");
    store
        .record_manifest_and_epoch(
            "koh0920/sample",
            &manifest("1.0.0"),
            b"payload-v1",
            "2026-03-05T00:00:00Z",
        )
        .expect("record first");
    let err = store
        .negotiate(&NegotiateRequest {
            scoped_id: "koh0920/sample".to_string(),
            target_manifest_hash:
                "blake3:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
                    .to_string(),
            have_chunks: vec![],
            have_chunks_bloom: None,
            reuse_lease_id: None,
            max_bytes: None,
        })
        .expect_err("unknown manifest must fail");
    assert!(err
        .to_string()
        .contains("target manifest is not part of scoped capsule history"));
}

#[test]
fn acquire_manifest_lease_tracks_chunks() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = RegistryStore::open(temp.path()).expect("open store");
    let recorded = store
        .record_manifest_and_epoch(
            "koh0920/sample",
            &manifest("1.0.0"),
            b"payload-v1",
            "2026-03-05T00:00:00Z",
        )
        .expect("record");
    let lease = store
        .acquire_manifest_lease(
            "koh0920/sample",
            &recorded.pointer.manifest_hash,
            "test-owner",
            "unit-test",
            900,
        )
        .expect("acquire lease");
    assert!(lease.chunk_count >= 1);

    let conn = store.connect().expect("connect");
    let rows: i64 = conn
        .query_row(
            "SELECT COUNT(1) FROM leases WHERE lease_id=?1",
            params![lease.lease_id],
            |row| row.get(0),
        )
        .expect("lease rows");
    assert_eq!(rows as usize, lease.chunk_count);
}

#[test]
fn gc_tick_cleans_expired_leases_first() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = RegistryStore::open(temp.path()).expect("open store");
    let recorded = store
        .record_manifest_and_epoch(
            "koh0920/sample",
            &manifest("1.0.0"),
            b"payload-v1",
            "2026-03-05T00:00:00Z",
        )
        .expect("record");
    let lease = store
        .acquire_manifest_lease(
            "koh0920/sample",
            &recorded.pointer.manifest_hash,
            "test-owner",
            "unit-test",
            900,
        )
        .expect("acquire lease");
    let conn = store.connect().expect("connect");
    conn.execute(
        "UPDATE leases SET expires_at='1970-01-01T00:00:00Z' WHERE lease_id=?1",
        params![lease.lease_id],
    )
    .expect("expire lease");

    let tick = store
        .gc_tick(&chrono::Utc::now().to_rfc3339(), 8)
        .expect("gc tick");
    assert!(tick.expired_leases >= 1);
}

#[test]
fn gc_tick_keeps_chunks_when_live_manifest_or_lease_exists() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = RegistryStore::open(temp.path()).expect("open store");
    let recorded = store
        .record_manifest_and_epoch(
            "koh0920/sample",
            &manifest("1.0.0"),
            b"payload-v1",
            "2026-03-05T00:00:00Z",
        )
        .expect("record");
    let conn = store.connect().expect("connect");
    let chunk_hash: String = conn
        .query_row(
            "SELECT chunk_hash FROM manifest_chunks WHERE manifest_hash=?1 ORDER BY ordinal ASC LIMIT 1",
            params![recorded.pointer.manifest_hash],
            |row| row.get(0),
        )
        .expect("chunk hash");
    store
        .enqueue_manifest_chunks_for_gc(
            &recorded.pointer.manifest_hash,
            "test-live-ref",
            &chrono::Utc::now().to_rfc3339(),
        )
        .expect("enqueue");

    let tick = store
        .gc_tick(&chrono::Utc::now().to_rfc3339(), 8)
        .expect("gc tick");
    assert!(tick.deferred >= 1);
    assert!(store
        .load_chunk_bytes(&chunk_hash)
        .expect("load chunk")
        .is_some());
}

#[test]
fn gc_tick_keeps_retention_pinned_release_chunks() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = RegistryStore::open(temp.path()).expect("open store");
    let publisher = "koh0920";
    let slug = "sample";
    let name = "sample";
    let description = "sample app";

    let mut releases = Vec::new();
    for idx in 0..6 {
        let version = format!("1.0.{}", idx);
        let capsule = build_capsule_bytes(&manifest(&version));
        let record = store
            .publish_registry_release(
                publisher,
                slug,
                name,
                description,
                &version,
                &format!("sample-{}.capsule", version),
                "sha256:abc",
                &format!("blake3:{:064x}", idx + 1),
                capsule.len() as u64,
                None,
                None,
                None,
                &capsule,
                &format!("2026-03-05T00:00:0{}Z", idx),
            )
            .expect("publish release");
        releases.push((version, record.pointer.manifest_hash));
    }

    let pinned_manifest_hash = releases[1].1.clone();
    store
        .tombstone_manifest("koh0920/sample", &pinned_manifest_hash)
        .expect("tombstone");
    store
        .enqueue_manifest_chunks_for_gc(
            &pinned_manifest_hash,
            "test-retention",
            &chrono::Utc::now().to_rfc3339(),
        )
        .expect("enqueue");

    let conn = store.connect().expect("connect");
    let chunk_hash: String = conn
        .query_row(
            "SELECT chunk_hash FROM manifest_chunks WHERE manifest_hash=?1 ORDER BY ordinal ASC LIMIT 1",
            params![&pinned_manifest_hash],
            |row| row.get(0),
        )
        .expect("chunk hash");

    let tick = store
        .gc_tick(&chrono::Utc::now().to_rfc3339(), 8)
        .expect("gc tick");
    assert!(tick.deferred >= 1);
    assert!(store
        .load_chunk_bytes(&chunk_hash)
        .expect("load chunk")
        .is_some());
}

#[test]
fn publish_registry_release_persists_lock_metadata() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = RegistryStore::open(temp.path()).expect("open store");
    let capsule = build_capsule_bytes(&manifest("1.2.3"));
    let lock_id = "blake3:1111111111111111111111111111111111111111111111111111111111111111";
    let closure_digest = "blake3:2222222222222222222222222222222222222222222222222222222222222222";

    store
        .publish_registry_release(
            "koh0920",
            "sample",
            "sample",
            "sample app",
            "1.2.3",
            "sample-1.2.3.capsule",
            "sha256:abc",
            "blake3:def",
            capsule.len() as u64,
            Some(lock_id),
            Some(closure_digest),
            None,
            &capsule,
            "2026-03-25T00:00:00Z",
        )
        .expect("publish release");

    let release = store
        .find_registry_release("koh0920", "sample", "1.2.3")
        .expect("find release")
        .expect("stored release");
    assert_eq!(release.lock_id.as_deref(), Some(lock_id));
    assert_eq!(release.closure_digest.as_deref(), Some(closure_digest));

    let resolved = store
        .resolve_release_version("koh0920", "sample", "1.2.3")
        .expect("resolve version")
        .expect("resolved release");
    assert_eq!(resolved.lock_id.as_deref(), Some(lock_id));
    assert_eq!(resolved.closure_digest.as_deref(), Some(closure_digest));
}

#[test]
fn publish_registry_release_persists_publish_metadata() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = RegistryStore::open(temp.path()).expect("open store");
    let capsule = build_capsule_bytes(&manifest("2.0.0"));
    let publish_metadata = PublishArtifactMetadata {
        identity_class: PublishArtifactIdentityClass::ImportedThirdPartyArtifact,
        delivery_mode: Some("artifact-import".to_string()),
        provenance_limited: true,
    };

    store
        .publish_registry_release(
            "koh0920",
            "desktop-demo",
            "desktop-demo",
            "desktop demo",
            "2.0.0",
            "desktop-demo-2.0.0.capsule",
            "sha256:abc",
            "blake3:def",
            capsule.len() as u64,
            None,
            None,
            Some(&publish_metadata),
            &capsule,
            "2026-03-28T00:00:00Z",
        )
        .expect("publish release");

    let release = store
        .find_registry_release("koh0920", "desktop-demo", "2.0.0")
        .expect("find release")
        .expect("stored release");
    assert_eq!(release.publish_metadata.as_ref(), Some(&publish_metadata));

    let resolved = store
        .resolve_release_version("koh0920", "desktop-demo", "2.0.0")
        .expect("resolve version")
        .expect("resolved release");
    assert_eq!(resolved.publish_metadata.as_ref(), Some(&publish_metadata));
}

#[test]
fn gc_tick_unlinks_and_reflects_db_for_eligible_chunks() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = RegistryStore::open(temp.path()).expect("open store");
    let recorded = store
        .record_manifest_and_epoch(
            "koh0920/sample",
            &manifest("1.0.0"),
            b"payload-v1",
            "2026-03-05T00:00:00Z",
        )
        .expect("record");
    let conn = store.connect().expect("connect");
    let chunk_hash: String = conn
        .query_row(
            "SELECT chunk_hash FROM manifest_chunks WHERE manifest_hash=?1 ORDER BY ordinal ASC LIMIT 1",
            params![recorded.pointer.manifest_hash],
            |row| row.get(0),
        )
        .expect("chunk hash");
    store
        .tombstone_manifest("koh0920/sample", &recorded.pointer.manifest_hash)
        .expect("tombstone manifest");
    store
        .enqueue_manifest_chunks_for_gc(
            &recorded.pointer.manifest_hash,
            "test-delete",
            &chrono::Utc::now().to_rfc3339(),
        )
        .expect("enqueue");

    let tick = store
        .gc_tick(&chrono::Utc::now().to_rfc3339(), 8)
        .expect("gc tick");
    assert!(tick.deleted >= 1);
    assert!(store
        .load_chunk_bytes(&chunk_hash)
        .expect("load chunk")
        .is_none());

    let remaining_chunks: i64 = conn
        .query_row(
            "SELECT COUNT(1) FROM chunks WHERE chunk_hash=?1",
            params![chunk_hash],
            |row| row.get(0),
        )
        .expect("remaining chunks");
    assert_eq!(remaining_chunks, 0);
}

#[test]
fn gc_related_queries_use_indexes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = RegistryStore::open(temp.path()).expect("open store");
    let conn = store.connect().expect("connect");

    let queue_plan = {
        let mut stmt = conn
            .prepare(
                "EXPLAIN QUERY PLAN
                     SELECT chunk_hash
                     FROM gc_queue
                     WHERE state IN ('pending', 'deferred', 'failed')
                       AND not_before <= ?1
                     ORDER BY not_before ASC
                     LIMIT ?2",
            )
            .expect("prepare queue plan");
        let rows = stmt
            .query_map(params!["2026-03-05T00:00:00Z", 8], |row| {
                row.get::<_, String>(3)
            })
            .expect("query queue plan");
        let mut lines = Vec::new();
        for row in rows {
            lines.push(row.expect("plan row"));
        }
        lines.join("\n")
    };
    assert!(
        queue_plan.contains("idx_gc_queue_state_not_before") || queue_plan.contains("USING INDEX"),
        "unexpected queue plan: {}",
        queue_plan
    );

    let lease_plan = {
        let mut stmt = conn
            .prepare(
                "EXPLAIN QUERY PLAN
                     SELECT 1 FROM leases WHERE chunk_hash=?1 AND expires_at > ?2 LIMIT 1",
            )
            .expect("prepare lease plan");
        let rows = stmt
            .query_map(params!["blake3:deadbeef", "2026-03-05T00:00:00Z"], |row| {
                row.get::<_, String>(3)
            })
            .expect("query lease plan");
        let mut lines = Vec::new();
        for row in rows {
            lines.push(row.expect("plan row"));
        }
        lines.join("\n")
    };
    assert!(
        lease_plan.contains("idx_leases_chunk_expires") || lease_plan.contains("USING INDEX"),
        "unexpected lease plan: {}",
        lease_plan
    );
}

#[test]
fn registry_store_fresh_db_creates_persistent_state_columns_once() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = RegistryStore::open(temp.path()).expect("open store");
    let conn = store.connect().expect("connect");

    let mut stmt = conn
        .prepare("PRAGMA table_info(persistent_states)")
        .expect("prepare table info");
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .expect("query table info")
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("collect columns");

    assert_eq!(
        columns,
        vec![
            "state_id",
            "owner_scope",
            "state_name",
            "kind",
            "backend_kind",
            "backend_locator",
            "producer",
            "purpose",
            "schema_id",
            "created_at",
            "updated_at",
        ]
    );
}

#[test]
fn persistent_state_registry_round_trips() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = RegistryStore::open(temp.path()).expect("open store");
    let record = store
        .register_persistent_state(&NewPersistentStateRecord {
            owner_scope: "demo-app".to_string(),
            state_name: "data".to_string(),
            kind: "filesystem".to_string(),
            backend_kind: "host_path".to_string(),
            backend_locator: "/var/lib/ato/persistent/demo-app/data".to_string(),
            producer: "demo-app".to_string(),
            purpose: "primary-data".to_string(),
            schema_id: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .to_string(),
        })
        .expect("register");
    assert!(record.state_id.starts_with("state-"));

    let fetched = store
        .find_persistent_state_by_owner_and_locator(
            "demo-app",
            "/var/lib/ato/persistent/demo-app/data",
        )
        .expect("lookup")
        .expect("record");
    assert_eq!(fetched, record);

    let by_id = store
        .find_persistent_state_by_id(&record.state_id)
        .expect("lookup by id")
        .expect("record by id");
    assert_eq!(by_id, record);

    let listed = store
        .list_persistent_states(Some("demo-app"), Some("data"))
        .expect("list states");
    assert_eq!(listed, vec![record]);
}

#[test]
fn service_binding_registry_round_trips_and_updates() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = RegistryStore::open(temp.path()).expect("open store");
    let record = store
        .register_service_binding(&NewServiceBindingRecord {
            owner_scope: "demo-app".to_string(),
            service_name: "main".to_string(),
            binding_kind: "ingress".to_string(),
            transport_kind: "https".to_string(),
            adapter_kind: "reverse_proxy".to_string(),
            endpoint_locator: "https://demo.local/".to_string(),
            tls_mode: "explicit".to_string(),
            allowed_callers: vec!["web".to_string(), "worker".to_string()],
            target_hint: Some("app".to_string()),
        })
        .expect("register binding");
    assert!(record.binding_id.starts_with("binding-"));
    assert_eq!(record.allowed_callers, vec!["web", "worker"]);

    let fetched = store
        .find_service_binding_by_identity("demo-app", "main", "ingress")
        .expect("lookup binding")
        .expect("binding record");
    assert_eq!(fetched, record);

    let by_id = store
        .find_service_binding_by_id(&record.binding_id)
        .expect("lookup binding by id")
        .expect("binding record by id");
    assert_eq!(by_id, record);

    let updated = store
        .register_service_binding(&NewServiceBindingRecord {
            owner_scope: "demo-app".to_string(),
            service_name: "main".to_string(),
            binding_kind: "ingress".to_string(),
            transport_kind: "http".to_string(),
            adapter_kind: "reverse_proxy".to_string(),
            endpoint_locator: "http://127.0.0.1:4310/".to_string(),
            tls_mode: "disabled".to_string(),
            allowed_callers: vec!["worker".to_string()],
            target_hint: Some("app".to_string()),
        })
        .expect("update binding");
    assert_eq!(updated.binding_id, record.binding_id);
    assert_eq!(updated.endpoint_locator, "http://127.0.0.1:4310/");
    assert_eq!(updated.tls_mode, "disabled");
    assert_eq!(updated.allowed_callers, vec!["worker"]);

    let listed = store
        .list_service_bindings(Some("demo-app"), Some("main"))
        .expect("list bindings");
    assert_eq!(listed, vec![updated]);
}

#[test]
fn service_binding_resolution_enforces_allowed_callers() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = RegistryStore::open(temp.path()).expect("open store");
    let record = store
        .register_service_binding(&NewServiceBindingRecord {
            owner_scope: "demo-app".to_string(),
            service_name: "api".to_string(),
            binding_kind: "service".to_string(),
            transport_kind: "http".to_string(),
            adapter_kind: "reverse_proxy".to_string(),
            endpoint_locator: "http://127.0.0.1:4310/".to_string(),
            tls_mode: "disabled".to_string(),
            allowed_callers: vec!["web".to_string()],
            target_hint: Some("app".to_string()),
        })
        .expect("register binding");

    let resolved = store
        .resolve_service_binding("demo-app", "api", "service", Some("web"))
        .expect("resolve binding")
        .expect("resolved record");
    assert_eq!(resolved, record);

    let missing_caller = store
        .resolve_service_binding("demo-app", "api", "service", None)
        .expect_err("caller is required for restricted bindings");
    assert!(missing_caller
        .to_string()
        .contains("requires caller_service"));

    let denied = store
        .resolve_service_binding("demo-app", "api", "service", Some("worker"))
        .expect_err("unauthorized caller must fail");
    assert!(denied.to_string().contains("not allowed"));
}

#[test]
fn delete_service_binding_by_identity_removes_record() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = RegistryStore::open(temp.path()).expect("open store");
    let record = store
        .register_service_binding(&NewServiceBindingRecord {
            owner_scope: "demo-app".to_string(),
            service_name: "api".to_string(),
            binding_kind: "service".to_string(),
            transport_kind: "http".to_string(),
            adapter_kind: "local_service".to_string(),
            endpoint_locator: "http://127.0.0.1:4310/".to_string(),
            tls_mode: "disabled".to_string(),
            allowed_callers: vec!["web".to_string()],
            target_hint: Some("app".to_string()),
        })
        .expect("register binding");

    let deleted = store
        .delete_service_binding_by_identity("demo-app", "api", "service")
        .expect("delete binding")
        .expect("deleted record");
    assert_eq!(deleted, record);

    let remaining = store
        .find_service_binding_by_identity("demo-app", "api", "service")
        .expect("lookup binding after delete");
    assert!(remaining.is_none());
}

#[test]
fn revoke_key_requires_did_when_key_id_collides() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = RegistryStore::open(temp.path()).expect("open store");
    let identity = store.ensure_signing_identity().expect("identity");
    let conn = store.connect().expect("connect");
    conn.execute(
        "INSERT OR REPLACE INTO trusted_keys(did, key_id, public_key, valid_from, valid_to, revoked_at)
             VALUES (?1, ?2, ?3, ?4, NULL, NULL)",
        params![
            "did:key:zcollision",
            identity.key_id,
            BASE64.encode([7u8; 32]),
            chrono::Utc::now().to_rfc3339()
        ],
    )
    .expect("insert collision key");

    let err = store
        .revoke_key(&identity.key_id, None)
        .expect_err("collision must require did");
    assert!(err.to_string().contains("specify --did"));
    assert!(err.to_string().contains("did:key:zcollision"));
}

#[test]
fn negotiate_reuses_lease_id_on_retry() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = RegistryStore::open(temp.path()).expect("open store");
    let recorded = store
        .record_manifest_and_epoch(
            "koh0920/sample",
            &manifest("1.0.0"),
            b"payload-v1",
            "2026-03-05T00:00:00Z",
        )
        .expect("record");

    let first = store
        .negotiate(&NegotiateRequest {
            scoped_id: "koh0920/sample".to_string(),
            target_manifest_hash: recorded.pointer.manifest_hash.clone(),
            have_chunks: vec![],
            have_chunks_bloom: Some(ChunkBloomFilterRequest {
                m_bits: 8,
                k_hashes: 1,
                seed: 7,
                bitset_base64: BASE64.encode([0xffu8]),
            }),
            reuse_lease_id: None,
            max_bytes: None,
        })
        .expect("first negotiate");
    assert!(first.required_chunks.is_empty());
    let lease_id = first.lease_id.clone().expect("lease_id");

    let second = store
        .negotiate(&NegotiateRequest {
            scoped_id: "koh0920/sample".to_string(),
            target_manifest_hash: recorded.pointer.manifest_hash.clone(),
            have_chunks: vec![],
            have_chunks_bloom: None,
            reuse_lease_id: Some(lease_id.clone()),
            max_bytes: None,
        })
        .expect("second negotiate");
    assert_eq!(second.lease_id.as_deref(), Some(lease_id.as_str()));
    assert!(!second.required_chunks.is_empty());
}
