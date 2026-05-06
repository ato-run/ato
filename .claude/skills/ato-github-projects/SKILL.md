---
name: ato-github-projects
description: "Manage ato-run/ato work in GitHub Projects using gh project with a minimal operating model. Covers auth, project bootstrap, field setup, issue or PR intake, triage queries, release audits, and the handoff point to built-in workflows or gh api graphql when gh project stops being the right tool."
argument-hint: "Describe the GitHub Projects task to perform for ato-run/ato, such as bootstrap the roadmap project, add issues, audit a release target, or update an item's fields"
user-invocable: true
---

# Ato GitHub Projects

## What This Skill Does

This skill standardises how `ato-run/ato` uses GitHub Projects through `gh project`.

It covers:

- checking and refreshing the required GitHub auth scopes
- creating or locating the organization-level roadmap project
- creating the minimum custom fields needed for planning
- keeping milestone usage separate from project fields and using milestones only for release buckets
- linking the project to `ato-run/ato` when it should appear in the repository's Projects tab
- adding issues and pull requests into the project
- running triage and release-readiness queries from the CLI
- deciding when to stay in `gh project`, when to switch to the GitHub UI, and when to escalate to `gh api graphql`

It is intentionally narrow. The goal is to keep the project current with low ceremony, not to rebuild Jira in shell scripts.

## When To Use

Use this skill when the user asks to:

- create or initialize the `ato-run` GitHub Project
- manage roadmap work for `ato-run/ato`
- add issues or PRs to the roadmap
- audit upcoming release work from GitHub Projects
- automate simple GitHub Projects maintenance with `gh`
- inspect project fields, items, or filtered item lists

Do not use this skill for:

- detailed board or roadmap view layout design
- complex bulk field synchronization across many items
- custom automations that require option IDs, field IDs, or mutations beyond `gh project`

For those cases, use the GitHub UI first, then `gh api graphql` only for the specific missing operation.

## Default Operating Model For ato-run/ato

Unless the user says otherwise, assume this model:

- owner: `ato-run`
- repository: `ato-run/ato`
- one organization project: `Ato Roadmap`
- GitHub UI owns views, layout, roadmap display, and charts
- `gh project` owns bootstrap, item intake, list queries, and light audits
- built-in project workflows or GitHub Actions own routine automation

Default custom fields:

- `Area`: `CLI`, `Desktop`, `Nacelle`, `Capsule Core`, `Docs`, `Website`, `Registry`, `Release`
- `Work type`: `Bug`, `Feature`, `DX`, `Safety`, `RFC`, `Chore`
- `Priority`: `P0`, `P1`, `P2`, `P3`
- `Target`: `v0.5.x`, `v0.6.x`, `Later`
- `User signal`: `None`, `1 user`, `2-5 users`, `Many`
- `Demo blocker`: `Yes`, `No`

Milestones are separate from Projects. Treat them as repository-level release buckets, for example:

- `v0.5.0`
- `v0.6.0`
- `Show HN Launch`

Use `Target` for rough planning horizons inside Projects, and use milestones only for work that is actually committed to a specific release or launch.

Treat `Status` as the main workflow field in the UI, with values like `Inbox`, `Triaging`, `Planned`, `In Progress`, `Blocked`, `Done`, and `Deferred`.

## Preflight

1. Confirm `gh` is authenticated.
2. Confirm the token has Projects access.
3. Prefer `project` scope when create or edit operations are in scope.
4. If the task is read-only, `read:project` is acceptable, but `project` avoids later re-auth churn.

Commands:

```bash
gh auth status
gh auth refresh -s project
```

If authentication is missing entirely:

```bash
gh auth login --scopes "project"
```

Do not continue with project mutations until scope is confirmed.

## Decision Rules

Choose the lightest tool that solves the task:

