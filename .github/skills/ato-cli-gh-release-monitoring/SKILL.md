---
name: ato-cli-gh-release-monitoring
description: "Release ato-cli or nacelle end-to-end using gh and sleep until the final GitHub Release is published. Use when pushing release-related changes, creating and monitoring PRs, handling ato-cli release-plz PRs, tagging ato-cli or nacelle, watching release workflows, verifying GitHub Release assets, and recovering from version or tag collisions."
argument-hint: "Describe which repo to release (ato-cli or nacelle) and the release target to ship"
user-invocable: true
---

# Ato GH Release Monitoring

## What This Skill Does

This skill packages the repo-specific GitHub shipping workflow for both `ato-cli` and `nacelle` when releasing through GitHub using `gh` and `sleep`.

It covers:

- selecting the correct local repo and remote slug
- pushing the current branch
- creating or finding the release PR
- monitoring required checks until green
- merging with the correct GitHub CLI strategy
- handling the `ato-cli` `dev -> main` plus `release-plz` flow
- handling the `nacelle` direct `main -> tag -> release` flow
- confirming the release version before tagging
- creating and pushing the version tag
- monitoring the release workflow and published GitHub Release
- verifying release assets after publication
- recovering safely when version, tag, or release state is inconsistent by moving to the next patch release

## When to Use

Use this skill when the user asks to:

- release ato-cli
- release nacelle
- deploy ato-cli from dev to main
- deploy nacelle to a GitHub release
- create a PR and monitor checks with `gh`
- wait for CI using `sleep`
- merge after GitHub checks pass
- follow `release-plz` output for ato-cli
- tag and publish a GitHub Release for ato-cli or nacelle
- verify release assets after tag push

Do not use this skill for general coding or debugging. Use it only for the repo shipping workflow.

## Repository Profiles

### ato-cli

- Local repo root: `apps/ato-cli`
- Release remote: usually `ato-run/ato-cli`
- Staging branch: `dev`
- Release branch: `main`
- Release PR automation: `release-plz`
- Publication: tag-triggered `Release` workflow via `cargo-dist`, publishing GitHub Release assets

### nacelle

- Local repo root: `apps/nacelle`
- Release remote: derive from `git remote get-url origin`
- Default release branch: `main` unless the user explicitly says otherwise
- Release PR automation: none by default
- Publication: tag-triggered `Build and Publish nacelle` workflow, publishing GitHub Release assets and optionally post-publish CDN/R2 side effects if configured

## Common Assumptions

- `gh` is installed and authenticated.
- For PR creation, merge, and release inspection, prefer running `gh` with `env -u GH_TOKEN -u GITHUB_TOKEN` so the stored repo-scoped credential is used instead of a narrow environment token.
- The repo may require `--admin` when branch policy blocks normal merge even after all checks are green.
- The version in `Cargo.toml` must be new. If the tag or GitHub Release already exists, do not try to reuse the tag. Create the next patch bump change and repeat the release cycle.

## Repository Selection

1. Identify which repo the user wants to release.
2. Resolve the local git root first:

```bash
git -C apps/ato-cli rev-parse --show-toplevel
git -C apps/nacelle rev-parse --show-toplevel
```

3. Resolve the GitHub repo slug from the checked-out repo before hardcoding `gh --repo`:

```bash
git -C <repo-root> remote get-url origin
```

4. Use repo-scoped commands for the selected repository only. Do not mix `ato-cli` and `nacelle` release commands in the same `gh` or `git` invocation.

## Preflight

1. Confirm the current branch with `git -C <repo-root> branch --show-current`.
2. Inspect `git -C <repo-root> status --short` before staging or committing anything.
3. If the working tree contains unrelated changes, stage only the files for the target change.
4. Confirm `gh auth status` and watch for missing scopes.
5. Check whether the repo already has a relevant open PR with `gh pr list` or `gh pr status`.
6. Run local checks before pushing.

