# ADR-017 — Legacy Formation Adapter

Status: draft · Scope: `ato` (builder), `ato-api` (Formation Plane)

## Context

Formation — turning a Source into a `ComputeSchema` — currently runs on a
builder daemon that exists **only** on the pre-restructure `crates/` topology
(`deploy/replay-static-lane`, ~24,000 lines across 20 modules:
`authoring_runtime`, `wizard_api`, `wizard_wire`, source archive transport,
upload, claim eligibility, static web producer/emit/transport).

Current `ato` has no counterpart. `nightly`'s `tools/snapshot-builder` is a
27-line snapshot registration CLI; `main` carries the static web producer
*library* (restored by #1316) but no daemon.

The two trees are 420 commits apart with a completely different layout
(`crates/` + `sidecars/` versus `apps/` + `extensions/` + `lib/` + `services/`
+ `tools/`).

## Decision

**Treat the running daemon as a Legacy Formation Adapter: a black box behind a
fixed contract, kept in service and not ported.**

Do not migrate its code into the current tree. When Formation is eventually
rebuilt, rebuild it against the contract below rather than moving 24k lines of
Authoring architecture across a topology change.

### Why not port it now

- It would import a large amount of old Authoring architecture **before** the
  `ComputeSchema` / `ComputeInstance` / `InstanceState` / `Run` boundaries have
  settled, and those boundaries are what the Runtime Track is establishing.
- The diff would be dominated by changes unrelated to runtime correctness.
- Much of it would be ported and then not used by Dynamic Compute.
- It is **not on the critical path**: Dynamic Compute reaches a workload
  through the Runner, not through this builder.

## The contract this adapter is held to

Everything below is what the rest of the system may depend on. Anything else
about the daemon is an implementation detail of a component scheduled for
replacement.

### Input

- An authoring job claimed from `ato-api` (`/v1/capsule-snapshots/authoring`),
  authenticated by a builder agent token and fenced by a builder lease.
- An Effective Build Plan, including `static_web_output` when the capsule
  manifest declares `[outputs.static_web]`.
- A materialized Source Revision.

### Output

- A **Static Web Materialization**: a canonical manifest plus
  content-addressed blobs in R2, and a bundle receipt. Its digest is the
  artifact's identity.
- A **Program Intent** with a generated `capsule.toml`, for the detect lane.
- Typed failures surfaced through the setup session and build attempt rows.

### What the adapter may put inside an artifact

Only what the delivery edge can rely on without rewriting structure:

- the entry document's declared instance state lane (placeholder + bridge tag,
  gated on `STATIC_WEB_INSTANCE_STATE_BRIDGE_ENABLED`);
- the replay bridge, gated on `STATIC_WEB_REPLAY_BRIDGE_ENABLED`;
- otherwise, the built bytes unchanged.

Adding anything else to the artifact requires updating this ADR first.

### Binary provenance

The deployed binary is identified by SHA-256 and source commit, recorded in
`docs/ops/static-web-instance-state-lane.md`, together with the rollback
binary's own digest. A deploy that cannot state both is not a deploy.

## Cutover conditions

Formation may be rebuilt on the current topology (Formation Track F2/F3) only
once **all** of these hold:

1. **P3 has landed**, so the Formation output a Dynamic Compute actually needs
   is known from a working FastAPI + SQLite case rather than predicted.
2. A `FormationResult` contract (F1) is fixed, expressed in current types, and
   both the legacy adapter and any replacement can satisfy it.
3. The replacement produces a **byte-identical** Static Web Materialization
   for a fixture the legacy adapter also builds — same manifest digest.
4. Both can run against staging simultaneously, so cutover is a routing change
   and rollback is the reverse routing change.
5. The legacy binary and its rollback remain installed for at least one
   release after cutover.

Until then, changes to the legacy adapter are limited to what a Formation gap
in an accepted fixture actually requires — narrow, contract-preserving
additions, never opportunistic modernization.

## Consequences

- The old topology stays alive in one deploy branch, with a documented
  contract and provenance instead of being folklore.
- Formation improvements are deferred rather than smuggled into runtime PRs.
- The eventual rebuild is scoped by a contract derived from a working system.
- Risk accepted: the daemon's dependencies age, and a security fix in them
  would need building from the old branch. Acceptable while it serves staging
  only; it is a blocker for production Dynamic Compute.

## Non-goals

- Porting, refactoring, or re-testing the legacy daemon.
- Changing its snapshot / VM lane.
- Deciding the shape of the replacement — that is F1/F2, after P3.

## Measured finding: v1 cannot commit a process build's output (blocks P3.0)

Established by running the FastAPI fixture through the real staging Formation
three times, not by reading code. Each run was refused by the v1 subset, and
each refusal was correct.

1. Declaring the pinned interpreter as a `ToolchainRequirementV1` emits
   `[tools]`, which ADR-015 §7 refuses: *"a pinned tool is a dependency, and
   per-dependency derivation and output digests have no producer yet, so its
   identity could not be measured."* Removing the declaration and
   single-sourcing the pin inside the materializer clears this one.
2. With `[tools]` gone, the same intake refuses `[[build.steps]]`: *"this build
   has no declared guest output root, so its projection could not be
   committed."*

The second refusal is the structural one. `[[build.steps]]` is admitted only
when `outputs.static_web` is present (`ManifestV1::validate_for_interactive_capture`),
because the v1 manifest has exactly **one** output projection and it is
web-shaped. `NormalizedProgramIntentV1::build_output_roots` is already the
general concept — a `Vec<WorkspacePathV1>` — but the manifest projection that
fills it reads `outputs.static_web.root` and nothing else.

A Python process app needs precisely what does not exist: a declared guest
output root (`/app`, holding the source and its `.venv`) whose contents can be
measured and committed.

There is no honest reuse available. `produce_static_web_bundle` does
content-address an arbitrary tree, but its manifest is a web-serving manifest
(`entry_path`, `spa_fallback`) and its transport registers a *static web
materialization* in ato-api. Pushing a `.venv` through it would make a process
app indistinguishable from a static web app to delivery — a category error, not
a shortcut. It is the only output-projection producer the builder has.

**Consequence for the tracks.** Carrying installed Python dependencies as a
Formation artifact requires a second, generic output projection plus its digest
producer and its commit path in ato-api. That is the FormationResult contract
(F1), which is already sequenced *after* P3. So P3 does not get its dependency
story from the legacy v1 Formation, and P3.1–P3.5 proceed on a
dependency-free process fixture: that still exercises Generic Process Dynamic
Compute, InstanceState, writer fencing and sleep/wake, which is what the
Runtime track owns. Dependency materialization stays open, on F1, and must not
be reported as done.
