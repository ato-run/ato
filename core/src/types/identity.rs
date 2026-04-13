//! Identity management for Magnetic Web (`did:key` support)
//!
//! This module provides DID (Decentralized Identifier) support for the Capsule ecosystem.
//! It implements the `did:key` method using Ed25519 keys, compatible with W3C DID Core.
//!
//! # Format
//!
//! - **did:key**: `did:key:z6Mk<multibase-encoded-ed25519-public-key>`
//! - **Internal**: `ed25519:<base64-encoded-public-key>`
//!
//! # Example
//!
//! ```rust
//! use capsule_core::types::identity::{public_key_to_did, did_to_public_key};
//!
//! let public_key = [0u8; 32]; // Example key
//! let did = public_key_to_did(&public_key);
//! assert!(did.starts_with("did:key:z6Mk"));
//!
//! let recovered = did_to_public_key(&did).unwrap();
//! assert_eq!(public_key, recovered);
//! ```

use anyhow::{anyhow, bail, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};

/// Multicodec prefix for Ed25519 public key (0xed01)
const ED25519_MULTICODEC: [u8; 2] = [0xed, 0x01];

/// Base58btc alphabet (same as Bitcoin)
const BASE58_ALPHABET: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// Convert a raw Ed25519 public key to a `did:key` identifier.
///
/// # Arguments
///
/// * `public_key` - 32-byte Ed25519 public key
///
/// # Returns
///
/// A `did:key` string in the format `did:key:z6Mk...`
///
/// # Example
///
/// ```rust
/// use capsule_core::types::identity::public_key_to_did;
///
/// let key = [0u8; 32];
/// let did = public_key_to_did(&key);
/// assert!(did.starts_with("did:key:z6Mk"));
/// ```
pub fn public_key_to_did(public_key: &[u8; 32]) -> String {
    // Prepend multicodec prefix
    let mut bytes = Vec::with_capacity(2 + 32);
    bytes.extend_from_slice(&ED25519_MULTICODEC);
    bytes.extend_from_slice(public_key);

    // Encode with base58btc (multibase prefix 'z')
    let encoded = base58_encode(&bytes);
    format!("did:key:z{}", encoded)
}

/// Extract a raw Ed25519 public key from a `did:key` identifier.
///
/// # Arguments
///
/// * `did` - A `did:key` string (e.g., `did:key:z6Mk...`)
///
/// # Returns
///
/// The 32-byte Ed25519 public key
///
/// # Errors
///
/// Returns an error if:
/// - The DID doesn't start with `did:key:z`
/// - The multicodec prefix is not Ed25519
/// - The decoded key is not 32 bytes
pub fn did_to_public_key(did: &str) -> Result<[u8; 32]> {
    // Validate prefix
    let multibase = did
        .strip_prefix("did:key:z")
        .ok_or_else(|| anyhow!("Invalid did:key format: must start with 'did:key:z'"))?;

    // Decode base58btc
    let bytes = base58_decode(multibase)?;

    // Validate multicodec prefix
    if bytes.len() < 2 {
        bail!("Invalid did:key: decoded bytes too short");
    }
    if bytes[0] != ED25519_MULTICODEC[0] || bytes[1] != ED25519_MULTICODEC[1] {
        bail!(
            "Invalid did:key: expected Ed25519 multicodec (0xed01), got {:02x}{:02x}",
            bytes[0],
            bytes[1]
        );
    }

    // Extract public key
    let key_bytes = &bytes[2..];
    if key_bytes.len() != 32 {
        bail!(
            "Invalid did:key: expected 32-byte public key, got {} bytes",
            key_bytes.len()
        );
    }

    let mut key = [0u8; 32];
    key.copy_from_slice(key_bytes);
    Ok(key)
}

/// Convert internal format (`ed25519:base64`) to `did:key`.
///
/// # Arguments
///
/// * `internal` - Internal format string (e.g., `ed25519:dGVzdC4uLg==`)
///
/// # Returns
///
/// A `did:key` string
pub fn to_did_key(internal: &str) -> Result<String> {
    let public_key = parse_internal_key(internal)?;
    Ok(public_key_to_did(&public_key))
}

/// Convert `did:key` to internal format (`ed25519:base64`).
///
/// # Arguments
///
/// * `did` - A `did:key` string
///
/// # Returns
///
/// Internal format string (e.g., `ed25519:dGVzdC4uLg==`)
pub fn from_did_key(did: &str) -> Result<String> {
    let public_key = did_to_public_key(did)?;
    Ok(format!("ed25519:{}", BASE64.encode(public_key)))
}

/// Parse internal key format (`ed25519:base64`) to raw bytes.
pub fn parse_internal_key(internal: &str) -> Result<[u8; 32]> {
    let value = internal
        .strip_prefix("ed25519:")
        .ok_or_else(|| anyhow!("Internal key must start with 'ed25519:'"))?;

    let decoded = BASE64
        .decode(value)
        .map_err(|e| anyhow!("Failed to decode base64: {}", e))?;

    if decoded.len() != 32 {
        bail!("Public key must be 32 bytes, got {} bytes", decoded.len());
    }

    let mut key = [0u8; 32];
    key.copy_from_slice(&decoded);
    Ok(key)
}

/// Format a raw public key as internal format (`ed25519:base64`).
pub fn format_internal_key(public_key: &[u8; 32]) -> String {
    format!("ed25519:{}", BASE64.encode(public_key))
}

