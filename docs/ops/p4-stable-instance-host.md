# P4 — Stable Instance Host, automatic wake/sleep, and Schema Update

**Status: implemented and unit-verified. NOT yet verified end to end from a
browser.** The blocking step is named at the bottom.

The central proposition, which every design decision below answers to:

> A person needs to know only their App. They never learn that a Run exists,
> and the same URL always continues where they left off.

```
ComputeInstance I1
      │ stable identity
      ▼
https://cinst-….app.ato.run
      │
      ├── no ready Run ──► wake ──► Run R7
      │
      └────────────────────────────► proxy
```

## P4-A — the URL means the App, not the Run

Before this, a hostname resolved to a Run: `app_proxy_bindings` bound one
`host_slug` to one `run_id` and one `upstream_url`. The link a person kept was
only as durable as the process behind it.

    InstanceEndpoint   permanent. One per (instance, export). What a URL means.
    RuntimeRoute       ephemeral. Where it points, and for how long.

**There is no `compute_instances.active_run_id`, on purpose.** A single
authoritative pointer must be written by whoever wins a race and cleared by
whoever notices a death — and every one of those writers is a Runner that may
itself be gone mid-write. The pointer then reads "running" forever, or a
straggler clears it after a newer Run already claimed it.

`generation` removes the pen. Installing allocates the next generation under a
UNIQUE constraint, so two wakes cannot both win. The current route is the
HIGHEST generation that reached `ready`. A late report from an older generation
loses by construction — not by being slow, and not by anyone remembering to
clean up after it.

Lane membership is decided by the PRESENCE of an endpoint rather than a `kind`
column: a Static App never has one, so the two paths cannot disagree about
which they are.

## P4-B — single-flight wake

Ten requests at a sleeping App must produce one Run. "Check for a Run, then
start one" cannot do it: all ten checks read the same empty answer before any
of them writes.

So the wake is a row, and a partial UNIQUE index over `status = 'in_flight'`
lets exactly one INSERT win. The losers read the winner and wait — on the
ROUTE, never on the wake row, because the route is what decides reachability
and it is also what a DIFFERENT wake would produce. A joiner whose leader died
still succeeds when somebody else's Run wins the endpoint.

The deadline is not optional. A Runner that dies mid-wake leaves `in_flight`
behind, and without a deadline that row holds the endpoint's only wake slot
forever — the App becomes permanently unstartable, which is the failure an
in-flight marker exists to prevent, inverted.

The leader does NOT mark the wake succeeded when the Run is dispatched. A
dispatched Run is not a ready one, and claiming success there would tell every
joiner to stop waiting for something that has not happened.

## P4-C — the cold request

    GET the stable host
      → no ready Run → wake → materialize → start → readiness → route installed
      → the ORIGINAL request continues

Bounded wait, and the answer is always the same shape: this URL either serves,
or gives a retryable reason. **Never a redirect to a Run-specific URL** — that
hands the person a link that dies with the process, which is exactly what P4-A
exists to prevent. The abstraction has to hold hardest at the moment it is most
tempting to leak.

A timeout leaves the wake in flight deliberately: the Run may yet come up, and
it is the wake's deadline that decides when the endpoint may be woken again,
not one request's patience.

## P4-D — idle sleep

The order IS the correctness argument:

    stop choosing this route for new requests   (draining)
      → ask the Run to stop
      → the Runner drains, kills the process subtree, and only THEN packs
        state, CASes the InstanceState head, releases the writer fence
      → the route detaches with the Run's teardown

`draining` exists as a status distinct from `detached` for the first step: a
request arriving mid-sleep must not be handed to the process being torn down.
It wakes a NEW Run on the same URL, and the person never learns anything was
being shut down.

This module packs nothing itself. Process-subtree-zero BEFORE state is packed
is P3's sequence, and a state packed while the process still lives is captured
mid-write — corruption that surfaces on the NEXT wake, far from its cause. It
never forces: a forced stop skips the drain and the seal, and what a person
just typed is exactly what would be lost.

A stop that fails leaves the route `draining` rather than restoring it to
`ready`. Out of resolution is the safe side.

## P4-E — recovery

    Run may die. Instance must survive.

| failure | what closes it |
|---|---|
| Runner death during wake | wake deadline reaped; the endpoint is wakeable again |
| Runner death while ready | reclamation is where a death is NOTICED, so it detaches routes there |
| Runner death during sleep | a drained route never returns to resolution |
| wake timeout | retryable; the wake keeps its own deadline |
| readiness failure | superseded by the next generation |
| concurrent wake | single-flight index |
| wake vs delete | a `deleting` instance is 410, not 503 — it will not succeed on retry |
| sleep vs incoming request | `draining` takes it out of resolution first |
| old Run late callback | `markRouteReady` refuses a superseded or detached route |
| stale RuntimeRoute | resolution takes the highest READY generation only |

Deleting an instance detaches its routes BEFORE dropping state: a route left
`ready` past that point would answer, wrongly, from whatever the process still
held in memory.

