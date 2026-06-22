# Testing Guide — ato-cli

Three layers of tests ensure `ato` stays reliable across releases.

---

## 1. Automated tests (CI — runs on every PR)

```bash
cargo test -p ato-cli         # unit + integration tests
cargo test --workspace        # all crates
```

E2E suites (some require Docker):

| Suite | File | What it covers |
|-------|------|----------------|
| CLI unit & integration | `tests/cli_tests.rs` | command parsing, manifest handling |
| Fail-closed | `tests/fail_closed_*.rs` | security boundary enforcement |
| Share / encap / decap | `tests/share_*.rs` | `ato encap`, `ato run <url>`, `ato decap` |
| Host isolation | `tests/e2e-host-isolation/` | PATH poisoning, shim injection, wrong-runtime |
| Provider (PyPI, npm) | `tests/provider_*.rs` | dependency resolution |
| IPC | `tests/ipc_*.rs` | Unix socket bridge |

Run only the host-isolation suite:

```bash
cargo test -p ato-cli host_isolation
```

Run Docker-backed E2E tests (requires Docker):

```bash
bash tests/docker_no_runtime_e2e.sh
bash tests/docker_shim_poisoning_e2e.sh
bash tests/docker_host_python_leakage_e2e.sh
```

---

## 2. Manual tests (pre-release human verification)

`tests/manual/` contains 5 test groups covering the behavioral axes that CI cannot
exercise automatically: interactive prompts, real network calls, real sandbox
enforcement, and secret masking.

```bash
cd tests/manual
./test-all.sh                           # All 5 groups (~15 min)
./01-group1-env/test-group1-env.sh      # Group 1 only
./02-group2-pm/test-group2-pm.sh        # Group 2 only
./03-group3-secrets/test-group3-secrets.sh
./04-group4-targets/test-group4-targets.sh
./05-group5-edge/test-group5-edge.sh
```

| Group | Cases | What it covers |
|-------|-------|----------------|
| `01-group1-env` | 1a–1e | Config / env variable handling |
| `02-group2-pm` | 2a–2e | Package manager detection (uv, pnpm, cargo, …) |
| `03-group3-secrets` | 3a–3f | `ato secrets` lifecycle, masking, `--dry-run` |
| `04-group4-targets` | 4a–4c | Target type combinations (source, wasm, oci) |
| `05-group5-edge` | 5a–5e | Edge cases & security boundaries |

**Prerequisites**: `ato` on `$PATH`, `jq`, internet access.

Results are written to `tests/manual/results/`. The directory is gitignored.

---

## 3. Sample compatibility tests (CI — ato-samples repo)

The [`ato-run/ato-samples`](https://github.com/ato-run/ato-samples) repository
runs every sample against new `ato` releases. A failing sample against a released
version is treated as a regression.

To reproduce a sample failure locally:

```bash
git clone https://github.com/ato-run/ato-samples
cd ato-samples/00-quickstart/url-to-run-hello-static
ato run .
```

---

## Reporting a bug

Open an issue at [github.com/ato-run/ato-cli/issues](https://github.com/ato-run/ato-cli/issues).
Include:
- `ato --version` output
- OS and architecture (`uname -a`)
- Minimal reproducing `capsule.toml` or command
- Full `ato run --verbose` output (or `ATO_LOG=debug ato run …`)
