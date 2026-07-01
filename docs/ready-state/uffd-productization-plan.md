# UFFD productization plan (post-merge audit)

> Status: **plan / boundary document — no code, no behavior change.** The UFFD
> spike (U0–U6, #852) is merged to `nightly` and hardware-validated (receipts in
> [`uffd-mem-backend.md`](./uffd-mem-backend.md), merged as #871). This document
> fixes the **spike ↔ product boundary** *before* any product path selects UFFD, so
> the spike can flow `nightly → dev → main` without its test-only knobs leaking into
> the default restore path. It does **not** remove `ATO_FC_UFFD`, wire placement
> selection, or change `ato run`.

## 1. Why this exists

The spike landed a full lazy-memory-restore stack (page-server, local + remote CAS
demand paging, hotset prefetch, fail-closed hardening). That is a large surface. The
risk is that "it works on the KVM host" becomes "turn it on in product". This doc
draws the line: what stays test-only, what is a product candidate, and what must
**never** reach the default path.

## 2. Current spike result (merged)

| phase | PR | what it proved |
|---|---|---|
| U0 | #862 | truthful `BackendCapabilities.supports_uffd_mem_backend` capability probe |
| U1 | #865 | Firecracker UFFD page-server handshake (`SCM_RIGHTS` fd + event loop + `UFFDIO_COPY`/`ZEROPAGE`); reaches `/health` from an `.mem` mmap |
| U2 | #866 | lazy demand paging from **local CAS** (no `.mem` materialization); `/health` faulting 2.3 % of a 512 MB image |
| U3 | #867 | per-restore `HotsetTrace` (pre-health hotset) |
| U4 | #868 | **hotset prefetch**: demand faults 2985 → 3, time-to-health 366 → 166 ms |
| U5 | #869 | fail-closed on CAS miss/corrupt; no orphan VM/tap |
| U6 | #870 | remote CAS read-through; reaches `/health` from remote, 50/256 chunks fetched on demand |
| — | #871 | results + when-UFFD-beats-File conditions written down (#852 acceptance) |

All are `#[ignore]`d KVM smokes behind the test-only `ATO_FC_UFFD` gate. Receipts:
see #871. **Caveat:** the combined `fc_kvm_uffd` suite flakes under `--test-threads=1`
from host pressure (6 back-to-back 512 MB-mmap boots); each test passes individually.

## 3. The productization boundary

### Current (must hold until each phase below explicitly changes it)
- UFFD modes are **test/spike-only**, gated behind `ATO_FC_UFFD` /
  `ATO_FC_UFFD_HOTSET` / `ATO_FC_UFFD_REMOTE`.
- The **default File backend restore path is unchanged** — with all UFFD env unset,
  `restore()` is byte-for-byte the pre-spike File path.
- The **`ato run` product path never selects UFFD**. Nothing in the CLI reads the
  `ATO_FC_UFFD*` vars; only the engine-level KVM smokes set them.

### Product future (the target — not yet built)
- UFFD selection must be **placement-contract-driven**, not ad-hoc env vars.
- Selection must depend on **`BackendCapabilities.supports_uffd_mem_backend`** (U0) —
  never assume UFFD support.
- Selection must use **RunnerClass / backend compatibility** (the same identity that
  gates a Ready-State restore today), not per-invocation env.
- Product selection requires **clean fallback / fail-closed rules**: an unsupported
  host, a missing hotset profile, or a corrupt CAS chunk must each have a defined,
  safe outcome (fall back to File where allowed, or fail closed in validation mode).

## 4. When UFFD beats File (selection input, from #871)

The File backend eagerly rehydrates the whole memory image (~0.69 s for 512 MB) but
**amortizes** it (warm restores skip the rehydrate, ~150–220 ms). UFFD faults in only
the working set (~2.3 %) every restore.

- **UFFD wins:** cold cache · large memory image · remote/streamed memory image ·
  hotset-predictable startup (U4 prefetch → 166 ms).
- **File wins or ties:** small image + warm disk cache (already Firecracker-bound).

→ A product path **must not select UFFD blindly for all capsules.** The selector
needs the capsule's memory size, cache warmth, hotset availability, and host UFFD
capability as inputs.

## 5. Productization phases

- **P0 — selection diagnostics only (no behavior change).** Compute + record which
  `mem_backend` the current capsule/runner/host *would* select and *why*, and **still
  restore via File**. This is the safe first step: it exercises the selection logic
  against real inputs without changing any behavior. Example receipt:
  ```
  mem_backend_would_select = File | Uffd
  reason:
    host_supports_uffd = true|false
    capsule_has_hotset_profile = true|false
    local_cas_has_memory_chunks = true|false
    bindings_required = true|false
    cold_cache_expected = true|false
  ```
- **P1 — placement contract extension.** Fold UFFD readiness into the placement
  contract (#816): `RunnerClass` / `BackendCapabilities` carry UFFD readiness;
  selection becomes a pure function of contract + capsule facts. **Still no default
  product selection** — P1 only makes the decision expressible.
- **P2 — opt-in product preview.** `ATO_READY_STATE_UFFD_PREVIEW=1` (or equivalent)
  enables UFFD in the *product* `ato run` path, **only** for: no-binding capsules,
  **local CAS only**, and **fail-closed on an unsupported host**. Default off.
- **P3 — hotset profile persistence policy.** Persist / invalidate profiles keyed by
  `capsule_manifest_hash` + `runner_class_id` + **memory image hash** (a profile from
  a different image/runner must never be applied — see U4's file-offset keying).
- **P4 — remote read-through policy.** Only after the local product preview (P2) is
  stable: define when remote read-through is allowed (network trust, cache locality,
  failure/timeout budgets).
- **P5 — retire the spike knobs.** Remove `ATO_FC_UFFD*` env names or convert them
  into internal test-only knobs once product selection is the real path.

## 6. Hard blockers before *any* product UFFD

- No **flaky `fc_kvm_uffd` suite** as a release gate (fix isolation / host-pressure
  first, or split into single-test invocations).
- No product selection without a **stable receipt schema** (the `UffdRestoreReceipt`
  shape must be frozen before anything depends on it).
- No product selection without **bounded page-server failure handling** (U5's
  fail-closed must cover the product path, not just the smoke).
- **No binding-required capsules** until Phase 8 `BindingLease` exists (#863 / the
  `binding-lease.md` contract) — a UFFD-restored session is still no-binding-only.
- **No post-bind snapshot / checkpoint / re-seal** (the Phase 8 hard invariant).
- **No Desktop / CRIU / UFFD mixing** yet — these are parallel tracks, not composed.

## 7. Required tests before any implementation PR

- Default `ato run` remains the **File path** unless preview is explicitly enabled.
- An **unsupported host** reports a reason and cold-paths **only when cold-path is
  allowed** (never in validation mode with a required sealed artifact).
- **Validation mode never silently cold-paths** when a sealed artifact is required
  (the #861 invariant, preserved).
- **Page-server crash → VM teardown** (U5 in the product path).
- **CAS miss / corrupt → fail closed** (U5 in the product path).
- **Hotset profile mismatch** (wrong capsule/runner/image) is **ignored or fails
  closed** — it must never corrupt guest memory (a stale profile applied to a
  different image would `UFFDIO_COPY` wrong bytes; the P3 keying prevents this and a
  test must enforce it).

## 8. Sequencing

```
1. UFFD productization plan doc            ← this PR (docs-only)
2. P0 selection diagnostics only           ← no behavior change; receipt-only
3. Phase 8 BindingLease implementation plan
4. Re-provision a KVM host; P0/P1 KVM smoke
5. only then: product preview (P2)
```

Product UFFD does **not** begin by replacing `ATO_FC_UFFD` with a live selector. It
begins by **printing what a selector would decide** (P0), leaving `nightly` behavior
untouched, and connecting naturally to the #816 placement contract.

See also [`uffd-mem-backend.md`](./uffd-mem-backend.md) (spike + receipts),
[`binding-lease.md`](./binding-lease.md) (Phase 8 mainline), and
[`desktop-runner.md`](./desktop-runner.md) / [`criu-container-spike.md`](./criu-container-spike.md)
(parallel tracks that must not be composed with UFFD yet).
