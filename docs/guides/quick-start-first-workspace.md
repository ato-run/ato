# Quick start: your first workspace

This is the "pick up your work on another computer" walkthrough: install the
`ato` CLI, open a three-app workspace (whiteboard, task board, calendar), edit,
checkpoint, stop, and resume with the data and the layout intact. You do not
build Rust, and you do not need an ato.run account.

The same flow is shown in the
[74-second demo](https://github.com/ato-run/ato-samples/releases/download/v0.9.1/ato-quick-start-demo-v0.9.1.mp4)
(no audio, English captions), recorded with the published `ato 0.9.1` and the
v0.9.1 bundle.

## 1. Install the CLI

- macOS/Linux (Homebrew): `brew install ato-run/ato/ato`
- Linux/macOS without Homebrew:
  `curl -fsSL https://github.com/ato-run/ato/releases/download/v0.9.1/ato-cli-installer.sh | sh`
- Windows x86_64: run the
  [`ato-cli-x86_64-pc-windows-msvc.msi`](https://github.com/ato-run/ato/releases/download/v0.9.1/ato-cli-x86_64-pc-windows-msvc.msi)
  release asset

`ato --version` should print `0.9.1`. The CLI is the only thing you install.
The output of the installer and MSI paths is verified on Linux x86_64 and
Windows x86_64; Homebrew uses the same release artifacts.

## 2. Get the sample workspace

The release carries a prebuilt bundle (three capsules plus a `workspace.toml`
that pins their SHA-256 digests); no `npm`, no `cargo`, no sample source:

```sh
curl -LO https://github.com/ato-run/ato-samples/releases/download/v0.9.1/ato-oss-quick-start-v0.9.1.tar.gz
curl -LO https://github.com/ato-run/ato-samples/releases/download/v0.9.1/ato-oss-quick-start-v0.9.1.tar.gz.sha256
shasum -a 256 -c ato-oss-quick-start-v0.9.1.tar.gz.sha256
tar xzf ato-oss-quick-start-v0.9.1.tar.gz
cd ato-oss-quick-start-v0.9.1
```

The bundle is built by `.github/workflows/quickstart-bundle.yml` in
`ato-run/ato-samples`; it contains `bundle.json` (schema, version, capsule
names) and is only published after that workflow opened it with the matching
`ato` release. The v0.9.1 bundle was built and checked by that workflow against
the v0.9.1 CLI. Source builds of the samples are documented in the
[sample repository](https://github.com/ato-run/ato-samples).

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
`.state/` directory) to the other host and resume there. The capsules are
byte-identical across hosts, so a bundle built elsewhere does not need new
digests; a checkpoint written on another machine resumes the same task and
layout. For live continuation between two machines, point both at one
Coordinator:

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
