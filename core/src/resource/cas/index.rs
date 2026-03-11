use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result as AnyResult};
use chrono::Utc;
use rand::RngCore;
use rusqlite::{params, Connection, OptionalExtension};

use crate::error::{CapsuleError, Result};

use super::bloom::{AtoBloomFilter, DEFAULT_BLOOM_FALSE_POSITIVE_RATE, DEFAULT_BLOOM_SEED};

const CHUNKS_DIR: &str = "chunks";
const DB_FILE_NAME: &str = "index.sqlite3";
const MIGRATION_0001: &str = "2026-03-06-0001-local-chunks";

#[derive(Debug, Clone)]
pub struct LocalCasIndex {
    root_dir: PathBuf,
    db_path: PathBuf,
}

impl LocalCasIndex {
    pub fn open_default() -> Result<Self> {
        let base = std::env::var("ATO_CAS_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                crate::common::paths::nacelle_home_dir()
                    .unwrap_or_else(|_| PathBuf::from("/tmp").join(".ato"))
                    .join("cas")
            });
        Self::open(base)
    }

    pub fn open(root_dir: impl AsRef<Path>) -> Result<Self> {
        let root_dir = root_dir.as_ref().to_path_buf();
        let chunks_dir = root_dir.join(CHUNKS_DIR);
        let db_path = root_dir.join(DB_FILE_NAME);
        (|| -> AnyResult<Self> {
            std::fs::create_dir_all(&chunks_dir)
                .with_context(|| format!("failed to create {}", chunks_dir.display()))?;
            let this = Self { root_dir, db_path };
            this.init_schema()?;
            Ok(this)
        })()
        .map_err(Into::into)
    }

    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    pub fn put_verified_chunk(&self, chunk_hash: &str, chunk_bytes: &[u8]) -> Result<PathBuf> {
        (|| -> AnyResult<PathBuf> {
            let normalized = normalize_blake3_hash(chunk_hash)?;
            let actual = blake3::hash(chunk_bytes).to_hex().to_string();
            if actual != normalized {
                return Err(anyhow::anyhow!(CapsuleError::HashMismatch(
                    format!("blake3:{normalized}"),
                    format!("blake3:{actual}"),
                )));
            }

            let rel_path = format!("{}/{}/{}", CHUNKS_DIR, &normalized[..2], normalized);
            let full_path = self.root_dir.join(&rel_path);
            atomic_write_file(&full_path, chunk_bytes)
                .with_context(|| format!("failed to write {}", full_path.display()))?;

            let now = Utc::now().to_rfc3339();
            let conn = self.connect()?;
            conn.execute(
                "INSERT INTO local_chunks(chunk_hash, rel_path, size_bytes, verified_at, created_at, last_seen_at)
                 VALUES (?1, ?2, ?3, ?4, ?4, ?4)
                 ON CONFLICT(chunk_hash) DO UPDATE SET
                   rel_path=excluded.rel_path,
                   size_bytes=excluded.size_bytes,
                   verified_at=excluded.verified_at,
                   last_seen_at=excluded.last_seen_at",
                params![
                    format!("blake3:{normalized}"),
                    rel_path,
                    chunk_bytes.len() as i64,
                    now
                ],
            )?;

            Ok(full_path)
        })()
        .map_err(Into::into)
    }

    pub fn load_chunk_bytes(&self, chunk_hash: &str) -> Result<Option<Vec<u8>>> {
        (|| -> AnyResult<Option<Vec<u8>>> {
            let canonical = canonical_hash(chunk_hash)?;
            let conn = self.connect()?;
            let rel_path: Option<String> = conn
                .query_row(
                    "SELECT rel_path FROM local_chunks WHERE chunk_hash=?1",
                    params![canonical],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(rel_path) = rel_path else {
                return Ok(None);
            };
            let full_path = self.root_dir.join(&rel_path);
            if !full_path.exists() {
                conn.execute(
                    "DELETE FROM local_chunks WHERE chunk_hash=?1",
                    params![canonical],
                )?;
                return Ok(None);
            }

            let bytes = std::fs::read(&full_path)
                .with_context(|| format!("failed to read {}", full_path.display()))?;
            let normalized = normalize_blake3_hash(&canonical)?;
            let actual = blake3::hash(&bytes).to_hex().to_string();
            if actual != normalized {
                return Err(anyhow::anyhow!(CapsuleError::HashMismatch(
                    format!("blake3:{normalized}"),
                    format!("blake3:{actual}"),
                )));
            }

            conn.execute(
                "UPDATE local_chunks SET last_seen_at=?2 WHERE chunk_hash=?1",
                params![canonical, Utc::now().to_rfc3339()],
            )?;
            Ok(Some(bytes))
        })()
        .map_err(Into::into)
    }

    pub fn build_bloom(&self, fp_rate: Option<f64>) -> Result<AtoBloomFilter> {
        (|| -> AnyResult<AtoBloomFilter> {
            let conn = self.connect()?;
            let mut stmt = conn.prepare("SELECT chunk_hash, rel_path FROM local_chunks")?;
            let mut rows = stmt.query([])?;
            let mut hashes = Vec::new();
            let mut stale = Vec::new();

            while let Some(row) = rows.next()? {
                let chunk_hash: String = row.get(0)?;
                let rel_path: String = row.get(1)?;
                if self.root_dir.join(rel_path).exists() {
                    hashes.push(chunk_hash);
                } else {
                    stale.push(chunk_hash);
                }
            }
            drop(rows);
            drop(stmt);

            if !stale.is_empty() {
                let tx = conn.unchecked_transaction()?;
                for hash in stale {
                    tx.execute(
                        "DELETE FROM local_chunks WHERE chunk_hash=?1",
                        params![hash],
                    )?;
                }
                tx.commit()?;
            }

            Ok(AtoBloomFilter::from_hashes_with_params(
                hashes,
                fp_rate.unwrap_or(DEFAULT_BLOOM_FALSE_POSITIVE_RATE),
                DEFAULT_BLOOM_SEED,
            ))
        })()
        .map_err(Into::into)
    }

    pub fn available_hashes_for_manifest(&self, chunk_hashes: &[String]) -> Result<Vec<String>> {
        (|| -> AnyResult<Vec<String>> {
            if chunk_hashes.is_empty() {
                return Ok(Vec::new());
            }
            let conn = self.connect()?;
            let mut stmt = conn.prepare("SELECT rel_path FROM local_chunks WHERE chunk_hash=?1")?;
            let mut available = Vec::new();
            let mut stale = Vec::new();

            for chunk_hash in chunk_hashes {
                let canonical = canonical_hash(chunk_hash)?;
                let rel_path: Option<String> = stmt
                    .query_row(params![canonical.as_str()], |row| row.get(0))
                    .optional()?;
                if let Some(rel_path) = rel_path {
                    if self.root_dir.join(rel_path).exists() {
                        available.push(canonical);
                    } else {
                        stale.push(canonical);
                    }
                }
            }
            drop(stmt);

            if !stale.is_empty() {
                let tx = conn.unchecked_transaction()?;
                for hash in stale {
                    tx.execute(
                        "DELETE FROM local_chunks WHERE chunk_hash=?1",
                        params![hash],
                    )?;
                }
                tx.commit()?;
            }

            Ok(available)
        })()
        .map_err(Into::into)
    }

    pub fn chunk_count(&self) -> Result<usize> {
        (|| -> AnyResult<usize> {
            let conn = self.connect()?;
            let count: i64 =
                conn.query_row("SELECT COUNT(1) FROM local_chunks", [], |row| row.get(0))?;
            Ok(count.max(0) as usize)
        })()
        .map_err(Into::into)
    }

    fn connect(&self) -> AnyResult<Connection> {
        let conn = Connection::open(&self.db_path)
            .with_context(|| format!("failed to open {}", self.db_path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(conn)
    }

    fn init_schema(&self) -> AnyResult<()> {
        let conn = self.connect()?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS schema_migrations(
              id TEXT PRIMARY KEY,
              applied_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS local_chunks(
              chunk_hash TEXT PRIMARY KEY,
              rel_path TEXT NOT NULL,
              size_bytes INTEGER NOT NULL,
              verified_at TEXT NOT NULL,
              created_at TEXT NOT NULL,
              last_seen_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_local_chunks_last_seen ON local_chunks(last_seen_at);
            ",
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO schema_migrations(id, applied_at) VALUES (?1, ?2)",
            params![MIGRATION_0001, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }
}

fn normalize_blake3_hash(value: &str) -> AnyResult<String> {
    let normalized = value
        .trim()
        .trim_start_matches("blake3:")
        .to_ascii_lowercase();
    if normalized.len() != 64 || !normalized.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        anyhow::bail!("invalid blake3 hash: {}", value);
    }
    Ok(normalized)
}

fn canonical_hash(value: &str) -> AnyResult<String> {
    Ok(format!("blake3:{}", normalize_blake3_hash(value)?))
}

fn atomic_write_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut nonce = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut nonce);
    let tmp_name = format!(
        ".{}.tmp-{}",
        path.file_name().and_then(|v| v.to_str()).unwrap_or("chunk"),
        hex::encode(nonce)
    );
    let tmp_path = path.with_file_name(tmp_name);
    {
        let mut file = File::create(&tmp_path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp_path, path)?;
    if let Some(parent) = path.parent() {
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::LocalCasIndex;

    fn chunk_hash(bytes: &[u8]) -> String {
        format!("blake3:{}", blake3::hash(bytes).to_hex())
    }

    #[test]
    fn put_verified_chunk_rejects_hash_mismatch() {
        let temp = tempfile::tempdir().expect("tempdir");
        let index = LocalCasIndex::open(temp.path()).expect("open index");
        let err = index
            .put_verified_chunk(
                "blake3:0000000000000000000000000000000000000000000000000000000000000000",
                b"payload",
            )
            .expect_err("must fail");
        assert!(err.to_string().contains("Hash mismatch"));
    }

    #[test]
    fn build_bloom_cleans_stale_rows() {
        let temp = tempfile::tempdir().expect("tempdir");
        let index = LocalCasIndex::open(temp.path()).expect("open index");

        let bytes = b"payload-a";
        let hash = chunk_hash(bytes);
        let path = index
            .put_verified_chunk(&hash, bytes)
            .expect("store chunk path");
        std::fs::remove_file(path).expect("remove chunk file");

        let bloom = index.build_bloom(None).expect("build bloom");
        assert_eq!(index.chunk_count().expect("count"), 0);
        assert!(!bloom.might_contain(&hash));
    }

    #[test]
    fn available_hashes_for_manifest_intersects_existing_chunks() {
        let temp = tempfile::tempdir().expect("tempdir");
        let index = LocalCasIndex::open(temp.path()).expect("open index");

        let hash_a = chunk_hash(b"a");
        let hash_b = chunk_hash(b"b");
        let hash_c = chunk_hash(b"c");

        index
            .put_verified_chunk(&hash_a, b"a")
            .expect("store a chunk");
        index
            .put_verified_chunk(&hash_c, b"c")
            .expect("store c chunk");

        let available = index
            .available_hashes_for_manifest(&[hash_a.clone(), hash_b.clone(), hash_c.clone()])
            .expect("available");

        assert_eq!(available, vec![hash_a, hash_c]);
    }
}
