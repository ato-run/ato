# A1 Garbage Collection

Status: Draft

## Goal

A1 adds reusable dependency blobs. GC must reclaim unreferenced blobs without
removing installed capsules, pinned artifacts, or dependencies used by active
runs.

## Command Sketch

```bash
ato gc
ato gc --dry-run
ato gc --max-age 30d
ato gc --target-free 20GiB
```

`--dry-run` prints candidate blobs and bytes without deleting. Default GC should
be conservative and delete only blobs older than the configured grace period.

## Automatic Triggers

Automatic GC may run after materialization when either condition is met:

- `~/.ato/store` exceeds a configured disk usage threshold.
- Unreferenced blobs exceed the configured age threshold.

Automatic GC must skip when another GC lock is active.

## Reachability

Strong roots:

- `store/refs/installed/<capsule-id>.json`
- `store/refs/pins/<name>.json`
- active `runs/<session>/session.json`

Weak refs:

- `store/refs/deps/<ecosystem>/<derivation-hash>.json`

Reachability starts from strong roots only. Traversal may read weak refs named
by strong roots, but it must not keep every `refs/deps` entry alive by default.
This keeps derivation cache entries disposable.

## Deletion Order

1. Acquire GC lock.
2. Snapshot roots.
3. Compute reachable blob hashes.
4. List blob directories.
5. Delete only unreachable blobs older than the grace period.
6. Remove dangling weak refs after blob deletion.

If any active run appears or disappears during the scan, GC should restart or
skip deletion for that cycle.
