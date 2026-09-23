# Stalwart + Missive staging acceptance — 2026-09-20

This record covers the private staging acceptance for one durable Ato Instance
containing Stalwart, Govcraft Missive, and a generic fixed-target transport
Adapter. It does not approve production, Discover, Public Try sending, public
SMTP, direct MX delivery, or a migration of existing mail data.

## Result

The clean reproducible bundle passed hosted validation and runtime Contract
verification, local authenticated delivery, IMAPS retrieval, the Missive JMAP
UI, the controlled outbound relay, and a normal owner stop/restart without
re-entering its secret Binding. The same fixed TCP allocation IDs moved from
the stopped Run to the replacement Run with incremented generations, while
the Stalwart volume and the previously delivered message remained intact.

A checkpoint was created and a post-checkpoint message was then written and
verified. Restoring that checkpoint removed the later message, retained the
checkpointed message, and installed an external-effect hold. `Keep running`
did not release the hold: the fixed addresses remained configured, but both
listeners returned EOF and no workload was running. Releasing the exact
operation/generation restored TLS, IMAPS, Missive, and controlled relay access.
The final acceptance Run was stopped, its bindings were detached, its fixed
TCP and egress grants were revoked, and the temporary relay/probe resources
were removed.

Production was not changed.

## Immutable inputs

The accepted local bundle is
`.tmp/stalwart-missive-mail-verify.capsule` in the acceptance worktree:

| Field | Value |
| --- | --- |
| Bundle SHA-256 | `aac707884bdcdf5e2fb1d39d4666fd7ff9c66b916cb3b368b0888bab39bd10f5` |
| `ContractRef` | `sha256:6f95b8360224e8dc0fc7871d7d3a42ab31f2e5ebdba7db25a7d56695f14e72c5` |
| selected `DerivationRef` | `sha256:49d27dfc7abf77e1479244718024f69036fac58bd1d9c0a99e567d2368c1963f` |
| workspace artifact | `sha256:514776aa45b9c3e0b1032fdb5ccaa64c90793f66e2ceee70b22cacadcc1b7a4b` |
| state seed | `sha256:0d1b1f76883afa58e8042fda55440f48dfb51c41fe9599d610cc7e55b7ad4fe2` (1,976,320 bytes) |

The bundle was rebuilt after removing generated Python bytecode; the earlier
acceptance draft containing `__pycache__` is not the final artifact. A source
tree scan found no `.pyc`, private key, password file, or Adapter `target/`
output in the committed fixture.

The three admitted Linux amd64 images are pinned by digest:

- Stalwart fixture image:
  `ghcr.io/koh0920/stalwart-staging@sha256:d0a846d7ab2749baa5bcd15a56b51f90ba99aea0c3256ce04d4f7e5e3076e8e0`;
  its binary comes from Stalwart `v0.16.22`, commit
  `474dd0229cb20cf513036619781ed97bd8073c3f`, whose upstream amd64 manifest is
  `sha256:d898e81b67b9f0f989b2aaec05f4c36fe9faa18fd570f8303ccef05c6127cdfe`.
- Govcraft Missive fixture image:
  `ghcr.io/koh0920/govcraft-missive@sha256:f2dd0d893a3fbf83812e35df3510d3fbc9846e9dd31a8a84899cf5fa818682fe`,
  built from commit `7716106b6c4638426169dac264657437914bbe7f` plus the two checked-in
  compatibility patches.
- Generic transport Adapter:
  `ghcr.io/koh0920/ato-fixed-socks5-tls-adapter@sha256:868ae56c18a004e7e4cc0e8d0707364b9d28242b914ec25f18f3fe1ca3a6aabf`.

The images are preloaded on the dedicated staging Runner. The available GHCR
credential did not have `write:packages`, so this acceptance does not claim
that the two locally built images are currently pullable on another Runner.

## Staging deployment

The acceptance ran only on the connected Runner
`01KX0SWDPP2GA41NEXQXNDCC0D` (`ubuntu-sugamo-staging`). Its active process was
PID `1067165`, binary
`/usr/local/bin/ato-connected-realization-worker-7391dbee`, SHA-256
`60427a09c5624bfba13c94cc7858e87fc1be1a64757844a2ed0fd7a319712c12`.

API migration `0286_portable_binding_config.sql` is applied in staging. The
active API Worker version is
`ab1aaacb-ff61-44ba-a161-812c12ce25e0` (`safe deploy 0707f0a419a4`), and
`wrangler d1 migrations list DB --env staging --remote` reported no pending
migrations.

