# Snapshot Run Control — API contract (Store Capsule Ready-State Run E2E)

> **This is the coordination artifact.** Every workstream (Registry API, Build
> Worker, Run Control, PWA, Desktop) implements against this contract. Change the
> contract here first, in a PR, before changing an implementation. Tracks
> [#896](https://github.com/ato-run/ato/issues/896).
>
> Status: **draft / PR 1.** No endpoints implemented yet. `nightly` + staging
> flags only; nothing touches `dev`/`main` runtime release. ato-api code lands
> behind staging/admin flags, inert until enabled.

## 0. Purpose & boundaries

Let a user **Run a Store Capsule from PWA/Desktop, restored from a sealed
snapshot**. Three Ato-owned control-plane servers — **not** a per-app always-on
server:

1. **Snapshot Build Server** — build → boot → verify → snapshot → seal a Capsule.
2. **Snapshot Registry / Artifact Server** — sealed artifact + manifest + receipt
   + eligibility.
3. **Run Control Server** — accept Run requests and restore on the right runner.

### Hard scope (this initiative)

- **no-binding public Capsules only.** A Capsule that requires `[secrets.*]`,
  `[bindings.*]`, `[external.*]`, or user files is **not** snapshotized and is
  **rejected at run** (`CAPSULE_REQUIRES_*`). Do **not** entangle with Phase 8
  BindingLease.
- **Default `mem_backend` is `File`.** UFFD is **preview/benchmark only** (staging
  flag), never selected by default, never a silent fallback.
- **No app-specific always-on server.** Snapshots restore on demand and tear down.

## 1. no-binding eligibility

A Capsule/target is **Ready-State eligible** for this initiative iff **all** hold
(the Build Worker enforces; the Registry surfaces the result):

| requirement | blocker code when false |
|---|---|
| public source (no private auth to fetch) | `CAPSULE_NOT_READY_STATE_ELIGIBLE` |
| a `targets.web` (or declared web target) exists | `CAPSULE_NOT_READY_STATE_ELIGIBLE` |
| a healthcheck / readiness probe is declared | `HEALTHCHECK_FAILED` (at build) / eligibility blocker |
| no `[secrets.*]` | `CAPSULE_REQUIRES_SECRETS` |
| no `[bindings.*]` | `CAPSULE_REQUIRES_BINDINGS` |
| no required `[external.*]` capability | `CAPSULE_REQUIRES_EXTERNAL_CAPABILITY` |
| no required user files | `CAPSULE_REQUIRES_BINDINGS` |

`ready_state_blockers` is a list of these codes (empty ⇒ eligible). Detection
reuses the existing binding guard
(`application/ready_state/bindings::requires_runtime_bindings`).

## 2. Snapshot Registry API

Base: ato-api. All snapshot/run endpoints are **staging + admin gated** at first.

### `GET /v1/store/capsules`
Existing Store listing, **additive** fields per capsule:
```jsonc
{
  "capsule_id": "…",
  // …existing store fields…
  "snapshot_status": "none | building | ready | failed",
  "latest_snapshot_id": "snap_… | null",
  "ready_state_eligible": true,
  "ready_state_blockers": []            // e.g. ["CAPSULE_REQUIRES_SECRETS"]
}
```

### `POST /v1/capsules/:capsule_id/snapshot-jobs`
Enqueue a snapshot build. **admin/staging only** (see §2.5 Auth). **Idempotent**
(see §2.6): pass `Idempotency-Key: <uuid>` (or body `client_request_id`).
```jsonc
// request (optional; defaults from the capsule's default web target)
{ "target_label": "web", "profile": "default", "source_ref": "…",
  "client_request_id": "uuid" }         // or the Idempotency-Key header
// 202 Accepted (new)  |  200 OK (idempotent replay of an existing job)
{ "job_id": "job_…", "status": "queued" }
// 409 if not eligible → { "error": "CAPSULE_NOT_READY_STATE_ELIGIBLE", "blockers": [...] }
```

### `GET /v1/snapshot-jobs/:job_id`
```jsonc
{
  "job_id": "job_…",
  "capsule_id": "…",
  "status": "queued | building | verifying | sealed | failed",
  "receipt": { … },                 // present when sealed
  "error_summary": "…"              // present when failed (no secrets)
}
```

### `GET /v1/capsules/:capsule_id/snapshots/latest`
Latest **sealed** snapshot (404 if none).
```jsonc
{
  "snapshot_id": "snap_…",
  "capsule_id": "…",
  "artifact_manifest_hash": "blake3:…",
  "runner_class_id": "blake3:…",
  "snapshot_backend": "firecracker",
  "execution_id": "blake3:…",
  "healthcheck": { "type": "http", "path": "/health", "port": 8080 },
  "no_binding_required": true,
  "public_run_eligible": true,
  "artifact_location": "cas://… | https://…"   // where a runner pulls it
}
```

## 2.5 Auth / ownership

- **Snapshot enqueue** (`POST snapshot-jobs`) and job/build reads are **admin +
  staging** only. A public capsule does **not** grant public snapshot-build access.
- **Runs are user-scoped.** `POST /v1/runs` requires an authenticated user;
  `runs.user_id` is the creator. Only the **run owner** (or an admin) may
  `GET /v1/runs/:id` or `POST /v1/runs/:id/stop` — a non-owner gets `403`
  (`RUN_FORBIDDEN`), never another user's `app_url`/receipt.
- A capsule being **publicly runnable** (`public_run_eligible`) governs *whether*
  a signed-in user may start a run — it does **not** make run control endpoints
  public or another user's run visible.

## 2.6 Idempotency

`POST snapshot-jobs` and `POST /v1/runs` accept an `Idempotency-Key` header (or
`client_request_id` in the body). A repeat with the **same key + same
(user, capsule, snapshot, provider)** returns the **existing** job/run (`200`),
never a duplicate builder/runner — this defends against PWA double-clicks,
retries, and network resends (which otherwise cause VM double-charge, runner
exhaustion, and orphans). Keys are retained long enough to cover client retry
windows.

## 3. Run Control API

### `POST /v1/runs`
```jsonc
// request  (Idempotency-Key header or client_request_id; user-scoped)
{
  "capsule_id": "…",
  "snapshot_id": "snap_… | null",         // null ⇒ latest sealed
  "target": "web",
  "provider": "managed_cloud | desktop",
  "mem_backend_policy": "file | uffd_preview | auto_preview",  // optional, default "file"
  "client_request_id": "uuid"             // or the Idempotency-Key header
}
// 201 (new)  |  200 (idempotent replay)
{
  "run_id": "run_…",
  "status": "queued",                     // see the run state machine (§4)
  "provider": "managed_cloud",
  "selected_snapshot_id": "snap_…",
  "receipt_id": "rcpt_…",
  // provider-specific handoff (see §3.1) — filled as the run progresses:
  "app_url": null,                        // managed_cloud, when ready
  "handoff_url": null,                    // desktop deeplink, when dispatched
  "desktop_session_id": null,             // desktop, when dispatched
  "unsupported_reason": null              // set instead if the provider can't run it
}
```
Rejections (fail-closed, with reason): `SNAPSHOT_NOT_READY`, `CAPSULE_REQUIRES_*`,
`RUNNER_UNSUPPORTED`, `UFFD_PREVIEW_DISABLED`, `UFFD_UNSUPPORTED`, `RUN_FORBIDDEN`.

### 3.1 Provider handoff model

| provider | ready response | how it runs |
|---|---|---|
| `managed_cloud` | `app_url` (`https://<session>.app.ato.run`) | Run Control restores on a managed runner and exposes the URL directly. |
| `desktop` | `handoff_url` + `desktop_session_id`, **or** `unsupported_reason` | Run Control does **not** restore Desktop itself. It records the run + the sealed-snapshot **metadata** (the Desktop reuses the Registry, it is not re-implemented) and returns a handoff the local Desktop Companion consumes (deeplink). Desktop pulls the artifact and restores locally, or reports `unsupported_reason` (no local restore support). `app_url` is null for `desktop`. |

### `GET /v1/runs/:run_id`
Owner or admin only.
```jsonc
{
  "run_id": "run_…",
  "status": "queued | dispatching | restoring | binding | ready | failed | stopped",
  "app_url": "https://<session>.app.ato.run | null",     // managed_cloud
  "handoff_url": "… | null",                             // desktop
  "desktop_session_id": "… | null",
  "unsupported_reason": "… | null",
  "receipt": { … },
  "failure_reason": "RUNNER_CLASS_MISMATCH | SNAPSHOT_RESTORE_FAILED | HEALTHCHECK_FAILED | RUNNER_UNSUPPORTED | …"
}
```
> `binding` is a reserved lifecycle state for Phase 8; in this initiative a
> no-binding run goes `… → restoring → ready` and never enters `binding`.

### `POST /v1/runs/:run_id/stop`
Owner or admin only. Stops the session; runner tears down VM/tap/overlay.
`200 { "status": "stopped" }`. Idempotent (double-stop safe). On teardown error →
`TEARDOWN_FAILED` (logged, best-effort).

## 4. State machines

### Snapshot job
```text
queued ──▶ building ──▶ verifying ──▶ sealed
   │           │            │
   └───────────┴────────────┴──────▶ failed   (error_summary set; no secrets)
```
`sealed` writes a `capsule_snapshots` row; `failed` is terminal and API-visible.

### Run
```text
(POST /v1/runs) ─▶ queued ─▶ dispatching ─▶ restoring ─▶ ready ─▶ stopped
                     │           │              │           ▲
                     └───────────┴──────────────┴──▶ failed  │  (POST /stop)
              [binding] reserved (Phase 8; unused for no-binding runs)
```
- `queued` — accepted; no runner slot yet (runner capacity may be full).
- `dispatching` — a runner/slot is claimed; artifact being fetched/verified.
- `restoring` — snapshot restore in progress on the runner.
- `ready` — healthcheck passed; `app_url`/`handoff_url` available.

`queued`/`dispatching` are first-class so Run Control composes cleanly with
managed Cloud Slots / runner capacity (a run is never silently dropped when no
slot is free).

## 5. Artifact metadata (what a runner needs to restore)

A runner (managed cloud or desktop) restores from the `capsule_snapshots` record
+ the artifact at `artifact_location`. `execution_id` ties the run to a
reproducible identity. `no_binding_required` MUST be `true` for any run in this
initiative.

### 5.1 Integrity verification (runner MUST, before restore)

`artifact_location` is a hint, **not trusted on its own**. Before restoring, the
runner MUST verify, fail-closed:

1. the fetched artifact's **`artifact_manifest_hash`** matches the record;
2. every **CAS chunk hash** matches (content-addressed; a corrupt/substituted
   chunk is rejected);
3. the record's **`snapshot_backend`** is one this runner can drive;
4. the host **`runner_class_id`** exactly matches the snapshot's (the existing
   restore Prepare gate) — no silent wrong-class restore;
