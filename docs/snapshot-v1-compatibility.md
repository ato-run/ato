# Snapshot v1 Compatibility Contract

> **Historical implementation contract.** This document describes the former
> Ready-State product path. Snapshot v1 is a physical realization format, not
> Capsule identity or a current Semantic Core primitive. Use
> [Materialization](concepts/materialization.md) for the current model.

> **Snapshot v1 is a sealed ready-state path for no-binding, single-process web
> apps. It is not a general VM hosting product.**

This document fixes the *supported application surface* of the Ready-State
snapshot path as a **contract guaranteed by end-to-end tests**, not as a list of
things the implementation happens to accept. A capsule class is "supported" if
and only if a compatibility fixture for it seals, restores, and serves in the
E2E suite; a capsule class is "rejected" if and only if a fixture for it fails
closed at the documented stage with the documented reason. Anything not covered
by a fixture is *unspecified*, not supported.

The enforcement surface for this contract is the fixture matrix in
[§4](#4-compatibility-fixture-matrix). The contract changes only when a fixture
is added or changed in the same PR.

## 1. What Snapshot v1 is

A **Snapshot v1 capsule** is built once on a KVM builder host
(`tools/snapshot-builder`): source is materialized from the *server-approved*
recipe or repository manifest, a rootfs is derived (Docker → ext4), the app is
booted under Firecracker, verified against its readiness probe, snapshotted,
sealed into a content-addressed artifact, scanned for secrets, and registered.
Runs restore the sealed artifact on a capable runner and proxy traffic to the
restored guest.

Everything below is stated per stage, in the order the pipeline enforces it.

## 2. Eligibility requirements (build-time, fail-closed)

A capsule is Snapshot-v1-eligible when **all** of the following hold
(`snapshot::rootfs_builder::derive_build_spec`; each violation fails closed
with an actionable reason — never a silent downgrade):

| # | Requirement | Rejection reason (fixture) |
|---|-------------|---------------------------|
| R1 | No required secrets (`secrets.*.required`) | `secret-required` |
| R2 | No bindings at all — this includes user-files and OAuth, which are declared as `BindingKind::UserFiles` / `::Oauth` | `user-files-binding`, `oauth-binding` |
| R3 | No external services (`external.*`) | `external-db-required` |
| R4 | No GPU (`build.gpu`) | `gpu-required` |
| R5 | Runtime is one of **static web**, **node source**, **python source** | (out-of-scope runtimes fail with the v1 support list) |
| R6 | The default target declares `port = <n>` | `missing-port` |
| R7 | The default target declares a **run command** (`execution.entrypoint`) — **including static web** | `missing-run` |
| R8 | Readiness: an explicit `readiness_probe.http_get` wins; with only a `port`, the builder **synthesizes** `http_get "/"` and records `synthesized_probe = true` in the receipt (never silent). No port ⇒ R6 rejection | `synthesized-root-404` exercises the synthesized-probe failure mode |

**Static web is served by the manifest's run command, not by the builder.** The
builder selects a `python:3.11-slim` base image for static-web capsules but
never generates an HTTP server; a static-web capsule must declare its own run
command (e.g. `python3 -m http.server 8080`). This unifies R7 across all v1
runtimes.

**Run-command dialect:** a single-token `*.py` entrypoint is normalized to
`python3 <script>` (mirroring the CLI's launch convention); multi-token
commands run verbatim. Commands must be single-line and NUL-free (they are
embedded quoted into the generated Dockerfile/init; enforcement is fail-closed).

**Manifest source:** for Store capsules the *server-approved* recipe
(`capsule_source_recipes.recipe_toml`, carried on the builder claim) is
authoritative and is written as `capsule.toml` over anything in the repository;
repo-manifest capsules use their committed `capsule.toml`. A client-supplied
source ref is never authoritative.

## 3. Runtime requirements (boot-verify and serve)

- **Bind `0.0.0.0`, not `127.0.0.1`.** The Firecracker boot-verify probe and
  the runtime proxy reach the guest over its TAP interface; loopback-bound
  servers are unreachable and fail boot-verify (fixture `localhost-only-bind`).
- **Single process.** One long-lived foreground process launched by the run
  command. No supervisor trees, no background daemons the contract would have
  to keep alive across snapshot/restore.
- **Answer the readiness probe.** Explicit probe path, or `/` when the probe
  was synthesized (R8). An app that 404s its synthesized `/` probe fails
  boot-verify with the probe result in the receipt (fixture
  `synthesized-root-404`).

## 4. Compatibility fixture matrix

The suite is intentionally minimal; widen it only by amending this contract.

**Positive — must seal, restore, and serve:**

| Fixture | Exercises |
|---------|-----------|
| `static-web-basic` | static web + explicit run command (R7 applies to static web) |
| `python-stdlib-explicit` | python, no deps, explicit `readiness_probe.http_get` |
| `python-bare-port-only` | bare `.py` entrypoint normalization + synthesized probe |
| `python-requirements-flask` | pip install path (`requirements.txt`) |
| `node-express-basic` | npm install path (`package.json`) |
| `node-port-only` | node + synthesized probe |
| `store-recipe-manifest-only` | recipe-as-manifest: no `capsule.toml` in the repo, recipe authoritative |
| `real-store-receipt-to-csv` | the first real Store capsule, pinned as a regression anchor |

**Negative — must fail closed at the documented stage with the documented reason:**

| Fixture | Stage | Guarantee |
|---------|-------|-----------|
| `missing-port` | eligibility | R6 message names the fix (`declare port = <n>`) |
| `missing-run` | eligibility | R7 |
| `secret-required` | eligibility | R1 |
| `user-files-binding` | eligibility | R2 (BindingKind::UserFiles) |
| `oauth-binding` | eligibility | R2 (BindingKind::Oauth) |
| `external-db-required` | eligibility | R3 |
| `gpu-required` | eligibility | R4 |
| `localhost-only-bind` | boot-verify | TAP probe unreachable ⇒ no seal |
| `synthesized-root-404` | boot-verify | synthesized `/` probe failing ⇒ no seal, receipt says probe was synthesized |
| `pem-marker-in-library` | no-secret scan | PEM literals in library constants are ADVISORY on the CAS (do not block a clean app) but still GATE `manifest.json` |
| `planted-builder-token` | no-secret scan | the builder's own live credentials in any artifact GATE the seal |

## 5. Out of scope for v1 (explicitly not supported)

- Secrets, bindings, OAuth, user files, external services at runtime — Phase 8
  BindingLease is the successor path; until it lands, binding-required capsules
  fail closed everywhere.
- GPU execution or GPU state in snapshots.
- Multi-process / supervisor / background-worker topologies.
- Non-web workloads (no port, batch jobs, CLIs).
- **Unbuilt SPA sources served as "static web".** A dev `index.html` that
  points at bundler entrypoints (`/src/main.tsx`, `/src/main.js`) seals and
  serves HTTP 200 but renders a **blank page** — the builder's node install
  path is `npm install --omit=dev`, so devDeps build tooling (Vite/webpack)
  cannot run. Static web means a *complete, already-servable* tree. Evidence:
  the `browser-daw` and `uptime-kuma` Store capsules were sealed this way,
  verified blank in a real browser, and de-listed (`public_run_eligible = 0`).
  A devDeps frontend-build pipeline (build → serve `dist/`) is a v1.x
  expansion candidate, not v1.
- UFFD memory backends by default (preview flags only), remote CAS serving.
- General VM hosting semantics: arbitrary kernels, custom init, persistent
  disk mutation across runs (rootfs is read-only-shared; RAM holds session
  state).

## 6. Verification pipeline

The contract is enforced by, in order:

1. **Fixtures** (`tools/snapshot-builder/fixtures/compat/*`) — one directory
   per row above.
2. **Staging recipe seed script** — seeds the fixtures as approved recipes so
   the real builder claims them.
3. **API E2E** — enqueue each fixture, assert seal/fail-closed + reason via the
   registry API.
4. **Browser E2E** — drives the real PWA Store card (Prepare → Ready → Run →
   Open → Stop) in a real browser, asserting on `window.__ATO_SNAPSHOT_DEBUG__`
   (the staging-gated request/response trace) rather than synthetic clients.
   Note: the PWA's service worker bypasses driver-level request interception —
   fault injection happens at the stub/API, assertions at the debug sink.
5. **Evidence report** — the fixture × result matrix, with receipts, posted to
   the tracking issue per run.

A change that makes any positive fixture stop sealing, or any negative fixture
stop failing (or fail with a different reason), is a contract break and must
either be fixed or ship with an amendment to this document.

---

## 7. Snapshot v1.2 Compatibility Contract (Bound Snapshot Apps — PLANNED)

> **Snapshot v1.2 supports: single-process web apps + runtime-injected secrets
> + app-private persistent state + user-selected file mounts + declared network
> egress allowlist.**

Status: **planned, not yet enforced** — this section becomes normative as the
v1.2 PRs land (plan: `docs/ready-state/snapshot-v1.2-bound-apps-plan.md`).
Everything in §§1–6 stays true for v1.0 capsules; v1.2 adds exactly four
**launch-time** bindings, resolved at restore — never baked into the artifact.

### 7.1 What v1.2 adds

| Binding | Manifest surface | Delivery |
|---|---|---|
| Secret | `[secrets.<NAME>]` `required` / `description` / `env` / `delivery` (`env` \| `file`) | `env`: guest-agent supervisor restarts the workload with the composed environment after bound-ready; `file`: request-time read of `/run/ato/bindings/<name>` (no restart) |
| App-private state | `[state.<name>]` + `[bindings.<name>] kind = "state", mount` | per-instance ext4 substituted into the build-declared `state` drive slot at restore; survives stop/relaunch and snapshot-revision updates |
| User files | `[bindings.*] kind = "user_files", mode = "read_only" \| "read_write", mount` | per-launch grant packed into the `input` (ro) / `output` (rw) drive slots; read-only is block-layer enforced; results extracted on stop |
| Network egress | `[network] egress_allow = [...]` | host-side domain-allowlist proxy on the TAP; deny-by-default is physical (no route exists for undeclared egress) |

### 7.2 Hard constraints (contract)

- **Snapshot build readiness must not require a real external secret.**
  Build-verify runs with a marked placeholder; an app whose startup/readiness
  validates the key against the real provider cannot seal under v1.2.
  Real-secret validation belongs to launch E2E and the bound-ready gate.
- **No secret / user data in the artifact, ever.** Launch Profiles carry
  references (`secret_ref`, grant ids), values live in the SecretStore;
  receipts and logs stay value-free. Post-bind state is dirty — no re-seal.
- **Drive-topology invariant.** Restore-time substitution preserves the exact
  build-time topology (drive id / recorded path / size / ro-rw flag / fs
  shape); the `state`/`input`/`output` slots are fixed contracts. **A v1.2
  bound artifact always requires a re-seal from v1.0 — never an in-place
  upgrade.**
- **Egress official support = proxy-aware HTTP(S) clients** honoring the
  injected `HTTPS_PROXY`/`HTTP_PROXY`. Raw sockets / proxy-ignoring libraries
  remain unsupported even with a declared allowlist — they fail with no-route,
  and the UI/receipt must attribute that to the client shape, not the policy.
- **Grants (implemented in v1.2 PR 2):** a grant IS the secret value being
  present in the capsule-scoped SecretStore namespace `rs-<hash16 of the
  capsule manifest hash>` — grant with `ato secrets set <NAME> --namespace
  <ns>`, revoke with `ato secrets delete <NAME> --namespace <ns>`. Scoping by
  manifest hash means another app can never resolve this app's grant. Launch
  preflight resolves EVERY declared binding before restore and blocks with an
  aggregated, actionable report; a grant revoked mid-session is revoked+
  scrubbed by the renewal loop. `description` is surfaced verbatim at
  grant/preflight time under fixed constraints: max 200 chars, control
  characters (incl. newlines) collapse to spaces, plain text only — UIs render
  it as a text node, never as HTML.
- **Identity:** `snapshot_artifact_id` = the sealed manifest content address;
  `launch_execution_id` additionally commits to stable **redacted** capability
  identities (secret_ref ids, grant ids, state instance id, network policy
  hash) — never transient session facts.

### 7.3 v1.2 fixture matrix (enforcement surface, when it lands)

Positive: `secret-openai-web` · `secret-optional-fallback` · `sqlite-state-web`
· `file-input-readonly` · `file-output-readwrite` · `network-allow-openai` ·
`combined-pdf-summarizer` · `combined-github-analyzer`

Negative: `missing-required-secret` (launch preflight, not seal) ·
`secret-leaked-to-rootfs` · `secret-printed-in-log` ·
`undeclared-network-egress` · `write-to-readonly-input` ·
`missing-file-binding` · `broad-home-mount-rejected` ·
`state-contract-incompatible`

### 7.4 Still out of scope in v1.2

OAuth flows · team/shared secrets · external managed DBs · multi-service
compose · background workers · scheduled jobs · GPU · private-repo sources ·
BYOC secret sync · host devices · broad filesystem mounts (`~/`, Desktop,
hidden files, recursive home access, background watching).
