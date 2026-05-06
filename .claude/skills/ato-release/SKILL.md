---
name: ato-release
description: 'Cut a new ato-run/ato release (v0.4.x). Bumps the four dist-tracked crates in lockstep, tags from main, monitors desktop-release.yml + cargo-dist release.yml, and recovers from the known failure modes (frontend dist, 403 on host, draft/published race). Use whenever the user says "リリースする", "0.4.x で公開", "cut a release", or `curl install.sh` is broken because of missing assets.'
license: MIT
---

# ato-release

Recipe for shipping a working `vX.Y.Z` from `ato-run/ato`. Optimised for the common failures we have actually hit in practice (v0.4.98–v0.4.101) so future releases do not re-discover them.

## What "released correctly" means

After you finish, `curl -fsSL https://ato.run/install.sh | sh` on a clean macOS host must install `ato`, `nacelle`, **and** the Desktop bundle without a single 404. The single source of truth for that is the GitHub Release for the tag — every asset that `install.sh` may probe must live on the **published** release, not on a draft.

Expected asset count when everything succeeds: **30**.
- 11 `ato-cli-*` (4 platform tar.xz + sha256 + windows .msi + .zip + their sha256 + `ato-cli.rb`)
- 11 `nacelle-*` (same shape)
- 4 `Ato-Desktop-*` (darwin-arm64.zip, windows-x86_64.{msi,zip}, x86_64.AppImage)
- 4 cargo-dist meta (`source.tar.gz` + sha256, `sha256.sum`, `dist-manifest.json`)

## Pre-flight (do before touching anything)

1. `git switch main && git pull --ff-only`
2. `git status` clean
3. Verify the 4 dist-tracked crate versions are aligned at the *previous* release:
   ```
   grep '^version = ' \
     crates/ato-cli/Cargo.toml \
     crates/ato-desktop/Cargo.toml \
     crates/ato-desktop/xtask/Cargo.toml \
     crates/nacelle/Cargo.toml
   ```
   All four must match (e.g. all at `0.4.100`). **If `nacelle` is behind, that is the bug** — cargo-dist `precise-builds = true` silently skips packages whose version ≠ tag, and that is exactly how `nacelle` disappeared from v0.4.98 → v0.4.100 releases. Catch it up in the same bump commit.

## Bump commit (single commit on main)

Edit five files, no more, no less:

- `crates/ato-cli/Cargo.toml` — bump `version`
- `crates/ato-desktop/Cargo.toml` — bump `version`
- `crates/ato-desktop/xtask/Cargo.toml` — bump `version` (must match `ato-desktop`; xtask reads `CARGO_PKG_VERSION` to embed into installer filenames)
- `crates/nacelle/Cargo.toml` — bump `version`
- `Cargo.lock` — update exactly four entries (`ato-cli`, `ato-desktop`, `ato-desktop-xtask`, `nacelle`). Edit the `version = "..."` line directly under each `name = "..."`. Do NOT run `cargo update`; that pulls in unrelated transitive bumps.

Sanity check after edits:
```
git diff --stat
# expected: 5 files, ~12 insertions(+), ~12 deletions(-)
grep -n '"X.Y.Z-prev"' crates/*/Cargo.toml crates/ato-desktop/xtask/Cargo.toml Cargo.lock
# expected: zero matches
```

Commit (use the current git-configured author identity; no `Co-Authored-By:` trailer):
```
git commit -m "chore(release): bump to X.Y.Z"
git tag -a vX.Y.Z -m "vX.Y.Z"
git push origin main
git push origin vX.Y.Z
```

The two `push` calls are deliberate. Pushing the tag separately lets you confirm `main` is healthy before you arm CI.

## What the push triggers

Two workflows run in parallel on the `vX.Y.Z` tag:

| Workflow | What it builds | Slowest job | Where it publishes |
|----------|----------------|-------------|--------------------|
| `desktop-release.yml` | 3 desktop bundles (`Ato-Desktop-*.{zip,msi,AppImage}`) | bundle Windows (~15 min) | uploads to existing release |
| `release.yml` (cargo-dist) | ato-cli + nacelle for 4 targets | `host` (waits for all builds, then `gh release create`) | **creates** the release |

Both want to land on the same `vX.Y.Z` GitHub Release. cargo-dist creates it; desktop-release uploads to it. The **race** is the headline issue (see Failure 3 below).

Monitor:
```
gh run list --limit 5 --json databaseId,name,status,conclusion,headBranch,displayTitle \
  | jq -r '.[] | select(.headBranch == "vX.Y.Z") | "\(.databaseId) \(.name): \(.status) \(.conclusion // "-")"'
```

## Known failure modes

