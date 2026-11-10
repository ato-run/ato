# OSS quick start: Windows x86_64 T1 runbook (2026-11-10)

Status: **not executed**. No Windows runner is registered for this account yet;
this records exactly what to run when one is available, so the missing 3OS
column is a checklist rather than a rediscovery exercise.

## Host and install

1. Windows x86_64, PowerShell 7 (or Windows PowerShell 5).
2. Install the released MSI (`ato-cli-x86_64-pc-windows-msvc.msi`) from the
   matching release; open a new terminal so `PATH` picks up `ato`.
3. `ato --version` → record the version in the T1 table. If the release does
   not include the workspace commands (`ato workspace --help` fails), stop: the
   90-second acceptance needs a release that contains this work.

## Bundle

1. Download `ato-oss-quick-start-<version>.tar.gz` and its `.sha256`.
2. Verify in PowerShell:
   ```powershell
   (Get-FileHash .\ato-oss-quick-start-<version>.tar.gz -Algorithm SHA256).Hash.ToLower()
   ```
   Compare with the first field of the `.sha256` file.
3. Expand (`tar xzf` works on Windows 10+), then `cd ato-oss-quick-start-<version>`.

## Run and measure

Record a start timestamp (`Get-Date -Format o`), then:

```powershell
ato workspace open workspace.toml --local-data --checkpoint-out workspace.checkpoint.toml
```

- Note when the shell URL prints and when the browser shows the three panes.
- Edit all three panes; press **Checkpoint** (Ctrl-S) then **Stop and save**.
- Record the elapsed seconds (target: install + open + edit + checkpoint inside
  90 s for the basic acceptance; the per-phase host numbers go in the table).
- Resume: `ato workspace open workspace.toml --checkpoint workspace.checkpoint.toml --local-data`,
  confirm the calendar shows the task created in the TODO pane and the split is
  restored.

## Cross-host continuation

Copy the capsules, `workspace.toml` and `workspace.checkpoint.toml` (+ its
`.state/` directory) to a macOS/Linux host and resume there — the capsules are
byte-identical across hosts (recorded in
`oss-quick-start-local-e2e-20261110.md`). For live hand-over, point both hosts
at one Coordinator (`--coordinator http://<host>:<port> --token <token>`); the
second writer must be refused until the first stops (lease epoch increments).

## Known Windows risks to check on the first run

- **Ctrl-C path**: a console handler is registered (compiled against the Windows
  bindings, not yet run); verify that Ctrl-C writes the checkpoint instead of
  terminating the process. The shell's Stop button is the fallback.
- **Console encoding**: Japanese paths should be avoided in the run directory
  for the first run (the shell/README assume UTF-8).
- **Port choice**: the CLI takes the first free port from 18788; a pinned busy
  port fails with the `--local-data-port` hint.
- **Acceptance script**: `scripts/oss-quick-start/acceptance.sh` needs bash,
  curl and `shasum`; Git Bash provides the first two, `shasum` comes with Git's
  Perl. If not available, do the steps above manually and copy the outputs into
  the T1 table.
