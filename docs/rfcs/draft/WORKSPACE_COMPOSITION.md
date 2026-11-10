# Workspace Composition — multiple apps as one continuing work surface

Status: Draft (2026-11-10; first implementation plan for the OSS quick start)
Owner: CLI / Portable application runtime / Desktop
Related: [K_D_MODEL](K_D_MODEL.md),
[PORTABLE_DYNAMIC_DERIVATIONS](PORTABLE_DYNAMIC_DERIVATIONS.md),
[PORTABLE_LOCAL_INSTANCE](PORTABLE_LOCAL_INSTANCE.md),
[COMPUTATION_ARCHITECTURE](../accepted/COMPUTATION_ARCHITECTURE.md),
[COMPOSITION](../accepted/COMPOSITION.md)

## Purpose

The OSS quick start opens **one work surface**: an Excalidraw whiteboard, a
TODO board, and a Calendar derived from its due dates, arranged in a resizable
split. The person edits, checkpoints, stops, and resumes, and the data and the
view state come back.

This document defines how that becomes a product feature without adding a new
semantic root. It is deliberately the smallest mapping onto the existing model:

- each app remains **its own Capsule** (its own `ContractRef`, its own D, its
  own `Run`/`Receipt`);
- the workspace is **orchestration + presentation**, not a new identity domain;
- state continuity reuses `InstanceSnapshotV1` and the local Instance
  lifecycle; it does not fold workspace state into any Capsule's K;
- nothing about the workspace is written into `ContractRef` or
  `DerivationRef`.

## Non-goals

- No "workspace" Capsule identity, no workspace-level `ContractRef`.
- No cross-capsule composition of Computations inside one K. The apps are
  separate computations that a person placed together; they may share a
  **DataResource** through declared Ports (TODO → Calendar), which is the
  existing data contract, not a semantic composition.
- No server-side workspace state. The workspace shell, the per-app local
  Instances and their snapshots are local orchestration.
- No silent D selection, no new D generation, no emulation.

## Mapping

| Workspace concept | Existing mechanism |
|---|---|
| an app (Excalidraw / TODO / Calendar) | one portable application Capsule + its selected D |
| app surface inside the split | the Capsule's declared Static Web Surface (`ato.http@1` Port + entry) |
| workspace layout | local shell configuration: panes, order, split ratio |
| data sharing (due dates) | DataResource Port + `ato.operations/v2` (already implemented; see `data-resource-operation-scope`) |
| Checkpoint | per-app `InstanceSnapshotV1` capture, committed as one workspace checkpoint set |
| Stop | stop every owned Run/Instance in reverse order; no orphan process/Resource |
| Resume | start each Instance with its saved snapshot, then restore the shell view state |
| receipt | one receipt per app Run (existing schema); the workspace checkpoint records the set of receipt ids/digests, not a new verifier |

The workspace is therefore a **local orchestration artifact**, addressed by its
own file, that refers to app capsules by content digest. It is not transport
and not identity.

## Workspace file (v0)

```toml
schema = "ato.workspace/1"

[layout]
direction = "horizontal"          # horizontal | vertical
split = [0.55, 0.25, 0.20]        # pane weights; order is `pane`

[[pane]]
id = "board"
app = "excalidraw"
capsule = "capsules/excalidraw.capsule"   # digest-pinned by the workspace loader
# optional per-pane view state hint, e.g. restore= "scene"
[[pane]]
id = "todo"
app = "todo"
capsule = "capsules/todo.capsule"
[[pane]]
id = "calendar"
app = "calendar"
capsule = "capsules/calendar.capsule"
```

`ato.workspace/1` is **not** a Capsule profile: `ato workspace` loads each
`capsule`, validates it independently (all-D validation, then selected-D
admission), imports each as a local Instance (or runs it ephemerally), and
serves a workspace shell that embeds each app's local surface.

## Runtime behaviour

