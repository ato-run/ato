use super::*;

fn generate_random_state_id() -> String {
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    format!("state-{}", hex::encode(bytes))
}

fn generate_random_binding_id() -> String {
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    format!("binding-{}", hex::encode(bytes))
}

fn normalize_allowed_callers(allowed_callers: &[String]) -> Vec<String> {
    let mut normalized = Vec::new();
    for value in allowed_callers {
        let trimmed = value.trim();
        if trimmed.is_empty() || normalized.iter().any(|existing| existing == trimmed) {
            continue;
        }
        normalized.push(trimmed.to_string());
    }
    normalized
}

fn encode_allowed_callers(allowed_callers: &[String]) -> Result<String> {
    Ok(serde_json::to_string(&normalize_allowed_callers(
        allowed_callers,
    ))?)
}

fn decode_allowed_callers(raw: Option<String>) -> rusqlite::Result<Vec<String>> {
    let Some(raw) = raw
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(Vec::new());
    };

    serde_json::from_str::<Vec<String>>(raw)
        .map(|values| normalize_allowed_callers(&values))
        .map_err(|err| rusqlite::Error::FromSqlConversionFailure(8, Type::Text, Box::new(err)))
}

impl RegistryStore {
    pub fn find_persistent_state_by_owner_and_locator(
        &self,
        owner_scope: &str,
        backend_locator: &str,
    ) -> Result<Option<PersistentStateRecord>> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT state_id, owner_scope, state_name, kind, backend_kind, backend_locator, producer, purpose, schema_id, created_at, updated_at
             FROM persistent_states
             WHERE owner_scope=?1 AND backend_locator=?2",
            params![owner_scope, backend_locator],
            Self::map_persistent_state_row,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn find_persistent_state_by_id(
        &self,
        state_id: &str,
    ) -> Result<Option<PersistentStateRecord>> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT state_id, owner_scope, state_name, kind, backend_kind, backend_locator, producer, purpose, schema_id, created_at, updated_at
             FROM persistent_states
             WHERE state_id=?1",
            params![state_id],
            Self::map_persistent_state_row,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn list_persistent_states(
        &self,
        owner_scope: Option<&str>,
        state_name: Option<&str>,
    ) -> Result<Vec<PersistentStateRecord>> {
        let conn = self.connect()?;
        let sql = match (owner_scope, state_name) {
            (Some(_), Some(_)) => {
                "SELECT state_id, owner_scope, state_name, kind, backend_kind, backend_locator, producer, purpose, schema_id, created_at, updated_at
                 FROM persistent_states
                 WHERE owner_scope=?1 AND state_name=?2
                 ORDER BY updated_at DESC, state_id ASC"
            }
            (Some(_), None) => {
                "SELECT state_id, owner_scope, state_name, kind, backend_kind, backend_locator, producer, purpose, schema_id, created_at, updated_at
                 FROM persistent_states
                 WHERE owner_scope=?1
                 ORDER BY updated_at DESC, state_id ASC"
            }
            (None, Some(_)) => {
                "SELECT state_id, owner_scope, state_name, kind, backend_kind, backend_locator, producer, purpose, schema_id, created_at, updated_at
                 FROM persistent_states
                 WHERE state_name=?1
                 ORDER BY updated_at DESC, state_id ASC"
            }
            (None, None) => {
                "SELECT state_id, owner_scope, state_name, kind, backend_kind, backend_locator, producer, purpose, schema_id, created_at, updated_at
                 FROM persistent_states
                 ORDER BY updated_at DESC, state_id ASC"
            }
        };

        let mut stmt = conn.prepare(sql)?;
        let rows = match (owner_scope, state_name) {
            (Some(owner_scope), Some(state_name)) => stmt.query_map(
                params![owner_scope, state_name],
                Self::map_persistent_state_row,
            )?,
            (Some(owner_scope), None) => {
                stmt.query_map(params![owner_scope], Self::map_persistent_state_row)?
            }
            (None, Some(state_name)) => {
                stmt.query_map(params![state_name], Self::map_persistent_state_row)?
            }
            (None, None) => stmt.query_map([], Self::map_persistent_state_row)?,
        };

        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    pub fn register_persistent_state(
        &self,
        record: &NewPersistentStateRecord,
    ) -> Result<PersistentStateRecord> {
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.connect()?;
        for _ in 0..MAX_STATE_ID_GENERATION_ATTEMPTS {
            let state_id = generate_random_state_id();
            match conn.execute(
                "INSERT INTO persistent_states(
                    state_id, owner_scope, state_name, kind, backend_kind, backend_locator, producer, purpose, schema_id, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
                params![
                    state_id,
                    record.owner_scope,
                    record.state_name,
                    record.kind,
                    record.backend_kind,
                    record.backend_locator,
                    record.producer,
                    record.purpose,
                    record.schema_id,
                    now,
                ],
            ) {
                Ok(_) => {
                    return self
                        .find_persistent_state_by_owner_and_locator(
                            &record.owner_scope,
                            &record.backend_locator,
                        )?
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "failed to retrieve persistent state after registration - database inconsistency detected"
                            )
                        });
                }
                Err(rusqlite::Error::SqliteFailure(err, _))
                    if err.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    if let Some(existing) = self.find_persistent_state_by_owner_and_locator(
                        &record.owner_scope,
                        &record.backend_locator,
                    )? {
                        return Ok(existing);
                    }
                    continue;
                }
                Err(err) => return Err(err.into()),
            }
        }

        anyhow::bail!(
            "failed to allocate a unique persistent state id after {} attempts",
            MAX_STATE_ID_GENERATION_ATTEMPTS
        );
    }

    pub fn find_service_binding_by_identity(
        &self,
        owner_scope: &str,
        service_name: &str,
        binding_kind: &str,
    ) -> Result<Option<ServiceBindingRecord>> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT binding_id, owner_scope, service_name, binding_kind, transport_kind, adapter_kind, endpoint_locator, tls_mode, allowed_callers_json, target_hint, created_at, updated_at
             FROM service_bindings
             WHERE owner_scope=?1 AND service_name=?2 AND binding_kind=?3",
            params![owner_scope, service_name, binding_kind],
            Self::map_service_binding_row,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn find_service_binding_by_id(
        &self,
        binding_id: &str,
    ) -> Result<Option<ServiceBindingRecord>> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT binding_id, owner_scope, service_name, binding_kind, transport_kind, adapter_kind, endpoint_locator, tls_mode, allowed_callers_json, target_hint, created_at, updated_at
             FROM service_bindings
             WHERE binding_id=?1",
            params![binding_id],
            Self::map_service_binding_row,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn resolve_service_binding(
        &self,
        owner_scope: &str,
        service_name: &str,
        binding_kind: &str,
        caller_service: Option<&str>,
    ) -> Result<Option<ServiceBindingRecord>> {
        let Some(record) =
            self.find_service_binding_by_identity(owner_scope, service_name, binding_kind)?
        else {
            return Ok(None);
        };

        if record.allowed_callers.is_empty() {
            return Ok(Some(record));
        }

        let caller_service = caller_service
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "service binding '{}' requires caller_service because access is restricted to {:?}",
                    record.binding_id,
                    record.allowed_callers
                )
            })?;

        if record
            .allowed_callers
            .iter()
            .any(|value| value == caller_service)
        {
            return Ok(Some(record));
        }

        anyhow::bail!(
            "service '{}' is not allowed to use binding '{}' (allowed callers: {:?})",
            caller_service,
            record.binding_id,
            record.allowed_callers
        );
    }

    pub fn list_service_bindings(
        &self,
        owner_scope: Option<&str>,
        service_name: Option<&str>,
    ) -> Result<Vec<ServiceBindingRecord>> {
        let conn = self.connect()?;
        let sql = match (owner_scope, service_name) {
            (Some(_), Some(_)) => {
                "SELECT binding_id, owner_scope, service_name, binding_kind, transport_kind, adapter_kind, endpoint_locator, tls_mode, allowed_callers_json, target_hint, created_at, updated_at
                 FROM service_bindings
                 WHERE owner_scope=?1 AND service_name=?2
                 ORDER BY updated_at DESC, binding_id ASC"
            }
            (Some(_), None) => {
                "SELECT binding_id, owner_scope, service_name, binding_kind, transport_kind, adapter_kind, endpoint_locator, tls_mode, allowed_callers_json, target_hint, created_at, updated_at
                 FROM service_bindings
                 WHERE owner_scope=?1
                 ORDER BY updated_at DESC, binding_id ASC"
            }
            (None, Some(_)) => {
                "SELECT binding_id, owner_scope, service_name, binding_kind, transport_kind, adapter_kind, endpoint_locator, tls_mode, allowed_callers_json, target_hint, created_at, updated_at
                 FROM service_bindings
                 WHERE service_name=?1
                 ORDER BY updated_at DESC, binding_id ASC"
            }
            (None, None) => {
                "SELECT binding_id, owner_scope, service_name, binding_kind, transport_kind, adapter_kind, endpoint_locator, tls_mode, allowed_callers_json, target_hint, created_at, updated_at
                 FROM service_bindings
                 ORDER BY updated_at DESC, binding_id ASC"
            }
        };

        let mut stmt = conn.prepare(sql)?;
        let rows = match (owner_scope, service_name) {
            (Some(owner_scope), Some(service_name)) => stmt.query_map(
                params![owner_scope, service_name],
                Self::map_service_binding_row,
            )?,
            (Some(owner_scope), None) => {
                stmt.query_map(params![owner_scope], Self::map_service_binding_row)?
            }
            (None, Some(service_name)) => {
                stmt.query_map(params![service_name], Self::map_service_binding_row)?
            }
            (None, None) => stmt.query_map([], Self::map_service_binding_row)?,
        };

        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    pub fn register_service_binding(
        &self,
        record: &NewServiceBindingRecord,
    ) -> Result<ServiceBindingRecord> {
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.connect()?;
        let allowed_callers_json = encode_allowed_callers(&record.allowed_callers)?;

        if let Some(existing) = self.find_service_binding_by_identity(
            &record.owner_scope,
            &record.service_name,
            &record.binding_kind,
        )? {
            conn.execute(
                "UPDATE service_bindings
                 SET transport_kind=?1,
                     adapter_kind=?2,
                     endpoint_locator=?3,
                     tls_mode=?4,
                     allowed_callers_json=?5,
                     target_hint=?6,
                     updated_at=?7
                 WHERE binding_id=?8",
                params![
                    record.transport_kind,
                    record.adapter_kind,
                    record.endpoint_locator,
                    record.tls_mode,
                    &allowed_callers_json,
                    record.target_hint,
                    now,
                    existing.binding_id,
                ],
            )?;
            return self
                .find_service_binding_by_id(&existing.binding_id)?
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "failed to retrieve service binding after update - database inconsistency detected"
                    )
                });
        }

        for _ in 0..MAX_BINDING_ID_GENERATION_ATTEMPTS {
            let binding_id = generate_random_binding_id();
            match conn.execute(
                "INSERT INTO service_bindings(
                    binding_id, owner_scope, service_name, binding_kind, transport_kind, adapter_kind, endpoint_locator, tls_mode, allowed_callers_json, target_hint, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
                params![
                    binding_id,
                    record.owner_scope,
                    record.service_name,
                    record.binding_kind,
                    record.transport_kind,
                    record.adapter_kind,
                    record.endpoint_locator,
                    record.tls_mode,
                    &allowed_callers_json,
                    record.target_hint,
                    now,
                ],
            ) {
                Ok(_) => {
                    return self
                        .find_service_binding_by_id(&binding_id)?
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "failed to retrieve service binding after registration - database inconsistency detected"
                            )
                        });
                }
                Err(rusqlite::Error::SqliteFailure(err, _))
                    if err.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    if let Some(existing) = self.find_service_binding_by_identity(
                        &record.owner_scope,
                        &record.service_name,
                        &record.binding_kind,
                    )? {
                        return Ok(existing);
                    }
                    continue;
                }
                Err(err) => return Err(err.into()),
            }
        }

        anyhow::bail!(
            "failed to allocate a unique service binding id after {} attempts",
            MAX_BINDING_ID_GENERATION_ATTEMPTS
        );
    }

    pub fn delete_service_binding_by_identity(
        &self,
        owner_scope: &str,
        service_name: &str,
        binding_kind: &str,
    ) -> Result<Option<ServiceBindingRecord>> {
        let Some(existing) =
            self.find_service_binding_by_identity(owner_scope, service_name, binding_kind)?
        else {
            return Ok(None);
        };

        let conn = self.connect()?;
        conn.execute(
            "DELETE FROM service_bindings WHERE binding_id=?1",
            params![existing.binding_id],
        )?;
        Ok(Some(existing))
    }

    pub fn list_store_metadata_entries(&self) -> Result<Vec<RegistryStoreMetadataRecord>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT scoped_id, icon_path, text, updated_at
             FROM registry_store_metadata
             ORDER BY scoped_id ASC",
        )?;
        let rows = stmt.query_map([], Self::map_store_metadata_row)?;
        let mut entries = Vec::new();
        for row in rows {
            entries.push(row?);
        }
        Ok(entries)
    }

    pub fn load_store_metadata_entry(
        &self,
        scoped_id: &str,
    ) -> Result<Option<RegistryStoreMetadataRecord>> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT scoped_id, icon_path, text, updated_at
             FROM registry_store_metadata
             WHERE scoped_id=?1",
            params![scoped_id],
            Self::map_store_metadata_row,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn upsert_store_metadata(
        &self,
        scoped_id: &str,
        icon_path: Option<&str>,
        text: Option<&str>,
        updated_at: &str,
    ) -> Result<()> {
        let conn = self.connect()?;
        if icon_path.is_none() && text.is_none() {
            conn.execute(
                "DELETE FROM registry_store_metadata WHERE scoped_id=?1",
                params![scoped_id],
            )?;
            return Ok(());
        }
        conn.execute(
            "INSERT INTO registry_store_metadata(scoped_id, icon_path, text, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(scoped_id) DO UPDATE SET
               icon_path=excluded.icon_path,
               text=excluded.text,
               updated_at=excluded.updated_at",
            params![scoped_id, icon_path, text, updated_at],
        )?;
        Ok(())
    }

    pub fn delete_store_metadata(&self, scoped_id: &str) -> Result<()> {
        let conn = self.connect()?;
        conn.execute(
            "DELETE FROM registry_store_metadata WHERE scoped_id=?1",
            params![scoped_id],
        )?;
        Ok(())
    }

    fn map_persistent_state_row(
        row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<PersistentStateRecord> {
        Ok(PersistentStateRecord {
            state_id: row.get(0)?,
            owner_scope: row.get(1)?,
            state_name: row.get(2)?,
            kind: row.get(3)?,
            backend_kind: row.get(4)?,
            backend_locator: row.get(5)?,
            producer: row.get(6)?,
            purpose: row.get(7)?,
            schema_id: row.get(8)?,
            created_at: row.get(9)?,
            updated_at: row.get(10)?,
        })
    }

    fn map_service_binding_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ServiceBindingRecord> {
        Ok(ServiceBindingRecord {
            binding_id: row.get(0)?,
            owner_scope: row.get(1)?,
            service_name: row.get(2)?,
            binding_kind: row.get(3)?,
            transport_kind: row.get(4)?,
            adapter_kind: row.get(5)?,
            endpoint_locator: row.get(6)?,
            tls_mode: row.get(7)?,
            allowed_callers: decode_allowed_callers(row.get(8)?)?,
            target_hint: row.get(9)?,
            created_at: row.get(10)?,
            updated_at: row.get(11)?,
        })
    }

    fn map_store_metadata_row(
        row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<RegistryStoreMetadataRecord> {
        Ok(RegistryStoreMetadataRecord {
            scoped_id: row.get(0)?,
            icon_path: row.get(1)?,
            text: row.get(2)?,
            updated_at: row.get(3)?,
        })
    }
}
