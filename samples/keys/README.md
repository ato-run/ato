# Test Keys

This directory contains **test/sample keys for development and CI environments only**.

⚠️ **These are not production keys. Do NOT use in production environments.**

## Usage

### Generate a new test keypair

```bash
./generate_test_key.sh e2e-test.json
```

This generates a fresh Ed25519 keypair in StoredKey JSON format (same format as used by `nacelle keygen`).

### Using in tests

Test keys can be referenced by CI workflows or local test scripts:

```bash
./generate_test_key.sh /tmp/test-key.json
cargo test -- --include-ignored --test-threads=1
```

## Format

Test keys are stored in JSON format matching the `StoredKey` struct from `src/capsule_types/signing.rs`:

```json
{
  "key_type": "ed25519",
  "public_key": "<base64-encoded-32-bytes>",
  "secret_key": "<base64-encoded-32-bytes>"
}
```

## Security Notes

- **e2e-test.sample.json**: A dummy placeholder (all zeros). Never commit real keys to this repository.
- **generate_test_key.sh**: Generates ephemeral test keys at runtime; output should NOT be committed.
- For production key generation, use: `nacelle keygen <name>`
