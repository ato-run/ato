---
name: warmup-snapshot-build
description: 'Build and verify warmup-enabled Ready-State snapshots (ato#1082) so a capsule''s first screen is warm on restore. Injects [snapshot] warmup into the recipe lane or the builder env for import lanes, enqueues rebuilds, batches around ENOSPC, verifies the sealed manifest, and repoints preview metadata. Use when asked to "warmupスナップショットをビルド", "make snapshots faster / warm the first screen", "rebuild snapshots with warmup", or to roll warmup out to a set of capsules on staging.'
license: MIT
---

# warmup-snapshot-build

Repeatable procedure for building **warmup snapshots** — snapshots whose sealed
memory image already carries the user's first-screen work, so restore is fast.
Feature reference (fields, safety rule, measured effect, source of truth):
**`docs/ready-state/warmup-snapshots.md`** — read it first; this skill is the
operational runbook.

Measured: first-screen **2106 → 1086 ms (−48%)** on a KVM host. Benefit scales
with the app's cold first-screen cost.

⚠️ This skill covers **warmup only** — it needs no UFFD flag. The `−66%` UFFD
figure from the same KVM run does not generalize: on staging hosts UFFD measured
*slower* than the File path, and `ATO_RUNNER_UFFD_PREVIEW` has no binding gate.
See `docs/ready-state/warmup-snapshots.md` §4 before setting it anywhere.

## The one safety rule (do this first)

`warmup_paths` **fails the build closed** if the path never returns 2xx/3xx, and a
non-serving `content_ready_path` makes **restore hang** to `boot_timeout`. So:

**Only warm a path the app actually serves 2xx/3xx.** Classify each target by its
`readiness_probe.http_get` (recipe) or `readiness_http_path` (import):

- readiness = `/` → `warmup_paths=["/"]`, `content_ready_path="/"` — safe.
- readiness = a non-`/` path that IS the first screen (e.g. `/notebooks`) → warm
  that path.
- readiness = a health-only endpoint, or API-only / desktop-RFB / supervisor-
  binding → **do NOT warm** (health-only gives no benefit; the others break or
  are auto-skipped).

A failed warmup build keeps the old snapshot eligible (no live breakage) — but it
wastes a build + disk, so classify, don't guess.

## Procedure

### 0. Scope and environment

- Staging chain: `stg-app.ato.run` PWA → `staging.api.ato.run` → D1
  `ato-store-db-stg` → **Sugamo** builder+runner (`ubuntu-sugamo`,
  `ssh ekohsuke@100.114.96.42`). Snapshots are `runner_class`-bound, so build on
  the SAME host that will restore.
- `_e2e` / `cap_compat_*` / ULID-named capsules are **pipeline fixtures**; the
  user-facing catalog is the **showcase feed** (`GET /v1/showcase/feed`). Pick
  targets that are BOTH in the feed AND restorable — measuring a non-visible
  fixture proves nothing to the user.
- Classify targets:
  ```sh
  # recipe lane readiness:
  wrangler d1 execute ato-store-db-stg --env staging --remote --json --command \
    "SELECT r.capsule_id, r.recipe_toml FROM capsule_source_recipes r WHERE r.capsule_id IN (...)"
  # import lane readiness (params_json.readiness_http_path):
  wrangler d1 execute ato-store-db-stg --env staging --remote --json --command \
    "SELECT capsule_id, kind, params_json FROM capsule_snapshot_jobs WHERE status='sealed' AND kind LIKE '%import%' GROUP BY capsule_id"
  ```

### 1. Inject warmup

- **Recipe lane** — append `[snapshot]` to the stored `recipe_toml` (the builder
  materializes it as `capsule.toml` at claim). Back up first, then:
  ```sql
  UPDATE capsule_source_recipes
  SET recipe_toml = recipe_toml || char(10) || '[snapshot]' || char(10)
    || 'warmup_paths = ["/"]' || char(10) || 'content_ready_path = "/"' || char(10)
  WHERE capsule_id IN (...) AND recipe_toml NOT LIKE '%[snapshot]%';
  ```
  Verify one round-trips as valid TOML (`tomllib.loads`) before enqueueing.
