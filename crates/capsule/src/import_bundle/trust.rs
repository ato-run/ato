//! Signer trust — the axis that is **not** signature validity.
//!
//! A valid signature proves the bytes have not changed since they were signed.
//! It says nothing about whether the signer is anyone this device should listen
//! to. RFC §"Signer trust": those are two independent axes and conflating them
//! is the specific regression this format is designed to prevent.
//!
//! Two rules carry most of the weight here:
//!
//! 1. **`claimed_issuer` is never read.** It is the signer's own claim about
//!    itself, so an attacker writes `"ato-store"` on a bundle they signed with
//!    their own key. Nothing in this file consults it.
//! 2. **Store pins are origin-scoped, and the origin travels with the call.** A
//!    policy holding pins for every known origin, asked "does this match *any*
//!    pinned key", would let a staging-only key authenticate a bundle claimed to
//!    come from `api.ato.run`. So [`CapsuleTrustPolicy::resolve`] only ever
//!    consults the pins registered for the exact [`NormalizedOrigin`] in the
//!    [`CapsuleImportContext::Store`] variant passed in for *this* call.

use ed25519_dalek::{Signer, SigningKey};
use rand::rngs::OsRng;

use super::index::Sha256Digest;
use super::signature::DidKey;

/// Where the bundle came from, and what the caller already knows about it.
///
/// The origin and the expected digest travel together because a Store-fetched
/// bundle always has both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapsuleImportContext {
    /// A `.capsule` file the user handed us.
    LocalFile {
        /// Checked when present, skipped when absent — a local file usually has
        /// no digest to check against, but a caller that does have one (a
        /// re-import of something it just wrote) gets the same check the Store
        /// path gets.
        expected_bundle_digest: Option<Sha256Digest>,
    },
    /// A bundle downloaded from a Store API origin.
    Store {
        /// Only this origin's pins are eligible to produce
        /// [`SignerTrust::TrustedStore`].
        api_origin: NormalizedOrigin,
        /// Mandatory: the Store told us which bytes to expect.
        expected_bundle_digest: Sha256Digest,
    },
}

impl CapsuleImportContext {
    /// The digest the caller asserted, if any.
    #[must_use]
    pub fn expected_bundle_digest(&self) -> Option<&Sha256Digest> {
        match self {
            Self::LocalFile {
                expected_bundle_digest,
            } => expected_bundle_digest.as_ref(),
            Self::Store {
                expected_bundle_digest,
                ..
            } => Some(expected_bundle_digest),
        }
    }
}

/// How much trust the signing key carries — never how valid the signature is.
///
/// [`Self::TrustedPublisher`] and [`Self::TrustedLocalKey`] are reserved so
/// downstream API responses and CLI output stay stable across slices. **No code
/// path in Slice 1 constructs either one**: there is no mechanism for a
/// publisher key to reach a builder, and persisted-TOFU local trust is deferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignerTrust {
    /// The key matches a pin registered for the exact origin this bundle was
    /// fetched from.
    TrustedStore,
    /// Reserved; not produced in Slice 1.
    TrustedPublisher,
    /// Reserved; not produced in Slice 1.
    TrustedLocalKey,
    /// Signature is structurally valid — integrity holds — but the signer's
    /// identity carries no established trust.
    UntrustedKey,
}

impl SignerTrust {
    /// The stable wire spelling used by API responses and CLI output.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TrustedStore => "trusted_store",
            Self::TrustedPublisher => "trusted_publisher",
            Self::TrustedLocalKey => "trusted_local_key",
            Self::UntrustedKey => "untrusted_key",
        }
    }
}

/// An API origin in one canonical spelling, so a pin lookup is a byte
/// comparison rather than a URL-equivalence question.
///
/// Normalization: lowercase scheme and host, default port for the scheme
/// dropped, no path, no query, no fragment, no trailing slash.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NormalizedOrigin(String);

impl NormalizedOrigin {
    /// Normalize an origin URL.
    ///
    /// # Errors
    ///
    /// A reason string when the input is not an absolute URL with a host.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let parsed = url::Url::parse(raw)
            .map_err(|source| format!("origin {raw:?} is not an absolute URL: {source}"))?;
        let scheme = parsed.scheme().to_ascii_lowercase();
        if scheme != "http" && scheme != "https" {
            return Err(format!(
                "origin {raw:?} must use http or https, got {scheme:?}"
            ));
        }
        let host = parsed
            .host_str()
            .ok_or_else(|| format!("origin {raw:?} has no host"))?
            .to_ascii_lowercase();
        // `port()` is already `None` for the scheme's default port, so the
        // canonical form drops `:443` on https without a special case.
        let origin = match parsed.port() {
            Some(port) => format!("{scheme}://{host}:{port}"),
            None => format!("{scheme}://{host}"),
        };
        Ok(Self(origin))
    }

    /// The canonical spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One API origin's pinned Store distribution keys.
