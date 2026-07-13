//! Verification of short-lived API-to-runner surface assertions.
//!
//! Assertions are compact HMAC-SHA256 envelopes (`base64url(json).hex_mac`).
//! Only the runner reads the claims. Browser-facing code receives an HttpOnly
//! cookie and the session-host boundary injects the assertion during upgrade.

use std::{collections::BTreeMap, fmt, sync::Arc, time::SystemTime};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use protocol::session_surface::{SurfaceAssertionClaims, SurfacePrincipalKind};
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::pixel_gateway::{
    AuthorizedSurfaceAccess, PixelGatewayScope, SurfaceAccessAuthorizer, SurfaceAuthorizationError,
};

const MAX_ASSERTION_BYTES: usize = 8 * 1024;
const MAX_ASSERTION_REMAINING_TTL_SECS: u64 = 5 * 60;
const MIN_KEY_BYTES: usize = 32;

type HmacSha256 = Hmac<Sha256>;

/// Rotation-ready key set indexed by the assertion's `kid` claim.
pub struct SurfaceAssertionKeyring {
    keys: BTreeMap<String, Zeroizing<Vec<u8>>>,
}

impl fmt::Debug for SurfaceAssertionKeyring {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SurfaceAssertionKeyring")
            .field("key_ids", &self.keys.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl SurfaceAssertionKeyring {
    /// Constructs a keyring from UTF-8 secrets. Empty, weak, or malformed
    /// entries fail closed before the runner starts accepting sessions.
    pub fn new(keys: BTreeMap<String, String>) -> Result<Self, SurfaceKeyringError> {
        if keys.is_empty() {
            return Err(SurfaceKeyringError::Empty);
        }

        let mut protected = BTreeMap::new();
        for (kid, secret) in keys {
            if kid.trim().is_empty() || kid.len() > 128 {
                return Err(SurfaceKeyringError::InvalidKeyId);
            }
            if secret.len() < MIN_KEY_BYTES {
                return Err(SurfaceKeyringError::WeakKey(kid));
            }
            protected.insert(kid, Zeroizing::new(secret.into_bytes()));
        }
        Ok(Self { keys: protected })
    }

    fn key(&self, kid: &str) -> Option<&[u8]> {
        self.keys.get(kid).map(|key| key.as_slice())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SurfaceKeyringError {
    #[error("surface assertion keyring must contain at least one key")]
    Empty,
    #[error("surface assertion key id is invalid")]
    InvalidKeyId,
    #[error("surface assertion key {0} must be at least 32 bytes")]
    WeakKey(String),
}

/// Authenticated-phase assertion verifier. Guest claims remain a reserved
/// protocol shape and are deliberately rejected until SPEC-G is approved.
pub struct HmacSurfaceAccessAuthorizer {
    keyring: Arc<SurfaceAssertionKeyring>,
    now: Arc<dyn Fn() -> u64 + Send + Sync>,
}

impl fmt::Debug for HmacSurfaceAccessAuthorizer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HmacSurfaceAccessAuthorizer")
            .field("keyring", &self.keyring)
            .finish_non_exhaustive()
    }
}

impl HmacSurfaceAccessAuthorizer {
    pub fn new(keyring: Arc<SurfaceAssertionKeyring>) -> Self {
        Self::with_clock(keyring, || {
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs())
        })
    }

    fn with_clock(
        keyring: Arc<SurfaceAssertionKeyring>,
        now: impl Fn() -> u64 + Send + Sync + 'static,
    ) -> Self {
        Self {
            keyring,
            now: Arc::new(now),
        }
    }

    fn verify(
        &self,
        assertion: &str,
        scope: &PixelGatewayScope,
    ) -> Result<AuthorizedSurfaceAccess, SurfaceAuthorizationError> {
        if assertion.len() > MAX_ASSERTION_BYTES {
            return Err(SurfaceAuthorizationError);
        }
        let mut parts = assertion.split('.');
        let encoded = parts.next().filter(|part| !part.is_empty());
        let signature = parts.next().filter(|part| !part.is_empty());
        if parts.next().is_some() {
            return Err(SurfaceAuthorizationError);
        }
        let (encoded, signature) = encoded.zip(signature).ok_or(SurfaceAuthorizationError)?;

        let payload = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| SurfaceAuthorizationError)?;
        let claims: SurfaceAssertionClaims =
            serde_json::from_slice(&payload).map_err(|_| SurfaceAuthorizationError)?;
        claims.validate().map_err(|_| SurfaceAuthorizationError)?;

        let key = self
            .keyring
            .key(&claims.kid)
            .ok_or(SurfaceAuthorizationError)?;
        let signature = hex::decode(signature).map_err(|_| SurfaceAuthorizationError)?;
        let mut mac = HmacSha256::new_from_slice(key).map_err(|_| SurfaceAuthorizationError)?;
        mac.update(encoded.as_bytes());
        mac.verify_slice(&signature)
            .map_err(|_| SurfaceAuthorizationError)?;

        let now = (self.now)();
        claims
            .exp
            .checked_sub(now)
            .filter(|ttl| *ttl > 0 && *ttl <= MAX_ASSERTION_REMAINING_TTL_SECS)
            .ok_or(SurfaceAuthorizationError)?;
        if claims.session_id != scope.session_id
            || claims.surface_id != scope.surface_id
            || claims.principal.kind != SurfacePrincipalKind::User
        {
            return Err(SurfaceAuthorizationError);
        }

        Ok(AuthorizedSurfaceAccess {
            principal: claims.principal.id,
            grant_id: claims.jti,
        })
    }
}

