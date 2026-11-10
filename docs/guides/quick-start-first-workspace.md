# Quick start: your first workspace

This is the "pick up your work on another computer" walkthrough: install the
`ato` CLI, open a three-app workspace (whiteboard, task board, calendar), edit,
checkpoint, stop, and resume with the data and the layout intact. You do not
build Rust, and you do not need an ato.run account.

## 1. Install the CLI

- macOS (Homebrew): `brew install ato-run/homebrew-ato/ato-cli`
- Windows: run the `ato-cli-x86_64-pc-windows-msvc.msi` release asset
- Linux/macOS (shell): run the `ato-cli-installer.sh` release asset

`ato --version` should print a version. The CLI is the only thing you install.

## 2. Get the sample workspace

Download the three sample capsules (`todo.capsule`, `calendar.capsule`,
`excalidraw.capsule`) and a `workspace.toml` that pins their SHA-256 digests.
The capsules are prebuilt: no `npm`, no `cargo`, no sample source.

## 3. Open it

```sh
ato workspace open workspace.toml --local-data --checkpoint-out workspace.checkpoint.toml
```

The CLI starts a local Coordinator, serves the three apps side by side, and
opens the shell in your browser. The apps share data through declared
operations - the calendar only receives due dates, never the board document.

## 4. Edit, checkpoint, stop

Edit in the panes, then press **Ctrl-C**. With `--checkpoint-out`, the CLI
writes a checkpoint containing the saved DataResource documents, the pane
browser state and the split/active-pane layout, then stops the Coordinator.

## 5. Resume

```sh
ato workspace open workspace.toml --checkpoint workspace.checkpoint.toml \
  --local-data --checkpoint-out workspace.checkpoint.toml
```

The same data and the same layout come back. A pane that cannot be restored
fails the resume instead of silently starting empty.

## Continue on another computer

Copy the capsules, `workspace.toml` and the checkpoint (including its
`.state/` directory) to the other host and resume there. For live continuation
between two machines, point both at one Coordinator:

```sh
ato workspace open workspace.toml --coordinator http://<coordinator>:8788 --token <token>
```

The Coordinator owns a single-writer lease: a second live writer is refused
(`workspace_lease_busy`), and a hand-over increments the lease epoch.

## Current limitations

- The shell has no Checkpoint button yet; Ctrl-C is the stop/checkpoint action.
- Window/split layout is restored; in-pane scroll or selection is not (it needs
  an app-declared state).
- Windows Ctrl-C is handled through the console handler; the shell Stop button
  works there as well.
