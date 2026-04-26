use crate::config;
use crate::error::{CapsuleError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

/// A revoked key entry stored in the local revocation cache.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RevokedKey {
    /// DID or fingerprint of the revoked key.
    pub id: String,
    /// ISO-8601 timestamp when revocation was recorded locally.
    pub revoked_at: String,
    /// Optional human-readable reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Persistent local trust store.
///
/// Tracks:
/// - TOFU fingerprints (`fingerprints`): maps DID → hex fingerprint
/// - Petnames (`petnames`): maps DID → human-friendly nickname
/// - Revocation cache (`revoked`): locally known revoked keys
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TrustStore {
    /// TOFU fingerprint map: DID → hex fingerprint.
    pub fingerprints: HashMap<String, String>,

    /// Human-readable petnames: DID → name.
    ///
    /// Petnames are purely local — they are never transmitted to the network.
    /// A petname overrides the raw DID in any UI that calls [`TrustStore::display_name`].
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub petnames: HashMap<String, String>,

    /// Locally-cached revocation entries.
    ///
    /// Populated by [`TrustStore::merge_revocation_list`] or manual `ato key revoke`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub revoked: Vec<RevokedKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustState {
    Verified,
    Untrusted,
    Unknown,
}

impl TrustStore {
    pub fn load() -> Result<Self> {
        let path = trust_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(&path)
            .map_err(|e| CapsuleError::Config(format!("Failed to read trust store: {}", e)))?;
        serde_json::from_str(&raw)
            .map_err(|e| CapsuleError::Config(format!("Failed to parse trust store: {}", e)))
    }

    pub fn save(&self) -> Result<()> {
        let path = trust_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| CapsuleError::Config(format!("Failed to create trust dir: {}", e)))?;
        }
        let raw = serde_json::to_string_pretty(self)
            .map_err(|e| CapsuleError::Config(format!("Failed to serialize trust store: {}", e)))?;
        fs::write(&path, raw)
            .map_err(|e| CapsuleError::Config(format!("Failed to write trust store: {}", e)))?;
        Ok(())
    }

    /// TOFU check: return `Verified` if the fingerprint matches the stored one,
    /// `Untrusted` if it conflicts, or `Unknown` on first-seen (also records it).
    pub fn verify_or_record(&mut self, did: &str, fingerprint: &str) -> TrustState {
        match self.fingerprints.get(did) {
            Some(existing) if existing == fingerprint => TrustState::Verified,
            Some(_) => TrustState::Untrusted,
            None => {
                self.fingerprints
                    .insert(did.to_string(), fingerprint.to_string());
                TrustState::Unknown
            }
        }
    }

    // ── Petnames ────────────────────────────────────────────────────────────

    /// Assign a petname to a DID.  Pass `None` to remove an existing petname.
    pub fn set_petname(&mut self, did: &str, name: Option<&str>) {
        match name {
            Some(n) => {
                self.petnames.insert(did.to_string(), n.to_string());
            }
            None => {
                self.petnames.remove(did);
            }
        }
    }

    /// Return the petname for a DID, if one has been assigned.
    pub fn get_petname(&self, did: &str) -> Option<&str> {
        self.petnames.get(did).map(String::as_str)
    }

    /// Return the best display name for a DID: petname if available, otherwise the raw DID.
    pub fn display_name<'a>(&'a self, did: &'a str) -> &'a str {
        self.petnames.get(did).map(String::as_str).unwrap_or(did)
    }

    // ── Revocation ──────────────────────────────────────────────────────────

    /// Return `true` if `did_or_fingerprint` appears in the local revocation cache.
    pub fn is_revoked(&self, did_or_fingerprint: &str) -> bool {
        self.revoked.iter().any(|r| r.id == did_or_fingerprint)
    }

    /// Add a single revocation entry.  No-op if the key is already revoked.
    pub fn add_revoked(&mut self, id: &str, reason: Option<&str>) {
        if !self.is_revoked(id) {
            self.revoked.push(RevokedKey {
                id: id.to_string(),
                revoked_at: chrono::Utc::now().to_rfc3339(),
                reason: reason.map(str::to_string),
            });
        }
    }

    /// Merge a list of revoked IDs fetched from a remote revocation list.
    ///
    /// Returns the number of newly added entries.
    pub fn merge_revocation_list(&mut self, ids: &[String]) -> usize {
        let mut added = 0;
        for id in ids {
            if !self.is_revoked(id) {
                self.revoked.push(RevokedKey {
                    id: id.clone(),
                    revoked_at: chrono::Utc::now().to_rfc3339(),
                    reason: Some("remote-revocation-list".to_string()),
                });
                added += 1;
            }
        }
        added
    }

    /// Fetch a JSON revocation list from `url` and return the list of revoked IDs.
    ///
    /// Expected response format: `{ "revoked": ["did:key:...", ...] }`
    ///
    /// This is a blocking HTTP GET.  Call from a `tokio::task::spawn_blocking` context
    /// if used inside an async runtime.
    ///
    /// Returns an empty list on network errors so that a missing/unreachable revocation
    /// endpoint never blocks capsule execution (fail-open for revocation fetch; the local
    /// cache remains authoritative).
    pub fn fetch_revocation_list(url: &str) -> Vec<String> {
        // Use ureq for a simple synchronous HTTP GET.  If ureq is not available in this
        // crate, the caller should gate this behind a feature flag.
        // Stub: network fetch is intentionally left as a compile-time stub.
        // A real implementation would call ureq/reqwest here.
        let _ = url;
        Vec::new()
    }
}

