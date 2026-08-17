# Desktop OCI Session Surface

> **Historical manual test.** This covers a removed pre-Computation CLI and OCI
> session surface. Do not use it as current CLI documentation.

## Scope

Desktop OCI Session Surface v1 makes OCI sessions started by the CLI visible in
the Desktop Open Windows running-apps surface. Desktop can open a session
endpoint and request cleanup for one OCI session.

The CLI OCI session model is authoritative for this slice. Desktop owns the
presentation surface only.

Desktop does not add OCI launch UX in this slice.

## Discovery

Desktop reads OCI sessions through the CLI boundary:

```bash
ato ps --all --json
```

Desktop accepts `kind = "oci"` rows and projects only safe session fields into
the UI:

- session id
- import kind (`compose`, `docker-run-script`, or `explicit-oci`)
- status (`running`, `stopped`, or `stop_failed`)
- main endpoint URL when present
- service count
- source path and source hash when the CLI projection includes them

Desktop does not read `${ATO_HOME}/oci-sessions/*.json` directly.

Desktop does not parse Compose files or `install.sh` scripts. Import behavior
stays owned by the CLI.

## ATO_HOME

Desktop must launch `ato ps` and `ato stop` with the same `ATO_HOME` that owns
the CLI-started session. A session started under an isolated `ATO_HOME` is only
discoverable from a Desktop process launched with that same environment.

## Supported Actions

The Open Windows running-apps section supports:

- viewing OCI session source, import kind, service count, endpoint, and status
- opening `main_endpoint` through the normal Desktop URL surface
- stopping one OCI session through `ato stop --id <session_id>`
- retrying cleanup for a `stop_failed` session

Partial cleanup remains visible as `stop_failed`; the record stays available for
the next retry.

All lifecycle mutation goes through the CLI. Desktop does not call Podman,
Docker, Compose, or `install.sh` directly.

## Non-goals

This slice does not add:

- OCI launch from Desktop
- a full OCI logs UI
- a stable public OCI launch UI in Desktop
- rich live container inspect
- Compose or `install.sh` auto-detection in normal Desktop launch
- recipe catalog integration
- direct Podman, Docker, Compose, or `install.sh` execution from Desktop

## AODD Result

Desktop GUI AODD was re-run with a short `ATO_HOME`:

```bash
export ATO_HOME="/tmp/ato-desk-oci"
```

The CLI started the Blinko fixture with `--oci-install-sh`; `ato ps --all --json`
reported an OCI session with `import_kind = "docker-run-script"`,
`service_count = 2`, `status = "running"`, and a localhost endpoint. Desktop was
launched with the same `ATO_HOME`; the Open Windows / Running Apps model included
the OCI session with the OCI badge data, import kind, service count, status, and
endpoint.

The endpoint returned HTTP 200. Cleanup through `ato stop --id <session_id>`
stopped the two Ato-managed containers, removed the session network, deleted the
session record, and left no running Ato-managed containers. The receipt is in
`.tmp/aodd-receipts/desktop-oci-session-surface.yaml`.

The earlier long worktree-local `ATO_HOME` failure is isolated to the macOS Unix
socket path length limit for Desktop automation. Follow-up:

- harden the Desktop automation socket path against macOS Unix socket length
  limits
- use a short hashed socket path under `/tmp` or another OS-appropriate runtime
  directory
- preserve `ATO_HOME` isolation by including a hash of `ATO_HOME` and/or session
  id in the socket name
- add a regression test for long `ATO_HOME` paths
