# GitHub Org Migration Checklist

This document is a how-to guide for moving `ato-cli` from the personal GitHub account `Koh0920` to the GitHub organization `ato-run` while keeping CI, release automation, and repository metadata working.

## Audience

This guide is for maintainers who administer GitHub repository settings, GitHub Actions, and releases.

## Goal

Transfer the repository to `ato-run/ato-cli` without breaking:

- `Build (Multi OS)`
- `Security Audit`
- `V3 Parity Matrix`
- `Release PR`
- tag-triggered release publishing

## Scope

Included:

- repository transfer preparation
- secrets and GitHub App migration
- branch protection and workflow validation
- public metadata updates in this repository

Excluded:

- migrating other repositories under `apps/`
- renaming the repository away from `ato-cli`
- rotating package names, crate names, or binary names

## Known Blockers

Resolve these before the transfer window:

1. `CODEOWNERS` still points at the personal account.
   Current file: `.github/CODEOWNERS`
   Action: replace `@Koh0920` with a real org team slug such as `@ato-run/maintainers` after that team exists.

2. GitHub Actions secrets must exist in the org-owned repository.
   Required at minimum:
   - `RELEASE_PLZ_TOKEN`
   - any release-signing secrets used outside the checked-in workflow defaults

3. Required GitHub Apps and integrations must be installed for the organization.
   Check:
   - `release-plz`
   - secret scanning integrations
   - DeepWiki or any external documentation indexers

4. Branch protection must be recreated or verified after transfer.
   Check:
   - required checks on `dev` and `main`
   - who can push tags
   - whether administrator bypass is still enabled
   - whether merge commits are allowed on `main`

## Pre-Transfer Steps

1. Create the destination organization and maintainer team structure.
2. Decide the final CODEOWNERS target team slug.
3. Copy repository secrets and variables into the destination repository settings.
4. Verify the org can install and authorize required GitHub Apps.
5. Record the current required checks on `dev` and `main`.
6. Confirm the destination repository name will remain `ato-cli`.
7. Confirm release tags should continue to be pushed from `main`.

## Repository Changes To Land Before Transfer

These are safe to update in-repo before the transfer:

1. `Cargo.toml` repository metadata
   - set `repository = "https://github.com/ato-run/ato-cli"`

2. `README.md` public badges and links
   - CI badge
   - release badge
   - issues badge
   - last-commit badge
   - DeepWiki link

3. Documentation references that explicitly mention `Koh0920/ato-cli`
   - changelog links are not blockers because GitHub redirects, but they should be cleaned up opportunistically

## Transfer Procedure

1. Freeze non-essential merges during the transfer window.
2. Confirm `dev` and `main` are green before transfer.
3. Transfer the repository from `Koh0920/ato-cli` to `ato-run/ato-cli` in GitHub settings.
4. Update local remotes:

```bash
git remote set-url origin git@github.com:ato-run/ato-cli.git
```

5. Re-open repository settings in the org and verify:
   - Actions enabled
   - secrets present
   - branch protection present
   - GitHub Apps installed

## Immediate Post-Transfer Validation

Run these checks in order:

1. Push a no-op branch or use `workflow_dispatch` to run `Build (Multi OS)`.
2. Run `Security Audit`.
3. Run `V3 Parity Matrix`.
4. Push or refresh `main` to ensure `Release PR` still opens or updates correctly.
5. Verify GitHub Release pages are accessible under the new org path.

## Post-Transfer Cleanup

1. Update `.github/CODEOWNERS` to the final org team slug.
2. Update any remaining user-facing docs that still mention `Koh0920/ato-cli`.
3. Update helper scripts, release notes templates, and dashboards that pin the old owner.
4. Announce the new clone URL to contributors.

## Rollback Criteria

Treat the migration as incomplete and pause release activity if any of the following fails:

- `Build (Multi OS)` is not green on the new org repo
- `Security Audit` loses access to its required tooling or settings
- `Release PR` cannot create or update PRs on `main`
- tag-triggered release publishing fails under the new org

## Current Repository-Specific Notes

- `Cargo.toml` still contains repository metadata and should point to the new org.
- `README.md` contains owner-specific badges and links.
- `.github/CODEOWNERS` is a blocker until an org team slug is decided.
- Most `Koh0920` strings inside tests are fixtures, not migration blockers.