5. **`execution_id`** is consistent with the manifest.

On any mismatch the run fails closed with the specific reason — never a
best-effort restore of unverified bytes:

| check fails | error |
|---|---|
| artifact absent / location unreachable | `SNAPSHOT_ARTIFACT_MISSING` |
| manifest hash / CAS chunk hash mismatch | `SNAPSHOT_RESTORE_FAILED` |
| backend not drivable here | `RUNNER_UNSUPPORTED` |
| runner class mismatch | `RUNNER_CLASS_MISMATCH` |

## 6. DB schema (minimal)

```sql
capsule_snapshot_jobs(
  id, capsule_id, source_ref, target_label, profile,
  status,                         -- queued|building|verifying|sealed|failed
  requested_by, idempotency_key,  -- unique(capsule_id, idempotency_key) → dedup enqueue
  created_at, started_at, finished_at,
  error_summary,                  -- no secrets
  receipt_json
);

capsule_snapshots(
  id, capsule_id, source_ref, target_label, profile,
  capsule_manifest_hash, execution_id, runner_class_id, snapshot_backend,
  artifact_manifest_hash, artifact_location, healthcheck_url_path,
  no_binding_required, public_run_eligible,
  created_at, receipt_json
);

runs(
  id, capsule_id, snapshot_id, user_id, provider,
  idempotency_key,                -- unique(user_id, idempotency_key) → dedup run
  status,                         -- queued|dispatching|restoring|binding|ready|failed|stopped
  app_url,                        -- managed_cloud
  handoff_url, desktop_session_id, unsupported_reason,  -- desktop
  selected_mem_backend,           -- file|uffd_preview
  created_at, ready_at, stopped_at,
  receipt_json, failure_reason
);
```
Migrations are additive; existing Store/run tables are untouched. (ato-api hand-
numbers migrations — renumber past the current head at implementation time.)

