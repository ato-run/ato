# OSS quick start: local 3-app workspace acceptance (2026-11-10)

Status: **verified through the published releases**. This file began as the
macOS development-binary log; the later sections record the Linux and Windows
x86_64 runs, the v0.9.0/v0.9.1 release artifacts, the 74-second public demo and
the acceptance numbers used by the README. Where an early section says a host is
pending, the later section supersedes it; the raw logs are kept as written.

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

## Cross-host with independently built bundles (Linux → macOS)

The Linux-built quick-start bundle was opened on `ubuntu-sugamo`, a task was
created (`from linux bundle`, due 2027-03-03), and the workspace stopped,
writing `workspace.checkpoint.toml`. The whole directory (workspace.toml, the
three capsules, the checkpoint and its `.state/` objects) was copied to macOS
arm64 and resumed there:

- calendar bridge returned the Linux-created task;
- the shell had three panes and the saved split (`0.4`);
- no re-pinning was needed — the capsules are byte-identical across the two
  builds.

This is the "pick up your work on another computer" path with artifacts that
came from different hosts, not a copied build.

## Linux: latest branch re-check (2026-11-10, round 78)

After the last UI/port changes, the branch was synced to `ubuntu-sugamo`,
rebuilt, and re-run against the Linux bundle:

```text
acceptance-median --runs 2:
PASS
  open median:   4213ms (4229 4197)
  resume median: 4163ms (4190 4136)

failure-paths: PASS
  ok: still running, status: error: failed to create the checkpoint state directory …
  ok: error: port 18991 is already in use; pass --local-data-port <port>
```

The latest workspace code therefore still meets the acceptance and failure-path
behaviour on Linux x86_64.

## Windows x86_64 run (yuki-laptop, 2026-11-10)

Host: `yuki-laptop` (Windows 11 26200, x86_64, VS 2022 BuildTools, Rust
1.96.0), reached over SSH. The CLI was built from the branch source
(`cargo build -p ato-cli`, debug) after fixing one real Windows build error: the
shutdown timer's `Duration` import was `cfg(unix)`. The prebuilt Linux/macOS
bundle (byte-identical capsules) was used as the workspace.

Acceptance under Git Bash:

```text
PASS
  open:   16907ms
  resume: 11994ms

acceptance-median --runs 2:
PASS
  open median:   12398ms (12913 11883)
  resume median: 14883ms (16756 13010)
```

So the 3-OS matrix now has a Windows column: macOS PASS, Linux PASS, Windows
x86_64 PASS (debug build, not the MSI release).

Failure path checked manually with PowerShell because the bash script's
`kill -0` liveness check does not see native Windows processes: after pressing
Stop with an unwritable checkpoint the process **stayed alive**, `/status`
returned `error: failed to create the checkpoint state directory …`, and the log
printed the retry hint — the same behaviour as macOS/Linux.

## Windows x86_64 with the released v0.9.0 artifacts (2026-11-11)

After the v0.9.0 release published (CLI assets + Windows MSI) and the quick-start
bundle was attached to `ato-run/ato-samples` v0.9.0, the 90-second flow was run
on `yuki-laptop` with **installed release artifacts only**:

| Step | Measurement |
|---|---|
| MSI install (`msiexec /qn`) | **1,181 ms** |
| bundle download from the GitHub release (`Invoke-WebRequest`) | **11,152 ms** |
| SHA-256 verification (`Get-FileHash`) + extract (`tar xzf`) | **135 ms**, hash OK |
| `ato workspace open` → shell URL | **3,869 ms** |
| TODO edit through the bridge (creates the task) | 5,187 ms (cumulative) |
| Checkpoint requested → `/status` = `ok:` | 5,538 ms (cumulative) |
| Stop and save → checkpoint file present, process exited | **6,031 ms** total |

Machine-attributable total: **≈18.5 s** (install + download + extract + flow),
well inside the 90-second basic acceptance; the human editing time is not part
of the automated measurement (the edit was driven through the same data-port
operation the UI uses).

Acceptance harness with the installed `ato 0.9.0` and the downloaded bundle:

```text
PASS
  open:   3534ms
  resume: 3949ms
```

The released artifacts therefore carry the workspace commands; the earlier
source-build Windows run (12–17 s per phase) was a debug build and is superseded
by these release numbers.

## Published v0.9.0 artifact verification (2026-11-11)

- **macOS arm64 release binary** (`ato-cli-aarch64-apple-darwin.tar.xz`): `ato 0.9.0`, `ato workspace open` present; acceptance harness **PASS** (open 2,698 ms / resume 5,368 ms).
- **Linux x86_64 release binary** (`ato-cli-x86_64-unknown-linux-gnu.tar.xz`): `ato 0.9.0`, workspace present; acceptance **PASS** (open 593 ms / resume 622 ms).
- **Windows x86_64 MSI** release artifact: installed (1,181 ms), acceptance PASS (open 3,534 ms / resume 3,949 ms), 90-second flow ≈18.5 s machine-attributable.
- **Bundle CI gate**: `quick-start-bundle.yml` ran on `ato-samples` main with v0.9.0 available; the verification step that opens the built bundle with the released CLI passed. The CI-built bundle and the published Windows-built bundle have **identical capsule SHA-256s** (`2c1fafd1…`, `a47f8c9c…`, `c42477d4…`); only tar metadata differs.

## Demo recording environment (2026-11-11)

The public 74-second demo was recorded from the published artifacts, not from
a development build:

- **Host**: macOS arm64 (Darwin `15.8.1`), real Google Chrome
  driven by Playwright; the shell and its three iframes were served by the
  `ato workspace open --local-data` process.
- **CLI**: `ato 0.9.1` release binary
  (`ato-cli-aarch64-apple-darwin.tar.xz`, downloaded from the v0.9.1 release).
- **Bundle**: `ato-oss-quick-start-v0.9.1.tar.gz` from the `ato-samples` v0.9.1
  release, SHA-256 `8fe7e38656fd2ab345e56ee6ae73c59cb03fdf38304f57c87a7e0744951e84ab`.
- **Method**: two browser sessions — session 1 edits the whiteboard, adds a task
  with a due date, reloads the Calendar pane so it re-reads the projection,
  presses Checkpoint and Stop and saves the checkpoint; session 2 launches with
  `--checkpoint` and shows the restored task, calendar entries and split. Raw
  WebM was captured at 1440x900/30 fps and edited to 74 s by adding title,
  OS/closing cards and captions (no speed change, no synthetic app states).
- **Observed during recording**: the Calendar pane showed the added due date
  after its reload, and the resumed Task Board reported `同期準備完了` with the
  same two tasks; the restored split was `[0.4, 0.3, 0.3]`.
- **Artifact**: `ato-quick-start-demo-v0.9.1.mp4`
  (https://github.com/ato-run/ato-samples/releases/download/v0.9.1/ato-quick-start-demo-v0.9.1.mp4),
  SHA-256 `8fb85dfd52e587586ff522c5de3722dcbbd1d5b73fb4eedb51244738a575140e`,
  duration 74.3 s.
