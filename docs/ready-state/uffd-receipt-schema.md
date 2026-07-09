# Ready-State restore receipt schema (U8 — #875)

> The single, versioned schema a File-vs-UFFD benchmark (U9) compares across.
> Written to `<overlay>/.uffd-receipt.json` by `restore()`. Struct:
> `snapshot::uffd_page_server::UffdRestoreReceipt`; version constant
> `UFFD_RECEIPT_SCHEMA_VERSION` (currently **1**).

## Versioning

- `schema_version` — bump on any breaking field change. A **legacy** receipt (pre-U8,
  no `schema_version`) deserializes with `schema_version = 0`; all context/measurement
  fields are `#[serde(default)]` so old receipts always parse forward.

## Fields

### Restore context (filled by `restore()`; File backend fills the shared subset)
| field | type | meaning |
|---|---|---|
| `schema_version` | u32 | schema version (0 = legacy) |
| `backend` | string | snapshot backend id (`firecracker`) |
| `mem_backend` | string | `file` \| `uffd` |
| `source` | string | where pages came from: `file` \| `local_cas` \| `remote_cas` (`zero` for the U1a plumbing mode) |
| `capsule_manifest_hash` | string | capsule identity |
| `runner_class_id` | string? | runner class the restore was pinned to |
| `memory_image_hash` | string | content hash of the memory image (a hotset profile is only valid for a matching image — U12) |
| `memory_bytes_total` | u64 | memory image size |
| `memory_bytes_materialized` | u64 | bytes written to disk before `LoadSnapshot` (File: whole image; UFFD demand: 0) |
| `pages_total` | u64 | `memory_bytes_total / page_size` |

### Page-server measurement (set by `PageServerHandle::receipt`)
| field | type | meaning |
|---|---|---|
| `fd_received` | bool | userfault fd received via `SCM_RIGHTS` |
| `region_count` | u32 | guest memory regions in the handshake |
| `page_fault_count` | u64 | **pages faulted on demand** (`UFFD_EVENT_PAGEFAULT` served) |
| `bytes_copied` | u64 | Σ page_size served (`UFFDIO_COPY`/`UFFDIO_ZEROPAGE`) |
| `first_fault_us` | u128? | latency to the first served fault |
| `p50_fault_service_us` / `p95_fault_service_us` | u128? | per-fault ioctl service time |
| `vm_reaches_health` | bool | real pages served → `/health` reached |
| `time_to_health_ms` | u128? | `LoadSnapshot` → `/health` |
| `page_server_pid` | i32? | local/in-process page-server pid |
| `pre_health_pages` | u64? | distinct pages faulted before `/health` (the hotset — U3) |
| `prefetch_pages` | u64 | pages prefetched from the hotset profile (0 = demand-only — U4) |
| `remote_chunks_fetched` | u64 | memory chunks fetched from the remote CAS via read-through (U6) |

### Outcome (filled by `restore()` / caller)
| field | type | meaning |
|---|---|---|
| `restore_total_ms` | u128? | total restore wall time (rehydrate + LoadSnapshot + health) |
| `fail_closed_reason` | string? | set when the restore failed closed (CAS miss/corrupt, page-server crash — U5); `None` on success |
| `teardown_clean` | bool? | teardown left no orphan VM/tap/overlay/socket (set by the caller when known) |

## File-vs-UFFD comparability (for U9)

The File backend restore fills the **shared subset**: `schema_version`, `backend`,
`mem_backend = file`, `source = file`, `capsule_manifest_hash`, `runner_class_id`,
`memory_image_hash`, `memory_bytes_total`, `memory_bytes_materialized` (= the whole
image, since File eagerly rehydrates), `pages_total`, `time_to_health_ms`,
`restore_total_ms`, `teardown_clean`. The UFFD-specific fields (`page_fault_count`,
`prefetch_pages`, `remote_chunks_fetched`, per-fault latencies) are `0`/`None`/`false`
for File. A benchmark table (U9) compares `time_to_health_ms`, `restore_total_ms`,
`memory_bytes_materialized`, and the fault/prefetch/remote counts across modes.

See [`uffd-mem-backend.md`](./uffd-mem-backend.md) (spike + receipts) and
[`uffd-productization-roadmap.md`](./uffd-productization-roadmap.md) (U7–U16).
