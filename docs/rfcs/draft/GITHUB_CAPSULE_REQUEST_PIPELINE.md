---
title: "GitHub Capsule Request Pipeline"
status: draft          # draft | accepted | archived
date: 2026-07-13
author: "@egamikohsuke"
ssot:                  # anchor code paths this pipeline extends (code is authoritative)
  - "crates/snapshot/src/rootfs_builder.rs"
  - "crates/snapshot/src/manifest.rs"
  - "crates/capsule/src/foundation/blob/tree_hash.rs"
  - "crates/capsule/src/foundation/types/manifest_v03.rs"
related:
  - "A1_SOURCE_TREE_PROFILE.md"
  - "SOURCE_MATERIALIZATION_SPEC.md"
  - "EXECUTION_IDENTITY_SPEC.md"
  - "ADR-011-source-materialization-placement.md"
  - "ADR-012-capsule-lifecycle-column.md"
  - "ADR-013-manifest-validator-wasm-split.md"
  - "HASH_AND_PROVENANCE_POLICY.md"
  - "../accepted/A1_BLOB_HASH.md"
---

# GitHub Capsule Request Pipeline

## Status

**Draft / Proposed.** This is the umbrella specification for turning a public
GitHub repository into a published, runnable Ato capsule without a human author
hand-writing a `capsule.toml`. It records six decisions that were taken as a set;
the deep contracts for each live in the companion documents listed under
`related`. Nothing here is implementation authority yet — it is direction.

This document is the map. Read it first, then descend into:

| Topic | Document | Type |
|-------|----------|------|
| 1. Source-tree hash profile | [A1_SOURCE_TREE_PROFILE.md](A1_SOURCE_TREE_PROFILE.md) | SPEC |
| 2. Where materialization runs (decision) | [ADR-011](ADR-011-source-materialization-placement.md) | ADR |
| 2. Materialization contract | [SOURCE_MATERIALIZATION_SPEC.md](SOURCE_MATERIALIZATION_SPEC.md) | SPEC |
| 3. Execution identity | [EXECUTION_IDENTITY_SPEC.md](EXECUTION_IDENTITY_SPEC.md) | SPEC |
| 4. Capsule lifecycle column | [ADR-012](ADR-012-capsule-lifecycle-column.md) | ADR |
| 5. Manifest validator WASM split | [ADR-013](ADR-013-manifest-validator-wasm-split.md) | ADR |
| 6. Submission integration + state machine | this document (§4–§7) | SPEC |

## 1. Overview

A user pastes a public GitHub URL. The platform resolves it to an immutable
commit, materializes the source deterministically, derives a candidate
`capsule.toml`, validates it, runs a headless QA boot, and — if everything
passes — publishes a Community capsule that anyone can Run. Every step is
content-addressed and receipted so the result is auditable and reproducible.

The pipeline spans three execution surfaces that already exist in the platform:

- **Cloudflare Worker (ato-api edge)** — request intake, ref resolution, commit
  pinning, GitHub Trees listing, the submission state machine, and R2-mediated
  storage. No repository *content* is materialized here (see [ADR-011](ADR-011-source-materialization-placement.md)).
- **Snapshot builder (claim/ack lease lane)** — the existing snapshot-builder
  worker gains two new job kinds, `source_materialize` and `candidate_qa`, that
  do the CPU/IO-heavy work (checkout, canonicalize, hash, archive, QA boot).
- **Capsule runtime (`crates/capsule`, `crates/snapshot`)** — the manifest
  validator core (extracted to WASM) and the ReadyStateManifest execution-identity
  fields.

## 2. Scope

### In scope

- End-to-end flow from a public GitHub URL to a published Community capsule.
- The submission record, its coarse operator status, and the fine-grained
  `pipeline_state` machine (§4).
- Deterministic identity of the materialized source, the recipe, and the
  execution (delegated to companion specs).
- MVP policy gates: license allowlist, per-user quotas, kill switches, and the
  rejection rules that map a repository to a `blocked_*` terminal.
- The two candidate-generation lanes (static web vs docker-import reuse) and the
  constrained-generation contract that bounds the AI's role.

### Out of scope

- Private repositories, non-GitHub providers, and any provider other than
  `github.com` (the schema leaves room; the MVP rejects them).
- Repositories that require executing repo-authored build commands outside the
  two MVP lanes.