impl SurfaceAccessAuthorizer for HmacSurfaceAccessAuthorizer {
    fn authorize(
        &self,
        assertion: &str,
        scope: &PixelGatewayScope,
    ) -> Result<AuthorizedSurfaceAccess, SurfaceAuthorizationError> {
        self.verify(assertion, scope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::session_surface::{
        SURFACE_GATEWAY_ASSERTION_AUDIENCE, SurfaceAssertionPrincipal,
    };

    const NOW: u64 = 1_800_000_000;
    const KEY: &str = "0123456789abcdef0123456789abcdef";

    fn scope() -> PixelGatewayScope {
        PixelGatewayScope {
            session_id: "session-1".to_string(),
            surface_id: "surface-1".to_string(),
        }
    }

    fn claims() -> SurfaceAssertionClaims {
        SurfaceAssertionClaims {
            aud: SURFACE_GATEWAY_ASSERTION_AUDIENCE.to_string(),
            session_id: "session-1".to_string(),
            surface_id: "surface-1".to_string(),
            principal: SurfaceAssertionPrincipal {
                kind: SurfacePrincipalKind::User,
                id: "user-1".to_string(),
            },
            exp: NOW + 60,
            jti: "grant-1".to_string(),
            kid: "staging-v1".to_string(),
        }
    }

    fn keyring() -> Arc<SurfaceAssertionKeyring> {
        Arc::new(
            SurfaceAssertionKeyring::new(BTreeMap::from([(
                "staging-v1".to_string(),
                KEY.to_string(),
            )]))
            .expect("valid keyring"),
        )
    }

    fn sign(claims: &SurfaceAssertionClaims, key: &[u8]) -> String {
        let encoded = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).expect("claims encode"));
        let mut mac = HmacSha256::new_from_slice(key).expect("valid HMAC key");
        mac.update(encoded.as_bytes());
        format!("{encoded}.{}", hex::encode(mac.finalize().into_bytes()))
    }

    fn authorizer() -> HmacSurfaceAccessAuthorizer {
        HmacSurfaceAccessAuthorizer::with_clock(keyring(), || NOW)
    }

    #[test]
    fn accepts_valid_authenticated_assertion() {
        let assertion = sign(&claims(), KEY.as_bytes());

        let access = authorizer()
            .authorize(&assertion, &scope())
            .expect("assertion should authorize");

        assert_eq!(access.principal, "user-1");
        assert_eq!(access.grant_id, "grant-1");
    }

    #[test]
    fn rejects_signature_scope_expiry_and_guest_claims() {
        let mut cases = Vec::new();

        let mut wrong_scope = claims();
        wrong_scope.session_id = "other-session".to_string();
        cases.push(sign(&wrong_scope, KEY.as_bytes()));

        let mut expired = claims();
        expired.exp = NOW;
        cases.push(sign(&expired, KEY.as_bytes()));

        let mut excessive_ttl = claims();
        excessive_ttl.exp = NOW + MAX_ASSERTION_REMAINING_TTL_SECS + 1;
        cases.push(sign(&excessive_ttl, KEY.as_bytes()));

        let mut guest = claims();
        guest.principal.kind = SurfacePrincipalKind::Guest;
        cases.push(sign(&guest, KEY.as_bytes()));

        cases.push(sign(&claims(), b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));

        for assertion in cases {
            assert!(authorizer().authorize(&assertion, &scope()).is_err());
        }
    }

    #[test]
    fn keyring_rejects_empty_and_weak_keys_without_exposing_secrets() {
        assert_eq!(
            SurfaceAssertionKeyring::new(BTreeMap::new()).expect_err("must reject empty"),
            SurfaceKeyringError::Empty
        );
        let weak = SurfaceAssertionKeyring::new(BTreeMap::from([(
            "weak".to_string(),
            "do-not-print-this".to_string(),
        )]));
        assert_eq!(
            weak.expect_err("must reject weak key"),
            SurfaceKeyringError::WeakKey("weak".to_string())
        );
    }
}
