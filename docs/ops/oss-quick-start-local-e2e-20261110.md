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

## Linux x86_64 run (ubuntu-sugamo)

- Host: `ubuntu-sugamo`, `x86_64`, Rust 1.96.0 installed with rustup (the
  first-time developer step). Source built with
  `cargo build --locked -p ato-cli` (debug), samples packed on the host with
  the built CLI, then the acceptance script ran unchanged.
- Result: **PASS**
  - open **4,195 ms**
  - resume **4,139 ms**
  - total **8,334 ms** (excluding sample acquisition and install)
- The install step here was the Rust toolchain + source build, not the published
  release installer; the release install path (H1) is verified separately and
  the 90-second acceptance still needs the packaged release on this host.
- A cross-host continuation (checkpoint taken on macOS, resumed on Linux) has
  not been run yet.

## Cross-host continuation: macOS → Linux x86_64

The checkpoint taken on macOS (three panes, a created task, split
`[0.4, 0.3, 0.3]`) and the same digest-pinned capsules were copied to
`ubuntu-sugamo`; there the workspace resumed with:

```sh
ato workspace open ws3.toml --checkpoint ws3-ck.toml \
  --local-data --local-data-port 18861 --no-open
```

- the calendar bridge returned the task created on macOS:
  `{task_id:"task-op-1", title:"3-app", due_at:"2026-12-25"}`;
- the shell carried the saved split (`0.4,0.3,0.3`) and three panes;
- the checkpoint's Resource documents and pane snapshots were read from the
  transferred `.state` directory with their digests verified.

This is the **same-user continuation** path: the receiving host imports the
DataResource documents into its own local Coordinator. Writer-authority
hand-over between two live Coordinators (D5) is not exercised by this run.

## Stable timing (median of three runs)

Single runs vary with machine load (4.1 s to 13.4 s observed for the same
work). `scripts/oss-quick-start/acceptance-median.sh --runs 3` runs the whole
acceptance several times and reports the median:

```text
run 1: open 4198ms resume 4173ms
run 2: open 4106ms resume 4002ms
run 3: open 3357ms resume 3732ms

PASS
  open median:   4106ms
  resume median: 4002ms
```

So the workspace lifecycle costs about **4 seconds** per phase on macOS arm64
(debug binary, prebuilt capsules); the earlier 13 s observation was one loaded
run, not a regression.

## Negative paths

`scripts/oss-quick-start/failure-paths.sh --ato … --capsules …` checks two
failures that must stay explicit:

1. **A checkpoint that cannot be written** (regular file where the checkpoint
   directory would be): after pressing Stop the workspace **keeps running**, and
   `/status` reports `error: …` so the page can offer a retry instead of
   pretending success.
2. **A busy local-data port**: the second `ato workspace open` fails with
   `port <n> is already in use; pass --local-data-port <port>`.

Both pass on macOS arm64.

## Linux x86_64: median timing and negative paths (2026-11-10, later run)

Repeated on `ubuntu-sugamo` with the current branch binary:

```text
acceptance-median --runs 3:
run 3: open 4125ms resume 4138ms
PASS
  open median:   4125ms (4124 4181 4125)
  resume median: 4138ms (4132 4215 4138)
```

`failure-paths.sh` also passes there:

- an unwritable checkpoint keeps the workspace running and reports
  `error: failed to create the checkpoint state directory …`;
- a busy local-data port fails with
  `error: port 18910 is already in use; pass --local-data-port <port>`.

## Walkthrough-as-written (bundle from the sample workflow)

The prebuilt bundle was built with the ato-samples
`tools/build-quickstart-bundle.sh` (operation-scoped sample sources) and the
quick-start steps were executed literally:

```text
ato-oss-quick-start-dev.tar.gz: OK          # shasum -a 256 -c
ato-oss-quick-start-dev/                    # tar extracts into a named directory
  README.md  workspace.toml  todo.capsule  calendar.capsule  excalidraw.capsule
shell=http://127.0.0.1:… iframes=3           # ato workspace open --local-data
CK_OK                                        # Stop and save wrote the checkpoint
```

The first attempt exposed a documentation/bundle mismatch (the tarball used to
extract its files at the top level, while the guide says
`cd ato-oss-quick-start-<version>`); the builder now packages a top-level
directory so the written steps work unchanged.

## Bundle built and verified on Linux x86_64

The same `tools/build-quickstart-bundle.sh` ran on `ubuntu-sugamo` (Linux-built
CLI, Linux sources for the samples and the Excalidraw dist) and produced
`ato-oss-quick-start-dev.tar.gz` (3.96 MB) plus its `.sha256`. Extracting it and
running the acceptance check on the same host:

```text
PASS
  open:   4122ms
  resume: 4138ms
```

So the distributable is not macOS-specific and the written steps work on a
second OS. (The builder also learned to run outside a git checkout, which is how
release CI may invoke it.)

## Capsule bytes are identical across macOS and Linux builds

Building the quick-start bundle on macOS arm64 and on Linux x86_64 from the same
sample sources produced **byte-identical capsules**:

| capsule | SHA-256 (both hosts) |
|---|---|
| `todo.capsule` | `d658283878f54096d0e9297927d4e1e331cb6f2c458e9371992224c658d3ed5e` |
| `calendar.capsule` | `6e8ba3d5d7387fc198f2e2da6cccb14e5067d5c5307a875c22e65422f4b30bd6` |
| `excalidraw.capsule` | `c42477d4e4bcbc252e00794e8fac77f1e5a36ca2c3e4181e596d56280557eb30` |

The tar wrapper differs (owner names carry the build host), but the capsule
payloads — the digests `workspace.toml` pins — are stable. That matters for
cross-host continuation: a bundle built on CI can be resumed on another host
without re-pinning.
