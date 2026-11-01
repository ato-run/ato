# TLS probe test fixtures

TEST-ONLY self-signed material. Every certificate here is generated locally
with the openssl CLI, is not issued by any CA, and must never be trusted by
production code. The probe only accepts them through TrustAnchors::TestOnly.

All three certificates use an ECDSA P-256 key in PKCS#8 PEM (the "PRIVATE KEY"
label), SHA-256 signatures, and a subjectAltName dNSName extension. They are
self-signed (issuer == subject), so each one can also act as its own trust
anchor in the tests.

Generated with OpenSSL 4.0.3 using exactly these commands, from the
repository root after creating the directories:

## valid/ (serial 0x1234abcd, valid 2026-01-01 to 2046-01-01)

    cd lib/tls-observe/tests/fixtures/valid
    openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
      -keyout key.pem -out cert.pem -sha256 \
      -subj "/CN=mail.ato.test" \
      -set_serial 0x1234abcd \
      -not_before 20260101000000Z -not_after 20460101000000Z \
      -addext "subjectAltName=DNS:mail.ato.test,DNS:mail.example" \
      -addext "basicConstraints=critical,CA:FALSE" \
      -addext "keyUsage=critical,digitalSignature" \
      -addext "extendedKeyUsage=serverAuth"

## wrong-name/ (serial 0x00c0ffee, valid 2026-01-01 to 2046-01-01)

    cd lib/tls-observe/tests/fixtures/wrong-name
    openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
      -keyout key.pem -out cert.pem -sha256 \
      -subj "/CN=other.invalid" \
      -set_serial 0x00c0ffee \
      -not_before 20260101000000Z -not_after 20460101000000Z \
      -addext "subjectAltName=DNS:other.invalid" \
      -addext "basicConstraints=critical,CA:FALSE" \
      -addext "keyUsage=critical,digitalSignature" \
      -addext "extendedKeyUsage=serverAuth"

## expired/ (serial 0x5555, valid 2020-01-01 to 2021-01-01)

    cd lib/tls-observe/tests/fixtures/expired
    openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
      -keyout key.pem -out cert.pem -sha256 \
      -subj "/CN=mail.ato.test" \
      -set_serial 0x5555 \
      -not_before 20200101000000Z -not_after 20210101000000Z \
      -addext "subjectAltName=DNS:mail.ato.test" \
      -addext "basicConstraints=critical,CA:FALSE" \
      -addext "keyUsage=critical,digitalSignature" \
      -addext "extendedKeyUsage=serverAuth"

## Why these facts

- valid/cert.pem is the end-entity the tests serve; TestOnly(valid/cert.pem)
  makes it its own trust anchor, so a chain with no intermediates validates.
- wrong-name/ has a SAN that covers nothing the probe asks for, so
  server_name_matches is false while the chain is otherwise fine.
- expired/ is outside its validity window at any plausible test time, so
  within_validity is false and days_remaining is negative.
- serial 0x00c0ffee has its top bit set, so its DER INTEGER content is
  00 c0 ff ee; the probe reports the raw contents as lowercase hex
  (00c0ffee), which is what makes the serial expectation meaningful.

The private keys are committed only so the loopback test servers can complete
a handshake; they protect nothing.
