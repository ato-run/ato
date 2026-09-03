# B1 Formation — staging acceptance record

Executed 2026-09-03 against real staging. No fake control plane: real
`staging.api.ato.run`, real D1, real R2, real `runner_leases`, a real Connected
Realization Worker and a real build sandbox on `ubuntu-sugamo`.

`B1_BASELINE`: ato `f637974e` · ato-api `c4d30f9` · ato-pwa `0c1785e`

## The claim

> A pinned GitHub source is formed into a canonical ComputeSchema and an
> immutable workspace, and P3 launches a Run from that Schema alone.

It holds. `POST /v1/internal/compute-instances/:id/runs` now takes `{}`.

## Environment

| | |
|---|---|
| API | `ato-store-staging` `da29f393-aa56-489b-9c3b-4ceae55d8a2f` |
| Migrations | `0195`–`0201` |
| Source | `ato-run/fixture-fastapi-sqlite-process` @ `922b112f63f58678a807aff0920690d113331199` |
| Workspace materialization | `sha256:95818fc8ece974f6849fcc77f3b3f0433e513f92d0e9a6ef7766771512aab1be` (112 MB) |
| ComputeSchema | `csch_01M1KE6Y4QNCPC1TD3351YW1N6` |
| Instances | `cinst_b1_a` / `_b` / `_c`, all on that one schema |

## Results

| | Scenario | Result |
|---|---|---|
| A | contract | **PASS** — forbidden fields (incl. nested `secret_value`), mutable ref, escaping subdirectory, `publish_enabled`+network, `oci_image`, unknown protocol: all typed refusals against the real API |
| B | source pinning | **PASS** — a full commit is required; `main` and a short SHA are refused as `ATO_ERR_FORMATION_SOURCE_NOT_PINNED` |
| C | closure identity | **PASS** (unit) — the same tree from two different archives yields one closure; the closure is neither the archive digest nor the tree digest |
| D | idempotency | **PASS** — a resubmitted key returns the same job |
| E | stale attempt | **PASS** — `409 formation_attempt_superseded`; a duplicate completion is `409 formation_result_already_accepted` |
| F | build sandbox | **PASS** — source read-only, output writable, host sentinel invisible, env not inherited, denied network genuinely fails, allowed network reaches the index |
| G | Static Formation lane | **PASS** — a pinned source formed into a manifest/receipt/blobs bundle by the canonical materializer; schema registered as `static_web`, no process realization, **0 Runner runs** |
| H | Python Formation → Run | **PASS** — unattended worker: claim → pinned fetch → build → publish → schema; then `{}` in, `{"status":"ok","db_path":"/data/app.sqlite"}` out |
| I | state continuation | **PASS** — three Runs of one instance, fences 1→2→3, revisions chained, `note-A` survives to the third |
| J | tenant isolation | **PASS** — B and C start empty on the same schema; B never sees A |
| K | existing artifact compatibility | **PASS** — both Static instances open unmodified, P0 bridge live, the untouched one has **0 runs** |
| L | rollback | **PASS** — off → shadow → primary-unallowlisted (degrades to shadow) → primary-allowlisted → **off**, then an existing instance still runs and its state is intact |

## The build, in full

```
pinned commit  922b112f
  -> source fetched, extracted read-only at /src
  -> CPython 3.12.7 provisioned to /opt/ato/toolchains  (NOT the host's 3.14)
  -> venv --copies --without-pip, libpython vendored beside it, ensurepip
  -> pip install -r requirements.txt
  -> import fastapi, uvicorn, pydantic, main   ->  OK, fastapi 0.115.6
  -> packed deterministically -> sha256:95818fc8…  (112 MB)
  -> R2 -> FormationResult -> ComputeSchema
  -> Run, with no caller-supplied artifact
```

## Eight defects the staging run found

None of these would have surfaced from reading the code.

| Symptom | Cause |
|---|---|
| `cannot exec /bin/sh` | usrmerge: `/bin` is a symlink into `/usr`, and only `/usr` was bound |
| `Permission denied` on `/bin/sh` | the Landlock policy and the bind list had drifted; now one list |
| `failed to get random numbers to initialize Python` | Landlock lacked the `/dev` bwrap supplies with `--dev`, so `/dev/urandom` was unreachable |
| `error: linker cc not found` building `pydantic-core` | the plan used the host's `python3` (3.14, no wheel) instead of the resolved 3.12.7 |
| `Temporary failure in name resolution` | `/etc` bound wholesale carries `resolv.conf` as a dangling symlink |
| a 471 MB workspace | `install_only` ships a 207 MB unstripped libpython; `install_only_stripped` is 38 MB |
| a venv that cannot start | symlinks cannot cross the artifact format, and `--copies` alone leaves the loader without libpython |
| `exited before becoming ready (exit status: 3)` | the materialized workspace **lost its execute bit** in transport — the same workspace served from the build directory and failed from its content address |

The last one is the one worth keeping: it was only findable by putting a real
Formation artifact through a real Runner.

## The artifact was three times larger than it needed to be

Worth recording, because the mistake generated most of this record's second
half. The workspace VENDORED the whole CPython — libpython plus the standard
library — and that single decision produced, in order:

- 198 MB artifacts (85 MB of which was a `lib64 -> lib` link materialized as a
  copy of the whole `lib` tree)
- past the control plane's 100 MB request cap, so a multipart upload
- past a Worker's 128 MB memory, so a streaming download and a digest check
  moved to the Runner
- a venv that could not start, because `--copies` leaves the loader without
  libpython and symlinks cannot cross the artifact format

`runtime_requirements` already said `python = 3.12.7`, the Runner already
provisions it at a fixed shared path, and the runtime sandbox already binds that
path read-only. The build was ignoring all three.

The artifact now carries the source and its installed dependencies and nothing
else: **11 MB**, down from 198 MB. The launch names the provisioned interpreter
and is told where its dependencies are through `PYTHONPATH`.

## Not done, and not claimed

**No shadow comparison against the existing Static path.** The Static lane runs
and produces a correct bundle, but it has not been run side by side with the
path that produced the current Static instances, so nothing here shows the two
agree. The gateway and semantic comparator are implemented and tested; the
comparison itself has not been performed.

**Untrusted public Python publish is not enabled**, per ADR-018. `uv sync` needs
the network, bwrap cannot express "the package index and nothing else", and the
contract refuses `publish_enabled` together with a dependency-resolving network.
The Python lane is allowlist-only on staging.

**`uv sync` was not exercised.** The fixture carries `requirements.txt`, so the
pip lane ran. The `uv` lane is compiled and unit-tested, not run on staging.

## Production

Unchanged.
