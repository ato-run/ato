# ADR-044 — Certificate Binding with automatic renewal

Status: Draft. Design plus a local tool. Nothing here is implemented in
`ato-api` and nothing is installed or deployed. Language: en.

Scope: slice S2 of the self-hosted mail plan (plan PR ato-run/ato#1665, "G2 —
host-managed certificate, delivered as a secret Binding"). It designs how a
host-managed certificate and its private key reach Stalwart as two secret
Bindings, how a renewal reaches a new Run **without a human re-authenticating on
every renewal**, what is validated before a pair may replace the running one,
and what the operator compares to prove that the new Run actually serves it.
It adds no Kernel feature, no Capsule format change and no mail-specific
semantic noun.

## Problem

D1 keeps the host Caddy as the certificate source and runs Stalwart as the
Capsule's component. Caddy renews on its own schedule (about 30 days before
expiry, and it may renew at any hour); Stalwart needs the current certificate
and key at process start. The product requirements are:

1. The certificate and the private key reach Stalwart as **two `ato.secret@1`
   Bindings**, read through Stalwart's `EnvironmentVariable` configuration
   reference. No host directory mount, no new Runner capability.
2. **Automatic renewal is the top requirement.** Caddy renews; a root oneshot
   detects the change; the new values replace the durable Binding config; an
   owner re-run starts a new Run that picks them up.
3. The owner re-run must **not** require a human to authenticate on every
   renewal. Whatever acts for the owner must be a credential that can update
   **only** the allowlisted (Instance, binding ids, domain) pair, with an
   expiry, rotation and an audit record.
4. Nothing replaces the running pair until the candidate has been validated:
   key matches certificate, SAN covers the allowlisted domain, validity window
   sane, `notAfter` warning threshold recorded.
5. The plaintext private key must never be logged, put in a Receipt, printed by
   the exporter, or delivered to another Instance.
6. A failed export, validation or update leaves the **running** certificate in
   place.
7. Acceptance evidence is a reflection proof: after the new Run is ready, the K5
   observation records the presented serial, and ops compares it with the
   export record.

## Audit basis (what exists at `origin/stg` `e2e6c3386`)

Verified in this worktree and in the `ato-api` worktree at
`feat/mail-tls-hosted-observation` (`52f2aaf2`, whose base `main` already
contains the refreshable-Binding path):

