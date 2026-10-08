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
Docker backup/restore, local offline Model Sets and a server management UI still
need their roadmap acceptance. No Kaggle/Colab or real GPU support is certified
by the read-only diagnostic or the API fixture tests alone.
