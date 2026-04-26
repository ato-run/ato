---
title: "Ato AppSync Signature Specification v1.0"
status: accepted
date: "2026-02-03"
author: "@egamikohsuke"
ssot:
  - "packages/capsule-core/"
---

# Ato AppSync Signature Specification v1.0

## Overview

This document defines the signature format and verification protocol for Ato AppSync `.sync` files.

## Hash Target

- **Target**: Raw bytes of the `.sync` file (entire file)
- **Algorithm**: BLAKE3-256
- **Output Format**: `blake3:<64-character-hex-string>`
- **Example**: `blake3:a1b2c3d4e5f6...` (64 hex chars)

### Rationale

Using the raw file bytes ensures:
1. **Simplicity**: Single pass over file bytes
2. **Security**: Cryptographically secure with BLAKE3

### Important Notes

- **ZIP Structure Dependency**: The hash is computed over the raw `.sync` file bytes, which is a ZIP archive. This means the hash depends on the exact ZIP structure, including file order and compression. Any re-packaging (e.g., re-compressing or reordering files) will change the hash.
- **Consistency**: The same build process must be used to ensure reproducible hashes.
- **Verification**: Signatures are valid only for the exact byte sequence of the signed file.

## Signature Format

Signatures are stored as JSON with the following schema:

```json
{
  "algorithm": "Ed25519",
  "public_key": "did:key:z6Mk...",
  "signature": "<base64-encoded-signature>",
  "content_hash": "blake3:<64-char-hex>",
  "signed_at": 1234567890
}
```

### Field Definitions

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `algorithm` | string | Yes | Signature algorithm. Always `"Ed25519"` |
| `public_key` | string | Yes | Signer's public key in `did:key` format |
| `signature` | string | Yes | Base64-encoded Ed25519 signature (64 bytes) |
| `content_hash` | string | Yes | BLAKE3 hash of `.sync` file. Must match actual file |
| `signed_at` | integer | Yes | Unix timestamp (seconds since epoch) |

### Public Key Format (did:key)

- **Format**: `did:key:z6Mk<multibase-base58btc-ed25519-public-key>`
- **Multicodec code**: `0xed` (Ed25519 public key, varint encoded as `0xed 0x01`)
- **Encoding**: Base58btc with `z` multibase prefix
- **Key length**: 32 bytes (after decoding)

**Multicodec Encoding Details:**
- Ed25519 public key uses multicodec code `0xed`
- In varint encoding, `0xed` becomes two bytes: `0xed 0x01`
- The full decoded bytes are: `[0xed, 0x01, <32-byte-public-key>]`

Example: `did:key:z6MkhaXg...` (full 48+ chars)

## Verification Steps

### Step 1: Compute Content Hash

```rust
let bytes = std::fs::read("app.sync")?;
let hash = blake3::hash(&bytes);
let hash_hex = format!("blake3:{}", hex::encode(hash.as_bytes()));
```

### Step 2: Verify Content Hash

```rust
if hash_hex != signature.content_hash {
    return Err("Content hash mismatch");
}
```

### Step 3: Extract Public Key

```rust
// Parse did:key format
let did = signature.public_key;
if !did.starts_with("did:key:z") {
    return Err("Invalid DID format");
}

// Decode base58btc
let encoded = &did[9..]; // Remove "did:key:z"
let decoded = bs58::decode(encoded).into_vec()?;

// Verify multicodec prefix (0xed01 for Ed25519)
let (codec, key_bytes) = varint_decode::u64(&decoded)?;
if codec != 0xed01 {
    return Err("Invalid key type");
}

// Verify key length
if key_bytes.len() != 32 {
    return Err("Invalid key length");
}
```

### Step 4: Verify Signature

```rust
use ed25519_dalek::{VerifyingKey, Signature};

let verifying_key = VerifyingKey::from_bytes(&key_bytes)?;
let sig_bytes = base64::decode(&signature.signature)?;
let signature = Signature::from_bytes(&sig_bytes.try_into().unwrap());

// Verify signature against hash bytes
let hash_bytes = hash.as_bytes();
verifying_key.verify(hash_bytes, &signature)?;
```

## Signing Process

### Step 1: Generate Key Pair (if needed)

```rust
use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;

let signing_key = SigningKey::generate(&mut OsRng);
let verifying_key = signing_key.verifying_key();
```

### Step 2: Convert to did:key

```rust
use unsigned_varint::encode as varint_encode;

// Encode multicodec prefix
let mut encoded = Vec::new();
varint_encode::u64(0xed01, &mut encoded);

// Append public key bytes
encoded.extend_from_slice(&verifying_key.to_bytes());

// Base58btc encode with 'z' prefix
let did = format!("did:key:z{}", bs58::encode(&encoded).into_string());
```

### Step 3: Sign File

```rust
// Compute hash
let bytes = std::fs::read("app.sync")?;
let hash = blake3::hash(&bytes);

// Sign hash bytes
let signature = signing_key.sign(hash.as_bytes());

// Create signature JSON
let sig_json = serde_json::json!({
    "algorithm": "Ed25519",
    "public_key": did,
    "signature": base64::encode(signature.to_bytes()),
    "content_hash": format!("blake3:{}", hex::encode(hash.as_bytes())),
    "signed_at": std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs()
});
```

### Step 4: Store Signature

Signature can be stored:
1. **Separate file**: `app.sync.sig` (JSON format)
2. **Embedded in .sync**: Inside the ZIP as `.signature` entry
3. **Registry metadata**: Sent alongside upload

## Registry Upload Flow

1. **Client** computes BLAKE3 hash of `.sync` file
2. **Client** creates signature JSON with hash
3. **Client** sends to Registry:
   - `.sync` file (binary)
   - `signature` (JSON)
   - `metadata` (name, description, category)
4. **Registry** verifies:
   - Signature is valid
   - Content hash matches file
   - Public key is valid `did:key`
5. **Registry** stores app with signature metadata

## Trust Levels

| Level | Indicator | Meaning |
|-------|-----------|---------|
| **Verified** | ✓ Green badge | Signature valid, author known |
| **Unverified** | ⚠ Yellow badge | No signature or invalid |
| **Unknown** | ? Gray badge | Signature valid but author not recognized |

## Security Considerations

1. **Key Storage**: Private keys must be stored securely (keyring/OS store)
2. **Replay Protection**: `signed_at` timestamp prevents replay attacks
3. **Hash Collision**: BLAKE3 provides 256-bit security against collisions
4. **Trust On First Use**: Users should verify author DID on first install

## Implementation References

- **BLAKE3**: https://github.com/BLAKE3-team/BLAKE3
- **Ed25519**: https://ed25519.cr.yp.to/
- **did:key**: https://w3c-ccg.github.io/did-method-key/
- **Multicodec**: https://github.com/multiformats/multicodec

## Version History

- **v1.0** (2026-02-03): Initial specification for Ato AppSync MVP
