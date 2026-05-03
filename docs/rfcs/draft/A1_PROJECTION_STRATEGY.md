# A1 Projection Strategy

Status: Draft

## Scope

A1 introduces whole-tree dependency derivation cache projection from
`store/blobs/<blob-hash>/` into `runs/<session>/deps`. It does not introduce
file-level CAS. Package/file-level dedup remains delegated to ecosystem tools
such as pnpm and uv.

## Store Layout

```text
~/.ato/store/
├── blobs/
│   └── <blob-hash>/
│       ├── payload/
│       └── blob.json
└── refs/
    └── deps/<ecosystem>/<derivation-hash>.json
```

`refs/deps` is a weak cache index from `derivation_hash` to `blob_hash`.
`blobs/<blob-hash>` is immutable once written. Active runs, installed capsules,
and pins are the only strong roots.

## macOS

On APFS, projection should prefer clone-on-write file cloning via
`COPYFILE_CLONE`. If clonefile is unavailable, fall back to copying into
`runs/<session>/deps`. Source trees must never receive symlink, hardlink, or
clone projections.

## Linux

Linux should prefer overlay or mount namespace projection so cached blobs remain
read-only from the run view. The fallback is hardlink projection plus a
read-only bind mount when available. If neither strict mode is available, A1
must degrade to A0 session materialization rather than writing into source.

## Invariants

- Projection target is always under `~/.ato/runs/<session>/deps`.
- `plan()` performs cache lookup only and does not mutate the filesystem.
- `materialize()` is the only method allowed to create projection paths.
- Projection must not create `node_modules`, `.venv`, `target`, `dist`, or
  `build` under the source root.
- Blob verification happens before projection.
