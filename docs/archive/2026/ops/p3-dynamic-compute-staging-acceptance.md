# P3 Dynamic Compute — staging acceptance record

Executed 2026-09-03 against real staging. No fake control plane anywhere: real
`staging.api.ato.run`, real D1, real R2, real `runner_leases`, a real
Connected Realization Worker on `ubuntu-sugamo`, real `bwrap`, real CPython.

## Environment

| | |
|---|---|
| API | `ato-store-staging`, version `78ef2e7b-aecb-4fbd-a6d2-2a47f1698635` |
| D1 | `ato-store-db-stg`, migrations `0196`–`0200` applied |
| R2 | `ato-store-artifacts-stg`, prefix `state/v1/artifacts/` |
| Runner host | `ubuntu-sugamo`, kernel 7.0.0-27, bubblewrap 0.11.1 |
| Worker | `feat/runtime-launch-worker-wiring` @ `5f1c583` |
| Runner id | `01M1JVXA4DX08ZR0VJQXV3BZFA` (`p3-acceptance-sugamo-2`), 1 slot |
| ComputeInstance | `cinst_01M1H8KNWF97MXSRC0DSZR1W40` |
| State slot | `isslot_01M1JX1YA5ENEC0FJWXYGKF6AV` |
| Workspace artifact | `sha256:bebcb7df8998511b3105c50836ea824864315924bf68ae6ec2399ebe7152a741` |

The existing staging hosted runners were left untouched; this acceptance ran on
a separate registered Runner with its own credential, work root and slot id.

## Runs

| Run | Lease | PID | Port | Fence | Start rev | Result | Grant |
|---|---|---|---|---|---|---|---|
| R1 `…FGJWE9M` | `…C37JRY462JAF` | 2635218 | 43481 | 3 | — | `…87SXDE8TEYE` | committed |
| R2 `…6CCXK796BXG` | `…983EEMP8VQ5DSJ` | 2635639 | 33587 | 4 | `…87SXDE8TEYE` | `…QFAD1PVB56HR` | committed |
| R3 `…ZD8YNX5K5EF9` | `…80FBWWA5EJVW54` | 2635775 | 37935 | 5 | `…QFAD1PVB56HR` | none (unchanged) | released |
| RF1 `…W5P2S1NDHHD` | `…ZH6QF9WW080YG` | — | — | 6 | `…QFAD1PVB56HR` | none | aborted |
| R4 `…C5J18DRR60T` | `…YVEZTGBYD2Z` | 2635968 | 41215 | 7 | `…QFAD1PVB56HR` | none | aborted, **reclaimed** |
| R5 `…WF7Q7586SZX` | `…ZHY006VTG4X5WD` | 2636650 | 40121 | 8 | `…QFAD1PVB56HR` | none | released |

Every Run has a different id, lease, PID, allocated port and workspace
directory. No two share a fence.

## Results

### A — cold start · PASS

R1 dispatched through `/v1/internal/compute-instances/:id/runs`, claimed by the
real Runner, workspace materialized from its content address, launched under
bwrap, `GET /health` → `200 {"ok": true}`.

Observed argv (abridged):

```
bwrap --unshare-all --share-net --die-with-parent --new-session
      --proc /proc --dev /dev --tmpfs /tmp
      --ro-bind /usr /usr  (+ lib, lib64, resolv.conf, hosts, ssl)
      --tmpfs ~/.ssh --tmpfs ~/.gnupg --tmpfs ~/.aws --tmpfs ~/.kube …
      --ro-bind <lease>/workspace                    /app
      --bind    <lease>/workspace/.ato/state/app_data /data
      --ro-bind <worker binary>                       /.ato/runner
      --chdir /app  /.ato/runner sandbox-exec --policy … -- python3 -B /app/app.py
```

### B — first persistent write · PASS

`POST /notes {"body":"first"}` → 201, `GET /notes` → `[{"id":1,"body":"first"}]`,
both over HTTP to the Runner-allocated port. Stopped through
`POST /v1/internal/runs/:id/stop` — the real control path, not SSH — after which
the process, the process group and the lease directory were all gone, and
revision `…87SXDE8TEYE` existed with artifact
`sha256:c2194e76…`.