- **Serve** (local): one shell origin (`127.0.0.1:<port>`), one path prefix per
  pane (`/apps/<pane>/`). Each app keeps its own origin-scoped state
  (`localStorage`, cookies) because path prefixes share an origin; the shell
  never reads an app's document.
- **Checkpoint**: quiesce only the panes that declare writable state, capture
  each pane's `InstanceSnapshotV1` and the shell view state (open pane, split
  ratio, scroll/selection where declared), then commit the checkpoint set.
  A single failure aborts the whole checkpoint: previous checkpoints and live
  state stay untouched, and the panes resume serving.
- **Stop**: stop panes in reverse order, prove each Run's process group is
  gone, release ports; a pane that cannot be stopped is reported and its state
  is retained for recovery.
- **Resume**: start each pane from its snapshot, verify the restored digest
  before reporting success, then restore the shell view state. A pane whose
  snapshot cannot be restored fails the resume; the workspace does not silently
  start a fresh app in its place.
- **Receipts**: every pane run writes the ordinary Contract verification
  receipt. `fully_satisfied` per pane is decided by the common verifier from
  that pane's observations; the shell does not aggregate it into a new K.
- **View state**: only declared, restorable state is saved — layout, open pane,
  scroll/zoom/selection where the app declares it. Process memory is not
  restored, and the UI says so.

## Cross-host continuation (relationship to D4/D5)

The workspace file and the per-app capsules are separately distributable. A
cross-host move exports the app Resources (D4) and transfers writer authority
(D5) for the shared DataResource; the workspace file itself is content-addressed
transport for the local shell. The receiving host selects known Ds per app and
runs them locally; the workspace does not create a new D or a new identity.

## Acceptance (proposal)

1. `ato workspace open <workspace.toml>` with three static apps shows one
   browser page with three panes; no Rust toolchain and no ato.run account.
2. Edit each app, Checkpoint, Stop, Resume: the same data and the declared view
   state come back; a failed pane aborts the checkpoint/resume with a named
   reason.
3. TODO writes, Calendar reads only `todo_list_due_dates`; a document read from
   Calendar is refused; revocation stops the projection.
4. No orphan process or Resource after stop/crash; receipts name each pane's
   Run.
5. 90-second basic acceptance is measured on the packaged sample (D7/T1), not
   on a source build.

## Undecided (needs review)

- Shell implementation surface: local Coordinator-served HTML with iframes vs
  the Desktop shell. The v0 CLI path should not require the Desktop product.
- Whether `ato workspace open` imports durable Instances by default or runs
  ephemerally and only persists on Checkpoint (leaning: durable Instance per
  pane after the first Checkpoint).
- Workspace checkpoint storage format and whether the checkpoint set is one
  file or a directory of `InstanceSnapshotV1` plus a view-state record.
- Whether the workspace file pins capsules by `ContractRef` in addition to the
  transport SHA-256 (identity vs transport evidence).

## v0 implementation notes (2026-11-10)

The first increment exists in `apps/cli/src/workspace.rs` (PR #1740). It keeps
the boundary above and makes these concrete choices, which the next increment
must either keep or explicitly revise:

- **Panes are separate origins.** Each pane's verified tree is served by its own
  loopback static server on its own port; the shell is another origin with one
  iframe per pane. This preserves per-app `localStorage` isolation without
  inventing path-scoped storage. A same-origin path-prefix layout remains
  possible later, but it would need a storage partition rule.
- **The shell is local orchestration.** `ato.workspace/1` names panes, app
  labels, digest-pinned capsules, an optional `derivation` and an optional
  restore hint. `ato workspace inspect` validates every declared D and the
  selected D; `ato workspace open` starts the pane servers and the shell. No
  ContractRef is created or changed by the workspace.
- **Checkpoint v0 captures browser state, not process state.** `capture()` reads
  each pane's served `localStorage` map (through the static server's
  state-bridge token) plus the shell's own view state (direction, split, active
  pane), computes a `sha256` over canonicalised state, and records it as
  `snapshot_ref`. Resume verifies the workspace file digest and injects the
  saved state back into the pane servers. `instance_id` is `ephemeral-<pane>`
  until durable Instances exist.
