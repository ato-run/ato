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
  - "../archived/CAPSULE_V1_EXECUTION_MODEL_SPEC.md"
  - "ADR-011-source-materialization-placement.md"
  - "ADR-012-capsule-lifecycle-column.md"
  - "ADR-013-manifest-validator-wasm-split.md"
  - "HASH_AND_PROVENANCE_POLICY.md"
  - "../accepted/A1_BLOB_HASH.md"
---

# GitHub Capsule Request Pipeline

> **Execution Identity migration:** The post-seal/Snapshot-derived identity
> sections in this draft predate #1086 and are superseded by
> [archived Capsule v1 Execution Identity and Snapshot Model](../archived/CAPSULE_V1_EXECUTION_MODEL_SPEC.md).
> Until this pipeline is fully reconciled, the Capsule v1 specification governs
> identity and Snapshot boundaries.

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
| 3. Execution identity | [CAPSULE_V1_EXECUTION_MODEL_SPEC.md](../archived/CAPSULE_V1_EXECUTION_MODEL_SPEC.md) | archived SPEC |
| 4. Capsule + revision lifecycle | [ADR-012](ADR-012-capsule-lifecycle-column.md) | ADR |
| 5. Manifest validator WASM split | [ADR-013](ADR-013-manifest-validator-wasm-split.md) | ADR |
| 6. Request model + state machine | this document (§4–§7) | SPEC |

## 1. Overview

A user pastes a public GitHub URL. The platform resolves it to an immutable
commit, materializes the source deterministically, derives a candidate
`capsule.toml`, validates it, runs a headless QA boot, and — if everything
passes and an admin approves — publishes a Community capsule that anyone can Run.
Every step is content-addressed and receipted so the result is auditable and
reproducible. An update to an already-published capsule produces a **new
revision** and never disturbs the currently-published one until the new revision
is itself published (see [ADR-012](ADR-012-capsule-lifecycle-column.md)).

The pipeline spans three execution surfaces that already exist in the platform:

- **Cloudflare Worker (ato-api edge)** — request intake, ref resolution, commit
  pinning, GitHub Trees enumeration, the candidate state machine, and R2-mediated
  storage. No repository *content* is materialized here (see [ADR-011](ADR-011-source-materialization-placement.md)).
- **Snapshot builder (claim/ack lease lane)** — the existing snapshot-builder
  worker gains two new job kinds, `source_materialize` and `candidate_qa`, that
  do the CPU/IO-heavy work (checkout, canonicalize, hash, archive, QA boot).
- **Capsule runtime (`crates/capsule`, `crates/snapshot`)** — the manifest
  validator core (extracted to WASM), resolved execution contract, and Capsule
  v1 Snapshot manifest.

## 2. Scope

### In scope

- End-to-end flow from a public GitHub URL to a published Community capsule.
- The **two-tier request model** (`capsule_requests` per requester,
  `source_candidates` per deduped repo+commit) and the fine-grained
  `pipeline_state` machine the candidate owns (§4).
- The **leased, fenced** driving of that state machine (§4.6).
- Deterministic identity of the materialized source, the recipe, and the sealed
  execution (delegated to companion specs).
- MVP policy gates: license allowlist, admin approval, per-user quotas, kill
  switches, and the rejection rules that map a repository to a `blocked_*`
  terminal.
- The two candidate-generation lanes (static web vs docker-import reuse), the
  constrained-generation contract, and the Lane B untrusted-build boundary (§7).

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
  [CAPSULE_V1_EXECUTION_MODEL_SPEC.md](../archived/CAPSULE_V1_EXECUTION_MODEL_SPEC.md) §4).

## 3. Actors and data flow