- Use `gh project list`, `view`, `create`, `field-create`, `item-add`, `item-list` for normal operations.
- Use repository milestones for release-scope tracking on issues and pull requests; do not try to model milestones as project custom fields.
- Use the GitHub UI for view layout, board columns, roadmap presentation, and charts.
- Use built-in project workflows or GitHub Actions for routine auto-add, archive, and status transitions.
- Use `gh api graphql` only when `gh project` cannot perform the required mutation cleanly.

Avoid large shell wrappers around `item-edit` unless the user explicitly wants that complexity. For normal items, `item-edit` requires `item ID`, `project ID`, and `field ID`, and it updates one field at a time.

## Milestones Vs Projects

Keep the boundary explicit:

- Milestone: repository-level release or launch bucket on issues and pull requests
- Project: status, priority, area, roadmap placement, and cross-cutting triage
- Label: lightweight classification tags

For `ato-run/ato`, the normal split is:

- milestone = `v0.5.0`, `v0.6.0`, `Show HN Launch`
- project `Target` = `v0.5.x`, `v0.6.x`, `Later`

If a task is not yet firmly committed to a release, keep it in the project with `Target` only. Do not assign a milestone just to fill the field.

## Bootstrap Flow

### Step 1: Locate Or Create The Project

List existing projects first:

```bash
gh project list --owner ato-run
```

If `Ato Roadmap` does not exist, create it:

```bash
gh project create --owner ato-run --title "Ato Roadmap"
```

View it after creation:

```bash
gh project view 1 --owner ato-run
gh project view 1 --owner ato-run --web
```

If the project number is unknown, resolve it from `gh project list` before moving on.

### Step 2: Create The Minimal Custom Fields

Create only the fields that support triage and release planning.

```bash
PROJECT=1
OWNER=ato-run

gh project field-create "$PROJECT" --owner "$OWNER" \
  --name "Area" \
  --data-type SINGLE_SELECT \
  --single-select-options "CLI,Desktop,Nacelle,Capsule Core,Docs,Website,Registry,Release"

gh project field-create "$PROJECT" --owner "$OWNER" \
  --name "Work type" \
  --data-type SINGLE_SELECT \
  --single-select-options "Bug,Feature,DX,Safety,RFC,Chore"

gh project field-create "$PROJECT" --owner "$OWNER" \
  --name "Priority" \
  --data-type SINGLE_SELECT \
  --single-select-options "P0,P1,P2,P3"

gh project field-create "$PROJECT" --owner "$OWNER" \
  --name "Target" \
  --data-type SINGLE_SELECT \
  --single-select-options "v0.5.x,v0.6.x,Later"

gh project field-create "$PROJECT" --owner "$OWNER" \
  --name "User signal" \
  --data-type SINGLE_SELECT \
  --single-select-options "None,1 user,2-5 users,Many"

gh project field-create "$PROJECT" --owner "$OWNER" \
  --name "Demo blocker" \
  --data-type SINGLE_SELECT \
  --single-select-options "Yes,No"
```

After creation, inspect the field list:

```bash
gh project field-list "$PROJECT" --owner "$OWNER"
```

If fields already exist, do not recreate them.

Avoid `Type` as a custom field name. GitHub Projects treats it as a reserved or conflicting name in practice.

### Step 3: Link The Project To The Repository When Needed

Creating an organization project does not automatically make it appear in the repository's `Projects` tab.

If the project should be visible from `https://github.com/ato-run/ato/projects`, do one of the following in the GitHub UI:

1. Open the repository `ato-run/ato`.
2. Go to `Projects`.
3. Click `Link a project` and select `Ato Roadmap`.

Or set `ato-run/ato` as the project's default repository from the project settings. GitHub documents that setting a default repository also makes the project appear in the repository's `Projects` tab.

Visibility still matters. Repository members can only see a linked project if they also have visibility to the project itself.

## Daily Intake Flow

Preferred flow:

1. Create or identify the issue or PR.
2. If the work is firmly committed to a release, set the repository milestone on the issue or PR.
3. Add it to the project.
4. Let UI workflows or lightweight manual triage assign `Status` and the minimum classification fields.

Issue milestone examples:

