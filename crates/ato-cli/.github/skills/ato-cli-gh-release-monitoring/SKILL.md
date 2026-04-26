---
name: ato-cli-gh-release-monitoring
description: "Deploy and release ato-cli using gh and sleep across dev, main, and tagged GitHub Releases. Use when pushing release-related changes, creating dev-to-main PRs, monitoring required GitHub checks, merging after green, dispatching the Release PR or release-tag workflow via workflow_dispatch, handling release-plz PRs, creating patch bump PRs after version collisions, and verifying the published GitHub Release assets."
argument-hint: "Describe the ato-cli change or release target to ship"
user-invocable: true
---

# ato-cli GH Release Monitoring

## What This Skill Does

This skill packages the repo-specific release workflow for ato-cli when shipping changes through GitHub using `gh` and `sleep`.

It covers:

- pushing the current branch
- creating or finding the `dev -> main` PR
- monitoring required checks until green
- merging with the correct GitHub CLI strategy
- monitoring the post-merge workflows on `main`
- dispatching the `Release / Release PR` workflow with command `release-pr` to open the Release PR
- monitoring and merging the release PR
- confirming the release version on `main`
- creating a follow-up patch bump PR when the release version already exists
- dispatching the `Release / Release PR` workflow with command `release` to push the version tag via release-plz
- monitoring the Release workflow and published GitHub Release
- recovering safely when version/tag/release state is inconsistent by moving to the next patch release

## When to Use

Use this skill when the user asks to:

- release ato-cli
- deploy ato-cli from dev to main
- create a PR and monitor checks with `gh`
- wait for CI using `sleep`
- merge after GitHub checks pass
- dispatch the release-plz workflow to create or trigger a Release PR
- dispatch the release-plz workflow to push the version tag
- verify release assets after tag push

Do not use this skill for general coding or debugging. Use it only for the repo shipping workflow.

## Assumptions

- The repo uses `dev` as the staging branch and `main` as the release branch.
- `gh` is installed and authenticated.
- For PR creation, merge, and release inspection, prefer running `gh` with `env -u GH_TOKEN -u GITHUB_TOKEN` so the stored repo-scoped credential is used instead of a narrow environment token.
- The repo may require `--admin` when branch policy blocks normal merge even after all checks are green.
- The version in `Cargo.toml` must be new. If the tag or GitHub Release already exists, do not try to reuse the tag. Create the next patch bump PR and repeat the release cycle.

## Preflight