The fixture used only these operator-managed network grants:

- `stalwart.submissions`: `0.0.0.0:19465`, allocation
  `tcp_01M2XNKF5MJYA4NWYG7R3NXAFD`;
- `stalwart.imaps`: `0.0.0.0:19993`, allocation
  `tcp_01M2XNKF5M55FC14D1XVPY4N3P`;
- `smtp_egress`: `208.111.34.11/32:10000`, grant
  `egr_01M2XNKF5M4CXGREVV33FRVXAK`.

The sink required SMTP AUTH, accepted only `relay.test`, and did not deliver to
the public Internet. Stalwart had no DNS grant, no general port-25 grant, and
no direct-MX route. The Adapter verified TLS using external SNI
`oci-linux-test.tail934987.ts.net`; certificate verification was not disabled.

The allocation and seed mutations used audited D1 records because the admin
API was behind a Cloudflare Access reauthentication prompt. The guarded
transaction first verified the old unlaunched owner, exact generations,
bundle digest, Derivation, encrypted Binding envelope, and absence of new
allocations before revoking the old owner and inserting the new records.

## Hosted Contract evidence

The final clean import is:

| Field | Value |
| --- | --- |
| import | `pai_01M2XNJVV5QTMMN8HW8YRJB0CA` |
| Instance | `cinst_01M2XNJVT8BAJGNT2X8VZGFGZR` |
| host | `https://cinst-r5kwk3dlrqmocldc.stg-app.ato.run/` |
| schema | `csch_01M2XNJVSGNMS3XGQ5ZT5ZP9Y0` |
| initial Run | `run_01M2XNNN4HJTK2TPA28JXAAEJG` |
| initial lease | `01M2XNNN4HTBK2B39J87TKXB4D` |
| verification attempt | `pav_01M2XNNNWST14R1HRG1176WRGX` |

The receipt was generated by the hosted runtime authority with the exact
bundle, Contract, and Derivation above and `fully_satisfied = true`. The lease
reported `ready`; the two fixed TCP runtime bindings were active at generation
2, and the persistent slot
`isslot_01M2XNKF5MD2EEXVN3SH2NY8CN` had writer epoch 1.

## Application acceptance

### Initial Run

Authenticated Submission to `alice@ato-mail.test` produced subject
`Ato final clean bundle acceptance 2026-09-20`. The client verified TLS 1.3
and the `mail.ato-mail.test` SAN. Authenticated IMAPS retrieved the same From,
To, and Subject over TLS 1.3 with the same SAN. Govcraft Missive then signed in
through its private JMAP connection and displayed that message in Inbox.

The controlled relay started with three prior acceptance records. Submission
of `Ato final clean relay acceptance 2026-09-20` increased the count to four.
The sink recorded sender `alice@ato-mail.test`, recipient
`receipt@relay.test`, and message SHA-256
`3af09f926a1a84ffa15e5549d8c547ea04f5616e922f925f45df5d99f18a6cf1`.

### Normal owner stop and restart

The owner Stop requested the initial lease at
`2026-09-19T20:30:41.628Z`; the Runner acknowledged `stopped` at
`2026-09-19T20:31:01.410Z`. Both runtime TCP bindings were `detached`, and the
state slot no longer named an active writer before restart.

Run again created `run_01M2XNV9J0A8WHZ0RWWPFD3G2H` with lease
`01M2XNV9J0A9G2ZTN19XJDFHM9` without prompting for `relay_auth`. The state
writer epoch advanced from 1 to 2, and the same two allocation IDs moved to
active generation 3. IMAPS still retrieved the original message.

The restart relay message `Ato final clean relay restart acceptance
2026-09-20` increased the controlled sink from four to five records. It was
recorded with SHA-256
`13436d851b6c0f35d110bd3b8fa4fd699fb8b1118318fc7b65397a4512c066b7`.
This demonstrates that the API decrypted Instance/Contract-bound durable
configuration and issued a fresh one-time Runner secret grant; the plaintext
secret was not stored in the portable import row.

### Checkpoint, restore, and post-restore hold

Checkpoint creation stopped the application, captured 2,452,992 bytes, and
advanced the slot head to `isrev_01M2XNXA6NRSP1TMJESAERSTBB`, digest
`sha256:96afb75e7af2c685f0dbc3f5f5e297e3504bee38c16b2c0e62d672e3eea3372c`.
Opening the app created `run_01M2XNXTMABRARS981AWSSRFAP` with lease
`01M2XNXTMAE6723B75J0DB7Q2G`; the slot writer epoch was 4.