### C — fresh wake · PASS

R2 on the same ComputeInstance: different run, lease, PID and workspace, fence
4 > 3, `start_revision_ref` = R1's revision. `GET /notes` returned `first`
**before any write**. `POST second` then stop produced revision `…D1PVB56HR`
whose parent is `…87SXDE8TEYE`.

### D — third realization · PASS

R3: `GET /notes` → `first, second`. This is P3's central proposition, on real
staging:

> same ComputeInstance · different Run · different PID · different workspace ·
> same continuing state

R3 changed nothing, so it minted **no revision** and released the slot — the
no-op path, proven incidentally.

### E — stale writer · PASS

| Attempt | Result |
|---|---|
| commit with fence 4 on a live fence-5 lease | `409 writer_fence_stale` |
| commit naming the wrong parent revision | `409 state_parent_revision_mismatch` |
| commit / redeem from a terminal lease | `409 forbidden` |

Head stayed `…D1PVB56HR`, epoch 5, and the winning artifact was untouched.

### F — readiness failure · PASS

RF1 launched `python3 -c "sys.exit(3)"`. The Runner detected the early exit
**instead of waiting out the 60s timeout**:

```
workload for run … exited before becoming ready (exit status: 3)
```

Grant `aborted`, no revision, head unchanged, and the next Run started
immediately at fence 7.

### G — authorization and sandbox · PASS

| Probe | Result |
|---|---|
| no token | 401 |
| another token | 401 |
| terminal / wrong lease | 409 |
| invented `state_key` | 404 `state_grant_not_found`, **no slot created** |
| foreign artifact digest | 404 |
| own workspace digest | 200 |
| another workspace digest | 404 |

Inside the sandbox:

| Probe | Result |
|---|---|
| write `/data` | OK — and the file appears in the host state directory |
| read `/app/app.py` | OK |
| write `/app/evil.py` | refused (read-only bind) |
| read host sentinel `~/p37-sentinel.txt` | not in the namespace |
| write `/home/ekohsuke/escaped.txt` | "succeeded" — into the sandbox's own ephemeral root; **no host file was created** |
| write `/etc/p37` | refused |

After the Run, neither `escaped.txt` nor `/etc/p37` existed on the host, and
`/app/evil.py` never reached the workspace.

### H — worker crash · PASS

R4 held fence 7 and was serving. The worker was `SIGKILL`ed.

- the workload died with it (`--die-with-parent`); nothing was left listening
- **no release request was ever sent** — a SIGKILL gives no chance to
- the slot stayed held by a Run that no longer existed: the permanent-stuck
  condition
- once the lease expired, the next dispatch reclaimed it. Grant `aborted` with
  `reclaimed_at` set, slot freed, R5 acquired at fence 8
- the crashed Run's late attempts were refused: `409 state_grant_spent` for both
  commit and redeem
- `GET /notes` on R5 still returned `first, second` — head never moved

## Known limitations, deliberately not closed here

- **`materialization_ref` is supplied by the caller.** The ComputeSchema has no
  field for a Dynamic materialization; `/v1/internal/` accepts a content address
  and refuses anything else, including the all-zero placeholder. **B1 deletes
  this parameter.**
- **No stable instance URL.** A process realization reports ready with no
  `ready_url` — the honest report the API already models — and the acceptance
  reaches it on the Runner's loopback. **P4.**
- **Formation installs no dependencies.** The fixture is standard-library only,
  because the v1 authoring subset cannot commit a process build's output
  (ADR-017). **B1/F1.**
- **Landlock is defence in depth, and unverified here.** Containment in this
  acceptance came from the bwrap mount namespace, which is what the probes
  measured. The shim applies Landlock and logs when it cannot; a kernel that
  refuses it still runs namespace-isolated.
- **Two copies of the sandbox policy exist** (`ato-sandbox` and nacelle's), and
  de-duplication is blocked on nacelle building again.
