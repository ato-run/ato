# Workspace W2c — one Task Board instance, two Resources (2026-11-10)

Status: the Personal / Ato Team switch works on a real Coordinator. The combined
"All" view and the hosted contract for the team Port are not done.

## What was built (ato-samples #14, stacked on the Task Board views branch)

- `ato.data.toml` is `ato.data-ports/2` with `tasks` (user scope) and
  `team_tasks` (instance scope, optional).
- The UI drives one Port at a time; the Board selector switches between
  **Personal** and **Ato Team** and remembers the choice locally.
- The team option is disabled and labelled `Ato Team（未接続）` when the
  instance has no binding for it.
- `e2e/board-team-context.cjs` runs the scenario against a real Coordinator.

## Evidence

```
personal visible: true | team visible: false
selector options: [ 'Personal', 'Ato Team' ]
after switch -> team visible: true | personal visible: false
page errors: none
PASS: one Task Board instance switched from the personal to the team Resource
```

Screenshot: `.tmp/board-team-context.png`.

## Not done

- The **All** view (merged display of personal + team) — must be read-only over
  the two observations and must not create a third document.
- Hosted runtime needs the team Port's operation set in the manifest
  (e.g. `todo_team_*`); the selfhost bridge addressed the Port directly, which
  is not the hosted contract yet.
- Calendar's `todo_list_due_dates` connection from the Workspace is still open.
- Nothing was deployed: staging is blocked on Cloudflare credentials.