For `ato-cli`:

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features`
- the narrow tests that cover the touched area, if available

For `nacelle`:

- `cargo fmt --all -- --check`
- `cargo test --lib`
- `cargo check`
- the narrow tests that cover the touched area, if available

7. If local checks fail, fix them before starting the PR/release monitoring flow.

## Workflow Split

- Use the `ato-cli` path when releasing the CLI through `dev`, `main`, `release-plz`, and the final version tag.
- Use the `nacelle` path when releasing `nacelle` directly through a normal PR to `main`, then a version tag, then the publish workflow.

## ato-cli Phase 1: Ship Change From dev To main

1. Commit only the intended release-related change.
2. Push the branch, usually `git push origin dev`.
3. Check whether a `dev -> main` PR already exists:

```bash
env -u GH_TOKEN -u GITHUB_TOKEN gh pr list --state open --head dev --base main --json number,title,url,headRefName,baseRefName
```

4. If none exists, create it:

```bash
env -u GH_TOKEN -u GITHUB_TOKEN gh pr create --base main --head dev --title "<title>" --body "<body>"
```

5. Record the PR number and URL immediately.

## ato-cli Phase 2: Monitor PR Checks With gh + sleep

1. Inspect the PR status:

```bash
env -u GH_TOKEN -u GITHUB_TOKEN gh pr view <pr> --json mergeable,mergeStateStatus,statusCheckRollup,url,headRefOid
```

2. If required checks are still running, start with short waits and only widen the interval when the remaining jobs are clearly long-running:

```bash
sleep 60 && env -u GH_TOKEN -u GITHUB_TOKEN gh pr view <pr> --json mergeable,mergeStateStatus,statusCheckRollup,url,headRefOid
```

3. Increase the wait window for slower matrix jobs:

- 60 to 120 seconds for PRs (lightweight: clippy + linux-x86_64 only, ~5-7 min total)
- 180 seconds once only matrix builds remain
- 240 to 300 seconds only when you are waiting on known long-running artifact or release publication jobs

4. Use `gh pr checks <pr>` for a compact summary once checks look close to done.
5. Treat skipped release jobs in pull request mode as expected if `gh pr checks` reports all checks successful.

## ato-cli Phase 3: Merge The Change PR

1. Attempt a normal merge first:

```bash
env -u GH_TOKEN -u GITHUB_TOKEN gh pr merge <pr> --merge --delete-branch=false
```

2. If GitHub reports branch policy blocking the merge after all checks are green, retry with admin merge:

```bash
env -u GH_TOKEN -u GITHUB_TOKEN gh pr merge <pr> --merge --delete-branch=false --admin
```

3. Record the merge commit SHA:

```bash
env -u GH_TOKEN -u GITHUB_TOKEN gh pr view <pr> --json mergedAt,mergeCommit,url,baseRefName,headRefName
```

## ato-cli Phase 4: Monitor Post-Merge main Workflows And Poll For release-plz Early

1. Immediately list workflows for the merge commit:

```bash
env -u GH_TOKEN -u GITHUB_TOKEN gh run list --commit <merge-sha> --json databaseId,status,conclusion,workflowName,url
```

2. In parallel, poll for the release-plz PR instead of waiting for every main workflow to finish before looking for it:

```bash
until env -u GH_TOKEN -u GITHUB_TOKEN gh pr list --base main --state open --search 'chore: release in:title' --json number,url | jq -e 'length > 0' >/dev/null; do
	echo "Waiting for release-plz PR..."
	sleep 30
