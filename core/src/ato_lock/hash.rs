use crate::ato_lock::canonicalize::canonical_projection;
use crate::ato_lock::schema::{AtoLock, LockId};
use crate::error::{CapsuleError, Result};

/// Returns the JCS bytes of the canonical lock identity projection.
pub fn canonical_projection_bytes(lock: &AtoLock) -> Result<Vec<u8>> {
    serde_jcs::to_vec(&canonical_projection(lock)).map_err(|err| {
        CapsuleError::Config(format!(
            "Failed to canonicalize ato.lock projection for lock_id: {err}"
        ))
    })
}

/// Computes the deterministic lock_id from the canonical projection only.
pub fn compute_lock_id(lock: &AtoLock) -> Result<LockId> {
    let canonical = canonical_projection_bytes(lock)?;
    Ok(LockId::new(format!(
        "blake3:{}",
        blake3::hash(&canonical).to_hex()
    )))
}

/// Recomputes and stores lock_id on a draft or persisted lock value.
pub fn recompute_lock_id(lock: &mut AtoLock) -> Result<LockId> {
    let lock_id = compute_lock_id(lock)?;
    lock.lock_id = Some(lock_id.clone());
    Ok(lock_id)
}

/// Returns the canonical persisted document bytes after recomputing lock_id.
pub fn canonical_document_bytes(lock: &AtoLock) -> Result<Vec<u8>> {
    let mut persisted = lock.clone();
    recompute_lock_id(&mut persisted)?;
    serde_jcs::to_vec(&persisted)
        .map_err(|err| CapsuleError::Config(format!("Failed to canonicalize ato.lock JSON: {err}")))
}