```text
 user ──URL──▶ ato-api (Worker)
                 │  create/link capsule_requests row (one per requester)
                 │  resolve ref → commit OID (immutable)
                 │  find-or-create source_candidate (deduped repo+commit)
                 │  GitHub Trees enumeration (recursive-first + non-recursive fallback)
                 ▼
   candidate.pipeline_state: requested → fetching
                 │
                 ▼  claim/ack lease lane
        builder job: source_materialize
                 │  pinned checkout → canonicalize → A1v2 hash → tar.zst
                 │  cross-check against API Trees listing (paths + OIDs)
                 │  API-mediated upload → R2 (source-archives/v1/...)
                 ▼
   candidate.pipeline_state: analyzing → generating
                 │  deterministic RepoAnalysisReport (ceiling)
                 │  constrained AI → candidate IDs only
                 │  deterministic renderer → schema-0.3 TOML
                 ▼
   candidate.pipeline_state: validating
                 │  capsule-manifest-core WASM validator (final gate)
                 ▼
   candidate.pipeline_state: qa_queued → qa_running
                 │  builder job: candidate_qa (headless boot on runner_class)
                 │  SnapshotManifestV1 + teardown receipt
                 ▼
   candidate.pipeline_state: publish_ready → awaiting_admin_approval → published
                 │  capsule_revision: verifying → publish_ready
                 │      → awaiting_admin_approval → published (or rejected)
                 │  capsule.lifecycle = active; current_revision_id switches ATOMICALLY
                 ▼
        Community capsule, Runnable by anyone
        (resolve/run gate = capsule active AND current revision published)
```

The three hashes that thread the whole pipeline together:

| Name | Value | Owner spec |
|------|-------|-----------|
| `materialized_source_tree_hash` | A1v2 digest (`sha256`) of the canonical checkout | [A1_SOURCE_TREE_PROFILE.md](A1_SOURCE_TREE_PROFILE.md) |
| `source_archive_hash` | `sha256` of the exact `tar.zst` bytes | [SOURCE_MATERIALIZATION_SPEC.md](SOURCE_MATERIALIZATION_SPEC.md) |
| `execution_id` | `blake3` over the resolved target launch contract; Snapshot layer IDs are excluded | [CAPSULE_V1_EXECUTION_MODEL_SPEC.md](../archived/CAPSULE_V1_EXECUTION_MODEL_SPEC.md) |

Hash-role split (consistent with [HASH_AND_PROVENANCE_POLICY.md](HASH_AND_PROVENANCE_POLICY.md)):
**`sha256` is identity for the A1 source-tree family; domain-separated `blake3`
identifies the resolved execution contract and Snapshot manifest, and remains
the CapsuleFS CAS transport hash.** The profiles never substitute for each
other.

## 4. Request model and state machine (topic 6)

### 4.1 No new API surface

There is **no** new `/v1/capsule-requests` endpoint. A GitHub capsule request is
an extension of the existing `capsule_submissions` machinery and the existing
`POST /v1/store/apply` entry point. The coarse operator-facing status machine
(the human review states already in the store) is unchanged; the fine-grained
automation state lives on a new work-unit record (§4.3).

### 4.2 Two-tier request model

The submission is modeled in two tiers so that "who asked" is decoupled from
"the work":

- **`capsule_requests`** — **one row per requester**. Owns status display,
  notifications, per-user quota accounting, withdrawal, and the audit trail. Two
  different users asking for the same repo+commit get two request rows.
- **`source_candidates`** (a.k.a. `pipeline_runs`) — the **deduped repo+commit
  work unit**. Owns `pipeline_state` and all the pipeline machinery. Many
  `capsule_requests` may point at one `source_candidate`.

A duplicate submission (same repo+commit) **creates a new `capsule_requests` row
linked to the existing `source_candidate`** — the work is done once; each
requester still has their own status/notification/quota row.

### 4.3 `pipeline_state` machine (on `source_candidates`)

`pipeline_state` is a column on `source_candidates`, not on `capsule_requests`
and not on capsules. Linear happy path:

```text
requested → fetching → analyzing → generating → validating
          → qa_queued → qa_running → publish_ready
          → awaiting_admin_approval → published
```

`awaiting_admin_approval` is a required gate before `published` (see §6.1);
allowlist auto-advance through it is a separate, later flag.

Branch / terminal states:

| State | Kind | Meaning |
|-------|------|---------|
| `needs_launch_info` | branch (recoverable) | Generation cannot pick an entrypoint/port without user input. |
| `policy_review` | branch | License not on the allowlist, or a policy signal needs a human. |
| `blocked_repo` | terminal (blocked) | Repo shape unsupported (e.g. Trees enumeration budget exceeded, >50,000 files, submodules). |
| `blocked_policy` | terminal (blocked) | Disallowed content/license after review. |
| `blocked_incompatible` | terminal (blocked) | Manifest shape the MVP cannot run (e.g. `[packages]` workspace delegation). |
| `failed_internal` | retryable | Transient platform failure. Max 3 attempts, exponential backoff. |
| `expired` | terminal | A `needs_launch_info` candidate left stale for 30 days. |
| `canceled` | terminal | All linked requests withdrew. |

