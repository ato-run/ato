# Runtime Network Phase 1 — acceptance (2026-09-23)

> The final-hardening re-run (Runtime-authoritative semantics, immutable
> profiles, idempotent orchestration; cases A–L) at the end of this file
> supersedes the first run.

`ato form <fixture> --runtime-network …` from a Mac against a coordinator,
with three owned machines serving as Runtimes (`ato runtime-network serve`).
ADR-021 describes the design. Fixtures:
`apps/formation-worker/fixtures/runtime-network/`.

## Setup

| | |
|---|---|
| Coordinator | `ato-api` `feat/runtime-network-phase1` under `wrangler dev --local` on the Mac; local D1 with the Runtime Network migration (then numbered 0286, now 0288); local R2. No staging or production resource was touched. |
| Transport | Remote Runtimes reach the coordinator through `ssh -R 18787:127.0.0.1:8787` |
| Identity | One account; three `runner_devices` rows (`user_managed`), one runner token each (mode 0600) |
| Requester | The Mac, with the Mac Runtime's token |
| Browser verifier | Only on `rt_oci-arm64`: Stagehand 3.7.3, Chrome/149 headless LOCAL, agent `deepseek/deepseek-flash`, judge `jev-1.13.0`; keys in a mode-0600 env file outside the repository |

### Runtimes

| runtime_id | Machine | Facts (selected) | `facts_ref` |
|---|---|---|---|
| `rt_sugamo-x86` | Ubuntu 26.04, kernel 7.0, Intel i7-8700, bwrap 0.11.1 | `linux/x86_64`, `cpu.vendor=intel`, `containment=bwrap+landlock`, `runtime.process=true`, `runtime.browser=false`, python 3.12.7, node 20.20.2 | `sha256:cd6850e2…` |
| `rt_oci-arm64` | Ubuntu 24.04, kernel 6.17 (OCI Ampere), bwrap 0.9.0 | `linux/aarch64`, `cpu.vendor=arm`, `containment=bwrap+landlock`, `runtime.process=true`, `runtime.browser=true`, python 3.12.7, node 20.20.2 | `sha256:c63d5236…` |
| `rt_mac-m1` | macOS 15.7.4, Apple M1 | `macos/aarch64`, `cpu.vendor=apple`, `containment=none`, `runtime.process=false`, `runtime.browser=true`, no toolchain root | `sha256:ffe61216…` |

Not enrolled:

- **Hetzner (Linux x86_64, AMD).** It runs the live runner, bwrap is not
  installed, and AppArmor restricts unprivileged user namespaces
  (`apparmor_restrict_unprivileged_userns=1`). Enrolling it would need both
  changed on a production host, which was not authorized.
- **Windows laptop.** Offline for the whole run, and the worker has no Windows
  containment.

## Results

| Case | Fixture / condition | Candidates (admissible / filtered with reason) | Attempts | Result |
|---|---|---|---|---|
| A | `notes`, `--mode all` | OCI ✓, sugamo ✓ / Mac: `requirement_unmet` runtime.process, containment, toolchain.root | OCI pass, sugamo pass | **satisfied — 2 VerifiedRoutes**, same K `22d8402e…`, same D `3a4c35b6…`, profiles `c63d…` and `cd68…` |
| B | `notes-x86-only` (`[[platform]] linux/x86_64`) | sugamo ✓ / OCI: `requirement_unmet` platform (expected `linux/x86_64`, actual `linux/aarch64`); Mac: platform + runtime.process + containment + toolchain.root | sugamo pass | **satisfied — 1 route**; ARM never received a ticket |
| C | `notes`, `--mode all`, OCI worker stopped for > 60 s | sugamo ✓ / OCI: `runtime_offline` (last seen 00:43:18Z); Mac: as in A | sugamo pass | **satisfied**; `GET /runtimes` still lists `rt_oci-arm64` with its facts and `facts_ref`, `online=false`; its route from A is still listed for K |
| D | `notes-arch-sensitive`, `first_pass` | OCI ✓ (rank 1), sugamo ✓ (rank 2) / Mac | OCI **fail** `http_status_mismatch` (`/health` 503), then sugamo pass | **satisfied** by fallback; both attempts kept |
| E | `notes-arch-sensitive`, `--exact-runtime rt_oci-arm64` | OCI ✓ / sugamo, Mac: `exact_runtime_mismatch` | OCI **fail** `http_status_mismatch` | **unsatisfied**; no second ticket issued |
| F | `notes`, `--verify-browser --accept "Create a note named 'formation-check'. Reload the page and verify that the note is still present."` | OCI ✓ / sugamo: `verifier_unavailable`; Mac: as in A | OCI pass, browser `pass` (judge `complete`) | **satisfied**; effective K `e7247bd4…` over base `22d8402e…`; route carries `http_contract` + `browser_contract` receipts; receipt `target.runtime_id = rt_oci-arm64` |
| S | `static-page`, `--mode all`, `--network denied` | Mac ✓, OCI ✓, sugamo ✓ | all three pass | **satisfied — 3 VerifiedRoutes** for one K `480f2e7a…` and one D `b4feb089…` on macOS ARM64, Linux ARM64 and Linux x86_64 |
| G | `notes`, `first_pass`; OCI worker stopped right after the ticket was issued to it | OCI ✓ (rank 1), sugamo ✓ | OCI **expired** (offline before claiming), then sugamo pass | **satisfied** after ~60 s; nothing left pending |

