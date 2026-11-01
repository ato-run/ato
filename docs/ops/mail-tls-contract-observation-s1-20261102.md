# `ato.contract.tls@1` — S1 implementation record (2026-11-02)

Status: implemented in the `ato` repository, verified by local build/tests.
Not deployed, not run against staging or production. Language: en.

This is slice S1 of the self-hosted mail plan (plan PR ato-run/ato#1665):
a product-agnostic observation of the certificate a public port presents, so
K5 ("every public port presents a valid certificate for the declared host
name") can be decided by the runtime instead of by reading exported files.

## What the Contract can now say

```toml
[[contract.observation]]
id = "imaps"
use = "ato.contract.tls@1"
port = "stalwart.imaps"      # a logical Port the Derivation declares
server_name = "mail.ato.run" # lowercase DNS name the certificate must cover
start = "implicit"           # or "smtp-starttls"
```

The same keys are accepted by the v1 grammar (`[[contract.require]]`) and the
v2 grammar (`[[contract.observation]]`). Only `port`, `server_name` and
`start` are accepted; request fields, expectations and captures are refused.

K states what must hold for every valid run: chain to a public root, SAN
covering `server_name`, observation time inside validity. The leaf serial is
**not** in K — it changes on every renewal — it is recorded in receipt evidence
and compared with the current `ato-cert-export` record by the ops procedure.

## Implementation

| Piece | Where |
|---|---|
| Verifier id, draft/bound types, server-name validation | `lib/formation/src/authoring.rs` |
| v1 grammar | `lib/formation/src/capsule_toml.rs` |
| v2 grammar | `lib/formation/src/capsule_toml_v2.rs` |
| Formation deferral, runtime verdicts, receipt evidence | `lib/formation/src/verify.rs` |
| Receipt /3 and its acceptance checks | `lib/formation/src/receipt.rs` |
| TLS probe (rustls, public roots, implicit + SMTP STARTTLS, DER facts) | `lib/tls-observe` |
| CLI/local attempt observation | `lib/runtime-attempt/src/{verification,attempt}.rs` |
| Local OCI service-group endpoints | `apps/portable-application/src/portable_run.rs` |
| Hosted validator observation | `apps/portable-application/src/validator_agent.rs` |

Behavior:

- Formation defers a TLS requirement to the runtime ("runtime tls verifier
  <id>"); it never satisfies one.
- The runtime connects to the endpoint its logical Port resolved to, performs
  the handshake (implicit, or SMTP 220 → EHLO → STARTTLS → 220), and records
  the presented serial, SAN dNSNames, notBefore/notAfter, days remaining,
  chain length, trust-anchor label and whether the certificate validated.
- Verdicts fail closed with distinct codes: `tls_observation_missing`,
  `tls_observation_mismatch`, `tls_server_name_mismatch`,
  `tls_certificate_outside_validity`, `tls_chain_untrusted`.
- A certificate that was presented but fails a check is still recorded, so the
  receipt says which certificate was served.
- Receipts carrying certificate evidence use
  `ato.contract-verification-receipt/3`; /1 and /2 are still accepted and
  unchanged for receipts without it.
- PROXY v2 is not handled by the verifier: the Runner's fixed-TCP listener
  inserts it toward the workload, exactly as it does for a remote client.

## Verification performed

Local worktree `.tmp/wt/mail-plan`, branch `feat/mail-tls-contract-observation`
(base `origin/stg` `e2e6c3386`):

- `cargo test -p ato-tls-observe` — 16 unit + 8 integration tests (real local
  TLS/STARTTLS servers, committed self-signed fixtures, public-roots rejection,
  expired and wrong-name certificates).
- `cargo test -p ato-formation --lib tls` — binding, grammar (v1 + v2),
  identity stability, deferral, runtime verdicts, receipt /3 and evidence.
- `cargo test -p ato-runtime-attempt tls` — TLS verdicts and receipt facts.
- `cargo test -p ato-portable-application --lib tls` — the authored v2 bundle
  carries the TLS requirement into K and refuses an undeclared Port.
- `cargo check --workspace --all-targets`, `cargo clippy -D warnings` for the
  touched crates, `cargo fmt --all`.

No staging or production host was touched.

## Not in this slice (open)

1. **Hosted transport (ato-api).** The validator job now accepts an optional
   list of public fixed-TCP endpoints per logical Port, but the API does not
   supply it yet and accepts only receipt /1. Until that follow-up lands, a
   TLS requirement on a hosted Run is unobserved and fails closed.
2. **K5 serial comparison with the export.** The observation records the
   presented serial; comparing it with `ato-cert-export`'s `meta.json` belongs
   to S2/S5 ops.
3. **Fixed-TCP public ports on staging.** S1 was verified against local
   listeners; the real public-port path (fixed TCP 25/465/587/993 with PROXY
   v2 on the dedicated Runner) is S3's acceptance.
4. **443.** K5 for HTTPS can be observed as an implicit TLS handshake on the
   Port; the HTTP response itself stays with `ato.contract.http@1` (K3).
