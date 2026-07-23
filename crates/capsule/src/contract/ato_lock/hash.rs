use crate::ato_lock::canonicalize::{
    canonical_identity_projection, canonical_signature_projection,
};
use crate::ato_lock::closure::normalize_lock_closure;
use crate::ato_lock::schema::{AtoLock, LockId};
use crate::error::{CapsuleError, Result};

/// Returns the JCS bytes of the canonical lock identity projection (the
/// `lock_id` preimage).
pub fn canonical_projection_bytes(lock: &AtoLock) -> Result<Vec<u8>> {
    serde_jcs::to_vec(&canonical_identity_projection(lock)?).map_err(|err| {
        CapsuleError::Config(format!(
            "Failed to canonicalize ato.lock projection for lock_id: {err}"
        ))
    })
}

/// Returns the canonical bytes that standard lock signatures must cover: the
/// identity projection ∪ the additive execution sections (`execution_contract`,
/// `launch`). This is a strict superset of [`canonical_projection_bytes`] and is
/// byte-identical to it when neither execution section is present, so an
/// already-signed legacy lock verifies unchanged.
pub fn canonical_signature_payload_bytes(lock: &AtoLock) -> Result<Vec<u8>> {
    serde_jcs::to_vec(&canonical_signature_projection(lock)?).map_err(|err| {
        CapsuleError::Config(format!(
            "Failed to canonicalize ato.lock signature projection: {err}"
        ))
    })
}

/// Computes the deterministic lock_id from the canonical identity projection
/// only. The additive execution sections are intentionally excluded so adding or
/// removing them never changes an existing lock's identity (D4).
pub fn compute_lock_id(lock: &AtoLock) -> Result<LockId> {
    let canonical = canonical_projection_bytes(lock)?;
    Ok(LockId::new(format!(
        "blake3:{}",
        blake3::hash(&canonical).to_hex()
    )))
}

/// Recomputes and stores lock_id on a draft or persisted lock value.
pub fn recompute_lock_id(lock: &mut AtoLock) -> Result<LockId> {
    normalize_lock_closure(lock)?;
    let lock_id = compute_lock_id(lock)?;
    lock.lock_id = Some(lock_id.clone());
    Ok(lock_id)
}

/// Returns the canonical persisted document bytes after recomputing lock_id.
pub fn canonical_document_bytes(lock: &AtoLock) -> Result<Vec<u8>> {
    let mut persisted = lock.clone();
    normalize_lock_closure(&mut persisted)?;
    recompute_lock_id(&mut persisted)?;
    serde_jcs::to_vec(&persisted)
        .map_err(|err| CapsuleError::Config(format!("Failed to canonicalize ato.lock JSON: {err}")))
}
