# Warmup snapshots — warming the user's first screen before seal

**Status:** shipped on `nightly` (ato#1082). Measured effect on a KVM host:
first-screen (restore-ready + first `GET /`) **2106 ms → 1086 ms (−48%)**, and
**720 ms (−66%)** with the UFFD preview. The benefit scales with the app's *cold
first-screen cost* — a lightweight app that already renders `/` fast gains little.

> Warmup and the UFFD preview are **independent** features. Warmup is the shipped
> win and needs no flag. The `−66%` UFFD figure is a single KVM-host measurement
> and does **not** generalize — on the staging hosts measured 2026-07-18 the same
> flag made restore *slower*. Read §4 before enabling UFFD anywhere.

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
mode = "warm"              # warm | booted only — cold/none stay on the legacy
                           # path (SnapshotConfig::is_ready_state)
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
in the ack. *(That schema lives in `ato-api`, not this repo — operational context,
confirm there if it matters.)* The authoritative copy is the **sealed manifest**,
which IS produced here (`snapshot::manifest::RestoreContract`):

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

`ATO_RUNNER_UFFD_PREVIEW=1` on a Connected Runner opts its restore leases into
UFFD demand-paging (the ~512 MB eager rehydrate moves off the restore critical
path). The flag is **lease-kind-agnostic**: `handle_restore_snapshot_lease` reads
it once (`crates/cli/src/application/runner_agent.rs:4600`) and passes it
unchanged into `restore_and_expose` (`:4632`), so it applies to **all three**
restore kinds that handler serves — `restore_snapshot`,
`restore_snapshot_with_bindings` (supervisor) and `restore_snapshot_preview`
(`crates/cli/src/application/ready_state/restore_lease.rs:41,50,60`).

**Three** ordered **fail-to-File** gates, all inside `uffd_preview_mode_for`
(`crates/snapshot/src/firecracker.rs`, `fn uffd_preview_mode_for`; reached from
`uffd_preview_mode`, which `restore()` calls when `input.uffd_preview` is set).
Every refusal `eprintln`s its reason and returns `None`, i.e. **falls back to the
eager File path** — File is the safe backend and UFFD is only ever the
optimization, so a preview opt-in can never take a runner's leases down with it:

1. **No required bindings** — `declares_required_bindings(supervisor_build)` refuses
   with "capsule requires bindings; UFFD is no-binding-only until Phase 8
   BindingLease". This is the pure selector's highest-precedence rule
   (`crates/snapshot/src/mem_backend_selector.rs`: "a binding-required capsule is
   never UFFD until Phase 8 `BindingLease`"), and it is enforced **in the backend**
   rather than at a call site precisely because the runner lane never evaluates
   `decide_mem_backend` — `ATO_RUNNER_UFFD_PREVIEW` flows straight from env into
   `RestoreReadyStateInput`, so a call-site check would be bypassable.
2. **Host capability** via `FirecrackerBackend::probe()` (`crate::uffd::evaluate`:
   x86_64 + `/dev/kvm` + Firecracker ≥ 1.0 + kernel userfaultfd) — an unsupported
   host degrades to File, logging why. The env gate `ATO_FC_UFFD` (test smokes)
   stays ungated and hard-fails by contrast.
3. **Memory image fully RESIDENT in the local CAS** — `CasStore::has_all_chunks`
   over the whole chunk list, not `CasStore::open`. Openable is not populated:
   `open` `create_dir_all`s the layout and therefore succeeds on an *empty* store,
   so the pre-#1127 openability check accepted exactly the case it was supposed to
   reject. Residency is a **precondition**, never a disqualifier: `PageSource::Cas`
   resolves every guest fault out of the local CAS and production builds it with
   `remote: None`, so a chunk absent when the guest touches that page has nowhere to
   come from. Refusing here converts a post-boot page-fault abort — after the
   session was handed out — into a clean pre-boot `MissingChunk` in
   `rehydrate_atomic`. The pure selector states the same rule: `memory image not in
   local CAS → File`.

All three gates are covered host-independently in CI by the single unit test
`uffd_preview_requires_a_resident_memory_image_not_just_an_openable_cas`
(`crates/snapshot/src/firecracker.rs`, `mod tests`), which asserts the
binding-refusal, the incapable-host refusal, full residency ⇒ `Some(UffdMode::Cas)`,
and both the empty-CAS and partial-residency refusals. `uffd_preview_mode_for` is
data-in/decision-out precisely so these rules do not depend on a `#[ignore]`d KVM
smoke to be enforced.

> Gates 1 and 3 landed in #1127; this section previously described only gate 2 plus
> a `CasStore::open` check, because it was rewritten in #1128 three seconds after
> #1127 merged and therefore documented the code as it had been. The paragraph that
> used to sit here — "Not yet enforced — the no-binding discipline … do not set this
> flag on a runner that serves supervisor / required-binding artifacts" — told
> operators to avoid a configuration the code now refuses, and has been deleted.

> **Measured (2026-07-18, warm-cache staging host, deployed 512 MB snapshots):**
> UFFD is *slower* than File when the image is local — File's cached sequential
> read of the materialized `.mem` beats UFFD's per-page `userfaultfd` + CAS-read
> overhead, and the content-ready probe faults pages in one at a time.
>
> | capsule | File `backend_restore_ms` | UFFD `backend_restore_ms` | content_ready File→UFFD |
> |---|---|---|---|
> | tobu | 212 | 297 | 47 → 128 |
> | blinko | 299 | **1383** | 78 → **1083** |
>
> So UFFD only pays off for an image that would otherwise be fetched **whole from
> a remote store**. Today the runner fetches the artifact whole before restore
> (`ensure_artifact_local`,
> `crates/cli/src/application/ready_state/restore_lease.rs:526` — `cas://` is a
> passthrough, `r2://` downloads the whole `.tar.gz` first), so the memory image
> is always local — which is precisely when the preview flag DOES select UFFD,
> and (per the table above) precisely when UFFD is slower.
>
> **Nothing in the code prevents this pessimization; the operator does. Leave
> `ATO_RUNNER_UFFD_PREVIEW` off on any runner that restores from a whole-fetched
> artifact.**
>
> UFFD pays off only once memory is demand-paged from a remote object store
> instead of whole-fetched — the deferred **R2-direct paging** work. That page
> source does not exist on the product path yet: `UffdMode::Cas` builds a remote
> read-through only under `ATO_FC_UFFD_REMOTE` (a test-only env read in
> `FirecrackerBackend::restore`), and `CasSource::ensure_local`
> (`crates/snapshot/src/uffd_page_server.rs`) early-returns when `remote` is `None`.

### The other two UFFD lanes (`ato run`, development only)

There is **no `ato run --experimental-uffd` CLI flag** — the only two hits for
that string on `nightly` are a test docstring and an aspirational row in
`docs/ready-state/uffd-productization-roadmap.md`. Every UFFD lane is an env var,
and the two `ato run` lanes do *not* gate identically
(`crates/cli/src/application/pipeline/phases/run.rs:3925-4000`):

| env var | lane | binding check | memory-locality check |
|---|---|---|---|
| `ATO_READY_STATE_UFFD_AUTO_PREVIEW=1` | U15 auto-select — `decide_mem_backend` chooses | ✅ `capsule_no_bindings` | ✅ `local_cas_has_memory` |
| `ATO_READY_STATE_UFFD_PREVIEW=1` | U11 forced preview | ❌ none | ✅ bails: `"UFFD preview requires the memory image in the local CAS; it is not present."` (`:3988`) |
| `ATO_RUNNER_UFFD_PREVIEW=1` | P1 Connected Runner (§4 above) | ✅ `declares_required_bindings` refuses (#1127) | ✅ **full** `has_all_chunks` sweep (#1127) |

Auto-select takes precedence over the forced preview when both are set
(`run.rs:3925`). Both are off by default (`…/ready_state/flags.rs:62-82`).

All three lanes end at the same place — the boolean they compute is passed as
`RestoreReadyStateInput.uffd_preview` (`run.rs:4018`) — so since #1127 the backend's
three gates apply to **all** of them. That closes the U11 forced lane's
binding-invariant hole too: combined with `ATO_READY_STATE_BINDINGS_PREVIEW=1` it can
still *ask* for UFFD on a binding-required capsule, but `uffd_preview_mode` refuses
and the restore runs on File.

⚠️ The two `ato run` lanes' own locality checks are still **first-chunk probes**, not
full residency sweeps — `memory.chunks.first()` + `store.has_chunk` (`run.rs:3934`,
`:3983`, `crates/cli/src/application/ready_state/diagnostics.rs:55-58`, which calls
itself "a cheap, honest liveness check"). They are now only a pre-check: a
partially-resident image (interrupted fetch) has chunk 0 present, so it passes them,
and the backend's `has_all_chunks` sweep then refuses UFFD and restores on File. So
the U11 lane's `bail!` still fires when chunk 0 is missing, but a half-fetched image
no longer aborts the run — it silently downgrades to File.

## Source of truth

- `crates/capsule/src/foundation/types/ready_state.rs` — `SnapshotConfig`
  fields, `DEFAULT_STABLE_*`, `is_valid_probe_path` / `validate_probe_paths`.
- `crates/snapshot/src/manifest.rs` — `RestoreContract` + `WarmupRecipe` +
  `effective_*` + `content_ready_path_or`.
- `crates/snapshot/src/firecracker.rs` — `warmup_paths()` (the build-time loop),
  `probe_ready` / `http_status_ready` (the shared 2xx/3xx probe),
  `uffd_preview_mode()` (the P1 gate), and the restore `content_ready` wait.
- `crates/snapshot/src/mem_backend_selector.rs` — `decide_mem_backend`, the pure
  placement-contract selector. It is the canonical statement of the UFFD rule:
  local CAS residency is a **precondition** (`memory image not in local CAS →
  File`), binding-required → File, remote read-through never auto-selected. It is
  consulted on the CLI auto lane only, **not** on the runner path (see §4).
- `tools/snapshot-builder/src/main.rs` — `warmup_from_manifest` /
  `warmup_from_env` (the two lanes).
- `crates/cli/src/application/runner_agent.rs` — the `RESTORE_PROF` line.

## Operational gotchas (rebuilding warmup snapshots on staging)

These bit us during the first staging rollout — encode them in any repeat.
Items 4–7 describe **ato-api / D1 state**, which is not in this repo: they are
recorded operational context from that rollout, not claims verifiable from the
`ato` tree. Re-confirm against ato-api before relying on a schema detail.

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