- **Stop is Ctrl-C.** The CLI process owns the servers; dropping them closes
  the listeners. `--checkpoint-out` writes the checkpoint atomically
  (same-directory temp file, fsync, rename). On platforms where the signal flag
  cannot be registered the stop signal is a line on stdin.
- **View state is only what the shell declares.** Split ratio, direction and
  active pane are restorable. Scroll/selection inside an app is not captured:
  it is cross-origin and needs an app-declared, restorable state, which is the
  same rule as D3's state compatibility.

Open questions this increment does **not** settle: how a workspace checkpoint
migrates from captured browser state to `InstanceSnapshotV1`; how the shell
learns each pane's DataResource bindings; and how a stop that cannot finish
keeps the pane's state recoverable. Those are tracked by D3/D5.

## Coordinator multi-app serving (design note for the next increment)

The v0 shell serves pane trees with the loopback static server, which has the
browser-state bridge but **not** the Data Ports bridge. To make TODO/Calendar
share a Resource, panes must be served by the Coordinator, which owns the
Resource database and the instance-scoped bindings.

Proposed shape, keeping the existing single-app mode intact:

- `ato-coordinator serve` accepts repeatable
  `--workspace-app <prefix>=<dir>@<instance>[:<port>]` entries. One Coordinator,
  one database, one origin; each app keeps its own prefix.
- Requests are routed by prefix: `/apps/<prefix>/…` serves that app's files and
  its bridges. The injected scripts are prefix-relative — the bridge derives its
  base from its own `<script src>` — so `atoDataPorts` and
  `atoInstanceOperations` keep their hosted shapes while resolving to
  `/apps/<prefix>/__ato/data-ports`.
- `/apps/<prefix>/__ato/data-ports` returns `bindings_for_instance(instance)`
  and the invoke path uses the same instance. The prefix, not a global, is what
  selects the app; no app-name branching is added.
- The shell keeps its own origin? No: for Data Ports to work, panes must share
  the Coordinator origin. The shell becomes a Coordinator-served page at
  `/apps/<prefix>/` with iframes into the other prefixes; `localStorage` is then
  shared per origin, so the shell namespaces its view state under
  `ato.workspace.view` and panes continue to use their own keys (TODO and
  Excalidraw already do). If two panes ever collide, the fallback is per-pane
  iframe origins with a bridge origin, which needs a storage-partition rule.
- Bootstrap then becomes: for each pane, register its `ato.operations.yaml`,
  ensure one Resource for the owner Port (idempotent by protocol + display
  name), bind every pane's Port to it with `instance_id = pane id`, then start
  the Coordinator with all panes.

Open question for review: whether the shell should be a Coordinator-served page
or the CLI should keep a separate shell origin and inject each pane's base URL.

## Bootstrap dependency (2026-11-10)

The workspace data bootstrap needs the declaration compiler that is being added
in the selfhost registration increment (`ato data protocol register-all`, PR
#1742): parse `ato.operations/v2`, register each module-backed operation and
reuse the same idempotency rule. Implementing a second parser inside the
workspace command would create two sources of truth for the declaration, so the
bootstrap is stacked on that branch and reuses it.

Bootstrap sequence per `ato workspace open --coordinator <url>`:

1. materialize each pane's verified tree (already required for serving);
2. read the pane's `ato.data.toml` and `ato.operations.yaml` from that tree;
3. for the pane that owns a `read_write` Port, register the declared operations
   (idempotent by module digest) and ensure one Resource for the protocol
   (idempotent by protocol + display name);
4. bind every pane's operation-only Port to that Resource with
   `instance_id = "<pane id>"`, using the Port's declared operation list;
5. start the Coordinator in workspace mode (`--workspace-app`) if the caller did
   not already point at one, and serve the shell from it.

Steps 3–4 must be safe to re-run: a second open reports `unchanged` rather than
duplicating Resources or bindings. A failure in any step stops the open before
the shell is exposed.
