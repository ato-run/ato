use super::*;

impl RegistryStore {
    pub fn refresh_lease(&self, lease_id: &str, ttl_secs: u64) -> Result<LeaseRefreshResult> {
        let lease_id = lease_id.trim();
        if lease_id.is_empty() {
            anyhow::bail!("lease_id is required");
        }
        let now = chrono::Utc::now();
        let expires_at = now
            .checked_add_signed(chrono::Duration::seconds(ttl_secs.max(1) as i64))
            .unwrap_or(now)
            .to_rfc3339();

        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        let chunk_count: usize = tx.query_row(
            "SELECT COUNT(1) FROM leases WHERE lease_id=?1",
            params![lease_id],
            |row| row.get::<_, i64>(0).map(|v| v as usize),
        )?;
        if chunk_count == 0 {
            anyhow::bail!("lease not found: {}", lease_id);
        }
        tx.execute(
            "UPDATE leases SET expires_at=?2 WHERE lease_id=?1",
            params![lease_id, expires_at],
        )?;
        tx.commit()?;

        Ok(LeaseRefreshResult {
            lease_id: lease_id.to_string(),
            expires_at,
            chunk_count,
        })
    }

    pub fn release_lease(&self, lease_id: &str) -> Result<usize> {
        let lease_id = lease_id.trim();
        if lease_id.is_empty() {
            anyhow::bail!("lease_id is required");
        }
        let conn = self.connect()?;
        let removed = conn.execute("DELETE FROM leases WHERE lease_id=?1", params![lease_id])?;
        Ok(removed)
    }

    pub fn cleanup_expired_leases(&self, now: &str) -> Result<usize> {
        let conn = self.connect()?;
        let removed = conn.execute("DELETE FROM leases WHERE expires_at <= ?1", params![now])?;
        Ok(removed)
    }

    #[cfg(test)]
    pub fn tombstone_manifest(&self, scoped_id: &str, manifest_hash: &str) -> Result<bool> {
        let manifest_hash = normalize_manifest_hash(manifest_hash);
        let now = chrono::Utc::now().to_rfc3339();
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        let scoped_known: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM epochs WHERE scoped_id=?1 AND manifest_hash=?2 LIMIT 1",
                params![scoped_id, manifest_hash],
                |row| row.get(0),
            )
            .optional()?;
        if scoped_known.is_none() {
            tx.rollback()?;
            return Ok(false);
        }

