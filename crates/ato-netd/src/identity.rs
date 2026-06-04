//! Runtime identity for this `ato-netd` installation.
//!
//! The identity is persisted to
//! `${ATO_HOME}/state/netd/runtime_identity.json` and loaded on every
//! daemon start. If no file exists it is generated and written on first
//! start. The file is never rotated automatically; operators can delete
//! it to force a new identity (e.g., after a re-provision).
//!
//! The `control_token` is an opaque 32-byte random hex string. It is
//! **not** included in the regular `status` response. Callers with local
//! socket access can retrieve it explicitly via the `BootstrapToken` verb,
//! which is intended for pairing and remote-control bootstrap flows.

use serde::{Deserialize, Serialize};

/// Stable identity for a single `ato-netd` installation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeIdentity {
    /// Stable UUID for this installation. Generated once and persisted.
    pub runtime_id: String,
    /// Opaque bearer token for authenticating remote callers. A 32-byte
    /// random value encoded as 64 lowercase hex characters.
    pub control_token: String,
}

impl RuntimeIdentity {
    /// Load the identity from `<ato_home>/state/netd/runtime_identity.json`.
    ///
    /// If the file does not exist (first start) a new identity is
    /// generated and written to disk before being returned. If the file
    /// exists but cannot be parsed it is overwritten with a fresh
    /// identity so that a corrupt file never prevents the daemon from
    /// starting.
    pub fn load_or_create(ato_home: &std::path::Path) -> anyhow::Result<Self> {
        let path = ato_home.join("state/netd/runtime_identity.json");

        if path.exists() {
            match try_load_from_path(&path) {
                Ok(identity) => return Ok(identity),
                Err(err) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %err,
                        "runtime identity file could not be parsed; regenerating"
                    );
                }
            }
        }

        let identity = Self::generate();
        persist(&path, &identity)?;
        Ok(identity)
    }

    /// Generate a fresh identity with random `runtime_id` and
    /// `control_token`. This is a pure function with no I/O — useful for
    /// tests.
    fn generate() -> Self {
        use rand::RngCore;

        // runtime_id: format a random 128-bit value as a UUID v4 string.
        // We generate the bytes manually so that we avoid adding a uuid
        // crate dep; the format is well-known and stable.
        let mut id_bytes = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut id_bytes);
        // Set version bits (v4) and variant bits (RFC 4122).
        id_bytes[6] = (id_bytes[6] & 0x0f) | 0x40;
        id_bytes[8] = (id_bytes[8] & 0x3f) | 0x80;
        let runtime_id = format!(
            "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
            u32::from_be_bytes(id_bytes[0..4].try_into().unwrap()),
            u16::from_be_bytes(id_bytes[4..6].try_into().unwrap()),
            u16::from_be_bytes(id_bytes[6..8].try_into().unwrap()),
            u16::from_be_bytes(id_bytes[8..10].try_into().unwrap()),
            {
                let mut tail = [0u8; 8];
                tail[2..].copy_from_slice(&id_bytes[10..16]);
                u64::from_be_bytes(tail)
            }
        );

        // control_token: 32 random bytes as 64 lowercase hex characters.
        let mut token_bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut token_bytes);
        let control_token = hex::encode(token_bytes);

        Self {
            runtime_id,
            control_token,
        }
    }
}

fn try_load_from_path(path: &std::path::Path) -> anyhow::Result<RuntimeIdentity> {
    let bytes = std::fs::read(path)?;
    let identity: RuntimeIdentity = serde_json::from_slice(&bytes)?;
    Ok(identity)
}

fn persist(path: &std::path::Path, identity: &RuntimeIdentity) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(identity)?;
    std::fs::write(path, json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_produces_valid_uuid_format() {
        let identity = RuntimeIdentity::generate();
        let parts: Vec<&str> = identity.runtime_id.split('-').collect();
        assert_eq!(parts.len(), 5, "runtime_id must have 5 UUID segments");
        assert_eq!(parts[0].len(), 8, "segment 0 must be 8 hex chars");
        assert_eq!(parts[1].len(), 4, "segment 1 must be 4 hex chars");
        assert_eq!(parts[2].len(), 4, "segment 2 must be 4 hex chars");
        assert_eq!(parts[3].len(), 4, "segment 3 must be 4 hex chars");
        assert_eq!(parts[4].len(), 12, "segment 4 must be 12 hex chars");
        // version nibble must be 4
        assert_eq!(&parts[2][0..1], "4", "version nibble must be '4'");
    }

    #[test]
    fn generate_produces_64_char_hex_token() {
        let identity = RuntimeIdentity::generate();
        assert_eq!(
            identity.control_token.len(),
            64,
            "control_token must be 64 hex chars"
        );
        assert!(
            identity
                .control_token
                .chars()
                .all(|c| c.is_ascii_hexdigit()),
            "control_token must be hex"
        );
    }

    #[test]
    fn two_identities_are_distinct() {
        let a = RuntimeIdentity::generate();
        let b = RuntimeIdentity::generate();
        assert_ne!(a.runtime_id, b.runtime_id);
        assert_ne!(a.control_token, b.control_token);
    }

    #[test]
    fn load_or_create_round_trips_through_json() {
        let dir = tempfile::tempdir().unwrap();
        // Write a known identity into the expected sub-path.
        let state_dir = dir.path().join("state/netd");
        std::fs::create_dir_all(&state_dir).unwrap();
        let path = state_dir.join("runtime_identity.json");
        let known = RuntimeIdentity {
            runtime_id: "11111111-2222-4333-8444-555555555555".to_string(),
            control_token: "a".repeat(64),
        };
        std::fs::write(&path, serde_json::to_string(&known).unwrap()).unwrap();
        let loaded = RuntimeIdentity::load_or_create(dir.path()).unwrap();
        assert_eq!(loaded.runtime_id, known.runtime_id);
        assert_eq!(loaded.control_token, known.control_token);
    }

    #[test]
    fn load_or_create_persists_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let first = RuntimeIdentity::load_or_create(dir.path()).unwrap();
        let second = RuntimeIdentity::load_or_create(dir.path()).unwrap();
        assert_eq!(first.runtime_id, second.runtime_id);
        assert_eq!(first.control_token, second.control_token);
    }
}
