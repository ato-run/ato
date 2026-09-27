# Staging orphaned lease recovery — 2026-09-27

## Symptom

The portable Trilium App (`cinst_01M2YH7FDA5Q37911BKY5C10BF`,
`cinst-tfgnfdkubznb7xh2.stg-app.ato.run`) refused every start with
`start_error=portable_dispatch_failed`. The failed Run rows showed only the
coarse code; the persisted cause was not recorded (now fixed, ato-api#698).

## Cause chain

1. `run_01M3GGB1GP857XP8VS0VFXZCXQ` (lease `01M3GGB1GPTKK5RQZ59K8JRQZX`)
   went `ready` at 04:00 UTC on Runner `01KX0SWDPP2GA41NEXQXNDCC0D`
   (`ubuntu-sugamo-staging`, slot `staging-fc-0`).
2. From ~04:39 UTC the staging API returned intermittent HTTP 500 on
   `/v1/runners/:id/heartbeat`, `/v1/runner-leases/:id/status`, and
   `leases/next` (bursts until ~07:33; the API-side trigger was not
   identified from the runner side).
3. The worker's serve loop ended on an API error, `finish` stopped the
   container gracefully (host log 04:39:06, exit_code 0), but both the
   `stopped` and the `failed` terminal reports returned 500.
4. The journal entry was dropped anyway because removal was gated on
   `stop_confirmed` alone — the defect fixed in this PR. The lease stayed
   `ready`, `reclaimAbandonedWriters` never saw it, and the state slot's
   writer stayed held (`state_writer_held` → `portable_dispatch_failed`).
5. The 05:27 idle-sleep sweep stamped `stop_requested_at` and moved the Run
   to `stopping`, but no live worker watched the lease anymore, so nothing
   acked it. The 07:33 service restart recovered an EMPTY journal — exactly
   the evidence hole — and the orphan persisted.

## Remediation applied

Host evidence first: no container, no process, no journal entry for the
lease — the workload was confirmed stopped (graceful stop logged at
04:39:06). With that, the Runner's own token delivered the ack the worker
could not send:

```
GET  /v1/runners/:id/leases/open   # confirmed the orphan: status=ready, stop_requested=true
POST /v1/runner-leases/:id/stopped {reason, stopped_at: actual stop time, cleanup: {process_terminated, proxy_stopped, slot_released} all true}
```

Result (verified in D1): lease `stopped` at the true stop time, Run
`stopped`, grant `aborted`, slot writer released, runtime route
`detached`. The next wake dispatches normally.

## Remaining known-stuck slots on staging (separate instances)

- `isslot_01M2T88K929AGNFPM8SKZAHV17` — quarantined since 2026-09-18
  (run/lease failed). Needs runner recovery or
  `POST /v1/admin/state-slots/:id/release-quarantine` with an audited reason.
- `isslot_01M2T7W9MRNKQVA8Q2KYYT3AWY` — same, run/lease cancelled.
- `isslot_01M2QFSB39ER0M4ZRJMF4F2FA3` — writer held since 2026-09-17 with a
  `cancelled` lease. `reclaimAbandonedWriters` ignores `cancelled`
  (a cancel may precede teardown+commit, so it is deliberately not terminal
  evidence); freeing it also needs runner/operator evidence.

## Follow-ups noted, not in this PR

- `POST /v1/runner-leases/:id/recovery` settles grants and ends the lease
  but leaves the Run row `stopping` and does not repair terminal resources
  (meter interval, proxy binding, always-on restart). A recovered Run is
  therefore still a zombie in "Running now" and its compute interval stays
  open.
- `GET /v1/runners/:id/leases/open` exists as the designed startup
  reconciliation list, but no worker consumes it; recovery still relies on
  the local journal alone.