## 7. Error codes

| code | meaning | surfaced by |
|---|---|---|
| `SNAPSHOT_NOT_READY` | no sealed snapshot for the capsule/target | run |
| `SNAPSHOT_BUILD_FAILED` | build/boot/verify/seal failed | job |
| `CAPSULE_NOT_READY_STATE_ELIGIBLE` | not eligible (source/target/healthcheck) | enqueue / run |
| `CAPSULE_REQUIRES_BINDINGS` | `[bindings.*]` / user files present | eligibility / run |
| `CAPSULE_REQUIRES_SECRETS` | `[secrets.*]` present | eligibility / run |
| `CAPSULE_REQUIRES_EXTERNAL_CAPABILITY` | required `[external.*]` present | eligibility / run |
| `RUNNER_UNSUPPORTED` | selected provider can't restore this class | run |
| `RUNNER_CLASS_MISMATCH` | host runner class ≠ snapshot's | run/restore |
| `RUN_FORBIDDEN` | caller is not the run owner (nor admin) | run get/stop |
| `SNAPSHOT_ARTIFACT_MISSING` | artifact not found at location | restore |
| `SNAPSHOT_RESTORE_FAILED` | restore failed on the runner | run |
| `HEALTHCHECK_FAILED` | app didn't become healthy | build / run |
| `TEARDOWN_FAILED` | stop/teardown error (best-effort) | stop |
| `UFFD_UNSUPPORTED` | host can't drive UFFD (see `uffd_reason`) | run |
| `UFFD_PREVIEW_DISABLED` | `uffd_preview` requested but staging flag off | run |