```bash
gh issue edit 123 --repo ato-run/ato --milestone "v0.5.0"
gh issue list --repo ato-run/ato --milestone "v0.5.0"
```

Milestones themselves are created at the repository layer, not with `gh project`. If needed, create them through GitHub UI or `gh api` against `repos/ato-run/ato/milestones`.

Example issue creation plus intake:

```bash
ISSUE_URL=$(gh issue create \
  --repo ato-run/ato \
  --title "Improve first-run error message for missing pnpm" \
  --body "Users should understand what Ato inferred and what failed." \
  --label "dx,cli" \
  --json url \
  --jq '.url')

gh project item-add 1 --owner ato-run --url "$ISSUE_URL"
```

If the repository already auto-adds matching issues to the project, prefer the automation over manual `item-add`.

## Triage And Audit Queries

List all project items:

```bash
gh project item-list 1 --owner ato-run
```

Inspect active bugs:

```bash
gh project item-list 1 \
  --owner ato-run \
  --query "label:bug -status:Done"
```

Inspect work assigned to the current user:

```bash
gh project item-list 1 \
  --owner ato-run \
  --query "assignee:@me is:issue is:open"
```

Inspect a release target before shipping:

```bash
gh project item-list 1 \
  --owner ato-run \
  --query "target:v0.5.x -status:Done"
```

For structured output, prefer JSON and `jq`:

```bash
gh project item-list 1 \
  --owner ato-run \
  --query "target:v0.5.x -status:Done" \
  --format json \
  --jq '.items[] | {title, status, priority, area, url: .content.url}'
```

## Updating Item Fields

Only use this path when the task truly requires field mutation from the CLI.

First gather the IDs you need:

```bash
OWNER=ato-run
PROJECT=1

gh project view "$PROJECT" --owner "$OWNER" --format json --jq '.id'
gh project field-list "$PROJECT" --owner "$OWNER" --format json --jq '.fields[] | {name, id, type}'
gh project item-list "$PROJECT" --owner "$OWNER" --format json
```

Then update exactly one field value per call:

```bash
gh project item-edit \
  --id <item-id> \
  --project-id <project-id> \
  --field-id <field-id> \
  --text "CLI"
```

If the user asks for repeated or bulk item updates, stop and evaluate whether `gh api graphql` or a purpose-built script is warranted.

## Automation Boundary

Keep this split unless the user explicitly wants a deeper automation layer:

- GitHub UI: views, board layout, roadmap, charts, human-oriented planning
- `gh project`: project creation, field creation, item add, item listing, focused audits
- built-in workflows or Actions: auto-add, auto-archive, status maintenance
- `gh api graphql`: missing fine-grained mutations or bulk operations

This is the main guardrail. Routine maintenance should be automated, but the model should stay simple enough that humans can still reason about it.

## Hard Rules

- Start with one organization project, not several overlapping projects.
- Keep the field set minimal and stable.
- Prefer queries and audits over large bespoke shell automations.
- Do not try to fully manage views or roadmaps through the CLI.
- Do not introduce GraphQL until a concrete `gh project` limitation is hit.
- Do not treat GitHub Projects like Jira if the team is not already operating that way.

## Completion Criteria

The task is complete only when the relevant checks pass for the requested scope:

- auth scope is correct for the intended operation
- the target project exists and is discoverable with `gh project list`
- required fields exist and are inspectable with `gh project field-list`
- issue or PR intake into the project works
- the requested audit or filtered query returns the expected slice of work
- if field mutation was required, the specific field change was verified after mutation

## Quick Reference

```bash
# auth
gh auth status
gh auth refresh -s project

# project discovery
gh project list --owner ato-run
gh project create --owner ato-run --title "Ato Roadmap"

# fields
gh project field-list 1 --owner ato-run

# intake
gh project item-add 1 --owner ato-run --url https://github.com/ato-run/ato/issues/123

# audits
gh project item-list 1 --owner ato-run --query "label:bug -status:Done"
gh project item-list 1 --owner ato-run --query "target:v0.5.x -status:Done"
```
