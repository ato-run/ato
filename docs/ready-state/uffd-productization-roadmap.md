# UFFD productization roadmap — U7–U16 (umbrella #873)

> Status: **roadmap / acceptance — docs-only, no code, no behavior change.** Fixes
> the U7–U16 execution plan and per-phase acceptance for taking the U0–U6 UFFD spike
> (#852, merged + hardware-validated) toward a CLI-previewable, benchmarkable,
> placement-selectable feature — **without** touching the default `ato run` path.
> This refines the phase model in [`uffd-productization-plan.md`](./uffd-productization-plan.md)
> (P0–P5) into ten reviewable slices. **`ato run` default behavior does not change in
> any phase below.**

## Discipline (whole track — non-negotiable)

- **`ato run` default stays the File path.** UFFD only via explicit preview flags.
- **No-binding capsules only.** Any capsule requiring `secrets` / `bindings` /
  `external` capabilities stays **fail-closed** until Phase 8 `BindingLease` (#863).
- **No post-bind snapshot / checkpoint / re-seal.**
- **No auto-selection / no product default** until U15, and even then opt-in only.
- **Phase 8 BindingLease returns after U16.**

## Why polish before Phase 8

Phase 8 (binding/security) blows the design space open. UFFD is now far along, so the
higher-value move is to get it **fast, safe, and measurable for no-binding capsules** —
CLI-try-able, benchmarked, and explainable ("under which conditions does it beat
File?") — before mixing in the binding mainline.

## Sequence

```
U0–U6 spike (done)
  → U7–U10  observe + benchmark        (test isolation, receipt schema, bench, diagnostics)
  → U11–U15 CLI preview + selection    (opt-in preview, profile persistence, hardening, dry-run, opt-in auto-select)
  → U16     readiness report / gate
  → Phase 8 BindingLease
```

Implementation order is strictly **U7 → U8 → U9** first (nothing user-facing depends
on an unstable receipt or a flaky suite), then the CLI-preview slices, then U16.

## Phases

| # | issue | phase (plan) | one-line | acceptance |
|---|---|---|---|---|
| U7 | #874 | audit | spike audit / test isolation | default File path confirmed unchanged (all `ATO_FC_UFFD*` unset); flaky suite split into single-test invocations; gate-eligible vs not-eligible UFFD tests listed |
| U8 | #875 | schema | stable `UffdRestoreReceipt` | versioned, frozen receipt; File + UFFD emit comparable fields (`time_to_health`, `page_faults`, `prefetch_pages`, `bytes_copied`, `remote_chunks`) |
| U9 | #876 | bench | benchmark harness/command | one command runs File / UFFD-demand / UFFD-hotset / UFFD-remote on one no-binding capsule → JSON + markdown comparison |
| U10 | #877 | **P0** | CLI diagnostics only | receipt prints `mem_backend_would_select` + reason; **zero behavior change** (still restores via File) |
| U11 | #878 | P2 | local UFFD preview command | `ato run --experimental-uffd`: no-binding + local-CAS only; unsupported host fail-closed; default unchanged |
| U12 | #879 | P3 | hotset profile persistence | profiles keyed by `capsule_manifest_hash` + `runner_class_id` + **memory_image_hash**; a mismatched profile is never applied (test proves no guest-memory corruption) |
| U13 | #880 | — | failure hardening (product path) | page-server crash → teardown; CAS **miss** and CAS **corrupt** → fail-closed **by different mechanisms** (see below); timeout → teardown; no orphan VM/tap/overlay/pid in the product path |
| U14 | #881 | P1 | placement contract dry-run | UFFD selectability computed from `BackendCapabilities` + `RunnerClass` and recorded; **no auto-selection**; no behavior change |
| U15 | #882 | P2/P5 | opt-in auto-selection preview | only behind an explicit preview flag does the selector choose File/UFFD; no-binding only; prefer local CAS + hotset; **remote off by default** |
| U16 | #883 | — | product readiness report | documented "when to enable UFFD"; a dev/main release gate; the go/no-go before Phase 8 |

### U13's two halves are NOT one mechanism (post-#1127)

"CAS miss/corrupt → fail-closed" reads as one property and is two, and they stopped
failing the same way when #1127 gave `uffd_preview_mode` a real residency gate.
Anything claiming to prove U13 must say which half it covers:

| half | what happens on the preview lane | where it fails | test |
|---|---|---|---|
| **miss** (a chunk is absent) | `uffd_preview_mode` sweeps `CasStore::has_all_chunks`, REFUSES UFFD and falls back to File | pre-boot, in `rehydrate_atomic` → `CapsuleFsError::MissingChunk`. No page server, no userfaultfd, no VMM started | `fc_kvm_uffd_preview_missing_cas_chunk_fails_closed_pre_boot` (KVM) + `uffd_preview_requires_a_resident_memory_image_not_just_an_openable_cas` (CI, gate decision only) |
| **corrupt** (a chunk is present with wrong bytes) | passes residency (`has_chunk` is a `stat`), so the preview really demand-pages | post-boot, in the page server's hash-verified `read_range` → fail-closed abort, or the `LoadSnapshot` the first bad fault stalls (~15 s) | `fc_kvm_uffd_preview_corrupt_cas_fails_closed`, `fc_kvm_uffd_corrupt_cas_chunk_fails_closed` (both KVM) |

The miss half is therefore now enforced **pre-boot** — strictly better than the
post-boot page-fault abort it used to mean, since the session is never handed out —
and a corrupt-contents fixture is the ONLY one that still reaches the serve path. A
missing-chunk fixture inside a corrupt-CAS test would produce a green test with no
UFFD in it at all.

## Hard blockers before any product default (carried from the plan)

- No flaky `fc_kvm_uffd` suite as a release gate (U7 fixes this).
- No product selection without the frozen receipt schema (U8) or bounded failure
  handling (U13).
- No binding-required capsules until Phase 8 `BindingLease`; no post-bind re-seal.
- No Desktop / CRIU / UFFD mixing.

## After U16

Return to **Phase 8 BindingLease** (#863 contract → implementation). Until then, the
UFFD product preview is **no-binding-only**, and this constraint is encoded in every
U11–U15 selection path:

```
Until Phase 8 BindingLease is implemented:
  UFFD product preview supports no-binding capsules only.
  Any capsule requiring secrets/bindings/external capabilities remains fail-closed.
  No post-bind snapshot/checkpoint/re-seal.
```

See [`uffd-productization-plan.md`](./uffd-productization-plan.md) (boundary + P0–P5),
[`uffd-mem-backend.md`](./uffd-mem-backend.md) (spike + receipts), and
[`binding-lease.md`](./binding-lease.md) (the Phase 8 mainline this track defers to).