Satisfy ids: A `01M35TY5NFYHXWBSCGTT9X211Z`, B `01M35V1CDPRVC4ZW9CKCB43RH3`,
C `01M35VH6V8020R43QJJNMBSAHA`, D `01M35V1SBP4S0RB22W8TVFMXFX`,
E `01M35V2587P7H9P318MVT54FS7`, F `01M35VDEZJMS7TVNJ52GWWJN4C`,
S `01M35VNQZWG10NP14GYAVSQH3X`, G `01M35VXNRP13SAG0825GPGHEQS`.

### Observations

- The same D has the same `derivation_ref` on x86_64 and ARM64 (A, D, F),
  because the target triple enters projection only.
- The four fixtures have four different Ks: `source-identity` captures the
  workspace, and `capsule.toml` is part of it. Within a fixture, every Runtime
  was asked for the same K.
- The Mac is admissible only for the static route: it has no containment, so
  it admits no process route and no build step (ADR-019). In A–F it took part
  as a candidate and was filtered with typed evidence each time.

## Defects found and fixed during acceptance

| Symptom | Fix |
|---|---|
| A worker exited on one transient coordinator error during `claim` | the claim loop backs off and keeps serving (`51b0eb69`) |
| The browser receipt and attempt evidence named the Runtime `local` | ticket execution records the ticket's `runtime_id` (`3be8b121`) |
| A ticket issued to a Runtime that went offline before claiming stayed `pending` forever, and the request stayed `running` | an unclaimable pending attempt expires (ato-api `56bc45ff`; case G) |

## Not covered

- Bindings (Phase 1 refuses every binding), OCI routes, ato-managed Runtimes,
  provisioning, several environments per Runtime.
- A Runtime on another account (covered by coordinator tests only).
- Staging or production: nothing was deployed; the migration was applied to
  the local D1 only.
- The Browser Verifier helper and Chrome are not OS-contained (ADR-021,
  security follow-up) — a blocker before the 100-app benchmark.

## Final hardening re-run (A–L)

Same three Runtimes, same fixtures, same coordinator setup, with ato
`feat/runtime-network-phase1` at `60eafa30` on every Runtime and the Mac, and
ato-api `feat/runtime-network-phase1` at `776a6fa5`. The local D1 was
recreated so the edited migration 0288 applied from scratch; fresh runner
tokens (mode 0600). Every profile ref came out identical to the first run
(content addressing): OCI `c63d5236…`, sugamo `cd6850e2…`, Mac `ffe61216…`.

H and I need a requester that lies. A local proxy
(`tamper_proxy.py`, scratch only) forwards everything to the coordinator and
rewrites only the metadata of `POST /satisfy` — never the route text or the
archive.

