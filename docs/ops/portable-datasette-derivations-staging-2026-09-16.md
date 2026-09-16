# Datasette portable Python/OCI staging acceptance — 2026-09-16

Status: **four receipt assertions PASS; full UI acceptance remains BLOCKED**.
This is staging evidence only. No production deploy, merge, feature activation,
or durable-data migration was performed.

The requested predecessor record
`docs/ops/portable-multi-derivation-staging-2026-09-16.md` was not present in
the supplied worktree or its starting commit. The pushed starting points were
checked directly: ato `75d2de23`, API `c4dce9e3`, PWA `edfb7856`.

## Fixed inputs and identity

| Item | Value |
|---|---|
| OSS | Datasette 0.65.2, no plugin; upstream source commit `6a141467f9f8b05c84bc00c10e157962e3e43960`, Apache-2.0 |
| Bundle | `samples/datasette-cpu.capsule`, 7,000,939 bytes, SHA-256 `88e4fd8d7ec46db05347bd4b03cfb3ceb7598e31fadd233d2bb68bb55df860e6` |
| ContractRef / CapsuleId | `sha256:d4bb7e9ae1be0f6b884d0d58561d7f974092de447da3eea9e2ffe5279b004a19` |
| D_python | `sha256:94ab606ee88d41da3af332e72d5da151fd0920a3aadc9c8c94aefaa80518b21f` |
| D_oci | `sha256:d3e7428a6c75e7d5726319ca61f06631e1811a698b2dd16b531029038dfd701f` |
| Input DB | `samples/interop-datasette/catalog.db`, 8,192 bytes, SHA-256 `ed2d4584363f47a38e24fc5afa37dc9fa42ba6eda9c72cbeefb1f2a7cc0c8c17` |
| Python | declared 3.12; observed `/opt/ato/toolchains/python/3.12.7/bin/python3.12`, Python 3.12.7, Linux x86_64 |
| OCI | `docker.io/datasetteproject/datasette@sha256:0f57db16cf4eb6cca57f1cedaa0a696bca1c65a1d75b8f7ee372c2dd909a32a0`, `linux/amd64`, 85,174,869 bytes uncompressed |
| Host | Ubuntu 26.04 LTS, Linux 7.0.0-27-generic x86_64; Docker client/server 29.1.3 |

The `requirements.lock` fixes all Python packages and hashes, including
transitive dependencies, and both macOS arm64 and Linux amd64 wheelhouses are
inside the bundle. A fresh venv is populated offline with hash checking; the
host Python interpreter itself is an explicitly measured capability. The OCI
image bytes **are not bundled**: its compressed transport exceeds the existing
32 MiB upload limit. The image is pulled by immutable digest over the network.
Thus the four-path experiment is **network-dependent OCI**, not a proof of
fully bundled offline OCI portability. No upload limit was raised and no
alternate transport was used. Source/release/image provenance is also recorded
in `samples/interop-datasette/UPSTREAM.json`.

K is the original bundled Contract, not recaptured at execution time:

1. `datasette-entry`: GET `/` returns HTTP 200. The entire entry HTML is not
   hashed because its runtime-dependent output is not the data assertion.
2. `items-in-id-order`: GET `/catalog/items.json?_shape=array&_sort=id` returns
   HTTP 200 and body digest
   `sha256:8607a7b55d2f0b4f9169a1c63b1554a60a6af1c8424ab591aa9a451265e0ee83`
   for `[{"id": 1, "name": "A", "quantity": 2}, {"id": 2, "name": "B", "quantity": 5}]`.
3. `quantity-total`: GET
   `/catalog.json?sql=select%20sum(quantity)%20as%20total%20from%20items&_shape=array`
   returns HTTP 200 and body digest
   `sha256:b4d3392b69f97f35ad0c927a0401766b538ebbf1baabd4ee14f620b0ac54632f`
   for `[{"total": 7}]`.

The paths are dynamic Datasette endpoints, not names of bundled files. The
existing generic versioned HTTP verifier compares actual status/body digest;
there is no Datasette-specific verifier or substituted success server.

## Four receipts

All four use **the same 7,000,939-byte file**. The CLI copy on the compatible
Linux host had the same SHA-256 as the PWA upload. The PWA did not execute
Python or Docker; its selected DerivationRef was dispatched to the existing
Hosted Runner/lease. Receipts are in `.tmp/datasette-evidence/` in this
worktree:

