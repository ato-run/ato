# H2 archive snapshot: manifest tooling and inventory (2026-11-10)

Status: prepared, **not moved**. The public archive repository has not been
created; moving `docs/archive` needs the owner's approval and a destination
URL. This note records the tooling and the exact snapshot a move would carry.

## Why a manifest

The archive is moving to its own public repository so the product repository
stops carrying ~180 MB of historical material. A reader of the archive must
still be able to check what moved: the exact source commit, the blob id and
content digest of every file, its size, the licence that applies, and where it
lives now. `scripts/archive_manifest.py` produces that evidence. It is
read-only and deterministic: the same source tree yields the same entries and
`entries_digest` on every run.

## Snapshot taken

- Source: `apps/ato` at `origin/dev`
- Origin commit: `55d5221cd597a2b1fe34f671ec4b8b3edadcc049`
- Tree: `docs/archive`
- Files: **488**; total size: **183,558,811 bytes**
- Source licence: **MPL-2.0** (`LICENSE` at the repository root)
- Intended destination base:
  `https://github.com/ato-run/ato-archive/tree/main/docs/archive`
- Entries digest (`sha256` of the canonical entries array):
  `sha256:4c6aa607a87e8a659bf83f94470714f4cef29f59b20a11d28a67f756258980d5`

Command used (run from a checkout; the full manifest is written outside the
repository until the destination exists):

```sh
python3 scripts/archive_manifest.py \
  --repo . --ref origin/dev \
  --archive-path docs/archive \
  --license MPL-2.0 \
  --destination-base https://github.com/ato-run/ato-archive/tree/main/docs/archive \
  --output ../archive-manifest.json
```

Manifest schema: `ato.archive-manifest/1`. Each entry carries `path`, `bytes`,
`declared_size`, `git_blob_sha1`, `sha256`, `media_type`, `license` and
`destination_url`; the top level adds `origin_commit`, `file_count`,
`total_bytes` and `entries_digest`.

## Reference audit (why the move is not a plain delete)

- 24 files in the repository reference `docs/archive`. The compatibility
  redirects/pages that point at moved files must be updated in the same change
  as the move, or they become 404s.
- `apps/desktop` is a legacy launcher shim (three source files; its README says
  the Desktop product lives in `ato-run/ato-desktop`). It is still referenced by
  `.github/workflows/desktop-release.yml` (builds `apps/desktop/xtask`, bundles
  `apps/desktop/assets/system/*`, uploads dmg/msi/zip for `v*` and
  `ato-desktop-v*` tags), `desktop-tauri-ci.yml`, `release-dry-run.yml`,
  `release.yml`, `CLAUDE.md` and `docs/desktop.md`. Isolating the shim needs the
  same reference-zero treatment as the archive: classify each reference first,
  then move, then update the workflows in one reviewed change.
- Cross-repository "ato-desktop" references (PWA bridge, API telemetry/schema,
  samples host-bridge docs) mean the separate Desktop product repository, not
  `apps/desktop`; they are not moved by this work.

## Pending

1. Owner approval for a public archive repository (proposed:
   `ato-run/ato-archive`), including its licence statement.
2. Move the tree, add the generated manifest and a README explaining the
   snapshot, then update the 24 references and the archive index in one change.
3. Isolate the legacy `apps/desktop` shim after its workflow references are
   classified and updated (separate change, same evidence discipline).
