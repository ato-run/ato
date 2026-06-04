# RFC: Capsule Core Model

**Status**: Draft
**Target milestone**: v0.6.x / v0.7 foundation
**Issue**: ato-run/ato#503 (umbrella ato-run/ato#502)
**Related**: #490 (Execution Graph Model, internal IR), #501 (provider boundary), #498/#499 (realization), #508 (installed-state DB), #509 (cross-device placement)

---

## Summary

This RFC defines the **Capsule** as Ato's core abstraction and the model around
it: the execution contract, object lifecycle, placement, installed state,
relaunch, and cross-device placement.

> **A Capsule is not a package format. A Capsule is an application-owned
> execution contract** — the application's own, machine-readable
> self-description of the *launch conditions* it needs to run.

The one-line articulation this RFC fixes:

> A Capsule describes **what is needed**. The installed-state DB records **what
> already exists in this environment**. The cross-device placement index answers
> **where the conditions can be satisfied**. Ato uses all three to decide where
> to install, launch, and relaunch.

Ato is therefore not a task runner. A task runner *runs a list of commands and
fails at runtime*. Ato reads an application's launch conditions, decides whether
and where they can be satisfied, realizes them, proves what held, and keeps the
capsule re-realizable later — with install admission, a launch resource ledger,
fast relaunch, and cross-device placement.

