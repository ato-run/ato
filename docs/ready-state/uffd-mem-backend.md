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