Rules:

- `failed_internal` retries automatically up to **3** attempts with exponential
  backoff; the fourth failure is surfaced, not retried.
- Admin retry increments `attempt_no` **on the same candidate row** (no new
  candidate).
- A `blocked_*` state is terminal for that `(repo, commit)` candidate but a
  **new commit** is a new candidate (see §4.5).

### 4.4 Candidate record shape (delta)

New columns split across the two tiers (ato-api migration; recorded here for
cross-repo visibility):

`capsule_requests` (per requester):

- `requester_id`, `source_candidate_id` (FK), `status`, `created_at`,
  `withdrawn_at`.

`source_candidates` (per repo+commit work unit):

- `source_provider` (`github`), `provider_repository_id`, `provider_owner`,
  `provider_repo`.
- `commit_algorithm` (`sha1` today; GitHub's object format), `commit_oid`.
- `pipeline_state`, `attempt_no`.
- Leasing/fencing columns: `pipeline_version`, `lease_owner`,
  `lease_expires_at`, `next_attempt_at`, `started_at`, `updated_at`,
  `last_error_code`, `last_error_detail` (see §4.6).
- `materialized_source_tree_hash`, `source_archive_hash`, `recipe_hash`,
  `execution_id` (filled as the pipeline advances).
- `target_capsule_id` (nullable; set when the candidate maps to an existing
  registered capsule — see §4.5) and, on success, `capsule_revision_id`.

### 4.5 Dedup and identity

The uniqueness index lives on the **candidate tier**:

```text
UNIQUE (source_provider, provider_repository_id, commit_algorithm, commit_oid)
```

- Two requests for the **same repo at the same commit** find-or-create **one**
  `source_candidate` and link two `capsule_requests` rows to it.
- A **new commit** for a repo that already maps to a registered capsule is a
  **new candidate** with `target_capsule_id` set to the existing capsule; on
  success it becomes a **new revision** of that capsule (never a duplicate
  listing, never disturbing the current published revision — see
  [ADR-012](ADR-012-capsule-lifecycle-column.md)).
- `provider_repository_id` (GitHub's numeric repo id), not `owner/repo`, is the
  stable key — it survives repo renames and owner transfers.

### 4.6 Driving the pipeline (leased and fenced)

`db.batch()` alone is **not** a compare-and-swap and cannot fence a lost racer.
Every transition is an optimistic, fenced UPDATE against the candidate row:

```sql
UPDATE source_candidates
   SET pipeline_state = :next,
       pipeline_version = pipeline_version + 1,
       lease_owner = :worker, lease_expires_at = :now_plus_ttl,
       updated_at = :now
 WHERE id = :id
   AND pipeline_state = :expected
   AND pipeline_version = :expected_version;
```

- **Affected-rows check.** 0 rows updated ⇒ the driver lost the race (another
  driver already advanced the row); it re-reads and yields. Only the winner
  proceeds.
- **Lease.** `lease_owner` / `lease_expires_at` mark a live job. A long-running
  builder job holds the lease and renews it.
- **Cron re-drives only expired leases.** The existing 15-minute cron sweep may
  re-drive a candidate **only if its lease has expired** (`lease_expires_at <
  now`) — so it can never double-start a job that is still live. `next_attempt_at`
  schedules `failed_internal` backoff.
- **`waitUntil`** kicks the next step in the background after a winning
  transition; the cron is the at-least-once safety net, not the primary driver.

**No new Queue is introduced.** A Cloudflare Queue is only added if measurement
shows the cron + lease + `waitUntil` combination cannot keep up (see §8).

### 4.7 Builder QA lane

The builder consumes two job kinds on the **existing** claim/ack lease lane
(the same lane snapshot builds already use):

- `source_materialize` — see [SOURCE_MATERIALIZATION_SPEC.md](SOURCE_MATERIALIZATION_SPEC.md).
- `candidate_qa` — headless boot of the candidate capsule on a target
  `runner_class`, producing a `SnapshotManifestV1` subordinate to the resolved
  `execution_id` and a **teardown receipt** that publication requires.

QA job uniqueness key (prevents duplicate QA work for an identical candidate):

```text
candidate_id + attempt_no + source_tree_hash + recipe_hash
             + runner_class_id + qa_contract_hash
```

## 5. Candidate generation (topic 6, generation half)

### 5.1 MVP lanes

Two lanes only:

- **Lane A — static web**: the repo already contains committed build output
  (e.g. a `dist/` or `public/` a static server can serve). **Zero repo command
  execution.**
- **Lane B — docker-import reuse**: reuse the docker-import v1.7 path for repos
  that ship a usable container definition. Repo-provided build commands run only
  under the untrusted-build boundary in §7.

**Step-0 eligibility measurement (2026-07-13) — Lane B is the first lane.**
A systematic 1-in-10 sample (n=102) of the real candidate corpus (the 1,016
distinct repos in production `capsule_submissions`; the
`store_publish_requests` lead table is empty in both envs) measured:

| Metric | n=102 | extrapolated |
|---|--:|--:|
| Lane A eligible (servable as committed, zero commands) | 2.9% | ~30 |
| Lane A ∩ license allowlist | **2.0%** | **~20** |
| Lane B eligible (Dockerfile/compose) | 88% | ~895 |
| Lane B ∩ license allowlist | **44%** | **~448** |
| needs-build (static after `npm build`; excluded from Lane A) | 46% | ~470 |

Only one sampled repo is a genuine committed-output static app; per the
approved plan rule ("if Lane A population is insufficient, promote Lane B"),
**the MVP starts with Lane B**. Lane A stays fully specified and gated behind
its own kill switch for the organic-submission funnel, on which there is no
data yet (the lead table has captured zero organic leads). Rejects observed in
the sample: submodules 8.8%, LFS 2.0%, truncated trees / >50k entries 0%.

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

### 6.1 License allowlist and admin approval

MVP SPDX allowlist:

```text
MIT, Apache-2.0, BSD-2-Clause, BSD-3-Clause, ISC, 0BSD
```

Anything else routes to `policy_review` (human), not auto-block. `LICENSE` and
`NOTICE` files are **displayed on the capsule page** and **bundled into the
source archive**.

**Admin approval is required for the MVP.** Every candidate stops at
`awaiting_admin_approval` before `published`; a human approves the first cohort.
**Allowlist auto-publish** (skipping the manual approval for allowlisted
licenses) is a **separate flag**, enabled **only after a 20–50-capsule
evaluation period** has validated the pipeline's output quality.

### 6.2 Provenance and labeling

- Every GitHub-request capsule is labeled **unofficial Community** unless the
  GitHub App owner-claim proves the requester controls the repo.
- A verified GitHub App owner claim upgrades the labeling; it does not change the
  pipeline.

### 6.3 Takedown

A takedown sets the **capsule** `lifecycle` to `archived` (the existence gate;
see [ADR-012](ADR-012-capsule-lifecycle-column.md)), which revokes all run
bindings in the takedown D1 batch; the app proxy gates per-run, so archived
capsules stop being runnable at the next run attempt. Archiving a capsule is
orthogonal to any revision's lifecycle — it removes the capsule from existence
regardless of which revision is current.

### 6.4 Quotas and kill switches

- Per-user quotas are enforced on the **`capsule_requests`** tier (per
  requester): **3 in-flight** requests, a daily cap, and a per-repo cooldown.
  Because dedup collapses work at the candidate tier, quota counts requester
  intent, not duplicated pipeline runs.
- **Kill switches at every stage** — intake, materialize, generate, QA,
  publish — each independently disableable without redeploying, so a bad lane
  can be shut off in isolation. Lane A and Lane B each have their own switch.

## 7. Security

- **Prompt injection**: repository content is untrusted data. The
  constrained-generation contract (§5.2) means model output cannot become
  manifest content directly; the WASM validator is the schema gate; repo text
  never enters the instruction channel.
- **Public repos only**: materialization uses a GitHub App installation token
  scoped to public reads; no private content is ever fetched.
- **Resolved launch identity**: `execution_id` is computed from the finalized
  `ato.execution-contract/v1` after runtime, dependency, and application output
  digests are observed. Snapshot IDs and sealed memory/VM-state layers are
  excluded; the former post-seal proposal is retained only in the
  [archived Snapshot-derived spec](../archived/EXECUTION_IDENTITY_SPEC.md).
- **No secret capture**: QA boots use the Capsule v1 structural capture policy:
  production secrets, user state, and user identity are never attached to the
  build guest. Sanitization and secret-scan attestations are supporting
  evidence, not proof. Compatibility, rather than a runner name alone,
  determines where the Snapshot may be restored.
- **Materialization resource caps**: bounded (100 MiB compressed / 250 MiB
  expanded / 50,000 files / 50 MiB single file) so a hostile repo cannot exhaust
  the builder.

### 7.1 Lane B untrusted-build boundary (NORMATIVE)

A repo-provided Dockerfile is untrusted code. It may execute **only** inside a
build environment that satisfies **all** of the following:

- **No host Docker socket** is mounted or reachable.
- **No cloud credentials or builder secrets** are present in the environment,
  filesystem, or metadata endpoint.
- **No privileged mode** and **no device mounts**.
- **Rootless or full-VM isolation** for the build.
- **Hard resource caps**: CPU, RAM, PID count, disk, and wall-time.
- **Explicit egress policy** (no unrestricted outbound network).
- **Forced post-build cleanup** of the build environment.

A build environment that violates **any** of these MUST NOT produce a
publishable candidate — the candidate fails closed (`failed_internal` or
`blocked_incompatible`, never `publish_ready`).

## 8. Known limitations / unresolved questions

Carried forward as explicit open questions (see also §12 of
[HASH_AND_PROVENANCE_POLICY.md](HASH_AND_PROVENANCE_POLICY.md)):

1. **R2 conditional-put verification.** Preferred: the R2 binding's
   `onlyIf: { etagDoesNotMatch: "*" }` (If-None-Match) to make archive upload
   idempotent by content hash. If the binding does not honor it reliably, fall
   back to a CAS-key write plus a GC tombstone/grace window for orphaned keys.
2. **Queue escalation criterion.** When do cron + lease + `waitUntil` stop being
   enough and justify a dedicated Cloudflare Queue? Needs a measured threshold
   (backlog depth / median time-in-state), not a guess.
3. **RunnerClassId derivation-change compatibility.** Narrowed by the 2026-07-13
   scope investigation: the production builder + `runner serve` path already
   resolves real `snapshot_format` / `vmm_version` / `guest_kernel_id` via
   `FirecrackerBackend::runner_facts()`, and the CLI-path asymmetry
   (`from_host()` sentinels) is fixed by delegating to the backend (PR #1058).
   What remains open, as two *separate deliberate decisions* rather than one
   backfill:
   - **`cpu_template`**: enabling it (T2CL/T2A) changes `runner_facts()` output
     → a snapshot flag-day (codec-playbook rebuild in both envs, builders
     colocated per env). Only worth scheduling for cross-silicon warm-pool
     portability.
   - **`rootfs_base_id`**: stays `"unset"` by design under the per-capsule
     full-rootfs layer model; resolving it first requires a base-image concept.

## 9. References

- [A1_SOURCE_TREE_PROFILE.md](A1_SOURCE_TREE_PROFILE.md) — source-tree hash profile.
- [SOURCE_MATERIALIZATION_SPEC.md](SOURCE_MATERIALIZATION_SPEC.md) — materialize job contract.
- [CAPSULE_V1_EXECUTION_MODEL_SPEC.md](../archived/CAPSULE_V1_EXECUTION_MODEL_SPEC.md) —
  Capsule v1 `execution_id` and Snapshot boundary.
- [ADR-011-source-materialization-placement.md](ADR-011-source-materialization-placement.md) — placement decision.
- [ADR-012-capsule-lifecycle-column.md](ADR-012-capsule-lifecycle-column.md) — capsule + revision lifecycle.
- [ADR-013-manifest-validator-wasm-split.md](ADR-013-manifest-validator-wasm-split.md) — validator WASM split.
- [HASH_AND_PROVENANCE_POLICY.md](HASH_AND_PROVENANCE_POLICY.md) — hash domains and Git provenance.
- [../accepted/A1_BLOB_HASH.md](../accepted/A1_BLOB_HASH.md) — the frozen A1 base algorithm.
- `crates/snapshot/src/rootfs_builder.rs:1474` — `materialize_source` (extended by the builder job).
- `crates/snapshot/src/snapshot_manifest.rs` — `SnapshotManifestV1`
  (`ato.snapshot-manifest/v1`); `manifest.rs` retains legacy
  `ReadyStateManifest` decoding for explicit migration.