- Paid/commercial capsule flows, entitlements, and the `license.sync`
  entitlement model (see [LICENSE_SPEC.md](LICENSE_SPEC.md) — a different
  concern from the SPDX *source* license gate in §6.4).
- The RunnerClass sentinel resolution that supplies `rootfs_base_id` /
  `guest_kernel_id` — a prerequisite tracked separately (see
  [EXECUTION_IDENTITY_SPEC.md](EXECUTION_IDENTITY_SPEC.md) §3).

## 3. Actors and data flow

```text
 user ──URL──▶ ato-api (Worker)
                 │  resolve ref → commit OID (immutable)
                 │  GitHub Trees listing (recursive-first)
                 │  create/update capsule_submissions row
                 ▼
        pipeline_state: requested → fetching
                 │
                 ▼  claim/ack lease lane
        builder job: source_materialize
                 │  pinned checkout → canonicalize → A1v2 hash → tar.zst
                 │  cross-check against API Trees listing (paths + OIDs)
                 │  API-mediated upload → R2 (source-archives/v1/...)
                 ▼
        pipeline_state: analyzing → generating
                 │  deterministic RepoAnalysisReport (ceiling)
                 │  constrained AI → candidate IDs only
                 │  deterministic renderer → schema-0.3 TOML
                 ▼
        pipeline_state: validating
                 │  capsule-manifest-core WASM validator (final gate)
                 ▼
        pipeline_state: qa_queued → qa_running
                 │  builder job: candidate_qa (headless boot on runner_class)
                 │  ReadyStateManifest w/ execution_id + teardown receipt
                 ▼
        pipeline_state: publish_ready → published
                 │  capsule lifecycle: draft → verifying → published
                 ▼
        Community capsule, Runnable by anyone
```

The three hashes that thread the whole pipeline together:

| Name | Value | Owner spec |
|------|-------|-----------|
| `materialized_source_tree_hash` | A1v2 digest (`sha256`) of the canonical checkout | [A1_SOURCE_TREE_PROFILE.md](A1_SOURCE_TREE_PROFILE.md) |
| `source_archive_hash` | `sha256` of the exact `tar.zst` bytes | [SOURCE_MATERIALIZATION_SPEC.md](SOURCE_MATERIALIZATION_SPEC.md) |
| `execution_id` | `blake3` over the canonical JCS of the identity facet set | [EXECUTION_IDENTITY_SPEC.md](EXECUTION_IDENTITY_SPEC.md) |

Hash-role split (consistent with [HASH_AND_PROVENANCE_POLICY.md](HASH_AND_PROVENANCE_POLICY.md)):
**`sha256` is identity (the A1 family); `blake3` is CapsuleFS CAS transport plus
the structural-id family that already backs `ReadyStateManifest::id()`**. The two
never substitute for each other.

## 4. Submission integration (topic 6)

### 4.1 No new API surface

There is **no** new `/v1/capsule-requests` endpoint. A GitHub capsule request is
an extension of the existing `capsule_submissions` table and the existing
`POST /v1/store/apply` entry point. The coarse operator-facing status machine
(the human review states already in the store) is unchanged; a new column
carries the fine-grained automation state.

### 4.2 `pipeline_state` machine

`pipeline_state` is a new column on `capsule_submissions`. Linear happy path:

```text
requested → fetching → analyzing → generating → validating
          → qa_queued → qa_running → publish_ready → published
```

Branch / terminal states:

| State | Kind | Meaning |
|-------|------|---------|
| `needs_launch_info` | branch (recoverable) | Generation cannot pick an entrypoint/port without user input. |
| `policy_review` | branch | License not on the allowlist, or a policy signal needs a human. |
| `blocked_repo` | terminal (blocked) | Repo shape unsupported (e.g. Trees `truncated=true`, submodules). |
| `blocked_policy` | terminal (blocked) | Disallowed content/license after review. |
| `blocked_incompatible` | terminal (blocked) | Manifest shape the MVP cannot run (e.g. `[packages]` workspace delegation). |
| `failed_internal` | retryable | Transient platform failure. Max 3 attempts, exponential backoff. |
| `expired` | terminal | A `needs_launch_info` submission left stale for 30 days. |
| `canceled` | terminal | User withdrew the submission. |

Rules:

- `failed_internal` retries automatically up to **3** attempts with exponential
  backoff; the fourth failure is surfaced, not retried.