| Case | Condition | Result |
|---|---|---|
| A | `notes`, `all` | satisfied — OCI pass, sugamo pass, 2 routes; attested `pure`, `execution_started=true`, route `agent_version=0.1.0` |
| B | `notes-x86-only`, `all` | satisfied — OCI hard-filtered `requirement_unmet: platform`; sugamo pass |
| C | OCI worker stopped > 60 s | satisfied — OCI `runtime_offline`, identity and profile listed `online=false`; sugamo pass |
| D | `notes-arch-sensitive`, `first_pass` | satisfied — OCI **fail** `http_status_mismatch` (attested `pure`, started) → fallback → sugamo pass |
| E | same, `--exact-runtime rt_oci-arm64` | unsatisfied — OCI fail, sugamo `exact_runtime_mismatch`, no second ticket |
| F | `notes` + browser Contract | satisfied — OCI only (`verifier_unavailable` on sugamo); browser `pass` (judge `complete`); route with `http_contract` + `browser_contract`, receipt `target.runtime_id = rt_oci-arm64` |
| S | `static-page`, `all` | satisfied — 3 routes: macOS ARM64, Linux ARM64, Linux x86_64 |
| G | OCI stopped right after its ticket was issued | satisfied — OCI `expired` (never claimed) → sugamo pass |
| **H** | `notes-x86-only`, `all`; proxy strips the `platform` requirement | satisfied — both admissible to the coordinator; sugamo pass; OCI **refused before execution**: `platform_unsupported` ("this route runs on linux/x86_64; this Runtime is linux/aarch64"), `execution_started=false`; both attempts record `metadata_mismatch: requirements` (claimed without `platform`, attested with it); one route, x86 |
| **I** | `notes-non-repeatable`; proxy rewrites `effects` to `pure` | unsatisfied — OCI **refused before execution**: `effect_policy`, attested `non-repeatable`, `execution_started=false`, `metadata_mismatch: effects (claimed pure, attested non-repeatable)`; **no fallback** to the admissible sugamo; no route. OCI's log shows the attempt ending `inconclusive` with no build or launch |
| **J** | `notes`, `all`; OCI worker held stopped so its ticket stays pending; sugamo drained after the candidates were created; OCI restarted | satisfied — 2 candidates admissible at creation; OCI pass; at issue time sugamo re-filtered to `runtime_drained` (rank 2 kept), no ticket |
| **K** | during G, 1 530 concurrent `GET /satisfy/:id` (each runs expiry + `advance()`) in 30-request bursts every 0.5 s across the expiry moment | exactly 2 attempts for the request: OCI `expired`, one sugamo ticket (then `pass`); every GET 200, no error in the coordinator log |
| **L** | OCI restarted without the browser verifier | current profile `1d71b686…` (`runtime.browser=false`); `GET /capability-profiles/c63d5236…` still returns the F route's facts (`runtime.browser=true`, first seen 01:44:27Z); the F route still names `c63d5236…`, `agent_version 0.1.0` |

Satisfy ids: A `01M35YZVR0VFDF4PP7XY0C98BE`, B `01M35YZYS9P5B55AC9YFJSBHRW`,
C `01M35ZA0YNRKQER6RR9WDCV64Z`, D `01M35Z2ZZQXKM2V527BEPTPDD7`,
E `01M35Z332WGY9EDCY5F3D306PC`, F `01M35Z3J9J54ZHT3RXAVVP25M4`,
S `01M35Z363R6FPXY6J88DX44A1P`, G/K `01M35Z74W7XJ5QHWNSS24ZDSAV`,
H `01M35Z51C94RKGCM79ZTYK1ZRR`, I `01M35Z54E9GYHHXNFF95TY5KMJ`,
J `01M35Z63AQK7JZRXZQ8B3MTYG1`.

### Defect found by the re-run

Right after an attempt, a Runtime still looked full: availability rode only
the 15 s heartbeat, so the issue-time re-filter skipped it with
`capacity_exhausted` (D and S when submitted back-to-back). The worker now
reports availability as soon as it takes or frees a slot, freeing it before
the result that advances the request (`60eafa30`). D, E and S were re-run
after the fix.

### Tests

- ato: `runtime_network_v1` (disposable route attested; non-repeatable
  refused whatever the metadata claimed; another platform refused before
  execution with the platform requirement attested; environment mismatch;
  ref mismatch refused before execution), `local_formation_v1`
  (`[[platform]]` for another host is Filtered under `--runtime local`).
- ato-api `runtime-network.test.ts`, 26 tests: the earlier cases plus H, I,
  pass-acceptance rules, claimed-then-silent expiry without fallback, J for
  drain / facts / browser / capacity, K (six concurrent `advance()` → one
  ticket; a direct second insert is refused by `UNIQUE`), L, and request
  validation (repeated D, K ≠ base without browser Contract, malformed or
  over-wide browser Contract). K was checked against a mutation: without the
  two uniqueness constraints six concurrent calls issue six tickets.

### Still not covered

Bindings, OCI routes, ato-managed Runtimes, provisioning, several
environments per Runtime (the worker refuses anything but `native`), staging
and production (nothing deployed; migration 0288 applied to a local D1 only),
and OS containment of the Browser Verifier helper and Chrome (next task).