| Path | Receipt file | Run / lease / attempt | Physical evidence |
|---|---|---|---|
| A CLI Python | `cli-python-linux-final-receipt.json` | local | PID `2835129`, Python 3.12.7 |
| B CLI OCI | `cli-oci-linux-final-receipt.json` | local | container `4e0316df7e81…`, digest-pinned image |
| C PWA Hosted Python | `hosted-python-receipt.json` | `run_01M2MENYFFZJ14DFVRXBB5GS75` / `01M2MENYM6A0YTJ36GGMSXRZVP` / `pav_01M2MENZ0PSN26M1BYKSX4V63Z` | PID `2837321`, slot 1 |
| D PWA Hosted OCI | `hosted-oci-receipt.json` | `run_01M2MFHB2PEVQ71KZQG0AK39QC` / `01M2MFHB9HW42F2HCD881H0NRQ` / `pav_01M2MFHBM3FB811J9VFW7HW83E` | container `19bbe40545ae…`, slot 0 |

Machine assertion on those four JSON files: one unique `bundle_sha256`, one
unique `contract_ref`, A.derivation_ref = C.derivation_ref = D_python,
B.derivation_ref = D.derivation_ref = D_oci, and D_python != D_oci.
Each has exactly the three K IDs above, each outcome `satisfied`, and
`fully_satisfied: true`; no deferred or absent observation was accepted.

CLI reproduction on the measured Linux host (the bundle and `ato` binary were
copied to `/home/ekohsuke/.ato-staging/.tmp/portable-datasette`):

```bash
cd /home/ekohsuke/.ato-staging/.tmp/portable-datasette
./ato run datasette-cpu.capsule --derivation sha256:94ab606ee88d41da3af332e72d5da151fd0920a3aadc9c8c94aefaa80518b21f --no-open --verification-receipt evidence/cli-python-linux-receipt.json
./ato run datasette-cpu.capsule --derivation sha256:d3e7428a6c75e7d5726319ca61f06631e1811a698b2dd16b531029038dfd701f --no-open --verification-receipt evidence/cli-oci-linux-receipt.json
```

The corresponding restart commands use the same file and different receipt
names; final and restart receipts and the two restart logs were copied to the
local `.tmp/datasette-evidence/`. The original CLI invocation logs were not
captured as files; their final receipts are preserved. Both restarted CLI
routes satisfied K, with new PID/container IDs. Use
`cargo run -p ato-cli --bin ato -- run ...` from a
source checkout; `--bin ato` is required because this package has three bins.

## Staging deployment and lifecycle

Executed source heads: ato `fab2888024aa072202d2d7fb3638c771496798f2`
(Linux CLI/Runner/validator binaries built from its runtime-code ancestor
`9c1b1475`); API `c545b2fa73a532007c31c91fc32682f1aec9dfae`
(active Worker runtime-code ancestor `7011f872`); PWA
`f503d773938b1efe570caa1cea085a456869b41f`.
The later ato commits add a regression test and AGENTS.md update; the later
API commit adds only the receipt backfill migration.

- Staging D1 migrations `0265`, `0266`, `0267`, `0268` applied. Current API
  Worker version `0f291d69-aa19-4dd7-a3cc-d9b340000260`, health 200.
- PWA Worker `54165d97-959d-45f8-b47d-fde01ccb0c1a`, served assets
  `index-DXP4MXZe.js` / `index-D3dVtnA9.css` were checked after deploy.
- Validator SHA-256 `93968b8a83ccbc06e7d54a14fa29545b75fc0d40b4e915672b57469e9bfa1f7e`.
  Runner SHA-256 `85bb8288fbb3733be8ec2ae928044238a76a06b1da2f279caeda3675d354ca39`
  for both active slot 0 and slot 1. The active slot 0 ExecStart initially
  pointed to an old August binary despite the new named binary being copied;
  this produced the first OCI `FORBIDDEN_FIELD` failure. Its exact old binary
  was backed up as `/usr/local/bin/ato-connected-realization-worker.before-9c1b1475`,
  the actual ExecStart path was updated, SHA checked, and the service restarted.
- First validation ACK stored a Base64 workspace in D1 and exceeded the
  practical row size; API `0e952777` instead verifies then stores the bytes
  once in R2, keeping only descriptors in D1. The exact failed claim was
  expired for retry. No observation or K was changed to make it pass.
- The first OCI failed attempt remains `pav_01M2MEX6CBHQNVYX363G54VYXJ`.
  API `1ba9c26d` provides a fresh Run/lease/attempt retry for a failed import.
  `7011f872` also permits a stopped verified Run to restart; `0267` records
  receipts per attempt and `0268` backfilled the prior success receipt.