### Failure 1 — desktop-release: `frontend dist missing`

**Symptom**: All three `bundle *` jobs fail in `cargo run -- bundle` with:
```
thread 'main' panicked at crates/ato-desktop/build.rs:24:5:
frontend dist missing at .../crates/ato-desktop/frontend/dist
```

**Cause**: `crates/ato-desktop/build.rs` panics if `frontend/dist` is absent. The build script was added after v0.4.99 but the workflow did not learn to populate `frontend/dist` first.

**Fix**: workflow already has the right steps on `main` (commit `8545aa7` — `ci(desktop-release): build frontend dist before bundling`). Each platform job runs Setup Node 20 → pnpm@10.15.0 → `pnpm install --frozen-lockfile && pnpm run build` in `crates/ato-desktop/frontend/` before the bundle step. If a future change reverts those steps, restore them; do **not** add `ATO_DESKTOP_SKIP_FRONTEND_BUILD=1` as a workaround — that ships an empty `host_panels` to users.

### Failure 2 — cargo-dist `host`: HTTP 403 "Resource not accessible by integration"

**Symptom**: All `build-local-artifacts` succeed; `host` job fails on `gh release create` with `HTTP 403`.

**Cause**: org-level **Workflow permissions** policy on `ato-run` is set to "read", which caps `GITHUB_TOKEN` to read regardless of what the workflow's `permissions: contents: write` block declares. We hit this on v0.4.100. v0.4.101 worked, so the cap may be intermittent (org policy edit, GitHub change, or token-issue timing).

**Fix paths, in order of preference**:
1. **Org admin flips the policy** — Settings → Actions → "Read and write permissions". This is the only durable fix.
2. **Repo-level override** (only works if org allows): `gh api -X PUT repos/ato-run/ato/actions/permissions/workflow -f default_workflow_permissions=write -F can_approve_pull_request_reviews=false`. If the org policy blocks it, the API responds `409 Conflict` — escalate to org admin.
3. **Hot-patch the failed release** without changing settings:
   ```
   # 1) download cargo-dist's artifacts from the run
   WORKDIR=$(mktemp -d)
   gh run download <RELEASE_RUN_ID> --pattern 'artifacts-*' --dir "$WORKDIR"

   # 2) upload to whatever release already exists for the tag (desktop-release
   #    has likely created a draft fallback by now — see Failure 3)
   find "$WORKDIR" -type f \( \
       -name "ato-cli-*.tar.xz" -o -name "ato-cli-*.tar.xz.sha256" \
       -o -name "ato-cli-*.msi"   -o -name "ato-cli-*.msi.sha256" \
       -o -name "ato-cli-*.zip"   -o -name "ato-cli-*.zip.sha256" \
       -o -name "nacelle-*.tar.xz" -o -name "nacelle-*.tar.xz.sha256" \
       -o -name "nacelle-*.msi"   -o -name "nacelle-*.msi.sha256" \
       -o -name "nacelle-*.zip"   -o -name "nacelle-*.zip.sha256" \
       -o -name "ato-cli.rb" -o -name "nacelle.rb" \
       -o -name "source.tar.gz" -o -name "source.tar.gz.sha256" \
       -o -name "sha256.sum" \
     \) -print0 | xargs -0 gh release upload vX.Y.Z --clobber

   # 3) replace title + body with cargo-dist's announcement, un-draft
   ARTIFACTS_DIR="$WORKDIR" python3 -c '
   import json, os
   m = json.load(open(os.environ["ARTIFACTS_DIR"] + "/artifacts-dist-manifest/dist-manifest.json"))
   open("/tmp/notes.txt","w").write(m["announcement_github_body"])
   ' && gh release edit vX.Y.Z --title "vX.Y.Z" --notes-file /tmp/notes.txt --draft=false
   ```

### Failure 3 — Two releases share the same tag (the headline race)

> **Status (post-v0.4.101)**: the workflows now self-heal. `desktop-release.yml` waits up to **15 min** (30 × 30s) and falls through to creating the release under the **canonical name `vX.Y.Z`** (no "Ato Desktop" prefix, no `--draft`). `release.yml`'s `Create GitHub Release` step is **idempotent** — if a release for the tag already exists, it `gh release edit`s the title/notes and `gh release upload --clobber`s its artifacts instead of `gh release create`. So in normal operation only one release object exists. This section stays as a recovery cookbook in case both safeguards somehow fail (e.g. tag pushed before the workflows were updated).

**Symptom**: `gh release list` shows two entries for the tag — `Ato Desktop vX.Y.Z` (draft, 4 desktop assets) and `vX.Y.Z` (published, 26 cargo-dist assets). `install.sh` 404s on `Ato-Desktop-X.Y.Z-darwin-arm64.zip` even though the Desktop bundle was clearly built.

