# Warmup snapshots — warming the user's first screen before seal

**Status:** shipped on `nightly` (ato#1082). Measured effect on a KVM host:
first-screen (restore-ready + first `GET /`) **2106 ms → 1086 ms (−48%)**, and
**720 ms (−66%)** with the UFFD preview. The benefit scales with the app's *cold
first-screen cost* — a lightweight app that already renders `/` fast gains little.

The idea: a v1 snapshot is sealed the moment `/health` answers, so the sealed
memory image does **not** contain the work the user's first `GET /` triggers
(template rendering, JIT, DB init, first-frame prep). That work is paid again,
in the browser, on every restore. Warmup drives the user-facing path(s) until
they are stably serving **before** the Pause+Snapshot, so the sealed image is
already warm and the first request hits warm pages.

## 1. How to declare it (build time)

### Recipe lane — `capsule.toml` has a `[snapshot]` table

```toml
[snapshot]
mode = "warm"              # warm | booted (any non-none mode)
warmup_paths = ["/"]       # GET-hit until stable, AFTER healthcheck, BEFORE seal
content_ready_path = "/"   # the path the runner probes for restore readiness
# stable_successes = 1     # consecutive stable rounds required (default 1)
# stable_interval_ms = 250 # poll interval between rounds (default 250)
```

- `warmup_paths` — each path receives an HTTP `GET` and must return **2xx/3xx**.
  Empty (the default) ⇒ unchanged v1 behavior (healthcheck-only seal point).
  Every path must start with `/` and contain no spaces/control chars (validated
  at build; a typo fails the build with a pointed error, not an opaque timeout).
- `content_ready_path` — the path the *runner* hits to decide "ready" at restore,
  i.e. the first screen the browser loads. Resolution order:
  `content_ready_path || healthcheck || "/"`. Without it, a runner reports ready
  after only `/health`, while the user's `GET /` still pays the cold cost.
- `stable_successes` / `stable_interval_ms` — how many consecutive good rounds,
  how far apart. Raise `stable_successes` for an app that answers `/` once then
  reloads routes/recompiles. `0` is clamped to `1`.

`[snapshot]` is additive: an empty/absent table parses byte-identically to v1,
and a manifest sealed before this feature restores unchanged (`effective_*`
apply the `1 / 250` fallback).

### Import lanes — no `capsule.toml` (dockerfile / oci / compose imports)

There is no manifest to author, so the **operator sets it on the builder**
(applies to every import build the builder claims while set):

```
ATO_SNAPSHOT_BUILDER_WARMUP_PATHS=/          # comma-separated
ATO_SNAPSHOT_BUILDER_CONTENT_READY_PATH=/
ATO_SNAPSHOT_BUILDER_STABLE_SUCCESSES=1      # optional
ATO_SNAPSHOT_BUILDER_STABLE_INTERVAL_MS=250  # optional
```

An invalid path here is a **build error**, not a silent skip (a typo that quietly
produced a non-warmed artifact is the confusion this whole feature removes).

Both lanes converge on one `snapshot::WarmupRecipe`, so they freeze the same
fields by the same rule.

## 2. The safety rule — only warm a path that actually serves 2xx/3xx

> **`warmup_paths` FAILS the build closed if the path never returns 2xx/3xx**
> (warmup timeout, bounded by `boot_timeout`), and a `content_ready_path` that
> does not serve makes **restore hang** to `boot_timeout`.

So warm only the path the app truly serves as its first screen. `readiness_probe.
http_get` in the manifest is the best signal: if it is `/`, `warmup_paths=["/"]`
is safe. **Do not blanket-warm `/`** on:

- API-only capsules (`/` is 404) or apps that serve at a subpath,
- desktop / pixel (RFB) surfaces — there is no HTTP first screen,
- required-binding supervisor builds — warmup is **auto-skipped** for these (the
  workload is stop+revoke'd before seal), so declaring it there is a no-op.

A failed warmup build does **not** delete the capsule's existing sealed snapshot
(the old one stays eligible) — so a wrong guess wastes a build, it does not break
a live preview. But it does burn builder time + disk, so classify first.

## 3. How to verify a sealed artifact carries warmup

The ack payload (`capsule_snapshot_jobs.receipt_json.artifact.restore_contract`)
is **empty** by design — the ato-api ack schema is `.strict()`, so warmup is not
in the ack. The authoritative copy is the **sealed manifest**:

```sh
# On the builder host, per sealed job:
sudo python3 -c "import json;print(json.load(open('/var/lib/ato/snapshots/<job>/manifest.json'))['restore_contract'])"
# → warmup_paths, content_ready_path, stable_successes, healthcheck present
```

At restore, `RESTORE_PROF … content_ready_ms=<N>` is emitted per lease (from
`RestoreReceipt.content_ready_ms`, computed on every restore — **not** a
`snapshot::bench` span, which is off unless `ATO_READY_STATE_BENCH=1`). `ungated`
there means no HTTP probe gated readiness (a supervisor artifact).

## 4. UFFD preview (P1, optional, off by default)

`ATO_RUNNER_UFFD_PREVIEW=1` on a Connected Runner opts its `restore_snapshot`
leases into UFFD demand-paging (the ~512 MB eager rehydrate moves off the restore
critical path). It is gated on host capability via `FirecrackerBackend::probe()`
(`crate::uffd::evaluate`: x86_64 + `/dev/kvm` + Firecracker ≥ 1.0 + kernel
userfaultfd) and **degrades to the eager File path, logging why**, on an
unsupported host — a wrong box costs latency, not leases. The env gate
`ATO_FC_UFFD` (test smokes) stays ungated and hard-fails by contrast.

## Source of truth

- `crates/capsule/src/foundation/types/ready_state.rs` — `SnapshotConfig`
  fields, `DEFAULT_STABLE_*`, `is_valid_probe_path` / `validate_probe_paths`.
- `crates/snapshot/src/manifest.rs` — `RestoreContract` + `WarmupRecipe` +
  `effective_*` + `content_ready_path_or`.
- `crates/snapshot/src/firecracker.rs` — `warmup_paths()` (the build-time loop),
  `probe_ready` / `http_status_ready` (the shared 2xx/3xx probe),
  `uffd_preview_mode()` (the P1 gate), and the restore `content_ready` wait.
- `crates/snapshot-builder/src/main.rs` — `warmup_from_manifest` /
  `warmup_from_env` (the two lanes).
- `crates/cli/src/application/runner_agent.rs` — the `RESTORE_PROF` line.

## Operational gotchas (rebuilding warmup snapshots on staging)

These bit us during the first staging rollout — encode them in any repeat.

1. **Disk / ENOSPC.** Each build writes a ~1 GB `rootfs.ext4` to
   `/var/lib/ato/snapshots/<job>/` plus (imports) several GB of docker images.
   `ATO_FC_WORK` is on `/var/lib/ato` (not the 16 GB `/tmp` tmpfs — that older
   gotcha is mitigated), but the disk still fills at ~10 builds. **Batch import
   builds ≤ 2–3 and clean between**: `docker system prune -f`, then
   `rm -rf /var/lib/ato/snapshots/*` for dirs that are *not* your in-flight batch.
2. **Staging serves artifacts from LOCAL disk**, not R2. The runner reads
   `/var/lib/ato/snapshots/<job>/manifest.json` at restore. So (a) never delete a
   live capsule's job dir during cleanup, and (b) a job dir clobbered by a later
   failed rebuild makes restore fail `No such file`.
3. **`ATO_FC_VSOCK=1` on the builder forces vsock onto every build.** Supervisor
   artifacts (imports) carry a `supervisor_build` receipt, so they stay
   consistent. Plain **recipe** artifacts get `has_vsock=true` with no supervisor
   receipt → the runner refuses them ("declares a vsock binding channel but
   carries no supervisor_build receipt"). To warm recipe apps, unset
   `ATO_FC_VSOCK=1` for their build (or only warm import/supervisor capsules).
4. **Repoint metadata after (re)building** or preview dispatch fails closed to
   offer `"preparing"`. Dispatch requires
   `capsule_snapshot_metadata.snapshot_artifact_manifest_hash` ==
   `capsule_snapshots.artifact_manifest_hash` of the latest eligible snapshot:
   ```sql
   UPDATE capsule_snapshot_metadata
   SET snapshot_artifact_manifest_hash = (
     SELECT s.artifact_manifest_hash FROM capsule_snapshots s
     WHERE s.capsule_id = capsule_snapshot_metadata.capsule_id
       AND s.target_label = capsule_snapshot_metadata.target_label
       AND s.public_run_eligible = 1
     ORDER BY s.created_at DESC LIMIT 1),
       snapshot_status = 'ready', updated_at = datetime('now')
   WHERE capsule_id IN (...) AND target_label = 'web';
   ```
5. **Enqueue via D1** (no staging auth cookie needed): insert a `queued` row into
   `capsule_snapshot_jobs`. Recipe jobs need only `capsule_id + kind='recipe'`
   (the builder resolves `recipe_toml` from `capsule_source_recipes` at claim, so
   editing that row's `recipe_toml` to add `[snapshot]` is how you inject warmup).
   Import jobs need the `params_json` cloned from the prior sealed job. The
   builder polls every 15 s (silent on an empty poll).
6. **`snapshot_ready` ≠ `guest_runnable`** in the showcase feed — the latter needs
   verification. A capsule can be snapshot-ready yet show `guest_runnable=false`
   (`guest_run_not_yet_available`, `verification_state=stale`) until verified.
7. **Preview rate limit** (`managed_cloud_mvp_rate_limits`): 5/min burst,
   100/30 min sustained, per client IP.
