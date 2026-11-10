# OSS quick start: publishing the bundle (manual runbook, 2026-11-10)

Status: procedure, not executed. The workflow builds the artifact; publishing it
is a human step until the owner decides whether the workflow may create
releases. No release is created by this document.

## 1. Build the artifact

- `ato-samples` → Actions → **quick-start bundle** → Run workflow with
  `version` = the `ato` release tag it was verified against (e.g. `v0.9.0`).
- The job downloads that same `ato` release, builds the Excalidraw dist, packs
  the capsules, writes `workspace.toml` and `bundle.json`, and runs the
  verification step. If the verification step printed the "no workspace
  commands yet" notice, the release is not ready for this walkthrough — stop.

## 2. Verify before publishing

- Download the `ato-oss-quick-start` artifact and the `.sha256`.
- On at least two hosts (macOS arm64, Linux x86_64 — Windows still missing):
  - `shasum -a 256 -c …` / `Get-FileHash` matches;
  - extract and run the walkthrough steps;
  - record open/resume timings and the acceptance PASS.

## 3. Attach to a release

- Create an `ato-samples` release with tag **equal to the verified `ato`
  version** (decision pending: same tag vs independent sample tag).
- Attach `ato-oss-quick-start-<version>.tar.gz` and its `.sha256`.
- Release notes: verified `ato` version/commit, capsule SHA-256 list (the three
  digests `workspace.toml` pins), acceptance host/timings, and a link to
  `docs/guides/quick-start-first-workspace.md`.

## 4. After publishing

- Update the README/Quick Start links if the tag scheme differs from
  `releases/download/<version>/…`.
- Record the release URL and the verified CLI version in the program ops doc.
- Re-run the workflow for the next `ato` release; the bundle must not be reused
  across CLI versions without re-running the acceptance check.
