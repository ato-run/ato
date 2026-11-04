# Research tour

Status: Current technical reference.

This page is a short path from the problem Ato addresses to the model, the
existing evidence and the questions we want reviewed. It links to the detailed
pages instead of repeating them. Ato is the computing interface; the web
service at [ato.run](https://ato.run/) is a separate product surface built on it.

## 1. The problem

Someone you know made a small tool for their own use. You want to use it with
your data, change one thing about it, and still be able to open it next week,
on another machine, after the original author has moved on. Today the usual
units of sharing are source code, container images or setup instructions. The
recipient reconstructs a point the author already reached, and the data,
processes and services around that point have to be reconstructed as well.

Ato asks whether the *point itself* — not only the recipe for reaching it —
can be the unit that is named, shared and resumed, independently of the
process, filesystem, container, snapshot or host that realizes it.

## 2. What Ato is testing

The model is described in [Capsule Process Model](../theory/capsule-process-model.md).
Two hypotheses matter for review:

1. **A persistent continuation independent of its realization.** Sealing a
   computation yields an immutable, addressable Capsule; resuming it yields a
   mutable Run. The Capsule's logical identity is separate from the physical
   means used to realize it (Replay, filesystem reconstruction, process
   checkpoint, VM snapshot).
2. **Contract-indexed equivalence.** Two realizations could count as the same
   point when they satisfy the same declared Contract K, even if their bytes
   differ. This is a hypothesis about how equivalence should be defined, not a
   behavior the current implementation checks for arbitrary artifacts.

Neither hypothesis is a claim of novelty. Residual-process semantics,
continuations, passivation calculi, distributed snapshots and local-first data
ownership are established areas; the open question is whether this particular
combination is a workable, day-to-day unit for sharing and continuing
software. Sections on [prior work](#9-related-work-and-what-we-want-to-learn)
record the connection.

## 3. The model in one page

```text
C ──α──▶ C'        a computation evolves through an interaction
seal(C) → κ        an immutable, addressable continuation point
resume(κ) → Run    a mutable evaluation that can diverge from the same κ
C ⊨ K              a Contract is a predicate over a computation
StateK(C)          state is a purpose-specific projection, not identity
```

- A **Computation** is what can still happen from a point, not the history that
  produced it ([Computation](../concepts/computation.md)).
- A **Capsule** is a sealed continuation point; a **Run** advances it. A
  **Record** is evidence of an observed step, not the current computation.
- A **Materialization** is a physical way to realize a Capsule; it does not
  define identity ([Materialization](../concepts/materialization.md)).
- A **Port** is a typed interaction boundary; a **Binding** maps it to a
  physical resource at realization time.
- For portable applications, **K** is the bound Contract, **D** is a declared
  Derivation (one way to realize K), and a verification **receipt** records
  the observations for a Run ([application contracts](../concepts/application-contracts.md)).

## 4. Two identity profiles

The repository currently has two roots, and they are not interchangeable:

| Profile | Logical identity | Transport | Local CLI surface |
|---|---|---|---|
| Computation-root | `ComputationRef` from canonical ComputationObject bytes | wire v2, `index.root` | `init`, `resume`, `stop`, `encap` |
| Portable application | `ContractRef` from canonical BoundContract bytes | wire v3 `ato.portable-application/1`, wire v4 `ato.portable-application/2` | `pack`, `form`, `reform`, `app`, `export`, `export-plan` |

`run` is a reader for either profile: the bundle reader dispatches on
`index.version` and refuses an unknown version instead of falling back. `form`
writes content-addressed Formation artifacts rather than a `.capsule` file;
`pack` is the bundle-producing front end.

The whole-file SHA-256 of a `.capsule` identifies transport bytes, not either
logical root. The [profile map](../concepts/application-contracts.md#profile-map)
and the [detected mismatches](../rfcs/draft/DOCUMENTATION_AUTHORITY_MISMATCHES.md)
record which commands belong to which profile and which scope decisions
(M1–M4) are still open. The accepted computation-root RFCs have not been
rewritten to manufacture agreement.

## 5. Data and identity: one JSON object

Take an application that stores a single document:

```json
{"items":[{"id":1,"name":"first","quantity":2}]}
```

What makes a later copy "the same data" depends on the Contract, not on the
file that happens to carry it. An exact-content observation (for example an
`ato.data.json@1` digest) and a semantic observation ("the item list contains
these entries in this order") are different Contracts and must not be
conflated. The file, embedded JSON resource or browser storage is a
realization D; the declared data slot and its read/replace operations are the
Port; the run-time path, permissions and external store are the Binding.
Secrets stay runtime inputs and do not become identity.

A saved-data snapshot that is bound into K creates a new K′ instead of
rewriting the frozen K of the original bundle. Implemented today: capture of
declared browser state, JSON Data Resources and Assets, local
materialization, and Hosted restore before launch. Not implemented or not
verified: browser-state flush from a live Hosted Instance, PWA export UI,
filesystem-state injection for Process/OCI routes, and a proven Hosted
round-trip. The worked example and the full implemented/unproven list are in
[State continuity](../concepts/state-continuity.md#data-and-identity-one-json-object).

## 6. What has been verified

The entries below are selective evidence, not a general portability claim.
Each links to the record that states its environment and limits.

| Claim | Evidence | Environment | What it does not show |
|---|---|---|---|
| A supplied bundle verifies its source identity and HTTP observation against K, and writes a receipt | [Getting started](getting-started.md), `samples/interop-static.capsule` | macOS, CLI built from the reviewed commit; both observations satisfied | other platforms, routes or dynamic applications |
| The same frozen K is preserved while source and D change | [Transformation v0 evidence](../archive/2026/ops/contract-preserving-transformation-v0.md) | Linux CI, bubblewrap, Python 3.12.7 | that K equality means the same feature or that the change is safe to roll out |
| The same K holds with a different source and a different internal database schema | [Stateful v1 evidence](../archive/2026/ops/stateful-transformation-v1.md) | Linux, fresh in-memory JSON vs in-memory SQLite; CRUD and authorization scenario | restart persistence, volume transfer, hosted runs |
| Selected portable imports and exports work between accounts | [Portable Capsules](https://ato.run/docs/capsule/) implementation review, 29 October 2026 | selected staging examples, static/Python/OCI | general availability, saved-data round-trips, other platforms or apps |

The archived experiments are historical records with frozen fixtures and
negative cases; they were not re-executed for this page. A fresh-instance
success is not evidence of state preservation across a restart, and a
`fully_satisfied` receipt covers the observations in that Contract only.

## 7. Reproduce the first result

No account is required:

```sh
git clone https://github.com/ato-run/ato.git
cd ato
cargo build -p ato-cli --bin ato --locked
mkdir -p .tmp/getting-started
target/debug/ato run samples/interop-static.capsule --no-open \
  --verification-receipt .tmp/getting-started/receipt.json
```

The receipt names the bundle transport hash, K, selected D, target and each
observation. See [Getting started](getting-started.md) for the expected
output, the authoring path, and the failure boundaries.

## 8. Independence: Ato and ato.run

Ato the interface is not the same thing as the ato.run service. The local CLI,
a durable local Instance, a user-owned Connected Runner and the hosted service
are different paths with different dependencies (account, central API,
dependency acquisition, network, data location, secrets). Running on your own
machine is not the same as continuing without ato.run, and the current
export/import flow moves an app between accounts *through* the service; it
does not by itself prove service independence. The dependency table is in
[Runner and Hosting](runner-hosting.md#dependencies-and-independence).

The repository records dependency and hosting boundaries instead of promising
a general hosted-to-local data migration.

## 9. Related work and what we want to learn

| Prior work | Point of contact | What Ato must not overstate |
|---|---|---|
| [Malleable Software](https://www.inkandswitch.com/essay/malleable-software/) (Ink & Switch) | reducing the step from using software to changing it; tools composed by users; execution and sharing infrastructure as part of the argument | that sealing and portability alone deliver an end-user modification experience |
| [Local-first software](https://www.inkandswitch.com/essay/local-first/) (Ink & Switch) | user ownership and continuity; servers are not ruled out | that storage location, execution location, control plane and post-separation use are interchangeable |
| [Livelymerge object model](https://www.inkandswitch.com/livelymerge/notebook/lm-03/) (Ink & Switch research notes dated 2026-08-07) | treating a CRDT document as a program heap, persisting and sharing state and code together; documents open questions about multiple-execution side effects, invariants and long histories | that treating app and data in one frame is novel by itself; compare the observation scope of K, replaceability of realizations and side effects |
| Process calculi, passivation (e.g. Kell), continuations, distributed snapshots (e.g. Chandy–Lamport) | residual-process semantics, capture of located processes, snapshots of consistent global state | a formal bisimulation, universal capture/restore, or an implemented distributed-snapshot protocol |

These are research points of contact. They do not imply that Ink & Switch or
any cited author endorses Ato.

Questions we would most like reviewed:

1. Is Contract-indexed equivalence the right notion for "the same point", or
   does it fail on realistic workloads (side effects, concurrency, secrets)?
2. Should application identity and saved-data identity share one Contract, or
   remain separate contracts with an explicit relationship?
3. What evidence would make a state-continuity claim reviewable without
   implying general migration?
4. When a bundle declares several Ds, who should choose, and what must the
   receipt record so that a Run is reproducible later?
5. Is the two-profile split (ComputationRef vs ContractRef) an acceptable
   layering, or a sign that one root should absorb the other? See M1–M4 in
   [Detected mismatches](../rfcs/draft/DOCUMENTATION_AUTHORITY_MISMATCHES.md).
6. Which artifact is most useful to read first: this tour, the
   [theory draft](../theory/capsule-process-model.md), or the archived
   transformation evidence?

## 10. What is not claimed

- General cross-host portability, process-memory restoration, or that every
  application runs everywhere.
- Preservation of existing user data across restarts, upgrades or migrations.
- That a matching Contract certifies security, license compliance, or safe
  side effects.
- That ato.run production offers every path described in this repository.
- That the cited researchers or projects endorse Ato.

## See also

- [Computation](../concepts/computation.md), [Capsule and Run](../concepts/capsule.md),
  [application contracts](../concepts/application-contracts.md), and the
  [glossary](../glossary-reference.md).
- [Specifications](../specifications/README.md) for the accepted RFCs and the
  portable-application implementation track.
- [Runner and Hosting](runner-hosting.md) and
  [Connect a user-owned Runner](connected-runner-self-host.md).