1. Confirm the current branch with `git branch --show-current`.
2. Inspect `git status --short` before staging or committing anything.
3. If the working tree contains unrelated changes, stage only the files for the target change.
4. Confirm `gh auth status` and watch for missing scopes.
5. Check whether the repo already has a relevant open PR with `gh pr list` or `gh pr status`.
6. Run local checks before pushing:

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features`
- the narrow tests that cover the touched area, if available

7. If local checks fail, fix them before starting the PR/release monitoring flow.

## Phase 1: Ship Change From dev To main

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

## Phase 2: Monitor PR Checks With gh + sleep

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

## Phase 3: Merge The Change PR

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

## Phase 4: Monitor Post-Merge main Workflows

1. Immediately list workflows for the merge commit:

```bash
env -u GH_TOKEN -u GITHUB_TOKEN gh run list --commit <merge-sha> --json databaseId,status,conclusion,workflowName,url
```

2. Wait and re-check with `sleep` until the required workflows on `main` are complete and green:

```bash
sleep 120 && env -u GH_TOKEN -u GITHUB_TOKEN gh run list --commit <merge-sha> --json databaseId,status,conclusion,workflowName,url
```

3. For ato-cli, the workflows to watch on `main` after a change PR merge are:

- `Build (Multi OS)`
- `Security Audit`
- `Secret Scan (gitleaks)`

4. Once all required checks are green, move to Phase 5 to create the Release PR.

Note: `Release / Release PR` no longer triggers automatically on pushes to `main`. The Release PR must be created explicitly via `workflow_dispatch` in Phase 5.

## Phase 5: Dispatch And Follow The Release PR

1. Dispatch the `Release / Release PR` workflow with command `release-pr` to open the Release PR:

```bash
env -u GH_TOKEN -u GITHUB_TOKEN gh workflow run release-plz.yml --ref main --field command=release-pr
```

2. Confirm the workflow started and wait a moment for release-plz to open the PR:

```bash
sleep 30 && env -u GH_TOKEN -u GITHUB_TOKEN gh run list --workflow release-plz.yml --limit 3 --json databaseId,status,conclusion,displayTitle,url
```

3. Find the open release PR, typically titled `chore: release`:

```bash
env -u GH_TOKEN -u GITHUB_TOKEN gh pr list --base main --state open --search 'chore: release in:title' --json number,title,url,headRefName,baseRefName
```

4. Before monitoring it to completion, extract the proposed version and check for collisions immediately:

```bash
env -u GH_TOKEN -u GITHUB_TOKEN gh pr diff <release-pr> --repo ato-run/ato-cli | rg '^\+version\s*=\s*"'
git tag -l 'v<version>'
env -u GH_TOKEN -u GITHUB_TOKEN gh release view v<version> --json tagName,publishedAt,url,isDraft,isPrerelease
```

If the proposed version already exists as a tag or published GitHub Release, do not merge the colliding release PR. Jump directly to the patch bump flow in Phase 7.

5. Inspect it directly:

```bash
env -u GH_TOKEN -u GITHUB_TOKEN gh pr view <release-pr> --json number,title,state,mergeable,mergeStateStatus,headRefName,baseRefName,statusCheckRollup,url
```

6. Monitor it with `gh pr checks <release-pr>` and adaptive `sleep` exactly like the earlier change PR.
7. In the current shortened workflow set, the release PR checks usually include `Build (Multi OS)`, `Security Audit`, and `Release/plan`.
8. Merge it once checks are green, again preferring normal merge first and `--admin` only if policy requires it.
9. Capture the release PR merge commit SHA.

## Phase 6: Confirm The Version Before Tagging

1. Fetch the latest `main`.
2. Read the version directly from `Cargo.toml` on `origin/main`:

```bash
git fetch origin main && git show origin/main:Cargo.toml | rg '^version\s*=\s*"'
```

3. Build the tag name as `v<version>`.
4. Before creating the tag, check both the local tag and GitHub Release state.

## Phase 7: Create The Next Patch Bump PR On Version Collisions

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
10. If release-plz does not open a follow-up PR and `origin/main` already contains the bumped version, treat the bumped merge commit itself as the release commit and continue to Phase 8 (dispatch release tag) after its `main` workflows are green.
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

## Phase 8: Dispatch The Release Tag

Only do this after the release PR merge commit is fully green on `main` and the tag does not already exist.

Dispatch the `Release / Release PR` workflow with command `release`. release-plz detects the merged Release PR, creates the version tag, and pushes it — which triggers the cargo-dist `release.yml` pipeline.

```bash
env -u GH_TOKEN -u GITHUB_TOKEN gh workflow run release-plz.yml --ref main --field command=release
```

Confirm the workflow started and watch for it to push the tag:

```bash
sleep 30 && env -u GH_TOKEN -u GITHUB_TOKEN gh run list --workflow release-plz.yml --limit 3 --json databaseId,status,conclusion,displayTitle,url
```

Verify the tag was created:

```bash
env -u GH_TOKEN -u GITHUB_TOKEN gh api repos/ato-run/ato-cli/git/refs/tags/v<version>
```

The tag push is what triggers the GitHub Release publication workflow for ato-cli.

## Phase 9: Monitor Release Publication

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

### If GitHub Release Exists But Assets Are Incomplete

- Action: keep monitoring the Release workflow
- Do not call the release complete until assets are visible in `gh release view`

## Completion Criteria

The workflow is complete only when all of the following are true:

- the change PR from `dev` to `main` is merged
- the post-merge `main` workflows for that commit are green
- the release PR is merged
- the post-release-merge `main` workflows are green
- the correct version tag exists and points at the intended release merge commit
- the GitHub Release for that tag exists
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

- `Release / Release PR` with command `release-pr` must be dispatched manually (or waits for the Monday schedule) — it no longer fires automatically on every push to `main`.
- `Release / Release PR` with command `release` requires `RELEASE_PLZ_TOKEN` (PAT). If the secret is missing, the workflow fails before pushing any tag and `release.yml` will not trigger.
- Publication starts from the tag that release-plz pushes via the `release` dispatch.
- The repo has shown branch-policy cases where admin merge is required even after checks are green.
- **PR checks are intentionally lightweight:** `Build (Multi OS)` on `pull_request` runs only `clippy`. The full 4-platform build matrix, `smoke`, `native_delivery_e2e_windows`, and `tar_pack_benchmark` run only on `push` to `dev` and `workflow_dispatch`. `V3 Parity` runs only on schedule and `workflow_dispatch`.
- This means PR check time is ~5-7 minutes, making the full release cycle achievable in under 1 hour.
- Expected timing: dev→main PR (~7 min) + post-merge (~3 min) + dispatch release-pr + release PR checks (~7 min) + post-merge (~3 min) + dispatch release (~2 min) + Release workflow (~20 min) = ~42 min total (no collision).
- With one version collision: +bump PR (~7 min + ~3 min) = ~52 min, still under 1 hour.
- Windows native delivery jobs and tar_pack_benchmark run on `dev` push and `workflow_dispatch` only (not PRs).
