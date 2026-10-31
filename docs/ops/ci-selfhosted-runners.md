# Self-hosted ephemeral runners for ato CI

Status: current as of 2026-10-28. Linux CI jobs in this repository run on a
self-hosted ephemeral runner pool; macOS/Windows jobs and every release/deploy
workflow still run on GitHub-hosted runners.

## What runs where (every workflow, every job)

Complete runner assignment for all 10 workflows in `.github/workflows/`. Check
names are the strings a PR reports. `Hosted minutes` answers whether the job can
consume GitHub-hosted runner minutes.

| Workflow | Job id | Check name | Runner after migration | Why | Hosted minutes |
| --- | --- | --- | --- | --- | --- |
| `rust-ci.yml` | `workspace` | `workspace (ubuntu-latest)` | self-hosted `[self-hosted, linux, x64, ato-ci]` | Linux leg of the workspace matrix | no |
| `rust-ci.yml` | `workspace` | `workspace (macos-latest)` | `macos-latest` | no self-hosted macOS capacity | yes |
| `rust-ci.yml` | `workspace` | `workspace (windows-latest)` | `windows-latest` | no self-hosted Windows capacity | yes |
| `cli-ci.yml` | `cli` | `cli (ubuntu-latest)` | self-hosted `[self-hosted, linux, x64, ato-ci]` | Linux leg of the CLI matrix | no |
| `cli-ci.yml` | `cli` | `cli (macos-latest)` | `macos-latest` | no self-hosted macOS capacity | yes |
| `cli-ci.yml` | `cli` | `cli (windows-latest)` | `windows-latest` | no self-hosted Windows capacity | yes |
| `dep-direction.yml` | `architecture` | `architecture` | self-hosted `[self-hosted, linux, x64, ato-ci]` | Linux-only dependency-boundary check | no |
| `snapshot-contract-ci.yml` | `snapshot` | `snapshot` | self-hosted `[self-hosted, linux, x64, ato-ci]` | Linux-only snapshot materializer gate | no |
| `docs-ci.yml` | `documentation` | `documentation` | self-hosted `[self-hosted, linux, x64, ato-ci]` | Linux-only docs audit (Python + Node) | no |
| `release-dry-run.yml` | `dist-check` | `cargo-dist generate --check + plan` | self-hosted `[self-hosted, linux, x64, ato-ci]` | PR-time release dry run; publishes nothing | no |
| `release-dry-run.yml` | `xtask-build-paths` | `desktop xtask build paths` | self-hosted `[self-hosted, linux, x64, ato-ci]` | PR-time xtask path check | no |
| `release.yml` | `plan` | `plan` | `ubuntu-22.04` | cargo-dist publish path; runs `dist plan` on PRs, creates releases on tags | yes (tag push; plan also on PR) |
| `release.yml` | `build-local-artifacts` | `build-local-artifacts (<target>)`, e.g. `build-local-artifacts (aarch64-apple-darwin)` | dist-computed `matrix.runner`: `macos-14`, `windows-2022`, `ubuntu-22.04`, `ubuntu-22.04-arm` | build one platform's release artifacts | yes (tag push) |
| `release.yml` | `build-global-artifacts` | `build-global-artifacts` | `ubuntu-22.04` | build universal installers | yes (tag push) |
| `release.yml` | `host` | `host` | `ubuntu-22.04` | create/update the GitHub Release (needs `contents: write`) | yes (tag push) |
| `release.yml` | `publish-homebrew-formula` | `publish-homebrew-formula` | `ubuntu-22.04` | push formula to the `ato-run/homebrew-ato` tap (needs `HOMEBREW_TAP_TOKEN`) | yes (tag push) |
| `release.yml` | `announce` | `announce` | `ubuntu-22.04` | publish release announcement | yes (tag push) |
| `desktop-release.yml` | `bundle-macos` | `bundle macOS (darwin-arm64)` | `macos-14` | macOS `.dmg` bundle and signing | yes (tag push / dispatch) |
| `desktop-release.yml` | `bundle-windows` | `bundle Windows (windows-x86_64)` | `windows-2022` | WiX `.msi` build and signing | yes (tag push / dispatch) |
| `desktop-release.yml` | `bundle-linux` | `bundle Linux (linux-x86_64)` | `ubuntu-22.04` | `.AppImage` build | yes (tag push / dispatch) |
| `desktop-release.yml` | `publish-release` | `Publish GitHub Release` | `ubuntu-22.04` | append bundles to the GitHub Release (needs `contents: write`) | yes (tag push / dispatch) |
| `desktop-tauri-ci.yml` | `legacy-desktop-shim` | `legacy apps/desktop shim compiles (macOS)` | `macos-14` | macOS-only workspace-excluded crate | yes (every PR) |
| `desktop-tauri-ci.yml` | `tauri` | `tauri shell (macOS)` | `macos-14` | Tauri macOS bundle requires macOS | yes (every PR) |
| `rfc-docs-pages.yml` | `build` | `build` | `ubuntu-latest` | GitHub Pages artifact + OIDC identity | yes (main push only) |
| `rfc-docs-pages.yml` | `deploy` | `deploy` | `ubuntu-latest` | `github-pages` environment deployment | yes (main push only) |