After that checkpoint, authenticated local Submission and IMAPS both verified
the new message `Ato final post-checkpoint mutation 2026-09-20`.

Restore operation `vop_01M2Y5YZH5MFZNT1M51RJV690B` restored that exact
checkpoint revision and succeeded at `2026-09-20T01:12:51.634Z`. The operation
created active recovery-hold generation 1. While that hold was active:

- the UI reported that the application remained stopped with network access
  off;
- selecting `Keep running` changed the availability policy but neither
  released the hold nor started a workload;
- both fixed listener addresses accepted TCP and immediately returned EOF,
  and no service-group container existed behind them.

The owner released the exact restore operation and generation at
`2026-09-20T01:15:21.138Z`. Run `run_01M2Y63KBT1RXWX374TAVSBVN6`, lease
`01M2Y63KBTDD70T9T47M8242CR`, then reached ready using the retained durable
Binding configuration. Both allocation IDs moved to active generation 5.
TLS 1.3 and the `mail.ato-mail.test` SAN verified again. Authenticated IMAPS
and Missive showed the checkpointed `Ato final clean bundle acceptance
2026-09-20` message, while the post-checkpoint mutation was absent.

The post-restore controlled relay message `Ato final post-restore relay
acceptance 2026-09-20` increased the sink from five to six records and was
recorded with SHA-256
`452a8bedbf2dd364258243f4f1c4a1d9b32f74b23280e83e508dcf31b98845bc`.

### Acceptance cleanup

Availability was returned to on-demand and the post-restore lease stopped at
`2026-09-20T01:25:11.880Z`. The open viewer briefly acquired one final
on-demand Run, `run_01M2Y6NTH5GKPSYJP0H2TGP7XM`, which was also stopped; its
lease stopped at `2026-09-20T01:25:47.468Z`. Both proxy bindings are `stopped`,
and the state slot has writer epoch 7 with no active writer.

After those stop fences were observed, a guarded staging-only D1 transaction
revoked both fixed TCP allocations at generation 7 and the egress grant at
generation 2. Audit event `aae_stalwart_v9_cleanup_20260920` records the exact
before/after generations. The Runner keeps its allowlisted listener sockets
open, but a connection to either revoked address immediately received EOF.

The five named manual probe containers, their anonymous volume and dedicated
network, the temporary Runner build directory, the controlled relay process,
and its credential/certificate directory were removed. The public raw-TCP
handler for port 10000 was removed; the unrelated existing HTTPS handler on
443 remains. Tailscale retains an inert `AllowFunnel` marker for port 10000,
but its status has no TCP or Web handler for that port and the host has no
listener on 10000 or the former local sink port 2465.

Local generated PKI/private keys, bootstrap responses, bytecode/build output,
state-seed scratch files, and superseded bundle drafts were moved to Trash.
Only the final digest-matched `.tmp/stalwart-missive-mail-verify.capsule`
remains in the acceptance worktree's temporary directory.

## Local verification

- transport Adapter: `cargo fmt --check`, `cargo clippy -- -D warnings`, and
  all 3 unit tests passed;
- fixture helpers: Python bytecode compilation, JSON/TOML parsing, and
  `bash -n generate_test_pki.sh` passed;
- API: typecheck passed; 17 focused durable-Binding/network tests passed; the
  exact initial-import ciphertext test passed;
- the complete capsule network registry file had 22 passing and 3 unrelated
  pre-existing failures (public capacity, managed-pool error expectation, and
  zero-Binding continuation); no skip was added;
- ESLint was unavailable because the installed binary was missing.

## Remaining gates and limitations

- The Runner currently extracts the state seed as its host uid and does not
  provide an id-mapped OCI state mount. The fixture therefore uses a thin
  Stalwart uid-1000 image while retaining the pinned upstream binary.
- The current raw fixed-TCP forwarder does not preserve client source IP.
  Public exposure remains blocked until authenticated Proxy Protocol emission,
  exact trusted-proxy ranges, spoof rejection, and log/policy evidence exist.
- Govcraft Missive's README and its latest license files disagree. This record
  makes no legal conclusion; publication requires upstream clarification or a
  separate license review.
- Checkpoints over 64 MiB, hard volume quota, host reboot, and dockerd stop are
  intentionally out of scope.
