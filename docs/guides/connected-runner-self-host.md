# Connect a user-owned Runner

Implementation branch guide, 2026-10-08 (TODO #137/#145). These changes require
the matching API/PWA release. No new binary distribution has been published.
This guide uses the current Connected Realization Worker, not retired
`ato runner login` or `ato runner serve` commands.

## Build and diagnose

On Linux or macOS with Rust installed:

```sh
cargo install --locked --git https://github.com/ato-run/ato.git ato-connected-realization-worker
ato-connected-realization-worker doctor --work-root "$HOME/.local/share/ato-runner"
```

For a reproducible installation add `--rev <reviewed-commit>` to the install
command. `doctor` needs neither an account credential nor an enrollment code.
It measures cgroup limits, disk and NVIDIA devices, plus Docker and isolation
availability, and does not create the work root or launch a workload. A missing
directory has unknown disk capacity. Diagnosis on macOS does not imply Linux
GPU/process/OCI compatibility; no Windows installer is provided by this slice.

## Enroll and reconnect

In ato.run Settings → Connected Runners, request a connection code. It expires
after ten minutes and can register one user-owned Runner. Copy it only to the
machine you own. In Bash, read it without terminal echo or shell-history text:

```bash
read -rs ATO_RUNNER_ENROLLMENT_TOKEN
export ATO_RUNNER_ENROLLMENT_TOKEN
ato-connected-realization-worker \
  --api-base 'https://api.ato.run' \
  --work-root "$HOME/.local/share/ato-runner" \
  --runner-credentials-file "$HOME/.local/share/ato-runner/runner-credentials.json" \
  --surface-target 127.0.0.1:18421
```

Use the API origin shown by the current PWA, including staging when applicable.
The first successful enrollment writes device credentials with POSIX mode 0600,
removes the one-time token and reexecutes. Restart the same command without the
enrollment environment variable: the saved device identity is reused. Do not
copy this credential file between hosts or include it in backups shared with
others. Revoke the device from Settings to invalidate it; a new code enrolls a
new device. A revoked code only invalidates an unspent registration.

A public origin is optional for control-plane connection. Add
`--public-base-url 'https://your-runner.example'` only when you already expose
the Worker Surface through an authenticated supported route. Outbound control
alone does not provide remote Web UI access. `--state-volume-root <directory>`
uses the existing volume-store admission; configured storage is not advertised
as durable until admission succeeds.

## Supervision

After enrolling in the foreground, a systemd user service can reuse those
credentials. Replace the API origin and executable path with verified values:

```ini
[Unit]
Description=Ato Connected Runner
After=network-online.target

[Service]
ExecStart=%h/.cargo/bin/ato-connected-realization-worker --api-base https://api.ato.run --work-root %h/.local/share/ato-runner --runner-credentials-file %h/.local/share/ato-runner/runner-credentials.json --surface-target 127.0.0.1:18421
Restart=on-failure
RestartSec=10
UMask=0077

[Install]
WantedBy=default.target
```

Save to `~/.config/systemd/user/ato-runner.service`, then use
`systemctl --user daemon-reload`, `systemctl --user enable --now ato-runner` and
`journalctl --user -u ato-runner`. Enable login-independent service operation
through your host's administration policy. Never put the enrollment token in
the service file. Stop workloads and confirm cleanup before replacing the
binary. Existing recovery fencing refuses new work while previous owned
resources remain unconfirmed; restarting is not a way to bypass that refusal.

## Acceptance boundary

The protocol never provisions or deletes an attached machine or Notebook.
Registration is distinct from GPU/isolation/storage admission. GPU LLM/Wan,
Docker backup/restore and a server management UI still
need their roadmap acceptance. No Kaggle/Colab or real GPU support is certified
by the read-only diagnostic or the API fixture tests alone.

## Import an existing Model Set offline

The current CLI can verify a canonical `ato.model-set/1` manifest and import
its objects from an existing directory. Use the manifest and SHA-256 reference
declared by the reviewed Capsule, rather than reconstructing its identity:

```sh
ato model-set import \
  --manifest ./models/model-set.json \
  --digest 'sha256:<declared-manifest-digest>' \
  --source /path/to/existing/model-files \
  --max-cache-bytes 68719476736
```

`--source` paths follow the manifest's relative object paths. The default
cache is `$ATO_HOME/cache/model-cache` (`ATO_HOME` defaults to the existing CLI
home); `--cache-root /path/to/runner-work-root` explicitly uses that Worker's
existing cache. Files are size-checked, fully SHA-256 verified and published
read-only. An interrupted copy resumes from verified final hash checks; a
verified cache hit requires no source-file read. The JSON report records the
Model Set reference, transferred bytes and logical cache usage.

The limit counts cached objects and partial transfers, excluding Instance
input/output and cache control files. An import refuses over-capacity and
concurrent mutations with `model_cache_quota_exceeded` or `model_cache_busy`;
stop the competing import/download and retry. It never removes existing model
objects or Instance data to make space. Importing weights alone grants no GPU,
workload execution or cloud-provider authority.

Local `ato run <reviewed.capsule>` process routes resolve their declared Model
Sets only from `$ATO_HOME/cache`. Every declared object must already be present
and verified; no upstream download or replacement Model Set is inferred. The
runtime supplies `ATO_INPUT_PATH_<INPUT_ID>` and read-only input mounts outside
writable process scratch. Stopping removes the delivery tree and keeps cached
objects. State-capable Linux routes also bind these input trees read-only in
bubblewrap. Missing objects refuse admission before the process starts.

This path still refuses GPU/other host-conditioned routes until local measured
host admission and device assignment are implemented. A successful CPU fixture
does not establish GPU compatibility. RunPod continues to use the existing
managed Worker, host admission and Data Grant delivery.
