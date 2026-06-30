# Firecracker UFFD memory backend (U0 — capability probe)

> Status: **U0 (probe + docs only)**, tracks #852 / #853. Ato truthfully reports
> whether a host *could* drive a Firecracker `Uffd` snapshot `mem_backend`. There
> is **no** restore-path change, **no** page-server, **no** `ato run` change, and
> no `MemoryPageIndex` / hotset / `BindingLease` here. U1+ build on this anchor.

## Background

Firecracker's `PUT /snapshot/load` takes a `mem_backend` object whose
`backend_type` is `File` **or** `Uffd` (`src/firecracker/swagger/firecracker.yaml`):

- **`File`** — guest memory is `mmap`'d from `backend_path` (a file). This is
  what Ato uses today (`crates/snapshot/src/firecracker.rs`,
  `"mem_backend": {"backend_type":"File","backend_path": <mem>}`).
- **`Uffd`** — `backend_path` is a **Unix domain socket** that a *page-server*
  listens on. On load, Firecracker registers guest memory with `userfaultfd` and
  hands the page-server the userfault file descriptor + a layout payload over the
  UDS; the page-server then serves guest pages **lazily**, on fault, instead of
  eagerly mapping the whole memory file. This is the basis for fast,
  partial-rehydrate restores (only the hot working set is paged in).

**`mem_backend` and `mem_file_path` are mutually exclusive** — exactly one is
sent. U0 changes neither; it only measures whether `Uffd` *would* be drivable.

## UDS init payload + fd handoff contract (for U1+)

When `Uffd` is used, on `LoadSnapshot` Firecracker connects to the page-server's
UDS and sends, as a single `SCM_RIGHTS` message:

- the **userfault fd** (ancillary data), and
- a JSON body describing the guest memory regions to serve: for each region, the
  base host virtual address, size, and the offset into the snapshot's memory
  layout. The page-server uses the fd + this mapping to `UFFDIO_COPY` pages on
  demand.

U0 does not implement this handshake; it is recorded here as the contract U1
(page-server handshake smoke) implements and tests.

## U0 capability probe

`crates/snapshot/src/uffd.rs` is the pure decision; `FirecrackerBackend::probe()`
populates the additive `BackendCapabilities` facet:

- `supports_uffd_mem_backend: bool`
- `uffd_reason: Option<String>` — a concrete reason when `false`.

`supports_uffd_mem_backend` is `true` **only** when **all** hold (U0 scope):

| precondition | false-reason example |
|---|---|
| host arch is **x86_64** (aarch64 is a later pass) | `"aarch64 not in U0 scope (x86_64 only)"` |
| `/dev/kvm` present | `"/dev/kvm not present"` |
| Firecracker ≥ the version whose swagger declares `Uffd` (pinned `1.0.0`) | `"firecracker 0.25.2 < 1.0.0"` |
| kernel `userfaultfd` (`CONFIG_USERFAULTFD` → `/proc/sys/vm/unprivileged_userfaultfd`) | `"userfaultfd disabled on host (no CONFIG_USERFAULTFD)"` |
| binary present at all | `"firecracker binary not found"` |

Non-Firecracker backends (`fake`/`kata`/`qemu`) report `false` with a reason —
UFFD `mem_backend` is a Firecracker snapshot feature. The facet is **fail-closed
but introspectable**: no panic, no `bail!`, always a reason when unsupported.

It does **not** participate in the placement contract (#816) yet — it is an
informational probe. Wiring it into placement/selection is a later phase.

## Measurement receipt schema (later phases — #852)

U2+ will benchmark `File` vs `Uffd` restore and emit a receipt. The intended
shape (filled by the page-server phases, recorded here so U0 is the anchor):

```text
UffdRestoreReceipt {
  backend            = "firecracker"
  mem_backend        = "file" | "uffd"
  arch, kvm, fc_version, kernel_userfaultfd
  guest_mem_bytes
  pages_total, pages_faulted_in        // uffd only: working-set size
  restore_to_ready_ms                  // LoadSnapshot → readiness
  first_fault_latency_us               // uffd only
  page_server_pid
}
```

## Scope guardrails (U0)

No page-server, no `LoadSnapshot` with `Uffd`, no `ato run` change, no
`MemoryPageIndex`, no hotset, no `BindingLease`. KVM-free unit tests cover every
`false`/unsupported path; a single `#[ignore]`d KVM-gated test asserts the probe
reports `true` on a conforming x86_64 host (mirrors `fc_kvm_probe_available`).

See also [`desktop-runner.md`](./desktop-runner.md) and
[`criu-container-spike.md`](./criu-container-spike.md) for the sibling
capability-probe spikes.

---

# Results — U0–U6 (hardware-validated, 2026-06-30)

All on GCP `n2-standard-4` (Cascade Lake) / Firecracker v1.16.0 / kernel 5.10.223,
read-only-shared rootfs, `fulltest` (FastAPI `/health`) rootfs, 512 MB guest memory.
Each phase is a `#[ignore]`d KVM smoke (`fc_kvm_uffd_*`, behind the test-only
`ATO_FC_UFFD` gate). The default File restore path + `ato run` are unchanged.

| phase | what | receipt |
|---|---|---|
| U0 (#862) | capability probe | truthful `supports_uffd_mem_backend` |
| U1 (#865) | page-server handshake | reaches `/health` from an `.mem` mmap; ~11.7 MiB working set, p50 fault 5 µs |
| U2 (#866) | lazy from **local CAS** (no `.mem` materialization) | `/health` in 363 ms faulting **2.3 % of 512 MB** |
| U3 (#867) | per-restore fault trace | 2874 pre-health pages = the hotset |
| U4 (#868) | **hotset prefetch** | demand faults **2985 → 3**, `/health` **366 → 166 ms (−55 %)** |
| U5 (#869) | fail-closed on CAS miss/corrupt | restore `Err`, no orphan VM/tap |
| U6 (#870) | **remote CAS read-through** | `/health` from remote, **50 / 256 chunks** fetched on demand (489 ms) |

## When UFFD beats the File backend (the U6 gate)

The File backend **eagerly rehydrates the whole memory image** (~512 MB ≈ 0.69 s)
before `LoadSnapshot`, but that cost is **amortized**: the rehydrated `.mem` is
content-addressed and reused, so *warm* File restores skip it (~150–220 ms,
Firecracker-bound). UFFD instead faults in only the **working set** (~12 MiB here =
2.3 %), every restore, with no eager copy.

- **UFFD wins on a cold cache** (first restore on a host, or eviction): File must
  pay the full eager rehydrate; UFFD pays only the working set.
- **UFFD wins as the memory image grows** (multi-GB): the eager-rehydrate cost
  scales with image size, while the working set stays roughly constant — so the
  larger the image relative to its hot set, the more UFFD wins.
- **UFFD wins for remote/streamed artifacts** (U6): only the working set crosses the
  network, not the whole image.
- **File is competitive when warm + small**: a small image with a hot disk cache is
  already Firecracker-bound; UFFD's per-fault overhead (and, for demand-only, its
  higher first-fault latency) buys little. **Hotset prefetch (U4) closes that gap** —
  166 ms time-to-health is in the warm-File range while still demand-loading from CAS.

**Net:** UFFD's value is **large or remote memory images on a cold cache**; the
hotset profile is what makes it competitive even when File is warm. Wiring this into
the product (replacing the `ATO_FC_UFFD` env gate with a placement-contract-driven
`mem_backend` selection, #816) is a separate phase beyond this spike.