        let changed = tx.execute(
            "UPDATE manifests
             SET tombstoned_at=COALESCE(tombstoned_at, ?2)
             WHERE manifest_hash=?1",
            params![manifest_hash, now],
        )?;
        tx.execute(
            "UPDATE chunks
             SET tombstoned_at=COALESCE(tombstoned_at, ?2)
             WHERE chunk_hash IN (
               SELECT chunk_hash FROM manifest_chunks WHERE manifest_hash=?1
             )",
            params![manifest_hash, now],
        )?;
        tx.commit()?;
        Ok(changed > 0)
    }

    #[cfg(test)]
    pub fn enqueue_manifest_chunks_for_gc(
        &self,
        manifest_hash: &str,
        reason: &str,
        not_before: &str,
    ) -> Result<usize> {
        let manifest_hash = normalize_manifest_hash(manifest_hash);
        let reason = if reason.trim().is_empty() {
            "unspecified"
        } else {
            reason.trim()
        };
        let now = chrono::Utc::now().to_rfc3339();
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        let chunk_hashes: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT chunk_hash FROM manifest_chunks
                 WHERE manifest_hash=?1 ORDER BY ordinal ASC",
            )?;
            let rows = stmt.query_map(params![manifest_hash], |row| row.get::<_, String>(0))?;
            let mut values = Vec::new();
            for row in rows {
                values.push(row?);
            }
            values
        };

        for chunk_hash in &chunk_hashes {
            tx.execute(
                "UPDATE chunks
                 SET tombstoned_at=COALESCE(tombstoned_at, ?2)
                 WHERE chunk_hash=?1",
                params![chunk_hash, now],
            )?;
            tx.execute(
                "INSERT INTO gc_queue(chunk_hash, not_before, reason, state, attempts, updated_at)
                 VALUES (?1, ?2, ?3, 'pending', 0, ?4)
                 ON CONFLICT(chunk_hash) DO UPDATE SET
                   not_before=excluded.not_before,
                   reason=excluded.reason,
                   state='pending',
                   updated_at=excluded.updated_at",
                params![chunk_hash, not_before, reason, now],
            )?;
        }
        tx.commit()?;
        Ok(chunk_hashes.len())
    }

    pub fn gc_tick(&self, now: &str, max_chunks: usize) -> Result<GcTickResult> {
        let mut result = GcTickResult {
            expired_leases: self.cleanup_expired_leases(now)?,
            ..GcTickResult::default()
        };

        let mut conn = self.connect()?;
        let queue: Vec<String> = {
            let mut stmt = conn.prepare(
                "SELECT chunk_hash
                 FROM gc_queue
                 WHERE state IN ('pending', 'deferred', 'failed')
                   AND not_before <= ?1
                 ORDER BY not_before ASC
                 LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![now, max_chunks as i64], |row| row.get(0))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            out
        };

        for chunk_hash in queue {
            result.processed += 1;
            let tombstoned: Option<Option<String>> = conn
                .query_row(
                    "SELECT tombstoned_at FROM chunks WHERE chunk_hash=?1",
                    params![chunk_hash],
                    |row| row.get(0),
                )
                .optional()?;

            if tombstoned.is_none() {
                conn.execute(
                    "UPDATE gc_queue SET state='deleted', updated_at=?2 WHERE chunk_hash=?1",
                    params![chunk_hash, now],
                )?;
                result.deleted += 1;
                continue;
            }

            let has_tombstone = tombstoned.flatten().is_some();
            let active_lease: Option<i64> = conn
                .query_row(
                    "SELECT 1 FROM leases WHERE chunk_hash=?1 AND expires_at > ?2 LIMIT 1",
                    params![chunk_hash, now],
                    |row| row.get(0),
                )
                .optional()?;
            let live_reference: Option<i64> = conn
                .query_row(
                    "SELECT 1
                     FROM manifest_chunks mc
                     JOIN manifests m ON m.manifest_hash = mc.manifest_hash
                     WHERE mc.chunk_hash=?1
                       AND m.tombstoned_at IS NULL
                     LIMIT 1",
                    params![chunk_hash],
                    |row| row.get(0),
                )
                .optional()?;
            let retention_pinned: Option<i64> = conn
                .query_row(
                    "SELECT 1
                     FROM manifest_chunks mc
                     JOIN (
                       SELECT scoped_id, manifest_hash
                       FROM (
                         SELECT scoped_id,
                                manifest_hash,
                                ROW_NUMBER() OVER (
                                  PARTITION BY scoped_id
                                  ORDER BY created_at DESC, version DESC
                                ) AS release_rank
                         FROM registry_releases
                       )
                       WHERE release_rank <= ?2
                     ) pinned ON pinned.manifest_hash = mc.manifest_hash
                     WHERE mc.chunk_hash=?1
                     LIMIT 1",
                    params![chunk_hash, RETENTION_PINNED_RELEASES],
                    |row| row.get(0),
                )
                .optional()?;

            if !has_tombstone
                || active_lease.is_some()
                || live_reference.is_some()
                || retention_pinned.is_some()
            {
                let defer_until = chrono::Utc::now()
                    .checked_add_signed(chrono::Duration::seconds(DEFAULT_GC_DEFER_SECS))
                    .unwrap_or_else(chrono::Utc::now)
                    .to_rfc3339();
                conn.execute(
                    "UPDATE gc_queue
                     SET state='deferred',
                         not_before=?2,
                         attempts=attempts + 1,
                         updated_at=?3
                     WHERE chunk_hash=?1",
                    params![chunk_hash, defer_until, now],
                )?;
                result.deferred += 1;
                continue;
            }

            let normalized = normalize_blake3_hash(&chunk_hash);
            let chunk_path = self.chunk_path(&normalized);
            if chunk_path.exists() {
                if let Err(err) = std::fs::remove_file(&chunk_path) {
                    conn.execute(
                        "UPDATE gc_queue
                         SET state='failed', attempts=attempts + 1, updated_at=?2, reason=?3
                         WHERE chunk_hash=?1",
                        params![chunk_hash, now, format!("unlink_failed:{err}")],
                    )?;
                    result.failed += 1;
                    continue;
                }
            }

            let tx = conn.transaction()?;
            let apply = (|| -> Result<()> {
                tx.execute(
                    "DELETE FROM leases WHERE chunk_hash=?1",
                    params![chunk_hash.clone()],
                )?;
                tx.execute(
                    "DELETE FROM manifest_chunks WHERE chunk_hash=?1",
                    params![chunk_hash.clone()],
                )?;
                tx.execute(
                    "DELETE FROM chunks WHERE chunk_hash=?1",
                    params![chunk_hash.clone()],
                )?;
                tx.execute(
                    "UPDATE gc_queue
                     SET state='deleted', attempts=attempts + 1, updated_at=?2
                     WHERE chunk_hash=?1",
                    params![chunk_hash, now],
                )?;
                Ok(())
            })();

            if let Err(err) = apply {
                tx.rollback()?;
                conn.execute(
                    "UPDATE gc_queue
                     SET state='failed', attempts=attempts + 1, updated_at=?2, reason=?3
                     WHERE chunk_hash=?1",
                    params![chunk_hash, now, format!("db_reflect_failed:{err}")],
                )?;
                result.failed += 1;
                continue;
            }

            tx.commit()?;
            result.deleted += 1;
        }

        Ok(result)
    }

    pub fn checkpoint_wal_truncate(&self) -> Result<()> {
        let conn = self.connect()?;
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    }

    pub fn incremental_vacuum(&self, pages: usize) -> Result<()> {
        let conn = self.connect()?;
        conn.execute_batch(&format!("PRAGMA incremental_vacuum({});", pages.max(1)))?;
        Ok(())
    }

    pub fn rotate_signing_key(&self, overlap_hours: i64) -> Result<KeyRotateResponse> {
        let now = chrono::Utc::now();
        let valid_from = now.to_rfc3339();
        let overlap_hours = overlap_hours.max(0);
        let previous_valid_to = now
            .checked_add_signed(chrono::Duration::hours(overlap_hours))
            .unwrap_or(now)
            .to_rfc3339();

        let (previous_key_id, previous_signing_key) = self.load_or_create_signing_key()?;
        let previous_public = previous_signing_key.verifying_key().to_bytes();
        let previous_did = public_key_to_did(&previous_public);
        let previous_public_b64 = BASE64.encode(previous_public);

        let (key_id, signing_key) = self.create_and_activate_signing_key()?;
        let public_key = signing_key.verifying_key().to_bytes();
        let signer_did = public_key_to_did(&public_key);
        let public_key_b64 = BASE64.encode(public_key);

        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO trusted_keys(did, key_id, public_key, valid_from, valid_to, revoked_at)
             VALUES (?1, ?2, ?3, ?4, NULL, NULL)",
            params![
                previous_did,
                previous_key_id,
                previous_public_b64,
                valid_from.clone()
            ],
        )?;
        tx.execute(
            "INSERT INTO trusted_keys(did, key_id, public_key, valid_from, valid_to, revoked_at)
             VALUES (?1, ?2, ?3, ?4, NULL, NULL)
             ON CONFLICT(did, key_id) DO UPDATE SET
               public_key=excluded.public_key,
               valid_from=excluded.valid_from,
               valid_to=NULL,
               revoked_at=NULL",
            params![signer_did, key_id, public_key_b64, valid_from],
        )?;

        if previous_key_id != key_id {
            tx.execute(
                "UPDATE trusted_keys
                 SET valid_to=CASE
                   WHEN valid_to IS NULL OR valid_to > ?3 THEN ?3
                   ELSE valid_to
                 END
                 WHERE did=?1 AND key_id=?2",
                params![previous_did, previous_key_id, previous_valid_to],
            )?;
        }
        tx.commit()?;

        Ok(KeyRotateResponse {
            signer_did,
            key_id: key_id.clone(),
            public_key: public_key_b64,
            valid_from: now.to_rfc3339(),
            previous_key_id: if previous_key_id == key_id {
                None
            } else {
                Some(previous_key_id)
            },
            previous_valid_to: if overlap_hours > 0 {
                Some(previous_valid_to)
            } else {
                Some(now.to_rfc3339())
            },
        })
    }

    pub fn revoke_key(&self, key_id: &str, did: Option<&str>) -> Result<KeyRevokeResponse> {
        let key_id = key_id.trim();
        if key_id.is_empty() {
            anyhow::bail!("key_id is required");
        }
        let did = did.map(str::trim).filter(|value| !value.is_empty());
        let mut conn = self.connect()?;
        let dids = {
            let mut stmt = conn.prepare(
                "SELECT did FROM trusted_keys
                 WHERE key_id=?1
                 ORDER BY did ASC",
            )?;
            let rows = stmt.query_map(params![key_id], |row| row.get::<_, String>(0))?;
            let mut values = Vec::new();
            for row in rows {
                values.push(row?);
            }
            values
        };
        if dids.is_empty() {
            anyhow::bail!("key_id not found: {}", key_id);
        }
        let target_dids: Vec<String> = if let Some(requested) = did {
            if !dids.iter().any(|candidate| candidate == requested) {
                anyhow::bail!("key_id {} not found for did {}", key_id, requested);
            }
            vec![requested.to_string()]
        } else if dids.len() > 1 {
            let candidates = dids.join(", ");
            anyhow::bail!(
                "key_id {} is shared by multiple did values; specify --did (candidates: {})",
                key_id,
                candidates
            );
        } else {
            vec![dids[0].clone()]
        };

        let now = chrono::Utc::now().to_rfc3339();
        let tx = conn.transaction()?;
        let mut revoked = 0usize;
        for target_did in &target_dids {
            let updated = tx.execute(
                "UPDATE trusted_keys
                 SET revoked_at=COALESCE(revoked_at, ?3),
                     valid_to=COALESCE(valid_to, ?3)
                 WHERE did=?1 AND key_id=?2",
                params![target_did, key_id, now],
            )?;
            revoked += updated;
        }
        tx.commit()?;

        let mut active_key_rotated_to = None;
        if let Some(active_key_id) = self.active_key_id()? {
            if active_key_id == key_id {
                let active_signing_key = self.load_signing_key_by_id(&active_key_id)?;
                let active_did = public_key_to_did(&active_signing_key.verifying_key().to_bytes());
                if target_dids.iter().any(|target| target == &active_did) {
                    let rotated = self.rotate_signing_key(0)?;
                    active_key_rotated_to = Some(rotated.key_id);
                }
            }
        }

        Ok(KeyRevokeResponse {
            revoked,
            active_key_rotated_to,
        })
    }

    pub fn load_manifest_document(&self, manifest_hash: &str) -> Result<Option<Vec<u8>>> {
        let manifest_hash = normalize_manifest_hash(manifest_hash);
        let conn = self.connect()?;
        let row: Option<(Vec<u8>, Option<String>)> = conn
            .query_row(
                "SELECT manifest_toml, yanked_at FROM manifests WHERE manifest_hash=?1",
                params![manifest_hash],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((canonical, yanked_at)) = row else {
            return Ok(None);
        };
        if let Some(yanked_at) = yanked_at {
            anyhow::bail!(
                "manifest yanked: manifest_hash={} yanked_at={}",
                manifest_hash,
                yanked_at
            );
        }
        let expected = normalize_manifest_hash(&manifest_hash);
        let actual = match std::str::from_utf8(&canonical) {
            Ok(manifest_toml) => match toml::from_str::<CapsuleManifest>(manifest_toml) {
                Ok(manifest) => compute_manifest_hash_without_signatures(&manifest)?,
                Err(_) => format!("blake3:{}", blake3::hash(&canonical).to_hex()),
            },
            Err(_) => format!("blake3:{}", blake3::hash(&canonical).to_hex()),
        };
        if normalize_manifest_hash(&actual) != expected {
            anyhow::bail!(
                "manifest hash mismatch in storage (expected blake3:{}, got blake3:{})",
                normalize_blake3_hash(&expected),
                normalize_blake3_hash(&actual)
            );
        }
        Ok(Some(canonical))
    }

    pub fn load_chunk_bytes(&self, chunk_hash: &str) -> Result<Option<Vec<u8>>> {
        let normalized = normalize_blake3_hash(chunk_hash);
        let path = self.chunk_path(&normalized);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(path)?;
        let actual = blake3::hash(&bytes).to_hex().to_string();
        if actual != normalized {
            anyhow::bail!(
                "chunk hash mismatch in storage (expected blake3:{}, got blake3:{})",
                normalized,
                actual
            );
        }
        Ok(Some(bytes))
    }
}
