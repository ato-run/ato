# Manual verification — OCI container `user` + state ownership init (#428)

Automated tests cover the resolution + chmod/chown plumbing (privilege-free,
against the current uid). They cannot prove a real non-root container can write
a mounted volume on a real engine, because that needs Docker/Podman and (on
macOS) a podman machine. This doc is the manual gate for that.

## What #428 does

- `[targets.<label>] user = "uid[:gid]"` on an OCI target → passed to the engine as `--user`.
- `[[services.<svc>.state_bindings]] owner = { uid, gid, recursive }` + `mode = "0777"`
  (octal) → **best-effort** host-side init of the bound state source before the
  container starts:
  - **`chmod` to `mode`** — the load-bearing op on Podman-machine/virtiofs. There,
    real I/O maps to the host owner, but apps gate on `access(2)`/mode bits, so the
    dir must be mode-accessible to the container user (e.g. `0777` when the
    container uid ≠ host uid).
  - **`chown` to `uid[:gid]`** — best-effort. Works for root / native-Linux rootful;
    on a normal macOS/Linux user it returns `EPERM`, which is **logged and skipped,
    not fatal**.
- Declaring `owner`/`mode` is the recipe author's explicit opt-in; bindings without
  it are left untouched (no silent mutation of user-provided paths).
- **Never aborts the launch.** If the container still can't write, the readiness path
  surfaces the real error via `oci_container_exited_before_ready` (#429).
- **`:U` is intentionally not used** — it corrupts the virtiofs mount on macOS Podman
  (`mkdir: No such file or directory`).

## A. Synthetic non-root smoke (engine-agnostic, fast)

```toml
[capsule]   # plus schema_version="0.3", name, version, type, default_target="main"

[targets.main]
runtime = "oci"
image = "alpine:3.20"
user = "1001:1001"
cmd = ["sh", "-lc", "id; touch /data/ok && echo WROTE_OK && sleep 30"]

[state.data]
kind = "filesystem"
durability = "persistent"
attach = "explicit"
purpose = "smoke state"
schema_id = "sha256:0000…0001"   # persistent state requires a (non-empty) schema_id

[services.main]
target = "main"
[services.main.readiness_probe]
exec = ["test", "-f", "/data/ok"]
timeout_seconds = 20
interval_seconds = 2

[[services.main.state_bindings]]
state = "data"
target = "/data"
mode = "0777"                      # chmod is load-bearing; owner is optional
```

```sh
mkdir -p /tmp/owner-smoke-data
ato run ./owner-smoke --state data=/tmp/owner-smoke-data --yes   # note: --state uses '='
```
Expected: readiness passes (`/data/ok` created by uid 1001). Host dir becomes
mode `0777` (still host-owned). If a stale run left the dir root-owned and
chmod can't fix it, the run fails via `oci_container_exited_before_ready` (#429).

## B. OpenList end-to-end (the real #394 target)

```toml
[targets.main]
runtime = "oci"
image = "openlistteam/openlist:v4.2.2"
user = "1001:1001"
port = 5244
env = { OPENLIST_ADMIN_PASSWORD = "dummy-pass" }

[[services.main.state_bindings]]
state = "data"
target = "/opt/openlist/data"
owner = { uid = 1001, gid = 1001, recursive = true }   # chown best-effort
mode = "0777"                                           # chmod unblocks access(2)
```

```sh
mkdir -p /tmp/openlist-data
ato run ./openlist --state data=/tmp/openlist-data --yes
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:<allocated-port>/
```

Acceptance:
- macOS Podman: HTTP 200, restart persistence, no secret leakage in Ato receipts.
- Docker Desktop: HTTP 200 (no regression — no `:U` is added for docker).

### Verified runs

- **2026-06-03, macOS, Podman 5.7.1, host uid 501** — gate A and gate B both pass:
  - chown→1001 logs `best-effort chown skipped (not fatal)`; chmod 0777 applies.
  - OpenList: `start HTTP server @ 0.0.0.0:5244`, **HTTP 200** on the allocated port.
  - Restart: second `ato run` logs `reading config file …config.json` (no
    "config file not exists") → **persistence confirmed**; HTTP 200 again.
  - `ato stop --all` removes the container + network cleanly.
- Docker Desktop: not yet run; expected to pass via permissive bind mounts.

## Notes / known limits

- `mode` is an **octal** string (`"0700"`, `"0777"`). Symbolic modes (`u+rwx`) are not parsed.
- `chmod 0777` is world-writable; on Podman-machine/virtiofs this does not widen real
  access beyond the host user, but on native Linux it does. Scope `mode` accordingly.
- On non-unix hosts, `owner`/`mode` are logged and skipped (left to the engine).
