# UFFD product readiness report (U16 — #883)

> The go/no-go for taking the UFFD lazy-memory work from `nightly` toward `dev`/`main`.
> Synthesizes the U0–U6 spike (#852) and the U7–U15 productization track (#873), all
> hardware-validated on GCP `n2-standard-4` / Firecracker v1.16.0 / guest kernel
> `vmlinux-5.10.223`. **Bottom line: the opt-in *preview* is ready; UFFD as a *default*
> is not — and must not be until the placement contract (P1) and Phase 8 land.**

## What shipped

| phase | PR | what |
|---|---|---|
| U0 | #862 | truthful `supports_uffd_mem_backend` capability probe |
| U1–U6 | #865–#870 | page-server handshake · local-CAS demand · fault trace · hotset prefetch · fail-closed · remote read-through (spike) |
| U7 | #885 | spike audit / test isolation (single-test KVM runner; env gate is test-only) |
| U8 | #886 | frozen, versioned, File-comparable `UffdRestoreReceipt` schema |
| U9 | #888 | real-app File-vs-UFFD **benchmark** (harness + results) |
| U10 | #889 | CLI **diagnostics** (`mem_backend_would_select`, no behavior change) |
| U11 | #890 | local UFFD **preview** (`ATO_READY_STATE_UFFD_PREVIEW`, input-driven, fail-closed) |
| U12 | #891 | hotset profile **persistence** (keyed store, never wrong-image) |
| U13 | #892 | **failure hardening** on the product preview path (fail-closed, no orphans) |
| U14 | #887 | pure `decide_mem_backend` **selector** (placement dry-run) |
| U15 | #893 | opt-in **auto-selection** preview (`ATO_READY_STATE_UFFD_AUTO_PREVIEW`) |

## Benchmark evidence (U9, #888 — 512 MiB memory, 5 iters, medians)

| app | file-cold | file-warm | uffd-local | **uffd-hotset** | uffd-remote |
|---|---:|---:|---:|---:|---:|
| tiny-http | 803 | 118 | 180 | **137** | 165 |
| light-python | 903 | 218 | 433 | **260** | 428 |
| light-node | 873 | 193 | 443 | **233** | 465 |

- **UFFD-hotset beats File-cold 3.3–6×**; hotset persistence (U12) cuts demand faults
  ~3000→2–5 on the second restore.
- **File-warm is fastest** (amortizes 512 MiB on disk); uffd-hotset is within ~40 ms
  while materializing **0 bytes**.
- **Remote read-through fetched 0 chunks** — CAS dedup already puts 52/55 memory chunks
  locally; remote's value needs a network CAS (P4). Full analysis:
  [`uffd-mem-backend.md`](./uffd-mem-backend.md) · raw:
  `benchmarks/ready-state/uffd-productization/2026-07-01/`.

## Recommended default policy

- **Default `ato run` stays the File backend.** With every `ATO_READY_STATE_UFFD_*`
  and `ATO_FC_UFFD*` flag unset, the restore path is byte-for-byte the pre-spike File
  path (guarded by `uffd_mode_is_env_only_and_defaults_to_file`, U7).
- **Enable UFFD (with hotset) for the win case:** cold cache / large or remote memory
  image / no-binding capsule — via the opt-in preview (`ATO_READY_STATE_UFFD_PREVIEW`)
  or auto-select (`ATO_READY_STATE_UFFD_AUTO_PREVIEW`).
- **Do not auto-enable UFFD for all capsules.** When File is warm and the image is
  small, File is already competitive; UFFD's value is conditional (U9).

## Release gate (nightly → dev → main)

A UFFD change may promote only when ALL hold:

1. **Default File path unchanged** with all UFFD flags unset (U7 invariant test in
   `cargo test -p snapshot`, part of normal CI).
2. **Receipt schema frozen** (`UFFD_RECEIPT_SCHEMA_VERSION`, U8) — anything depending
   on it pins the version.
3. **KVM smokes green when run one-per-invocation** via
   `scripts/ready-state/run-uffd-kvm-smokes.sh` (U7). The **combined** `fc_kvm_uffd`
   suite flakes under host pressure and is **NOT** a release gate.
4. **Fail-closed proven on the product path** (U13): CAS miss/corrupt → `Err`, no
   orphan VM/tap/firecracker; corrupt/mismatched hotset profile → ignored, never
   corrupts guest memory.
5. **No-binding-only** enforced end-to-end (the binding guard + the selector).

## Known limitations

- **No-binding capsules only.** Secrets/bindings/external → fail-closed until Phase 8.
- **Remote read-through is preview/spike-level** — off by default, not auto-selected;
  real network CAS + locality/timeout policy is **P4**.
- **Corrupt-CAS fail-closed is safe but slow** (~15 s `LoadSnapshot` timeout) — the
  page-server cannot serve the corrupt first fault. Acceptable (Err + clean), not fast.
- **Host coverage is x86_64 + kernel `userfaultfd`** (U0 probe is truthful elsewhere).
- The auto-selector is **preview-gated** (U15); it is a dry-run/opt-in, not a default.

## Phase 8 dependency (hard)

UFFD restores a **pre-bind, secret-free** session. **Binding-required capsules must
not run under UFFD (or any restore) until Phase 8 `BindingLease` exists** (#863 /
[`binding-lease.md`](./binding-lease.md)); there is **no post-bind snapshot /
checkpoint / re-seal**. The no-binding-only constraint in U11–U15 is the placeholder
for that boundary.

## Go / no-go

- ✅ **Product PREVIEW: GO.** Opt-in, no-binding, local CAS, hotset-persisted,
  fail-closed, KVM-validated. Safe to exercise on `nightly` and to demo.
- ⛔ **Default enablement: NO-GO (yet).** Requires: the placement contract fully wired
  (P1, beyond the U14/U15 dry-run), broader host/kernel coverage, and Phase 8 for any
  binding-required capsule. Revisit when those land.

See [`uffd-productization-roadmap.md`](./uffd-productization-roadmap.md) (U7–U16),
[`uffd-productization-plan.md`](./uffd-productization-plan.md) (P0–P5), and
[`uffd-receipt-schema.md`](./uffd-receipt-schema.md).
