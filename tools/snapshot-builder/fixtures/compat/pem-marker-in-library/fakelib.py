"""A stand-in for the ssh/crypto libraries whose string-constant tables carry
PEM format markers (the #932 finding-4 false-positive shape). Not a key."""

PEM_HEADER_MARKERS = (
    "-----BEGIN RSA PRIVATE KEY-----",
    "-----BEGIN OPENSSH PRIVATE KEY-----",
)


def looks_like_pem(text: str) -> bool:
    return any(m in text for m in PEM_HEADER_MARKERS)
