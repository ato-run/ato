# PMF Golden Scenario — verified from a browser

**Status: PASS. All 18 steps, driven from Chrome against staging, 2026-09-03.**

The proposition under test was not a feature list:

> A person needs to know only their App. They never learn that a Run exists,
> and the same URL always continues where they left off.

## What was run

| | |
|---|---|
| App | `cinst_b1_b` → `https://cinst-b1-b.stg-app.ato.run/` |
| Compute | `cmp_01M1H0VTQQXSFKK198DE9XVBKG` (FastAPI + SQLite expenses fixture) |
| S1 | `csch_01M1KE6Y4QNCPC1TD3351YW1N6` — workspace `sha256:95818fc8…`, argv `/app/.venv/bin/python` |
| S2 | `csch_01M1KK70F1C9R5SYA6MHS5YGHD` — workspace `sha256:cb56195a…`, argv `/opt/ato/toolchains/python/3.12.7/bin/python3` |
| Staging API | `ac52ed90-965d-41b4-926d-264d7df522ae` |
| Runner | `01M1JVXA4DX08ZR0VJQXV3BZFA` on `ubuntu-sugamo`, publishing on the borrowed `s2-rstg002.ato.run` slot |

S1 and S2 differ in BOTH workspace and argv — the update is a real change of
program, not a relabelling.

## The 18 steps

| # | step | evidence |
|---|---|---|
| 1–2 | App exists, listed | `GET /v1/compute-instances` returns it with `runtime_state` |
| 3 | open the stable URL | cold request → wake → `run_01M1MJ9R7JFMRF0WT1K3VM9SS1` → uvicorn on 8422 → `GET /health` 200 → route generation 1 `ready` → the original request forwarded. FastAPI answered. **No redirect to a Run URL.** |
| 4 | add data | `P4-before-sleep` written. `note-B` from the B1 acceptance ten hours earlier was ALREADY there — the state restored across the whole B1→P4 gap |
| 5–6 | close, idle sleep | cron tick 21:30 → route `ready` 21:30:11 → `detached` 21:30:57; run and lease `stopped`; the Runner's last line is the seal: `state=app_data fence=2 isrev_01M1KECMAX… -> isrev_01M1MJX33…` — packed AFTER the process was gone |
| 7–9 | open the same URL again | automatic wake → `run_01M1MJYKVV283B9EZ6HV2F80R8`, generation 2 — a DIFFERENT Run on the SAME URL — and both records present |
| 10 | add more data | `P4-after-wake` |
| 11–12 | update | `POST /v1/compute-instances/cinst_b1_b/schema` → 200 in ~4s |
| 13–15 | same URL, new code, same data | generation 3 runs S2 (different workspace, different argv); generation 2 on S1 detached; all three records intact, including the one written under S1 |
| 16–18 | sleep and wake again | generation 3 sealed `<unchanged>` and detached; a later cold request woke generation 4 in **7 seconds**; `P4-final` added; all four records present |

Two schema updates were run, in opposite directions. Nothing was lost.

## A defect the run found, and its fix

The first update recorded checkpoint `isrev_01M1MJX33…` while the Run it had
just quiesced went on to seal `isrev_01M1MK0E1…` **two seconds later**.

`performRunStop` REQUESTS a graceful stop; it does not wait for one. The Runner
drains, kills the process subtree, packs the state and CASes the head all after
that call returns — so reading the checkpoint straight afterwards names the
head as it stood when the stop was merely asked for.

The data was never at risk: the seal completed and the head advanced. The
CHECKPOINT was wrong, by one revision — so a rollback would have returned to a
state missing the last thing the person typed, while the code claimed it named
"the state the old schema finished with".

Quiesced now means the **writer fence is released**, not that a stop was asked
for. The fence is the only honest signal that a seal FINISHED: the Runner
releases it last, after the process is gone and the head is advanced. A
terminal Run row can be stamped before the pack, and an unchanged head is
indistinguishable from a Run that had nothing to write.

Re-verified after the fix: the next two updates recorded
`isrev_01M1MK0E1…` and `isrev_01M1MKFB4…` — each exactly the live head at the
moment, with `active_writer_run_id` null.

A second defect was found before the run even started: a process App that had
never run had no endpoint, so its FIRST request went down the Static path and
served the artifact of a process App — the one moment in an App's life where
being wrong is most visible.

## Observed once, not reproduced

One cold request during an in-flight schema update returned
`ERR_CONNECTION_CLOSED` instead of a retryable answer, and the immediate retry
succeeded. A deliberate reproduction — update, then request 1.5s later — did
NOT reproduce it: that request returned all four records on the new schema.
`wrangler tail` showed no exceptions. Recorded as seen, not diagnosed.

## Cost of a cold start

Wake to serving: **7 seconds**, for a 112MB workspace already materialized on
the Runner. First-ever materialization will be slower; this number is the warm
Runner case and should be read that way.

## Staging afterwards

The verification borrowed the `s2-rstg002.ato.run` ingress slot, because the
Cloudflare Tunnel routes are per-hostname and no spare routed slot exists.
Everything was returned:

    s2 slot                  → 01M0YQGVA0Q8YDXEH3VG0ZZBPV / slot 1 / active
    P4 runner ingress rows   → 0
    ato-runner-agent         → active
    ato-hosted-runner-slot1  → active
    ato-hosted-runner-slot2  → active
    listener on 8422         → none
    borrowed worker          → stopped

`cinst_b1_b` is left on S2 with four records, asleep.

## What this milestone is

    Personal Compute v0 / PMF-ready

P5 OCI is not required for it. The next thing this earns is dogfooding, then
5–10 external people.