- **Import lane** — set on the builder, then restart it (env applies at claim):
  ```sh
  ssh ekohsuke@100.114.96.42 'printf "ATO_SNAPSHOT_BUILDER_WARMUP_PATHS=/\nATO_SNAPSHOT_BUILDER_CONTENT_READY_PATH=/\n" \
    | sudo tee -a /etc/ato/runner-builder-override.env && sudo systemctl restart ato-snapshot-builder'
  ```
  ⚠️ Env must be set **before** the import jobs are enqueued (builder polls 15 s).
  ⚠️ `ATO_FC_VSOCK=1` in that same file forces vsock on ALL builds → recipe
  (non-supervisor) artifacts become inconsistent (`has_vsock` w/o
  `supervisor_build`) and the runner refuses them. To warm recipe apps, unset it
  for their build; import/supervisor artifacts are unaffected.

### 2. Enqueue (D1 direct — no auth cookie)

Recipe:
```sql
INSERT OR IGNORE INTO capsule_snapshot_jobs
  (id, capsule_id, target_label, profile, status, kind, attempt_count)
VALUES ('job_warm_<slug>', '<capsule_id>', 'web', 'default', 'queued', 'recipe', 0);
```
Import: clone the prior sealed job's `kind` + `params_json` + `source_ref` into a
new `queued` row. **Batch ≤ 2–3 import jobs at a time** (ENOSPC — see step 3).

### 3. Monitor + disk hygiene

- Poll `SELECT id,status,failure_reason FROM capsule_snapshot_jobs WHERE id LIKE 'job_warm_%'`.
- Watch disk: `ssh … 'df -h / | tail -1'`. At >90%, between batches:
  `docker system prune -f` and
  `sudo find /var/lib/ato/snapshots -mindepth 1 -maxdepth 1 -type d ! -name 'job_warm_*' -exec rm -rf {} +`
  — but **never delete a live capsule's job dir**: on staging the runner restores
  from `/var/lib/ato/snapshots/<job>/manifest.json` on local disk.
- Re-queue a disk/transient failure by resetting the SPECIFIC failed job id back
  to `status='queued', attempt_count=0` (clear claim/result cols). Reset ONLY the
  failed id — a `WHERE id IN (a,b)` that includes an already-sealed job needlessly
  rebuilds it. External failures (e.g. a Dockerfile `curl` to archive.org 500,
  ghcr.io pull) are orthogonal to warmup — retry.

### 4. Verify the sealed artifact carries warmup

```sh
ssh ekohsuke@100.114.96.42 "sudo python3 -c \"import json;m=json.load(open('/var/lib/ato/snapshots/job_warm_<slug>/manifest.json'));print(m['restore_contract'].get('warmup_paths'), m['restore_contract'].get('content_ready_path'))\""
# → ['/'] /
```
NOT the ack `receipt_json` — it is empty by the strict schema. Also check
`has_vsock` vs `supervisor_build` are consistent (see step 1 warning).

### 5. Make it dispatchable — repoint preview metadata

A new snapshot registers in `capsule_snapshots` but leaves
`capsule_snapshot_metadata` pointing at the old artifact → preview returns offer
`"preparing"`. Repoint (see the SQL in `docs/ready-state/warmup-snapshots.md` §
gotcha 4). Then a preview dispatches:
```sh
curl -s -X POST https://staging.api.ato.run/v1/preview-runs \
  -H 'content-type: application/json' -d '{"capsule_id":"<id>","target_label":"web"}'
# offer=run_now + app_url=https://<id>.stg-app.ato.run/ ; poll /v1/runs/<run_id> to ready
```
Rate limit: 5/min burst, 100/30 min sustained per IP
(`managed_cloud_mvp_rate_limits`, key `preview-start:<ip>`).

## Known limits / not-yet-automated

- **App-view screenshot** in a browser needs the preview surface's access grant
  (`app_view_access_required`), which the PWA performs and which is not trivially
  replicable headlessly; the staging PWA itself is behind Cloudflare Access. The
  restore-to-ready time (POST → `/v1/runs/<id>` ready) is a fair proxy for the
  user wait, since "ready" = the content-ready probe passed on the warm path.
- Feed `guest_runnable=false` (`verification_state=stale`) is a separate gate from
  `snapshot_ready` — a warmed capsule may still need verification before the UI
  lets a user launch it.