/// Extract the short fingerprint from a DID for display purposes.
///
/// Returns the last 8 characters of the DID.
pub fn did_short_fingerprint(did: &str) -> String {
    if did.len() > 8 {
        did[did.len() - 8..].to_string()
    } else {
        did.to_string()
    }
}

/// Validate that a string is a valid `did:key` identifier.
pub fn is_valid_did_key(did: &str) -> bool {
    did_to_public_key(did).is_ok()
}

// ============================================================================
// Base58 Implementation (minimal, no external dependency)
// ============================================================================

fn base58_encode(data: &[u8]) -> String {
    if data.is_empty() {
        return String::new();
    }

    // Count leading zeros
    let mut zeros = 0;
    for &byte in data {
        if byte == 0 {
            zeros += 1;
        } else {
            break;
        }
    }

    // Convert to base58
    let mut result = Vec::new();
    let mut num = data.to_vec();

    while !(num.is_empty() || num.len() == 1 && num[0] == 0) {
        let mut remainder = 0u32;
        let mut new_num = Vec::new();

        for &byte in &num {
            let acc = (remainder << 8) + byte as u32;
            let digit = acc / 58;
            remainder = acc % 58;

            if !new_num.is_empty() || digit > 0 {
                new_num.push(digit as u8);
            }
        }

        result.push(BASE58_ALPHABET[remainder as usize]);
        num = new_num;
    }

    // Add leading '1's for leading zeros
    result.extend(std::iter::repeat_n(b'1', zeros));

    result.reverse();
    String::from_utf8(result).expect("base58 alphabet is valid UTF-8")
}

fn base58_decode(s: &str) -> Result<Vec<u8>> {
    if s.is_empty() {
        return Ok(Vec::new());
    }

    // Build reverse lookup table
    let mut alphabet_map = [255u8; 128];
    for (i, &c) in BASE58_ALPHABET.iter().enumerate() {
        alphabet_map[c as usize] = i as u8;
    }

    // Count leading '1's (zeros)
    let mut zeros = 0;
    for c in s.chars() {
        if c == '1' {
            zeros += 1;
        } else {
            break;
        }
    }

    // Convert from base58
    let mut result: Vec<u8> = Vec::new();

    for c in s.chars() {
        let c_byte = c as usize;
        if c_byte >= 128 || alphabet_map[c_byte] == 255 {
            bail!("Invalid base58 character: '{}'", c);
        }
        let digit = alphabet_map[c_byte] as u32;

        // Multiply result by 58 and add digit
        let mut carry = digit;
        for byte in result.iter_mut().rev() {
            let acc = (*byte as u32) * 58 + carry;
            *byte = (acc & 0xff) as u8;
            carry = acc >> 8;
        }

        while carry > 0 {
            result.insert(0, (carry & 0xff) as u8);
            carry >>= 8;
        }
    }

    // Add leading zeros
    let mut final_result = vec![0u8; zeros];
    final_result.extend(result);

    Ok(final_result)
}

#[cfg(test)]
mod tests {
    use super::{
        base58_decode, base58_encode, did_short_fingerprint, did_to_public_key,
        format_internal_key, from_did_key, is_valid_did_key, public_key_to_did, to_did_key,
    };

    #[test]
    fn test_public_key_to_did_roundtrip() {
        // Test with a known key
        let public_key: [u8; 32] = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
            0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c,
            0x1d, 0x1e, 0x1f, 0x20,
        ];

        let did = public_key_to_did(&public_key);
        assert!(
            did.starts_with("did:key:z6Mk"),
            "DID should start with did:key:z6Mk"
        );

        let recovered = did_to_public_key(&did).unwrap();
        assert_eq!(public_key, recovered);
    }

    #[test]
    fn test_internal_format_conversion() {
        let public_key: [u8; 32] = [0x42; 32];

        let internal = format_internal_key(&public_key);
        assert!(internal.starts_with("ed25519:"));

        let did = to_did_key(&internal).unwrap();
        assert!(did.starts_with("did:key:z"));

        let back_to_internal = from_did_key(&did).unwrap();
        assert_eq!(internal, back_to_internal);
    }

    #[test]
    fn test_invalid_did_key() {
        assert!(did_to_public_key("not-a-did").is_err());
        assert!(did_to_public_key("did:web:example.com").is_err());
        assert!(did_to_public_key("did:key:abc").is_err()); // Missing 'z' prefix
    }

    #[test]
    fn test_base58_roundtrip() {
        let data = b"Hello, World!";
        let encoded = base58_encode(data);
        let decoded = base58_decode(&encoded).unwrap();
        assert_eq!(data.to_vec(), decoded);
    }

    #[test]
    fn test_base58_leading_zeros() {
        let data = vec![0, 0, 0, 1, 2, 3];
        let encoded = base58_encode(&data);
        assert!(encoded.starts_with("111")); // Leading zeros become '1's
        let decoded = base58_decode(&encoded).unwrap();
        assert_eq!(data, decoded);
    }

    #[test]
    fn test_did_short_fingerprint() {
        let did = "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK";
        let short = did_short_fingerprint(did);
        assert_eq!(short.len(), 8);
    }

    #[test]
    fn test_is_valid_did_key() {
        let public_key: [u8; 32] = [0x42; 32];
        let valid_did = public_key_to_did(&public_key);

        assert!(is_valid_did_key(&valid_did));
        assert!(!is_valid_did_key("did:web:example.com"));
        assert!(!is_valid_did_key("invalid"));
    }
}
