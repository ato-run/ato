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

The release carries a prebuilt bundle (three capsules plus a `workspace.toml`
that pins their SHA-256 digests); no `npm`, no `cargo`, no sample source:

```sh
curl -LO https://github.com/ato-run/ato-samples/releases/download/<tag>/ato-oss-quick-start-<version>.tar.gz
curl -LO https://github.com/ato-run/ato-samples/releases/download/<tag>/ato-oss-quick-start-<version>.tar.gz.sha256
shasum -a 256 -c ato-oss-quick-start-<version>.tar.gz.sha256
tar xzf ato-oss-quick-start-<version>.tar.gz
cd ato-oss-quick-start-<version>
```

The bundle is built by `.github/workflows/quickstart-bundle.yml` in
`ato-run/ato-samples`; until that release is published, use the capsules from a
local build of the sample sources.

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

- The shell has **Checkpoint** (save while it keeps running) and **Stop and
  save** buttons; **Ctrl-S / Cmd-S** also requests a checkpoint, and Ctrl-C is
  the same stop action from the terminal.
- Window/split layout and the active pane are restored; in-pane scroll or
  selection is not (it needs an app-declared state).
- The local Coordinator takes the first free port from `18788`; pass
  `--local-data-port <port>` to pin one (a pinned busy port is named in the
  error).
- If a checkpoint cannot be written, the workspace stays open and the button
  shows **Failed**; fix the path/permissions and press it again. Nothing is
  silently discarded.
- Windows Ctrl-C is handled through the console handler; the shell buttons work
  there as well.