Two consequences for runner-reference audits:

1. The Linux legs of `rust-ci.yml` / `cli-ci.yml` keep the historical display
   labels `ubuntu-latest` so the PR check names do not change; that string is a
   matrix `label`, **not** a runner reference. The runner for those legs is the
   `[self-hosted, linux, x64, ato-ci]` list.
2. Hosted runner references that intentionally remain are: `macos-latest` and
   `windows-latest` (macOS/Windows legs of `rust-ci.yml`, `cli-ci.yml`),
   `macos-14` (`desktop-tauri-ci.yml`, `desktop-release.yml`), `windows-2022` and
   `ubuntu-22.04` / `ubuntu-22.04-arm` (`release.yml`, `desktop-release.yml`),
   and `ubuntu-latest` (`rfc-docs-pages.yml`).

## Runner label contract

Linux CI jobs target:

```yaml
runs-on: [self-hosted, linux, x64, ato-ci]
```

`self-hosted`, `linux` and `x64` are GitHub built-in labels; `ato-ci` is the
pool's own label. Deploy and production workflows must stay on GitHub-hosted
runners and must not be pointed at this pool.

## Ephemeral execution contract

Each job gets **one disposable VM** and the VM is destroyed afterwards:

- one job per fresh VM; no workspace, toolchain, cache or process state is
  reused between jobs;
- the VM boots from an immutable golden image with a per-job writable overlay;
- target queue time is ~2 minutes when the pool is idle;
- per-VM resources: 6 vCPU, ~5 GiB RAM plus swap, 40 GiB disk (measured
  values to be recorded once the pool is live);
- the job VM has public internet egress only — it cannot reach the runner host,
  the LAN, Docker bridges or the tailnet;
- job logs are kept on the host side, outside the VM.

Because nothing persists in the VM, jobs must stay self-contained: a job that
relies on leftovers from a previous run is a bug, not a cache miss.
`Swatinem/rust-cache` still works because its cache lives in GitHub's cache
service, not on the runner disk; the first run after the switch is cold.

## Untrusted pull requests

`ato-run/ato` is a public repository, so `pull_request` runs execute code from
forks on this infrastructure. The contract is:

- no workflow uses `pull_request_target` with a PR-head checkout — a fork PR can
  only run the `pull_request` (read-only token) path;