- OCI stop request at `2026-09-16T06:51:18.881Z` was acknowledged at
  `06:51:19.653Z`; its container, network and lease workspace were gone.
  Concurrent Python continued to answer both data queries. The same bundle
  then relaunched OCI as `run_01M2MG0X712Y68J2GT0GZ0EBMP`, lease
  `01M2MG0XDE9RCHQ9SEDBSB0F23`, attempt
  `pav_01M2MG0XQCAWJ38H53GVB5AN2F`, container `b78181ec7e33…`;
  `hosted-oci-restart-receipt.json` again has all three satisfied outcomes.
  Stopped Python and the restarted OCI at the end: both leases acknowledged,
  PID/container and per-lease workspaces removed. Other host containers were
  untouched. This tests read-only bundled data, **not durable data transfer**.

## UI and outstanding acceptance

Python's PWA ready screen shows the selected Python D and Verified status.
The actual Datasette Surface was opened and used in Chrome: the `items` table
showed A/2 and B/5, and the SQL UI returned total 7. Screenshots:
`.tmp/datasette-evidence/screenshots/pwa-python-ready.png` and
`.tmp/datasette-evidence/screenshots/pwa-python-datasette-sum.png`.

OCI's PWA ready screen shows the OCI D and Verified status; screenshot:
`.tmp/datasette-evidence/screenshots/pwa-oci-ready.png`. The Open App action
created its instance tab, but that tab showed Cloudflare 524 (origin timeout)
at `2026-09-16T06:50:24Z`. The browser automation then refused control of
that URL under its URL policy. **No screenshot of a usable OCI Datasette UI
exists.** The Runner-local OCI Port did return the real two rows and sum 7,
and the Hosted HTTP verifier returned satisfied receipts, but these do not
substitute for the required end-user Surface. The OCI public ingress/proxy
must be diagnosed and the PWA table/SQL interaction rerun before declaring
full four-path UI acceptance. Do not bypass the browser safety policy to do so.
At the same second `cloudflared` logged a canceled incoming request for
`s0-rstg002.ato.run` via `http://localhost:18080`; the Runner log records the
OCI container as ready at `06:47:48Z`, but has no per-request entry at the
timeout. This does not yet establish whether the failure was at Cloudflare,
Caddy, the Runner proxy, or the application. The relevant host logs can be
read with `journalctl --utc -u cloudflared -u caddy -u ato-runner-agent
--since '2026-09-16 06:49:45 UTC' --until '2026-09-16 06:50:45 UTC'`.

## Negative tests and regressions

- HTTP 200 with the wrong body yields `http_body_digest_mismatch` in
  `ato-formation` tests; deferred does not count as fully satisfied.
- Missing/wrong Python, readiness timeout, and early exit fail with a reason
  in the connected Runner tests. A machine with no local Docker Engine refused
  explicit D_oci (`exit 1`, daemon socket unavailable) without switching D;
  the compatible Linux host was used for the actual CLI OCI PASS.
- Mutable OCI tag, digest/platform mismatch, unsafe run flags, and missing or
  tampered DB/wheel/closure are rejected by OCI Adapter and bundle tests.
  A rehashed broken one-sided route leaves the other route and K intact;
  an unrehashed tamper is rejected before route selection. Nonexistent D and
  implicit selection with multiple D are refused.
- Existing Static and Process CLI fixtures still PASS. Rust checks: 22 IPC
  launch tests, 12 portable bundle tests, 42 connected runtime tests (some
  macOS-only sandbox cases explicitly skipped without bwrap), 3 OCI Adapter
  tests, 5 CLI portability tests. PWA typecheck/build and 4 targeted tests
  PASS; API typecheck and targeted import/retry test PASS.
- Full `capsule-network-registry.test.ts` on API HEAD: 18 passed / 3 failed
  (21 total). Same command/dependencies on `origin/main` at `25abe426`:
  15 passed / 5 failed (20 total). The three common failures have identical
  test names/results (Public Try capacity, `no_runner` expectation vs
  `managed_pool_unavailable`, and zero-Binding continuation 409 vs 202).
  The two extra base-only failures are missing `ready_local_port` in the old
  test harness; the branch's fixture migration setup fixes those. No new
  failing route was introduced; the new portable import/retry test passes.

## Scope review

The common process and Runner-owned OCI Adapter accept declared argv, cwd,
environment, inputs, Port, runtime/platform constraints and lifecycle without
adding a Datasette kind to Kernel/Core. Datasette-specific values exist only
in the example fixture and locked inputs. A second OSS still needs its own
bundle/Contract and capability review, but should not need a new Core noun or
Datasette-specific verifier. Do not claim arbitrary OSS compatibility or
fully offline OCI portability from this one experiment.
