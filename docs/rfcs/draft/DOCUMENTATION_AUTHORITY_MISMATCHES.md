# Detected mismatches: documentation authority and implementation

Status: Draft / maintainer decisions required
Date: 2026-10-26
Scope: documentation audit at dev `0c93d81a7f86140868fbe84efb63538360a9cba1`.

This register does not amend accepted specifications, wire formats or code.
It records decisions to make separately. “Implemented” describes source at the
audit baseline, not deployment or feature availability.

## M1 — Capsule identity across profiles

The accepted [Computation Architecture](../accepted/COMPUTATION_ARCHITECTURE.md)
states `Capsule = ComputationRef` and describes the only canonical semantic
value. The [portable application draft](PORTABLE_APPLICATION_BUNDLE_V3.md)
and [implemented reader](../../../lib/objects/src/portable_bundle.rs) instead
use `root_contract_ref` in wire v3/v4. Workspace direction explicitly preserves
both profiles without casting their refs.

Decision needed: formally scope the accepted architecture/bundle documents to
the computation-root profile and adopt an application-profile contract, or
decide a different normative relationship. Current explanatory docs distinguish
profiles; no accepted text was changed to manufacture agreement.

## M2 — Public packaging / Formation commands

The accepted [CLI lifecycle](../accepted/CAPSULE_CLI_LIFECYCLE.md) says `encap`
is the only portable Materialization boundary. Current
[CLI commands](../../../apps/cli/src/lib.rs) also include `pack`, `form`,
`reform` and durable portable `app` operations.

Decision needed: clarify which accepted lifecycle is profile-specific and
adopt the implemented public application command contract. Keep the
implemented commands; documentation cleanup does not remove behavior.

## M3 — Initial v3 scope versus later implementation

The v3 draft's initial scope is static/single-D without external bindings,
runtime dependencies or state. Later
[dynamic derivations](PORTABLE_DYNAMIC_DERIVATIONS.md),
[authoring v2](PORTABLE_APPLICATION_AUTHORING_V2.md) and
[Instance snapshots](PORTABLE_INSTANCE_SNAPSHOT.md) describe broader paths.
`PortableBundleIndex` implements v3/v4 profiles, multiple Derivation roots and
an optional v4 Instance snapshot ref.

Decision needed: consolidate versioned profile specifications and their
extensions with an explicit adoption record. Do not rewrite initial historical
scope into a claim that every route or deployment is supported.

## M4 — Draft status and receipt authority

The [RFC index](../README.md) explicitly states that drafts are non-normative
even if merged. `BoundContract`, `BoundDerivation` and
[`ContractVerificationReceipt`](../../../lib/formation/src/verify.rs) are
implemented, but their application-profile authority remains spread across
drafts/ADRs. The receipt type permits absent bundle hash and execution evidence
on some creation paths; merely naming a schema cannot prove attempt linkage.

Decision needed: adopt a consolidated Contract/Derivation/Receipt specification,
including required execution provenance per acceptance path, and audit actual
issuers against it. Keep optional fields and existing schemas unchanged here.

## M5 — Snapshot implementation-status claims

Older concept pages describe every VM checkpoint as future work. The
[VM materializer](../../../extensions/materializers/vm-snapshot/src/lib.rs)
reports restore support through a backend and RunnerCapabilities; workspace
snapshots remain verify-only. This is a stale explanatory status claim, rather
than grounds for promising universal capture/restore. The snapshot overview
now distinguishes these paths. The [VM RFC](VM_SNAPSHOT_MATERIALIZATION.md)
still requires formal adoption.

## M6 — Retired commands and unverified release instructions

Five former host runbooks invoke `ato runner` commands absent from the current
public CLI. They were archived with their historical text. The root
`AGENTS.md` already marks its release instructions unverified, including absent
workflow and old crate paths; it also retains old Desktop/branch descriptions.
Agent/release-policy changes require a separate owner review; they were not
silently rewritten during docs cleanup.
Current navigation points to actual workflows, current Runner instructions
and operating procedures.

## State and side-effect boundary

K equality plus verification establishes the observations in K. It does not
establish arbitrary state migration, exactly-once side effects, patch status
or safe rollout. The
[stateful transformation evidence](../../archive/2026/ops/stateful-transformation-v1.md)
uses fresh process memory and expressly excludes restart persistence. Any
stronger claim needs a separate state/effect contract and measured evidence;
there is no adoption proposal for a new universal continuity schema here.
