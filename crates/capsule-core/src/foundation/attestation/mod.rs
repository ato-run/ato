//! A2 trust layer: signed attestations over A1 blobs and capsule payloads.
//!
//! The attestation format is intentionally narrow — one envelope per
//! signature, JCS-canonical statement bytes, Ed25519 only — so it can be
//! understood without a transparency log or external trust ceremony.
//! Sigstore / online verifiers can layer on top of this format later
//! without changing the wire bytes.
//!
//! ## Module layout
//!
//! - [`types`] — `AttestationStatement`, `AttestationEnvelope`, and the
//!   subject / predicate value types. Frozen wire format.
//! - [`sign`] — produce envelopes from statements and an Ed25519 key.
//! - [`verify`] — re-canonicalize and validate an envelope against a
//!   trust root.
//! - [`store`] — read / write envelopes under
//!   `~/.ato/store/attestations/<kind>/<hash>/`.
//!
//! ## Frozen prefix
//!
//! Statement bytes that go through the signature are always preceded by
//! `b"ato-attestation-v1\0"`. A future revision must change the prefix
//! and the on-wire `algorithm` tag together.

pub mod sign;
pub mod store;
pub mod types;
pub mod verify;

pub use sign::{
    generate_keypair, sign_envelope, AttestationKey, AttestationKeyError, StoredAttestationKey,
};
pub use store::{
    blob_attestations_dir, payload_attestations_dir, read_envelope, store_envelope,
    trust_root_path, write_trust_root_pubkey,
};
pub use types::{
    AttestationEnvelope, AttestationPredicate, AttestationStatement, AttestationSubject,
    FreezeMetadata, PolicySnapshot, Signature, SourceRef, ATTESTATION_ALGORITHM,
    ATTESTATION_SCHEMA_VERSION, ATTESTATION_SIGNING_PREFIX,
};
pub use verify::{verify_envelope, AttestationVerifyError, TrustRoot, VerifiedAttestation};
