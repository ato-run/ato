//! IPC Bearer Token management — random token generation for Capsule IPC.
//!
//! The current runtime path only needs per-session token generation.
//! Validation, revocation, and TTL tracking were dormant code paths and
//! have been removed until they are wired into production flows.

use rand::Rng;
use serde::{Deserialize, Serialize};
use tracing::debug;

/// Token length in bytes (32 bytes = 256 bits of entropy).
const TOKEN_LENGTH: usize = 32;

/// An IPC bearer token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcToken {
    /// The token value (hex-encoded).
    pub value: String,
    /// Capabilities this token is scoped to.
    pub scoped_capabilities: Vec<String>,
}

/// Token manager — random token generator.
#[derive(Debug, Clone, Default)]
pub struct TokenManager;

impl TokenManager {
    /// Create a new token manager.
    pub fn new() -> Self {
        Self
    }

    /// Generate a new token with the given capabilities.
    pub fn generate(&self, capabilities: Vec<String>) -> IpcToken {
        let mut rng = rand::thread_rng();
        let mut bytes = [0u8; TOKEN_LENGTH];
        rng.fill(&mut bytes);

        let token = IpcToken {
            value: hex::encode(bytes),
            scoped_capabilities: capabilities,
        };

        debug!(capabilities = ?token.scoped_capabilities, "Generated new IPC token");
        token
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_token() {
        let manager = TokenManager::new();
        let token = manager.generate(vec!["greet".to_string()]);

        assert_eq!(token.value.len(), TOKEN_LENGTH * 2);
        assert_eq!(token.scoped_capabilities, vec!["greet"]);
    }

    #[test]
    fn test_token_uniqueness() {
        let manager = TokenManager::new();
        let t1 = manager.generate(vec![]);
        let t2 = manager.generate(vec![]);
        assert_ne!(t1.value, t2.value, "Each token should be unique");
    }
}
