# Runner workspace capacity preflight (draft)

Existing workspace and VolumeStore admission retain their limits. Before a lease
creates local files, observe available space on the nearest existing work-root
ancestor and refuse a completely full filesystem with a typed failure. Before
unpacking a digest-verified uncompressed workspace tar, require at least its byte
length on that same filesystem. Bound both HTTP Content-Length and streamed bytes
to the existing workspace artifact limit.

This observation is outside K/D and does not change identity, execution authority,
state placement or snapshot semantics. It is not a reservation, quota, complete
expanded-size accounting, Docker image-store admission or immunity to concurrent
writes. Filesystem errors propagate rather than being treated as unknown success.
No bytes are deleted or paths changed to make a refused lease run.

Regression boundaries: refuse before download when full; reject oversized actual
streams even with missing or lying headers; preserve an existing workspace when
capacity is insufficient; retain content-address, expansion, containment and atomic
unpack checks. Validation is recorded in docs/ops/oss-790-nonroot-state-2026-10-15.md.