///
/// RFC §"Store trust roots": at most two (current + next), which is the whole
/// of Slice 1's rotation mechanism — no bundle-carried `previous_key` record,
/// no general rotation infrastructure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedStoreOrigin {
    origin: NormalizedOrigin,
    keys: Vec<DidKey>,
}

/// The maximum number of keys one origin may pin.
pub const MAX_PINNED_KEYS_PER_ORIGIN: usize = 2;

impl PinnedStoreOrigin {
    /// Pin `keys` to `origin`.
    ///
    /// # Errors
    ///
    /// A reason string when the pin array is empty or longer than
    /// [`MAX_PINNED_KEYS_PER_ORIGIN`].
    pub fn new(origin: NormalizedOrigin, keys: Vec<DidKey>) -> Result<Self, String> {
        if keys.is_empty() {
            return Err(format!(
                "origin {} must pin at least one key",
                origin.as_str()
            ));
        }
        if keys.len() > MAX_PINNED_KEYS_PER_ORIGIN {
            return Err(format!(
                "origin {} pins {} keys; at most {MAX_PINNED_KEYS_PER_ORIGIN} (current + next) \
                 are allowed",
                origin.as_str(),
                keys.len()
            ));
        }
        Ok(Self { origin, keys })
    }

    /// The origin these keys are pinned to.
    #[must_use]
    pub fn origin(&self) -> &NormalizedOrigin {
        &self.origin
    }
}

/// The caller's trust configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapsuleTrustPolicy {
    store_key_pins: Vec<PinnedStoreOrigin>,
    accept_untrusted_with_confirmation: bool,
}

impl CapsuleTrustPolicy {
    /// A policy with no Store pins that refuses an untrusted local signer.
    ///
    /// Fail-closed default: a caller that has not decided what to do about an
    /// unknown key gets a refusal, not an import.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an origin's pin array. A second registration for the same origin
    /// replaces the first.
    #[must_use]
    pub fn with_store_pins(mut self, pins: PinnedStoreOrigin) -> Self {
        self.store_key_pins
            .retain(|existing| existing.origin != pins.origin);
        self.store_key_pins.push(pins);
        self
    }

    /// Declare that this caller's UI layer will obtain explicit confirmation
    /// before an [`SignerTrust::UntrustedKey`] local import is admitted.
    ///
    /// This module never prompts — that is the CLI/PWA layer's job. Setting this
    /// only means "return me the classification instead of refusing outright".
    #[must_use]
    pub fn accepting_untrusted_local_signers(mut self) -> Self {
        self.accept_untrusted_with_confirmation = true;
        self
    }

    /// Whether an untrusted local signer may be handed back to the caller.
    #[must_use]
    pub fn accepts_untrusted_with_confirmation(&self) -> bool {
        self.accept_untrusted_with_confirmation
    }

    /// Classify `key` under `context`.
    ///
    /// The origin boundary lives here: a [`CapsuleImportContext::Store`] call
    /// consults **only** the pins registered for its own `api_origin`, so a key
    /// pinned for staging cannot authenticate a bundle claimed to come from
    /// production. A [`CapsuleImportContext::LocalFile`] call consults no pins at
    /// all — Slice 1 has no local trust store, so every local signer is
    /// [`SignerTrust::UntrustedKey`].
    #[must_use]
    pub fn resolve(&self, context: &CapsuleImportContext, key: &DidKey) -> SignerTrust {
        let CapsuleImportContext::Store { api_origin, .. } = context else {
            return SignerTrust::UntrustedKey;
        };
        let matched = self
            .store_key_pins
            .iter()
            .filter(|pins| &pins.origin == api_origin)
            .any(|pins| pins.keys.iter().any(|pinned| pinned == key));
        if matched {
            SignerTrust::TrustedStore
        } else {
            SignerTrust::UntrustedKey
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Writer-side signing
// ─────────────────────────────────────────────────────────────────────────────

/// What the v3 writer needs from a signing identity.
///
/// The abstraction exists so one writer serves both producers: the Slice-1
/// ephemeral local export signer below, and (in a later slice, on the API side)
/// a Store distribution signer whose secret never enters this process.
pub trait CapsuleIndexSigner {
    /// The canonical `did:key` written into `signature.json`'s `key_id`.
    fn key_id(&self) -> &DidKey;

    /// Sign `message` — the already domain-separated bytes from
    /// [`super::signing_message`].
    ///
    /// # Errors
    ///
    /// Implementation-defined; a remote signer can fail for reasons an in-process
    /// one cannot.
    fn sign(&self, message: &[u8]) -> Result<[u8; 64], String>;
}

/// A bundle-scoped Ed25519 key: generated fresh, used once, discarded.
///
/// RFC §"Slice 1 signer policy": the local `export` path signs with a key that
/// must not outlive the signing operation, which is exactly why it is **not**
/// [`crate::types::signing::StoredKey`] — that type persists `secret_key` as
/// plaintext base64 JSON. This one has no `write`, no `read`, no `Serialize`,
/// and a redacted [`std::fmt::Debug`]; it exists only inside the call that makes
/// a bundle.
///
/// Readers resolve bundles signed this way to [`SignerTrust::UntrustedKey`], by
/// design: an ephemeral key is unrecognizable by construction.
pub struct EphemeralLocalSigner {
    signing_key: SigningKey,
    key_id: DidKey,
}

impl EphemeralLocalSigner {
    /// Generate a fresh keypair from the OS CSPRNG.
    #[must_use]
    pub fn generate() -> Self {
        let signing_key = SigningKey::generate(&mut OsRng);
        let key_id = DidKey::from_public_key(&signing_key.verifying_key().to_bytes());
        Self {
            signing_key,
            key_id,
        }
    }
}

/// Redacted, not derived: a derived `Debug` would print the secret scalar, which
/// is the one thing this type exists to keep from escaping.
impl std::fmt::Debug for EphemeralLocalSigner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EphemeralLocalSigner")
            .field("key_id", &self.key_id.as_str())
            .field("signing_key", &"<redacted>")
            .finish()
    }
}

impl CapsuleIndexSigner for EphemeralLocalSigner {
    fn key_id(&self) -> &DidKey {
        &self.key_id
    }