- no CI workflow references a secret; CI permissions are `contents: read`;
- only `release.yml` and `desktop-release.yml` declare write scopes or
  reference secrets, and both publish only on tag pushes /
  `workflow_dispatch`; `release.yml` also runs its `plan` job on pull
  requests, where `publishing` is false and nothing is released (token
  analysis in [Residual risk and recommendations](#residual-risk-and-recommendations));
- a failing step still fails the check — there is no `continue-on-error` or
  skip added to hide a red job;
- the repository's fork-PR policy is "require approval for all external
  contributors", so a first-time fork PR needs a maintainer approval before its
  workflows run. Approval is a review gate, not a sandbox: once approved, the
  job still runs untrusted code, which is why the VM-level isolation above is
  load-bearing.

`release.yml` declares `contents: write` because tag pushes publish a GitHub
Release, and it also has a `pull_request` trigger. The risk this creates is
analysed in [Residual risk and recommendations](#residual-risk-and-recommendations);
the working assumption is that it is **not** edited as part of the CI runner
migration.

## Residual risk and recommendations

### Facts

- `release.yml` is generated by cargo-dist, but the committed file is **locally
  customised**: the tag pattern is `'v*'` (dist's default is a version-regex
  form) and the `host` job carries an idempotent "edit existing release, then
  clobber-upload" branch that avoids racing `desktop-release.yml`.
  `allow-dirty = ["ci"]` in `dist-workspace.toml` is what protects those edits
  — `dist generate` skips CI regeneration because of it.
  Regenerating `release.yml` with `dist generate` discards the customisations
  (verified with cargo-dist 0.31.0), so it must not be regenerated to change
  permissions.
- cargo-dist hardcodes the workflow-level `permissions: contents: write`
  (plus `id-token: write` when attestations are enabled) in both 0.31.0 and the
  current upstream main line; there is no configuration key for root
  permissions. `github-custom-job-permissions` only covers custom jobs, and
  `pr-run-mode = "skip"` only removes the `pull_request:` trigger at generation
  time.

### Risk assessment

- **Fork PRs:** GitHub downgrades `GITHUB_TOKEN` to read-only for every
  `pull_request` run originating from a fork, and passes no secrets. The
  declared `contents: write` is therefore **unreachable** from a fork PR. The
  real boundary for untrusted code is the per-job VM described above.
- **Same-repo PRs:** for `pull_request` events the workflow file comes from the
  PR's merge commit, so a contributor with branch-push rights can change
  `release.yml` and use its token. That is not a privilege escalation: the same
  person already has repository write access outside CI. The residual concern is
  accidental release damage, not an outside attacker.
- **What still bounds the damage:** tag creation is blocked by the
  `protect-tags` ruleset, and direct updates to `main`/`dev` are blocked by
  `protect-main-dev` (deletion, non-fast-forward, update and creation rules),
  which `GITHUB_TOKEN` cannot bypass. Release objects are not covered by those
  rules.
- The repository has **no required status checks today** (see the evidence
  file), so none of this blocks merging the runner migration.

### Recommendations (not implemented in this PR — separate decision)

- **A. Drop the PR trigger.** Remove `pull_request:` from `release.yml` and
  record `pr-run-mode = "skip"` in `dist-workspace.toml` so the generated
  intent and the committed file agree. PR-time release validation is already
  covered by `release-dry-run.yml` (`dist generate --check` + `dist plan`).
  Caveat: that workflow currently only runs for PRs targeting `main`/`dev`, so
  `nightly`/`stg` PRs would lose the plan check unless its `branches` filter is
  widened.
- **B. Scope the token per job.** Set workflow-level `permissions: contents:
  read` and add `contents: write` only to `host`, `publish-homebrew-formula` and
  `announce`. This shrinks the blast radius of accidental use but does not
  defend against a same-repo author (see above).
- **C. Label the local customisation.** Add a header comment to `release.yml`
  stating that it is locally customised and must not be regenerated while
  `allow-dirty = ["ci"]` stands, so a future `dist` upgrade does not silently
  drop the tag routing and the idempotent release path.
- **Rejected:** upgrading cargo-dist to obtain configurable root permissions
  (still hardcoded upstream), or regenerating `release.yml` (destroys the local
  customisations).

## What the VM image must provide

The jobs keep using upstream setups where they matter for exactness, but the
image has to supply the host-level pieces:

- `bubblewrap` and `patchelf` (the workflow only falls back to `apt-get` when
  either is missing);
- unprivileged user namespaces enabled (`kernel.unprivileged_userns_clone`, and
  `kernel.apparmor_restrict_unprivileged_userns=0` on Ubuntu 24.04+). The
  workflow still runs the `bwrap --unshare-all ...` isolation canary and fails
  the job when the host cannot provide namespaces;
- passwordless sudo **inside the VM** for the runner user: the workspace tests
  resolve fixture runtimes from the fixed `/opt/ato/toolchains` root
  (`lib/formation/src/intent.rs`), so `rust-ci.yml` provisions
  `/opt/ato/toolchains/python/3.12.7` and `/opt/ato/toolchains/node/22.23.4`
  per job;
- Rust (stable + rustup), Node 22, Python 3, git with submodule support, Docker,
  `build-essential`.

## GitHub-hosted minutes that remain

The table above is the complete list. In short: the macOS and Windows legs of
`rust-ci.yml` / `cli-ci.yml`, `desktop-tauri-ci.yml`, and the release/deploy
workflows (`release.yml`, `desktop-release.yml`, `rfc-docs-pages.yml`) still
consume GitHub-hosted minutes and are the only parts of this repository still
exposed to the account's Actions spending limit and hosted-runner queueing.
The release workflows run on tags only (plus `plan` on PRs), so their cost is
per release rather than per PR.

Linux PR CI no longer consumes hosted minutes.

## Failure semantics

A red test is still a red check: the edited workflows add only least-privilege
permissions, per-ref `concurrency`, job `timeout-minutes`, and the
conditional-dependency changes described above. Every `cargo fmt/check/test/
clippy`, `arch-check`, E2E and artifact-upload step is preserved verbatim.

## Rollback

Each migrated job is independent. Reverting a job to a GitHub-hosted runner is a
one-line change: put back `ubuntu-latest` (or the matrix entry) for that job.
No production behaviour depends on the pool.

## Validation

Static validation (actionlint, YAML parse, and a job/step semantic diff against
`origin/main`) is recorded in
[`docs/evidence/ci-selfhosted-workflows-20261028.txt`](../evidence/ci-selfhosted-workflows-20261028.txt).
A real green job on the pool is tracked separately; until it exists, these
workflows stay in draft and CI jobs targeting `ato-ci` queue rather than run.
