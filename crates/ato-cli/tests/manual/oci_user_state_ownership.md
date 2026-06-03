# Manual verification — OCI container `user` + state ownership init (#428)

Automated unit tests cover the resolution + chown plumbing (privilege-free, against
the current uid). They cannot prove a real non-root container can write a mounted
volume on a real engine, because that needs Docker/Podman and (on macOS) a
podman machine. This doc is the manual gate for that.

## What #428 adds

- `[targets.<label>] user = "uid[:gid]"` on an OCI target → passed to the engine as `--user`.
- `[[services.<svc>.state_bindings]] owner = { uid, gid, recursive }` + `mode = "0700"`
  → Ato `chown`s (and optionally `chmod`s) the **host-side** state source before the
  container starts. Declaring `owner` is the opt-in; without it Ato never changes
  ownership of a bound path.

## A. Minimal synthetic check (engine-agnostic, fast)

Any non-root image that writes to a mounted dir works. Example `capsule.toml`:

```toml
[capsule]
name = "owner-smoke"

[targets.main]
runtime = "oci"
image = "alpine:3.20"
user = "1001:1001"
# Fail loudly if the volume is not writable by uid 1001:
cmd = ["sh", "-lc", "touch /data/ok && echo WROTE_OK && sleep 30"]

[state.data]
kind = "filesystem"
durability = "persistent"
attach = "explicit"
purpose = "smoke state"

[services.main]
target = "main"
[[services.main.state_bindings]]
state = "data"
target = "/data"
owner = { uid = 1001, gid = 1001, recursive = true }
mode = "0775"

[services.main.readiness_probe]
exec = ["test", "-f", "/data/ok"]
timeout_seconds = 20
interval_seconds = 2
```

Run (pick a host dir you own):

```sh
mkdir -p /tmp/owner-smoke-data
ato run ./owner-smoke --state data:/tmp/owner-smoke-data --yes
```

Expected:
- Readiness passes (the `exec` probe finds `/data/ok`), i.e. uid 1001 could write.
- Without the fix, the container exits and you get
  `oci_container_exited_before_ready` (from #429) or a readiness timeout.
- `ls -ln /tmp/owner-smoke-data` shows the dir owned by `1001:1001` on the host.

## B. OpenList end-to-end (the real #394 target)

Re-point the OpenList recipe target/binding to the new fields:

```toml
[targets.main]
runtime = "oci"
image = "openlistteam/openlist:v4.2.2"
user = "1001:1001"
port = 5244

[state.data]
kind = "filesystem"
durability = "persistent"
attach = "explicit"
purpose = "OpenList data"

[[services.main.state_bindings]]
state = "data"
target = "/opt/openlist/data"
owner = { uid = 1001, gid = 1001, recursive = true }
mode = "0700"
```

```sh
mkdir -p /tmp/openlist-data
OPENLIST_ADMIN_PASSWORD=dummy ato run ./openlist --state data:/tmp/openlist-data --yes
curl -sS -o /dev/null -w '%{http_code}\n' http://127.0.0.1:<allocated-port>/
```

Acceptance:
- macOS Podman: HTTP 200 (was: `Exited (1)` "does not have write … permissions for ./data").
- Restart persistence: stop, `ato run` again, prior config under `/tmp/openlist-data` is intact.
- Docker Desktop: HTTP 200 (no regression).
- Secrets (`OPENLIST_ADMIN_PASSWORD`, OAuth, Crypt password/salt) do not appear in logs/receipts.

## Notes / known limits

- On macOS the host `chown` runs against the host path; whether uid 1001 then maps
  through the podman-machine VM is exactly what step B verifies.
- `mode` is an **octal** string (`"0700"`, `"0775"`). Symbolic modes (`u+rwx`) are not parsed.
- A `mode` without an `owner` is rejected at resolution (no ownership pass to apply it during).
- On non-unix hosts, `owner` is logged and skipped (chown semantics don't transfer).
