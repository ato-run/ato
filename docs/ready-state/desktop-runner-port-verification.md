# Desktop Runner — Apple `container` port-publish verification (M3.5 Step 1)

> Status: **verification harness only**. Nothing here changes the product. The
> live `ato run` Desktop Runner path (M3) still only checks the container's
> running state and tears down — it does **not** publish a port. Local serving
> (M3.5 Step 2) will be implemented **after** this verification confirms the real
> Apple `container` CLI shape on macOS 26.

Local serving means publishing a capsule's port so the user can reach it from
the host. We refuse to wire a *guessed* publish flag into `ato run`: the publish
/ port-forward command shape must be confirmed against the real Apple
[`container`](https://github.com/apple/container) CLI first (it requires
macOS 26 + Apple silicon, which CI and most dev machines do not have).

## What the harness does

`crates/cli/src/application/desktop_runner/port_verify.rs` is a `#[cfg(test)]`,
`#[ignore]`d smoke. On a real macOS 26 host it:

1. allocates a free local host port (`allocate_local_port`);
2. trials each publish-flag candidate (`--publish`, then `-p`) by running a tiny
   HTTP OCI image with `<flag> <host>:<guest>`;
3. proves the port is reachable from the host (TCP connect to
   `127.0.0.1:<host>`);
4. stops + deletes the container and confirms a **second stop is safe**
   (idempotent cleanup);
5. prints a `PortVerificationReceipt` with the working flag and command shapes.

It never leaves a container running, and it is never part of `ato run`.

## Running it

On a macOS 26 Apple-silicon host with Apple `container` installed:

```sh
# Ensure the container system service is running, or opt in to starting it:
ATO_DESKTOP_SMOKE_START_SERVICE=1 \
  cargo test -p cli desktop_runner::port_verify::port_serving_verification \
  -- --ignored --nocapture
```

Override the image with `ATO_DESKTOP_SMOKE_IMAGE` if you need an offline mirror
or a different tiny HTTP server (the default expects `python3 -m http.server`).

## What to report back

Paste the printed `port-verification receipt` JSON into the M3.5 tracking issue.
The fields Step 2 depends on:

| field | why |
|---|---|
| `working_publish_flag` | the flag `ato run` should use (`--publish` / `-p` / other) |
| `publish_command_shape` | exact `<flag> host:guest` form that worked |
| `run_command_shape` | the full `container run` argv that started a reachable service |
| `reachable` | whether **any** candidate produced a host-reachable port |
| `start_to_health_ms` | cold-start → reachable latency |
| `cleanup_ok` / `second_stop_safe` | that `container stop` + `delete`/`rm` removes it and double-stop is safe |
| `container_version` / `macos_version` | the CLI/OS the result is valid for |

If **no** candidate is reachable (`reachable: false`), the receipt still records
what was tried — adjust `PUBLISH_FLAG_CANDIDATES` / the run shape in
`port_verify.rs` to match the real CLI (e.g. a `container network`/port-forward
subcommand) and re-run.

## Step 2 (deferred until verified)

Once the receipt confirms a working publish flag and stop sequence, M3.5 Step 2
implements local serving: allocate a host port, run with the verified publish
command, wait for a host healthcheck, and keep the container alive **only** once
an explicit Desktop Runner stop path exists (so `ato run` never leaves
containers running). All M3 safety gates remain — explicit selection only,
no-binding capsules only, `runtime="oci"` only, no Ready-State restore, no CRIU,
no binding injection, no default `ato run` change. See
[`desktop-runner.md`](./desktop-runner.md).
