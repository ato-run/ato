# Local Formation Phase 1 — Linux acceptance (2026-09-23)

What was run to accept `ato form <dir> --runtime local` after the Phase 1
hardening (frozen Initial Condition, Runtime-backed temporary realization,
disposable verification state). ADR-019 describes the design.

## Host

| | |
|---|---|
| Host | `oci-linux-test` (Ubuntu 24.04, aarch64) |
| Kernel | `6.17.0-1016-oracle` (Landlock with TCP rules) |
| bwrap | 0.9.0, unprivileged user namespaces enabled |
| Toolchain root | `/opt/ato/toolchains`, created for this run (`ubuntu`-owned) |
| Binary | `target/debug/ato` built from the PR head |

## Python web app (authored, process lane)

`capsule.toml` names Python 3.12.7, `argv = [.../python3, -B, /app/app.py,
8000]`, guest port 8000, and three requirements: `GET /health` 200, `GET /`
200, workspace identity `capture`. `app.py` writes `/app/first-run.db` and
`/tmp/cache.json` on start-up. The directory is a git repository.

An unrelated `python3 -m http.server 8000` held port 8000 for the whole run.

```text
ato form py-web --runtime local --network dependency-resolution
exit 0 · status formed
attempt authored: verified
  health / root / source-identity: satisfied
  realization:
    executor           runtime-process
    containment        bwrap+landlock
    workspace          disposable-copy, read-only at /app; /tmp is tmpfs
    build_network      dependency-resolution
    candidate_network  no-egress; tcp bind limited to allocated host ports
    endpoints          app.http: guest 8000 -> host 39759
    destroyed          true
```

After the run:

- artifact `sha256:de8bb286…` contains `app.py` and `capsule.toml` only — no
  `first-run.db`, no `.git`;
- no candidate process remained (`pgrep -af /app/app.py`);
- the work root held only the build workspace and cache; the realization
  scratch and the frozen source tree were removed.

## Node web app (node-static lane)

`package.json` + lockfile + `build` script that writes `dist/index.html`.

```text
ato form node-web --runtime local --network dependency-resolution
exit 0 · status formed
attempt node-static/v1: verified (root, source-identity satisfied)
```

The build ran contained with a provisioned Node (the bundle's page reads
`built by node v20.20.2`). This is a static lane: the verdict is decided from
the built artifact, not from a running process — Formation has no Node
process lane.

## Test suites on the same host

| Suite | Result |
|---|---|
| `formation-worker --test temporary_realization_v1` | 8/8 — port mapped past an occupied guest port, disposable copy, host paths unreadable, no egress to `1.1.1.1:443`, cleanup after pass / failing observation / 30 s readiness timeout / early exit |
| `formation-worker --test local_formation_v1` | 11/11, including the authored process candidate end to end |
| `formation-worker` other suites, `ato-formation`, `ato-adapter-process` | pass, except `sandbox_v1::a_step_that_declared_no_network_does_not_get_one` |
| `connected-realization-worker --lib` | 121–135 of 140; the failing set changes between runs and the same tests fail on the base commit |

Pre-existing, reproduced on base `a1cf5c42` under the same conditions:

- `sandbox_v1::a_step_that_declared_no_network_does_not_get_one` — the test's
  `worker_binary()` resolves `target/debug/deps/ato-formation-worker`, which
  does not exist; bwrap cannot bind it.
- `connected-realization-worker` volume / network_broker / recovery /
  browser e2e tests fail under parallel execution on this host; individually
  they pass (checked: `mismatched_python_version_fails_admission_with_a_reason`,
  `volume::tests::a_replacement_is_all_or_nothing`).

On macOS (no bwrap) process candidates are Filtered at admission and the
realization refuses to launch; those paths are what the macOS run asserts.
