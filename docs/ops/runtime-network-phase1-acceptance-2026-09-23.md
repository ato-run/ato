# Runtime Network Phase 1 — acceptance (2026-09-23)

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
