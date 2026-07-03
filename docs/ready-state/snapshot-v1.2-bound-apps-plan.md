# Snapshot v1.2 Plan — Bound Snapshot Apps

> **Snapshot v1.2 turns sealed web capsules into practical user apps by adding
> launch-time secrets, app-private state, user file bindings, and declared
> network access — without baking user data or credentials into the snapshot.**

v1.0/v1.1 ([snapshot-v1-compatibility.md](../snapshot-v1-compatibility.md))
seal/restore **no-binding, single-process web apps**. v1.2 keeps that base and
adds exactly four **launch-time** bindings:

1. Secret binding
2. App-private state binding
3. User file binding
4. Network egress binding

The design core is unchanged from the Phase 8 contract
([binding-lease.md](binding-lease.md)): **the snapshot artifact never contains
a secret or user data; the Launch Profile carries references, not values**
(per `docs/rfcs/draft/ato-resource-namespace.md` — `SecretRefNode` is "a
reference and a permission boundary, not a secret value"; secret values live in
a SecretStore backend per accepted ADR-005). Bindings are resolved at
**restore/launch time**, never at build/seal time.

This is a plan, not the v1.2 contract. The v1.2 Compatibility Contract is
written (as a §v1.2 extension of `docs/snapshot-v1-compatibility.md`) in PR 1
below, after the design decisions in §2 are reviewed.

---

## 1. What already exists (v1.2 builds on Phase 8a, not greenfield)

Verified on nightly `7a807a0b`:

| Piece | State |
|---|---|
| Lease delivery: vsock guest-agent → tmpfs `/run/ato/bindings/<name>` 0600, atomic write, zero-wipe scrub | **DONE** (#913–#915, guest-agent crate) |
| Bound-ready gate before expose; fail-closed on missing/failed binding | **DONE** (`binding_host.rs::establish_bindings`, N-lease loop) |
| Run-gate wiring behind `ATO_READY_STATE_BINDINGS_PREVIEW=1`; #837 guard otherwise | **DONE** (D1–D3 #916–#918, `run.rs:3759-3795`) |
| Stop-scrub over vsock; never re-seal post-bind | **DONE** (#918, L5 #923) |
| `SecretResolver` trait; `SecretValue` Debug-redacted; receipts carry names only | **DONE** (L3 #922, PR1 #895) |
| Hard invariants: no secret in snapshot; post-bind state dirty (no re-capture) | **PINNED** (binding-lease.md:23-32) |
| `env` binding = env-*like* read at request time; env-rewrite into a snapshotted process **rejected**; real process-env = "a later supervisor/restart mode" | **PINNED** (binding-lease.md:58-70) |
| Host-side lease renewal loop; `revoke_binding` wiring | **MISSING** (L8 — dead code, fixed 1h TTL) |
| Runner-local / user-store secret resolvers | **MISSING** (L9 — only `EnvSecretResolver`; Vault/UserStore/Cloud are fail-closed stubs) |
| Multi-binding live E2E | **MISSING** (L7 — code supports N, KVM E2E delivers 1) |
| Guest-agent process supervision (exec/restart a workload) | **MISSING** (agent can only write/scrub files today) |

v1.2 is therefore: **finish L7–L9, add the supervisor mode, add three new
binding kinds (state / user files / network), and productize the grant +
identity surface.**

## 2. Design decisions (review these first)

### D1. Secret `env` semantics: **restart-with-env on bind** (recommended)

The v1.2 requirement "env var injection from secret_ref" collides with the
pinned invariant: a snapshotted process's `environ` cannot be rewritten
(binding-lease.md:58-70). The same contract already names the resolution — the
supervisor/restart mode. Recommendation:

- Seal exactly as today: app booted and verified **pre-bind, secret-free**
  (the artifact stays identical bound or not, and v1.0 capsules are untouched).
- For capsules declaring `delivery = "env"` secrets, the guest-agent gains a
  **supervisor role**: at launch, after all leases are delivered and verified,
  it **restarts the workload process** with the composed environment
  (bindings + `HTTPS_PROXY` etc. from §D3), then readiness gates traffic.
- Warmth: the re-exec happens inside the restored VM whose **page cache is
  part of the sealed memory image** — interpreter/`.so`/`.pyc` pages are
  already resident, so restart cost is process-init only, not cold I/O.
- `delivery = "file"` (exists today) remains the zero-restart path for
  Ato-aware apps reading `/run/ato/bindings/<name>` at request time.
- Post-restart state is **dirty by definition** — the existing no-re-seal
  invariant already covers it.

Build-verify for env-secret apps: boot-verify runs with a **marked placeholder
value** (`ATO-PLACEHOLDER-<nonce>`), then the builder **stops the app process,
scrubs, arms the supervisor, and only then snapshots** — the L4/no-secret scan
gains a placeholder-sentinel check proving no placeholder byte was sealed.

### D2. State & user-file mounts: **build-declared drive slots, restore-time substitution**

Firecracker has **no virtio-fs/9p** (device profile is fixed
virtio-blk+virtio-net+vsock; Virtiofs exists only on the unavailable QEMU/Kata
skeletons), and drives are baked into the snapshot (restore never calls the
drives API — it re-materializes files at the recorded host paths). The proven
mechanism (rw-rootfs mode; vsock UDS precedent) is **path identity**: declare
the drive at build with a deterministic per-capsule path, place a different
file at that path before `/snapshot/load`.

- Build (v1.2-eligible capsules only) attaches up to three additional empty
  ext4 drives at deterministic paths: `state` (rw), `input` (ro), `output`
  (rw); guest init mounts them at the declared mount points.
- Restore substitutes per-instance files at those paths pre-load:
  - `state`: the instance's persistent ext4 (created empty on first launch,
    reused across launches **and across snapshot revisions** of the same
    instance).
  - `input`: an ext4 the host packs from the user's per-launch file grant
    (read-only drive — the guest cannot write it, enforcement is the block
    layer, not app cooperation).
  - `output`: an empty per-launch ext4; on stop the host extracts results to
    the user's granted output directory.
- The existing single-session-per-snapshot constraint (baked TAP/vsock/drive
  paths, per-tap lock) already matches this model; true concurrency stays
  future work as documented.

State schema **reuses the existing cold-path tables** rather than inventing
`[state] kind = "app_private"`: `[state.<name>]` (`StateRequirement`:
durability/attach/sharing/schema_id, `manifest.rs:1195`) + a mount binding
(the parse-only `BindingKind::State` + `mount` field graduates to drive
wiring). "App-private" maps to `sharing = "exclusive"` + instance-scoped
source directories, the same identity rules `--managed-state-root` uses today.

**No automatic state migration in v1.2** (precedent:
CAPSULE_DEPENDENCY_CONTRACTS §7.7 — provider-declared compat boundary, no auto
migration in v1). New policy authored in PR 4: launch preflight compares the
artifact's `state_contract` (schema_id + declared state version) with the
instance state's recorded contract — **compatible proceeds; incompatible
blocks with an actionable reason; "migration needed" is surfaced to the user
(future workflow), never auto-run.**

### D3. Network egress: **host-side, fail-closed by construction**

Today the FC TAP has **no egress at all** — `net_up()` is three `ip` commands,
no NAT/forwarding/filtering exists anywhere in crates, so a guest can only
reach the host /24. Deny-by-default is already physically true; v1.2 makes
egress an **opt-in, allowlisted grant**:

- `[network] egress_allow = ["api.openai.com"]` (field already exists in the
  typed manifest, `manifest.rs:1414`) becomes the snapshot-path policy input.
- Enforcement (enforced tier): host-side proxy bound on the TAP host IP
  (172.16.0.1) doing domain (SNI/CONNECT) allowlisting — reusing the egress
  machinery nacelle already has for cold runs (tsnet SOCKS5 sidecar pattern +
  eBPF cgroup egress precedent). Supervisor mode (D1) injects
  `HTTPS_PROXY`/`HTTP_PROXY` into the restarted process env; DNS resolves only
  allowlisted names. No NAT is ever configured for non-proxied traffic —
  undeclared egress fails because **no route exists**, not because a filter
  caught it.
- Advisory tier: runtimes/paths where enforcement isn't available yet display
  the declared policy as **advisory** and record it as such in the receipt —
  official v1.2 support is **enforced-path only** (issue #786 shows the cold
  Linux source path's `enabled=false` is unenforced today; snapshot v1.2 must
  not repeat that silent gap).
- Preflight consent: "this app talks to api.openai.com" — instance-scoped,
  recorded in the receipt as the effective policy.

### D4. Identity: `snapshot_artifact_id` / `launch_execution_id` split

- `snapshot_artifact_id` **already exists** as
  `ReadyStateManifest::id()` (blake3 over JCS) — adopt the name, no new
  mechanism.
- `launch_execution_id` is a **new digest** over: `snapshot_artifact_id` +
  secret **refs** (names + secret_ref ids, never values) + state binding
  (instance id + state_contract hash) + file grant ids + network policy hash +
  launch args + profile id. This aligns with `LaunchTemplateKey` in the
  resource-namespace RFC (commits to binding_set_hash / network_policy_hash /
  state_contract_hash; must not contain secret values or session facts) and
  with ADR-009's `artifact_build_id ≠ execution_id`. The existing launch
  digest (`ato-launch-digest-v2`) commits to command/args/cwd/port/probe only —
  v1.2 extends it (v3) rather than inventing a parallel scheme.

## 3. Scope

### v1.2 Compatibility Contract (one line)

```
Snapshot v1.2 supports:
  single-process web apps
  + runtime-injected secrets
  + app-private persistent state
  + user-selected file mounts
  + declared network egress allowlist
```

### Manifest surface (additions in PR 1)

```toml
[secrets.OPENAI_API_KEY]
required = true
description = "OpenAI API key used to summarize documents"   # NEW field on SecretSpec
env = "OPENAI_API_KEY"          # delivery = "env" (default) → supervisor restart-with-env
                                # delivery = "file" → request-time read (Phase 8a path)

[state.appdata]
durability = "persistent"       # existing StateRequirement
sharing = "exclusive"           # app-private
schema_id = "jobs-v1"           # state contract for preflight compat triage

[bindings.appdata]
kind = "state"
mount = "/data"
required = true

[bindings.input_files]
kind = "user_files"
mode = "read_only"              # NEW field on BindingSpec
mount = "/input"
required = true

[bindings.output_dir]
kind = "user_files"
mode = "read_write"
mount = "/output"
required = false

[network]
egress_allow = ["api.openai.com"]   # existing field, becomes enforced on the snapshot path
```

Launch Profile (control-plane side): env entries carry `secret_ref` only,
per the resource-namespace RFC.

### Eligibility gates to relax (exact positions)

- Gate A `snapshot::rootfs_builder::derive_build_spec` (`rootfs_builder.rs:94`):
  today rejects required secrets (:95), ANY binding (:100), ANY external
  (:104). v1.2: accept `secrets.*` (any), `bindings.*` of kind
  `secret|state|user_files`, keep rejecting `oauth`/`llm`/`runner`/`context`
  kinds, all `external.*`, and GPU.
- Gate B `ready_state/bindings.rs::requires_runtime_bindings` (:53): today ANY
  declared name triggers the guard (stricter than Gate A — fix the asymmetry);
  v1.2 routes the four supported kinds through the launch-binding path and
  keeps fail-closed for everything else.
- The `ATO_READY_STATE_BINDINGS_PREVIEW` flag graduates for the four supported
  kinds once PR 7's fixtures are green.

### Out of scope for v1.2 (explicit)

OAuth browser flow · refresh-token lifecycle · team/shared secrets · external
managed DB binding · Postgres/MySQL service binding · multi-service compose ·
background workers · scheduled jobs · GPU · private-repo source snapshots ·
BYOC secret sync · host device binding · broad filesystem mounts (`~/`,
Desktop/Documents, hidden files, recursive home access, background file
watching).

## 4. PR decomposition (order fixed)

| PR | Scope | Where | Reuse anchors |
|---|---|---|---|
| **PR 1** | Manifest schema v1.2: `SecretSpec.description`, `BindingSpec.mode`, state-binding graduation, `[network]` on the snapshot path; **v1.2 contract §** in snapshot-v1-compatibility.md | `crates/capsule` (ready_state.rs, manifest.rs), docs | parse-only tables already exist; add fields, no behavior |
| **PR 2** | SecretStore-backed resolver + launch-profile `secret_ref` + grant model (per-app/profile grant, revoke blocks launch); renewal loop + `revoke_binding` wiring (closes L8) | `crates/cli` (secret_resolver.rs, binding_host.rs), ato-api (grant surface) | ADR-005 SecretStore; L3 trait; dead `revoke_binding` |
| **PR 3** | Guest-agent **supervisor mode** (exec/restart workload with composed env, bound-ready after restart); builder placeholder-verify + scrub-before-seal; placeholder sentinel in L4 scan; redaction regression tests | `crates/guest-agent`, `crates/snapshot`, `crates/snapshot-builder` | binding-lease.md's named supervisor mode; L4 scanner |
| **PR 4** | App-private state: build-declared `state` drive slot; per-instance ext4 lifecycle (create/attach/preserve across revisions); state-contract preflight triage (compatible/blocked); checkpoint hook (optional) | `crates/snapshot` (firecracker.rs), `crates/cli` | rw-rootfs path-substitution mechanism; `[state.*]` schema; managed-state-root identity rules |
| **PR 5** | User files: per-launch grant → packed ro `input` ext4 + rw `output` ext4; extraction on stop; receipt carries grant-id + redacted paths; broad-mount rejection | `crates/cli`, `crates/snapshot`; PWA file picker in ato-pwa | same drive-slot mechanism as PR 4 |
| **PR 6** | Network egress: host-side domain-allowlist proxy on TAP host IP + proxy-env injection via supervisor; enforced vs advisory tier recorded in receipt; preflight consent; diagnostics | `crates/snapshot` (net_up seam), `crates/cli`, nacelle egress reuse | zero-egress TAP (fail-closed seam); tsnet/eBPF precedents |
| **PR 7** | v1.2 fixtures (below) + API E2E; L7 multi-binding live E2E folded in | `crates/snapshot-builder/fixtures/compat/`, scripts | v1 compat suite (steps 3–5 of the v1 plan) |
| **PR 8** | PWA UX: secret entry → grant → launch; file picker; consent screen ("talks to api.openai.com"); actionable preflight blocks; browser E2E via `__ATO_SNAPSHOT_DEBUG__` | ato-pwa | v1 browser E2E harness; P0 instrumentation |

Managed-runner note: PR 2's resolver must be **runner-local** for the cloud
path (secret values never transit the control API) — this closes L9 and is a
precondition for v1.2 on managed runners; local `ato run` uses the user
SecretStore directly.

## 5. v1.2 Compatibility fixtures

Positive: `secret-openai-web` · `secret-optional-fallback` · `sqlite-state-web`
· `file-input-readonly` · `file-output-readwrite` · `network-allow-openai` ·
`combined-pdf-summarizer` · `combined-github-analyzer`

Negative: `missing-required-secret` (blocks at **launch preflight**, not seal)
· `secret-leaked-to-rootfs` (seal gate) · `secret-printed-in-log` (redaction)
· `undeclared-network-egress` (no route/proxy ⇒ fails) ·
`write-to-readonly-input` (block-layer EROFS) · `missing-file-binding`
(launch blocked until grant) · `broad-home-mount-rejected` ·
`state-contract-incompatible` (preflight block)

Acceptance is **safe-stop as much as run**: required secret missing ⇒ blocked
before launch with actionable UI; secret value absent from
rootfs/receipt/log (scan-proven); no file access without a grant; read-only
input unwritable; undeclared egress unreachable; state survives
stop→restore→relaunch and snapshot-revision updates.

## 6. First demos (all three exercised by `combined-*` fixtures + PR 8)

1. **PDF Summarizer** — secret `OPENAI_API_KEY`, `/input/*.pdf` (ro),
   `/output/summary.md` (rw), egress `api.openai.com`.
2. **CSV Cleaner** — `/input/*.csv`, state `/data/jobs.sqlite`,
   `/output/cleaned.csv`, **no network**.
3. **GitHub Issue Analyzer** — secret `GITHUB_TOKEN`, state
   `/data/cache.sqlite`, egress `api.github.com`.

Together they cover secret / file / state / network once each and combined.

## 7. Risks

- **D1 restart cost**: if re-exec inside the restored VM proves slow for heavy
  apps (page cache not covering enough), the fallback is `delivery = "file"`
  plus SDK-side lazy read; measure in PR 3 with the bench harness.
- **Drive-slot topology**: v1.2 artifacts have a different device topology than
  v1.0 (extra drives) ⇒ v1.2 requires a re-seal; the contract must say a v1.0
  artifact cannot gain bindings without rebuild.
- **Output extraction**: block devices are single-writer; host reads `output`
  only after stop (or via guest-agent streaming later). UX must set the
  expectation that results appear on stop/finish.
- **Egress advisory tier**: must be visually distinct in UI + receipt, or it
  becomes #786 all over again on the snapshot path.
