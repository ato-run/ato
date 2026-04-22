# e2e-host-isolation — Multi-Target E2E Test Suite

OS-level host isolation tests for `ato-cli`. Each case asserts a specific
class of PATH/runtime pollution cannot affect `ato run` invocations.

## Test Cases

| # | Name | Linux | macOS | Windows | Notes |
|---|------|:-----:|:-----:|:-------:|-------|
| 01 | no-runtime | ✅ Docker | — | — | Reuses `tests/docker_no_runtime_runner.sh` |
| 02 | wrong-node | ✅ | ✅ | ✅ | Host Node version != managed 20.11.0 |
| 03 | wrong-python | ✅ | ✅ | ✅ | `continue-on-error` — PythonProvisioner pending |
| 04 | shim-poisoning | ✅ `sudo` | ✅ `sudo` | ✅ LOCALAPPDATA | asdf/mise/nvm shim in PATH |
| 05 | cwd-untouched | ✅ | ✅ | ✅ | No artifacts left in user cwd |
| 06 | child-spawn | ✅ | ✅ | ✅ | npm child process inherits managed PATH |
| 07 | sh-lc-trap | ✅ `sudo` | ✅ `sudo` | — | `/etc/profile.d` login shell trap |
| 08 | windows-path-case | — | — | ✅ | Dual PATH/Path env key injection |
| 09 | macos-path-helper | — | ✅ `sudo` | — | `/etc/paths.d` path_helper injection |
| 10 | symlink-shim | ✅ | ✅ | — | Symlink-to-shim in user PATH |

## Directory Layout

```
e2e-host-isolation/
├── cases/
│   ├── 01-no-runtime/      # (no run.sh — uses existing docker_no_runtime_runner.sh)
│   ├── 02-wrong-node/      run.sh  run.ps1
│   ├── 03-wrong-python/    run.sh  run.ps1
│   ├── 04-shim-poisoning/  run.sh  run.ps1
│   ├── 05-cwd-untouched/   run.sh  run.ps1
│   ├── 06-child-spawn/     run.sh  run.ps1
│   ├── 07-sh-lc-trap/      run.sh
│   ├── 08-windows-path-case/       run.ps1
│   ├── 09-macos-path-helper/ run.sh
│   └── 10-symlink-shim/    run.sh
├── docker/
│   └── Dockerfile.no-runtime
├── harness/
│   ├── assert.sh           # Bash assertion library
│   └── assert.ps1          # PowerShell assertion library
└── README.md
```

## Running Locally

### Prerequisites

- `ato` binary on PATH
- Bash 5+ (macOS: `brew install bash`)
- PowerShell 7+ (Windows)

### Run a single case

```bash
# Unix
bash tests/e2e-host-isolation/cases/02-wrong-node/run.sh

# Windows (PowerShell)
pwsh tests/e2e-host-isolation/cases/02-wrong-node/run.ps1
```

### Run Test 01 via Docker

```bash
# Build the tester image
docker build -t ato-no-runtime-tester \
  -f tests/e2e-host-isolation/docker/Dockerfile.no-runtime \
  tests/e2e-host-isolation/docker/

# Run (inject your local ato binary)
docker run --rm \
  -v "$(which ato)":/usr/local/bin/ato:ro \
  -v "$(pwd)/tests":/tests:ro \
  ato-no-runtime-tester \
  bash /tests/docker_no_runtime_runner.sh
```

### Cases requiring sudo

Tests 04, 07, and 09 write to system directories (`/usr/local/bin`,
`/etc/profile.d`, `/etc/paths.d`). They clean up via `trap EXIT`.
On GitHub Actions, `sudo` is passwordless. Locally, you will be prompted.

## CI Workflow

Triggered by `workflow_dispatch` on
`.github/workflows/e2e-host-isolation.yml`.

Inputs:
- `ato_ref` — branch/tag/SHA to build from (default: `dev`)
- `log_level` — ato log verbosity (default: `info`)

The workflow:
1. Builds release binaries in a matrix (Linux/macOS/Windows)
2. Runs each OS's relevant tests in parallel separate jobs
3. Emits a GITHUB_STEP_SUMMARY table with pass/fail per case
4. Uploads `~/.ato/logs/` on failure

## Known Limitations

- **Test 03 (wrong-python)**: `continue-on-error: true` in CI until
  PythonProvisioner v0.5.x lands. See `docker_host_python_leakage_runner.sh`.
- **Test 01 (no-runtime)**: Linux-only (Docker). macOS/Windows variants
  would require VM image customization not worth the maintenance cost.