fn trust_path() -> Result<PathBuf> {
    Ok(config::config_dir()?.join("trust_store.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_or_record_tofu() {
        let mut store = TrustStore::default();
        assert_eq!(
            store.verify_or_record("did:key:abc", "fp1"),
            TrustState::Unknown
        );
        assert_eq!(
            store.verify_or_record("did:key:abc", "fp1"),
            TrustState::Verified
        );
        assert_eq!(
            store.verify_or_record("did:key:abc", "fp2"),
            TrustState::Untrusted
        );
    }

    #[test]
    fn petname_crud() {
        let mut store = TrustStore::default();
        let did = "did:key:z6MkAlice";
        assert_eq!(store.get_petname(did), None);
        store.set_petname(did, Some("alice"));
        assert_eq!(store.get_petname(did), Some("alice"));
        assert_eq!(store.display_name(did), "alice");
        store.set_petname(did, None);
        assert_eq!(store.get_petname(did), None);
        assert_eq!(store.display_name(did), did);
    }

    #[test]
    fn revocation_add_and_check() {
        let mut store = TrustStore::default();
        let did = "did:key:z6MkEvil";
        assert!(!store.is_revoked(did));
        store.add_revoked(did, Some("compromised"));
        assert!(store.is_revoked(did));
        // Adding again is idempotent
        store.add_revoked(did, None);
        assert_eq!(store.revoked.len(), 1);
    }

    #[test]
    fn merge_revocation_list() {
        let mut store = TrustStore::default();
        let ids = vec!["did:key:a".to_string(), "did:key:b".to_string()];
        let added = store.merge_revocation_list(&ids);
        assert_eq!(added, 2);
        // Idempotent
        let added2 = store.merge_revocation_list(&ids);
        assert_eq!(added2, 0);
    }

    #[test]
    fn trust_store_serde_roundtrip() {
        let mut store = TrustStore::default();
        store.verify_or_record("did:key:abc", "fp1");
        store.set_petname("did:key:abc", Some("alice"));
        store.add_revoked("did:key:evil", None);

        let json = serde_json::to_string(&store).unwrap();
        let back: TrustStore = serde_json::from_str(&json).unwrap();
        assert_eq!(back.fingerprints, store.fingerprints);
        assert_eq!(back.petnames, store.petnames);
        assert_eq!(back.revoked.len(), 1);
    }
}