    fn sign(&self, message: &[u8]) -> Result<[u8; 64], String> {
        Ok(self.signing_key.sign(message).to_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin(raw: &str) -> NormalizedOrigin {
        NormalizedOrigin::parse(raw).expect("test origin")
    }

    #[test]
    fn origin_normalization_is_canonical() {
        assert_eq!(
            origin("https://API.ato.run").as_str(),
            "https://api.ato.run"
        );
        assert_eq!(
            origin("https://api.ato.run:443/v1/store").as_str(),
            "https://api.ato.run"
        );
        assert_eq!(
            origin("http://localhost:8787/").as_str(),
            "http://localhost:8787"
        );
        assert!(NormalizedOrigin::parse("ftp://api.ato.run").is_err());
        assert!(NormalizedOrigin::parse("/relative").is_err());
    }

    #[test]
    fn store_pins_do_not_cross_origins() {
        let key = DidKey::from_public_key(&[9u8; 32]);
        let policy = CapsuleTrustPolicy::new().with_store_pins(
            PinnedStoreOrigin::new(origin("https://api.ato.run"), vec![key.clone()])
                .expect("pin array"),
        );

        let same_origin = CapsuleImportContext::Store {
            api_origin: origin("https://api.ato.run"),
            expected_bundle_digest: Sha256Digest::of_bytes(b""),
        };
        assert_eq!(
            policy.resolve(&same_origin, &key),
            SignerTrust::TrustedStore
        );

        let other_origin = CapsuleImportContext::Store {
            api_origin: origin("https://staging-api.ato.run"),
            expected_bundle_digest: Sha256Digest::of_bytes(b""),
        };
        assert_eq!(
            policy.resolve(&other_origin, &key),
            SignerTrust::UntrustedKey,
            "a pin registered for one origin must not authenticate another"
        );
    }

    #[test]
    fn local_context_never_consults_pins() {
        let key = DidKey::from_public_key(&[9u8; 32]);
        let policy = CapsuleTrustPolicy::new().with_store_pins(
            PinnedStoreOrigin::new(origin("https://api.ato.run"), vec![key.clone()])
                .expect("pin array"),
        );
        let local = CapsuleImportContext::LocalFile {
            expected_bundle_digest: None,
        };
        assert_eq!(policy.resolve(&local, &key), SignerTrust::UntrustedKey);
    }

    #[test]
    fn pin_arrays_are_capped_at_two() {
        let keys: Vec<DidKey> = (0..3u8)
            .map(|index| DidKey::from_public_key(&[index; 32]))
            .collect();
        assert!(PinnedStoreOrigin::new(origin("https://api.ato.run"), keys).is_err());
        assert!(PinnedStoreOrigin::new(origin("https://api.ato.run"), Vec::new()).is_err());
    }

    #[test]
    fn ephemeral_signer_debug_redacts_the_secret() {
        let signer = EphemeralLocalSigner::generate();
        let rendered = format!("{signer:?}");
        assert!(rendered.contains("<redacted>"));
        assert!(rendered.contains(signer.key_id().as_str()));
    }
}
