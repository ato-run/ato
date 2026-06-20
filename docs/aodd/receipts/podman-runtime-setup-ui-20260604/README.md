# Podman Runtime Setup — smoke receipts (2026-06-04)

Validation of the merged Podman Runtime Setup prepare flow (PR #438 backend, PR
#443 UI) plus the disabled-state polish added on branch
`chore/podman-runtime-smoke-polish`.

## Scope of verification (honest)

The smoke was executed through the **CLI command the Desktop button shells out
to** — `ato internal runtime prepare --tools podman --emit-json` — which is the
exact argv built by `runtime_setup::install::RuntimeJobKind::Prepare.cli_args`
in `ato-desktop`. This exercises the real prepare backend, the streamed phase
protocol, machine planning, connection pinning, and the OCI provider's
connection selection end to end.

What was **not** exercised in this session: the literal GUI click path (Wry
WebView → `prepare_runtime_tools` IPC → broker capability check → the command
above). This environment has no interactive display automation, so button
clicks / screenshots were not captured. That IPC routing and capability gating
are covered by unit tests in `ato-desktop` (PR #443: `prepare_runtime_tools`
routes for onboarding+settings; `RuntimeSetupPrepare` allowed only for those
surfaces; this PR: settings snapshot exposes `runtime.podmanEnabled`). The
`screenshots/` directory is intentionally empty for that reason.

## Environment

See `environment.txt`. Summary:

- macOS (Darwin 24.6.0, arm64)
- Podman 5.7.1 at `/opt/homebrew/bin/podman` (Homebrew)
- Homebrew present (`/opt/homebrew/bin/brew`)
- `ato` built from this worktree (`target/debug/ato`, v0.5.5)
- Desktop GUI launch (Finder/Dock) PATH does not include `/opt/homebrew/bin`;
  this run was from a shell that does. The GUI-PATH podman resolver itself is
  covered by `capsule::podman` (PR #440); not re-tested here.

## Machine state before / after

| Point | State |
|-------|-------|
| Before (`podman-machine-list-before.txt`) | `podman-machine-default` running; **no `ato-podman`** |
| After cleanup (`cleanup-final-machine-list.txt`) | `podman-machine-default` running; `ato-podman` removed |
| Post-restore status (`post-restore-status.txt`) | podman `ready=true action=none` — matches before |

The developer machine had a running `podman-machine-default`, so the clean
"empty machine list" Case A was forced by **temporarily** stopping the default
(it had no running containers) to reach the `InitAndStartAto` plan, then the
default was restarted and `ato-podman` removed. Net change to the machine: none.

## Cases

### Case A — installed, no `ato-podman` machine → create + start + verify ✅

1. Stopped `podman-machine-default` (`caseA-01-stop-default.txt`).
2. Status with no machine running (`caseA-02-status-no-machine.json`):
   `ready=false, action=start_service` — Podman detected installed, not falsely
   missing.
3. `prepare` phases (`caseA-03-prepare-emit-json.txt`):
   `queued → locating → initializing_machine → starting_machine → verifying →
   ready` (~33s).
4. `ato-podman` created + running (`podman-machine-list-after.txt`); connections
   `ato-podman` / `ato-podman-root` created (`caseA-04-connection-list.txt`).
   **Global default connection unchanged** (`podman-machine-default-root`).
5. `podman --connection ato-podman info` → `localhost.localdomain / 5.7.1`
   (`caseA-05-connection-info.txt`).
6. Ran `ato run samples/recipes/adminer` (`capsule-run-log.txt`): network +
   image pull + container start + readiness `200 GET /`, service at
   `http://127.0.0.1:43167/`.
7. **Capsule ran on `ato-podman`** (`caseA-06-adminer-on-ato-podman.txt`):
   container `ato-adminer-main-c0b08cd4` and network `ato-adminer-c0b08cd4` are
   on `ato-podman`; `podman-machine-default` was stopped (connection refused),
   proving `--connection ato-podman` was used.

### Case B — `ato-podman` exists but stopped → start only (no init) ✅

1. Stopped `ato-podman` (`caseB-01-stop-ato-podman.txt`).
2. Status (`caseB-02-status-stopped.json`): `ready=false,
   action=prepare_host_runtime` (both machines stopped → ambiguous).
3. `prepare` phases (`caseB-03-prepare-start-only.txt`):
   `queued → locating → starting_machine → verifying → ready` — **no
   `initializing_machine`**: it started the existing machine, did not recreate
   it (the `StartAto` plan).

### Case C — Podman missing, Homebrew available — NOT run live ⚠️

Would require uninstalling Homebrew Podman from the dev machine (destructive).
Covered by unit tests: `runtime_prepare` `install_podman` runs the brew path and
re-resolves; resolver `NotFound → install → StillMissingAfterInstall`. Not
GUI/live-tested.

### Case D — Podman missing, Homebrew missing — NOT run live ⚠️

Would require removing Homebrew. Covered by unit test: `install_podman` returns
`InstallUnavailable` with actionable instructions when `brew_bin()` is `None`;
no installer script is run. The UI renders this as non-preparable guidance
(`open_instructions`) with a Skip path (Step5 / settings row code).

### Case E — Settings: Podman disabled in Ato (the polish) ✅ (code + unit test)

Fix added this branch: the settings snapshot now exposes
`resolved.runtime.podmanEnabled`, and the Settings → Runtime Podman row renders
**"Disabled in Ato"** when `podman_enabled === false` regardless of host
readiness, with an **Enable Podman** (host ready) / **Enable & Prepare** (host
not ready) button. Language-runtime install still omits `podman_enabled`.

- Rust test `settings::tests::snapshot_exposes_runtime_podman_enabled` ✅.
- Config preservation on omitted fields:
  `runtime_setup::tests::apply_runtime_setup_preserves_omitted_fields` (PR #443).
- Live GUI toggle not screenshot-captured (no display automation).

### Case F — Onboarding keyboard does not bypass prepare — NOT GUI-run ⚠️

Covered by Step5.jsx logic (PR #443): Enter/ArrowRight route through
`handlePrimary`, which runs prepare when Podman is pending and only finishes
when nothing is pending; "Skip Podman for now" persists `podman_enabled:false`.
Not interactively GUI-tested here.

## Result

- Backend prepare flow: **PASS** for installed-no-machine (create) and
  stopped-machine (start-only); capsule confirmed launching via
  `--connection ato-podman`.
- No provider/machine-policy change was made; an observed nuance (when one
  non-Ato machine is already running, `prepare` reuses it via `UseDefault` and
  the OCI provider likewise does not pin `ato-podman`) is **internally
  consistent**, not a defect — documented, not changed.
- Disabled-state polish: implemented + unit-tested.
- GUI-click, Case C/D/E/F live runs: not automatable in this session — covered
  by unit tests + code review as noted above.