## 8. End-to-end lifecycle

```text
Store (no-binding web capsule)
  └─ admin: POST /v1/capsules/:id/snapshot-jobs
       └─ Build Worker: build→boot→verify→snapshot→seal→scan→upload
            └─ capsule_snapshots row (sealed)  ·  GET …/snapshots/latest → ready
  PWA/Desktop: capsule card shows "Ready" → Run enabled
  └─ user: POST /v1/runs {provider}
       └─ Run Control: pick sealed snapshot (File backend) → runner restore
            └─ managed: app_url ready  ·  desktop: local restore or unsupported reason
  └─ GET /v1/runs/:id → ready (app_url)  → user opens app
  └─ user: POST /v1/runs/:id/stop → clean teardown
```

## 9. Flags & gating

- `nightly` (ato) + staging (ato-api / ato-pwa) only. `dev`/`main` runtime
  release untouched.
- Snapshot/run endpoints: **staging + admin** until an explicit enablement flag.
- `mem_backend_policy=uffd_preview` requires the staging UFFD-preview flag +
  host UFFD support + no-binding + local CAS mem chunks; else fail-closed.
- Every binding-required capsule is rejected at both enqueue and run — the Phase 8
  firewall stays intact.

## 10. Parallelization

Once this contract merges, the workstreams proceed in parallel against it:
A (#897 Registry API) · B (#898 Build Worker) · C (#899 Run Control) · D (#900
PWA) · E (#901 Desktop) · F (#902 snapshotization) · G (#903 E2E). D can build UI
against a stubbed API from day one. Recommended PR order: schema+read-model →
enqueue → builder MVP → run-control MVP → managed restore → PWA UI → desktop →
allowlist snapshotization → E2E smoke.

## 11. Reconciliation with existing ato-api run infrastructure

ato-api already owns a run model + control plane from the managed-cloud work.
Snapshot-run **extends** it — it does **not** fork a parallel run system:

- **`runs` table + `/v1/runs`** (existing) are reused. Snapshot-run adds *additive*
  columns: `snapshot_id`, `selected_mem_backend`, `idempotency_key`, and the
  desktop handoff fields (`handoff_url`, `desktop_session_id`, `unsupported_reason`).
  The contract's `queued/dispatching/restoring/ready/failed/stopped` map onto the
  existing `runs.status` vocabulary (extend the enum; don't replace it).
- **Runner dispatch** reuses the existing `runners` / `runner-leases` / Cloud
  Slots infrastructure — Track C adds **no** new dispatcher.
- **Only new tables:** `capsule_snapshot_jobs` and `capsule_snapshots` (Ready-State
  *sealed artifacts*). These are **distinct from `source_snapshots`** (source
  materialization) — do not conflate the two.
- The Store `capsules` listing gains *additive* read-model fields; the `capsules`
  table is not forked.

So §3/§6's `runs` shape describes the **columns snapshot-run relies on**, realized
as additive columns on the existing `runs` table, not a new table.
