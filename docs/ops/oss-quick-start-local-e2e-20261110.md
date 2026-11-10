# OSS quick start: local 3-app workspace acceptance (2026-11-10)

Status: **local verification only**. The CLI install and the Windows/Linux hosts
are still pending (H1 CI, T1), so this records what the workspace lifecycle
already does with the development binary on macOS.

## Setup

- CLI: `ato` built from the workspace-runtime branch (workspace open,
  `--local-data`, checkpoint, in-process Coordinator).
- Samples: TODO and Calendar from the operation-scoped sample work (their
  `ato.data.toml` / `ato.operations.yaml`), Excalidraw from the sample's Vite
  `dist` build; each packed with `ato pack` from `ato.capsule/2` authoring into
  a digest-pinned `.capsule`.
- Workspace: `ato.workspace/1` with three panes (`todo`, `calendar`,
  `excalidraw`), horizontal split `[0.4, 0.3, 0.3]`.
- Command: `ato workspace open workspace.toml --local-data
  --local-data-port 18800 --no-open --checkpoint-out checkpoint.toml`.

## Results

| Step | Result |
|---|---|
| shell HTTP 200 (fresh, 3 panes) | **3.36 s** |
| iframes in the shell | 3 |
| TODO bridge (instance `todo`) | binding `tasks` / `read_write` |
| Calendar bridge (instance `calendar`) | binding `tasks` / **`operations`** / `["todo_list_due_dates"]` |
| Excalidraw bridge (instance `excalidraw`) | binding `scene` / `read_write` |
| `todo_create` through the TODO bridge | `revision 1`, task `#001` created |
| `todo_list_due_dates` through Calendar | `{task_id:"task-op-1", title:"3-app", due_at:"2026-12-25"}` (projection only) |
| `scene_load` through Excalidraw | `{elements: [], appState: {}, files: {}}` |
| Ctrl-C checkpoint | written (Resource export + view state + pane snapshots) |

Resume (`--checkpoint checkpoint.toml`):

| Step | Result |
|---|---|
| shell HTTP 200 (resume, 3 panes) | **3.20 s** |
| iframes / saved split | 3 / `[0.4, 0.3, 0.3]` embedded |
| `todo_list_due_dates` after resume | the same task (`task-op-1`, due 2026-12-25) |
| `scene_load` after resume | empty scene document |

The 90-second basic acceptance excludes these measured steps being a problem:
the workspace lifecycle is a few seconds; what remains unmeasured is the CLI
install, the sample acquisition, and the real-host runs (T1).

## Not covered here

- CLI install channels (H1) and the packaged sample release.
- Windows x86_64 and Linux x86_64 hosts, and cross-host continuation (D4/D5).
- A real browser session: bridge calls were made with HTTP requests, not by
  clicking the UI (the shell and bridge contract are the same).

## Acceptance script

`scripts/oss-quick-start/acceptance.sh` turns the steps above into one check:

```sh
scripts/oss-quick-start/acceptance.sh \
  --ato path/to/ato --capsules path/to/capsule-dir --port 18846
# optionally --install-cmd "<per-host install command>"
```

It builds the digest-pinned workspace, opens it with `--local-data`, asserts the
three panes, the per-instance bindings and the operation results, checkpoints,
resumes, asserts the restored data and split, and prints the phase times.

Run on this macOS host (debug binary, three packed samples):

```text
PASS
  install: 0ms      (not included; --install-cmd records it)
  open:    4014ms
  resume:  3753ms
  total:   7767ms (excluding sample acquisition)
```

Both phases are far inside the 90-second basic budget; the remaining budget
question is the one-time install and sample acquisition on each host, which the
script reports separately via `--install-cmd`.