| Piece | Where | State |
|---|---|---|
| Binding declaration, per-step injection | `lib/formation/src/authoring.rs`, `apps/portable-application/src/lib.rs` (`resolve_application_bindings`, `binding_environment_name`) | Existing. Value limit 64 KiB, NUL rejected; a multi-line PEM is **already legal here**. |
| Env name shape | `binding_environment_name` → `ATO_BINDING_<ID uppercased>` | Existing. Only the two OpenAI embedding protocols are special-cased. |
| Durable Binding config | `portable_application_imports.binding_config_encrypted`, AAD `["portable-instance-binding-config", instanceId, parentRoot]`, `capsule_binding_grant.ts` | Existing, encrypted at rest, Instance/Contract-bound. |
| Per-Run one-time grant | `runner_leases` `POST /:id/runtime-bindings`: `DELETE … RETURNING` the single row, AAD `[runId, leaseId, parentRoot]` | Existing, one-time by construction. |
| Runner secret boundary | `ResolvedSecret` (no `Serialize`, redacting `Debug`, single `expose_for_spawn` accessor) in `lib/runtime-attempt/src/launch/resolved.rs` | Existing. |
| Refreshable secret path | `ato.secret-refresh@1`, `portable_binding_update_grants`, `binding_config_generation`, route `PUT /v1/runtime/binding-updates/:grantId` | Existing in `ato-api` `main` (migration `0294`). Run-scoped capability delivered **inside the workload's Binding value**; it updates the durable config and rotates its token, and it does **not** start a Run. |
| Owner start / stop | `POST /compute-instances/:id/start` → `ensureReadyRoute` (single-flight wake; re-reads and opens the durable config), `POST /compute-instances/:id/stop` → `stopInstanceRuntime` | Existing, owner session + `apps:write`. |
| Operator surface | `/v1/operator` group (`requireAdmin()` + `ato_session`) and `/v1/internal` (`MANAGED_RUNNER_ADMIN_TOKEN`) | Existing; interactive admin session or one static service token. Neither is a per-Instance credential. |
| K5 observation | `ato.contract.tls@1` and receipt `/3` evidence (serial, SAN, validity, chain, anchor) | S1, unmerged (`feat/mail-tls-contract-observation`, ato#1666). **This design depends on it.** |

## Detected blocker: the OCI environment channel cannot carry a multi-line value

This is the one thing that does not work today, and it is a gap between two
existing rules rather than a missing capability.

- The authoring and API layers **allow** a Binding value with newlines:
  `resolve_application_bindings` rejects only empty values, `> 64 KiB` and NUL
  (`apps/portable-application/src/lib.rs`).
- The OCI adapter **refuses** them: `OciSpec` validation asserts
  `!value.contains(['\0', '\n', '\r'])` with `"OCI environment is invalid"`,
  and the launch writes `NAME=VALUE\n` lines to a file passed as `--env-file`
  (`extensions/adapters/oci/src/lib.rs`). A PEM certificate or key always has
  line breaks, so a service group fed two PEM Bindings fails at admission.

Because it fails closed, this is safe today; it is also why "certificate as an
`ato.secret@1` Binding" cannot reach D1 without one change. Docker's
`--env-file` has no escaping or quoting, so the value cannot be encoded into
that file.

**Required ato change (small, no new capability).** Deliver environment values
that cannot be represented in `--env-file` through the docker client's own
environment instead:

1. Validation keeps rejecting NUL and `\r`, and stops rejecting `\n`.
2. The staged environment file carries only names whose value has no line break.
3. For each remaining name the argv gains `--env NAME` **without a value**
   (the docker CLI then propagates `NAME` from its own environment), and the
   spawned docker client gets that variable set explicitly — `DockerClient::command`
   calls `env_clear()` when a docker owner socket is configured, so the
   variable must be set on the child, not inherited.
4. Tests: argv carries `--env NAME` and no value; the child environment carries
   the value; a value with NUL or `\r` is still refused; a single-line value
   still goes through the file.

Why this is not the "new Runner feature" the plan excludes: no mount is added,
no new Binding protocol, no new Capability, no Capsule-format change, and the
contract that already accepts the value is unchanged. It is the existing
environment channel being made able to carry what the existing declaration
already permits. Exposure is the same class as today's 0600 environment file:
the value is in the docker client process's environment for the launch window
and never in argv.

**Alternative considered and rejected:** base64-encoding the PEM and decoding it
inside Stalwart. Stalwart's `Certificate.certificate`/`privateKey` accept
`Text`, `File` or `EnvironmentVariable` with no decoding step, so this would
move the decoding problem into the workload and add a mail-specific convention.
**Also rejected:** writing the pair under the workspace mount or a new Runner
mount — the withdrawn option (b) of the plan, and it would put key material into
an artifact tree that the artifact guard scans for exactly those bytes.

## Decision 1 — two `ato.secret@1` Bindings, declared by D

The Derivation declares the two values; Stalwart reads them from its own
environment through its configuration registry:

```toml
[[binding]]
id = "mail_tls_cert"
protocol = "ato.secret@1"
required = true

[[binding]]
id = "mail_tls_key"
protocol = "ato.secret@1"
required = true
```

The injected names are `ATO_BINDING_MAIL_TLS_CERT` and
`ATO_BINDING_MAIL_TLS_KEY` (derived, not authored), and the Stalwart
configuration references them:

```json
{ "certificate": { "@type": "EnvironmentVariable", "variable": "ATO_BINDING_MAIL_TLS_CERT" },
  "privateKey":  { "@type": "EnvironmentVariable", "variable": "ATO_BINDING_MAIL_TLS_KEY" } }
```

`Certificate.certificate` / `privateKey` accepting `EnvironmentVariable` is the
plan's implementation finding #2 (Stalwart v0.16.22). The pair is a runtime
input, never part of the Capsule: `ContractRef`, `DerivationRef`, the bundle
and the Receipt are unchanged by a renewal. The mail sample (S3) is what
authors these two bindings; this slice only decides the shape.

Consequences that follow from "runtime input, not identity":

- The values are stored only as the encrypted durable Binding config and
  delivered per Run through the existing one-time grant.
- A renewal is a **new Run** of the same Capsule, not a new Capsule.
- The running Run keeps the pair it started with. Nothing rewrites it in place.

## Decision 2 — automatic renewal without per-renewal owner re-authentication

### The shape

```text
Caddy (user caddy) ──renews──▶ its storage (never shared, never mounted)
ato-cert-export (root, hourly timer) ── reads only the allowlisted pair
      │ validates: key matches, SAN covers, window sane, notAfter threshold
      │ writes export dir: cert.pem, key.pem(0600), meta.json, *.prev
      ▼
operator renewal grant (per Instance, hashed bearer token, expiry, rotation)
      │ POST /v1/operator/certificate-bindings/:grantId/apply
      │   expected_generation + values + evidence
      ▼
ato-api: verify credential → verify allowlist → verify value shape →
         seal durable config → generation CAS → audit →
         replace the Run through the existing owner start path
      ▼
new Run (env) ──▶ Stalwart  ──▶ K5 observation records the presented serial
```

### What must not be built

- A stored account session (cookie or device credential) for the root-side
  process: it is full-account authority and its issuance is interactive.
- A single static operator or service token (`MANAGED_RUNNER_ADMIN_TOKEN`
  style) with no Instance/domain scoping and no rotation.
- Any path where the workload rotates the secret it consumes. The
  `ato.secret-refresh@1` capability exists for a workload that holds a
  short-lived credential; a mail server cannot act as the renewal actor, and a
  compromised workload must not be able to name its own replacement key.

### Chosen: a per-Instance operator renewal grant (one new table, two endpoints)

New table `operator_certificate_binding_grants` (control plane, operator
created, one row per allowlisted pair):

| Column | Meaning |
|---|---|
| `id` | `opcbg_<ulid>`, the grant id the tool passes as `--grant-id` |
| `instance_id`, `owner_user_id` | exactly one Instance; the owner is recorded, not authenticated |
| `domain` | the single DNS name this grant may renew |
| `certificate_binding_id`, `private_key_binding_id` | the only two Binding ids it may write |
| `token_sha256` | SHA-256 of the bearer token; the token itself is never stored |
| `config_generation` | the durable Binding config generation the token was issued against |
| `expires_at`, `rotated_at`, `last_used_at`, `revoked_at` | lifetime and use record |
| `created_by` | the admin session that issued it |

Endpoints:

- `GET /v1/operator/certificate-bindings/:grantId` — read-only status for the
  timer: current durable `generation`, the last applied serial and
  `notAfter`, and this grant's `expires_at`. It returns no Binding value and no
  certificate.
- `POST /v1/operator/certificate-bindings/:grantId/apply` — the update.

Both live beside the existing `/v1/operator` group but **not** behind
`requireAdmin()`: they authenticate the grant token instead. Issuing, rotating
and revoking a grant stays an admin-session action on the existing operator
surface (`requireAdmin()` + `ato_session`), so a host compromise cannot mint
itself a wider credential.

Request body (strict, unknown fields refused):

```json
{
  "schema": "ato.certificate-binding-update/1",
  "domain": "mail.ato.run",
  "expected_generation": 7,
  "values": { "mail_tls_cert": "<PEM>", "mail_tls_key": "<PEM>" },
  "evidence": {
    "serial": "0a1b…", "not_before": "…", "not_after": "…",
    "certificate_sha256": "…", "exported_at": "…"
  }
}
```

### The exact minimal `ato-api` change

1. **Auth check.** `Authorization: Bearer <token>`; compare
   `SHA-256(token)` with `token_sha256` in constant time (the same
   `tokenMatches` helper the refresh route already uses). Refuse when the row is
   missing, revoked, expired (`410`), or when `config_generation` no longer
   matches the durable config generation (the grant must be re-issued).
2. **Instance and binding allowlist.** Resolve the Instance from the grant, not
   from the body. Load the bundle's `validator_report`
   (`required_bindings`) and require that the body's value keys are exactly
   `{certificate_binding_id, private_key_binding_id}`, that both are declared,
   that both are `ato.secret@1`, and that the Instance is `active` with a
   `running`/`ready` lease, no stop request, and no active restore hold.
3. **Value shape checks (cheap, no X.509 parsing in the Worker).** Each value
   must be ≤ 64 KiB, free of NUL, a single PEM block with the expected header
   (`CERTIFICATE` / `PRIVATE KEY`), and the SHA-256 of the submitted
   certificate PEM must equal `evidence.certificate_sha256` (WebCrypto). The
   domain in the body must equal the grant's domain. The API records the rest of
   the evidence as **reported**; it does not re-run X.509 validation, because the
   Rust authority and the K5 observation are where certificate validity is
   decided, and a TypeScript X.509 parser would be a second authority.
4. **Durable config write.** Merge the two values into the existing map (read
   with `openPortableBindingConfig`, replacing only the two allowlisted ids) and
   seal it with `sealPortableBindingConfig` for
   `{instanceId, parentRoot}`. The write is a CAS:
   `WHERE binding_config_generation = expected_generation`, setting
   `+1`; zero rows changed is `409 certificate_binding_generation_conflict`.
   The seal, the generation bump and the audit row are one `DB.batch`: D1
   batches are atomic, so a failure mid-apply leaves the previous values and the
   previous generation exactly as they were, which is requirement 6.
5. **Audit.** One payload-free row per application:
   grant id, Instance, actor `operator:certificate-binding`, generation
   before/after, serial, `not_after`, certificate SHA-256, resulting run id,
   outcome, timestamp. No Binding value and no key digest.
6. **Run replace.** After the CAS commits, replace the Run through the same
   internal path the owner's `POST /compute-instances/:id/stop` and
   `/start` use (`stopInstanceRuntime`, then `ensureReadyRoute` with the
   Instance's owner as actor). The wake path already opens
   `binding_config_encrypted` before launching, so the new Run starts with the
   new pair. Fixed-TCP generation fencing hands the public ports over; the old
   Run is stopped first because two processes must not serve the same mail ports.
   The response is `{applied_generation, serial, not_after, run_id}`.
7. **Token rotation.** On success, replace `token_sha256` with a fresh random
   token and return it once (`next_token`), exactly as the refresh route
   rotates. The host overwrites its 0400 token file. A leaked old token is dead
   after the next renewal or at `expires_at`.
8. **Failure response.** If the CAS or the checks fail, nothing is written and
   the running Run keeps its pair. If the CAS succeeded but the Run replace
   failed, respond `502` with `applied_generation` and `run_id: null`: the
   durable config is ahead of the running Run, which is safe (the running
   certificate is still the previous, still-valid one) and the next wake uses the
   new pair. The timer alerts on a non-2xx and retries.

### What it must refuse

- Any value key other than the two allowlisted Binding ids, and any id that is
  not declared `ato.secret@1` in the bundle (this also rules out writing an
  operator-managed binding such as `ato.tcp-egress@1`).
- Any domain other than the grant's, and any Instance other than the grant's.
- `expected_generation` that is not the current generation.
- A stale, revoked, rotated or expired token; a token used for a different
  grant id (the hash comparison is per row).
- Values with NUL, over 64 KiB, without the expected PEM header, or whose
  certificate digest does not match the evidence.
- An Instance that is not active, whose lease is not running/ready, is pending a
  stop, or holds an active restore hold.
- Any request while a replacement started by a previous apply is still in
  flight for that Instance (single-flight: the second request loses the CAS).

### Existing paths evaluated

| Path | Verdict |
|---|---|
| `ato.secret-refresh@1` run capability (`portable_binding_update_grants`) | **Not reusable as the renewal actor.** The token is delivered inside the workload's Binding value, so the host cannot obtain it without asking Stalwart; the update does not start a Run; and it would let the consumer name its own replacement. Its *design* is reused: the same sealed config, the same generation CAS, the same rotation-on-use and constant-time hash comparison. |
| Owner session / device credential + `PUT /:bundleId/import-app` (which does seal owner-supplied bindings) and `POST /compute-instances/:id/start` | **Rejected for this requirement.** Everything needs an owner session, i.e. a human, or a long-lived full-account credential. It also cannot restart a running Run: `start` returns the existing ready route. |
| Admin session (`/v1/operator`) | **Rejected as the per-renewal credential** (an interactive admin session or Cloudflare Access; far too broad) but **kept for grant issuance/rotation/revocation**, which is the correct place for it. |
| `MANAGED_RUNNER_ADMIN_TOKEN` / `/v1/internal` | **Rejected.** One environment-wide static token, no instance/domain scoping, no rotation, no per-use audit. |

### Credential lifetime

- Default TTL 30 days, capped at 90; a timer alert at 7 days before expiry.
- Rotated on every successful apply; rotation is atomic with the config CAS.
- Revocation is `revoked_at` set by an admin session; the next request fails
  `403`.
- The token exists in plaintext in exactly two places: the API response that
  returns it once, and the host's root-only (0400) token file. It is never in a
  log, a Receipt, an export directory or a Git repository.

## Validation before any replacement

The exporter (`tools/cert-export`, implemented in this slice) performs, in this
order, and **before the first byte of the export directory changes**:

| Check | Rule | Refusal |
|---|---|---|
| Pair | the certificate's public key equals the private key's public key | `validation` |
| SAN | a dNSName entry covers the allowlisted domain (exact, or `*.parent` matching exactly one label) | `validation` |
| Validity | `notAfter > now` | `validation` |
| Expiry margin | `notAfter - now >= --min-remaining-hours` (default 48 h) | `validation` |
| Clock | `notBefore <= now + 5 min` | `validation` |
| Window | `notAfter - notBefore <= --max-validity-days` (default 400) | `validation` |
| Warning | `notAfter - now < --warn-remaining-days` (default 14 d) | recorded warning, still exported |
| Key shape | the key parses and is **unencrypted** (a passphrase-protected key is refused, not prompted for) | `tool` |

It then writes the export directory:

```text
<state-root>/<instance>/           0700
  cert.pem       the validated leaf, byte-identical to the source
  key.pem        0600
  meta.json      0600, written last, names the digests of the two PEM files
  cert.pem.prev  kept on change (the previous validated pair)
  key.pem.prev   0600
  meta.json.prev
```

`meta.json` records the leaf serial (lowercase hex), the dNSName SANs,
`notBefore`/`notAfter` (ISO-8601 UTC and the raw ASN.1 strings), subject,
issuer, the source SHA-256 of both files, the action, `exported_at` and the
previous record. It contains **no private key material**; the key digest is a
local change-detection fingerprint and is deliberately absent from the update
payload. Because the meta names the two PEM digests, an export directory left
inconsistent by an interrupted write (or edited afterwards) is refused instead
of being sent.

A pair whose source bytes are unchanged is a no-op: no write, no `*.prev`, no
Run start.

## Threat model

Assets, in order of severity: the private key; the operator renewal token; the
durable Binding config ciphertext; the audit record's integrity.

| # | Threat | Mitigation | Residual |
|---|---|---|---|
| T1 | Key leaks through a log, Receipt or stdout | The exporter prints key material only for `--print-binding-update`/`print-binding-update`, warns on stderr, and everything else is a journal-safe plan line. `ResolvedSecret` has no `Serialize` and a redacting `Debug`; the Runner's only exposure point is `expose_for_spawn`. Receipts never contain Binding values (K holds facts about the run, not secrets). | A shell redirection by the operator can still persist it; the runbook forbids it and the command says so. |
| T2 | Key leaks by being passed as an argument | `openssl` receives paths, never bytes; the empty passphrase `pass:` carries no secret; the docker CLI gets `--env NAME` without a value, so the PEM is not in argv. | `ps` shows file paths only. |
| T3 | Key read by another Instance | The value is sealed with AAD `[instanceId, parentRoot]`, so a ciphertext cannot be opened for another Instance; the per-Run grant is one-time (`DELETE … RETURNING`) with AAD `[runId, leaseId, parentRoot]`; the renewal grant names exactly one Instance and two binding ids. | A compromised control plane is out of scope (it can already re-seal). |
| T4 | Key read by the Runner host | The environment file is 0600 inside a Runner-owned runtime directory and removed as soon as `docker run` returns; with the required change the value is also in the docker client's environment for the launch window. `/proc/<pid>/environ` is readable by root and by the same UID. | Root on the host can always read it; the tool's privilege checks only keep *accidents* from happening. |
| T5 | Key read by other workloads | Only the Stalwart container receives the two variables. Every other service in the group gets its own environment; the Runner writes one environment file per container. | Any process inside the Stalwart container can read them, and Stalwart persists the pair in its own registry (the field is marked `secret`, so its API does not return it). This is the Instance-compromise boundary, not a key-loss boundary. |
| T6 | Cross-Instance or cross-binding write through the renewal credential | The API resolves the Instance from the grant, requires the two binding ids to be declared `ato.secret@1`, and refuses everything else. | A stolen token can still roll the allowlisted pair for its own Instance; the audit record and the serial mismatch alert are the detection. |
| T7 | An old certificate is silently kept or served | The K5 reflection proof (below) compares the presented serial with the export record after every renewal; a mismatch is an alert, and `notAfter < 14 d` is an alert even when nothing changed. | A hand-run renewal can skip the comparison; the runbook makes it a gate. |
| T8 | The exporter is tricked into reading another domain's key | It reads only the paths in a root-owned 0600 allowlist, one entry at a time; the file must be root-owned and root-only or the tool refuses to start. | Root compromise is out of scope. |
| T9 | The renewal token leaks into an image, layer or log | The token lives in one 0400 file outside the repository and outside the export directory; it is rotated on use and expires. | A root compromise of the host reads it, exactly as it reads the key. |

Root-only inventory on the host (anything on this list must be `root` and
`0600`/`0400`/`0700`, and no group or world access): the allowlist, the
renewal token file, the export directory and its `*.prev`, the systemd unit and
its script, and Caddy's own certificate storage.

Log hygiene rules this design depends on (each is a review checkpoint for the
implementation): never log `bindingValues`/`resolvedBindings`/`values` maps;
never log the `next_token` (only that it rotated); never put `cert.pem` or
`key.pem` contents in an error message; do not attach the request body to a
sentry/analytics event; the audit row carries identifiers and certificate facts
only.

## Reflection proof (the acceptance evidence)

K5 keeps saying only what K must say: a chain to a public root, an SAN covering
`server_name`, and the observation inside the validity window. The serial is
**not** in K (S1 decision), because it changes at every renewal.

After the new Run is ready:

1. The K5 observation on each public port records the presented leaf serial,
   SAN, `notBefore`/`notAfter`, chain length and trust-anchor label in the
   receipt `/3` evidence.
2. Ops compares, for every port:

   ```text
   receipt.evidence[port].serial      == meta.json certificate.serial
   receipt.evidence[port].not_after   == meta.json certificate.not_after
   receipt.evidence[port].trust_anchor is a public root
   ```

3. The same serial is recorded in the audit row written by the apply call, so
   the chain is: Caddy source → export meta → API audit → K5 receipt.
4. A mismatch is an incident: the certificate served on a public port is not
   the one this host exported, which means the new Run did not start with the
   applied values, or something else is listening.

This is evidence, not a Contract condition: K is unchanged by a renewal, and a
renewal cannot make a previously satisfied Capsule unsatisfied.

## Rollback

| Situation | Behaviour | Recovery |
|---|---|---|
| Validation fails | Nothing is written; the export directory and the running Run are untouched. | Fix the source (Caddy, DNS, clock); the timer retries. |
| Caddy renewal fails | The running Run keeps its pair; `notAfter < 14 d` is an alert. | Fix Caddy; export reruns on the next change. |
| Apply fails (CAS, auth, value shape) | The API writes nothing; the running Run keeps its pair. | Investigate; the timer retries. |
| Apply succeeds, Run replace fails | The durable config is ahead; the running Run still serves the previous, still-valid certificate. `502` with `applied_generation`; the next wake uses the new pair. | Retry the replace, or let the always-on policy do it. |
| New Run serves the wrong certificate | The K5 serial comparison fails. | Re-apply the previous pair (`*.prev`) and replace the Run; investigate why the workload did not pick up the values. |
| The previous pair is needed | `*.prev` holds it, root-only, next to the export. | A second root-only allowlist entry naming the `.prev` paths (in a separate rollback allowlist file and a separate export directory) re-validates and re-applies it. It is refused if it is expired or inside the expiry margin, which is correct. |

The old Run is never modified: a renewal is a switch to a new Run, so an
abandoned renewal leaves a fully working previous Run behind. That is what makes
"keep the RUNNING certificate when anything fails" true without an extra
mechanism.

## What this slice implements now, and what it does not

Implemented and verified locally in this worktree:

- `tools/cert-export` (`ato-cert-export`): the allowlist, validation, export
  directory, change detection, `*.prev` retention, and the
  `ato.certificate-binding-update/1` payload printer. 46 tests, `clippy -D
  warnings`, `fmt --check`. No network I/O.
- This document and `docs/ops/cert-binding-renewal-runbook.md`.

Not implemented, deliberately:

- The `ato-api` table, endpoints and Run-replace flow described above. They are
  specified here but not written; they are a separate `ato-api` change that
  depends on S1 (receipt `/3`) being merged and on the `ato` OCI environment
  fix.
- The `ato` OCI environment change (Detected blocker). Until it lands, D1
  cannot run with PEM Bindings at all.
- The S3 sample (`samples/ato-mail`): declaring the two bindings and pointing
  Stalwart's registry at the two environment variables.
- Anything on the host: no systemd unit, no timer, no token, no export
  directory is installed. No Caddy instance was read.

## Consequences

- Good: a renewal is unattended and end-to-end verifiable (Caddy → export meta
  → audit row → K5 receipt serial).
- Good: the credential is per-Instance, short-lived, rotated on use and audited,
  so a host compromise cannot widen it into another Instance or another binding.
- Good: no Capsule, Contract, Derivation or binding-protocol change; a renewal
  cannot change `ContractRef` and cannot make a satisfied Capsule unsatisfied.
- Good: the running Run is never mutated, so "keep the current certificate" is
  the default outcome of every failure.
- Bad: one small `ato` change is required first (the OCI environment channel),
  which means S2 spans two repositories.
- Bad: a renewal restarts the mail workload, so public ports are briefly down
  and remote MTAs must queue and retry; that is the accepted cutover behaviour
  and it is why the Run replace is the last step, after everything else passed.
- Bad: the private key is visible to every process inside the Stalwart container
  and is persisted in Stalwart's own registry, so the boundary is "the Instance
  is trusted", not "the key never leaves the control plane".
- Bad: the operator endpoint is new control-plane surface that must itself be
  reviewed (constant-time comparison, strict body, CAS, payload-free audit).

## Open decisions for review

1. **The OCI environment change.** Accept the small adapter change above
   (recommended) or choose another delivery. Without one, requirement 1 cannot
   be met for D1.
2. **Grant scope**: one grant per (Instance, domain) as designed, or per
   environment with several Instances. The design deliberately keeps it to one
   pair; a per-environment grant would widen the blast radius of a stolen token.
3. **Audit storage**: a new `certificate_binding_audit_events` table (proposed)
   or an extension of `capsule_access_audit_events`, which is post-scoped and
   would need a nullable post id.
4. **Status endpoint**: include `GET /v1/operator/certificate-bindings/:grantId`
   (proposed; lets the timer pass `expected_generation` instead of parsing a 409
   conflict).
5. **Rollback mechanism**: a second root-only allowlist file (proposed, uses
   only existing code) or a `--from-prev` flag in the tool.
6. **Always-on interaction**: whether the replace should temporarily set
   `desired_state = stopped` (as the owner's stop route does) or rely on the
   single-flight wake alone. Needs an S3 look at how a service-group Run with
   fixed TCP is started and stopped in practice.
7. **Grant TTL and rotation policy**: 30 days rotating on every apply
   (proposed) versus a longer TTL with a scheduled rotation that does not depend
   on a renewal happening.