This RFC is the single Capsule Core Model RFC. It absorbs the former Object
Model issue (#504) and summarizes the install/placement epics. Detailed
implementation is tracked by the follow-up epics (§12). It does **not** claim
full reproducibility — see §8.

This RFC is docs-only. It fixes the model and vocabulary; it does not implement
detectors, providers, the DB, or the index.

## 1. Capsule is an application-owned execution contract

Today an app's launch conditions are scattered across README, setup scripts,
Dockerfile, `.env.example`, CI config, cloud config, and the user's local
machine — and the user reassembles them by hand. Ato inverts this: the app
declares its launch conditions as one machine-readable contract, and Ato
resolves, verifies, places, and reconstructs them.

| | Today | Ato |
|---|---|---|
| App | launch conditions scattered across README / Dockerfile / .env / CI / cloud config | one machine-readable execution contract |
| User | hand-adapts the environment | no manual environment adaptation; may grant secrets, consent, or select placement |

Infrastructure (servers, GPU) is only one category of those conditions.

## 2. Launch conditions taxonomy

```
runtime        Python / Node / Deno / Java / CUDA / browser / OCI runtime
dependencies   lockfiles / node_modules / model weights / native libs
configuration  env vars / config files / launch args / cwd / feature flags
secrets/auth   API keys / OAuth / tokens / DB passwords — as secret-ref / grant, never the value
state          database / cache / workspace / model cache / persistent volume
network        allowed hosts / ports / ingress / egress / DNS / local service graph
hardware/sys   CPU arch / memory / disk / GPU / VRAM / OS / kernel capability
policy         filesystem permission / host capability / sandbox level / consent
placement      local / desktop-companion / Ato Cloud / external runner / BYOC
```

`requires` (dependency on other capsules / services / state / secrets) differs
from `requirements` (capabilities the *execution location* must have:
GPU/CUDA/VRAM/RAM/OS/arch/disk/egress/…). Both are conditions in the contract.

## 3. Capsule / Placement / ExecutionGraph

The user-facing subjects are **Capsule** (the app's execution contract) and
**Placement** (where Ato satisfies it). **ExecutionGraph (#490) is internal** —
the canonical representation of those conditions across declared / resolved /
observed layers, used to resolve, verify, reconstruct, and diff. It is not the
user-facing subject.

> Capsule = an app self-describing its launch conditions; Ato finds where they
> hold and launches; ExecutionGraph is the internal verification form.

## 4. Capsule object model + lifecycle

A Capsule is **not a format**. The formats are surfaces:

| Surface | Role |
|---|---|
| `capsule.toml` | **declaration** format |
| `capsule.zip` | **transport** format |
| `capsule://…@version-id` | **reference / locator** (point-in-time, no floating alias) |
| ExecutionGraph | canonical IR (#490) |
| Receipt | realization proof |

Object shape:

```
Capsule {
  metadata, interface { provides, requires }, requirements,
  realization, placement, policy, identity, evidence
}
```

Lifecycle:

```
Declared → resolve → Resolved → realize → Realized → launch → Running
        → install/pin → Installed Capsule Revision → relaunch → Re-realized
```

Identities are distinct: `declared_capsule_id` / `resolved_capsule_id` /
`realization_id`; and on the install axis `install_profile_key` (stable,
user-facing) / `install_revision_id` (immutable installed revision) /
`capsule_instance_key = H(install_profile_key, install_revision_id,
resolved_execution_id)`. OS shortcuts and Dashboard entries point to
`install_profile_key`, never to `execution_id`.

## 5. Lockfile / Installed-State DB / Placement Index (three layers)

These three are distinct and must not be conflated:

| Layer | Nature | Use |
|---|---|---|
| Capsule lock / resolved contract | portable, shareable, reproducible **description** of a resolution | share / review / CI / regenerate |
| **Installed-State DB** (device/provider-local) | record of **what was materialized** + what is claimed | admission, fast relaunch, GC |
| **Cross-device Placement Index** | redacted **cross-device query** layer | which device/provider can satisfy a capsule |

**Ato does not re-read scattered lockfiles on every operation.** At install
time, Ato ingests the resolved state into the installed-state DB; admission,
relaunch, GC, and placement then **query the DB/index**. The index is a fast
map; the selected provider's local DB performs final admission.

## 6. Install admission

Before download/build, Ato checks the capsule's conditions against the existing
installed claims on the target device/provider:

```
capsule conditions × DeviceResourceIndex (existing installed claims) = admission decision
```

e.g. a 20GB capsule on a device with 10GB free-after-claims is **rejected up
front** — not after downloading 10GB — and Ato offers: free space / install on
Ato Cloud / use external runner. Port 7860 already claimed → remap or prompt.
Missing GPU → cloud placement. Missing secret grant → an unsatisfied launch
condition surfaced pre-launch (secret *values* never enter the graph, receipt,
logs, or the placement decision; placement may depend on whether the selected
provider can project the secret reference safely).

## 7. Relaunch and re-realizability

Install is a **re-realization contract**, not a saved launcher: pin the resolved
revision + immutable input hashes, bind mutable state and secret grants as
contracts, and store a **Launch Resource Ledger** for fast relaunch.

The ledger is a **reusable relaunch plan, not exclusive ownership**:

```
Port claim is not permanent port ownership.
Secret ref is not secret value storage.
State binding is not immutable artifact identity.
Provider placement is not guaranteed availability.
```

A stable logical endpoint (`ato://app/<id>/http` via Ato ingress) keeps the
user-facing URL stable while the actual backend port may remap. Relaunch reads
the ledger, does cheap checks (ledger exists, state binding present, secret grant
valid, provider available, port usable-or-remappable, runtime/tool cache
marker), and launches immediately if all hold — otherwise it repairs / remaps /
re-places the broken entry.

Relaunch does not fully re-solve or re-hash every immutable node on every start.
The fast path uses the Launch Resource Ledger and cheap invalidation markers.
When a marker is missing, stale, corrupted, or strict/costly policy applies,
Ato falls back to full verification, repair, remap, or re-placement before
launch. Ato must never trust a cache once its identity marker is invalidated.

> Ato guarantees that an installed/resolved capsule can be re-realized later, or
> fails with a typed explanation of which condition is no longer satisfiable.

This is the third of three guarantee levels: **can launch now** (§6 placement),
**was launched with proof** (§8 receipts), **can launch again later** (this
section). The relaunch receipt records whether the launch was an exact
re-realization, a repaired realization, or a re-placement.

## 8. Realization contract and reproducibility discipline

> Ato guarantees that a resolved capsule can either reconstruct an equivalent
> launch envelope or fail with a typed explanation — it must not pretend
> success.

Each node of the resolved capsule is classified `materializable` / `host-bound`
/ `state-bound` / `unavailable`, with pre-launch content-hash verification
(source / runtime / runtime tool / dependency-output / build-artifact /
filesystem-view). Receipts become **materialization proofs**, not logs.

Discipline (no overclaiming): this RFC does **not** claim full reproducibility,
and Ato does not claim identical behavior. While runtime observation is
unimplemented (#490), receipts do not synthesize `observed_execution_id` and
`GraphCompleteness::Complete` is not emitted. Reproducibility is classified
(pure / host-bound / state-bound / time-bound / network-bound / best-effort),
and pre-observation classification is conservative — `network-bound` means
egress is allowed in the envelope, not that the network was accessed.

## 9. Interface and composition

Typed `provides` / `requires` make a capsule composable. A `requires` is
satisfied not only by another capsule's `provides` but also by a user secret
grant / provider capability / managed resource / state binding — secrets, GPU,
network egress, and ports are **not** things a peer capsule provides.

Composition is **policy-aware** (not a free monoid): it must never silently
widen host-fs / network / capability beyond consent. Its output is **not only a
service graph** but an **aggregate execution contract** — the combined
requirements, required secrets, state bindings, network policy, ports, provider
capabilities, and policy constraints — which then drives placement. (Detail:
#505 interface, #506 composition.)

## 10. Provider projection boundary

A Capsule projects onto a realization provider (source-native / OCI / web /
wasm / managed-cloud / external-runner). The provider is an implementation
detail of realization, never the identity object:

```
image digest      != capsule identity
container id       != execution identity
docker/podman run  == derived projection, not source of truth
```

OCI facts are graph nodes (`OciImageNode`, `OciRuntimeNode`,
`FilesystemViewNode`, `StateBindingNode`, `NetworkPolicyNode`,
`EntrypointNode`); the generated run invocation is derived output. **Ato is not a
container wrapper; it is a capsule realization engine, and OCI is one provider.**
(Detail: #501.)

## 11. Relationship to the Execution Graph Model (#490)

`#490` is the internal canonical IR, identity, receipt, replay, and
provider-projection *implementation*. This Capsule Core Model defines the
application-facing contract; `G_declared` / `G_resolved` / `G_observed` are the
Declared / Resolved / Observed Capsule graphs. The Capsule is the subject;
ExecutionGraph is how Ato verifies and reconstructs it.

## 12. Implementation epic map

This RFC fixes the model. Each chapter is implemented by a follow-up epic:

| Chapter | Epic |
|---|---|
| §6 admission, §7 relaunch, installed-state DB, ledger | **#508** |
| §5 cross-device placement index | **#509** |
| §9 interface | **#505** |
| §9 composition | **#506** |
| §10 provider boundary | **#501** |
| §8 realization contract + materialization verifier | **#498 / #499** (on the #490 track) |

Implement order: this RFC → #508 + #509 (product impact; #508 gates #498) →
#505 + #506 → #501 → the #490 internal-IR track.

## Positioning

> Docker packages applications. Nix makes build outputs reproducible. **Ato makes
> application execution conditions self-describing, placeable, and
> reproducible.**
>
> **Ato turns application setup from a user-managed environment problem into a
> machine-solvable execution-conditions problem.**

## Acceptance (of the implementation, not this doc)

The model is realized incrementally; the per-epic acceptance lives in #498–#509.
This RFC is accepted when the project agrees on: the execution-contract
definition, the launch-conditions taxonomy, the Capsule/Placement/ExecutionGraph
roles, the object lifecycle and identities, the three-layer (lockfile /
installed-state DB / placement index) split, install admission, the relaunch /
re-realizability contract, cross-device placement, interface/composition,
provider boundary, and the implementation epic map.

## Open questions

1. Schema/version of the installed-state DB and how it ingests lockfile-equivalent data.
2. Which capability facets are required for a provider to be a valid placement per runtime.
3. Redaction policy for host paths in the placement index vs the local DB.
4. Lease semantics (TTL, renewal) for ports / storage / GPU slots / provider slots.
5. How much placement is decided account-side (index) vs provider-side (final admission).
