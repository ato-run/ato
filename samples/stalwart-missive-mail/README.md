# Stalwart + Missive staging fixture

This fixture is the closed staging acceptance for one Ato Instance containing
Stalwart, Govcraft Missive, and one generic fixed-target transport Adapter.
Missive is the only Web Surface. Only Stalwart receives the persistent state
slot. This is not a public SMTP service or a production mail configuration.
Missive reads its non-secret internal JMAP endpoint from the explicit
`missive-config.toml`; the pinned upstream commit documents an environment
variable that does not populate its custom configuration in this build.

## Fixed inputs

- Stalwart `v0.16.22`, source commit
  `474dd0229cb20cf513036619781ed97bd8073c3f`, upstream amd64 manifest
  `sha256:d898e81b67b9f0f989b2aaec05f4c36fe9faa18fd570f8303ccef05c6127cdfe`.
- Govcraft Missive commit `7716106b6c4638426169dac264657437914bbe7f`,
  with two included narrow patches. One removes a non-default port from the
  redirect-host allowlist; without it, the pinned JMAP client rejects the
  relative `/.well-known/jmap` redirect for `stalwart:8080` as an unknown host.
  The other adds the advertised JMAP Submission capability to `Identity/get`;
  Stalwart correctly rejects that method when the capability is absent.
- The transport Adapter is not an SMTP implementation. It accepts plaintext
  SMTP only from Stalwart on the isolated service network, opens only the
  numeric target embedded in `capsule.toml` through `ato.tcp-egress@1`, and
  verifies the configured external TLS identity.
- The controlled relay sink requires AUTH and accepts recipients only in
  `relay.test`. It never delivers to the public Internet.

The two locally built images and the thin Stalwart uid image are preloaded on
the dedicated staging Runner. The Stalwart layer removes the upstream
`cap_net_bind_service` file xattr because the Runner deliberately combines
`cap-drop=ALL` with `no-new-privileges`; the executable bytes remain those of
the pinned upstream image. The images are intentionally not claimed to exist
in GHCR: the available credential did not have `write:packages`.
Consequently the fixture is not portable to another Runner yet.

When building Missive from the pinned checkout, copy
`missive-nondefault-port.patch` into `ato-patches/` in that checkout and use
`Dockerfile.missive`. The image label records both the upstream revision and
the applied compatibility patch.

## Staging-only state seed

Stalwart's dynamic registry lives in `/var/lib/stalwart`. The acceptance seed
contains the bootstrapped domain, password hashes, an environment-variable
reference for relay authentication, the test certificate, and no plaintext
relay password. The seed is installed as the Instance's initial State Revision
before its first Run.

The current Runner unpacks a seed as its host uid and deliberately discards
archive ownership. It does not yet provide an id-mapped OCI state mount. The
thin `Dockerfile.stalwart` therefore selects uid `1000` while retaining the
exact pinned upstream binary. This is a recorded staging limitation, not a
portable application contract.

Generate acceptance-only certificates with:

```sh
./generate_test_pki.sh .tmp/pki
```

Only the CA certificate enters the workspace. Private keys remain in `.tmp`
or in the Stalwart state and must not be committed.

## Build checks

The manifest pins the thin image digest preloaded on the target Runner. Build
the portable bundle with:

```sh
cargo run -p ato-cli --bin ato -- pack samples/stalwart-missive-mail \
  --output .tmp/stalwart-missive-mail.capsule
```

The imported Instance also needs one operator-managed `smtp_egress` grant for
the exact `/32` and port in `capsule.toml`, plus the runtime `relay_auth`
secret. No unrestricted egress is used.
