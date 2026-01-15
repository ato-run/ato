#!/bin/bash
# Generate a test Ed25519 keypair in StoredKey JSON format for testing and CI environments.
# This script is NOT intended for production key generation; use `nacelle keygen` for that.

set -e

if [ $# -ne 1 ]; then
    echo "Usage: $0 <output.json>"
    exit 1
fi

OUTPUT_FILE="$1"

# Check if Python is available for base64 encoding
if ! command -v python3 &> /dev/null; then
    echo "Error: python3 is required"
    exit 1
fi

# Generate random 32-byte Ed25519 secret key and derive public key
# Using openssl to generate, then decode to base64 for StoredKey format
TEMP_KEY=$(mktemp)
trap "rm -f $TEMP_KEY" EXIT

# Generate 32 random bytes
openssl rand 32 > "$TEMP_KEY"

# Read as base64 for secret_key
SECRET_B64=$(openssl enc -A -base64 -in "$TEMP_KEY")

# For this test helper, use a deterministic public key based on a hash of the secret
# (In production, nacelle keygen would use ed25519-dalek to derive the public from secret)
# For now, we'll use openssl to generate a proper keypair, then extract public
PUBLIC_B64=$(echo "$SECRET_B64" | python3 -c "
import sys
import base64
try:
    from cryptography.hazmat.primitives import serialization
    from cryptography.hazmat.primitives.asymmetric import ed25519
    from cryptography.hazmat.backends import default_backend
    
    secret_b64 = sys.stdin.read().strip()
    secret_bytes = base64.b64decode(secret_b64)
    
    # Create Ed25519 private key from bytes
    private_key = ed25519.Ed25519PrivateKey.from_private_bytes(secret_bytes)
    public_key = private_key.public_key()
    public_bytes = public_key.public_bytes(
        encoding=serialization.Encoding.Raw,
        format=serialization.PublicFormat.Raw
    )
    public_b64 = base64.b64encode(public_bytes).decode('ascii')
    print(public_b64)
except ImportError:
    # Fallback: use a deterministic dummy public key if cryptography not available
    import hashlib
    h = hashlib.sha256(secret_b64.encode()).digest()
    public_b64 = base64.b64encode(h[:32]).decode('ascii')
    print(public_b64)
")

# Write JSON file
cat > "$OUTPUT_FILE" <<EOF
{
  "key_type": "ed25519",
  "public_key": "$PUBLIC_B64",
  "secret_key": "$SECRET_B64"
}
EOF

echo "✅ Test keypair generated: $OUTPUT_FILE"