**Cause (historical, pre-fix)**: `desktop-release.yml`'s `publish-release` job polled for the release with `for attempt in 1 2 3 4 5 6` — only **6 × 30s = 3 minutes**. cargo-dist's `host` job typically doesn't even *start* until ~8 minutes after desktop-release's bundles finish (because Windows + nacelle compile is the long pole on cargo-dist). desktop-release fell through to creating its own draft, then uploaded desktop bundles into the draft. Minutes later cargo-dist created the published release with the same tag — but they were two distinct release objects in GitHub.

Confirm with:
```
gh api repos/ato-run/ato/releases --jq '.[] | select(.tag_name=="vX.Y.Z") | "id=\(.id) name=\(.name) draft=\(.draft) assets=\(.assets | length)"'
```

**Recovery (consolidate into the published release)**:
```
# Identify the two release ids
DRAFT_ID=$(gh api repos/ato-run/ato/releases --jq '.[] | select(.tag_name=="vX.Y.Z" and .draft==true) | .id')
PUB_TAG=vX.Y.Z

# Download the stranded desktop assets from the draft
mkdir -p /tmp/v0.4.x-desktop && cd /tmp/v0.4.x-desktop
gh api repos/ato-run/ato/releases/$DRAFT_ID --jq '.assets[] | "\(.id) \(.name)"' \
  | while read id name; do
      gh api -H "Accept: application/octet-stream" \
             repos/ato-run/ato/releases/assets/$id > "$name"
    done

# Upload them onto the published release
gh release upload "$PUB_TAG" Ato-Desktop-* --clobber

# Delete the now-empty draft
gh api -X DELETE repos/ato-run/ato/releases/$DRAFT_ID
```

**Permanent fix (already applied on main)**: see `desktop-release.yml` (retry `$(seq 1 30)` + canonical fall-through name) and `release.yml` `host` step (idempotent `edit`/`upload --clobber` when the release already exists). Both files carry inline comments pointing back to this skill.

## Verification (do this every time)

1. **Asset count is 30** (or document why not):
   ```
   gh release view vX.Y.Z --json assets --jq '.assets | length'
   ```
2. **No draft sibling**:
   ```
   gh api repos/ato-run/ato/releases --jq '.[] | select(.tag_name=="vX.Y.Z") | .draft' \
     | sort -u   # expected: only "false"
   ```
3. **Smoke test installer end-to-end** on a clean machine:
   ```
   curl -fsSL https://ato.run/install.sh | sh
   # expect: ato + nacelle + Ato Desktop installed, no 404 lines
   ```
4. **Homebrew tap updated**: `brew install ato-run/ato/ato-cli` should resolve to the new version. `publish-homebrew-formula` runs as part of cargo-dist `release.yml` and auto-PRs the tap repo (`ato-run/homebrew-ato`).

## Hard rules

- **All four crates bump together.** ato-cli, ato-desktop, ato-desktop-xtask, nacelle. Same version. No exceptions. cargo-dist will silently drop any laggard.
- **Release commit goes on main, not a feature branch.** push:tag triggers two workflows that pin themselves to the tagged commit; they cannot see commits on side branches.
- **Never amend after tagging.** The tag will silently keep pointing at the old commit; the workflows will run on stale code.
- **Do not hardcode `Koh0920` or any other author.** Use the currently authenticated `gh` user for GitHub operations, and use the current repository/global `git config user.name` and `git config user.email` for the release commit. **No `Co-Authored-By:` trailers** in the release commit.
- **Don't `cargo update` to refresh the lockfile.** Edit the four entries by hand. `cargo update` pulls in unrelated dependency bumps that have no business riding inside a release commit.
- **Don't push a release commit if `crates/nacelle/Cargo.toml` lags `crates/ato-cli/Cargo.toml`.** That single check would have prevented the v0.4.98–v0.4.100 install.sh outage.

## Quick reference

```
# 1. bump
git switch main && git pull
$EDITOR crates/{ato-cli,ato-desktop,ato-desktop/xtask,nacelle}/Cargo.toml Cargo.lock
git add -p && git commit -m "chore(release): bump to X.Y.Z"
git tag -a vX.Y.Z -m "vX.Y.Z"
git push origin main vX.Y.Z

# 2. monitor
gh run watch $(gh run list --workflow=Release --limit=1 --json databaseId --jq '.[0].databaseId')
gh run watch $(gh run list --workflow=desktop-release --limit=1 --json databaseId --jq '.[0].databaseId')

# 3. verify
gh release view vX.Y.Z --json assets --jq '.assets | length'  # expect 30
```