## P4-F — derived runtime state

Two questions, two fields, and only one of them is stored:

    status         what the owner decided — available, deleting, deleted.
                   Persisted, because intent is a fact.
    runtime_state  whether a process happens to be up. Read from the routes
                   every time it is asked.

Persisting the second would create a truth that goes stale exactly when it
matters: when a Runner died without telling anyone, and the row still says
`ready` while nothing answers.

## P4.5 — Schema Update

    ComputeSchema S1 + InstanceState D1  →  ComputeSchema S2 + the SAME state

What a person sees: "Update available / [Update]", then new UI at the same URL
with their data still there.

What this is NOT is `UPDATE compute_instances SET current_schema_id = ...`. A
bare pointer swap has no answer for the Run still writing under the old schema,
no record of what the state was before, and no way back.

    1. claim        single-flight
    2. quiesce      draining first
    3. the seal     P3's stop
    4. checkpoint   read the head AFTER the seal, so it names the state the old
                    schema FINISHED with
    5. advance      compare-and-set from S1
    6. wake         by the ordinary cold-request path

Step 6 is a boundary, not laziness: an update that also owned starting the new
Run would make every wake bug an update bug. That separation of failure domains
is why Schema Update is P4.5 rather than part of P4.

A schema from another Compute is refused — keeping the data and replacing the
program that owns it is not an update, it is handing one App's data to another.

## The Runner half

The runtime-launch lane used to report ready with NO url, and the comment said
why: a process realization was reachable only on the Runner's own loopback, and
synthesizing a public address would publish a URL that served nothing. It ended
"the stable instance host is P4."

It now binds `surface_listen` — the loopback port its ingress slot already
forwards to — and reports `public_base_url`, the hostname it forwards from.
Both are existing Runner configuration. The control plane still never picks a
host port: only the Runner knows what is free, and the stable URL must not
depend on it. The API validates the reported hostname against that Runner's
active ingress slots, so a misconfigured base URL is refused rather than
believed.

## What ships

| | |
|---|---|
| Migrations | `0202` endpoints + routes, `0203` wake operations, `0204` route activity, `0205` schema updates — applied to staging |
| Staging API | `08d1dd5c-df52-4901-bc61-c9c3c2d15eb1` |
| Tests | P4-A 12, P4-B/C 9, P4-D 7, P4-E 8, P4.5 9; regressions green (dynamic-instance-state 22, personal-apps 23, app-proxy 40) |
| Runner | `ato-connected-realization-worker`, 81 tests green |

## Not verified, and what it needs

The PMF Golden Scenario — create → open → add data → sleep → wake elsewhere →
data still there → update → same URL, new UI, same data → sleep → wake — has
NOT been run from a browser. Every part is implemented and unit-tested; what is
missing is a publicly reachable process instance on staging.

Established by measurement:

- `ubuntu-sugamo` IS `rstg002`, running Caddy and a token-based Cloudflare
  Tunnel whose routes are dashboard-managed.
- `s2-rstg002.ato.run` → `127.0.0.1:8422` is live (200 on
  `/.well-known/ato-runner-ingress`) and the port is free.
- The runtime-launch Runner `01M1JVXA4DX08ZR0VJQXV3BZFA` has the
  `runtime_launch` capability and **no ingress rows at all**, so nothing it runs
  is publicly reachable.

### What a verification run needs, measured

The D1 half is settled and reversible, and was exercised end to end:

- `runner_ingress.base_hostname` and `runner_ingress_slots.hostname` are both
  UNIQUE among non-revoked rows. A slot is owned by exactly ONE Runner, which
  is correct — so borrowing one is explicit: revoke the incumbent slot row,
  insert the borrower's, and restore afterwards. Scripts:
  `p4_ingress_borrow.sql` / `p4_ingress_restore.sql`.
- The Cloudflare Tunnel routes are per-hostname, not wildcard: `s3-rstg002`
  and `s4-rstg002` do not resolve at all. A NEW slot therefore cannot be added
  without a Cloudflare dashboard change, which is why the verification borrows
  an existing one rather than creating capacity.
- All three routed slots (`s0`/`s1`/`s2-rstg002`) belong to Runners that are
  live and heartbeating, so any borrow degrades a real lane for its window.
- The runtime-launch worker must be BUILT ON THE RUNNER: the release binary
  from a darwin workstation is not executable there.

The step that remains is starting the runtime-launch worker on the runner host
with the routed slot's port and hostname:

    ./target/release/ato-connected-realization-worker \
      --public-base-url https://s2-rstg002.ato.run \
      --surface-listen 127.0.0.1:8422 \
      --work-root … --slot-id p4stable --max-slots 1

Starting a long-lived process on a shared staging host is the one action this
session could not take on its own. Staging was returned to its exact prior
state after the attempt: the s2 slot is back with its original Runner and
`active`, no borrower rows remain, and all three runner services are running.