done
```

3. Wait and re-check with `sleep` until the required workflows on `main` are complete and green:

```bash
sleep 120 && env -u GH_TOKEN -u GITHUB_TOKEN gh run list --commit <merge-sha> --json databaseId,status,conclusion,workflowName,url
```

4. For ato-cli, the workflows to watch on `main` in the shortened release path usually include:

- `Security Audit`
- `Release PR`

`Build (Multi OS)`, `V3 Parity Matrix`, and `Secret Scan (gitleaks)` are expected to run from pull request or scheduled/manual paths rather than every `main` push.

5. Once the release-plz PR exists, move to Phase 5 immediately while only the remaining `main` jobs continue in the background.

## ato-cli Phase 5: Follow The release-plz PR

1. Find the open release PR, typically titled `chore: release`:

```bash
env -u GH_TOKEN -u GITHUB_TOKEN gh pr list --base main --state open --search 'chore: release in:title' --json number,title,url,headRefName,baseRefName
```

2. Before monitoring it to completion, extract the proposed version and check for collisions immediately:

```bash
env -u GH_TOKEN -u GITHUB_TOKEN gh pr diff <release-pr> --repo ato-run/ato-cli | rg '^\+version\s*=\s*"'
git tag -l 'v<version>'
env -u GH_TOKEN -u GITHUB_TOKEN gh release view v<version> --json tagName,publishedAt,url,isDraft,isPrerelease
```

If the proposed version already exists as a tag or published GitHub Release, do not merge the colliding release PR. Jump directly to the patch bump flow in Phase 7.

3. Inspect it directly:

```bash
env -u GH_TOKEN -u GITHUB_TOKEN gh pr view <release-pr> --json number,title,state,mergeable,mergeStateStatus,headRefName,baseRefName,statusCheckRollup,url
```

4. Monitor it with `gh pr checks <release-pr>` and adaptive `sleep` exactly like the earlier change PR.
5. In the current shortened workflow set, the release PR checks usually include `Build (Multi OS)`, `Security Audit`, and `Release/plan`.
6. Merge it once checks are green, again preferring normal merge first and `--admin` only if policy requires it.
7. Capture the release PR merge commit SHA.

## ato-cli Phase 6: Confirm The Version Before Tagging

1. Fetch the latest `main`.
2. Read the version directly from `Cargo.toml` on `origin/main`:

```bash
git fetch origin main && git show origin/main:Cargo.toml | rg '^version\s*=\s*"'
```

3. Build the tag name as `v<version>`.
4. Before creating the tag, check both the local tag and GitHub Release state.

## ato-cli Phase 7: Create The Next Patch Bump PR On Version Collisions

If any of the following are true, do not force the tag and do not reuse the existing release:

- `git tag -l 'v<version>'` already returns the tag
- `gh release view v<version>` already shows a published release
- the existing tag points to an older merge commit

Useful commands:

```bash
git rev-list -n 1 v<version>
git show -s --format='%H %ci %s' v<version>
env -u GH_TOKEN -u GITHUB_TOKEN gh release view v<version> --json tagName,publishedAt,url,isDraft,isPrerelease
```

If a collision exists, the correct next step is a patch bump and a fresh release PR, not tag reuse.

1. Create a new branch from the current `main`, for example `release-bump-v<next-patch>`.
2. Bump the patch version in the release-managed files for ato-cli:

- `Cargo.toml`
- `Cargo.lock`
- `CHANGELOG.md`
- `core/Cargo.toml` and `core/CHANGELOG.md` only if `capsule-core` public API changed

3. Commit the bump with a release-focused message.
4. Push the bump branch.
5. Create a PR targeting `main` that explains the previous version collision and the new patch version.
6. Monitor that PR with the same `gh` + `sleep` loop used earlier.
7. Merge it after checks are green, using `--admin` only if branch policy still blocks a green PR.
8. Monitor the post-merge `main` workflows for the bump merge commit until green.
9. Re-check whether release-plz opened a new `chore: release` PR for the bumped version.
10. If release-plz does not open a follow-up PR and `origin/main` already contains the bumped version, treat the bumped merge commit itself as the release commit and continue to tagging after its `main` workflows are green.
11. Otherwise follow the new release PR through the same monitoring and merge flow.

Useful commands for the bump PR path:

```bash
git checkout main && git pull --ff-only origin main
git checkout -b release-bump-v<next-patch>
env -u GH_TOKEN -u GITHUB_TOKEN gh pr create --base main --head release-bump-v<next-patch> --title "Bump release to v<next-patch>" --body "## Summary
- previous release version v<current-version> is already tagged and published
- bump patch version to v<next-patch>
- unblock release tagging for the next release-plz cycle"
```

## ato-cli Phase 8: Tag And Push The Release

Only do this after the release PR merge commit is fully green on `main` and the tag does not already exist.

```bash
git tag -a v<version> <release-merge-sha> -m "ato-cli v<version>"
git push origin v<version>
```

This tag push is what triggers the GitHub Release publication workflow for ato-cli.

## ato-cli Phase 9: Monitor Release Publication

1. Watch runs associated with the tag or latest release workflow:

```bash
env -u GH_TOKEN -u GITHUB_TOKEN gh run list --limit 10 --json databaseId,status,conclusion,workflowName,displayTitle,url
```

2. The slow path inside `release.yml` is usually `build-local-artifacts`, then `build-global-artifacts`, then `host`. Poll job status directly when you want to know the real bottleneck instead of only checking the top-level run state:

```bash
env -u GH_TOKEN -u GITHUB_TOKEN gh api repos/ato-run/ato-cli/actions/runs/<run-id>/jobs --paginate --jq '.jobs[] | [.name, .status, (.conclusion // "")] | @tsv'
```

3. Wait and re-check with `sleep` until the Release workflow reaches `host` success and the GitHub Release entry appears.
4. Confirm the final GitHub Release:

```bash
env -u GH_TOKEN -u GITHUB_TOKEN gh release view v<version> --json name,tagName,isDraft,isPrerelease,url,assets,publishedAt
```

5. Verify that expected assets are uploaded, not just the shell release entry.
6. You do not need to keep waiting on late signature/provenance uploads if the release entry exists and the expected primary archives are already attached, unless you explicitly need SBOM or sigstore completeness before stopping.

## nacelle Phase 1: Ship Change To main

1. Commit only the intended release-related change in `apps/nacelle`.
2. If you are not already on the intended release branch, create or update a release PR targeting `main`.
3. Push the branch.
4. Check whether a relevant open PR already exists before creating a new one:

```bash
env -u GH_TOKEN -u GITHUB_TOKEN gh pr list --state open --base main --json number,title,url,headRefName,baseRefName
```

5. If none exists, create it:

```bash
env -u GH_TOKEN -u GITHUB_TOKEN gh pr create --base main --head <branch> --title "<title>" --body "<body>"
```

6. Record the PR number and URL immediately.

## nacelle Phase 2: Monitor And Merge The PR

1. Inspect the PR status:

```bash
env -u GH_TOKEN -u GITHUB_TOKEN gh pr view <pr> --json mergeable,mergeStateStatus,statusCheckRollup,url,headRefOid
```

2. Poll with adaptive `sleep` exactly like the `ato-cli` flow.
3. Merge normally first, then retry with `--admin` only if policy blocks a green PR.
4. Capture the merge commit SHA.

## nacelle Phase 3: Confirm The Version Before Tagging

1. Fetch the latest `main`.
2. Read the version from `Cargo.toml` on `origin/main`:

```bash
git fetch origin main && git show origin/main:Cargo.toml | rg '^version\s*=\s*"'
```

3. Build the tag name as `v<version>`.
4. Check whether the tag or GitHub Release already exists before creating anything:

```bash
git tag -l 'v<version>'
env -u GH_TOKEN -u GITHUB_TOKEN gh release view v<version> --json tagName,publishedAt,url,isDraft,isPrerelease
```

5. If a collision exists, bump the patch version in `Cargo.toml` and `Cargo.lock`, merge that bump through `main`, and repeat the tag flow. Do not reuse an existing release tag.

## nacelle Phase 4: Tag And Push The Release

Only do this after the merge commit is fully green on `main` and the tag does not already exist.

```bash
git tag -a v<version> <merge-sha> -m "nacelle v<version>"
git push origin v<version>
```

This tag push triggers the `Build and Publish nacelle` workflow.

## nacelle Phase 5: Monitor Release Publication

1. Watch the tag-triggered workflow:

```bash
env -u GH_TOKEN -u GITHUB_TOKEN gh run list --limit 10 --json databaseId,status,conclusion,workflowName,displayTitle,url
```

2. Inspect jobs directly when needed:

```bash
env -u GH_TOKEN -u GITHUB_TOKEN gh api repos/<owner>/<repo>/actions/runs/<run-id>/jobs --paginate --jq '.jobs[] | [.name, .status, (.conclusion // "")] | @tsv'
```

3. For `nacelle`, the release workflow usually runs these stages in order:

- `build_macos`
- `build_linux`
- `build_windows`
- `publish_github_release`
- `upload_r2` if CDN credentials are configured

4. Confirm the final GitHub Release:

```bash
env -u GH_TOKEN -u GITHUB_TOKEN gh release view v<version> --json name,tagName,isDraft,isPrerelease,url,assets,publishedAt
```

5. Verify that the expected `nacelle-v<version>-darwin-x64`, `darwin-arm64`, `linux-x64`, `linux-arm64`, and `windows-x64.exe` assets are attached, along with checksum files and `SHA256SUMS` if generated.
6. If the workflow also uploads R2 artifacts, treat that as secondary verification. The release is not complete until the GitHub Release itself is present with the expected primary assets.

## Decision Points

### If gh Uses The Wrong Token

- Symptom: missing scopes like `repo` or `read:org`
- Action: rerun commands with `env -u GH_TOKEN -u GITHUB_TOKEN`

### If PR Checks Show Green But Merge Is Still Blocked

- Symptom: `gh pr checks` says successful, but `gh pr merge` says base branch policy prohibits merge
- Action: use `--admin` if repo policy allows and the checks are genuinely green

### If A Required Workflow Is Still Running On main

- Action: do not create the tag yet
- Keep polling the merge commit workflows with `gh run list --commit <sha>`

### If release-plz Produces A Release PR That Reuses An Existing Version

- Action: create the next patch bump PR immediately
- Report which version collided and which patch version will replace it
- Merge the bump PR, wait for a fresh release-plz PR, and continue the workflow on the new version

### If nacelle Tag Or Release Already Exists

- Action: bump the next patch version manually in `apps/nacelle`
- Merge the bump to `main`
- Re-run the tag and release flow on the new version

### If GitHub Release Exists But Assets Are Incomplete

- Action: keep monitoring the Release workflow
- Do not call the release complete until assets are visible in `gh release view`

## Completion Criteria

The workflow is complete only when all of the following are true for the selected repo:

- `ato-cli`:
  - the change PR from `dev` to `main` is merged
  - the post-merge `main` workflows for that commit are green
  - the release PR is merged
  - the post-release-merge `main` workflows are green
  - the correct version tag exists and points at the intended release merge commit
  - the GitHub Release for that tag exists with the expected primary assets
- `nacelle`:
  - the release PR to `main` is merged, or the intended release commit is already on `main`
  - the post-merge `main` workflows for that commit are green
  - the correct version tag exists and points at the intended release commit
  - the `Build and Publish nacelle` workflow finishes successfully
  - the GitHub Release for that tag exists with the expected primary assets

If the user asks to release both repos, complete the full criteria for `ato-cli` first or `nacelle` first as requested, then repeat the entire workflow for the other repo. Do not treat the task as complete after shipping only one of them.

- the release is not draft or prerelease unless intentionally requested
- expected assets are present in the published release
- if a collision occurred, the obsolete version was not reused and the successful published release is the bumped patch version

## Example Prompts

- `/ato-cli-gh-release-monitoring ship this ato-cli change from dev to main and monitor checks`
- `/ato-cli-gh-release-monitoring follow the current release-plz PR and tell me when it is safe to tag`
- `/ato-cli-gh-release-monitoring verify whether v0.4.31 is safe to tag and publish`
- `/ato-cli-gh-release-monitoring if v0.4.31 is already published, create the next patch bump PR and monitor the new release to completion`
- `/ato-cli-gh-release-monitoring monitor the GitHub Release assets after tag push`

## Notes For This Repo

- `Release PR` success on `main` does not publish the current version by itself.
- Publication starts from the pushed version tag.
- The repo has shown branch-policy cases where admin merge is required even after checks are green.
- **PR checks are intentionally lightweight:** `Build (Multi OS)` on `pull_request` runs only `clippy`. The full 4-platform build matrix, `smoke`, `native_delivery_e2e_windows`, and `tar_pack_benchmark` run only on `push` to `dev` and `workflow_dispatch`. `V3 Parity` runs only on schedule and `workflow_dispatch`.
- This means PR check time is ~5-7 minutes, making the full release cycle achievable in under 1 hour.
- Expected timing: dev→main PR (~7 min) + post-merge (~3 min) + release-plz PR (~7 min) + post-merge (~3 min) + tag + Release workflow (~20 min) = ~40 min total (no collision).
- With one version collision: +bump PR (~7 min + ~3 min) = ~50 min, still under 1 hour.
- Windows native delivery jobs and tar_pack_benchmark run on `dev` push and `workflow_dispatch` only (not PRs).