- Admin retry increments `attempt_no` **on the same row** (no new submission).
- A `blocked_*` state is terminal for that `(repo, commit)` but a **new commit**
  re-opens the pipeline (see §4.4).

### 4.3 Submission record shape (delta)

New/extended columns on `capsule_submissions` (ato-api migration; recorded here
for cross-repo visibility):

- `source_provider` (`github`), `provider_repository_id`, `provider_owner`,
  `provider_repo`.
- `commit_algorithm` (`sha1` today; GitHub's object format), `commit_oid`.
- `pipeline_state`, `attempt_no`.
- `materialized_source_tree_hash`, `source_archive_hash`, `recipe_hash`,
  `execution_id` (filled as the pipeline advances).
- `target_capsule_id` (nullable; set when a request maps to an existing
  registered capsule — see §4.4).

### 4.4 Dedup and identity

Uniqueness is enforced by a DB unique index on:

```text
(source_provider, provider_repository_id, commit_algorithm, commit_oid)
```

- Two requests for the **same repo at the same commit** collapse to one row
  (idempotent intake — a duplicate `POST /v1/store/apply` returns the existing
  submission).
- A **new commit** for a repo that already maps to a registered capsule does not
  create a competing listing: it updates the submission with
  `target_capsule_id` pointing at the existing capsule, so the pipeline produces
  a new candidate/version for that capsule rather than a duplicate.
- `provider_repository_id` (GitHub's numeric repo id), not `owner/repo`, is the
  stable key — it survives repo renames and owner transfers.

### 4.5 Driving the pipeline

Advancement is driven by:

1. **`db.batch()` CAS** — each state transition is a compare-and-swap on
   `pipeline_state` inside a single D1 batch, so two concurrent drivers cannot
   double-advance a row.
2. **`waitUntil`** — the request that causes a transition kicks the next step in
   the background.
3. **The existing 15-minute cron sweep** — provides at-least-once recovery:
   any row stuck in a non-terminal state past its deadline is re-driven. This is
   the safety net that makes `waitUntil` best-effort rather than load-bearing.

**No new Queue is introduced.** A Cloudflare Queue is only added if measurement
shows the cron + `waitUntil` combination cannot keep up (see §8).

### 4.6 Builder QA lane

The builder consumes two job kinds on the **existing** claim/ack lease lane
(the same lane snapshot builds already use):

- `source_materialize` — see [SOURCE_MATERIALIZATION_SPEC.md](SOURCE_MATERIALIZATION_SPEC.md).
- `candidate_qa` — headless boot of the candidate capsule on a target
  `runner_class`, producing a `ReadyStateManifest` (with `execution_id`) and a
  **teardown receipt** that publication requires.

QA job uniqueness key (prevents duplicate QA work for an identical candidate):

```text
submission_id + attempt_no + source_tree_hash + recipe_hash
              + runner_class_id + qa_contract_hash
```

## 5. Candidate generation (topic 6, generation half)

### 5.1 MVP lanes

Two lanes only. Which one a repo takes is decided by a **step-0 eligibility
measurement** (out of scope here; the pipeline records the chosen lane):

- **Lane A — static web**: the repo already contains committed build output
  (e.g. a `dist/` or `public/` a static server can serve). **Zero repo command
  execution.** The safest lane; the MVP prioritizes it.
- **Lane B — docker-import reuse**: reuse the docker-import v1.7 path for repos
  that ship a usable container definition.

### 5.2 Constrained generation

The AI's role is deliberately narrow and cannot inject arbitrary manifest
content:

1. A **deterministic `RepoAnalysisReport`** is computed from the frozen source
   archive. It is a *ceiling*: it enumerates the only entrypoints, ports,
   framework signals, and file references the generator is allowed to choose
   from.
2. The AI returns **candidate IDs only** — selections into that bounded set,
   never free-form TOML.
3. A **deterministic renderer** turns the selected IDs into schema-0.3
   `capsule.toml`.
4. The **`capsule-manifest-core` WASM validator is the final gate** (see
   [ADR-013](ADR-013-manifest-validator-wasm-split.md)); a candidate that fails
   validation never reaches QA.

**Repository text is passed to the model as quoted data, never as
instructions.** README bodies, code comments, and file names cannot escape into
the prompt's instruction channel; prompt-injection from repo content is a
first-class threat (see §7).

## 6. Policy (topic 6, policy half)

### 6.1 License allowlist

MVP SPDX allowlist for automatic publication:

```text
MIT, Apache-2.0, BSD-2-Clause, BSD-3-Clause, ISC, 0BSD
```

Anything else routes to `policy_review` (human), not auto-block. `LICENSE` and
`NOTICE` files are **displayed on the capsule page** and **bundled into the
source archive**.

### 6.2 Provenance and labeling

- Every GitHub-request capsule is labeled **unofficial Community** unless the
  GitHub App owner-claim proves the requester controls the repo.
- A verified GitHub App owner claim upgrades the labeling; it does not change the
  pipeline.

### 6.3 Takedown

A takedown transitions the capsule `lifecycle` to `archived` (see
[ADR-012](ADR-012-capsule-lifecycle-column.md)), which revokes all run bindings
in the takedown D1 batch; the app proxy gates per-run, so archived capsules stop
being runnable at the next run attempt.

### 6.4 Quotas and kill switches

- Per-user quotas: **3 in-flight** requests, a daily cap, and a per-repo
  cooldown.
- **Kill switches at every stage** — intake, materialize, generate, QA, publish
  can each be independently disabled without redeploying, so a bad lane can be
  shut off in isolation.

## 7. Security

- **Prompt injection**: repository content is untrusted data. The
  constrained-generation contract (§5.2) means model output cannot become
  manifest content directly; the WASM validator is the schema gate; repo text
  never enters the instruction channel.
- **Public repos only**: materialization uses a GitHub App installation token
  scoped to public reads; no private content is ever fetched.
- **Deterministic identity**: `execution_id` binds source + recipe + policy +
  runner class, so a published capsule's runtime identity is reproducible and
  auditable (see [EXECUTION_IDENTITY_SPEC.md](EXECUTION_IDENTITY_SPEC.md)).
- **No secret capture**: QA boots reuse the ReadyStateManifest no-secret
  invariant — the sealed artifact carries no secret and is reusable across hosts
  of the same `runner_class_id`.
- **Resource caps**: materialization is bounded (100 MiB compressed / 250 MiB
  expanded / 50,000 files / 50 MiB single file) so a hostile repo cannot exhaust
  the builder.

## 8. Known limitations / unresolved questions

Carried forward as explicit open questions (see also §12 of
[HASH_AND_PROVENANCE_POLICY.md](HASH_AND_PROVENANCE_POLICY.md)):

1. **R2 conditional-put verification.** Preferred: the R2 binding's
   `onlyIf: { etagDoesNotMatch: "*" }` (If-None-Match) to make archive upload
   idempotent by content hash. If the binding does not honor it reliably, fall
   back to a CAS-key write plus a GC tombstone/grace window for orphaned keys.
2. **Queue escalation criterion.** When does cron + `waitUntil` stop being
   enough and justify a dedicated Cloudflare Queue? Needs a measured threshold
   (backlog depth / median time-in-state), not a guess.
3. **RunnerClassId derivation-change compatibility.** How a change to
   `rootfs_base_id` / `guest_kernel_id` affects `execution_id` for *existing*
   published snapshots — i.e. when a runner-class change should force
   re-verification vs. remain compatible.

## 9. References

- [A1_SOURCE_TREE_PROFILE.md](A1_SOURCE_TREE_PROFILE.md) — source-tree hash profile.
- [SOURCE_MATERIALIZATION_SPEC.md](SOURCE_MATERIALIZATION_SPEC.md) — materialize job contract.
- [EXECUTION_IDENTITY_SPEC.md](EXECUTION_IDENTITY_SPEC.md) — `execution_id` facets.
- [ADR-011-source-materialization-placement.md](ADR-011-source-materialization-placement.md) — placement decision.
- [ADR-012-capsule-lifecycle-column.md](ADR-012-capsule-lifecycle-column.md) — lifecycle column.
- [ADR-013-manifest-validator-wasm-split.md](ADR-013-manifest-validator-wasm-split.md) — validator WASM split.
- [HASH_AND_PROVENANCE_POLICY.md](HASH_AND_PROVENANCE_POLICY.md) — hash domains and Git provenance.
- [../accepted/A1_BLOB_HASH.md](../accepted/A1_BLOB_HASH.md) — the frozen A1 base algorithm.
- `crates/snapshot/src/rootfs_builder.rs:1474` — `materialize_source` (extended by the builder job).
- `crates/snapshot/src/manifest.rs:19-63` — `ReadyStateManifest` (`ato.ready-state/v1`).
